//! Integration tests for `property(Cardinality, key, value)`, `property(map)` and
//! `property(Cardinality, map)`, in the Rust API and in the Rhai DSL.

#[cfg(feature = "script")]
use std::sync::Arc;

#[cfg(feature = "script")]
use crate::script_host::eval_value;
use graphersal::prelude::*;

fn modern() -> TraversalGraph {
    TraversalGraph::tinkerpop_modern()
}

fn count(
    graph: &TraversalGraph,
    key: &str,
    value: impl Into<ElementProperty>,
) -> Option<ElementProperty> {
    graph
        .traversal()
        .v(None)
        .has(key, value)
        .count()
        .next()
        .unwrap()
}

fn object(entries: &[(&str, ElementProperty)]) -> ElementProperty {
    let mut map = PropertyMap::default();
    for (key, value) in entries {
        map.insert(*key, value.clone());
    }
    ElementProperty::Object(Box::new(map))
}

fn root_cause_display(err: &TraverserError) -> String {
    err.root_cause().to_string()
}

/// Runs `script` against a fresh modern graph and returns the value of its last statement.
#[cfg(feature = "script")]
fn script_int(script: &str) -> i64 {
    eval_value(Arc::new(GraphSource::tinkerpop_modern()), script)
        .unwrap()
        .as_int()
        .unwrap()
}

#[cfg(feature = "script")]
fn script_err(script: &str) -> String {
    eval_value(Arc::new(GraphSource::tinkerpop_modern()), script)
        .expect_err("script must fail")
        .to_string()
}

#[test]
fn single_replaces_the_vertex_value() {
    let mut graph = modern();
    graph
        .traversal_mut()
        .v("1")
        .property_with(Cardinality::Single, "name", "stephen")
        .property_with(Cardinality::Single, "name", "stephenm")
        .next()
        .unwrap();
    assert_eq!(
        count(&graph, "name", "stephen"),
        Some(ElementProperty::from(0))
    );
    assert_eq!(
        count(&graph, "name", "stephenm"),
        Some(ElementProperty::from(1))
    );
}

#[test]
fn single_with_a_traversal_value() {
    let mut graph = modern();
    graph
        .traversal_mut()
        .v("1")
        .property_by_with(Cardinality::Single, "age", __::values("age"))
        .next()
        .unwrap();
    assert_eq!(count(&graph, "age", 29), Some(ElementProperty::from(1)));
}

#[test]
fn list_and_set_fail_and_leave_the_graph_unchanged() {
    for cardinality in [Cardinality::List, Cardinality::Set] {
        let mut graph = modern();
        let err = graph
            .traversal_mut()
            .v("1")
            .property_with(cardinality, "age", 1)
            .next()
            .expect_err("list/set must fail");
        assert!(
            matches!(
                err.root_cause(),
                TraverserError::UnsupportedCardinality { .. }
            ),
            "{err:?}"
        );
        assert!(
            root_cause_display(&err)
                .contains(&format!("Cardinality.{cardinality} is not supported: Graphersal stores one value per property key")),
            "{err}"
        );
        assert_eq!(count(&graph, "age", 29), Some(ElementProperty::from(1)));
        assert_eq!(count(&graph, "age", 1), Some(ElementProperty::from(0)));
    }
}

#[test]
fn list_with_a_traversal_value_fails_when_a_value_is_written() {
    let mut graph = modern();
    let err = graph
        .traversal_mut()
        .v("1")
        .property_by_with(Cardinality::List, "age", __::values("age"))
        .next()
        .expect_err("list must fail");
    assert!(matches!(
        err.root_cause(),
        TraverserError::UnsupportedCardinality { .. }
    ));

    // An empty value traversal writes nothing, so nothing fails.
    graph
        .traversal_mut()
        .v("1")
        .property_by_with(Cardinality::List, "age", __::values("missing"))
        .next()
        .unwrap();
}

#[test]
fn zero_traversers_is_a_no_op() {
    let mut graph = modern();
    let result = graph
        .traversal_mut()
        .v("404")
        .property_with(Cardinality::List, "age", 1)
        .to_list()
        .unwrap();
    assert!(result.is_empty());
}

#[test]
fn any_cardinality_on_an_edge_fails() {
    let mut graph = modern();
    let err = graph
        .traversal_mut()
        .e("0")
        .property_with(Cardinality::Single, "weight", 1.0)
        .next()
        .expect_err("edges take no cardinality");
    assert!(
        root_cause_display(&err).contains("Property cardinality can only be set for a Vertex"),
        "{err}"
    );
    let err = graph
        .traversal_mut()
        .e("0")
        .property_map_with(Cardinality::Single, object(&[("weight", 1.0.into())]))
        .next()
        .expect_err("edges take no cardinality");
    assert!(root_cause_display(&err).contains("Property cardinality can only be set for a Vertex"));
}

