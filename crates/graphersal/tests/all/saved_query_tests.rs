//! Saved queries in the Rhai DSL (`book/src/users_guide/saved_queries.md`): defining
//! (`define_query`, `define_queries`), calling (`query`: named arguments, defaults, values and
//! continued traversals), read-only enforcement (direct, nested, continued, returned), the call
//! depth bound, the compiled-body cache across redefinition and rollback, management
//! (`queries`, `get_query`, `drop_query`, `move_query`, `describe_query`) and the errors.
#![cfg(feature = "script")]

use std::sync::Arc;

use graphersal::catalog::{DefinitionKind, QueryDefinition, QueryParam};
use graphersal::prelude::*;

use crate::script_host::{ScriptGraphTraversalSource, ScriptLimits, eval_value, eval_value_atomic};
use graphersal::script::rhai::{self, Dynamic};

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

fn text(value: Dynamic) -> String {
    value.to_string()
}

/// The modern graph with `people(min = 0)`: persons older than `min`, in folder `reports/people`.
fn with_people() -> Arc<Graph> {
    let graph = modern();
    let _ = ok(
        &graph,
        r#"g.define_query(#{
            name: "people",
            folder: "reports/people",
            params: #{min: #{schema: #{type: "integer", minimum: 0, maximum: 200}, default: 0}},
            body: `/// Persons older than min.
                   fn people(min) { g.v().has_label("person").has("age", P.gt(min)) }`
        })"#,
    );
    graph
}

/// Stores a query through the Rust API, which keeps the read-only dialect it is given (a query
/// saved by 0.2.0 or loaded from a catalog file): its writes are denied when it is called.
fn store_read_only(graph: &Arc<Graph>, name: &str, body: &str) {
    graph
        .write()
        .traversal_mut()
        .define_query(name, QueryDefinition::new(body))
        .unwrap();
}

fn query_count(graph: &Arc<Graph>) -> usize {
    graph
        .read()
        .traversal()
        .list_definitions(Some(DefinitionKind::QUERY))
        .unwrap()
        .len()
}

// ---------------------------------------------------------------------------------------------
// Defining and calling
// ---------------------------------------------------------------------------------------------

