//! `g.layer_diff()` / `g.layer_diff(#{kinds, labels, limit})` and
//! `GraphTraversalSource::layer_diff` (layered graph S2.3): what a layer changed against its
//! frozen base as one map value. A plain graph answers an empty diff; a `Graph<LayeredGraph>`
//! edited by Rhai scripts answers its added, removed (with the base edges a removed vertex took
//! along, carrying base values) and modified vertices and edges (properties added, changed and
//! removed, labels), a cross edge between base vertices, each option and its errors, and the
//! JSON form that pins the shape.
#![cfg(feature = "script")]

use std::collections::HashMap;
use std::sync::Arc;

use graphersal::prelude::*;
use graphersal::storage::{Graph, IntoAnyGraph, LayerDiffRequest, LayeredGraph};

use crate::script_host::{ScriptLimits, eval_value_with_limits};

/// `(script).to_string()` on `graph`, or the error text.
fn run(graph: impl IntoAnyGraph, script: &str) -> Result<String, String> {
    eval_value_with_limits(
        graph,
        &format!("({{ {script} }}).to_string()"),
        HashMap::new(),
        &ScriptLimits::default(),
    )
    .map(|value| value.to_string())
    .map_err(|err| err.to_string())
}

fn ok(graph: impl IntoAnyGraph, script: &str) -> String {
    run(graph, script).unwrap_or_else(|err| panic!("`{script}`: {err}"))
}

fn layered() -> Arc<Graph<LayeredGraph>> {
    let base = Arc::new(TraversalGraph::tinkerpop_modern());
    Arc::new(Graph::new(LayeredGraph::over(base).unwrap()))
}

/// The modern graph's layer after these edits:
/// - vertex `x1` (label `class`) and edge `e-new` (`x1` uses 1) added;
/// - vertex 1: `age` changed, `nick` added, label `admin` added;
/// - vertex 2: `age` removed;
/// - edge `0` (1 knows 2): `weight` changed;
/// - cross edge `cross` between the base vertices 2 and 3 added;
/// - vertex 6 removed, with its base edge.
fn edited() -> Arc<Graph<LayeredGraph>> {
    let graph = layered();
    for script in [
        r#"g.add_v("class").property(T.id, "x1").property("name", "c").iterate()"#,
        r#"g.add_e("uses").from(__.v("x1")).to(__.v("1")).property(T.id, "e-new").iterate()"#,
        r#"g.v("1").property("age", 30).property("nick", "m").add_label("admin").iterate()"#,
        r#"g.v("2").properties("age").drop().iterate()"#,
        r#"g.e("0").property("weight", 0.9).iterate()"#,
        r#"g.add_e("cross").from(__.v("2")).to(__.v("3")).property(T.id, "cross").iterate()"#,
        r#"g.v("6").drop().iterate()"#,
    ] {
        ok(graph.clone(), script);
    }
    graph
}

/// The ids of each list of `g.layer_diff(options)`, as `added | removed | modified | truncated`.
fn ids(graph: &Arc<Graph<LayeredGraph>>, options: &str) -> String {
    ok(
        graph.clone(),
        &format!(
            "let d = g.layer_diff({options}); \
             `${{d.added.map(|e| e.id)}} | ${{d.removed.map(|e| e.id)}} | \
             ${{d.modified.map(|e| e.id)}} | ${{d.truncated}}`"
        ),
    )
}