#[test]
fn map_writes_every_entry() {
    let mut graph = TraversalGraph::new();
    graph
        .traversal_mut()
        .add_v(None)
        .property_map(object(&[("name", "foo".into()), ("age", 42.into())]))
        .next()
        .unwrap();
    assert_eq!(count(&graph, "name", "foo"), Some(ElementProperty::from(1)));
    assert_eq!(count(&graph, "age", 42), Some(ElementProperty::from(1)));

    graph
        .traversal_mut()
        .v(None)
        .property_map_with(Cardinality::Single, object(&[("age", 43.into())]))
        .next()
        .unwrap();
    assert_eq!(count(&graph, "age", 43), Some(ElementProperty::from(1)));
}

#[test]
fn map_on_an_edge_without_cardinality_is_allowed() {
    let mut graph = modern();
    graph
        .traversal_mut()
        .e("0")
        .property_map(object(&[("since", 2010.into())]))
        .next()
        .unwrap();
    let since = graph.traversal().e("0").values("since").next().unwrap();
    assert_eq!(since, Some(ElementProperty::from(2010)));
}

#[test]
fn null_and_empty_maps_are_no_ops() {
    let mut graph = TraversalGraph::new();
    for map in [ElementProperty::Null, object(&[])] {
        graph
            .traversal_mut()
            .add_v("person")
            .property_map(map.clone())
            .next()
            .unwrap();
        graph
            .traversal_mut()
            .add_v("person")
            .property_map_with(Cardinality::Set, map)
            .next()
            .unwrap();
    }
    let values = graph
        .traversal()
        .v(None)
        .values(None)
        .count()
        .next()
        .unwrap();
    assert_eq!(values, Some(ElementProperty::from(0)));
}

#[test]
fn reserved_and_hidden_keys_are_rejected() {
    for key in ["T.id", "T.label", "T.key", "Direction.OUT", "~id"] {
        let mut graph = modern();
        let err = graph
            .traversal_mut()
            .v("1")
            .property_map(object(&[(key, 1.into())]))
            .next()
            .expect_err("reserved key");
        assert!(root_cause_display(&err).contains(key), "{key}: {err}");
    }
}

#[test]
fn a_non_map_argument_is_rejected() {
    let mut graph = modern();
    let err = graph
        .traversal_mut()
        .v("1")
        .property_map(ElementProperty::from(5))
        .next()
        .expect_err("not a map");
    assert!(
        root_cause_display(&err).contains("property(map) takes a map or null"),
        "{err}"
    );
}

#[test]
fn a_rejected_entry_leaves_the_whole_map_unwritten() {
    let mut graph = modern();
    graph
        .traversal_mut()
        .v("1")
        .property_map_with(Cardinality::List, object(&[("color", "red".into())]))
        .next()
        .expect_err("list");
    assert_eq!(
        count(&graph, "color", "red"),
        Some(ElementProperty::from(0))
    );
}

#[test]
fn profile_names_the_cardinality() {
    let mut graph = modern();
    let plan = graph
        .traversal_mut()
        .v("1")
        .property_with(Cardinality::Single, "name", "x")
        .profile()
        .unwrap()
        .to_string();
    assert!(
        plan.contains(r#"property(Cardinality.single, "name", "x")"#),
        "{plan}"
    );
}

#[cfg(feature = "script")]
#[test]
fn rhai_cardinality_forms() {
    assert_eq!(
        script_int(
            r#"g.V("1").property(Cardinality.single, "name", "a").property(Cardinality::single, "name", "b").to_list();
               g.V().has("name", "b").count().next()"#
        ),
        1
    );
    assert_eq!(
        script_int(
            r#"g.V("1").property(Cardinality.single, "age", __.values("age").as_number(GType::LONG)).to_list();
               g.V().has("age", 29).count().next()"#
        ),
        1
    );
    let err = script_err(r#"g.V("1").property(Cardinality.list, "age", 1).to_list()"#);
    assert!(err.contains("Cardinality.list is not supported"), "{err}");
}

#[cfg(feature = "script")]
#[test]
fn rhai_map_forms() {
    assert_eq!(
        script_int(
            r#"g.addV().property(#{"name": "foo", "age": 42}).to_list(); g.V().has("name", "foo").count().next()"#
        ),
        1
    );
    assert_eq!(
        script_int(
            r#"g.addV().property(Cardinality.single, #{"name": "foo", "age": 42}).to_list();
               g.V().has("age", 42).count().next()"#
        ),
        1
    );
    assert_eq!(
        script_int(
            r#"g.addV("x").property(()).to_list(); g.addV("x").property(#{}).to_list();
               g.addV("x").property(Cardinality.set, ()).to_list(); g.addV("x").property(Cardinality.set, #{}).to_list();
               g.V().hasLabel("x").values().count().next()"#
        ),
        0
    );
    // A per-entry wrapper: single is written, set fails for the whole map.
    assert_eq!(
        script_int(
            r#"g.V("1").property(Cardinality.set, #{"age": Cardinality.single(43)}).to_list();
               g.V().has("age", 43).count().next()"#
        ),
        1
    );
    let err = script_err(
        r#"g.V().has("name", "marko").property(#{"name": Cardinality.set("bar")}).to_list()"#,
    );
    assert!(err.contains("Cardinality.set is not supported"), "{err}");
    let err = script_err(r#"g.V("1").property(#{"T.label": "x"}).to_list()"#);
    assert!(err.contains("\"T.label\" is reserved"), "{err}");
}
