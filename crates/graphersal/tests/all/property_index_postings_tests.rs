//! The postings layout of property indexes (book page "Property Indexes"): each key once with the
//! sorted slots of its elements; one slot inline, a sorted run from the second on, sorted chunks
//! past 4096 slots. These tests drive one key through every shape and back (inserts, updates,
//! drops, rollbacks) and compare the maintained index with a fresh rebuild
//! (`TraversalGraph::check_property_indexes`) after every write, the lookups with the scan, and
//! the `stats()` numbers with what the index holds.
#![cfg(feature = "script")]

use graphersal::catalog::{Definition, PropertyIndexDefinition};
use graphersal::changes::RollbackReason;
use graphersal::prelude::*;
use graphersal::storage::{
    ElementKind, Graph, GraphStorage, PropertyIndexRange, PropertyIndexStats, UnitMark,
    VertexHandle,
};

use crate::script_host::{ScriptLimits, eval_value_atomic};

/// Runs a script as one unit (a failure rolls it back).
fn run(
    graph: &std::sync::Arc<Graph>,
    script: &str,
) -> Result<graphersal::script::rhai::Dynamic, String> {
    eval_value_atomic(
        graph.clone(),
        script,
        std::collections::HashMap::new(),
        &ScriptLimits::default(),
    )
    .map_err(|err| err.to_string())
}

/// Past one chunk of the postings (4096 slots).
const MANY: usize = 5_000;

fn vertex_index(label: &str, key: &str) -> Definition {
    Definition::property_index(
        format!("{label}_{key}"),
        PropertyIndexDefinition::new(ElementKind::Vertex, label, key),
    )
}

fn edge_index(label: &str, key: &str) -> Definition {
    Definition::property_index(
        format!("e_{label}_{key}"),
        PropertyIndexDefinition::new(ElementKind::Edge, label, key),
    )
}

fn props(key: &str, value: ElementProperty) -> PropertyMap {
    let mut map = PropertyMap::default();
    map.insert(key, value);
    map
}

fn check(graph: &TraversalGraph) {
    graph.check_property_indexes().unwrap();
}

fn stats(graph: &TraversalGraph, name: &str) -> PropertyIndexStats {
    graph
        .property_indexes()
        .unwrap()
        .into_iter()
        .find(|info| info.name == name)
        .expect("the index")
        .stats
}

/// The vertices labelled `p` with `k == value`, from the index, in the index's order.
fn lookup(graph: &TraversalGraph, value: &str) -> Vec<VertexHandle> {
    graph
        .vertices_by_property_index("p", "k", &PropertyIndexRange::equal(value.into()))
        .unwrap()
        .expect("an index")
        .collect()
}

/// The vertices labelled `p` with `k == value`, by a scan.
fn scan(graph: &TraversalGraph, value: &str) -> Vec<VertexHandle> {
    let wanted = ElementProperty::from(value);
    let mut found: Vec<VertexHandle> = graph
        .vertices_by_label("p")
        .unwrap()
        .filter(|v| {
            graph
                .vertex_properties(*v)
                .unwrap()
                .and_then(|map| map.get("k").cloned())
                .is_some_and(|stored| stored == wanted)
        })
        .collect();
    found.sort_by_key(|v| graph.vertex_id(*v).unwrap().map(str::to_string));
    found
}

fn sorted_by_id(graph: &TraversalGraph, mut handles: Vec<VertexHandle>) -> Vec<VertexHandle> {
    handles.sort_by_key(|v| graph.vertex_id(*v).unwrap().map(str::to_string));
    handles
}

fn add(graph: &mut TraversalGraph, id: &str, value: &str) -> VertexHandle {
    graph
        .insert_vertex(Some(id.into()), vec!["p".into()], props("k", value.into()))
        .unwrap()
}

fn rollback(graph: &mut TraversalGraph, mark: UnitMark) {
    graph
        .rollback_unit(mark, &RollbackReason::failed(&"test"))
        .unwrap();
}