#[test]
fn a_defined_query_is_called_with_named_arguments_and_defaults() {
    let graph = with_people();
    assert_eq!(
        text(ok(
            &graph,
            r#"g.query("people", #{min: 30}).values("name").order().to_list()"#
        )),
        r#"["josh", "peter"]"#
    );
    // The default (0) is substituted when the argument is left out.
    assert_eq!(text(ok(&graph, r#"g.query("people").count().next()"#)), "4");
    assert_eq!(
        text(ok(&graph, r#"g.query("people", #{}).count().next()"#)),
        "4"
    );
}

#[test]
fn the_camel_case_spellings_work_alike() {
    let graph = modern();
    let _ = ok(
        &graph,
        r#"g.define_query(#{name: "names", body: "fn names() { g.v().values(\"name\") }"});
           g.define_queries("x", "fn one() { 1 }");
           g.move_query("names", "y");
           g.describe_query("names", "All names.");"#,
    );
    let entry = ok(&graph, r#"g.get_query("names")"#).cast::<rhai::Map>();
    assert_eq!(entry["folder"].to_string(), "y");
    assert_eq!(entry["description"].to_string(), "All names.");
    assert_eq!(text(ok(&graph, r#"g.drop_query("one")"#)), "true");
    assert_eq!(query_count(&graph), 1);
}

#[test]
fn a_query_may_return_a_value() {
    let graph = modern();
    let _ = ok(
        &graph,
        r#"g.define_query(#{name: "stats", body: `fn stats() {
            #{people: g.v().has_label("person").count().next(),
              names: g.v().values("name").order().limit(2).to_list()}
        }`})"#,
    );
    let value = ok(&graph, r#"g.query("stats")"#).cast::<rhai::Map>();
    assert_eq!(value["people"].as_int().unwrap(), 4);
    assert_eq!(value["names"].to_string(), r#"["josh", "lop"]"#);
}

#[test]
fn a_returned_traversal_is_continued_and_optimized_with_the_caller_steps() {
    let graph = modern();
    let _ = ok(
        &graph,
        r#"g.define_query(#{name: "everything", body: "fn everything() { g.v() }"})"#,
    );
    assert_eq!(
        text(ok(
            &graph,
            r#"g.query("everything").has_label("person").count().next()"#
        )),
        "4"
    );
    // The caller's filter is pushed into the body's v(): one fused source step.
    let profile = text(ok(
        &graph,
        r#"g.query("everything").has_label("person").count().profile().to_string()"#,
    ));
    assert_contains(&profile, r#"v(labels: ["person"]).count()"#);
    assert_contains(&profile, "source_filter_pushdown");
}

#[test]
fn query_works_in_loops_functions_and_bodies() {
    let graph = with_people();
    let _ = ok(
        &graph,
        r#"g.define_query(#{name: "older_people", params: #{min: "integer"},
            body: "fn older_people(min) { g.query(\"people\", #{min: min}).values(\"name\") }"})"#,
    );
    assert_eq!(
        text(ok(
            &graph,
            r#"let counts = [];
               for min in [0, 30] { counts.push(g.query("people", #{min: min}).count().next()); }
               counts"#
        )),
        "[4, 2]"
    );
    assert_eq!(
        text(ok(
            &graph,
            r#"g.query("older_people", #{min: 31}).order().to_list()"#
        )),
        r#"["josh", "peter"]"#
    );
}

#[test]
fn an_int64_argument_is_accepted_for_a_number_parameter() {
    let graph = modern();
    let _ = ok(
        &graph,
        r#"g.define_query(#{name: "half", params: #{x: "number"}, body: "fn half(x) { x / 2.0 }"})"#,
    );
    assert_eq!(text(ok(&graph, r#"g.query("half", #{x: 3})"#)), "1.5");
    assert_eq!(text(ok(&graph, r#"g.query("half", #{x: 3.0})"#)), "1.5");
}

#[test]
fn a_uuid_parameter_takes_a_uuid_value() {
    let graph = modern();
    let _ = ok(
        &graph,
        r#"g.define_query(#{name: "echo", params: #{id: "uuid"}, body: "fn echo(id) { id }"})"#,
    );
    let echoed = ok(
        &graph,
        r#"g.query("echo", #{id: UUID.from_string("123e4567-e89b-12d3-a456-426614174000")}).to_string()"#,
    );
    assert_eq!(echoed.to_string(), "123e4567-e89b-12d3-a456-426614174000");
}

#[test]
fn defining_again_replaces_the_query() {
    let graph = modern();
    let _ = ok(
        &graph,
        r#"g.define_query(#{name: "n", body: "fn n() { 1 }"})"#,
    );
    assert_eq!(text(ok(&graph, r#"g.query("n")"#)), "1");
    let _ = ok(
        &graph,
        r#"g.define_query(#{name: "n", body: "fn n() { 2 }"})"#,
    );
    assert_eq!(text(ok(&graph, r#"g.query("n")"#)), "2");
    assert_eq!(query_count(&graph), 1);
}

#[test]
fn the_description_comes_from_the_doc_comment_unless_given() {
    let graph = modern();
    let _ = ok(
        &graph,
        r#"g.define_query(#{name: "a", body: "/// From the comment.\nfn a() { 1 }"});
           g.define_query(#{name: "b", description: "Given.", body: "/// Comment.\nfn b() { 1 }"})"#,
    );
    assert_eq!(
        text(ok(&graph, r#"g.get_query("a")["description"]"#)),
        "From the comment."
    );
    assert_eq!(
        text(ok(&graph, r#"g.get_query("b")["description"]"#)),
        "Given."
    );
}

#[test]
fn define_queries_stores_every_function_with_its_doc_comment() {
    let graph = modern();
    let names = ok(
        &graph,
        r#"g.define_queries("reports/sales", `
            /// Persons, oldest first.
            /// At most n of them.
            fn oldest(n) { g.v().has_label("person").order().by("age", Order.desc).limit(n) }

            /** Software names. */
            fn software() { g.v().has_label("software").values("name") }
        `, #{oldest: #{n: #{schema: #{type: "integer", minimum: 1}, default: 2}}})"#,
    );
    assert_eq!(names.to_string(), r#"["oldest", "software"]"#);
    let oldest = ok(&graph, r#"g.get_query("oldest")"#).cast::<rhai::Map>();
    assert_eq!(oldest["folder"].to_string(), "reports/sales");
    assert_eq!(
        oldest["description"].to_string(),
        "Persons, oldest first.\nAt most n of them."
    );
    // Each query stores the text of its own function only.
    let body = oldest["body"].to_string();
    assert!(body.starts_with("fn oldest(n) {"), "{body}");
    assert!(!body.contains("software"), "{body}");
    assert_eq!(
        text(ok(&graph, r#"g.query("oldest").values("name").to_list()"#)),
        r#"["peter", "josh"]"#
    );
    assert_eq!(
        text(ok(&graph, r#"g.query("software").order().to_list()"#)),
        r#"["lop", "ripple"]"#
    );
    assert_eq!(
        text(ok(&graph, r#"g.get_query("software")["description"]"#)),
        "Software names."
    );
}

#[test]
fn define_queries_stores_all_or_nothing() {
    let graph = modern();
    let message = err(
        &graph,
        r#"g.define_queries("", `fn good() { 1 } fn bad(x) { x }`)"#,
    );
    assert_contains(&message, "Invalid body of saved query 'bad'");
    assert_contains(&message, "'x' is not declared in params");
    assert_eq!(query_count(&graph), 0);
    // A catalog rule broken by one query (a name longer than 128 bytes) stores none either:
    // the queries are stored in one unit, rolled back as a whole.
    let message = err(
        &graph,
        "g.define_queries(\"\", `fn good() { 1 } fn qxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx() { 2 }`)",
    );
    assert_contains(&message, "Invalid query name");
    assert_eq!(query_count(&graph), 0);
}

// ---------------------------------------------------------------------------------------------
// Checks at definition
// ---------------------------------------------------------------------------------------------

#[test]
fn a_body_must_be_exactly_one_function_named_like_the_query() {
    let graph = modern();
    let cases = [
        (
            r#"#{name: "q", body: "fn q( { 1 }"}"#,
            "it does not compile",
        ),
        (
            r#"#{name: "q", body: "fn q() { 1 } fn r() { 2 }"}"#,
            "it defines 2 functions",
        ),
        (
            r#"#{name: "q", body: "fn other() { 1 }"}"#,
            "it defines the function 'other', but the query is named 'q'",
        ),
        (
            r#"#{name: "q", body: "let x = 1; fn q() { x }"}"#,
            "statements outside the function",
        ),
        (r#"#{name: "q", body: "1 + 1"}"#, "statements outside"),
        (
            r#"#{name: "q", body: "fn q(a) { a }"}"#,
            "'a' is not declared",
        ),
        (
            r#"#{name: "q", params: #{b: "integer"}, body: "fn q() { 1 }"}"#,
            "params declares 'b', which the function does not take",
        ),
    ];
    for (spec, expected) in cases {
        let message = err(&graph, &format!("g.define_query({spec})"));
        assert_contains(&message, "Invalid body of saved query 'q'");
        assert_contains(&message, expected);
        assert_contains(
            &message,
            "Help: The body of a saved query is the text of ONE",
        );
    }
    assert_eq!(query_count(&graph), 0);
}

#[test]
fn closures_are_part_of_the_one_function() {
    // Issue #16: Rhai compiles every closure into a function of its own (`anon$..`); it is
    // part of the query's function, not a second one.
    let graph = modern();
    let _ = ok(
        &graph,
        r#"g.define_query(#{name: "long_names", params: #{min: "integer"},
            body: `/// Names longer than min, upper case.
                   fn long_names(min) {
                       g.v().values("name").to_list().filter(|n| n.len() > min).map(|n| n.to_upper())
                   }`})"#,
    );
    assert_eq!(
        text(ok(&graph, r#"g.query("long_names", #{min: 4})"#)),
        r#"["MARKO", "VADAS", "RIPPLE", "PETER"]"#
    );
    // The doc comment is the named function's, not a closure's.
    assert_eq!(
        text(ok(&graph, r#"g.get_query("long_names")["description"]"#)),
        "Names longer than min, upper case."
    );
    // A closure never becomes a query of its own in define_queries.
    let names = ok(
        &graph,
        r#"g.define_queries("f", `fn a() { [1, 2, 3].filter(|i| i > 1) }
                                   fn b() { [1].map(|i| i * 2) }`)"#,
    );
    assert_eq!(text(names), r#"["a", "b"]"#);
    assert_eq!(text(ok(&graph, r#"g.query("a")"#)), "[2, 3]");
    // Two named functions are still refused, closures or not.
    let message = err(
        &graph,
        r#"g.define_query(#{name: "q", body: "fn q() { [1].map(|i| i) } fn r() { 2 }"})"#,
    );
    assert_contains(&message, "it defines 2 functions (");
    assert!(!message.contains("anon$"), "{message}");
}

#[test]
fn get_query_params_are_given_back_to_define_query() {
    // Issue #17: the list `get_query` returns is accepted as `params`, so get -> change ->
    // define round-trips.
    let graph = with_people();
    let before = text(ok(&graph, r#"g.get_query("people")["params"]"#));
    let _ = ok(
        &graph,
        r#"let q = g.get_query("people");
           let body = q["body"];
           body.replace("P.gt", "P.gte");
           g.define_query(#{name: "people", folder: q["folder"], params: q["params"], body: body})"#,
    );
    assert_eq!(
        text(ok(&graph, r#"g.get_query("people")["params"]"#)),
        before
    );
    assert_eq!(
        text(ok(
            &graph,
            r#"g.query("people", #{min: 32}).values("name").to_list()"#
        )),
        r#"["josh", "peter"]"#
    );
    // Without a default the parameter is required; define_queries takes the list too.
    let _ = ok(
        &graph,
        r#"g.define_queries("f", "fn twice(n) { n * 2 }",
               #{twice: [#{name: "n", schema: #{type: "integer"}, required: true}]})"#,
    );
    assert_eq!(text(ok(&graph, r#"g.query("twice", #{n: 4})"#)), "8");
    assert_contains(
        &err(&graph, r#"g.query("twice")"#),
        "needs the argument 'n' (int64), it has no default",
    );

    let refused = [
        (
            r#"[#{name: "n", schema: "integer", required: false}]"#,
            "the parameter 'n' with required: false but without a default",
        ),
        (
            r#"[#{name: "n", schema: "integer", default: 1, required: true}]"#,
            "the parameter 'n' with required: true and a default",
        ),
        (
            r#"[#{name: "n", schema: "integer"}, #{name: "n", schema: "integer"}]"#,
            "the parameter 'n' twice",
        ),
        (r#"[#{schema: "integer"}]"#, "a list item without a name"),
        (
            r#"[#{name: "n", kind: 1}]"#,
            "a list item with the key 'kind'",
        ),
        (r#"["n"]"#, "a list item of type string"),
    ];
    for (params, expected) in refused {
        let message = err(
            &graph,
            &format!(
                r#"g.define_query(#{{name: "q", params: {params}, body: "fn q(n) {{ n }}"}})"#
            ),
        );
        assert_contains(&message, "Argument mismatch for step 'define_query'");
        assert_contains(&message, expected);
        assert_contains(&message, "params may also be the list get_query returns");
    }
}

#[test]
fn the_catalog_rules_apply_to_a_dsl_definition() {
    let graph = modern();
    // A parameter default that violates its schema.
    let message = err(
        &graph,
        r#"g.define_query(#{name: "q", params: #{n: #{schema: #{type: "integer", minimum: 1}, default: 0}}, body: "fn q(n) { n }"})"#,
    );
    assert_contains(&message, "Invalid parameter 'n'");
    // A parameter named like a DSL global is refused by the parser already (it is a Rhai
    // function parameter), a reserved query name by the catalog.
    let message = err(
        &graph,
        r#"g.define_query(#{name: "q", folder: "/a", body: "fn q() { 1 }"})"#,
    );
    assert_contains(&message, "Invalid folder '/a'");
    let message = err(
        &graph,
        r#"g.define_query(#{name: "q", body: "fn q() { 1 }", colour: "red"})"#,
    );
    assert_contains(&message, "the key 'colour'");
    let message = err(
        &graph,
        r#"g.define_query(#{name: "loop", body: "fn q() { 1 }"})"#,
    );
    assert_contains(&message, "Invalid query name 'loop': it is a reserved word");
    let message = err(&graph, r#"g.define_query(#{body: "fn q() { 1 }"})"#);
    assert_contains(&message, "no name");
    assert_eq!(query_count(&graph), 0);
}

#[test]
fn the_short_parameter_form_declares_a_bare_type() {
    let graph = modern();
    let _ = ok(
        &graph,
        r#"g.define_query(#{name: "q", params: #{n: "integer", s: "string"}, body: "fn q(s, n) { s + n }"})"#,
    );
    assert_eq!(text(ok(&graph, r#"g.query("q", #{n: 1, s: "a"})"#)), "a1");
    // The parameters are listed in signature order, not in the (sorted) map order.
    assert_eq!(
        text(ok(&graph, r#"g.get_query("q")["params"].map(|p| p.name)"#)),
        r#"["s", "n"]"#
    );
}

// ---------------------------------------------------------------------------------------------
// Arguments
// ---------------------------------------------------------------------------------------------

#[test]
fn arguments_are_checked_against_the_parameters() {
    let graph = with_people();
    let unknown = err(&graph, r#"g.query("people", #{minimum: 3})"#);
    assert_contains(&unknown, "Saved query 'people' has no parameter 'minimum'");
    assert_contains(&unknown, "parameters of 'people': min");

    let wrong_type = err(&graph, r#"g.query("people", #{min: "30"})"#);
    assert_contains(
        &wrong_type,
        "Invalid argument 'min' for saved query 'people'",
    );
    assert_contains(&wrong_type, "expected int64, got string");

    let constraint = err(&graph, r#"g.query("people", #{min: 5000})"#);
    assert_contains(&constraint, "Invalid argument 'min'");
    assert_contains(&constraint, "200");

    let traversal = err(&graph, r#"g.query("people", #{min: g.v().count()})"#);
    assert_contains(&traversal, "is not a data value");

    let not_a_map = err(&graph, r#"g.query("people", 30)"#);
    assert_contains(&not_a_map, "named arguments as a map");
}

#[test]
fn a_required_parameter_must_be_passed() {
    let graph = modern();
    let _ = ok(
        &graph,
        r#"g.define_query(#{name: "q", params: #{n: "integer"}, body: "fn q(n) { n }"})"#,
    );
    let message = err(&graph, r#"g.query("q")"#);
    assert_contains(
        &message,
        "Saved query 'q' needs the argument 'n' (int64), it has no default",
    );
    assert_contains(&message, r#"g.query("q", #{n: ..})"#);
    // `()` is a value, not an omitted argument.
    let message = err(&graph, r#"g.query("q", #{n: ()})"#);
    assert_contains(&message, "Invalid argument 'n'");
}

// ---------------------------------------------------------------------------------------------
// Read-only
// ---------------------------------------------------------------------------------------------

#[test]
fn a_query_cannot_write_directly() {
    let graph = modern();
    store_read_only(&graph, "writer", r#"fn writer() { g.add_v("x").next() }"#);
    let message = err(&graph, r#"g.query("writer")"#);
    assert_contains(&message, "Permission denied for 'add_v'");
    assert_contains(&message, "a saved query is read-only");
    assert_contains(&message, r#"in saved query: "writer""#);
    assert_contains(&message, "Help: A read-only saved query");
    assert_eq!(graph.read().vertex_count(), 6);
}

#[test]
fn a_query_cannot_write_the_schema_or_the_catalog() {
    let graph = modern();
    store_read_only(
        &graph,
        "schema",
        "fn schema() { g.set_schema(g.infer_schema()) }",
    );
    store_read_only(
        &graph,
        "definer",
        r#"fn definer() { g.drop_query("schema") }"#,
    );
    assert_contains(
        &err(&graph, r#"g.query("schema")"#),
        "a saved query is read-only",
    );
    assert_contains(
        &err(&graph, r#"g.query("definer")"#),
        "a saved query is read-only",
    );
    assert!(graph.read().get_schema().is_none());
    assert_eq!(query_count(&graph), 2);
}

#[test]
fn a_nested_query_cannot_write_either() {
    let graph = modern();
    store_read_only(
        &graph,
        "inner",
        r#"fn inner() { g.v().property("k", 1).to_list() }"#,
    );
    let _ = ok(
        &graph,
        r#"g.define_query(#{name: "outer", body: "fn outer() { g.query(\"inner\") }"});"#,
    );
    let message = err(&graph, r#"g.query("outer")"#);
    assert_contains(&message, "a saved query is read-only");
    // One context line per call, innermost first.
    let inner = message
        .find(r#"in saved query: "inner""#)
        .expect("inner named");
    let outer = message
        .find(r#"in saved query: "outer""#)
        .expect("outer named");
    assert!(inner < outer, "{message}");
    assert_eq!(text(ok(&graph, r#"g.v().has("k").count().next()"#)), "0");
}

#[test]
fn a_returned_traversal_cannot_be_continued_with_a_writing_step() {
    let graph = modern();
    let _ = ok(
        &graph,
        r#"g.define_query(#{name: "people", body: "fn people() { g.v().has_label(\"person\") }"})"#,
    );
    let message = err(
        &graph,
        r#"g.query("people").property("seen", true).to_list()"#,
    );
    assert_contains(&message, "Permission denied for 'property'");
    assert_contains(&message, "a saved query is read-only");
    let message = err(&graph, r#"g.query("people").drop().to_list()"#);
    assert_contains(&message, "a saved query is read-only");
    assert_eq!(text(ok(&graph, r#"g.v().has("seen").count().next()"#)), "0");
    // The caller writes in a traversal of its own.
    let _ = ok(
        &graph,
        r#"let ids = g.query("people").id().to_list(); g.v(ids).property("seen", true).to_list()"#,
    );
    assert_eq!(text(ok(&graph, r#"g.v().has("seen").count().next()"#)), "4");
}

#[test]
fn nothing_built_from_a_query_result_is_mutating() {
    let graph = modern();
    let _ = ok(
        &graph,
        r#"g.define_query(#{name: "people", body: "fn people() { g.v().has_label(\"person\") }"})"#,
    );
    let continued =
        ok(&graph, r#"g.query("people").property("k", 1)"#).cast::<ScriptGraphTraversalSource>();
    assert!(!continued.is_mutating());
    let plain = ok(&graph, r#"g.v().property("k", 1)"#).cast::<ScriptGraphTraversalSource>();
    assert!(plain.is_mutating());
}

#[test]
fn a_query_cannot_return_a_writing_traversal() {
    let graph = modern();
    // Read-only queries (a query that writes runs a returned writing traversal of the graph,
    // `saved_query_effects_tests.rs`).
    store_read_only(
        &graph,
        "sneaky",
        r#"fn sneaky() { [1, g.v().property("k", 1)] }"#,
    );
    store_read_only(&graph, "anon", r#"fn anon() { __.add_v("x") }"#);
    let message = err(&graph, r#"g.query("sneaky")"#);
    assert_contains(
        &message,
        "Saved query 'sneaky' returned a traversal with a writing step (result[1])",
    );
    let message = err(
        &graph,
        r#"g.inject(1).side_effect(g.query("anon")).to_list()"#,
    );
    assert_contains(&message, "returned a traversal with a writing step");
    assert_eq!(graph.read().vertex_count(), 6);
}

#[test]
fn a_query_may_write_a_graph_it_builds_itself() {
    let graph = modern();
    let _ = ok(
        &graph,
        r#"g.define_query(#{name: "scratch", body: `fn scratch() {
            let s = GraphSource::empty();
            s.add_v("x").next();
            s.v().count().next()
        }`})"#,
    );
    assert_eq!(text(ok(&graph, r#"g.query("scratch")"#)), "1");
    assert_eq!(graph.read().vertex_count(), 6);
}

// ---------------------------------------------------------------------------------------------
// Position, depth, missing queries, errors
// ---------------------------------------------------------------------------------------------

#[test]
fn query_only_starts_a_traversal() {
    let graph = with_people();
    let message = err(&graph, r#"g.v().query("people")"#);
    assert_contains(
        &message,
        r#"query("people") can only start a traversal, it cannot follow other steps"#,
    );
    assert_contains(&message, r#"g.query("people")"#);
    let message = err(&graph, r#"g.v().where(__.query("people")).to_list()"#);
    assert_contains(&message, "it cannot follow __ (an anonymous traversal)");
}

#[test]
fn the_call_depth_is_bounded() {
    let graph = modern();
    let _ = ok(
        &graph,
        r#"g.define_query(#{name: "r", params: #{n: "integer"},
            body: "fn r(n) { if n >= 16 { n } else { g.query(\"r\", #{n: n + 1}) } }"})"#,
    );
    // Sixteen levels (n = 1 ..= 16) are allowed, a seventeenth is not. (They fit the 2 MiB
    // stack of a test thread in a debug build.)
    assert_eq!(text(ok(&graph, r#"g.query("r", #{n: 1})"#)), "16");
    let message = err(&graph, r#"g.query("r", #{n: 0})"#);
    assert_contains(
        &message,
        "Saved query calls nested deeper than 16 levels (calling 'r')",
    );
    assert_contains(
        &message,
        "Help: Saved queries may call each other at most 16",
    );
    // The repeated stack lines of the recursion are counted, not printed 16 times.
    assert_eq!(
        message.matches(r#"in saved query: "r""#).count(),
        1,
        "{message}"
    );
    assert_contains(&message, "(×");
}

#[test]
fn a_missing_query_fails_with_its_name() {
    let graph = modern();
    let message = err(&graph, r#"g.query("nope")"#);
    assert_contains(&message, "There is no query named 'nope'");
    assert_contains(&message, "g.queries()");
    // A missing callee fails at call time, located in the caller.
    let _ = ok(
        &graph,
        r#"g.define_query(#{name: "caller", body: "fn caller() { g.query(\"gone\") }"})"#,
    );
    let message = err(&graph, r#"g.query("caller")"#);
    assert_contains(&message, "There is no query named 'gone'");
    assert_contains(&message, r#"in saved query: "caller" (line 1, position"#);
}

#[test]
fn an_error_in_the_body_names_the_query() {
    let graph = modern();
    let _ = ok(
        &graph,
        r#"g.define_query(#{name: "broken", body: "fn broken() {\n  g.v().values(\"name\").as_number(GType.LONG).to_list()\n}"});
           g.define_query(#{name: "thrower", body: "fn thrower() { throw \"custom\" }"});
           g.define_query(#{name: "unknown_var", body: "fn unknown_var() { missing + 1 }"});"#,
    );
    let message = err(&graph, r#"g.query("broken")"#);
    assert_contains(&message, "as_number");
    assert_contains(&message, r#"in saved query: "broken" (line 2"#);
    let message = err(&graph, r#"g.query("thrower")"#);
    assert_contains(&message, "custom");
    assert_contains(&message, r#"in saved query: "thrower""#);
    let message = err(&graph, r#"g.query("unknown_var")"#);
    assert_contains(&message, "missing");
    assert_contains(&message, r#"in saved query: "unknown_var""#);
}

#[test]
fn a_stored_body_that_no_longer_compiles_is_listed_with_its_error() {
    let graph = modern();
    // Stored through the Rust API, which does not compile bodies.
    graph
        .write()
        .traversal_mut()
        .define_query(
            "stale",
            QueryDefinition::new("fn stale(x) { x + }")
                .with_param("x", QueryParam::from_value("x", &"integer".into()).unwrap()),
        )
        .unwrap();
    graph
        .write()
        .traversal_mut()
        .define_query("fine", QueryDefinition::new("fn fine() { 1 }"))
        .unwrap();
    let listed = ok(&graph, r#"g.queries()"#).cast::<rhai::Array>();
    let stale = listed[1].clone().cast::<rhai::Map>();
    assert_eq!(stale["name"].to_string(), "stale");
    assert_eq!(stale["status"].to_string(), "error");
    assert_contains(&stale["error"].to_string(), "it does not compile");
    let message = err(&graph, r#"g.query("stale", #{x: 1})"#);
    assert_contains(&message, "Invalid body of saved query 'stale'");
    let fine = listed
        .iter()
        .map(|entry| entry.clone().cast::<rhai::Map>())
        .find(|entry| entry["name"].to_string() == "fine")
        .expect("listed");
    assert_eq!(fine["status"].to_string(), "ok");
    assert!(!fine.contains_key("error"));
}

// ---------------------------------------------------------------------------------------------
// The compiled-body cache
// ---------------------------------------------------------------------------------------------

#[test]
fn a_rolled_back_redefinition_restores_the_old_body() {
    let graph = modern();
    let _ = ok(
        &graph,
        r#"g.define_query(#{name: "v", body: "fn v() { 1 }"})"#,
    );
    assert_eq!(text(ok(&graph, r#"g.query("v")"#)), "1");
    // A whole-script unit redefines the query, calls the new body, then fails: rolled back.
    let failed = eval_value_atomic(
        graph.clone(),
        r#"g.define_query(#{name: "v", body: "fn v() { 2 }"});
           if g.query("v") != 2 { throw "the new body did not run" }
           throw "roll back""#,
        Default::default(),
        &ScriptLimits::default(),
    );
    assert!(failed.unwrap_err().to_string().contains("roll back"));
    // The old body runs again (the cache is keyed by the stored text).
    assert_eq!(text(ok(&graph, r#"g.query("v")"#)), "1");
    // A transaction of the Rust API that is rolled back too.
    struct Abort;
    impl From<graphersal::error::GraphError> for Abort {
        fn from(_: graphersal::error::GraphError) -> Self {
            Abort
        }
    }
    let result: Result<(), Abort> = graph.write().transaction(|unit| {
        unit.traversal_mut()
            .define_query("v", QueryDefinition::new("fn v() { 3 }"))
            .map_err(|_| Abort)?;
        assert_eq!(unit.traversal().list_definitions(None).unwrap().len(), 1);
        Err(Abort)
    });
    assert!(result.is_err());
    assert_eq!(text(ok(&graph, r#"g.query("v")"#)), "1");
}

// ---------------------------------------------------------------------------------------------
// Management
// ---------------------------------------------------------------------------------------------

#[test]
fn queries_lists_by_name_and_filters_by_folder() {
    let graph = modern();
    let _ = ok(
        &graph,
        r#"g.define_query(#{name: "b", folder: "reports/sales", body: "fn b() { 1 }"});
           g.define_query(#{name: "a", folder: "reports", body: "fn a() { 1 }"});
           g.define_query(#{name: "c", folder: "reportsx", body: "fn c() { 1 }"});
           g.define_query(#{name: "d", body: "fn d() { 1 }"});"#,
    );
    let names = |script: &str| text(ok(&graph, &format!("{script}.map(|q| q.name)")));
    assert_eq!(names("g.queries()"), r#"["a", "b", "c", "d"]"#);
    assert_eq!(names(r#"g.queries("reports")"#), r#"["a", "b"]"#);
    assert_eq!(names(r#"g.queries("reports/")"#), r#"["a", "b"]"#);
    assert_eq!(names(r#"g.queries("reports/sales")"#), r#"["b"]"#);
    let entry = ok(&graph, r#"g.queries()[3]"#).cast::<rhai::Map>();
    assert!(entry["folder"].is_unit());
    assert!(entry["description"].is_unit());
    assert_eq!(entry["status"].to_string(), "ok");
    assert_eq!(entry["params"].to_string(), "[]");
    assert!(!entry.contains_key("body"));
}

#[test]
fn get_query_returns_the_full_definition() {
    let graph = with_people();
    let entry = ok(&graph, r#"g.get_query("people")"#).cast::<rhai::Map>();
    assert_eq!(entry["name"].to_string(), "people");
    assert_eq!(entry["folder"].to_string(), "reports/people");
    assert_eq!(entry["description"].to_string(), "Persons older than min.");
    assert_eq!(entry["dialect"].to_string(), "graphersal-rhai/1");
    assert_contains(&entry["body"].to_string(), "fn people(min)");
    let params = entry["params"].clone().cast::<rhai::Array>();
    let min = params[0].clone().cast::<rhai::Map>();
    assert_eq!(min["name"].to_string(), "min");
    assert_eq!(min["default"].as_int().unwrap(), 0);
    assert!(!min["required"].as_bool().unwrap());
    assert!(ok(&graph, r#"g.get_query("nope")"#).is_unit());
}

#[test]
fn drop_move_and_describe_change_the_catalog() {
    let graph = with_people();
    let _ = ok(
        &graph,
        r#"g.move_query("people", "archive"); g.describe_query("people", "Old.")"#,
    );
    let entry = ok(&graph, r#"g.get_query("people")"#).cast::<rhai::Map>();
    assert_eq!(entry["folder"].to_string(), "archive");
    assert_eq!(entry["description"].to_string(), "Old.");
    // Moving does not break a caller: the name stays.
    assert_eq!(text(ok(&graph, r#"g.query("people").count().next()"#)), "4");
    let _ = ok(
        &graph,
        r#"g.move_query("people", ()); g.describe_query("people", "")"#,
    );
    let entry = ok(&graph, r#"g.get_query("people")"#).cast::<rhai::Map>();
    assert!(entry["folder"].is_unit());
    assert!(entry["description"].is_unit());
    assert_contains(
        &err(&graph, r#"g.move_query("nope", "x")"#),
        "There is no query named 'nope'",
    );
    assert_eq!(text(ok(&graph, r#"g.drop_query("people")"#)), "true");
    assert_eq!(text(ok(&graph, r#"g.drop_query("people")"#)), "false");
    assert_eq!(query_count(&graph), 0);
}

#[test]
fn catalog_changes_are_reported_by_execute() {
    let graph = modern();
    let changes = ok(&graph, r#"g.v().execute().changes"#).cast::<rhai::Map>();
    assert!(!changes["definitions"].as_bool().unwrap());
    assert!(!changes["data"].as_bool().unwrap());
}

// ---------------------------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------------------------

/// A query defined through the DSL on a Store survives a reopen (the journal records it) and a
/// rolled-back whole-script definition leaves nothing in it.
#[cfg(feature = "persist")]
#[test]
fn a_query_defined_on_a_store_survives_a_reopen() {
    use graphersal::persist::{MemDir, Store, StoreDir, StoreOptions};

    let dir: Arc<dyn StoreDir> = Arc::new(MemDir::named("saved-query-store"));
    Store::create(&dir, StoreOptions::new())
        .unwrap()
        .close()
        .unwrap();
    {
        let store = Store::open(&dir, StoreOptions::new()).unwrap();
        let graph = store.graph().clone();
        let _ = ok(
            &graph,
            r#"g.define_query(#{name: "answer", params: #{x: #{schema: #{type: "integer"}, default: 41}},
                body: "fn answer(x) { x + 1 }"})"#,
        );
        let failed = eval_value_atomic(
            graph.clone(),
            r#"g.define_query(#{name: "gone", body: "fn gone() { 1 }"}); throw "roll back""#,
            Default::default(),
            &ScriptLimits::default(),
        );
        assert!(failed.is_err());
        store.close().unwrap();
    }
    let store = Store::open(&dir, StoreOptions::new()).unwrap();
    let graph = store.graph().clone();
    assert_eq!(text(ok(&graph, r#"g.query("answer")"#)), "42");
    assert_eq!(
        text(ok(&graph, r#"g.queries().map(|q| q.name)"#)),
        r#"["answer"]"#
    );
    store.close().unwrap();
}

// ---------------------------------------------------------------------------------------------
// Saved queries written in GQL (`language: "gql"`, book "Saved queries in GQL")
// ---------------------------------------------------------------------------------------------

#[cfg(feature = "gql")]
mod gql {
    use super::*;
    use graphersal::catalog::{GQL_QUERY_DIALECT, GQL_WRITING_QUERY_DIALECT, QUERY_DIALECT};

    fn field(graph: &Arc<Graph>, name: &str, key: &str) -> String {
        text(ok(graph, &format!(r#"g.get_query("{name}")["{key}"]"#)))
    }

    fn effects(graph: &Arc<Graph>, name: &str) -> String {
        text(ok(
            graph,
            &format!(r#"g.get_query("{name}")["effects"].map(|e| e.text)"#),
        ))
    }

    /// The modern graph with `older(min = 30)` in GQL and `add(name)`, a GQL insert returning
    /// what it created.
    fn with_gql() -> Arc<Graph> {
        let graph = modern();
        let _ = ok(
            &graph,
            r#"g.define_query(#{name: "older", language: "gql", folder: "reports",
                   description: "Persons older than min.",
                   params: #{min: #{schema: #{type: "integer"}, default: 30}},
                   body: "MATCH (p:person) WHERE p.age > $min RETURN p.name AS name ORDER BY name"});
               g.define_query(#{name: "add", language: "gql", params: #{name: "string"},
                   body: "INSERT (p:person {name: $name}) RETURN p.name AS name"})"#,
        );
        graph
    }

    #[test]
    fn a_gql_query_is_stored_as_written_with_its_language_and_dialect() {
        let graph = with_gql();
        assert_eq!(
            field(&graph, "older", "body"),
            "MATCH (p:person) WHERE p.age > $min RETURN p.name AS name ORDER BY name"
        );
        assert_eq!(field(&graph, "older", "language"), "gql");
        assert_eq!(field(&graph, "older", "dialect"), GQL_QUERY_DIALECT);
        assert_eq!(field(&graph, "older", "writes"), "false");
        assert_eq!(field(&graph, "older", "folder"), "reports");
        assert_eq!(field(&graph, "add", "dialect"), GQL_WRITING_QUERY_DIALECT);
        assert_eq!(field(&graph, "add", "writes"), "true");
        // The listing shows the language and the dialect too, Rhai queries as "rhai".
        let _ = ok(
            &graph,
            r#"g.define_query(#{name: "n", body: "fn n() { 1 }"})"#,
        );
        assert_eq!(
            text(ok(
                &graph,
                r#"g.queries().map(|q| q.name + ":" + q.language + ":" + q.dialect)"#
            )),
            format!(
                r#"["add:gql:{GQL_WRITING_QUERY_DIALECT}", "n:rhai:{QUERY_DIALECT}", "older:gql:{GQL_QUERY_DIALECT}"]"#
            )
        );
        // An explicit "rhai" is the default.
        let _ = ok(
            &graph,
            r#"g.define_query(#{name: "m", language: "rhai", body: "fn m() { 2 }"})"#,
        );
        assert_eq!(field(&graph, "m", "language"), "rhai");
        // The Rust API reads the same.
        let definition = graph
            .read()
            .traversal()
            .get_definition(DefinitionKind::QUERY, "older")
            .unwrap()
            .unwrap();
        let query = definition.as_query().unwrap();
        assert_eq!(query.language(), "gql");
        assert_eq!(query.dialect(), GQL_QUERY_DIALECT);
    }

    #[test]
    fn a_call_returns_rows_like_gql_and_gremlin_may_continue() {
        let graph = with_gql();
        assert_eq!(
            text(ok(&graph, r#"g.query("older").to_list()"#)),
            r#"[#{"name": "josh"}, #{"name": "peter"}]"#
        );
        assert_eq!(
            text(ok(
                &graph,
                r#"g.query("older", #{min: 31}).select("name").to_list()"#
            )),
            r#"["josh", "peter"]"#
        );
        // Equal to the same g.gql() call.
        assert_eq!(
            text(ok(&graph, r#"g.query("older", #{min: 26}).to_list()"#)),
            text(ok(
                &graph,
                r#"g.gql("MATCH (p:person) WHERE p.age > $min RETURN p.name AS name ORDER BY name", #{min: 26}).to_list()"#
            ))
        );
        // A parameter the plan needs (LIMIT $k).
        let _ = ok(
            &graph,
            r#"g.define_query(#{name: "top", language: "gql", params: #{k: "integer"},
                   body: "MATCH (p:person) RETURN p.name AS n ORDER BY n LIMIT $k"})"#,
        );
        assert_eq!(
            text(ok(
                &graph,
                r#"g.query("top", #{k: 2}).select("n").to_list()"#
            )),
            r#"["josh", "marko"]"#
        );
        assert_eq!(field(&graph, "top", "writes"), "false");
    }

    #[test]
    fn parameters_are_stored_in_the_order_the_query_reads_them_and_checked_at_a_call() {
        let graph = modern();
        let _ = ok(
            &graph,
            r#"g.define_query(#{name: "between", language: "gql",
                   params: #{a: "integer", b: "integer"},
                   body: "MATCH (p:person) WHERE p.age > $b AND p.age < $a RETURN p.name AS n"})"#,
        );
        assert_eq!(
            text(ok(
                &graph,
                r#"g.get_query("between")["params"].map(|p| p.name)"#
            )),
            r#"["b", "a"]"#
        );
        assert_eq!(
            text(ok(
                &graph,
                r#"g.query("between", #{a: 33, b: 28}).select("n").to_list()"#
            )),
            r#"["marko", "josh"]"#
        );
        // The arguments are checked like those of a Rhai query.
        assert_contains(
            &err(&graph, r#"g.query("between", #{a: 1})"#),
            "Saved query 'between' needs the argument 'b' (int64), it has no default",
        );
        assert_contains(
            &err(&graph, r#"g.query("between", #{a: 1, b: 2, c: 3})"#),
            "Saved query 'between' has no parameter 'c'",
        );
        assert_contains(
            &err(&graph, r#"g.query("between", #{a: "x", b: 2})"#),
            "Invalid argument 'a' for saved query 'between'",
        );
        assert_contains(
            &err(&graph, r#"g.v().query("between", #{a: 1, b: 2})"#),
            "can only start a traversal",
        );
    }

    #[test]
    fn a_gql_insert_writes_in_one_unit_and_returns_a_reading_traversal() {
        let graph = with_gql();
        assert_eq!(
            effects(&graph, "add"),
            r#"["Create Data(vertex \"person\")"]"#
        );
        assert_eq!(
            text(ok(
                &graph,
                r#"g.query("add", #{name: "zed"}).select("name").to_list()"#
            )),
            r#"["zed"]"#
        );
        assert_eq!(
            text(ok(&graph, r#"g.v().has("name", "zed").count().next()"#)),
            "1"
        );
        // Effects are exact per label, edges included.
        let _ = ok(
            &graph,
            r#"g.define_query(#{name: "adopt", language: "gql", params: #{owner: "string"},
                   body: "MATCH (p:person {name: $owner}) INSERT (p)-[:OWNS]->(:Pet&Cat {owner: $owner})"})"#,
        );
        assert_eq!(
            effects(&graph, "adopt"),
            r#"["Create Data(vertex \"Pet\")", "Create Data(vertex \"Cat\")", "Create Data(edge \"OWNS\")"]"#
        );
        let _ = ok(&graph, r#"g.query("adopt", #{owner: "marko"})"#);
        assert_eq!(
            text(ok(
                &graph,
                r#"g.v().has_label("Cat").in("OWNS").values("name").to_list()"#
            )),
            r#"["marko"]"#
        );
        // A failing row rolls back everything the call wrote.
        let _ = ok(
            &graph,
            r#"g.define_query(#{name: "many", language: "gql", params: #{xs: #{schema: #{type: "array"}}},
                   body: "FOR x IN $xs INSERT (:B {v: 1 / x})"})"#,
        );
        let failed = err(&graph, r#"g.query("many", #{xs: [1, 2, 0]})"#);
        assert_contains(&failed, "division by zero");
        assert_contains(&failed, "in saved query: \"many\"");
        assert_eq!(
            text(ok(&graph, r#"g.v().has_label("B").count().next()"#)),
            "0"
        );
        let _ = ok(&graph, r#"g.query("many", #{xs: [1, 2]})"#);
        assert_eq!(
            text(ok(&graph, r#"g.v().has_label("B").count().next()"#)),
            "2"
        );
    }

    #[test]
    fn a_read_only_gql_query_cannot_write_through_its_result() {
        let graph = with_gql();
        let failed = err(
            &graph,
            r#"g.query("older").select("name").drop().iterate()"#,
        );
        assert_contains(&failed, "a saved query is read-only");
        assert_eq!(graph.read().vertex_count(), 6);
        // A GQL insert stored with the read-only dialect (Rust API, which keeps the dialect):
        // its writes are denied before anything is written.
        graph
            .write()
            .traversal_mut()
            .define_query(
                "sneak",
                QueryDefinition::new("INSERT (:Sneak)").with_dialect(GQL_QUERY_DIALECT),
            )
            .unwrap();
        assert_eq!(field(&graph, "sneak", "writes"), "false");
        assert_eq!(
            text(ok(
                &graph,
                r#"g.get_query("sneak")["denied_writes"].map(|e| e.text)"#
            )),
            r#"["Create Data(vertex \"Sneak\")"]"#
        );
        let failed = err(&graph, r#"g.query("sneak")"#);
        assert_contains(
            &failed,
            r#"Permission denied for 'gql:insert_node' (Create Data(vertex "Sneak"))"#,
        );
        assert_contains(&failed, "in saved query: \"sneak\"");
        assert_eq!(graph.read().vertex_count(), 6);
    }

    #[test]
    fn rhai_queries_calling_gql_queries_carry_their_effects() {
        let graph = with_gql();
        let _ = ok(
            &graph,
            r#"g.define_query(#{name: "outer", body: "fn outer() { g.query(\"add\", #{name: \"o\"}).to_list() }"})"#,
        );
        assert_eq!(field(&graph, "outer", "dialect"), "graphersal-rhai/2");
        assert_eq!(
            effects(&graph, "outer"),
            r#"["Create Data(vertex \"person\")"]"#
        );
        assert_eq!(
            text(ok(&graph, r#"g.query("outer")"#)),
            r#"[#{"name": "o"}]"#
        );
        // A read-only body calling it: denied, nothing written.
        graph
            .write()
            .traversal_mut()
            .define_query(
                "reader",
                QueryDefinition::new(r#"fn reader() { g.query("add", #{name: "r"}).to_list() }"#),
            )
            .unwrap();
        let failed = err(&graph, r#"g.query("reader")"#);
        assert_contains(&failed, "a saved query is read-only");
        assert_eq!(
            text(ok(&graph, r#"g.v().has("name", "r").count().next()"#)),
            "0"
        );
        // A read-only GQL query called from a Rhai body.
        let _ = ok(
            &graph,
            r#"g.define_query(#{name: "names", body: "fn names() { g.query(\"older\").select(\"name\").to_list() }"})"#,
        );
        assert_eq!(field(&graph, "names", "dialect"), QUERY_DIALECT);
        assert_eq!(
            text(ok(&graph, r#"g.query("names")"#)),
            r#"["josh", "peter"]"#
        );
    }

    #[test]
    fn an_invalid_body_is_refused_before_anything_is_stored() {
        let graph = modern();
        for (script, expected) in [
            (
                r#"g.define_query(#{name: "bad", language: "gql", body: "MATCH (p) RETRUN p"})"#,
                "unexpected `RETRUN`",
            ),
            (
                r#"g.define_query(#{name: "bad", language: "gql", body: "MATCH (p) RETURN q"})"#,
                "GQL semantic error",
            ),
            (
                r#"g.define_query(#{name: "bad", language: "gql", body: "MATCH (p) RETURN p.name AS n LIMIT $k"})"#,
                "reads the parameter $k, which its params do not declare",
            ),
            (
                r#"g.define_query(#{name: "bad", language: "gql", params: #{k: "integer"}, body: "MATCH (p) RETURN p"})"#,
                "declares the parameter 'k', which its GQL query never reads",
            ),
            (
                r#"g.define_query(#{name: "bad", language: "gql", body: "fn bad() { g.v() }"})"#,
                "GQL syntax error",
            ),
            (
                r#"g.define_query(#{name: "bad", body: "MATCH (p) RETURN p"})"#,
                "Invalid body of saved query 'bad': it does not compile",
            ),
            (
                r#"g.define_query(#{name: "bad", language: "cypher", body: "MATCH (p) RETURN p"})"#,
                "got the language 'cypher'",
            ),
            (
                r#"g.define_query(#{name: "bad", language: "gql"})"#,
                "the key body (the text of one GQL query)",
            ),
        ] {
            assert_contains(&err(&graph, script), expected);
        }
        assert_eq!(text(ok(&graph, "g.queries().len()")), "0");
        // define_queries stays Rhai: a GQL text does not compile there.
        assert_contains(
            &err(&graph, r#"g.define_queries("", "MATCH (p) RETURN p")"#),
            "the source does not compile",
        );
    }

    #[test]
    fn redefining_replaces_the_language_and_the_cached_form() {
        let graph = with_gql();
        assert_eq!(text(ok(&graph, r#"g.query("older").count().next()"#)), "2");
        // The same name in Rhai, then in GQL again: every call sees the latest definition.
        let _ = ok(
            &graph,
            r#"g.define_query(#{name: "older", body: "fn older() { g.v().has_label(\"person\") }"})"#,
        );
        assert_eq!(field(&graph, "older", "language"), "rhai");
        assert_eq!(text(ok(&graph, r#"g.query("older").count().next()"#)), "4");
        let _ = ok(
            &graph,
            r#"g.define_query(#{name: "older", language: "gql", body: "MATCH (s:software) RETURN s"})"#,
        );
        assert_eq!(text(ok(&graph, r#"g.query("older").count().next()"#)), "2");
        // A rollback restores the earlier body, and the call follows it.
        let failed = eval_value_atomic(
            graph.clone(),
            r#"g.define_query(#{name: "older", language: "gql", body: "MATCH (p) RETURN p"});
               if g.query("older").count().next() != 6 { throw "wrong" }
               throw "roll back""#,
            Default::default(),
            &ScriptLimits::default(),
        )
        .unwrap_err()
        .to_string();
        assert_contains(&failed, "roll back");
        assert_eq!(text(ok(&graph, r#"g.query("older").count().next()"#)), "2");
    }

    #[test]
    fn the_rust_api_keeps_the_gql_language_when_it_infers_the_dialect() {
        let graph = modern();
        let engine = graphersal::script::engine();
        let catalog = |_: &str| None;
        let inferred = graphersal::script::with_inferred_dialect(
            &engine,
            &catalog,
            "add",
            QueryDefinition::new("INSERT (:person {name: $n})")
                .with_dialect(GQL_QUERY_DIALECT)
                .with_param("n", QueryParam::from_value("n", &"string".into()).unwrap()),
        )
        .unwrap();
        assert_eq!(inferred.dialect(), GQL_WRITING_QUERY_DIALECT);
        let effects =
            graphersal::script::saved_query_effects(&engine, &catalog, "add", &inferred).unwrap();
        assert!(effects.writes);
        assert_eq!(effects.effects.len(), 1);
        // A read-only body back to the read-only dialect.
        let read = graphersal::script::with_inferred_dialect(
            &engine,
            &catalog,
            "all",
            QueryDefinition::new("MATCH (n) RETURN n").with_dialect(GQL_WRITING_QUERY_DIALECT),
        )
        .unwrap();
        assert_eq!(read.dialect(), GQL_QUERY_DIALECT);
        // Errors are reasons.
        let refused = graphersal::script::with_inferred_dialect(
            &engine,
            &catalog,
            "bad",
            QueryDefinition::new("MATCH (n) RETURN $x").with_dialect(GQL_QUERY_DIALECT),
        )
        .unwrap_err();
        assert_contains(&refused, "it reads the parameter $x");
        drop(graph);
    }

    /// A GQL query defined on a Store survives the journal, a checkpoint, a fork and a backup
    /// unchanged, and runs after each.
    #[cfg(feature = "persist")]
    #[test]
    fn a_gql_query_survives_every_store_path() {
        use graphersal::persist::{MemDir, RecoveryTarget, Store, StoreDir, StoreOptions};

        let definition = |graph: &Arc<Graph>| {
            graph
                .read()
                .traversal()
                .get_definition(DefinitionKind::QUERY, "add")
                .unwrap()
                .unwrap()
        };
        let dir: Arc<dyn StoreDir> = Arc::new(MemDir::named("gql-saved-query"));
        let store = Store::create(&dir, StoreOptions::new()).unwrap();
        let _ = ok(
            store.graph(),
            r#"g.define_query(#{name: "add", language: "gql", params: #{name: "string"},
                   body: "INSERT (p:person {name: $name}) RETURN p.name AS name"})"#,
        );
        let expected = definition(store.graph());
        assert_eq!(
            expected.as_query().unwrap().dialect(),
            GQL_WRITING_QUERY_DIALECT
        );
        store.close().unwrap();
        let check = |graph: &Arc<Graph>, name: &str| {
            assert_eq!(definition(graph), expected);
            assert_eq!(
                text(ok(
                    graph,
                    &format!(r#"g.query("add", #{{name: "{name}"}}).select("name").to_list()"#)
                )),
                format!(r#"["{name}"]"#)
            );
        };
        // The journal.
        let store = Store::open(&dir, StoreOptions::new()).unwrap();
        check(store.graph(), "wal");
        // A checkpoint.
        store.checkpoint(None).unwrap();
        store.close().unwrap();
        let store = Store::open(&dir, StoreOptions::new()).unwrap();
        check(store.graph(), "snapshot");
        // A fork and a backup.
        let fork: Arc<dyn StoreDir> = Arc::new(MemDir::named("gql-saved-query-fork"));
        store.fork(RecoveryTarget::Latest, &fork).unwrap();
        let backup: Arc<dyn StoreDir> = Arc::new(MemDir::named("gql-saved-query-backup"));
        store.backup(&backup).unwrap();
        store.close().unwrap();
        let forked = Store::open(&fork, StoreOptions::new()).unwrap();
        check(forked.graph(), "fork");
        forked.close().unwrap();
        let opened = Store::open_backup(&backup, StoreOptions::new()).unwrap();
        assert_eq!(definition(opened.graph()), expected);
    }
}
