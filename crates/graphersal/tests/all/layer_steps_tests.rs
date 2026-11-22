//! The `layer()` and `has_layer(..)` steps (layered graph S2.2): the origin of each vertex or
//! edge as `"base"` / `"added"` / `"modified"`, and the filter by it. A plain graph answers
//! `"base"` for everything; a `Graph<LayeredGraph>` answers from its layer: added and modified
//! vertices and edges (written by Rhai scripts), a cross edge between base vertices (its
//! endpoints stay base), an element changed back (base again), a removed element (never
//! traversed; one removed during the traversal behaves like `label()` / `has_label()`), the
//! Rust API, child traversals and the profile. The storage-error case lives in
//! `graphersal-storage-tests/tests/failing_storage.rs` (`Failing` with `Faults::layer`).
#![cfg(feature = "script")]

use std::collections::HashMap;
use std::sync::Arc;

use graphersal::prelude::*;
use graphersal::storage::{Graph, IntoAnyGraph, LayeredGraph};

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

fn plain() -> Arc<Graph<TraversalGraph>> {
    Arc::new(Graph::new(TraversalGraph::tinkerpop_modern()))
}

fn layered() -> Arc<Graph<LayeredGraph>> {
    let base = Arc::new(TraversalGraph::tinkerpop_modern());
    Arc::new(Graph::new(LayeredGraph::over(base).unwrap()))
}

/// The modern graph's layer after the edits every layered test starts from:
/// - vertex `x1` (label `class`) added, edge `e-new` from `x1` to vertex 1 added;
/// - vertex 1 modified (property), edge `0` (1 knows 2) modified (property);
/// - cross edge `cross` between the base vertices 2 and 3 added;
/// - vertex 4 changed and changed back (base again);
/// - vertex 6 (with its edge `5`) removed.
fn edited() -> Arc<Graph<LayeredGraph>> {
    let graph = layered();
    for script in [
        r#"g.add_v("class").property(T.id, "x1").property("name", "c").iterate()"#,
        r#"g.add_e("uses").from(__.v("x1")).to(__.v("1")).property(T.id, "e-new").iterate()"#,
        r#"g.v("1").property("age", 30).iterate()"#,
        r#"g.e("0").property("weight", 0.9).iterate()"#,
        r#"g.add_e("cross").from(__.v("2")).to(__.v("3")).property(T.id, "cross").iterate()"#,
        r#"g.v("4").property("age", 99).iterate()"#,
        r#"g.v("4").property("age", 32).iterate()"#,
        r#"g.v("6").drop().iterate()"#,
    ] {
        ok(graph.clone(), script);
    }
    graph
}