#[test]
fn a_plain_graph_has_an_empty_diff() {
    let graph = Arc::new(Graph::new(TraversalGraph::tinkerpop_modern()));
    ok(graph.clone(), r#"g.v("1").property("age", 30).iterate()"#);
    assert_eq!(
        ok(graph.clone(), "g.layer_diff()"),
        r#"#{"added": [], "modified": [], "removed": [], "truncated": false}"#
    );
    // Options are accepted (and checked) on a plain graph too.
    assert_eq!(
        ok(
            graph.clone(),
            r#"g.layer_diff(#{kinds: ["vertex", "edge"], labels: "person", limit: 1}).truncated"#
        ),
        "false"
    );
    let lock = graph.read();
    let diff = lock
        .traversal()
        .layer_diff(&LayerDiffRequest::new())
        .unwrap();
    assert!(diff.is_empty() && !diff.truncated);
    assert_eq!(
        diff.to_json(),
        r#"{"added":[],"removed":[],"modified":[],"truncated":false}"#
    );
}

#[test]
fn an_untouched_layer_has_an_empty_diff() {
    let graph = layered();
    assert_eq!(ids(&graph, ""), "[] | [] | [] | false");
}

#[test]
fn the_diff_lists_added_removed_and_modified_elements() {
    let graph = edited();
    assert_eq!(
        ids(&graph, ""),
        r#"["x1", "e-new", "cross"] | ["6", "5"] | ["1", "2", "0"] | false"#
    );
    // Added: the layer's labels and properties; edges with their endpoints (the cross edge
    // between two base vertices too) and a one-element label list.
    assert_eq!(
        ok(graph.clone(), "g.layer_diff().added[0]"),
        r#"#{"id": "x1", "kind": "vertex", "labels": ["class"], "properties": #{"name": "c"}}"#
    );
    assert_eq!(
        ok(graph.clone(), "g.layer_diff().added[2]"),
        r#"#{"id": "cross", "in": "3", "kind": "edge", "labels": ["cross"], "out": "2", "properties": #{}}"#
    );
    // Removed: the base's values, also for the base edge the removed vertex took along.
    assert_eq!(
        ok(graph.clone(), "g.layer_diff().removed"),
        r#"[#{"id": "6", "kind": "vertex", "labels": ["person"], "properties": #{"age": 35, "name": "peter"}}, #{"id": "5", "in": "3", "kind": "edge", "labels": ["created"], "out": "6", "properties": #{"weight": 0.2}}]"#
    );
    // Modified: a changed value has both sides, an added key only `layer`, a removed key only
    // `base`; the labels before and after.
    assert_eq!(
        ok(graph.clone(), "g.layer_diff().modified[0]"),
        r#"#{"base_labels": ["person"], "id": "1", "kind": "vertex", "labels": ["person", "admin"], "properties": #{"age": #{"base": 29, "layer": 30}, "nick": #{"layer": "m"}}}"#
    );
    assert_eq!(
        ok(graph.clone(), "g.layer_diff().modified[1].properties"),
        r#"#{"age": #{"base": 27}}"#
    );
    assert_eq!(
        ok(graph.clone(), "g.layer_diff().modified[2]"),
        r#"#{"base_labels": ["knows"], "id": "0", "in": "2", "kind": "edge", "labels": ["knows"], "out": "1", "properties": #{"weight": #{"base": 0.5, "layer": 0.9}}}"#
    );
    // The diff agrees with layer(): every added and modified element the layer can reach.
    assert_eq!(
        ok(
            graph.clone(),
            r#"[g.v().has_layer("added").count().next() + g.e().has_layer("added").count().next(),
                g.v().has_layer("modified").count().next() + g.e().has_layer("modified").count().next()]"#
        ),
        "[3, 3]"
    );
}

