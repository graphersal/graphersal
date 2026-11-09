//! Nested objects stored as shapes (stage V3 of the compact memory track): the objects inside a
//! stored value share their key lists from the second sighting of a key sequence. Everything
//! observable stays the same: reads, equality, ordering, deduplication, jpath writes, rollbacks,
//! compression rules, snapshots; only the memory figures shrink.

use graphersal::catalog::{Codec, CompressionDefinition};
use graphersal::prelude::*;
use graphersal::storage::{ElementKind, MemoryReportRequest};

fn object(entries: Vec<(&str, ElementProperty)>) -> ElementProperty {
    ElementProperty::Object(Box::new(entries.into_iter().collect()))
}

fn hashes(tag: &str, value: &str) -> ElementProperty {
    object(vec![(tag, value.into())])
}

/// `n` `Method` vertices with a `_hashes` object (one of `tags` key names) and an `attrs` array
/// of two objects.
fn methods(n: usize, tags: usize) -> TraversalGraph {
    let mut graph = TraversalGraph::new();
    for i in 0..n {
        let props: PropertyMap = [
            ("name", ElementProperty::from(format!("m{i}"))),
            (
                "_hashes",
                hashes(&format!("ACM:{}", i % tags), &format!("{:016x}", i % 13)),
            ),
            (
                "attrs",
                ElementProperty::Array(vec![
                    object(vec![("line", (i as i64).into()), ("kind", "call".into())]),
                    object(vec![("line", 1i64.into()), ("kind", "use".into())]),
                ]),
            ),
        ]
        .into_iter()
        .collect();
        graph
            .insert_vertex(Some(format!("m{i}").into()), vec!["Method".into()], props)
            .unwrap();
    }
    graph
}

fn value_of(graph: &TraversalGraph, id: &str, key: &str) -> ElementProperty {
    let handle = graph.get_vertex_by_id(id).unwrap().expect("vertex");
    graph
        .vertex_properties(handle)
        .unwrap()
        .expect("live")
        .get(key)
        .expect("key")
        .clone()
}

#[test]
fn a_repeated_nested_object_costs_its_values_only() {
    let n = 1000;
    let mut graph = TraversalGraph::new();
    for i in 0..n {
        let props: PropertyMap = [("_hashes", hashes("ACM:3", &format!("{:016x}", i)))]
            .into_iter()
            .collect();
        graph
            .insert_vertex(None, vec!["Method".into()], props)
            .unwrap();
    }
    let stats = graph.memory_usage().unwrap().expect("stats");
    // Per vertex: the slot (32), the object's box (56) and its one value (32); the 16-byte
    // string is inline. Only the first object (its key sequence seen once) is a hash map.
    let per_vertex = (stats.property_bytes - stats.shape_bytes) / n as u64;
    assert_eq!(per_vertex, 32 + 56 + 32);
    let report = GraphTraversalSource::new(&graph)
        .memory_report(&MemoryReportRequest::new())
        .unwrap();
    assert_eq!(report.property_bytes(), stats.property_bytes);
}

#[test]
fn queries_over_nested_objects_return_what_plain_values_give() {
    let graph = methods(200, 7);
    let g = || graph.traversal();
    // Vertex 10 of `methods`: its hash is `i % 13` = 10.
    let wanted = hashes("ACM:3", &format!("{:016x}", 10));
    assert_eq!(
        g().v(None)
            .has("_hashes", wanted.clone())
            .count()
            .next()
            .unwrap(),
        Some(ElementProperty::from(
            (0..200).filter(|i| i % 7 == 3 && i % 13 == 10).count() as i64
        ))
    );
    // Distinct objects by value: 7 tags x 13 hashes that occur together.
    let distinct: std::collections::HashSet<(usize, usize)> =
        (0..200).map(|i| (i % 7, i % 13)).collect();
    assert_eq!(
        g().v(None)
            .values("_hashes")
            .dedup()
            .count()
            .next()
            .unwrap(),
        Some(ElementProperty::from(distinct.len() as i64))
    );
    // Ordering and grouping see the same values.
    let ordered = g()
        .v(None)
        .order()
        .by("_hashes")
        .by("name")
        .values("name")
        .limit(3)
        .to_list()
        .unwrap();
    let mut expected: Vec<(String, String, String)> = (0..200)
        .map(|i| {
            (
                format!("ACM:{}", i % 7),
                format!("{:016x}", i % 13),
                format!("m{i}"),
            )
        })
        .collect();
    expected.sort();
    let expected: Vec<ElementProperty> = expected
        .into_iter()
        .take(3)
        .map(|(_, _, name)| ElementProperty::from(name.as_str()))
        .collect();
    assert_eq!(ordered, expected);
    assert_eq!(
        value_of(&graph, "m10", "_hashes"),
        hashes("ACM:3", &format!("{:016x}", 10))
    );
    assert_eq!(
        value_of(&graph, "m4", "attrs"),
        ElementProperty::Array(vec![
            object(vec![("line", 4i64.into()), ("kind", "call".into())]),
            object(vec![("kind", "use".into()), ("line", 1i64.into())]),
        ])
    );
}