#[test]
fn a_plain_graph_is_all_base() {
    let graph = plain();
    assert_eq!(
        ok(graph.clone(), r#"g.v().layer().to_list()"#),
        r#"["base", "base", "base", "base", "base", "base"]"#
    );
    assert_eq!(
        ok(graph.clone(), r#"g.e().layer().dedup().to_list()"#),
        r#"["base"]"#
    );
    assert_eq!(
        ok(graph.clone(), r#"g.v().has_layer("base").count().next()"#),
        "6"
    );
    assert_eq!(
        ok(graph.clone(), r#"g.e().has_layer("base").count().next()"#),
        "6"
    );
    assert_eq!(
        ok(graph.clone(), r#"g.v().has_layer("added").count().next()"#),
        "0"
    );
    assert_eq!(
        ok(
            graph.clone(),
            r#"g.e().has_layer("added", "modified").count().next()"#
        ),
        "0"
    );
    // Without arguments every element passes; an empty list keeps nothing (like has_label).
    assert_eq!(
        ok(graph.clone(), r#"g.v().has_layer().count().next()"#),
        "6"
    );
    assert_eq!(
        ok(graph.clone(), r#"g.v().has_layer([]).count().next()"#),
        "0"
    );
    // The array form equals the positional one.
    assert_eq!(
        ok(
            graph,
            r#"g.v().has_layer(["base", "added"]).count().next()"#
        ),
        "6"
    );
}

#[test]
fn an_untouched_layer_is_all_base() {
    let graph = layered();
    assert_eq!(
        ok(graph.clone(), r#"g.v().layer().dedup().to_list()"#),
        r#"["base"]"#
    );
    assert_eq!(
        ok(
            graph,
            r#"g.e().has_layer("added", "modified").count().next()"#
        ),
        "0"
    );
}

#[test]
fn vertex_origins_follow_the_layer() {
    let graph = edited();
    assert_eq!(
        ok(
            graph.clone(),
            r#"g.v().order().by(T.id).project("id", "o").by(T.id).by(__.layer()).to_list()"#
        ),
        r##"[#{"id": "1", "o": "modified"}, #{"id": "2", "o": "base"}, #{"id": "3", "o": "base"}, #{"id": "4", "o": "base"}, #{"id": "5", "o": "base"}, #{"id": "x1", "o": "added"}]"##
    );
    assert_eq!(
        ok(
            graph.clone(),
            r#"g.v().has_label("class").layer().to_list()"#
        ),
        r#"["added"]"#
    );
    assert_eq!(
        ok(
            graph.clone(),
            r#"g.v().has_layer("added", "modified").id().order().to_list()"#
        ),
        r#"["1", "x1"]"#
    );
    assert_eq!(
        ok(
            graph.clone(),
            r#"g.v().has_layer("base").id().order().to_list()"#
        ),
        r#"["2", "3", "4", "5"]"#
    );
    // The removed vertex 6 is never traversed, under any origin.
    assert_eq!(ok(graph, r#"g.v().has_layer().count().next()"#), "6");
}

#[test]
fn edge_origins_follow_the_layer() {
    let graph = edited();
    assert_eq!(
        ok(
            graph.clone(),
            r#"g.e().order().by(T.id).project("id", "o").by(T.id).by(__.layer()).to_list()"#
        ),
        r##"[#{"id": "0", "o": "modified"}, #{"id": "1", "o": "base"}, #{"id": "2", "o": "base"}, #{"id": "3", "o": "base"}, #{"id": "4", "o": "base"}, #{"id": "cross", "o": "added"}, #{"id": "e-new", "o": "added"}]"##
    );
    assert_eq!(
        ok(
            graph.clone(),
            r#"g.e().has_layer("added").id().order().to_list()"#
        ),
        r#"["cross", "e-new"]"#
    );
    assert_eq!(
        ok(
            graph.clone(),
            r#"g.e().has_layer("modified").id().to_list()"#
        ),
        r#"["0"]"#
    );
    // The cross edge is added, its base endpoints stay base.
    assert_eq!(
        ok(graph.clone(), r#"g.e("cross").both_v().layer().to_list()"#),
        r#"["base", "base"]"#
    );
    // Adjacency through the layer, filtered by origin.
    assert_eq!(
        ok(
            graph,
            r#"g.v("1").both_e().has_layer("added", "modified").id().order().to_list()"#
        ),
        r#"["0", "e-new"]"#
    );
}

#[test]
fn an_element_changed_back_is_base_again() {
    let graph = edited();
    assert_eq!(ok(graph.clone(), r#"g.v("4").layer().next()"#), "base");
    ok(graph.clone(), r#"g.v("1").property("age", 29).iterate()"#);
    ok(
        graph.clone(),
        r#"g.e("0").property("weight", 0.5).iterate()"#,
    );
    assert_eq!(ok(graph.clone(), r#"g.v("1").layer().next()"#), "base");
    assert_eq!(ok(graph.clone(), r#"g.e("0").layer().next()"#), "base");
    assert_eq!(
        ok(graph, r#"g.v().has_layer("modified").count().next()"#),
        "0"
    );
}

/// Inside child traversals (`where`, `not`, `local`) and with the Rust API.
#[test]
fn children_and_the_rust_api() {
    let graph = edited();
    assert_eq!(
        ok(
            graph.clone(),
            r#"g.v().where(__.out_e().has_layer("added")).id().order().to_list()"#
        ),
        r#"["2", "x1"]"#
    );
    assert_eq!(
        ok(
            graph.clone(),
            r#"g.v().not(__.has_layer("base")).count().next()"#
        ),
        "2"
    );
    let lock = graph.read();
    let g = GraphTraversalSource::new(&*lock);
    let origins = g
        .v(None)
        .has_layer(vec!["added", "modified"])
        .layer()
        .to_list()
        .unwrap();
    let mut origins: Vec<&str> = origins.iter().filter_map(|o| o.as_str()).collect();
    origins.sort_unstable();
    assert_eq!(origins, ["added", "modified"]);
    let g = GraphTraversalSource::new(&*lock);
    let added = g
        .e(None)
        .where_t(__::has_layer("added"))
        .count()
        .next()
        .unwrap();
    assert_eq!(added.and_then(|c| c.as_i64()), Some(2));
}

/// Element removed during the traversal: `layer()` fails like `label()` (the element was
/// removed), `has_layer()` drops it like `has_label()`; the same on a plain and a layered graph.
#[test]
fn an_element_removed_during_the_traversal() {
    fn check(graph: impl IntoAnyGraph + Clone) {
        let label = run(
            graph.clone(),
            r#"g.v("2").side_effect(__.drop()).label().to_list()"#,
        )
        .unwrap_err();
        let layer = run(
            graph.clone(),
            r#"g.v("2").side_effect(__.drop()).layer().to_list()"#,
        )
        .unwrap_err();
        assert!(layer.contains("removed"), "{layer}");
        assert_eq!(
            layer.lines().find(|l| l.starts_with("Caused by")),
            label.lines().find(|l| l.starts_with("Caused by"))
        );
        assert_eq!(
            ok(
                graph.clone(),
                r#"g.v("2").side_effect(__.drop()).has_label("person").count().next()"#
            ),
            "0"
        );
        assert_eq!(
            ok(
                graph,
                r#"g.v("2").side_effect(__.drop()).has_layer().count().next()"#
            ),
            "0"
        );
    }
    check(plain());
    check(edited());
}

/// A value is not an element: both steps fail with a cast error, like `label()` /
/// `has_label()`.
#[test]
fn a_value_is_a_cast_error() {
    for script in [
        r#"g.v("1").values("name").layer().to_list()"#,
        r#"g.inject(1).has_layer("base").to_list()"#,
        r#"g.v("1").properties("name").layer().to_list()"#,
    ] {
        let err = run(plain(), script).unwrap_err();
        assert!(
            err.contains("vertex") && err.contains("edge"),
            "`{script}`: {err}"
        );
    }
}

/// Both steps render in the profile and the camel spelling keeps their snake names; no rule
/// folds `has_layer` into the source.
#[test]
fn profile_and_spelling() {
    let graph = edited();
    let profile = ok(
        graph.clone(),
        r#"g.v().has_label("person").has_layer("added", "modified").layer().profile()"#,
    );
    assert!(
        profile.contains(r#"has_layer("added", "modified")"#),
        "{profile}"
    );
    assert!(profile.contains("layer()"), "{profile}");
    assert!(profile.contains(r#"v(labels: ["person"])"#), "{profile}");
    let camel = ok(
        graph,
        r#"g.with("render.spelling", "camel").v().has_layer("added").layer().profile()"#,
    );
    assert!(camel.contains(r#"has_layer("added")"#), "{camel}");
}