#[test]
fn the_kinds_option_selects_vertices_or_edges() {
    let graph = edited();
    let vertices = r#"["x1"] | ["6"] | ["1", "2"] | false"#;
    let edges = r#"["e-new", "cross"] | ["5"] | ["0"] | false"#;
    assert_eq!(ids(&graph, r#"#{kinds: "vertex"}"#), vertices);
    assert_eq!(ids(&graph, r#"#{kinds: ["vertex"]}"#), vertices);
    assert_eq!(ids(&graph, r#"#{kinds: "edge"}"#), edges);
    assert_eq!(ids(&graph, r#"#{kinds: ["edge", "edge"]}"#), edges);
    assert_eq!(
        ids(&graph, r#"#{kinds: ["edge", "vertex"]}"#),
        ids(&graph, "")
    );
    // `()` stands for a missing key.
    assert_eq!(
        ids(&graph, "#{kinds: (), labels: (), limit: ()}"),
        ids(&graph, "")
    );
}

#[test]
fn the_labels_option_matches_base_or_layer_labels() {
    let graph = edited();
    // `admin` exists only in the layer, `person` in base and layer.
    assert_eq!(
        ids(&graph, r#"#{labels: "admin"}"#),
        r#"[] | [] | ["1"] | false"#
    );
    assert_eq!(
        ids(&graph, r#"#{labels: ["person"]}"#),
        r#"[] | ["6"] | ["1", "2"] | false"#
    );
    assert_eq!(
        ids(&graph, r#"#{labels: ["uses", "created"], kinds: "edge"}"#),
        r#"["e-new"] | ["5"] | [] | false"#
    );
    assert_eq!(ids(&graph, r#"#{labels: []}"#), "[] | [] | [] | false");
}

#[test]
fn the_limit_option_cuts_the_diff_and_says_so() {
    let graph = edited();
    // Filled vertices first (added, removed, modified), then edges.
    assert_eq!(ids(&graph, "#{limit: 2}"), r#"["x1"] | ["6"] | [] | true"#);
    assert_eq!(
        ids(&graph, "#{limit: 7}"),
        r#"["x1", "e-new", "cross"] | ["6", "5"] | ["1", "2"] | true"#
    );
    // Exactly as many entries as there are: nothing cut.
    assert_eq!(
        ids(&graph, "#{limit: 8}"),
        r#"["x1", "e-new", "cross"] | ["6", "5"] | ["1", "2", "0"] | false"#
    );
    assert_eq!(
        ids(&graph, r#"#{limit: 1, kinds: "edge"}"#),
        r#"["e-new"] | [] | [] | true"#
    );
}

#[test]
fn bad_options_fail_with_the_valid_keys_and_values() {
    let graph = edited();
    for (options, actual) in [
        (r#"#{kind: "vertex"}"#, "the key 'kind'"),
        (r#"#{kinds: "node"}"#, "\"node\""),
        (r#"#{kinds: []}"#, "an empty list"),
        (r#"#{kinds: 1}"#, "kinds as a string or a list of strings"),
        (
            r#"#{labels: #{a: 1}}"#,
            "labels as a string or a list of strings",
        ),
        (r#"#{limit: -1}"#, "limit as a positive integer"),
        (r#"#{limit: 1.5}"#, "limit as a positive integer"),
        (r#""vertex""#, "a map #{kinds?, labels?, limit?}"),
    ] {
        let err = run(graph.clone(), &format!("g.layer_diff({options})")).unwrap_err();
        assert!(err.contains("layer_diff"), "{options}: {err}");
        assert!(err.contains(actual), "{options}: {err}");
    }
}

#[test]
fn the_rust_api_answers_the_same_diff() {
    let graph = edited();
    let lock = graph.read();
    let g = GraphTraversalSource::new(&*lock);
    let all = g.layer_diff(&LayerDiffRequest::new()).unwrap();
    assert_eq!(
        (all.added.len(), all.removed.len(), all.modified.len()),
        (3, 2, 3)
    );
    let edges = g
        .layer_diff(
            &LayerDiffRequest::new()
                .kind(graphersal::storage::ElementKind::Edge)
                .limit(1),
        )
        .unwrap();
    assert_eq!(edges.len(), 1);
    assert!(edges.truncated);
    assert_eq!(edges.added[0].id, "e-new");
}

#[test]
fn the_json_form_pins_the_shape() {
    let graph = edited();
    let lock = graph.read();
    let diff = GraphTraversalSource::new(&*lock)
        .layer_diff(&LayerDiffRequest::new())
        .unwrap();
    // Keys in the documented order (the Rhai map sorts them; the value form keeps this order).
    assert_eq!(
        diff.to_json(),
        concat!(
            r#"{"added":["#,
            r#"{"kind":"vertex","id":"x1","labels":["class"],"properties":{"name":"c"}},"#,
            r#"{"kind":"edge","id":"e-new","labels":["uses"],"properties":{},"out":"x1","in":"1"},"#,
            r#"{"kind":"edge","id":"cross","labels":["cross"],"properties":{},"out":"2","in":"3"}],"#,
            r#""removed":["#,
            r#"{"kind":"vertex","id":"6","labels":["person"],"properties":{"name":"peter","age":35}},"#,
            r#"{"kind":"edge","id":"5","labels":["created"],"properties":{"weight":0.2},"out":"6","in":"3"}],"#,
            r#""modified":["#,
            r#"{"kind":"vertex","id":"1","base_labels":["person"],"labels":["person","admin"],"#,
            r#""properties":{"age":{"base":29,"layer":30},"nick":{"layer":"m"}}},"#,
            r#"{"kind":"vertex","id":"2","base_labels":["person"],"labels":["person"],"#,
            r#""properties":{"age":{"base":27}}},"#,
            r#"{"kind":"edge","id":"0","base_labels":["knows"],"labels":["knows"],"out":"1","in":"2","#,
            r#""properties":{"weight":{"base":0.5,"layer":0.9}}}],"#,
            r#""truncated":false}"#
        )
    );
}

#[test]
fn a_diff_inside_one_script_sees_its_earlier_writes() {
    let graph = layered();
    assert_eq!(
        ok(
            graph.clone(),
            r#"g.add_v("n").property(T.id, "n1").iterate();
               g.v("3").property("lang", "rust").iterate();
               let d = g.layer_diff();
               [d.added.map(|e| e.id), d.modified[0].properties.lang]"#
        ),
        r#"[["n1"], #{"base": "java", "layer": "rust"}]"#
    );
}
