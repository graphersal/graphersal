//! Integration tests for `property(T.id, v)` / `property(T.label, v)` folded into the preceding
//! `addV()`/`addE()`, and for `addE()`'s self-loop default.

#[cfg(feature = "script")]
use std::collections::HashMap;
#[cfg(feature = "script")]
use std::sync::Arc;

#[cfg(feature = "script")]
use crate::script_host::{eval_value, eval_value_with_params};
use graphersal::prelude::*;

fn one() -> Option<ElementProperty> {
    Some(ElementProperty::from(1))
}

#[cfg(feature = "script")]
fn empty() -> Arc<Graph> {
    Arc::new(Graph::new(TraversalGraph::new()))
}

#[cfg(feature = "script")]
fn script_int(graph: &Arc<Graph>, script: &str) -> i64 {
    eval_value(graph.clone(), script).unwrap().as_int().unwrap()
}

#[test]
fn id_after_add_v_with_properties_in_between() {
    let mut graph = TraversalGraph::new();
    graph
        .traversal_mut()
        .add_v("person")
        .property("name", "mike")
        .property_id("1")
        .next()
        .unwrap();
    assert_eq!(
        graph
            .traversal()
            .v("1")
            .has("name", "mike")
            .count()
            .next()
            .unwrap(),
        one()
    );
}

#[test]
fn integer_id_becomes_its_decimal_string() {
    let mut graph = TraversalGraph::new();
    graph
        .traversal_mut()
        .add_v(None)
        .property_id(1)
        .next()
        .unwrap();
    assert_eq!(
        graph
            .traversal()
            .v(None)
            .has_id("1")
            .count()
            .next()
            .unwrap(),
        one()
    );
}

#[test]
fn label_after_add_v_replaces_the_label() {
    let mut graph = TraversalGraph::new();
    graph
        .traversal_mut()
        .add_v("x")
        .property_label("person")
        .next()
        .unwrap();
    assert_eq!(
        graph
            .traversal()
            .v(None)
            .has_label("person")
            .count()
            .next()
            .unwrap(),
        one()
    );
    assert_eq!(
        graph
            .traversal()
            .v(None)
            .has_label("x")
            .count()
            .next()
            .unwrap(),
        Some(ElementProperty::from(0))
    );
}

#[test]
fn duplicate_id_fails() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let err = graph
        .traversal_mut()
        .add_v("person")
        .property_id("1")
        .next()
        .expect_err("id 1 exists");
    assert!(
        matches!(
            err.root_cause(),
            TraverserError::Graph(GraphError::VertexIdUniqueConstraint { .. })
        ),
        "{err:?}"
    );
}

#[test]
fn id_after_add_e_with_from_to() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    graph
        .traversal_mut()
        .v("1")
        .add_e("likes")
        .to("2")
        .property("since", 2010)
        .property_id("e1")
        .next()
        .unwrap();
    assert_eq!(graph.traversal().e("e1").count().next().unwrap(), one());
}

#[test]
fn misuse_fails_with_token_property_outside_add() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let err = graph
        .traversal_mut()
        .v("1")
        .property_id(5)
        .next()
        .expect_err("not after addV");
    assert!(
        matches!(
            err.root_cause(),
            TraverserError::TokenPropertyOutsideAdd { .. }
        ),
        "{err:?}"
    );
    assert!(err.root_cause().to_string().contains(
        "property(T.id, ..) can only set the id of an element add_v()/add_e() is creating"
    ));

    // A non-property step in between breaks the fold. (It must pass the new vertex on: the
    // rejected step fails when a traverser reaches it, and a mutating step over an empty stream,
    // such as `add_v("person").out()`, is not run at all.)
    let err = graph
        .traversal_mut()
        .add_v("person")
        .identity()
        .property_id("x")
        .to_list()
        .expect_err("identity() in between");
    assert!(matches!(
        err.root_cause(),
        TraverserError::TokenPropertyOutsideAdd { .. }
    ));

    // The label of an edge comes from addE(label).
    let err = graph
        .traversal_mut()
        .v("1")
        .add_e("likes")
        .to("2")
        .property_label("x")
        .next()
        .expect_err("T.label after addE");
    assert!(
        err.root_cause()
            .to_string()
            .contains("property(T.label, ..)"),
        "{err}"
    );
}

