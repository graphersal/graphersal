//! Property indexes with the transform `each` and transform chains (book page "Property
//! Indexes", section "Arrays: the each transform"; design section 4, D8/D15 of the property
//! index extensions): an array value gives one entry per element, `has(key,
//! __.unfold().is(P))` reads the index, `has(key, __.unfold().to_lower().is(P))` reads an
//! `["each", "to_lower"]` index while it skipped nothing, and a condition on the whole value
//! (`has(key, x)`, `has(key, P)`) reads an `each` index as a superset.
//!
//! The oracle is the scan, as in `property_index_tests.rs`: every query runs with and without
//! the index (`g.with("optimizer.disabled", ["property_index"])`), and the maintained entries
//! are compared with a fresh rebuild (`check_property_indexes`) after every write.
#![cfg(feature = "script")]

use std::collections::HashMap;
use std::sync::Arc;

use graphersal::catalog::{
    Definition, DefinitionKind, PropertyIndexDefinition, PropertyIndexTransform,
};
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

/// Runs `script` for its effect.
fn exec(graph: &Arc<Graph>, script: &str) {
    let _ = ok(graph, script);
}

fn check(graph: &Graph) {
    graph.read().check_property_indexes().unwrap();
}

/// The rendered values of a list result, in order.
fn listed(value: Dynamic) -> Vec<String> {
    value
        .into_array()
        .expect("a list")
        .into_iter()
        .map(|item| item.to_string())
        .collect()
}

/// `query` (after `g`, no terminal) with and without the index: the sorted ids. The indexed
/// result never holds an element twice.
fn both(graph: &Arc<Graph>, query: &str) -> (Vec<String>, Vec<String>) {
    let mut with = listed(ok(graph, &format!("g.{query}.id().to_list()")));
    let distinct: std::collections::HashSet<&String> = with.iter().collect();
    assert_eq!(
        distinct.len(),
        with.len(),
        "{query}: an element twice: {with:?}"
    );
    with.sort();
    let mut without = listed(ok(
        graph,
        &format!("g.with(\"optimizer.disabled\", [\"property_index\"]).{query}.id().to_list()"),
    ));
    without.sort();
    (with, without)
}