#[test]
fn jpath_writes_and_rollbacks_inside_nested_objects() {
    let mut graph = methods(10, 2);
    let before = value_of(&graph, "m5", "_hashes");
    let handle = graph.get_vertex_by_id("m5").unwrap().expect("vertex");
    let mark = graph.begin_unit().unwrap();
    let added = "_hashes.extra".parse::<JsonPath>().unwrap();
    graph
        .set_vertex_property_path(handle, &added, 9.into())
        .unwrap();
    let changed = "_hashes.ACM:1".parse::<JsonPath>().unwrap();
    graph
        .set_vertex_property_path(handle, &changed, "new".into())
        .unwrap();
    assert_eq!(
        value_of(&graph, "m5", "_hashes"),
        object(vec![("ACM:1", "new".into()), ("extra", 9.into())])
    );
    graph
        .rollback_unit(mark, &graphersal::changes::RollbackReason::failed(&"test"))
        .unwrap();
    assert_eq!(value_of(&graph, "m5", "_hashes"), before);
}

#[test]
fn compression_rules_inside_and_on_nested_objects_keep_their_invariant() {
    let mut graph = methods(300, 5);
    {
        let mut g = graph.traversal_mut();
        g.define_compression(
            "hashes",
            CompressionDefinition::new(ElementKind::Vertex, "Method", "_hashes")
                .unwrap()
                .with_codec(Codec::Dedup),
        )
        .unwrap();
        g.define_compression(
            "attrs_kind",
            CompressionDefinition::with_path(
                ElementKind::Vertex,
                "Method",
                "attrs.kind".parse::<JsonPath>().unwrap(),
            )
            .with_codec(Codec::Dedup),
        )
        .unwrap();
    }
    graph.check_compression_invariant().unwrap();
    assert_eq!(
        value_of(&graph, "m7", "_hashes"),
        hashes("ACM:2", &format!("{:016x}", 7))
    );
    let stats = graph.memory_usage().unwrap().expect("stats");
    let report = GraphTraversalSource::new(&graph)
        .memory_report(&MemoryReportRequest::new())
        .unwrap();
    assert_eq!(report.property_bytes(), stats.property_bytes);
}

/// A value nested close to `traversal.max_value_depth` is stored twice (so its top levels are
/// shaped), read, compared, cloned and dropped on the test thread's stack.
#[test]
fn deeply_nested_values_are_stored_and_compared_within_the_stack() {
    let mut value = ElementProperty::from(1);
    for level in 0..120 {
        value = if level % 3 == 0 {
            ElementProperty::Array(vec![value])
        } else {
            object(vec![("next", value)])
        };
    }
    let mut graph = TraversalGraph::new();
    for i in 0..2 {
        let props: PropertyMap = [("deep", value.clone())].into_iter().collect();
        graph
            .insert_vertex(Some(format!("d{i}").into()), vec![], props)
            .unwrap();
    }
    let first = value_of(&graph, "d0", "deep");
    let second = value_of(&graph, "d1", "deep");
    assert_eq!(first, value);
    assert_eq!(second, value);
    assert_eq!(first, second);
    let listed = graph.traversal().v(None).values("deep").to_list().unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0], listed[1]);
    drop(graph);
}

#[cfg(feature = "persist")]
#[test]
fn a_snapshot_reload_gives_the_same_values_and_memory() {
    let graph = methods(500, 11);
    let mut bytes = Vec::new();
    graph.write_snapshot(&mut bytes).unwrap();
    let loaded = TraversalGraph::read_snapshot(bytes.as_slice()).unwrap();
    for i in [0, 1, 17, 499] {
        let id = format!("m{i}");
        for key in ["_hashes", "attrs"] {
            assert_eq!(value_of(&graph, &id, key), value_of(&loaded, &id, key));
        }
    }
    let mut again = Vec::new();
    loaded.write_snapshot(&mut again).unwrap();
    let reloaded = TraversalGraph::read_snapshot(again.as_slice()).unwrap();
    assert_eq!(
        loaded.memory_usage().unwrap(),
        reloaded.memory_usage().unwrap()
    );
}