#[test]
fn invalid_values_are_rejected() {
    let mut graph = TraversalGraph::new();
    for bad in [
        ElementProperty::from(1.5),
        ElementProperty::Boolean(true),
        ElementProperty::Null,
    ] {
        let err = graph
            .traversal_mut()
            .add_v(None)
            .property_id(bad.clone())
            .next()
            .expect_err("invalid id");
        assert!(
            err.root_cause().to_string().contains("string or integer"),
            "{err}"
        );
    }
    let err = graph
        .traversal_mut()
        .add_v(None)
        .property_label("")
        .next()
        .expect_err("empty label");
    assert!(
        err.root_cause().to_string().contains("non-empty string"),
        "{err}"
    );
}

#[test]
fn add_e_without_from_and_to_is_a_self_loop() {
    let mut graph = TraversalGraph::new();
    graph
        .traversal_mut()
        .add_v("person")
        .property("name", "marko")
        .add_e("self")
        .next()
        .unwrap();
    assert_eq!(graph.traversal().e(None).count().next().unwrap(), one());
    assert_eq!(
        graph
            .traversal()
            .v(None)
            .out("self")
            .has("name", "marko")
            .count()
            .next()
            .unwrap(),
        one()
    );
}

#[test]
fn add_e_as_a_start_step_still_needs_endpoints() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let err = graph
        .traversal_mut()
        .add_e("x")
        .next()
        .expect_err("no incoming vertex");
    assert!(
        err.root_cause()
            .to_string()
            .contains("add_e() as a start step"),
        "{err}"
    );
}

