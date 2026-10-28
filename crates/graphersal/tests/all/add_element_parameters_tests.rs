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
