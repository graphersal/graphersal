//! The canonical rendering of steps in `.profile()`, error locations and `help()`: one text per
//! step that reads like the DSL (snake_case names, Rhai literals, `__.` children, tokens such as
//! `Order.desc`, no `Debug` text), and its Gremlin camelCase spelling through
//! `g.with("render.spelling", "camel")` or the session option of a front end.
#![cfg(feature = "script")]

use std::sync::Arc;

use crate::script_host::{eval, eval_value, graph_scope_with_options};
use graphersal::exec::{RenderOptions, Spelling};
use graphersal::prelude::*;
use graphersal::profile::Metrics;

fn modern() -> Arc<Graph> {
    Arc::new(GraphSource::tinkerpop_modern())
}

/// Every step name of a profile, depth first.
fn all_names(metrics: &TraversalMetrics) -> Vec<String> {
    fn walk(list: &[Metrics], out: &mut Vec<String>) {
        for m in list {
            out.push(m.name().to_string());
            walk(m.nested(), out);
        }
    }
    let mut out = Vec::new();
    walk(metrics.metrics(), &mut out);
    out
}

/// The top-level step names of the profile of `query` (a DSL traversal starting with `g.`), in
/// the default spelling and with `render.spelling` set to camel.
fn names(query: &str) -> (Vec<String>, Vec<String>) {
    let top = |script: String| -> Vec<String> {
        let metrics = eval_value(modern(), &format!("{script}.profile()"))
            .unwrap_or_else(|err| panic!("{script}: {err}"))
            .cast::<TraversalMetrics>();
        metrics
            .metrics()
            .iter()
            .map(|m| m.name().to_string())
            .collect()
    };
    let rest = query.strip_prefix("g.").expect("a query starting with g.");
    (
        top(query.to_string()),
        top(format!(r#"g.with("render.spelling", "camel").{rest}"#)),
    )
}

/// One row: the query, the name of its last top-level step in snake_case and in camelCase.
/// Covers every step family: sources and fused sources, append, map, flat map, filter, flat
/// barrier, barrier, side effect, mutation, labels, child traversals, predicates and tokens.
const FAMILIES: &[(&str, &str, &str)] = &[
    // sources, fused by the optimizer
    (r#"g.V("1", "2")"#, r#"v("1", "2")"#, r#"v("1", "2")"#),
    (
        r#"g.V().hasLabel("person")"#,
        r#"v(labels: ["person"])"#,
        r#"v(labels: ["person"])"#,
    ),
    (
        r#"g.V().hasLabel("person").count()"#,
        r#"v(labels: ["person"]).count()"#,
        r#"v(labels: ["person"]).count()"#,
    ),
    (
        "g.E().groupCount().by(T.label)",
        "e().group_count().by(T.label)",
        "e().groupCount().by(T.label)",
    ),
    // an empty id intersection is no full scan
    (r#"g.V("1").hasId("2")"#, "v([])", "v([])"),
    // append, with every literal kind
    (
        r#"g.inject(1, 2.5, "a", [1, true], #{a: (), "b c": 2})"#,
        r#"inject(1, 2.5, "a", [1, true], #{a: (), "b c": 2})"#,
        r#"inject(1, 2.5, "a", [1, true], #{a: (), "b c": 2})"#,
    ),
    // map
    (
        r#"g.V().valueMap(true, "name")"#,
        r#"value_map(true, "name")"#,
        r#"valueMap(true, "name")"#,
    ),
    (
        r#"g.V().elementMap("name")"#,
        r#"element_map("name")"#,
        r#"elementMap("name")"#,
    ),
    (
        r#"g.V().as("a").out().as("b").select("a", "b").by("name")"#,
        r#"select("a", "b").by("name")"#,
        r#"select("a", "b").by("name")"#,
    ),
    (
        r#"g.V().project("x").by(__.outE().count())"#,
        r#"project("x").by(__.out_e().count())"#,
        r#"project("x").by(__.outE().count())"#,
    ),
    (
        r#"g.V().values("age").math("_ + 1")"#,
        r#"math("_ + 1")"#,
        r#"math("_ + 1")"#,
    ),
    (
        "g.V().constant(#{a: 1})",
        "constant(#{a: 1})",
        "constant(#{a: 1})",
    ),
    (
        r#"g.V().values("age").fold().count(Scope.local)"#,
        "count(Scope.local)",
        "count(Scope.local)",
    ),
    (
        r#"g.V().fold().order(Scope.local).by("age", Order.desc)"#,
        r#"order(Scope.local).by("age", Order.desc)"#,
        r#"order(Scope.local).by("age", Order.desc)"#,
    ),
    (
        r#"g.V().values("name").toUpper()"#,
        "to_upper()",
        "toUpper()",
    ),
    (
        r#"g.V().values("age").asNumber(GType.LONG)"#,
        "as_number(GType.LONG)",
        "asNumber(GType.LONG)",
    ),
    (
        r#"g.V().out().path().by("name")"#,
        r#"path().by("name")"#,
        r#"path().by("name")"#,
    ),
    (
        r#"g.V().as("a").out().as("a").select(Pop.first, "a")"#,
        r#"select(Pop.first, "a")"#,
        r#"select(Pop.first, "a")"#,
    ),
    (
        "g.V().groupCount().by(T.label).select(Column.keys)",
        "select(Column.keys)",
        "select(Column.keys)",
    ),
    (
        r#"g.V().values("name").fold().combine(["x"])"#,
        r#"combine(["x"])"#,
        r#"combine(["x"])"#,
    ),
    (
        r#"g.V().values("name").split(", ")"#,
        r#"split(", ")"#,
        r#"split(", ")"#,
    ),
    // flat map
    (
        r#"g.V().out("knows")"#,
        r#"out("knows")"#,
        r#"out("knows")"#,
    ),
    ("g.V().inE()", "in_e()", "inE()"),
    ("g.E().bothV()", "both_v()", "bothV()"),
    (
        "g.V().union(__.out(), __.in())",
        "union(__.out(), __.in())",
        "union(__.out(), __.in())",
    ),
    (
        r#"g.V().coalesce(__.out("knows"), __.identity())"#,
        r#"coalesce(__.out("knows"), __.identity())"#,
        r#"coalesce(__.out("knows"), __.identity())"#,
    ),
    (
        "g.V().local(__.out().limit(1))",
        "local(__.out().limit(1))",
        "local(__.out().limit(1))",
    ),
    (
        r#"g.V().choose(__.has("age"), __.values("age"), __.constant("x"))"#,
        r#"choose(__.has("age"), __.values("age"), __.constant("x"))"#,
        r#"choose(__.has("age"), __.values("age"), __.constant("x"))"#,
    ),
    (
        r#"g.V().choose(__.values("age")).option(P.gt(30), __.constant("old")).option(Pick.none, __.constant("young"))"#,
        r#"choose(__.values("age")).option(P.gt(30), __.constant("old")).option(Pick.none, __.constant("young"))"#,
        r#"choose(__.values("age")).option(P.gt(30), __.constant("old")).option(Pick.none, __.constant("young"))"#,
    ),
    // filter, with predicates
    (
        r#"g.V().has("age", P.gt(30).and(P.lt(40)))"#,
        r#"has("age", P.gt(30).and(P.lt(40)))"#,
        r#"has("age", P.gt(30).and(P.lt(40)))"#,
    ),
    (
        r#"g.V().out().hasLabel("software")"#,
        r#"has_label("software")"#,
        r#"hasLabel("software")"#,
    ),
    (
        r#"g.V().hasNot("age")"#,
        r#"has_not("age")"#,
        r#"hasNot("age")"#,
    ),
    (
        r#"g.V().values("age").is(P.within([29, 27]))"#,
        "is(P.within([29, 27]))",
        "is(P.within([29, 27]))",
    ),
    (
        r#"g.V().as("a").out().where(P.neq("a"))"#,
        r#"where(P.neq("a"))"#,
        r#"where(P.neq("a"))"#,
    ),
    (
        r#"g.V().where(__.as("a").out().as("b"))"#,
        r#"where(__.as("a").out().as("b"))"#,
        r#"where(__.as("a").out().as("b"))"#,
    ),
    ("g.V().not(__.out())", "not(__.out())", "not(__.out())"),
    (
        "g.V().and(__.out(), __.in())",
        "and(__.out(), __.in())",
        "and(__.out(), __.in())",
    ),
    ("g.V().out().simplePath()", "simple_path()", "simplePath()"),
    (
        "g.V().dedup().by(T.label)",
        "dedup().by(T.label)",
        "dedup().by(T.label)",
    ),
    (
        r#"g.V().has("name", TextP.startingWith("m"))"#,
        r#"has("name", TextP.starting_with("m"))"#,
        r#"has("name", TextP.startingWith("m"))"#,
    ),
    (
        r#"g.V().has("age", P.typeOf(GType.LONG))"#,
        r#"has("age", P.typeOf(GType.LONG))"#,
        r#"has("age", P.typeOf(GType.LONG))"#,
    ),
    // flat barrier
    (
        r#"g.V().order().by("age", Order.desc)"#,
        r#"order().by("age", Order.desc)"#,
        r#"order().by("age", Order.desc)"#,
    ),
    ("g.V().range(0, 2)", "range(0, 2)", "range(0, 2)"),
    (
        r#"g.V().repeat(__.out()).until(__.hasLabel("software")).emit()"#,
        r#"repeat(__.out()).until(__.has_label("software")).emit()"#,
        r#"repeat(__.out()).until(__.hasLabel("software")).emit()"#,
    ),
    // barrier
    (
        r#"g.V().values("age").fold(0, Operator.sum)"#,
        "fold(0, Operator.sum)",
        "fold(0, Operator.sum)",
    ),
    ("g.V().groupCount()", "group_count()", "groupCount()"),
    (
        r#"g.V().aggregate("x").cap("x")"#,
        r#"cap("x")"#,
        r#"cap("x")"#,
    ),
    // side effect
    (
        "g.V().sideEffect(__.out())",
        "side_effect(__.out())",
        "sideEffect(__.out())",
    ),
    (
        r#"g.V().groupCount("x").by(T.label)"#,
        r#"group_count("x").by(T.label)"#,
        r#"groupCount("x").by(T.label)"#,
    ),
    (
        r#"g.withSack(0).V().sack(Operator.sum).by("age")"#,
        r#"sack(Operator.sum).by("age")"#,
        r#"sack(Operator.sum).by("age")"#,
    ),
    // mutation, a folded add_v() under the name the profile documents
    (
        r#"g.addV("person").property("name", "x")"#,
        r#"add_v("person", properties: ["name"])"#,
        r#"addV("person", properties: ["name"])"#,
    ),
    (
        r#"g.V().property("x", 1)"#,
        r#"property("x", 1)"#,
        r#"property("x", 1)"#,
    ),
    (
        r#"g.V("1").as("a").V("2").addE("knows").from("a")"#,
        r#"add_e("knows").from("a")"#,
        r#"addE("knows").from("a")"#,
    ),
    (
        r#"g.V().remove_property("age")"#,
        r#"remove_property("age")"#,
        r#"remove_property("age")"#,
    ),
    (
        r#"g.mergeV(#{"T.label": "person"})"#,
        r#"merge_v(#{"T.label": "person"}) [lookup: label]"#,
        r#"mergeV(#{"T.label": "person"}) [lookup: label]"#,
    ),
];

#[test]
fn every_step_family_renders_like_the_dsl_in_both_spellings() {
    let mut problems = Vec::new();
    for (query, snake, camel) in FAMILIES {
        let (snake_names, camel_names) = names(query);
        if snake_names.last().map(String::as_str) != Some(*snake) {
            problems.push(format!(
                "{query}\n  snake: {snake_names:?}\n  want:  {snake}"
            ));
        }
        if camel_names.last().map(String::as_str) != Some(*camel) {
            problems.push(format!(
                "{query}\n  camel: {camel_names:?}\n  want:  {camel}"
            ));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// The rendering never shows `Debug` text: no struct dumps, no `None`/`Some(`, no Rust-only
/// step names, no leading dot.
#[test]
fn no_debug_text_reaches_a_profile() {
    let mut problems = Vec::new();
    for (query, _, _) in FAMILIES {
        for spelling in ["snake", "camel"] {
            let rest = query.strip_prefix("g.").unwrap_or(query);
            let script = format!(r#"g.with("render.spelling", "{spelling}").{rest}.profile()"#);
            let metrics = eval_value(modern(), &script)
                .unwrap_or_else(|err| panic!("{script}: {err}"))
                .cast::<TraversalMetrics>();
            for name in all_names(&metrics) {
                for artefact in [
                    "{ ",
                    "None",
                    "Some(",
                    "AnonymousTraversal",
                    "(Traversal(",
                    "(Property(",
                    "__::",
                    "where_t",
                    "has_p(",
                    "where_p",
                    "start=",
                ] {
                    if name.contains(artefact) || name.starts_with('.') {
                        problems.push(format!("{query} ({spelling}): {name}"));
                    }
                }
            }
        }
    }
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn camel_spelling_respells_error_locations_and_help() {
    let report = eval(
        modern(),
        r#"g.with("render.spelling", "camel").V().hasLabel("person").values("name").sum().toList()"#,
    )
    .unwrap_err()
    .to_string();
    assert!(
        report.contains(r#"at #2: v().hasLabel("person").values("name").sum()"#),
        "{report}"
    );
    assert!(
        report.contains(r#"plan:  v(labels: ["person"]).values("name").sum()"#),
        "{report}"
    );
    assert!(
        report.contains(r#"values("name").asNumber(GType.LONG)"#),
        "help is respelled too: {report}"
    );
    assert!(!report.contains("has_label"), "{report}");
    assert!(!report.contains("as_number"), "{report}");

    let snake = eval(
        modern(),
        r#"g.V().hasLabel("person").values("name").sum().toList()"#,
    )
    .unwrap_err()
    .to_string();
    assert!(
        snake.contains(r#"at #2: v().has_label("person").values("name").sum()"#),
        "{snake}"
    );
    assert!(snake.contains("as_number(GType.LONG)"), "{snake}");
}

/// The caret stays under the failing step in the respelled plan.
#[test]
fn the_caret_points_at_the_step_in_the_respelled_plan() {
    let report = eval(
        modern(),
        r#"g.with("render.spelling", "camel").V().sideEffect(__.drop()).math("_ + 1").toList()"#,
    )
    .unwrap_err()
    .to_string();
    let lines: Vec<&str> = report.lines().collect();
    let at = lines
        .iter()
        .position(|line| line.contains("at #2:"))
        .unwrap_or_else(|| panic!("{report}"));
    assert!(
        lines[at].ends_with(r#"v().sideEffect(__.drop()).math("_ + 1")"#),
        "{report}"
    );
    let column = lines[at].find(r#"math("#).unwrap();
    let caret = lines[at + 1];
    assert_eq!(caret.find('^'), Some(column), "{report}");
    assert_eq!(caret.trim().len(), r#"math("_ + 1")"#.len(), "{report}");
}

#[test]
fn the_rust_api_takes_the_option_too() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let metrics = lock
        .traversal()
        .with("render.spelling", "camel")
        .v(None)
        .out_e(None)
        .group_count()
        .by(By::Label)
        .profile()
        .unwrap();
    assert_eq!(
        all_names(&metrics),
        ["v()", "outE()", "groupCount().by(T.label)"]
    );
    let execution = lock
        .traversal()
        .with("render.spelling", "camel")
        .v(None)
        .values("name")
        .sum()
        .execute_with(ExecOptions::profile(ProfileType::none()));
    let error = execution.error().expect("sum of names fails");
    let location = error.step_location().expect("a step location");
    assert_eq!(location.step_path[0].name, "sum()");
    assert_eq!(error.spelling(), Spelling::Camel);
}

/// A front end passes the spelling as a session option (`RenderOptions::spelling`, CLI
/// `--spelling`, the playground setting); a query's own `g.with()` wins.
#[test]
fn the_session_option_sets_the_default_and_a_query_overrides_it() {
    let options = graphersal::script::render_scope_options(
        &RenderOptions::new().with_spelling(Spelling::Camel),
    );
    assert_eq!(
        options
            .iter()
            .find(|(key, _)| *key == "render.spelling")
            .map(|(_, value)| value.clone()),
        Some(ElementProperty::from("camel"))
    );
    let mut scope = graph_scope_with_options(modern(), &options);
    let engine = crate::script_host::engine();
    let names = |scope: &mut graphersal::script::rhai::Scope, script: &str| -> Vec<String> {
        let metrics = engine
            .eval_with_scope::<graphersal::script::rhai::Dynamic>(scope, script)
            .unwrap()
            .cast::<TraversalMetrics>();
        all_names(&metrics)
    };
    assert_eq!(
        names(&mut scope, "g.V().outE().profile()"),
        ["v()", "outE()"]
    );
    assert_eq!(
        names(
            &mut scope,
            r#"g.with("render.spelling", "snake").V().outE().profile()"#
        ),
        ["v()", "out_e()"]
    );
    // the default spelling adds no session option
    assert!(graphersal::script::render_scope_options(&RenderOptions::new()).is_empty());
}

#[test]
fn an_invalid_spelling_is_an_invalid_option_with_its_own_help() {
    let report = eval(
        modern(),
        r#"g.with("render.spelling", "kebab").V().count().next()"#,
    )
    .unwrap_err()
    .to_string();
    assert!(
        report.contains(r#"Invalid value for option 'render.spelling': expected "snake" or "camel", got string "kebab""#),
        "{report}"
    );
    assert!(
        report.contains(r#"g.with("render.spelling", "camel").v().has_label("person").profile()"#),
        "{report}"
    );
}

/// Results never depend on the spelling.
#[test]
fn data_output_is_unaffected() {
    for query in [
        r#"g.V().hasLabel("person").outE().groupCount().by(T.label).toList()"#,
        r#"g.V().valueMap(true, "name").toList()"#,
    ] {
        let rest = query.strip_prefix("g.").unwrap();
        let snake = eval(modern(), query).unwrap();
        let camel = eval(
            modern(),
            &format!(r#"g.with("render.spelling", "camel").{rest}"#),
        )
        .unwrap();
        assert_eq!(snake, camel, "{query}");
    }
}

/// A nested child keeps its place in the profile tree and is respelled with it.
#[test]
fn nested_steps_are_respelled() {
    let metrics = eval_value(
        modern(),
        r#"g.with("render.spelling", "camel").V().where(__.outE().hasLabel("knows")).profile()"#,
    )
    .unwrap()
    .cast::<TraversalMetrics>();
    // `outE().hasLabel("knows")` is folded into one `outE("knows")` (`barrier_pushdown`)
    assert_eq!(
        all_names(&metrics),
        ["v()", r#"where(__.outE("knows"))"#, r#"outE("knows")"#]
    );
}