#[test]
fn one_key_goes_from_one_slot_to_chunks_and_back_to_nothing() {
    let mut graph = TraversalGraph::new();
    graph.set_definition(vertex_index("p", "k")).unwrap();
    let empty = stats(&graph, "p_k");
    assert_eq!(
        (empty.entries, empty.distinct_values, empty.bytes),
        (0, 0, 0)
    );

    // One slot: inline, no heap of its own.
    let first = add(&mut graph, "v00000", "a");
    check(&graph);
    let one = stats(&graph, "p_k");
    assert_eq!((one.entries, one.distinct_values), (1, 1));
    assert!(one.bytes > 0);
    assert_eq!(lookup(&graph, "a"), vec![first]);

    // A second slot: a run with heap of its own.
    add(&mut graph, "v00001", "a");
    check(&graph);
    let two = stats(&graph, "p_k");
    assert_eq!((two.entries, two.distinct_values), (2, 1));
    assert!(two.bytes > one.bytes);

    // Past one chunk.
    let mut handles = vec![first];
    handles.extend(
        (2..MANY)
            .map(|n| add(&mut graph, &format!("v{n:05}"), "a"))
            .collect::<Vec<_>>(),
    );
    handles.insert(1, lookup(&graph, "a")[1]);
    check(&graph);
    let many = stats(&graph, "p_k");
    assert_eq!((many.entries, many.distinct_values), (MANY as u64, 1));
    // The value once, about four bytes per element: far below the 48 bytes per entry of a
    // (value, handle) set.
    assert!(
        many.bytes < 8 * MANY as u64 + 1024,
        "{} bytes for {MANY} entries",
        many.bytes
    );
    assert_eq!(lookup(&graph, "a").len(), MANY);
    assert_eq!(sorted_by_id(&graph, lookup(&graph, "a")), scan(&graph, "a"));

    // Back down to one slot (every other element first, then the rest), then nothing.
    for (n, v) in handles.iter().enumerate().skip(1).step_by(2) {
        graph.drop_vertex(*v).unwrap();
        if n % 999 == 1 {
            check(&graph);
        }
    }
    check(&graph);
    assert_eq!(stats(&graph, "p_k").entries, MANY as u64 / 2);
    for v in handles.iter().skip(2).step_by(2) {
        graph.drop_vertex(*v).unwrap();
    }
    check(&graph);
    let back = stats(&graph, "p_k");
    assert_eq!((back.entries, back.distinct_values), (1, 1));
    assert_eq!(back.bytes, one.bytes, "a single slot is inline again");
    assert_eq!(lookup(&graph, "a"), vec![first]);
    graph.drop_vertex(first).unwrap();
    check(&graph);
    let gone = stats(&graph, "p_k");
    assert_eq!((gone.entries, gone.distinct_values, gone.bytes), (0, 0, 0));
    assert!(lookup(&graph, "a").is_empty());
}

#[test]
fn a_rollback_restores_every_shape() {
    let mut graph = TraversalGraph::new();
    graph.set_definition(vertex_index("p", "k")).unwrap();
    let only = add(&mut graph, "only", "a");
    add(&mut graph, "b0", "b");
    add(&mut graph, "b1", "b");
    check(&graph);
    let before = stats(&graph, "p_k");

    // One -> chunks, rolled back.
    let mark = graph.begin_unit().unwrap();
    for n in 0..MANY {
        add(&mut graph, &format!("a{n:05}"), "a");
    }
    check(&graph);
    assert_eq!(lookup(&graph, "a").len(), MANY + 1);
    rollback(&mut graph, mark);
    check(&graph);
    assert_eq!(lookup(&graph, "a"), vec![only]);
    let after = stats(&graph, "p_k");
    assert_eq!(
        (after.entries, after.distinct_values),
        (before.entries, before.distinct_values)
    );

    // Chunks -> nothing by drops, rolled back (the elements come back).
    for n in 0..MANY {
        add(&mut graph, &format!("c{n:05}"), "c");
    }
    check(&graph);
    let mark = graph.begin_unit().unwrap();
    for v in lookup(&graph, "c") {
        graph.drop_vertex(v).unwrap();
    }
    check(&graph);
    assert!(lookup(&graph, "c").is_empty());
    rollback(&mut graph, mark);
    check(&graph);
    assert_eq!(lookup(&graph, "c").len(), MANY);
    assert_eq!(sorted_by_id(&graph, lookup(&graph, "c")), scan(&graph, "c"));

    // A run -> One by an update, rolled back.
    let mark = graph.begin_unit().unwrap();
    let b0 = graph.get_vertex_by_id("b0").unwrap().unwrap();
    graph
        .set_vertex_property(b0, "k".into(), "z".into())
        .unwrap();
    check(&graph);
    assert_eq!(lookup(&graph, "b").len(), 1);
    rollback(&mut graph, mark);
    check(&graph);
    assert_eq!(lookup(&graph, "b").len(), 2);
}

#[test]
fn updates_move_elements_between_keys_across_every_shape() {
    let mut graph = TraversalGraph::new();
    for n in 0..MANY {
        add(&mut graph, &format!("v{n:05}"), "x");
    }
    // Built over existing data.
    graph.set_definition(vertex_index("p", "k")).unwrap();
    check(&graph);
    let built = stats(&graph, "p_k");
    assert_eq!((built.entries, built.distinct_values), (MANY as u64, 1));
    assert!(built.bytes < 5 * MANY as u64 + 1024, "{}", built.bytes);

    // Every element from x to y, in a shuffled order (inserts into the middle of chunks).
    let mut handles = lookup(&graph, "x");
    let mut state = 0x9E37_79B9_7F4A_7C15_u64;
    for i in (1..handles.len()).rev() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        handles.swap(i, (state % (i as u64 + 1)) as usize);
    }
    for (n, v) in handles.iter().enumerate() {
        graph
            .set_vertex_property(*v, "k".into(), "y".into())
            .unwrap();
        if n % 1_250 == 0 {
            check(&graph);
            let now = stats(&graph, "p_k");
            assert_eq!(now.entries, MANY as u64);
            assert_eq!(now.distinct_values, if n + 1 == MANY { 1 } else { 2 });
        }
    }
    check(&graph);
    assert!(lookup(&graph, "x").is_empty());
    assert_eq!(lookup(&graph, "y").len(), MANY);
    // Maintained runs stay small: at most twice the slots plus the chunk headers.
    let moved = stats(&graph, "p_k");
    assert_eq!((moved.entries, moved.distinct_values), (MANY as u64, 1));
    assert!(moved.bytes < 9 * MANY as u64 + 1024, "{}", moved.bytes);
    // Removing the key from half of them (a non-indexed value is counted as skipped).
    for v in handles.iter().step_by(2) {
        graph
            .set_vertex_property(*v, "k".into(), ElementProperty::Array(vec![]))
            .unwrap();
    }
    check(&graph);
    let half = stats(&graph, "p_k");
    assert_eq!(half.entries, MANY as u64 / 2);
    assert_eq!(half.skipped, MANY as u64 / 2);
}

