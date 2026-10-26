//! Saved queries that write (`book/src/users_guide/saved_queries.md`, "Queries that write"):
//! the dialect recomputed on every save, the inferred effects in `g.queries()`, a call as one
//! unit with its commit metadata, rollback, the effect set asked before the first write,
//! nesting, the read-only result, the read-only dialect of stored queries and unavailable ones.
#![cfg(feature = "script")]

use std::cell::RefCell;
use std::sync::Arc;

use graphersal::catalog::{QUERY_DIALECT, QueryDefinition, WRITING_QUERY_DIALECT};
use graphersal::changes::{ChangeSet, CommitHook, HookError};
use graphersal::prelude::*;

use crate::script_host::{eval_value, eval_value_atomic, eval_value_dry_run};
use graphersal::script::rhai::{self, Dynamic};
use graphersal::script::{ScriptLimits, saved_query_effects, with_inferred_dialect};

fn modern() -> Arc<Graph> {
    Arc::new(GraphSource::tinkerpop_modern())
}

fn run(graph: &Arc<Graph>, script: &str) -> Result<Dynamic, String> {
    eval_value(graph.clone(), script).map_err(|err| err.to_string())
}

fn ok(graph: &Arc<Graph>, script: &str) -> Dynamic {
    run(graph, script).unwrap_or_else(|err| panic!("script failed: {script}\n{err}"))
}

fn err(graph: &Arc<Graph>, script: &str) -> String {
    match run(graph, script) {
        Ok(value) => panic!("script must fail: {script}\ngot {value:?}"),
        Err(err) => err,
    }
}

fn assert_contains(text: &str, part: &str) {
    assert!(text.contains(part), "expected `{part}` in:\n{text}");
}

fn count(graph: &Arc<Graph>, script: &str) -> i64 {
    ok(graph, script).as_int().expect("an integer")
}

fn dialect(graph: &Arc<Graph>, name: &str) -> String {
    ok(graph, &format!(r#"g.get_query("{name}")["dialect"]"#)).to_string()
}

/// Stores a query through the Rust API, which keeps the dialect as given.
fn store(graph: &Arc<Graph>, name: &str, query: QueryDefinition) {
    graph
        .write()
        .traversal_mut()
        .define_query(name, query)
        .unwrap();
}

/// The `effects` texts of a `g.get_query(name)` entry field.
fn effect_texts(graph: &Arc<Graph>, name: &str, field: &str) -> Vec<String> {
    ok(graph, &format!(r#"g.get_query("{name}")["{field}"]"#))
        .cast::<rhai::Array>()
        .into_iter()
        .map(|effect| effect.cast::<rhai::Map>()["text"].to_string())
        .collect()
}

const ADD_PERSON: &str = r#"g.define_query(#{name: "add_person", params: #{name: "string"},
    body: "fn add_person(name) { g.add_v(\"person\").property(\"name\", name).next() }"})"#;

thread_local! {
    static COMMITS: RefCell<Vec<ChangeSet>> = const { RefCell::new(Vec::new()) };
}

struct Commits;

impl CommitHook for Commits {
    fn before_commit(&mut self, changes: &ChangeSet) -> Result<(), HookError> {
        COMMITS.with(|commits| commits.borrow_mut().push(changes.clone()));
        Ok(())
    }
}

fn commits() -> Vec<ChangeSet> {
    COMMITS.with(|commits| std::mem::take(&mut *commits.borrow_mut()))
}

// ---------------------------------------------------------------------------------------------
// The dialect and the listed effects
// ---------------------------------------------------------------------------------------------

#[test]
fn the_dialect_follows_the_inferred_effects_on_every_save() {
    let graph = modern();
    let _ = ok(&graph, ADD_PERSON);
    assert_eq!(dialect(&graph, "add_person"), WRITING_QUERY_DIALECT);
    // Saved again without writes: back to the read-only dialect, which 0.2.0 runs.
    let _ = ok(
        &graph,
        r#"g.define_query(#{name: "add_person", params: #{name: "string"},
            body: "fn add_person(name) { g.v().has(\"name\", name).count().next() }"})"#,
    );
    assert_eq!(dialect(&graph, "add_person"), QUERY_DIALECT);
    // Moving or describing a query is no save of its body.
    let _ = ok(&graph, ADD_PERSON);
    let _ = ok(
        &graph,
        r#"g.move_query("add_person", "admin"); g.describe_query("add_person", "Adds one.")"#,
    );
    assert_eq!(dialect(&graph, "add_person"), WRITING_QUERY_DIALECT);
}

