//! Label-less property indexes (book page "Property Indexes", section "Label-less indexes";
//! design D1/D2/D14/D17 of the property index extensions): `create_index(#{key})` without a
//! label holds every element of the kind, unlabeled ones included; a source without labels
//! reads it, a labelled source reads it when it is cheaper than its per-label indexes (checking
//! each candidate's labels), GQL node patterns without a label use it, and a fused `count()`
//! is answered from its postings.
//!
//! The oracle is the scan, as in `property_index_tests.rs`: every query runs with and without
//! the index (`g.with("optimizer.disabled", ["property_index"])`), and the maintained entries
//! are compared with a fresh rebuild (`check_property_indexes`) after every write.
#![cfg(feature = "script")]

use std::collections::HashMap;
use std::sync::Arc;

use graphersal::catalog::{Definition, DefinitionKind, PropertyIndexDefinition};
use graphersal::prelude::*;
use graphersal::script::rhai::Dynamic;
use graphersal::storage::{ElementKind, Graph, GraphStorage, PropertyIndexRange};

use crate::script_host::{ScriptLimits, eval_value_with_limits};

// ---------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------

/// A small deterministic generator (xorshift64*).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn run(graph: &Arc<Graph>, script: &str) -> Result<Dynamic, String> {
    eval_value_with_limits(
        graph.clone(),
        script,
        HashMap::new(),
        &ScriptLimits::default(),
    )
    .map_err(|err| err.to_string())
}

fn ok(graph: &Arc<Graph>, script: &str) -> Dynamic {
    run(graph, script).unwrap_or_else(|err| panic!("{script}: {err}"))
}

fn check(graph: &Graph) {
    graph.read().check_property_indexes().unwrap();
}

/// The rendered values of a list result, sorted.
fn sorted(value: Dynamic) -> Vec<String> {
    let mut items: Vec<String> = value
        .into_array()
        .expect("a list")
        .into_iter()
        .map(|item| item.to_string())
        .collect();
    items.sort();
    items
}

/// `query` (after `g`, no terminal) with and without the index: the sorted ids.
fn both(graph: &Arc<Graph>, query: &str) -> (Vec<String>, Vec<String>) {
    let with = sorted(ok(graph, &format!("g.{query}.id().to_list()")));
    let without = sorted(ok(
        graph,
        &format!("g.with(\"optimizer.disabled\", [\"property_index\"]).{query}.id().to_list()"),
    ));
    (with, without)
}

/// The step names of `query`'s profile (`query` after `g`).
fn profile_steps(graph: &Arc<Graph>, query: &str) -> Vec<String> {
    let metrics = ok(graph, &format!("g.{query}.profile()"))
        .try_cast::<graphersal::profile::TraversalMetrics>()
        .expect("metrics");
    metrics
        .metrics()
        .iter()
        .map(|step| step.name().to_string())
        .collect()
}

fn first_step(graph: &Arc<Graph>, query: &str) -> String {
    profile_steps(graph, query)
        .into_iter()
        .next()
        .unwrap_or_default()
}

fn props(entries: &[(&str, ElementProperty)]) -> PropertyMap {
    let mut map = PropertyMap::default();
    for (key, value) in entries {
        map.insert(*key, value.clone());
    }
    map
}

/// A graph of vertices with labels `a`, `b`, `c`, two labels or none, a string `k` from a
/// small set (sometimes missing, sometimes a number, an array or null) and edges with `o`.
fn random_graph(rng: &mut Rng, vertices: usize) -> TraversalGraph {
    let label_sets: [&[&str]; 6] = [&["a"], &["b"], &["c"], &["a", "b"], &["b", "c"], &[]];
    let values: Vec<ElementProperty> = vec![
        "x".into(),
        "y".into(),
        "z".into(),
        "x".into(),
        1_i64.into(),
        ElementProperty::Null,
        ElementProperty::Array(vec!["x".into()]),
    ];
    let mut graph = TraversalGraph::new();
    let mut handles = Vec::new();
    for _ in 0..vertices {
        let labels: Vec<CompactString> = label_sets[rng.below(label_sets.len())]
            .iter()
            .map(|l| CompactString::from(*l))
            .collect();
        let mut map = PropertyMap::default();
        if rng.below(5) != 0 {
            map.insert("k", values[rng.below(values.len())].clone());
        }
        handles.push(graph.add_vertex(None, labels, map).unwrap());
    }
    for _ in 0..vertices {
        let from = handles[rng.below(handles.len())];
        let to = handles[rng.below(handles.len())];
        let label = if rng.below(2) == 0 { "r1" } else { "r2" };
        let owner = values[rng.below(4)].clone();
        graph
            .add_edge(None, Some(label.into()), from, to, props(&[("o", owner)]))
            .unwrap();
    }
    graph
}

