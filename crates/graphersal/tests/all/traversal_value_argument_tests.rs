//! A traversal given where a step (or a token, a map lookup, a saved query or a job) takes a
//! value is refused at the call with an argument error naming the step; it never becomes the
//! string "GraphTraversalSource" (or any other text) stored, injected or compared as data. The
//! forms that run a traversal argument (`property(key, traversal)`, `property(T.id, traversal)`,
//! `merge_v(traversal)`, `combine(traversal)`, ...) keep running it.
#![cfg(feature = "script")]

use std::sync::Arc;

use crate::script_host::{eval, eval_value};
use graphersal::prelude::*;

fn modern() -> Arc<Graph> {
    Arc::new(GraphSource::tinkerpop_modern())
}

/// `query` fails with an argument error containing `error`, and the graph keeps its 6 vertices
/// and 6 edges (nothing written before the refusal).
fn refused(graph: &Arc<Graph>, query: &str, error: &str) {
    let report = eval(graph.clone(), query)
        .expect_err(&format!("{query} must fail"))
        .to_string();
    assert!(report.contains(error), "{query}: {report}");
    assert!(
        !report.contains("\"GraphTraversalSource\""),
        "{query}: the traversal must not appear as a string value: {report}"
    );
    assert_eq!(
        eval_value(
            graph.clone(),
            "[g.v().count().next(), g.e().count().next()]"
        )
        .unwrap()
        .into_typed_array::<i64>()
        .unwrap(),
        vec![6, 6],
        "{query} must write nothing"
    );
}