/// The count of `query` with and without the index.
fn counts(graph: &Arc<Graph>, query: &str) -> (i64, i64) {
    let count = format!("{query}.count().next()");
    (
        ok(graph, &format!("g.{count}")).as_int().unwrap(),
        ok(
            graph,
            &format!("g.with(\"optimizer.disabled\", [\"property_index\"]).{count}"),
        )
        .as_int()
        .unwrap(),
    )
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

fn list(items: &[ElementProperty]) -> ElementProperty {
    ElementProperty::Array(items.to_vec())
}

fn object(key: &str, value: ElementProperty) -> ElementProperty {
    ElementProperty::Object(Box::new(props(&[(key, value)])))
}

/// One random `tags` value: an array of up to four elements (strings, numbers, null, a nested
/// array, an object), a scalar, an object, a vector, or null.
fn random_tags(rng: &mut Rng) -> ElementProperty {
    let pool: Vec<ElementProperty> = vec![
        "x".into(),
        "y".into(),
        "z".into(),
        "X".into(),
        1_i64.into(),
        1.0_f64.into(),
        2_i64.into(),
        ElementProperty::Null,
        list(&["x".into()]),
        object("k", "x".into()),
    ];
    match rng.below(8) {
        0 => pool[rng.below(4)].clone(),
        1 => 1_i64.into(),
        2 => object("x", "x".into()),
        3 => ElementProperty::Null,
        _ => {
            let len = rng.below(5);
            list(
                &(0..len)
                    .map(|_| pool[rng.below(pool.len())].clone())
                    .collect::<Vec<_>>(),
            )
        }
    }
}

/// Vertices labelled `a`, `b`, both or none with a random `tags` (sometimes missing), and
/// edges with one.
fn random_graph(rng: &mut Rng, vertices: usize) -> TraversalGraph {
    let label_sets: [&[&str]; 4] = [&["a"], &["b"], &["a", "b"], &[]];
    let mut graph = TraversalGraph::new();
    let mut handles = Vec::new();
    for _ in 0..vertices {
        let labels: Vec<CompactString> = label_sets[rng.below(label_sets.len())]
            .iter()
            .map(|l| CompactString::from(*l))
            .collect();
        let mut map = PropertyMap::default();
        if rng.below(6) != 0 {
            map.insert("tags", random_tags(rng));
        }
        handles.push(graph.add_vertex(None, labels, map).unwrap());
    }
    for _ in 0..vertices {
        let from = handles[rng.below(handles.len())];
        let to = handles[rng.below(handles.len())];
        let tags = random_tags(rng);
        graph
            .add_edge(None, Some("r".into()), from, to, props(&[("tags", tags)]))
            .unwrap();
    }
    graph
}

/// The queries compared with the scan on a random graph.
const QUERIES: [&str; 17] = [
    r#"v().has("tags", __.unfold().is("x"))"#,
    r#"v().has("tags", __.unfold().is(P.within(["x", "z"])))"#,
    r#"v().has("tags", __.unfold().is(P.gte("y")))"#,
    r#"v().has("tags", __.unfold().is(P.lt("y")))"#,
    r#"v().has("tags", __.unfold().is(P.between(0, 2)))"#,
    r#"v().has("tags", __.unfold().is(P.outside(1, 1.5)))"#,
    r#"v().has("tags", __.unfold().is(1))"#,
    r#"v().has("tags", __.unfold().is(P.eq(())))"#,
    r#"v().has("tags", "x")"#,
    r#"v().has("tags", P.within(["x", 1]))"#,
    r#"v().has("tags", P.gt(0))"#,
    r#"v().has_label("a").has("tags", __.unfold().is("x"))"#,
    r#"v().has_label("a", "b").has("tags", __.unfold().is(P.within(["x", "y"])))"#,
    r#"v().has_label("b").has("tags", "y")"#,
    r#"e().has("tags", __.unfold().is("x"))"#,
    r#"e().has("tags", __.unfold().is(P.inside(0, 3)))"#,
    r#"e().has_label("r").has("tags", "z")"#,
];

// ---------------------------------------------------------------------------------------------
// The superset argument: what unfold() emits for values the index skips
// ---------------------------------------------------------------------------------------------

/// `unfold()` of an object value emits its entries, of a vector the vector itself, of a nested
/// array the nested array: none of them equals or ranges with a scalar constant (strings,
/// numbers, null, ranges of both), so an `each` index that skips them misses no match.
#[test]
fn values_the_each_index_skips_never_match_a_scalar_predicate_under_unfold() {
    let graph = Arc::new(GraphSource::empty());
    exec(
        &graph,
        r#"g.add_v("o").property("tags", #{x: "x", n: 1}).iterate();
           g.add_v("v").property("tags", vector([1.0, 2.0])).iterate();
           g.add_v("n").property("tags", [["x"], [1], #{x: "x"}]).iterate();"#,
    );
    // The object unfolds into entries, the vector passes through whole.
    assert_eq!(
        ok(
            &graph,
            r#"g.v().has_label("o").values("tags").unfold().count().next()"#
        )
        .as_int(),
        Ok(2)
    );
    assert_eq!(
        ok(
            &graph,
            r#"g.v().has_label("v").values("tags").unfold().count().next()"#
        )
        .as_int(),
        Ok(1)
    );
    for predicate in [
        r#""x""#,
        "1",
        "1.0",
        "()",
        r#"P.within(["x", 1, 2.0])"#,
        r#"P.gt("")"#,
        r#"P.lte("zzz")"#,
        "P.gt(0)",
        "P.lt(10)",
        "P.between(-5, 5)",
        "P.outside(100, 200)",
        r#"P.inside("a", "z")"#,
    ] {
        let predicate = if predicate == "()" {
            "P.eq(())"
        } else {
            predicate
        };
        let query = format!(r#"g.v().has("tags", __.unfold().is({predicate})).count().next()"#);
        assert_eq!(ok(&graph, &query).as_int(), Ok(0), "{query}");
    }
    // With an `each` index they are all skipped, and the served answers are the same.
    exec(
        &graph,
        r#"g.create_index(#{key: "tags", transform: "each"})"#,
    );
    check(&graph);
    let info = &graph.read().property_indexes().unwrap()[0];
    assert_eq!(info.stats.entries, 0);
    assert_eq!(
        info.stats.skipped,
        1 + 1 + 3,
        "the object, the vector, three elements"
    );
    for query in QUERIES.iter().filter(|query| query.starts_with("v()")) {
        let (with, without) = both(&graph, query);
        assert_eq!(with, without, "{query}");
    }
}

// ---------------------------------------------------------------------------------------------
// Entries, deduplication, skipped, the definition
// ---------------------------------------------------------------------------------------------

#[test]
fn entries_are_per_distinct_element_and_skipped_counts_unheld_elements() {
    let mut graph = TraversalGraph::new();
    graph
        .add_vertex(
            None,
            vec!["t".into()],
            props(&[("tags", list(&["x".into(), "y".into(), "x".into()]))]),
        )
        .unwrap();
    graph
        .add_vertex(None, vec!["t".into()], props(&[("tags", "x".into())]))
        .unwrap();
    graph
        .add_vertex(
            None,
            vec!["t".into()],
            props(&[(
                "tags",
                list(&[
                    list(&["x".into()]),
                    object("k", "x".into()),
                    ElementProperty::Null,
                    1_i64.into(),
                    1.0_f64.into(),
                ]),
            )]),
        )
        .unwrap();
    graph
        .add_vertex(None, vec!["t".into()], props(&[("tags", list(&[]))]))
        .unwrap();
    graph
        .add_vertex(
            None,
            vec!["t".into()],
            props(&[("tags", ElementProperty::Null)]),
        )
        .unwrap();
    let graph = Arc::new(Graph::new(graph));
    exec(
        &graph,
        r#"g.create_index(#{label: "t", key: "tags", transform: "each"})"#,
    );
    check(&graph);
    let infos = graph.read().property_indexes().unwrap();
    let stats = infos[0].stats;
    // x, y | x | null, 1, 1.0 | - | null: 7 entries over x, y, null, 1, 1.0.
    assert_eq!(stats.entries, 7, "{stats:?}");
    assert_eq!(stats.distinct_values, 5, "{stats:?}");
    assert_eq!(stats.skipped, 2, "the nested array and the object");
    // The index listing shows the transform and the kind.
    let row = ok(&graph, "g.indexes()").into_array().unwrap()[0].clone();
    let row = row.try_cast::<graphersal::script::rhai::Map>().unwrap();
    assert_eq!(row["transform"].to_string(), "each");
    assert_eq!(row["name"].to_string(), "t_tags_each");
    let stored = graph
        .read()
        .definition(DefinitionKind::PROPERTY_INDEX_EXT, "t_tags_each")
        .unwrap()
        .cloned()
        .expect("a per-label each index is kind 6");
    assert!(stored.flags().is_critical());
    // The storage lookup: a point once per element, a range over x and y deduplicated, a
    // number point over the keys 1 and 1.0 deduplicated.
    let read = graph.read();
    let count = |range: &PropertyIndexRange| {
        read.vertices_by_transformed_property_index(
            Some("t"),
            "tags",
            &PropertyIndexTransform::Each,
            range,
        )
        .unwrap()
        .expect("served")
        .count()
    };
    assert_eq!(count(&PropertyIndexRange::equal("x".into())), 2);
    assert_eq!(
        count(&PropertyIndexRange::new(
            std::ops::Bound::Included("a".into()),
            std::ops::Bound::Unbounded
        )),
        2
    );
    assert_eq!(count(&PropertyIndexRange::equal(1_i64.into())), 1);
    // Count from the postings: exact for one key, refused for several.
    assert_eq!(
        read.count_by_property_index(
            ElementKind::Vertex,
            Some("t"),
            "tags",
            Some(&PropertyIndexTransform::Each),
            &PropertyIndexRange::equal("x".into())
        )
        .unwrap(),
        Some(2)
    );
    assert_eq!(
        read.count_by_property_index(
            ElementKind::Vertex,
            Some("t"),
            "tags",
            Some(&PropertyIndexTransform::Each),
            &PropertyIndexRange::equal(1_i64.into())
        )
        .unwrap(),
        None,
        "1 and 1.0 are two keys of one element"
    );
}

#[test]
fn the_chain_payload_is_an_array_and_the_wrong_order_is_refused() {
    let graph = Arc::new(GraphSource::empty());
    exec(
        &graph,
        r#"g.add_v("t").property("tags", ["Rust", "Go"]).iterate()"#,
    );
    exec(
        &graph,
        r#"g.create_index(#{label: "t", key: "tags", transform: ["each", "to_lower"]})"#,
    );
    check(&graph);
    let row = ok(&graph, "g.indexes()").into_array().unwrap()[0].clone();
    let row = row.try_cast::<graphersal::script::rhai::Map>().unwrap();
    assert_eq!(row["transform"].to_string(), r#"["each", "to_lower"]"#);
    assert_eq!(row["name"].to_string(), "t_tags_each_to_lower");
    let stored = graph
        .read()
        .definition(DefinitionKind::PROPERTY_INDEX_EXT, "t_tags_each_to_lower")
        .unwrap()
        .cloned()
        .expect("kind 6");
    let index = stored.as_property_index().unwrap();
    assert_eq!(
        index.to_payload().get("transform"),
        Some(&list(&["each".into(), "to_lower".into()]))
    );
    assert_eq!(
        PropertyIndexDefinition::from_payload(&index.to_payload()).unwrap(),
        *index
    );
    // A single name stays a string; `["each"]` is `each`.
    exec(
        &graph,
        r#"g.create_index(#{label: "t", key: "tags", transform: ["each"], name: "one"})"#,
    );
    let one = graph
        .read()
        .definition(DefinitionKind::PROPERTY_INDEX_EXT, "one")
        .unwrap()
        .cloned()
        .unwrap();
    assert_eq!(
        one.as_property_index()
            .unwrap()
            .to_payload()
            .get("transform"),
        Some(&"each".into())
    );
    // The wrong order, an unknown member, an empty chain and a non-string member are refused.
    for (spec, part) in [
        (
            r#"["to_lower", "each"]"#,
            r#"the chain ["to_lower", "each"] is not one this version serves"#,
        ),
        (r#"["each", "each"]"#, "is not one this version serves"),
        (
            r#"["each", "unaccent"]"#,
            "'unaccent' is not an index transform this version knows",
        ),
        ("[]", "an empty array"),
        (r#"["each", 1]"#, "an array holding"),
    ] {
        let err = run(
            &graph,
            &format!(r#"g.create_index(#{{label: "t", key: "tags", transform: {spec}}})"#),
        )
        .unwrap_err();
        assert!(err.contains(part), "{spec}: {err}");
    }
    // A definition stored with the wrong order (a newer writer, set_definition) is kept, never
    // served, and listed with the reason.
    let wrong = PropertyIndexDefinition::new(ElementKind::Vertex, "t", "tags")
        .with_transform(PropertyIndexTransform::from_names(["to_lower", "each"]));
    assert!(!wrong.is_served_type_public());
    graph
        .write()
        .set_definition(Definition::property_index("wrong", wrong))
        .unwrap();
    check(&graph);
    let rows = ok(&graph, "g.indexes()").into_array().unwrap();
    let wrong = rows
        .iter()
        .map(|row| {
            row.clone()
                .try_cast::<graphersal::script::rhai::Map>()
                .unwrap()
        })
        .find(|row| row["name"].to_string() == "wrong")
        .unwrap();
    assert!(!wrong["served"].as_bool().unwrap());
    assert!(
        wrong["reason"]
            .to_string()
            .contains("not one this version serves"),
        "{wrong:?}"
    );
}

/// What the old `is_served_type` check says, through the public API: the listing of a
/// definition that is never built.
trait ServedType {
    fn is_served_type_public(&self) -> bool;
}

impl ServedType for PropertyIndexDefinition {
    fn is_served_type_public(&self) -> bool {
        self.unserved_reason().is_none()
    }
}

#[test]
fn the_schema_check_looks_at_the_array_item_type() {
    let graph = Arc::new(GraphSource::empty());
    let _ = ok(
        &graph,
        r#"g.set_schema(#{mode: "open", vertices: #{doc: #{schema: #{type: "object", properties: #{
             tags: #{type: "array", items: #{type: "string"}},
             nums: #{type: "array", items: #{type: "integer"}},
             any: #{type: "array"},
             objs: #{type: "array", items: #{type: "object"}},
             meta: #{type: "object"}}}}}})"#,
    );
    for ok_spec in [
        r#"#{label: "doc", key: "tags", transform: "each"}"#,
        r#"#{label: "doc", key: "tags", transform: ["each", "to_lower"]}"#,
        r#"#{label: "doc", key: "nums", transform: "each"}"#,
        r#"#{label: "doc", key: "any", transform: "each"}"#,
        r#"#{label: "doc", key: "any", transform: ["each", "to_lower"]}"#,
        r#"#{key: "tags", transform: "each"}"#,
    ] {
        exec(&graph, &format!("g.create_index({ok_spec})"));
    }
    for (spec, part) in [
        (
            r#"#{label: "doc", key: "nums", transform: ["each", "to_lower"]}"#,
            "with the transform 'each to_lower' holds none of its values",
        ),
        (
            r#"#{label: "doc", key: "objs", transform: "each"}"#,
            "with the transform 'each' holds none of its values",
        ),
        (
            r#"#{label: "doc", key: "meta", transform: "each"}"#,
            "with the transform 'each' holds none of its values",
        ),
        // A plain index on an array: the help names the each transform.
        (
            r#"#{label: "doc", key: "tags"}"#,
            r#"g.create_index(#{label: "doc", key: "tags", transform: "each"})"#,
        ),
    ] {
        let err = run(&graph, &format!("g.create_index({spec})")).unwrap_err();
        assert!(err.contains(part), "{spec}: {err}");
    }
}

// ---------------------------------------------------------------------------------------------
// The planner: served shapes, rendering, superset, preference, to_lower chain
// ---------------------------------------------------------------------------------------------

fn owners() -> Arc<Graph> {
    Arc::new(owners_graph())
}

fn owners_graph() -> Graph {
    let mut graph = TraversalGraph::new();
    for i in 0..60_i64 {
        let label = if i % 3 == 0 { "Method" } else { "Type" };
        let owners: Vec<ElementProperty> = match i % 4 {
            0 => vec!["o1".into()],
            1 => vec!["o1".into(), "o2".into()],
            2 => vec!["o2".into(), "O3".into()],
            _ => vec!["o3".into()],
        };
        graph
            .add_vertex(
                None,
                vec![label.into()],
                props(&[("_owners", list(&owners)), ("n", i.into())]),
            )
            .unwrap();
    }
    Graph::new(graph)
}

#[test]
fn the_owner_query_reads_the_each_index_and_profile_names_it() {
    let graph = owners();
    exec(
        &graph,
        r#"g.create_index(#{key: "_owners", transform: "each"})"#,
    );
    check(&graph);
    for query in [
        r#"v().has("_owners", __.unfold().is("o1"))"#,
        r#"v().has_label("Method").has("_owners", __.unfold().is("o2"))"#,
        r#"v().has("_owners", __.unfold().is(P.within(["o1", "o3"])))"#,
        r#"v().has("_owners", __.unfold().is(P.gte("o2")))"#,
    ] {
        // A range keeps its re-check; the points are answered exactly.
        let expected = if query.contains("P.gte") {
            ": vertex _owners each, any label]"
        } else {
            ": vertex _owners each, any label, exact]"
        };
        assert_eq!(
            first_step(&graph, query).split(" [index").nth(1),
            Some(expected),
            "{query}"
        );
        let (with, without) = both(&graph, query);
        assert_eq!(with, without, "{query}");
        assert!(!with.is_empty(), "{query}");
        let (with, without) = counts(&graph, query);
        assert_eq!(with, without, "{query}");
    }
    // An each count is never answered from the postings (it is not exact).
    let steps = profile_steps(&graph, r#"v().has("_owners", "o1").count()"#);
    assert!(
        !steps.iter().any(|step| step.contains("count from index")),
        "{steps:?}"
    );
    // A per-label each index renders like every per-label index.
    exec(
        &graph,
        r#"g.create_index(#{label: "Method", key: "_owners", transform: "each"})"#,
    );
    assert!(
        first_step(
            &graph,
            r#"v().has_label("Method").has("_owners", __.unfold().is("o2"))"#
        )
        .ends_with("[index: vertex Method._owners each, exact]"),
    );
    // Not served: another child, a negation, the wrong transform order in the query.
    for query in [
        r#"v().has("_owners", __.unfold().is(P.neq("o1")))"#,
        r#"v().has("_owners", __.unfold().unfold().is("o1"))"#,
        r#"v().has("_owners", __.unfold().to_lower().is("o1"))"#,
    ] {
        assert!(!first_step(&graph, query).contains("[index"), "{query}");
        let (with, without) = both(&graph, query);
        assert_eq!(with, without, "{query}");
    }
}

#[test]
fn a_whole_value_condition_reads_an_each_index_as_a_superset_and_prefers_an_exact_index() {
    let graph = owners();
    exec(
        &graph,
        r#"g.add_v("Type").property("_owners", "o1").iterate();
           g.add_v("Type").property("_owners", ["o1"]).iterate();"#,
    );
    exec(
        &graph,
        r#"g.create_index(#{key: "_owners", transform: "each"})"#,
    );
    for query in [
        r#"v().has("_owners", "o1")"#,
        r#"v().has("_owners", P.within(["o1", "o2"]))"#,
        r#"v().has("_owners", P.gt("o"))"#,
    ] {
        assert!(
            first_step(&graph, query).ends_with("[index: vertex _owners each, any label]"),
            "{query}: {:?}",
            profile_steps(&graph, query)
        );
        let (with, without) = both(&graph, query);
        assert_eq!(with, without, "{query}");
    }
    let found = listed(ok(
        &graph,
        r#"g.v().has("_owners", "o1").count().to_list()"#,
    ));
    assert_eq!(found, vec!["1"], "only the scalar");
    // With a plain index on the key too, the exact one serves the whole value.
    exec(&graph, r#"g.create_index(#{key: "_owners"})"#);
    assert!(
        first_step(&graph, r#"v().has("_owners", "o1")"#)
            .ends_with("[index: vertex _owners, any label, exact]"),
    );
    assert!(
        first_step(&graph, r#"v().has("_owners", __.unfold().is("o1"))"#)
            .ends_with("[index: vertex _owners each, any label, exact]"),
    );
}

#[test]
fn the_each_to_lower_chain_serves_only_while_nothing_is_skipped() {
    let graph = Arc::new(GraphSource::empty());
    exec(
        &graph,
        r#"g.add_v("t").property("tags", ["Rust", "go"]).iterate();
           g.add_v("t").property("tags", "RUST").iterate();
           g.add_v("t").property("tags", ["Java", ()]).iterate();
           g.add_v("t").property("tags", []).iterate();"#,
    );
    exec(
        &graph,
        r#"g.create_index(#{label: "t", key: "tags", transform: ["each", "to_lower"]})"#,
    );
    check(&graph);
    let query = r#"v().has_label("t").has("tags", __.unfold().to_lower().is("rust"))"#;
    assert!(
        first_step(&graph, query).ends_with("[index: vertex t.tags each to_lower, exact]"),
        "{:?}",
        profile_steps(&graph, query)
    );
    let (with, without) = both(&graph, query);
    assert_eq!(with, without);
    assert_eq!(with.len(), 2);
    let ranged = r#"v().has_label("t").has("tags", __.unfold().to_lower().is(P.lt("z")))"#;
    let (with, without) = both(&graph, ranged);
    assert_eq!(with, without);
    // A null constant is never served by a to_lower transform (to_lower() passes a null
    // through, the index holds none).
    let null = r#"v().has_label("t").has("tags", __.unfold().to_lower().is(P.eq(())))"#;
    assert!(!first_step(&graph, null).contains("[index"));
    let (with, without) = both(&graph, null);
    assert_eq!(with, without);
    assert_eq!(with.len(), 1);
    // A number element: skipped, the chain is not served and the scan reports the error.
    exec(
        &graph,
        r#"g.add_v("t").property("tags", ["c", 7]).iterate()"#,
    );
    check(&graph);
    assert_eq!(graph.read().property_indexes().unwrap()[0].stats.skipped, 1);
    // (A served plan would not reach the number: the error shows the scan ran.)
    let err = run(&graph, &format!("g.{query}.to_list()")).unwrap_err();
    assert!(err.contains("to_lower"), "{err}");
}

/// The same null rule for a plain `to_lower` index (a pre-existing gap: the index held no null
/// while `to_lower()` passes one through).
#[test]
fn a_to_lower_index_never_serves_a_null_constant() {
    let graph = Arc::new(GraphSource::empty());
    exec(
        &graph,
        r#"g.add_v("a").property("n", ()).iterate();
           g.add_v("a").property("n", "X").iterate();"#,
    );
    exec(
        &graph,
        r#"g.create_index(#{label: "a", key: "n", transform: "to_lower"})"#,
    );
    for query in [
        r#"v().has_label("a").has("n", __.to_lower().is(P.eq(())))"#,
        r#"v().has_label("a").has("n", __.to_lower().is(P.within([(), "x"])))"#,
    ] {
        assert!(!first_step(&graph, query).contains("[index"), "{query}");
        let (with, without) = both(&graph, query);
        assert_eq!(with, without, "{query}");
    }
    assert!(
        first_step(
            &graph,
            r#"v().has_label("a").has("n", __.to_lower().is("x"))"#
        )
        .contains("[index")
    );
}

// ---------------------------------------------------------------------------------------------
// Random data, writes and rollbacks
// ---------------------------------------------------------------------------------------------

#[test]
fn random_data_queries_equal_the_scan_and_writes_keep_it_exact() {
    for seed in [5_u64, 17, 41] {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let graph = Arc::new(Graph::new(random_graph(&mut rng, 90)));
        exec(
            &graph,
            r#"g.create_index(#{key: "tags", transform: "each"})"#,
        );
        exec(
            &graph,
            r#"g.create_index(#{label: "a", key: "tags", transform: "each"})"#,
        );
        exec(
            &graph,
            r#"g.create_index(#{element: "edge", key: "tags", transform: "each"})"#,
        );
        exec(
            &graph,
            r#"g.create_index(#{key: "tags", transform: ["each", "to_lower"]})"#,
        );
        check(&graph);
        let writes = [
            r#"g.v().has("tags", __.unfold().is("x")).limit(2).property("tags", ["y", "y", 2]).iterate()"#,
            r#"g.v().has("tags", __.unfold().is("y")).limit(1).properties("tags").drop().iterate()"#,
            r#"g.add_v("a").property("tags", ["x", "z", ["x"]]).iterate()"#,
            r#"g.add_v().property("tags", "x").iterate()"#,
            r#"g.v().has("tags", __.unfold().is("z")).limit(1).drop().iterate()"#,
            r#"g.v().has_label("b").limit(1).add_label("a").iterate()"#,
            r#"g.v().has_label("a").limit(1).drop_label("a").iterate()"#,
            r#"g.e().has("tags", __.unfold().is("x")).limit(1).property("tags", [1, 1.0]).iterate()"#,
            r#"g.e().limit(1).drop().iterate()"#,
            r#"g.v().has("tags", __.unfold().is("x")).limit(1).property("tags", "q").v().fail("roll back").iterate()"#,
            r#"g.add_v("a").property("tags", ["x"]).v().fail("roll back").iterate()"#,
            r#"g.v().limit(3).property("tags", #{x: "x"}).v().fail("roll back").iterate()"#,
        ];
        for step in 0..48 {
            let write = writes[rng.below(writes.len())];
            let _ = run(&graph, write);
            check(&graph);
            let query = QUERIES[step % QUERIES.len()];
            let (with, without) = both(&graph, query);
            assert_eq!(with, without, "seed {seed}, after {write}: {query}");
            let (with, without) = counts(&graph, query);
            assert_eq!(with, without, "seed {seed}, after {write}: {query}.count()");
        }
        // The owner shape and the whole-value shape are served on this data.
        assert!(
            first_step(&graph, QUERIES[0]).contains("[index: vertex tags each, any label, exact]")
        );
        assert!(first_step(&graph, QUERIES[8]).contains("each, any label]"));
        assert!(
            first_step(&graph, QUERIES[14]).contains("[index: edge tags each, any label, exact]")
        );
    }
}

#[test]
fn a_rollback_restores_every_each_entry() {
    let graph = owners();
    exec(
        &graph,
        r#"g.create_index(#{key: "_owners", transform: "each"})"#,
    );
    let before = graph.read().property_indexes().unwrap()[0].stats;
    for write in [
        r#"g.v().has("_owners", __.unfold().is("o1")).property("_owners", ["o9", "o9", 3]).
             fail("roll back").iterate()"#,
        r#"g.add_v("Type").property("_owners", ["o1", "o4"]).fail("roll back").iterate()"#,
        r#"g.v().has("n", 1).side_effect(__.drop()).fail("roll back").iterate()"#,
    ] {
        assert!(run(&graph, write).is_err(), "{write}");
        check(&graph);
    }
    // The same entries (the runs' capacities may differ from the build's).
    let after = graph.read().property_indexes().unwrap()[0].stats;
    assert_eq!(
        (after.entries, after.distinct_values, after.skipped),
        (before.entries, before.distinct_values, before.skipped)
    );
    let (with, without) = both(&graph, r#"v().has("_owners", __.unfold().is("o1"))"#);
    assert_eq!(with, without);
    assert_eq!(with.len(), 30);
}

// ---------------------------------------------------------------------------------------------
// Advisor, GQL, foreign storage, persistence
// ---------------------------------------------------------------------------------------------

#[test]
fn the_advisor_recommends_each_for_a_key_holding_arrays() {
    use graphersal::advisor::{AdviceRequest, RecommendedAction};
    use graphersal::workload::{WorkloadLevel, WorkloadStats};
    let graph = owners_graph();
    graph.attach_workload_stats(Arc::new(WorkloadStats::new(WorkloadLevel::Memory)));
    let graph = Arc::new(graph);
    for _ in 0..3 {
        exec(
            &graph,
            r#"g.v().has("_owners", __.unfold().is("o1")).values("n").to_list()"#,
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
        .find(|r| r.id == "create_index:vertex:*:_owners")
        .unwrap_or_else(|| panic!("{ids:?}"));
    assert!(
        matches!(
            &index.action,
            RecommendedAction::CreateIndex {
                transform: Some(PropertyIndexTransform::Each),
                ..
            }
        ),
        "{index:?}"
    );
    assert_eq!(
        index.action.command(),
        r#"g.create_index(#{key: "_owners", transform: "each"})"#
    );
    assert!(advice.to_json().contains(r#""transform":"each""#));
    graph
        .apply_recommendations(&[index.id.as_str()], &AdviceRequest::everything(), None)
        .unwrap();
    check(&graph);
    let infos = graph.read().property_indexes().unwrap();
    assert_eq!(infos[0].transform, Some(PropertyIndexTransform::Each));
    assert!(first_step(&graph, r#"v().has("_owners", __.unfold().is("o1"))"#).contains("each"));
    let after = graph.advise(&AdviceRequest::everything()).unwrap();
    assert!(
        !after
            .recommendations
            .iter()
            .any(|r| r.id == "create_index:vertex:*:_owners")
    );
}

/// GQL's `x IN n.list` is a `gql:filter` expression (a non-list right operand is an error
/// there, 22G03), so it is not served by an `each` index; its answers stay the scan's.
#[cfg(feature = "gql")]
#[test]
fn gql_in_on_a_list_property_answers_like_the_scan() {
    let graph = owners();
    exec(
        &graph,
        r#"g.create_index(#{key: "_owners", transform: "each"})"#,
    );
    let gql = |query: &str| listed(ok(&graph, &format!(r#"g.gql("{query}").to_list()"#)));
    assert_eq!(
        gql("MATCH (n) WHERE 'o1' IN n._owners RETURN count(*) AS c"),
        vec![r#"#{"c": 30}"#]
    );
}

#[test]
fn the_forwarding_storage_serves_the_same_each_index() {
    use crate::forwarding::Forwarding;
    let mut inner = TraversalGraph::new();
    for owners in [vec!["o1"], vec!["o1", "o2"], vec!["o2"]] {
        inner
            .add_vertex(
                None,
                vec!["m".into()],
                props(&[(
                    "_owners",
                    list(&owners.iter().map(|o| (*o).into()).collect::<Vec<_>>()),
                )]),
            )
            .unwrap();
    }
    let forwarded = Arc::new(Graph::new(Forwarding::new(inner)));
    let on = |script: &str| {
        eval_value_with_limits(
            forwarded.clone(),
            script,
            HashMap::new(),
            &ScriptLimits::default(),
        )
        .map(|value| value.to_string())
        .map_err(|err| err.to_string())
    };
    on(r#"g.create_index(#{key: "_owners", transform: "each"})"#).unwrap();
    assert_eq!(
        on(r#"g.v().has("_owners", __.unfold().is("o1")).count().next()"#).unwrap(),
        "2"
    );
    assert_eq!(
        on(r#"g.v().has("_owners", __.unfold().is(P.within(["o1", "o2"]))).count().next()"#)
            .unwrap(),
        "3"
    );
    let profile =
        on(r#"g.v().has("_owners", __.unfold().is("o1")).profile().to_string()"#).unwrap();
    assert!(
        profile.contains("[index: vertex _owners each, any label, exact]"),
        "{profile}"
    );
}

#[cfg(all(feature = "persist", not(target_arch = "wasm32")))]
#[test]
fn a_snapshot_keeps_the_chain_and_rebuilds_the_entries() {
    let graph = owners();
    exec(
        &graph,
        r#"g.create_index(#{key: "_owners", transform: ["each", "to_lower"], name: "lower"})"#,
    );
    exec(
        &graph,
        r#"g.create_index(#{label: "Method", key: "_owners", transform: "each"})"#,
    );
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
    let mut infos = loaded.property_indexes().unwrap();
    infos.sort_by(|a, b| a.name.cmp(&b.name));
    assert_eq!(infos.len(), 2);
    let transform = |name: &str| {
        infos
            .iter()
            .find(|info| info.name == name)
            .and_then(|info| info.transform.clone())
    };
    assert_eq!(
        transform("lower"),
        PropertyIndexTransform::from_names(["each", "to_lower"])
    );
    assert_eq!(
        transform("Method__owners_each"),
        Some(PropertyIndexTransform::Each)
    );
    assert_eq!(infos, {
        let mut original = graph.read().property_indexes().unwrap();
        original.sort_by(|a, b| a.name.cmp(&b.name));
        original
    });
}