#[test]
fn a_lookup_goes_by_value_then_slot_and_follows_slot_reuse() {
    let mut graph = TraversalGraph::new();
    graph.set_definition(vertex_index("p", "k")).unwrap();
    // Slots in creation order: values interleaved.
    let mut expected: Vec<(String, VertexHandle)> = Vec::new();
    for n in 0..30 {
        let value = ["c", "a", "b"][n % 3];
        let v = add(&mut graph, &format!("v{n:02}"), value);
        expected.push((value.to_string(), v));
    }
    expected.sort_by_key(|(value, _)| value.clone()); // stable: slot order inside a value
    let range = PropertyIndexRange::new(
        std::ops::Bound::Included("a".into()),
        std::ops::Bound::Included("c".into()),
    );
    let got: Vec<VertexHandle> = graph
        .vertices_by_property_index("p", "k", &range)
        .unwrap()
        .unwrap()
        .collect();
    assert_eq!(
        got,
        expected.iter().map(|(_, v)| *v).collect::<Vec<_>>(),
        "by value, then by slot"
    );

    // A dropped element's slot taken by a new element: the lookup returns the new handle under
    // the new value only.
    let old = lookup(&graph, "a")[0];
    graph.drop_vertex(old).unwrap();
    let new = add(&mut graph, "new", "q");
    check(&graph);
    assert!(!lookup(&graph, "a").contains(&new));
    assert_eq!(lookup(&graph, "q"), vec![new]);
    assert_eq!(graph.vertex_id(new).unwrap(), Some("new"));
}

#[test]
fn dropping_all_elements_of_a_large_owner_keeps_edge_and_vertex_indexes_exact() {
    let graph = std::sync::Arc::new(Graph::new(TraversalGraph::new()));
    {
        let mut g = graph.write();
        g.set_definition(vertex_index("p", "owner")).unwrap();
        g.set_definition(edge_index("calls", "owner")).unwrap();
        let mut previous: Option<VertexHandle> = None;
        for n in 0..3 * MANY {
            let owner = if n % 3 == 0 {
                "big"
            } else {
                ["o1", "o2", "o3"][n % 4 % 3]
            };
            let v = g
                .insert_vertex(
                    Some(format!("v{n}").into()),
                    vec!["p".into()],
                    props("owner", owner.into()),
                )
                .unwrap();
            if let Some(p) = previous {
                g.insert_edge(
                    None,
                    Some("calls".into()),
                    p,
                    v,
                    props("owner", owner.into()),
                )
                .unwrap();
            }
            previous = Some(v);
        }
        check(&g);
    }
    let count = |query: &str| -> i64 { run(&graph, query).unwrap().as_int().unwrap() };
    let indexed = r#"g.v().has_label("p").has("owner", "big").count().next()"#;
    let scanned = r#"g.with("optimizer.disabled", ["property_index"]).v().has_label("p").has("owner", "big").count().next()"#;
    assert_eq!(count(indexed), MANY as i64);
    assert_eq!(count(indexed), count(scanned));
    let edges_indexed = r#"g.e().has_label("calls").has("owner", "big").count().next()"#;
    let edges_scanned = r#"g.with("optimizer.disabled", ["property_index"]).e().has_label("calls").has("owner", "big").count().next()"#;
    assert_eq!(count(edges_indexed), count(edges_scanned));

    // A failing script drops them and is rolled back as one unit.
    let failing = run(
        &graph,
        r#"g.v().has_label("p").has("owner", "big").drop().iterate(); throw "no""#,
    );
    assert!(failing.is_err());
    check(&graph.read());
    assert_eq!(count(indexed), MANY as i64);

    let _ = run(
        &graph,
        r#"g.v().has_label("p").has("owner", "big").drop().iterate()"#,
    )
    .unwrap();
    check(&graph.read());
    assert_eq!(count(indexed), 0);
    assert_eq!(count(edges_indexed), 0);
    assert_eq!(count(edges_indexed), count(edges_scanned));
    assert_eq!(count(indexed), count(scanned));
    let g = graph.read();
    let vertices = g
        .property_indexes()
        .unwrap()
        .into_iter()
        .find(|info| info.name == "p_owner")
        .unwrap()
        .stats;
    assert_eq!(vertices.distinct_values, 3);
    assert_eq!(vertices.entries, 2 * MANY as u64);
}