/// Every binding that converts a script value: a traversal argument (or a traversal inside a
/// list or map argument) is an argument error naming the step.
#[test]
fn a_traversal_where_a_value_is_expected_is_refused() {
    let graph = modern();
    let traversal = "got a traversal (__.out())";
    let cases: &[(&str, &str)] = &[
        // Source steps.
        (
            r#"g.with_sack(__.out()).v().sack().to_list()"#,
            "'with_sack'",
        ),
        (
            r#"g.withSack(__.out(), Operator.sum).v().sack().to_list()"#,
            "'with_sack'",
        ),
        (
            r#"g.with_side_effect("a", __.out()).v().cap("a").to_list()"#,
            "'with_side_effect'",
        ),
        (
            r#"g.withSideEffect("a", __.out(), Operator.sum).v().to_list()"#,
            "'with_side_effect'",
        ),
        (
            r#"g.with("evaluationTimeout", __.out()).v().count().next()"#,
            "'with'",
        ),
        // Steps.
        (r#"g.v().constant(__.out()).to_list()"#, "'constant'"),
        (r#"g.inject(__.out()).to_list()"#, "'inject'"),
        (r#"g.inject(1, __.out()).to_list()"#, "'inject'"),
        (r#"g.v().inject(__.out()).to_list()"#, "'inject'"),
        (
            r#"g.v().values("age").fold(__.out(), Operator.sum).to_list()"#,
            "'fold'",
        ),
        (r#"g.v(__.out()).to_list()"#, "'V'"),
        (r#"g.e([__.out()]).to_list()"#, "'E'"),
        (r#"g.v(#{id: __.out()}).to_list()"#, "'V'"),
        (
            r#"g.inject("x").as_number(GType.LONG, CastPolicy.Default(__.out())).to_list()"#,
            "'CastPolicy.Default'",
        ),
        (
            r#"g.v("1").property_json(__.out()).to_list()"#,
            "'property_json'",
        ),
        (
            r#"let m = g.v().group_count().by("age").next(); m.contains(__.out())"#,
            "'contains'",
        ),
        (
            r#"let m = g.v().group_count().by("age").next(); m[__.out()]"#,
            "'map[key]'",
        ),
    ];
    for (query, step) in cases {
        refused(&graph, query, &format!("{step}: expected"));
        refused(&graph, query, traversal);
    }
}

/// A traversal nested in a list or map argument is refused like the argument itself.
#[test]
fn a_traversal_inside_a_list_or_map_value_is_refused() {
    let graph = modern();
    let inside = "got a traversal (__.out()) inside a list or map";
    for (query, step) in [
        (r#"g.inject([__.out()]).to_list()"#, "'inject'"),
        (r#"g.inject(#{a: [1, __.out()]}).to_list()"#, "'inject'"),
        (r#"g.v().constant([__.out()]).to_list()"#, "'constant'"),
        (
            r#"g.with_side_effect("a", [__.out()]).v().cap("a").to_list()"#,
            "'with_side_effect'",
        ),
        (
            r#"g.v("1").property_json(#{a: __.out()}).to_list()"#,
            "'property_json'",
        ),
        (
            r#"g.v("1").property_json(#{a: #{b: [__.out()]}}).to_list()"#,
            "'property_json'",
        ),
        (
            r#"g.inject([1]).combine([__.out()]).to_list()"#,
            "'combine'",
        ),
        (
            r#"g.inject(1).option([__.out()], __.identity()).to_list()"#,
            "'option'",
        ),
    ] {
        refused(&graph, query, step);
        refused(&graph, query, inside);
    }
}

/// The values of `property(map)`, `merge_v(map)`, `merge_e(map)` and `option(Merge.x, map)`
/// are values: a traversal there is refused naming its key, and nothing is created.
#[test]
fn a_traversal_as_a_map_value_of_a_writing_step_is_refused() {
    let graph = modern();
    for (query, error) in [
        (
            r#"g.v("1").property(#{nick: __.constant("m")}).to_list()"#,
            r#"'property': expected a map of values, got a traversal (__.constant("m")) as the value of "nick""#,
        ),
        (
            r#"g.v("1").property(Cardinality.single, #{nick: __.out()}).to_list()"#,
            r#"'property': expected a map of values, got a traversal (__.out()) as the value of "nick""#,
        ),
        (
            r#"g.add_v("person").property(#{nick: Cardinality.single(__.out())}).to_list()"#,
            r#"'property': expected a map of values, got a traversal (__.out()) as the value of "nick""#,
        ),
        (
            r#"g.merge_v(#{name: __.constant("x")}).to_list()"#,
            r#"'merge_v': expected a map of values, got a traversal (__.constant("x")) as the value of "name""#,
        ),
        (
            r#"g.merge_e(#{weight: __.out()}).to_list()"#,
            r#"'merge_e': expected a map of values, got a traversal (__.out()) as the value of "weight""#,
        ),
        (
            r#"g.merge_v(#{name: "nobody"}).option(Merge.onCreate, #{age: __.out()}).to_list()"#,
            r#"'option': expected a map of values, got a traversal (__.out()) as the value of "age""#,
        ),
    ] {
        refused(&graph, query, error);
    }
}

/// Saved-query parameter defaults and arguments, and job arguments, are data.
#[test]
fn a_traversal_as_a_saved_query_or_job_value_is_refused() {
    let graph = modern();
    refused(
        &graph,
        r#"g.define_query(#{name: "top", params: #{limit: #{schema: #{type: "integer"}, default: __.out()}}, body: "fn top(limit) { g.v().limit(limit) }"})"#,
        "'define_query': expected a value",
    );
    let _defined = eval_value(
        graph.clone(),
        r#"g.define_query(#{name: "top", params: #{limit: "integer"}, body: "fn top(limit) { g.v().limit(limit) }"})"#,
    )
    .unwrap();
    refused(
        &graph,
        r#"g.query("top", #{limit: __.out()}).to_list()"#,
        "not a data value",
    );
    refused(
        &graph,
        r#"g.define_job(#{name: "j", schedule: "0 * * * *", action: "query", query: "top", args: #{limit: __.out()}})"#,
        "'define_job': expected a value",
    );
}

/// The forms that run a traversal argument keep running it (the refusal is only where a value
/// is expected): `property(T.id, traversal)` (the regression of the previous fix),
/// `property(key, traversal)`, `merge_v(traversal)`, `has(key, traversal)`, `combine(traversal)`,
/// `date_diff(traversal)` and `with_side_effect(name, graph)`.
#[test]
fn forms_that_run_a_traversal_argument_still_run_it() {
    let graph = Arc::new(Graph::new(TraversalGraph::new()));
    let id = eval_value(
        graph.clone(),
        r#"g.inject(#{id: "p1"}).as("m").add_v("person").property(T.id, __.select("m").select("id")).id().next()"#,
    )
    .unwrap();
    assert_eq!(id.into_string().unwrap(), "p1");

    let graph = modern();
    let friends = eval_value(
        graph.clone(),
        r#"g.v("1").property("friends", __.out("knows").count()).values("friends").next()"#,
    )
    .unwrap();
    assert_eq!(friends.as_int().unwrap(), 2);
    let merged = eval_value(
        graph.clone(),
        r#"g.v("2").merge_v(__.project("name").by("name")).id().next()"#,
    )
    .unwrap();
    assert_eq!(merged.into_string().unwrap(), "2");
    let has = eval_value(
        graph.clone(),
        r#"g.v().has("name", __.is("marko")).count().next()"#,
    )
    .unwrap();
    assert_eq!(has.as_int().unwrap(), 1);
    let combined = eval_value(
        graph.clone(),
        r#"g.inject([1, 2]).combine(__.constant([3])).next()"#,
    )
    .unwrap();
    assert_eq!(combined.into_typed_array::<i64>().unwrap(), vec![1, 2, 3]);
    let days = eval_value(
        graph.clone(),
        r#"g.inject(date("2024-01-01")).date_diff(__.constant(date("2024-01-03")), DT.day).next()"#,
    )
    .unwrap();
    assert_eq!(days.as_int().unwrap(), -2);
    let subgraph = eval_value(
        graph.clone(),
        r#"let sg = GraphSource::empty(); g.with_side_effect("sg", sg).e().has_label("knows").subgraph("sg").count().next(); sg.v().count().next()"#,
    )
    .unwrap();
    assert_eq!(subgraph.as_int().unwrap(), 3);
}

/// A traversal is never a value: the plain value forms of the refused bindings still work.
#[test]
fn the_value_forms_of_the_refused_bindings_still_work() {
    let graph = modern();
    for (query, expected) in [
        (r#"g.v("1").constant("x").next()"#, "x"),
        (r#"g.inject("a").next()"#, "a"),
        (r#"g.with_sack("s").v("1").sack().next()"#, "s"),
        (
            r#"g.with_side_effect("a", "s").v("1").select("a").next()"#,
            "s",
        ),
        (
            r#"g.inject("x").as_number(GType.LONG, CastPolicy.Default("d")).next()"#,
            "d",
        ),
    ] {
        let value = eval_value(graph.clone(), query).unwrap();
        assert_eq!(value.into_string().unwrap(), expected, "{query}");
    }
}