#[cfg(feature = "script")]
#[test]
fn rhai_token_forms() {
    let graph = empty();
    assert_eq!(
        script_int(
            &graph,
            r#"g.add_v().property(T.label, "person").to_list(); g.v().has_label("person").count().next()"#
        ),
        1
    );
    assert_eq!(
        script_int(
            &graph,
            r#"g.add_v().property(T.id, 7).to_list(); g.v().has_id("7").count().next()"#
        ),
        1
    );
    assert_eq!(
        script_int(
            &graph,
            r#"g.add_v("person").property("name", "mike").property(T.id, "m").to_list(); g.v("m").count().next()"#
        ),
        1
    );
    assert_eq!(
        script_int(
            &graph,
            r#"g.v("m").add_e("self").to_list(); g.v("m").out("self").count().next()"#
        ),
        1
    );
    let err =
        eval_value(graph.clone(), r#"g.V("m").property(T.id, 5).to_list()"#).expect_err("misuse");
    assert!(err.to_string().contains("can only set the id"), "{err}");
    let err = eval_value(graph.clone(), r#"g.V("m").property(T.value, 5)"#);
    assert!(err.is_err());
}

#[cfg(feature = "script")]
#[test]
fn rhai_token_forms_with_parameters() {
    let graph = empty();
    let mut params = HashMap::new();
    params.insert("xx1".to_string(), ElementProperty::from("person"));
    let _ = eval_value_with_params(
        graph.clone(),
        r#"g.addV().property(T.label, xx1).to_list()"#,
        params,
    )
    .unwrap();
    let mut params = HashMap::new();
    params.insert("xx1".to_string(), ElementProperty::from("p1"));
    let _ = eval_value_with_params(
        graph.clone(),
        r#"g.addV().property(T.id, xx1).to_list()"#,
        params,
    )
    .unwrap();
    assert_eq!(
        script_int(&graph, r#"g.v("p1").has_label("person").count().next()"#),
        0
    );
    assert_eq!(
        script_int(&graph, r#"g.v().has_label("person").count().next()"#),
        1
    );
    assert_eq!(script_int(&graph, r#"g.v("p1").count().next()"#), 1);
}

/// The ids of every vertex, sorted.
fn vertex_ids(graph: &TraversalGraph) -> Vec<String> {
    let mut ids: Vec<String> = graph
        .traversal()
        .v(None)
        .id()
        .to_list()
        .unwrap()
        .into_iter()
        .map(|id| id.as_string().unwrap().to_string())
        .collect();
    ids.sort();
    ids
}

#[test]
fn id_traversal_runs_per_traverser_reaching_add_v() {
    let mut graph = TraversalGraph::new();
    graph
        .traversal_mut()
        .inject(vec!["p0", "p1"])
        .as_("m")
        .add_v("person")
        .property("name", "x")
        .property_id_by(__::select("m"))
        .iterate()
        .unwrap();
    assert_eq!(vertex_ids(&graph), ["p0", "p1"]);
    assert_eq!(
        graph
            .traversal()
            .v("p1")
            .has("name", "x")
            .count()
            .next()
            .unwrap(),
        one()
    );
}

#[test]
fn id_traversal_on_add_v_start_step_and_add_v_by() {
    let mut graph = TraversalGraph::new();
    // The start form runs the traversal on a `null` seed.
    graph
        .traversal_mut()
        .add_v("person")
        .property_id_by(__::constant("s1"))
        .iterate()
        .unwrap();
    // `add_v(traversal)` keeps both children: the label and the id.
    graph
        .traversal_mut()
        .inject(vec!["thing"])
        .add_v_by(__::identity())
        .property_id_by(__::constant("t1"))
        .iterate()
        .unwrap();
    assert_eq!(vertex_ids(&graph), ["s1", "t1"]);
    assert_eq!(
        graph
            .traversal()
            .v("t1")
            .has_label("thing")
            .count()
            .next()
            .unwrap(),
        one()
    );
}

#[test]
fn integer_id_from_a_traversal_becomes_its_decimal_string() {
    let mut graph = TraversalGraph::new();
    graph
        .traversal_mut()
        .inject(vec![41_i64, 42])
        .add_v("n")
        .property_id_by(__::identity())
        .iterate()
        .unwrap();
    assert_eq!(vertex_ids(&graph), ["41", "42"]);
}

#[test]
fn id_traversal_after_add_e() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    graph
        .traversal_mut()
        .v(vec!["1", "2"])
        .as_("a")
        .add_e("likes")
        .to("3")
        .property_id_by(__::select("a").id().concat(vec!["-likes"]))
        .iterate()
        .unwrap();
    let mut ids: Vec<String> = graph
        .traversal()
        .e(None)
        .has_label("likes")
        .id()
        .to_list()
        .unwrap()
        .into_iter()
        .map(|id| id.as_string().unwrap().to_string())
        .collect();
    ids.sort();
    assert_eq!(ids, ["1-likes", "2-likes"]);
}

#[test]
fn id_traversal_without_a_usable_result_fails_naming_the_step() {
    let mut graph = TraversalGraph::new();
    let err = graph
        .traversal_mut()
        .inject(vec![1])
        .add_v("n")
        .property_id_by(__::identity().limit(0))
        .iterate()
        .expect_err("no result");
    let message = err.root_cause().to_string();
    assert!(
        message.contains("property(T.id, __.identity().limit(0)) returned no result"),
        "{message}"
    );
    for bad in [
        ElementProperty::from(1.5),
        ElementProperty::Boolean(true),
        ElementProperty::Null,
    ] {
        let err = graph
            .traversal_mut()
            .inject(vec![1])
            .add_v("n")
            .property_id_by(__::constant(bad.clone()))
            .iterate()
            .expect_err("invalid id");
        let message = err.root_cause().to_string();
        assert!(
            message.contains("an id is a string or an integer"),
            "{bad:?}: {message}"
        );
    }
    // Every failure was rolled back.
    assert_eq!(
        graph.traversal().v(None).count().next().unwrap(),
        Some(0.into())
    );
}

#[test]
fn duplicate_id_from_a_traversal_fails() {
    let mut graph = TraversalGraph::new();
    let err = graph
        .traversal_mut()
        .inject(vec!["a", "a"])
        .add_v("n")
        .property_id_by(__::identity())
        .iterate()
        .expect_err("the second vertex reuses id a");
    assert!(
        matches!(
            err.root_cause(),
            TraverserError::Graph(GraphError::VertexIdUniqueConstraint { .. })
        ),
        "{err:?}"
    );
    assert_eq!(
        graph.traversal().v(None).count().next().unwrap(),
        Some(0.into())
    );
}

#[test]
fn id_traversal_is_profiled_and_reads_the_path() {
    let mut graph = TraversalGraph::new();
    let metrics = graph
        .traversal_mut()
        .inject(vec!["p0"])
        .as_("m")
        .add_v("person")
        .property_id_by(__::select("m"))
        .profile()
        .unwrap();
    let plan = metrics.to_string();
    // The id traversal reads the label: the path analysis records it at `as("m")`.
    assert!(plan.contains("as(\"m\") [path: labels(m)]"), "{plan}");
    assert!(
        plan.contains("add_v(\"person\").property(T.id, __.select(\"m\"))"),
        "{plan}"
    );
    assert!(plan.contains("\\> select(\"m\")"), "{plan}");
    assert_eq!(vertex_ids(&graph), ["p0"]);
}

#[test]
fn id_traversal_outside_an_add_step_is_refused() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let err = graph
        .traversal_mut()
        .v("1")
        .property_id_by(__::constant("x"))
        .next()
        .expect_err("not after addV");
    assert!(
        matches!(
            err.root_cause(),
            TraverserError::TokenPropertyOutsideAdd { .. }
        ),
        "{err:?}"
    );
}

#[test]
fn label_traversal_after_add_v_sets_the_label_per_traverser() {
    let mut graph = TraversalGraph::new();
    graph
        .traversal_mut()
        .inject(vec!["a", "b"])
        .as_("m")
        .add_v("x")
        .property("k", 1)
        .property_label_by(__::select("m"))
        .property_id_by(__::select("m"))
        .iterate()
        .unwrap();
    for id in ["a", "b"] {
        assert_eq!(
            graph
                .traversal()
                .v(id)
                .has_label(id)
                .has("k", 1)
                .count()
                .next()
                .unwrap(),
            one()
        );
    }
    assert_eq!(
        graph
            .traversal()
            .v(None)
            .has_label("x")
            .count()
            .next()
            .unwrap(),
        Some(0.into())
    );
    // After add_e() the label comes from add_e(label): refused as with a constant.
    let mut graph = TraversalGraph::tinkerpop_modern();
    let err = graph
        .traversal_mut()
        .v("1")
        .add_e("likes")
        .to("2")
        .property_label_by(__::constant("x"))
        .next()
        .expect_err("T.label after addE");
    assert!(
        matches!(
            err.root_cause(),
            TraverserError::TokenPropertyOutsideAdd { .. }
        ),
        "{err:?}"
    );
}

#[cfg(feature = "script")]
#[test]
fn rhai_token_forms_with_traversals() {
    let graph = empty();
    assert_eq!(
        script_int(
            &graph,
            r#"g.inject(1, 2).add_v("n").property(T.id, __.identity()).iterate(); g.v(["1", "2"]).count().next()"#
        ),
        2
    );
    assert_eq!(
        script_int(
            &graph,
            r#"g.v("1").as("a").add_e("e").to("2").property(T.id, __.select("a").id().concat("-e")).iterate(); g.e("1-e").count().next()"#
        ),
        1
    );
    assert_eq!(
        script_int(
            &graph,
            r#"g.inject("tag").add_v("x").property(T.label, __.identity()).iterate(); g.v().has_label("tag").count().next()"#
        ),
        1
    );
    let err = eval_value(
        graph.clone(),
        r#"g.inject(1).add_v("n").property(T.id, __.constant(1.5)).iterate()"#,
    )
    .expect_err("a float id");
    assert!(
        err.to_string()
            .contains("property(T.id, __.constant(1.5)) returned 1.5 (a float64)"),
        "{err}"
    );
    let err = eval_value(
        graph.clone(),
        r#"g.v("1").add_e("e").to("2").property(T.label, __.constant("x")).iterate()"#,
    )
    .expect_err("T.label after addE");
    assert!(err.to_string().contains("property(T.label, ..)"), "{err}");
    // Nothing was stringified into an id.
    assert_eq!(
        script_int(&graph, r#"g.v("GraphTraversalSource").count().next()"#),
        0
    );
}
