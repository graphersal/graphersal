//! `remove_property(keys...)`: explicit property removal that keeps
//! the element in the stream (a Graphersal extension; `property(k, null)` still stores a null).
#![cfg(feature = "script")]

use std::sync::Arc;

use crate::script_host::eval_value;
use graphersal::prelude::*;
use graphersal::schema::PropertySchema;

fn modern() -> Arc<Graph> {
    Arc::new(GraphSource::tinkerpop_modern())
}

fn int(graph: &Arc<Graph>, script: &str) -> i64 {
    eval_value(graph.clone(), script)
        .unwrap_or_else(|e| panic!("{script}: {e}"))
        .as_int()
        .unwrap_or_else(|_| panic!("{script}: not an int"))
}

fn text(graph: &Arc<Graph>, script: &str) -> String {
    format!(
        "{:?}",
        eval_value(graph.clone(), script).unwrap_or_else(|e| panic!("{script}: {e}"))
    )
}

fn fail(graph: &Arc<Graph>, script: &str) -> String {
    eval_value(graph.clone(), script)
        .map(|_| ())
        .expect_err(script)
        .to_string()
}

const SPELLINGS: [&str; 1] = ["remove_property"];

#[test]
fn one_key_both_spellings() {
    for sp in SPELLINGS {
        let g = modern();
        assert_eq!(
            int(
                &g,
                &format!(r#"g.V().hasLabel("person").{sp}("age").count().next()"#)
            ),
            4
        );
        assert_eq!(int(&g, r#"g.V().has("age").count().next()"#), 0);
        assert_eq!(int(&g, r#"g.V().has("name").count().next()"#), 6);
        assert_eq!(int(&g, r#"g.V().hasNot("age").count().next()"#), 6);
    }
}

#[test]
fn several_keys_as_an_array_and_all_keys() {
    for sp in SPELLINGS {
        let g = modern();
        run_ok(&g, &format!(r#"g.V("1").{sp}(["age", "name"]).toList()"#));
        assert_eq!(int(&g, r#"g.V("1").properties().count().next()"#), 0);
        run_ok(&g, &format!(r#"g.V("2").{sp}().toList()"#));
        assert_eq!(int(&g, r#"g.V("2").properties().count().next()"#), 0);
        assert_eq!(int(&g, r#"g.V("3").properties().count().next()"#), 2);
        // The vertex itself survives.
        assert_eq!(int(&g, "g.V().count().next()"), 6);
    }
}

fn run_ok(g: &Arc<Graph>, script: &str) {
    let _ = eval_value(g.clone(), script).unwrap_or_else(|e| panic!("{script}: {e}"));
}

#[test]
fn edges_lose_properties_too() {
    for sp in SPELLINGS {
        let g = modern();
        run_ok(&g, &format!(r#"g.E().{sp}("weight").toList()"#));
        assert_eq!(int(&g, r#"g.E().has("weight").count().next()"#), 0);
        assert_eq!(int(&g, "g.E().count().next()"), 6);
        let g = modern();
        run_ok(&g, &format!(r#"g.E().{sp}().toList()"#));
        assert_eq!(int(&g, "g.E().properties().count().next()"), 0);
    }
}

#[test]
fn a_missing_key_is_a_no_op_and_the_element_flows() {
    let g = modern();
    assert_eq!(
        int(&g, r#"g.V().remove_property("nope").count().next()"#),
        6
    );
    assert_eq!(
        int(
            &g,
            r#"g.V().remove_property("nope").values("name").count().next()"#
        ),
        6
    );
    assert_eq!(
        int(
            &g,
            r#"g.V().remove_property("age").values("name").count().next()"#
        ),
        6
    );
    assert_eq!(
        int(
            &g,
            r#"g.V().remove_property(["zz", "yy"]).properties().count().next()"#
        ),
        8
    );
}

#[test]
fn null_is_stored_and_removed_explicitly() {
    let g = modern();
    run_ok(&g, r#"g.V("1").property("k", ()).toList()"#);
    assert_eq!(int(&g, r#"g.V().has("k", ()).count().next()"#), 1);
    run_ok(&g, r#"g.V("1").remove_property("k").toList()"#);
    assert_eq!(int(&g, r#"g.V().has("k").count().next()"#), 0);
    assert_eq!(int(&g, r#"g.V().has("k", ()).count().next()"#), 0);
}

#[test]
fn empty_stream_emits_nothing() {
    let g = modern();
    assert_eq!(
        int(
            &g,
            r#"g.V().has("name", "nobody").remove_property("age").count().next()"#
        ),
        0
    );
}

#[test]
fn non_elements_are_cast_errors() {
    let g = modern();
    for script in [
        r#"g.V().values("age").remove_property("a").toList()"#,
        r#"g.V().properties().remove_property().toList()"#,
        r#"g.V().path().remove_property("a").toList()"#,
    ] {
        let msg = fail(&g, script);
        assert!(
            msg.contains("vertex") && msg.contains("edge"),
            "{script}: {msg}"
        );
    }
}

#[test]
fn bad_arguments_are_mismatches_with_help() {
    let g = modern();
    for script in [
        "g.V().remove_property(1).toList()",
        r#"g.V().remove_property(["a", 2]).toList()"#,
        "g.V().remove_property([]).toList()",
    ] {
        let msg = fail(&g, script);
        assert!(msg.contains("property keys as strings"), "{script}: {msg}");
        assert!(
            msg.contains("remove_property() takes property keys as strings"),
            "{script}: {msg}"
        );
    }
}

#[test]
fn bulk_merge_on_and_off() {
    for merge in ["true", "false"] {
        let g = modern();
        // `lop` is reached twice by out().out().
        run_ok(
            &g,
            &format!(
                r#"g.with("bulk.merge", {merge}).V("1").out().out().remove_property("lang").toList()"#
            ),
        );
        assert_eq!(
            int(&g, r#"g.V().has("lang").count().next()"#),
            0,
            "merge={merge}"
        );
        assert_eq!(int(&g, r#"g.V().has("name").count().next()"#), 6);
    }
}

#[test]
fn inside_local_union_and_repeat() {
    let g = modern();
    run_ok(&g, r#"g.V("1").local(__.remove_property("age")).toList()"#);
    assert_eq!(int(&g, r#"g.V("1").has("age").count().next()"#), 0);
    run_ok(
        &g,
        r#"g.V("2").union(__.remove_property("age"), __.identity()).toList()"#,
    );
    assert_eq!(int(&g, r#"g.V("2").has("age").count().next()"#), 0);
    run_ok(
        &g,
        r#"g.V("1").repeat(__.out()).times(1).remove_property("name").toList()"#,
    );
    assert_eq!(int(&g, r#"g.V().has("name").count().next()"#), 3);
    run_ok(
        &g,
        r#"g.V("4").repeat(__.remove_property("age")).times(2).toList()"#,
    );
    assert_eq!(int(&g, r#"g.V("4").has("age").count().next()"#), 0);
}

#[test]
fn the_plan_is_mutating_and_persists() {
    let g = modern();
    run_ok(
        &g,
        r#"g.V("1").local(__.sideEffect(__.remove_property("age"))).toList()"#,
    );
    assert_eq!(int(&g, r#"g.V("1").has("age").count().next()"#), 0);
}

#[test]
fn count_pushdown_does_not_skip_the_removal() {
    let g = modern();
    assert_eq!(int(&g, r#"g.V().remove_property("age").count().next()"#), 6);
    assert_eq!(int(&g, r#"g.V().has("age").count().next()"#), 0);
}

#[test]
fn profile_shows_the_step() {
    let g = modern();
    let out = text(
        &g,
        r#"g.V().remove_property(["age", "name"]).remove_property().profile().to_string()"#,
    );
    assert!(
        out.contains(r#"remove_property([\"age\", \"name\"])"#),
        "{out}"
    );
    assert!(out.contains("remove_property()"), "{out}");
}

#[test]
fn relation_to_properties_drop() {
    let a = modern();
    let b = modern();
    run_ok(&a, r#"g.V().properties("age").drop().toList()"#);
    run_ok(&b, r#"g.V().remove_property("age").toList()"#);
    for script in [r#"g.V().valueMap().toList()"#, r#"g.V().count().next()"#] {
        assert_eq!(text(&a, script), text(&b, script));
    }
    assert_eq!(
        int(&a, r#"g.V().properties("age").drop().count().next()"#),
        0
    );
    assert_eq!(int(&b, r#"g.V().remove_property("age").count().next()"#), 6);
}

#[test]
fn merge_e_on_match_option_child() {
    let g = Arc::new(Graph::new(TraversalGraph::new()));
    run_ok(
        &g,
        r#"g.addV("p").property(T.id,"a").addV("p").property(T.id,"b").toList()"#,
    );
    run_ok(
        &g,
        r#"g.mergeE(#{"T.label": "knows", "Direction.OUT": "a", "Direction.IN": "b", "weight": 1}).toList()"#,
    );
    run_ok(
        &g,
        r#"g.mergeE(#{"T.label": "knows", "Direction.OUT": "a", "Direction.IN": "b"}).option(Merge.onMatch, __.sideEffect(__.remove_property("weight")).constant(#{})).toList()"#,
    );
    assert_eq!(int(&g, r#"g.E().has("weight").count().next()"#), 0);
    assert_eq!(int(&g, "g.E().count().next()"), 1);
}

#[test]
fn rust_api_and_anonymous_forms() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    graph
        .traversal_mut()
        .v("1")
        .remove_property("age")
        .next()
        .unwrap();
    assert_eq!(graph.traversal().v("1").values("age").next().unwrap(), None);
    assert!(
        graph
            .traversal()
            .v("1")
            .values("name")
            .next()
            .unwrap()
            .is_some()
    );
    graph
        .traversal_mut()
        .v("2")
        .remove_property(vec!["age", "name"])
        .next()
        .unwrap();
    assert_eq!(
        graph
            .traversal()
            .v("2")
            .properties(None::<()>)
            .count()
            .next()
            .unwrap(),
        Some(ElementProperty::from(0))
    );
    graph
        .traversal_mut()
        .v("4")
        .local(__::remove_property(None::<()>))
        .next()
        .unwrap();
    assert_eq!(
        graph
            .traversal()
            .v("4")
            .properties(None::<()>)
            .count()
            .next()
            .unwrap(),
        Some(ElementProperty::from(0))
    );
}

#[test]
fn a_required_schema_field_cannot_be_removed() {
    // Removing a required property is rejected (it used to succeed and leave an invalid vertex).
    let schema = GraphSchema::new()
        .with_mode(SchemaMode::Closed)
        .with_vertex(
            "Person",
            PropertySchema::object().with_required_property("age", PropertySchema::integer()),
        );
    let mut graph = TraversalGraph::new();
    graph.set_schema(schema).unwrap();
    let mut p = PropertyMap::default();
    p.insert("age", ElementProperty::from(3));
    graph
        .add_vertex(Some("1".into()), vec!["Person".into()], p)
        .unwrap();
    let err = graph
        .traversal_mut()
        .v("1")
        .remove_property("age")
        .next()
        .expect_err("required property removal must fail");
    assert!(
        err.root_cause()
            .to_string()
            .contains("Cannot remove required"),
        "{err}"
    );
    assert_eq!(
        graph.traversal().v("1").values("age").next().unwrap(),
        Some(ElementProperty::from(3))
    );
}