// ---------------------------------------------------------------------------------------------
// Definition, catalog kind, DSL
// ---------------------------------------------------------------------------------------------

#[test]
fn a_label_less_index_is_created_listed_and_dropped_as_kind_property_index_ext() {
    let graph = Arc::new(GraphSource::tinkerpop_modern());
    let _ = ok(&graph, r#"g.create_index(#{key: "name"})"#);
    let _ = ok(
        &graph,
        r#"g.create_index(#{element: "edge", key: "weight", transform: ()})"#,
    );
    check(&graph);
    {
        let g = graph.read();
        let stored = g
            .definition(DefinitionKind::PROPERTY_INDEX_EXT, "name")
            .unwrap()
            .expect("stored as kind 6");
        assert!(
            stored.flags().is_critical(),
            "a label-less index is critical"
        );
        assert_eq!(stored.as_property_index().unwrap().label(), None);
        assert!(
            g.definition(DefinitionKind::PROPERTY_INDEX, "name")
                .unwrap()
                .is_none()
        );
        // The default name of an edge index on a key: `<key>_edge`.
        assert!(
            g.definition(DefinitionKind::PROPERTY_INDEX_EXT, "weight_edge")
                .unwrap()
                .is_some()
        );
        let infos = g.property_indexes().unwrap();
        let name = infos.iter().find(|i| i.name == "name").unwrap();
        assert_eq!(name.label, None);
        assert_eq!(name.stats.entries, 6, "every vertex holds a name");
    }
    // `indexes()` lists both kinds by name; a label-less row has `label: ()`.
    let rows = ok(&graph, "g.indexes()").into_array().unwrap();
    let names: Vec<String> = rows
        .iter()
        .map(|row| row.clone().cast::<graphersal::script::rhai::Map>()["name"].to_string())
        .collect();
    assert_eq!(names, vec!["name", "weight_edge"]);
    let row = rows[0].clone().cast::<graphersal::script::rhai::Map>();
    assert!(row["label"].is_unit(), "{row:?}");
    assert_eq!(row["entries"].as_int().unwrap(), 6);
    assert_eq!(
        ok(&graph, r#"g.drop_index("weight_edge")"#).as_bool(),
        Ok(true)
    );
    assert_eq!(
        ok(&graph, r#"g.drop_index("weight_edge")"#).as_bool(),
        Ok(false)
    );
    check(&graph);
    // The Rust API: create, list, drop.
    let mut tg = TraversalGraph::tinkerpop_modern();
    tg.traversal_mut()
        .create_index(
            "ages",
            PropertyIndexDefinition::label_less(ElementKind::Vertex, "age"),
        )
        .unwrap();
    let listed = tg.traversal().list_indexes().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].0.kind(), DefinitionKind::PROPERTY_INDEX_EXT);
    assert_eq!(listed[0].1.unwrap().entries, 4);
    assert!(tg.traversal_mut().drop_index("ages").unwrap());
    assert!(tg.traversal().list_indexes().unwrap().is_empty());
}

#[test]
fn the_payload_omits_the_label_and_a_plain_kind_needs_one() {
    let index = PropertyIndexDefinition::label_less(ElementKind::Vertex, "name");
    let payload = index.to_payload();
    assert!(payload.get("label").is_none(), "{payload:?}");
    assert_eq!(
        PropertyIndexDefinition::from_payload(&payload).unwrap(),
        index
    );
    let definition = Definition::property_index("name", index.clone());
    let json = definition.to_json();
    assert!(json.contains("\"property_index_ext\""), "{json}");
    assert_eq!(Definition::from_json(&json).unwrap(), definition);
    // The exchange form of kind 6 without flags is still critical.
    let bare = Definition::from_json(
        r#"{"kind": "property_index_ext", "name": "n", "element": "vertex", "key": "name"}"#,
    )
    .unwrap();
    assert!(bare.flags().is_critical());
    bare.validate().unwrap();
    // A plain index kind without a label is malformed; kind 6 without the flag is refused.
    assert!(
        Definition::from_json(
            r#"{"kind": "property_index", "name": "n", "element": "vertex", "key": "name"}"#
        )
        .is_err()
    );
    let uncritical = definition.with_flags(graphersal::catalog::DefinitionFlags::NONE);
    assert!(uncritical.validate().is_err());
}

#[test]
fn index_names_are_one_namespace_across_both_kinds() {
    let graph = Arc::new(GraphSource::tinkerpop_modern());
    let _ = ok(
        &graph,
        r#"g.create_index(#{label: "person", key: "name", name: "names"})"#,
    );
    // The same name, label-less: replaces the per-label index in one unit.
    let _ = ok(&graph, r#"g.create_index(#{key: "name", name: "names"})"#);
    check(&graph);
    {
        let g = graph.read();
        assert!(
            g.definition(DefinitionKind::PROPERTY_INDEX, "names")
                .unwrap()
                .is_none()
        );
        let infos = g.property_indexes().unwrap();
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].label, None);
    }
    // A rollback of the replacement inside a unit restores the per-label one exactly.
    {
        let mut g = graph.write();
        let mark = g.begin_unit().unwrap();
        g.set_definition(Definition::property_index(
            "names",
            PropertyIndexDefinition::new(ElementKind::Vertex, "person", "name"),
        ))
        .unwrap();
        g.check_property_indexes().unwrap();
        assert_eq!(
            g.property_indexes().unwrap()[0].label.as_deref(),
            Some("person")
        );
        g.rollback_unit(mark, &graphersal::changes::RollbackReason::failed(&"test"))
            .unwrap();
        g.check_property_indexes().unwrap();
        assert_eq!(g.property_indexes().unwrap()[0].label, None);
    }
    // A per-label and a label-less index on one key coexist under different names.
    let _ = ok(&graph, r#"g.create_index(#{label: "person", key: "name"})"#);
    assert_eq!(graph.read().property_indexes().unwrap().len(), 2);
    check(&graph);
    // Two label-less indexes on one key: a conflict.
    let conflict = run(&graph, r#"g.create_index(#{key: "name", name: "again"})"#).unwrap_err();
    assert!(
        conflict.contains("already covers every vertex"),
        "{conflict}"
    );
}

// ---------------------------------------------------------------------------------------------
// What it holds and how it is maintained
// ---------------------------------------------------------------------------------------------

#[test]
fn unlabeled_and_multi_label_elements_are_held_once() {
    let mut tg = TraversalGraph::new();
    let bare = tg
        .add_vertex(None, vec![], props(&[("k", "x".into())]))
        .unwrap();
    let both = tg
        .add_vertex(
            None,
            vec!["a".into(), "b".into()],
            props(&[("k", "x".into())]),
        )
        .unwrap();
    tg.add_vertex(None, vec!["a".into()], props(&[("k", "y".into())]))
        .unwrap();
    tg.set_definition(Definition::property_index(
        "k",
        PropertyIndexDefinition::label_less(ElementKind::Vertex, "k"),
    ))
    .unwrap();
    tg.check_property_indexes().unwrap();
    let mut found: Vec<_> = tg
        .vertices_by_property_index(None, "k", &PropertyIndexRange::equal("x".into()))
        .unwrap()
        .unwrap()
        .collect();
    found.sort_by_key(|v| tg.vertex_id(*v).unwrap().map(str::to_string));
    let mut expected = vec![bare, both];
    expected.sort_by_key(|v| tg.vertex_id(*v).unwrap().map(str::to_string));
    assert_eq!(found, expected);
    // Labels change: the element stays in the label-less index exactly once.
    tg.add_vertex_label(bare, "c".into()).unwrap();
    tg.remove_vertex_label(both, "a").unwrap();
    tg.check_property_indexes().unwrap();
    assert_eq!(
        tg.count_by_property_index(
            ElementKind::Vertex,
            None,
            "k",
            None,
            &PropertyIndexRange::equal("x".into())
        )
        .unwrap(),
        Some(2)
    );
    let graph = Arc::new(Graph::new(tg));
    assert_eq!(
        ok(&graph, r#"g.v().has("k", "x").count().next()"#).as_int(),
        Ok(2)
    );
}

#[test]
fn random_writes_and_rollbacks_keep_it_exact_and_queries_equal() {
    for seed in [3_u64, 11, 29] {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let graph = Arc::new(Graph::new(random_graph(&mut rng, 80)));
        let _ = ok(&graph, r#"g.create_index(#{key: "k"})"#);
        let _ = ok(&graph, r#"g.create_index(#{element: "edge", key: "o"})"#);
        check(&graph);
        let writes = [
            r#"g.v().has("k", "x").limit(2).property("k", "y").iterate()"#,
            r#"g.v().has("k", "y").limit(1).properties("k").drop().iterate()"#,
            r#"g.add_v().property("k", "x").iterate()"#,
            r#"g.add_v("a", "b").property("k", "z").iterate()"#,
            r#"g.v().has("k", "z").limit(1).drop().iterate()"#,
            r#"g.v().has_label("a").limit(1).add_label("c").iterate()"#,
            r#"g.v().has_label("b").limit(1).drop_label("b").iterate()"#,
            r#"g.e().has("o", "x").limit(1).property("o", "y").iterate()"#,
            r#"g.e().has("o", "z").limit(1).drop().iterate()"#,
            r#"g.v().has("k", "x").limit(1).property("k", "q").v().fail("roll back").iterate()"#,
            r#"g.add_v().property("k", "x").v().fail("roll back").iterate()"#,
        ];
        let queries = [
            r#"v().has("k", "x")"#,
            r#"v().has("k", "y")"#,
            r#"v().has_label("a").has("k", "x")"#,
            r#"v().has_label("a", "c").has("k", "z")"#,
            r#"v().has("k", P.within("x", "z"))"#,
            r#"v().has("k", P.gt("x"))"#,
            r#"v().has("k", 1)"#,
            r#"e().has("o", "x")"#,
            r#"e().has_label("r1").has("o", "y")"#,
        ];
        for step in 0..40 {
            let write = writes[rng.below(writes.len())];
            let _ = run(&graph, write);
            check(&graph);
            let query = queries[step % queries.len()];
            let (with, without) = both(&graph, query);
            assert_eq!(with, without, "seed {seed}, after {write}: {query}");
            let count = format!("{query}.count().next()");
            assert_eq!(
                ok(&graph, &format!("g.{count}")).as_int(),
                ok(
                    &graph,
                    &format!("g.with(\"optimizer.disabled\", [\"property_index\"]).{count}")
                )
                .as_int(),
                "{count}"
            );
        }
        assert!(first_step(&graph, r#"v().has("k", "x")"#).contains("any label"));
    }
}

// ---------------------------------------------------------------------------------------------
// The planner: D2 both ways, rendering, GQL, count from postings
// ---------------------------------------------------------------------------------------------

#[test]
fn a_source_without_labels_reads_the_label_less_index_and_profile_says_any_label() {
    let graph = Arc::new(GraphSource::tinkerpop_modern());
    // No index yet: a plain scan.
    assert!(!first_step(&graph, r#"v().has("name", "josh")"#).contains("[index"));
    let _ = ok(&graph, r#"g.create_index(#{key: "name"})"#);
    assert_eq!(
        first_step(&graph, r#"v().has("name", "josh")"#),
        r#"v().has("name", "josh") [index: vertex name, any label]"#
    );
    // A has(key, P) after the source is served too.
    assert_eq!(
        first_step(&graph, r#"v().has("name", P.within("josh", "lop"))"#),
        r#"v() [index: vertex name, any label]"#
    );
    // A labelled source without a per-label index reads it and checks the label (D2, D17).
    assert_eq!(
        first_step(&graph, r#"v().has_label("person").has("name", "lop")"#),
        r#"v(labels: ["person"]).has("name", "lop") [index: vertex name, any label]"#
    );
    let (with, without) = both(&graph, r#"v().has_label("person").has("name", "lop")"#);
    assert!(with.is_empty());
    assert_eq!(with, without);
    // A transformed label-less index: `to_lower` is named before the marker.
    let _ = ok(
        &graph,
        r#"g.create_index(#{key: "name", transform: "to_lower"})"#,
    );
    assert_eq!(
        first_step(&graph, r#"v().has("name", __.to_lower().is("josh"))"#),
        r#"v() [index: vertex name to_lower, any label]"#
    );
    // Edges.
    let _ = ok(
        &graph,
        r#"g.create_index(#{element: "edge", key: "weight"})"#,
    );
    assert_eq!(
        first_step(&graph, r#"e().has("weight", 0.5)"#),
        r#"e().has("weight", 0.5) [index: edge weight, any label]"#
    );
    let (with, without) = both(&graph, r#"e().has("weight", 0.5)"#);
    assert_eq!(with.len(), 1);
    assert_eq!(with, without);
    // The rule can be turned off.
    let off = profile_steps(
        &graph,
        r#"with("optimizer.disabled", ["property_index"]).v().has("name", "josh")"#,
    );
    assert!(!off[0].contains("[index"), "{off:?}");
}

/// 100 `a` vertices share one `k`; 900 `c` vertices have distinct ones.
fn skewed() -> Arc<Graph> {
    let mut tg = TraversalGraph::new();
    for _ in 0..100 {
        tg.add_vertex(None, vec!["a".into()], props(&[("k", "shared".into())]))
            .unwrap();
    }
    for i in 0..900 {
        tg.add_vertex(
            None,
            vec!["c".into()],
            props(&[("k", format!("v{i}").as_str().into())]),
        )
        .unwrap();
    }
    Arc::new(Graph::new(tg))
}

#[test]
fn the_cheaper_of_per_label_and_label_less_wins_both_ways() {
    // Per-label `a.k`: 100 entries over 1 value (estimate 100); label-less: 1000 over 901
    // (estimate 1): the label-less index wins for `a` and checks the label.
    let graph = skewed();
    let _ = ok(&graph, r#"g.create_index(#{label: "a", key: "k"})"#);
    let _ = ok(&graph, r#"g.create_index(#{key: "k"})"#);
    let query = r#"v().has_label("a").has("k", "shared")"#;
    assert!(
        first_step(&graph, query).ends_with("[index: vertex k, any label]"),
        "{}",
        first_step(&graph, query)
    );
    let (with, without) = both(&graph, query);
    assert_eq!(with.len(), 100);
    assert_eq!(with, without);

    // Per-label `c.k`: 900 entries over 900 values (estimate 1); label-less over a skewed
    // whole: 1000 over 901 (estimate 1): a tie keeps the per-label index (no label check).
    let graph = skewed();
    let _ = ok(&graph, r#"g.create_index(#{label: "c", key: "k"})"#);
    let _ = ok(&graph, r#"g.create_index(#{key: "k"})"#);
    let query = r#"v().has_label("c").has("k", "v7")"#;
    assert!(
        first_step(&graph, query).ends_with(r#"[index: vertex c.k]"#),
        "{}",
        first_step(&graph, query)
    );
    // A label whose per-label index is the cheaper one keeps it: `c` vertices with 9
    // repeated values (100 each) and `b` with 900 distinct; per-label `b.k` estimate 1,
    // label-less (1000 entries over 909 values) estimate 1 -> tie: per-label.
    let mut tg = TraversalGraph::new();
    for i in 0..900 {
        let value = format!("c{}", i % 9);
        tg.add_vertex(
            None,
            vec!["c".into()],
            props(&[("k", value.as_str().into())]),
        )
        .unwrap();
    }
    for i in 0..100 {
        let value = format!("b{i}");
        tg.add_vertex(
            None,
            vec!["b".into()],
            props(&[("k", value.as_str().into())]),
        )
        .unwrap();
    }
    let graph = Arc::new(Graph::new(tg));
    let _ = ok(&graph, r#"g.create_index(#{label: "c", key: "k"})"#);
    let _ = ok(&graph, r#"g.create_index(#{key: "k"})"#);
    // `c.k`: 900 over 9 = 100 per value; label-less: 1000 over 109 = 9 per value: label-less.
    assert!(first_step(&graph, r#"v().has_label("c").has("k", "c1")"#).contains("any label"));
    let _ = ok(&graph, r#"g.create_index(#{label: "b", key: "k"})"#);
    // `b.k`: 100 over 100 = 1 per value; label-less 9: per-label wins.
    assert!(
        first_step(&graph, r#"v().has_label("b").has("k", "b1")"#).ends_with("[index: vertex b.k]")
    );
    let (with, without) = both(&graph, r#"v().has_label("b", "c").has("k", "c1")"#);
    assert_eq!(with.len(), 100);
    assert_eq!(with, without);
}

#[test]
fn a_fused_count_is_answered_from_the_postings_only_when_exact() {
    let graph = Arc::new(GraphSource::tinkerpop_modern());
    let _ = ok(&graph, r#"g.create_index(#{key: "name"})"#);
    let _ = ok(&graph, r#"g.create_index(#{key: "age"})"#);
    let _ = ok(&graph, r#"g.create_index(#{label: "person", key: "lang"})"#);
    let count =
        |query: &str| {
            (
            ok(&graph, &format!("g.{query}.count().next()")).as_int().unwrap(),
            ok(
                &graph,
                &format!(
                    "g.with(\"optimizer.disabled\", [\"property_index\"]).{query}.count().next()"
                ),
            )
            .as_int()
            .unwrap(),
        )
        };
    // One folded string equality, no label: the postings answer it.
    assert_eq!(
        first_step(&graph, r#"v().has("name", "josh").count()"#),
        r#"v().has("name", "josh").count() [index: vertex name, any label] [count from index]"#
    );
    assert_eq!(count(r#"v().has("name", "josh")"#), (1, 1));
    assert_eq!(count(r#"v().has("name", "nobody")"#), (0, 0));
    // A number is ranged by its float value (a superset): no count from the postings.
    assert!(!first_step(&graph, r#"v().has("age", 29).count()"#).contains("count from index"));
    assert_eq!(count(r#"v().has("age", 29)"#), (1, 1));
    // A label to check (label-less index under a labelled source): no count from postings.
    assert!(
        !first_step(
            &graph,
            r#"v().has_label("software").has("name", "lop").count()"#
        )
        .contains("count from index")
    );
    assert_eq!(
        count(r#"v().has_label("software").has("name", "lop")"#),
        (1, 1)
    );
    // A second folded condition: re-checked, no count from postings.
    assert!(
        !first_step(&graph, r#"v().has("name", "josh").has("age", 32).count()"#)
            .contains("count from index")
    );
    // A per-label index of the source's one label is exact too.
    let _ = ok(
        &graph,
        r#"g.v().has_label("person").limit(2).property("lang", "en").iterate()"#,
    );
    assert_eq!(
        first_step(
            &graph,
            r#"v().has_label("person").has("lang", "en").count()"#
        ),
        r#"v(labels: ["person"]).has("lang", "en").count() [index: vertex person.lang] [count from index]"#
    );
    assert_eq!(
        count(r#"v().has_label("person").has("lang", "en")"#),
        (2, 2)
    );
    // Mid-traversal: one pass per incoming traverser.
    assert_eq!(
        ok(
            &graph,
            r#"g.v().has_label("software").map(__.V().has("name", "josh").count()).sum().next()"#
        )
        .as_int(),
        Ok(2)
    );
    // Edges.
    let _ = ok(&graph, r#"g.e().property("tag", "t").iterate()"#);
    let _ = ok(&graph, r#"g.create_index(#{element: "edge", key: "tag"})"#);
    assert!(
        first_step(&graph, r#"e().has("tag", "t").count()"#)
            .contains("[index: edge tag, any label] [count from index]")
    );
    assert_eq!(count(r#"e().has("tag", "t")"#), (6, 6));
}

#[cfg(feature = "gql")]
#[test]
fn gql_node_patterns_without_a_label_use_the_label_less_index() {
    let graph = Arc::new(GraphSource::tinkerpop_modern());
    let _ = ok(&graph, r#"g.create_index(#{key: "name"})"#);
    let gql = |query: &str| format!("gql({query:?})");
    // The root scan of a node without a label.
    let root = gql(r#"MATCH (n {name: "josh"}) RETURN n.age"#);
    assert_eq!(
        first_step(&graph, &root),
        "v() [index: vertex name, any label]"
    );
    assert_eq!(
        sorted(ok(&graph, &format!("g.{root}.to_list()"))),
        vec![r#"#{"n.age": 32}"#]
    );
    // A correlated lookup of a node without a label, and one with a label no per-label index
    // serves (its label is checked after the lookup).
    for (query, expected) in [
        (
            r#"MATCH (a:person {name: "marko"}) MATCH (b) WHERE b.name = a.name RETURN b.age"#,
            vec![r#"#{"b.age": 29}"#],
        ),
        (
            r#"MATCH (a:person {name: "lop"}) MATCH (b:software) WHERE b.name = a.name RETURN b.lang"#,
            vec![],
        ),
        (
            r#"MATCH (a:software {name: "lop"}) MATCH (b:software) WHERE b.name = a.name RETURN b.lang"#,
            vec![r#"#{"b.lang": "java"}"#],
        ),
    ] {
        let steps = profile_steps(&graph, &gql(query));
        let lookup = steps
            .iter()
            .find(|s| s.starts_with("gql:lookup"))
            .unwrap_or_else(|| panic!("{steps:?}"));
        assert!(
            lookup.ends_with("[index: vertex name, any label]"),
            "{lookup}"
        );
        let with = sorted(ok(&graph, &format!("g.{}.to_list()", gql(query))));
        let without = sorted(ok(
            &graph,
            &format!(
                "g.with(\"optimizer.disabled\", [\"property_index\"]).{}.to_list()",
                gql(query)
            ),
        ));
        assert_eq!(with, without, "{query}");
        let expected: Vec<String> = expected.into_iter().map(str::to_string).collect();
        assert_eq!(with, expected, "{query}");
    }
}

// ---------------------------------------------------------------------------------------------
// Schema check, advisor (help texts: `error_diagnostics_tests.rs`)
// ---------------------------------------------------------------------------------------------

#[test]
fn the_schema_check_looks_at_every_label_that_declares_the_key() {
    let graph = Arc::new(GraphSource::tinkerpop_modern());
    let _ = ok(
        &graph,
        r#"g.set_schema(#{mode: "open", vertices: #{
            person: #{schema: #{type: "object", properties: #{name: #{type: "string"}}}},
            doc: #{schema: #{type: "object", properties: #{name: #{type: "array"}, code: #{type: "integer"}}}}
        }})"#,
    );
    // `doc` declares `name` as an array: refused, the error names the label.
    let refused = run(&graph, r#"g.create_index(#{key: "name"})"#).unwrap_err();
    assert!(refused.contains("vertex 'doc' key 'name'"), "{refused}");
    // `code` is declared an integer only by `doc`: a plain index is fine, `to_lower` is not.
    let _ = ok(&graph, r#"g.create_index(#{key: "code"})"#);
    let refused = run(
        &graph,
        r#"g.create_index(#{key: "code", transform: "to_lower"})"#,
    )
    .unwrap_err();
    assert!(refused.contains("'to_lower'"), "{refused}");
    // A key no label declares: fine.
    let _ = ok(&graph, r#"g.create_index(#{key: "age"})"#);
}

#[test]
fn the_advisor_recommends_a_label_less_index_for_scans_without_a_label() {
    use graphersal::advisor::{AdviceRequest, RecommendedAction};
    use graphersal::workload::{WorkloadLevel, WorkloadStats};
    let graph = GraphSource::tinkerpop_modern();
    graph.attach_workload_stats(Arc::new(WorkloadStats::new(WorkloadLevel::Memory)));
    let graph = Arc::new(graph);
    for _ in 0..3 {
        let _ = ok(
            &graph,
            r#"g.v().has("name", "josh").values("age").to_list()"#,
        );
    }
    let advice = graph.advise(&AdviceRequest::everything()).unwrap();
    let ids: Vec<&str> = advice
        .recommendations
        .iter()
        .map(|r| r.id.as_str())
        .collect();
    let index = advice
        .recommendations
        .iter()
        .find(|r| r.id == "create_index:vertex:*:name")
        .unwrap_or_else(|| panic!("{ids:?}"));
    assert!(
        matches!(
            &index.action,
            RecommendedAction::CreateIndex { label: None, .. }
        ),
        "{index:?}"
    );
    assert_eq!(index.label, None);
    assert_eq!(index.action.command(), r#"g.create_index(#{key: "name"})"#);
    assert!(
        index.evidence.contains("no single label"),
        "{}",
        index.evidence
    );
    let json = advice.to_json();
    assert!(json.contains(r#""label":null"#), "{json}");
    // Applying it ends the advice; the recommendation's id applies the same definition.
    graph
        .apply_recommendations(&[index.id.as_str()], &AdviceRequest::everything(), None)
        .unwrap();
    check(&graph);
    assert_eq!(graph.read().property_indexes().unwrap()[0].label, None);
    let after = graph.advise(&AdviceRequest::everything()).unwrap();
    assert!(
        !after
            .recommendations
            .iter()
            .any(|r| r.id.starts_with("create_index:vertex:*:name"))
    );
    // A labelled scan of the same key is served by it: no per-label advice either.
    for _ in 0..3 {
        let _ = ok(
            &graph,
            r#"g.v().has_label("person").has("name", "josh").values("age").to_list()"#,
        );
    }
    let after = graph.advise(&AdviceRequest::everything()).unwrap();
    assert!(
        !after
            .recommendations
            .iter()
            .any(|r| r.id == "create_index:vertex:person:name")
    );
    // A per-label index on the same key: the advisor points out the entries kept twice.
    let _ = ok(&graph, r#"g.create_index(#{label: "person", key: "name"})"#);
    let after = graph.advise(&AdviceRequest::everything()).unwrap();
    let twice = after
        .recommendations
        .iter()
        .find(|r| r.id == "drop_index:person_name")
        .unwrap_or_else(|| panic!("{:?}", after.recommendations));
    assert!(twice.title.contains("label-less index"), "{}", twice.title);
}

// ---------------------------------------------------------------------------------------------
// Stores: the definition travels as kind 6 (critical), the entries are rebuilt at every load
// ---------------------------------------------------------------------------------------------

#[cfg(all(feature = "persist", not(target_arch = "wasm32")))]
mod stores {
    use super::*;
    use graphersal::persist::{MemDir, RecoveryTarget, Store, StoreDir, StoreOptions};

    fn options() -> StoreOptions {
        StoreOptions::new()
            .with_wal_segment_bytes(600)
            .with_checkpoint_after_bytes(0)
    }

    fn served(graph: &TraversalGraph, entries: u64) {
        graph.check_property_indexes().unwrap();
        let stored = graph
            .definition(DefinitionKind::PROPERTY_INDEX_EXT, "owner")
            .unwrap()
            .expect("kind 6 after a load");
        assert!(stored.flags().is_critical());
        let infos = graph.property_indexes().unwrap();
        assert_eq!(infos.len(), 1, "{infos:?}");
        assert_eq!(infos[0].label, None);
        assert_eq!(infos[0].stats.entries, entries);
        assert_eq!(
            graph
                .count_by_property_index(
                    ElementKind::Edge,
                    None,
                    "_owner",
                    None,
                    &PropertyIndexRange::equal("o1".into())
                )
                .unwrap(),
            Some(entries - 1)
        );
    }

    #[test]
    fn the_critical_kind_is_persisted_and_reloaded() {
        let main: Arc<dyn StoreDir> = Arc::new(MemDir::named("pindex-ll-main"));
        let store = Store::create(&main, options()).unwrap();
        let graph = store.graph().clone();
        let _ = ok(
            &graph,
            r#"g.add_v("p").property("n", 1).iterate();
               g.add_v("q").property("n", 2).iterate();
               g.v().has("n", 1).add_e("r").to(__.V().has("n", 2)).property("_owner", "o1").iterate();
               g.v().has("n", 2).add_e("s").to(__.V().has("n", 1)).property("_owner", "o2").iterate();"#,
        );
        let _ = ok(
            &graph,
            r#"g.create_index(#{element: "edge", key: "_owner", name: "owner"})"#,
        );
        let _ = ok(
            &graph,
            r#"g.v().has("n", 1).add_e("t").to(__.V().has("n", 2)).property("_owner", "o1").iterate()"#,
        );
        served(&graph.read(), 3);
        store.close().unwrap();
        // Reopen from the WAL, then from a checkpoint.
        let store = Store::open(&main, options()).unwrap();
        served(&store.graph().read(), 3);
        assert!(store.read_only_reason().unwrap().is_none());
        store.checkpoint(None).unwrap();
        store.close().unwrap();
        let store = Store::open(&main, options()).unwrap();
        served(&store.graph().read(), 3);
        let latest = Store::open_read_only(&main, RecoveryTarget::Latest).unwrap();
        served(&latest.graph, 3);
        // A backup carries it.
        let backup: Arc<dyn StoreDir> = Arc::new(MemDir::named("pindex-ll-backup"));
        store.backup(&backup).unwrap();
        let opened = Store::open_backup(&backup, options()).unwrap();
        served(&opened.graph().read(), 3);
        drop(opened);
        store.close().unwrap();
    }

    #[test]
    fn a_packed_snapshot_keeps_the_kind() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());
        let _ = ok(&graph, r#"g.create_index(#{key: "name"})"#);
        let mut bytes = Vec::new();
        graphersal::persist::write_snapshot(
            &*graph.read(),
            &mut bytes,
            &graphersal::persist::WriteOptions::default(),
        )
        .unwrap();
        let loaded = graphersal::persist::read_snapshot(
            bytes.as_slice(),
            &graphersal::persist::ReadOptions::default(),
        )
        .unwrap();
        loaded.check_property_indexes().unwrap();
        let stored = loaded
            .definition(DefinitionKind::PROPERTY_INDEX_EXT, "name")
            .unwrap()
            .unwrap();
        assert!(stored.flags().is_critical());
        assert_eq!(loaded.property_indexes().unwrap()[0].stats.entries, 6);
    }
}

// ---------------------------------------------------------------------------------------------
// A foreign storage
// ---------------------------------------------------------------------------------------------

#[test]
fn the_forwarding_storage_serves_the_same_label_less_index() {
    use crate::forwarding::Forwarding;
    let forwarded = Arc::new(Graph::new(Forwarding::new(
        TraversalGraph::tinkerpop_modern(),
    )));
    let on_forwarded = |script: &str| {
        eval_value_with_limits(
            forwarded.clone(),
            script,
            HashMap::new(),
            &ScriptLimits::default(),
        )
        .map(|value| value.to_string())
        .map_err(|err| err.to_string())
    };
    on_forwarded(r#"g.create_index(#{key: "name"})"#).unwrap();
    assert_eq!(
        on_forwarded(r#"g.v().has("name", "josh").values("age").to_list()"#).unwrap(),
        "[32]"
    );
    assert_eq!(
        on_forwarded(r#"g.v().has("name", "josh").count().next()"#).unwrap(),
        "1"
    );
    let profile =
        on_forwarded(r#"g.v().has("name", "josh").count().profile().to_string()"#).unwrap();
    assert!(
        profile.contains("[index: vertex name, any label] [count from index]"),
        "{profile}"
    );
}