#[test]
fn queries_lists_writes_and_effects() {
    let graph = modern();
    let _ = ok(&graph, ADD_PERSON);
    let _ = ok(
        &graph,
        r#"g.define_query(#{name: "people", body: "fn people() { g.v().has_label(\"person\") }"})"#,
    );
    let listed: Vec<rhai::Map> = ok(&graph, "g.queries()")
        .cast::<rhai::Array>()
        .into_iter()
        .map(|entry| entry.cast::<rhai::Map>())
        .collect();
    assert_eq!(listed[0]["name"].to_string(), "add_person");
    assert!(listed[0]["writes"].as_bool().unwrap());
    assert!(!listed[1]["writes"].as_bool().unwrap());
    assert_eq!(
        effect_texts(&graph, "add_person", "effects"),
        [
            r#"Create Data(vertex "person")"#,
            r#"Update Data(vertex "person")"#
        ]
    );
    let first = ok(&graph, r#"g.get_query("add_person")["effects"][0]"#).cast::<rhai::Map>();
    assert_eq!(first["action"].to_string(), "Create");
    assert_eq!(first["element"].to_string(), "vertex");
    assert_eq!(first["label"].to_string(), "person");
    assert!(effect_texts(&graph, "people", "effects").is_empty());
    assert!(effect_texts(&graph, "add_person", "denied_writes").is_empty());
}

#[test]
fn define_queries_sees_the_effects_of_a_query_of_the_same_source() {
    let graph = modern();
    let _ = ok(
        &graph,
        r#"g.define_queries("admin", `
            fn outer() { g.query("inner") }
            fn inner() { g.e().has_label("knows").drop().iterate() }
            fn reader() { g.v().count().next() }
        `)"#,
    );
    assert_eq!(dialect(&graph, "outer"), WRITING_QUERY_DIALECT);
    assert_eq!(dialect(&graph, "inner"), WRITING_QUERY_DIALECT);
    assert_eq!(dialect(&graph, "reader"), QUERY_DIALECT);
    assert_eq!(
        effect_texts(&graph, "outer", "effects"),
        [r#"Delete Data(edge "knows")"#]
    );
    let _ = ok(&graph, r#"g.query("outer")"#);
    assert_eq!(count(&graph, "g.e().count().next()"), 4);
}

#[test]
fn a_body_that_writes_and_hides_its_calls_is_refused() {
    let graph = modern();
    let message = err(
        &graph,
        r#"g.define_query(#{name: "q", params: #{n: "string"},
            body: "fn q(n) { g.query(n); g.add_v(\"x\").next() }"})"#,
    );
    assert_contains(&message, "Invalid body of saved query 'q'");
    assert_contains(&message, "hides what it calls");
    assert_contains(
        &message,
        "names its steps and the saved queries it calls literally",
    );
    // Without writes the same construct keeps a read-only query.
    let _ = ok(
        &graph,
        r#"g.define_query(#{name: "r", params: #{n: "string"}, body: "fn r(n) { g.query(n) }"})"#,
    );
    assert_eq!(dialect(&graph, "r"), QUERY_DIALECT);
}

#[test]
fn a_body_that_writes_the_schema_the_catalog_or_files_is_refused() {
    let graph = modern();
    for (body, part) in [
        (
            "fn q() { g.set_schema(g.infer_schema()) }",
            "it changes the schema",
        ),
        ("fn q() { g.drop_query(\"x\") }", "it changes the catalog"),
        (
            "fn q() { g.export_graphml(\"x.xml\") }",
            "it reads or writes a file",
        ),
    ] {
        let message = err(
            &graph,
            &format!("g.define_query(#{{name: \"q\", body: {body:?}}})"),
        );
        assert_contains(&message, part);
        assert_contains(&message, "a saved query may write data only");
    }
}

// ---------------------------------------------------------------------------------------------
// A call
// ---------------------------------------------------------------------------------------------

#[test]
fn a_call_writes_as_one_unit_with_the_query_and_its_arguments() {
    let graph = modern();
    let _ = ok(&graph, ADD_PERSON);
    let _ = ok(
        &graph,
        r#"g.define_query(#{name: "two", body: `fn two() {
            g.add_v("person").property("name", "a").iterate();
            g.add_v("person").property("name", "b").iterate();
        }`})"#,
    );
    graph
        .write()
        .register_commit_hook(Box::new(Commits))
        .unwrap();
    let before = graph.read().last_commit_seq();
    let _ = ok(&graph, r#"g.query("two")"#);
    assert_eq!(graph.read().last_commit_seq(), before + 1);
    let _ = ok(&graph, r#"g.query("add_person", #{name: "ann"})"#);
    let recorded = commits();
    assert_eq!(recorded.len(), 2);
    // Both traversals of the body in ONE change set (each `add_v` with its property is one
    // mutation).
    assert_eq!(recorded[0].mutations().len(), 2);
    let metadata = recorded[1].metadata().attributes();
    assert_eq!(
        metadata.get("saved_query"),
        Some(&ElementProperty::from("add_person"))
    );
    let arguments = metadata.get("saved_query_args").expect("arguments");
    assert_contains(&format!("{arguments:?}"), "ann");
    assert_eq!(
        count(&graph, r#"g.v().has("name", "ann").count().next()"#),
        1
    );
}

#[test]
fn a_failing_body_rolls_back_everything_it_wrote() {
    let graph = modern();
    let _ = ok(
        &graph,
        r#"g.define_query(#{name: "bad", body: `fn bad() {
            g.add_v("person").property("name", "x").iterate();
            g.v().values("name").as_number(GType.LONG).to_list()
        }`})"#,
    );
    let before = graph.read().last_commit_seq();
    let message = err(&graph, r#"g.query("bad")"#);
    assert_contains(&message, r#"in saved query: "bad""#);
    assert_eq!(graph.read().vertex_count(), 6);
    assert_eq!(graph.read().last_commit_seq(), before);
}

#[test]
fn a_call_inside_a_script_unit_is_a_savepoint() {
    let graph = modern();
    let _ = ok(&graph, ADD_PERSON);
    // A later failure of the script rolls the query's writes back too.
    let failed = eval_value_atomic(
        graph.clone(),
        r#"g.query("add_person", #{name: "ann"}); g.v().values("name").as_number(GType.LONG).to_list()"#,
        Default::default(),
        &ScriptLimits::default(),
    );
    assert!(failed.is_err());
    assert_eq!(graph.read().vertex_count(), 6);
    // A handled failure of the query keeps what the script wrote before.
    let _ = ok(
        &graph,
        r#"g.define_query(#{name: "bad", body: "fn bad() { g.add_v(\"x\").iterate(); g.v().values(\"name\").as_number(GType.LONG).to_list() }"})"#,
    );
    let handled = eval_value_atomic(
        graph.clone(),
        r#"g.add_v("kept").iterate(); try { g.query("bad") } catch { }; g.v().count().next()"#,
        Default::default(),
        &ScriptLimits::default(),
    )
    .unwrap();
    assert_eq!(handled.as_int().unwrap(), 7);
    assert_eq!(count(&graph, r#"g.v().has_label("x").count().next()"#), 0);
}

#[test]
fn a_dry_run_previews_a_call() {
    let graph = modern();
    let _ = ok(&graph, ADD_PERSON);
    let (_, changes) = eval_value_dry_run(
        graph.clone(),
        r#"g.query("add_person", #{name: "ann"})"#,
        Default::default(),
        &ScriptLimits::default(),
    )
    .unwrap();
    assert_eq!(changes.compact().mutations().len(), 1);
    assert_eq!(graph.read().vertex_count(), 6);
}

#[test]
fn a_query_that_writes_may_call_queries_that_write() {
    let graph = modern();
    let _ = ok(&graph, ADD_PERSON);
    let _ = ok(
        &graph,
        r#"g.define_query(#{name: "outer", body: `fn outer() {
            g.query("add_person", #{name: "ann"});
            g.e().has_label("knows").drop().iterate()
        }`})"#,
    );
    assert_eq!(
        effect_texts(&graph, "outer", "effects"),
        [
            r#"Delete Data(edge "knows")"#,
            r#"Create Data(vertex "person")"#,
            r#"Update Data(vertex "person")"#
        ]
    );
    let _ = ok(&graph, r#"g.query("outer")"#);
    assert_eq!(count(&graph, "g.v().count().next()"), 7);
    assert_eq!(count(&graph, "g.e().count().next()"), 4);
}

#[test]
fn a_read_only_query_calling_one_that_writes_stays_read_only() {
    let graph = modern();
    let _ = ok(&graph, ADD_PERSON);
    // `reader` writes nothing itself, but calls a query that writes: it is saved as one that
    // writes. Stored through the Rust API in the read-only dialect, it stays read-only.
    store(
        &graph,
        "reader",
        QueryDefinition::new(r#"fn reader() { g.query("add_person", #{name: "ann"}) }"#),
    );
    let message = err(&graph, r#"g.query("reader")"#);
    assert_contains(&message, "a saved query is read-only");
    assert_contains(&message, r#"in saved query: "add_person""#);
    assert_eq!(graph.read().vertex_count(), 6);
    assert_eq!(
        effect_texts(&graph, "reader", "denied_writes"),
        [
            r#"Create Data(vertex "person")"#,
            r#"Update Data(vertex "person")"#
        ]
    );
}

#[test]
fn what_a_query_that_writes_returns_is_read_only_on_the_callers_graph() {
    let graph = modern();
    let _ = ok(
        &graph,
        r#"g.define_query(#{name: "ret", body: "fn ret() { g.add_v(\"x\").iterate(); g.v().has_label(\"x\") }"})"#,
    );
    assert_eq!(count(&graph, r#"g.query("ret").count().next()"#), 1);
    assert_eq!(count(&graph, r#"g.v().has_label("x").count().next()"#), 1);
    let message = err(&graph, r#"g.query("ret").drop().iterate()"#);
    assert_contains(&message, "a saved query is read-only");
    assert_eq!(count(&graph, r#"g.v().has_label("x").count().next()"#), 2);
    // A returned writing traversal runs in the call's unit; the caller continues over what it
    // wrote, read-only.
    let _ = ok(
        &graph,
        r#"g.define_query(#{name: "add_person", params: #{p_name: "string"},
            body: "fn add_person(p_name) { g.add_v(\"person\").property(\"name\", p_name) }"})"#,
    );
    assert_eq!(
        count(
            &graph,
            r#"g.query("add_person", #{p_name: "Bill"}).count().next()"#
        ),
        1
    );
    assert_eq!(
        ok(
            &graph,
            r#"g.query("add_person", #{p_name: "Ann"}).values("name").next()"#
        )
        .to_string(),
        "Ann"
    );
    assert_eq!(
        count(&graph, r#"g.v().has_label("person").count().next()"#),
        6
    );
    let message = err(
        &graph,
        r#"g.query("add_person", #{p_name: "Zed"}).drop().iterate()"#,
    );
    assert_contains(&message, "a saved query is read-only");
    // The call wrote; only the continuation was refused (the script is no unit here).
    assert_eq!(
        count(&graph, r#"g.v().has("name", "Zed").count().next()"#),
        1
    );
    // Also inside a returned list; a failing returned traversal rolls the body back.
    let _ = ok(
        &graph,
        r#"g.define_query(#{name: "pair", body: "fn pair() { [g.add_v(\"y\"), g.add_v(\"z\")] }"})"#,
    );
    let pair = ok(&graph, r#"g.query("pair")"#).cast::<rhai::Array>();
    assert_eq!(pair.len(), 2);
    assert_eq!(count(&graph, r#"g.v().has_label("y").count().next()"#), 1);
    let _ = ok(
        &graph,
        r#"g.define_query(#{name: "bad", body: "fn bad() { g.add_v(\"w\").iterate(); g.v().property(\"n\", __.values(\"name\").as_number(GType.LONG)) }"})"#,
    );
    let message = err(&graph, r#"g.query("bad")"#);
    assert_contains(&message, r#"in saved query: "bad""#);
    assert_eq!(count(&graph, r#"g.v().has_label("w").count().next()"#), 0);
    // An anonymous writing traversal is no query of the graph: refused, the body rolled back.
    let _ = ok(
        &graph,
        r#"g.define_query(#{name: "anon", body: "fn anon() { g.add_v(\"w\").iterate(); __.add_v(\"x\") }"})"#,
    );
    let message = err(&graph, r#"g.query("anon")"#);
    assert_contains(&message, "returned a traversal with a writing step");
    assert_eq!(count(&graph, r#"g.v().has_label("w").count().next()"#), 0);
}

// ---------------------------------------------------------------------------------------------
// Stored dialects
// ---------------------------------------------------------------------------------------------

#[test]
fn a_stored_read_only_query_with_a_writing_body_stays_read_only_until_saved_again() {
    let graph = modern();
    store(
        &graph,
        "writer",
        QueryDefinition::new(r#"fn writer() { g.add_v("x").next() }"#),
    );
    assert_eq!(
        effect_texts(&graph, "writer", "denied_writes"),
        [r#"Create Data(vertex "x")"#]
    );
    let message = err(&graph, r#"g.query("writer")"#);
    assert_contains(&message, "a saved query is read-only");
    assert_contains(
        &message,
        "becomes a query that writes when it is saved again",
    );
    assert_eq!(graph.read().vertex_count(), 6);
    // Saved again: it writes.
    let _ = ok(
        &graph,
        r#"g.define_query(#{name: "writer", body: g.get_query("writer")["body"]})"#,
    );
    assert_eq!(dialect(&graph, "writer"), WRITING_QUERY_DIALECT);
    let _ = ok(&graph, r#"g.query("writer")"#);
    assert_eq!(graph.read().vertex_count(), 7);
}

#[test]
fn a_query_that_cannot_run_is_listed_as_unavailable() {
    let graph = modern();
    store(
        &graph,
        "broken",
        QueryDefinition::new("fn broken() { g.v( }").with_dialect(WRITING_QUERY_DIALECT),
    );
    store(
        &graph,
        "hidden",
        QueryDefinition::new(r#"fn hidden() { let f = Fn("drop"); g.v().count() }"#)
            .with_dialect(WRITING_QUERY_DIALECT),
    );
    store(
        &graph,
        "future",
        QueryDefinition::new("fn future() { 1 }").with_dialect("graphersal-rhai/3"),
    );
    let _ = ok(&graph, ADD_PERSON);
    let entry = |name: &str| ok(&graph, &format!(r#"g.get_query("{name}")"#)).cast::<rhai::Map>();
    for (name, reason) in [
        ("broken", "it does not compile"),
        ("hidden", "hides what it calls"),
        (
            "future",
            "this version runs 'graphersal-rhai/1' and 'graphersal-rhai/2'",
        ),
    ] {
        let listed = entry(name);
        assert_eq!(listed["status"].to_string(), "error", "{name}");
        assert_contains(&listed["error"].to_string(), reason);
        assert!(!listed["writes"].as_bool().unwrap());
        assert_contains(
            &err(&graph, &format!(r#"g.query("{name}")"#)),
            &format!("Invalid body of saved query '{name}'"),
        );
    }
    // The others still run.
    assert_eq!(entry("add_person")["status"].to_string(), "ok");
    let _ = ok(&graph, r#"g.query("add_person", #{name: "ann"})"#);
}

#[test]
fn the_rust_api_infers_effects_and_dialects_for_front_ends() {
    let graph = modern();
    let _ = ok(&graph, ADD_PERSON);
    let engine = graphersal::script::engine();
    let stored = graph.clone();
    let catalog = move |name: &str| {
        stored
            .read()
            .traversal()
            .get_definition(graphersal::catalog::DefinitionKind::QUERY, name)
            .ok()
            .flatten()
            .and_then(|definition| definition.as_query().cloned())
    };
    let query = with_inferred_dialect(
        &engine,
        &catalog,
        "outer",
        QueryDefinition::new(r#"fn outer() { g.query("add_person", #{name: "x"}) }"#),
    )
    .unwrap();
    assert_eq!(query.dialect(), WRITING_QUERY_DIALECT);
    let effects = saved_query_effects(&engine, &catalog, "outer", &query).unwrap();
    assert!(effects.writes);
    assert_eq!(effects.effects.len(), 2);
    let read_only = with_inferred_dialect(
        &engine,
        &catalog,
        "count",
        QueryDefinition::new("fn count() { g.v().count() }").with_dialect(WRITING_QUERY_DIALECT),
    )
    .unwrap();
    assert_eq!(read_only.dialect(), QUERY_DIALECT);
    let refused = with_inferred_dialect(
        &engine,
        &catalog,
        "s",
        QueryDefinition::new("fn s() { g.set_schema(#{}) }"),
    )
    .unwrap_err();
    assert_contains(&refused, "it changes the schema");
}

#[test]
fn a_closure_writes_inside_its_query() {
    // Issue #16: a closure's writes are inferred like the function's, also when it captures a
    // parameter (Rhai compiles that into `curry(Fn("anon$.."), ..)`, which hides nothing).
    let graph = Arc::new(GraphSource::tinkerpop_modern());
    let _ = ok(
        &graph,
        r#"g.define_query(#{name: "add_named", params: #{label: "string"},
            body: `fn add_named(label) {
                       ["p1", "p2"].for_each(|n| g.add_v(label).property("name", n).iterate());
                   }`})"#,
    );
    assert_eq!(dialect(&graph, "add_named"), "graphersal-rhai/2");
    assert_eq!(
        effect_texts(&graph, "add_named", "effects"),
        ["Create Data(vertex)", "Update Data(vertex)"]
    );
    let _ = ok(&graph, r#"g.query("add_named", #{label: "dog"})"#);
    assert_eq!(count(&graph, r#"g.v().has_label("dog").count().next()"#), 2);

    // A closure called through a variable still hides what it calls.
    let message = err(
        &graph,
        r#"g.define_query(#{name: "hidden", body: "fn hidden() { let f = |x| g.add_v(\"a\").iterate(); f.call(1) }"})"#,
    );
    assert_contains(&message, "uses call, which hides what it calls");
}
