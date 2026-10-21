//! Integration tests for traversal diagnostics: step paths, physical plan rendering and the
//! provenance of the value that caused a failure.

use graphersal::auth::{AccessPolicy, Authorizer};
use graphersal::changes::{ChangeSet, CommitHook, HookError};
use graphersal::exec::ExecutionPolicy;
use graphersal::exec::Spelling;
use graphersal::prelude::PropertyMap;
use graphersal::prelude::*;
use graphersal::schema::ValueType;
use graphersal::schema::{PropertySchema, VertexSchema};

/// Builds the modern graph with a single `age` property stored as a string, reproducing the
/// common case of heterogeneous data reaching a numeric aggregation.
fn graph_with_string_age() -> Graph {
    let graph = GraphSource::tinkerpop_modern();
    {
        let mut writable = graph.write();
        let _ = writable
            .traversal_mut()
            .v("4")
            .property("age", "999")
            .next();
    }
    graph
}

#[test]
fn aggregation_failure_reports_step_path_plan_and_origin() {
    let graph = graph_with_string_age();
    let locked = graph.read();
    let err = locked
        .traversal()
        .v(None)
        .has_label("person")
        .values("age")
        .max()
        .next()
        .expect_err("a string age must not be aggregated");

    let report = format_diagnostic(&err);

    assert!(
        report.starts_with("Error: Step #2 'max()' execution failed"),
        "missing step path in: {report}"
    );
    assert!(
        report.contains(r#"at #2: v().has_label("person").values("age").max()"#),
        "the caret must be placed in the query as written in: {report}"
    );
    assert!(
        report.contains(r#"plan:  v(labels: ["person"]).values("age").max()"#),
        "missing the executed physical plan in: {report}"
    );
    assert!(report.contains('^'), "missing caret marker in: {report}");
    assert!(
        report.contains("Caused by: Step 'max' cannot order a string value among number values"),
        "missing root cause in: {report}"
    );
    assert!(
        report.contains(r#"value:  "999""#),
        "missing offending value in: {report}"
    );
    assert!(
        report.contains(r#"origin: vertex id="4" label="person" property="age""#),
        "missing value origin in: {report}"
    );
    assert!(
        report.contains(
            r#"Help: Property 'age' holds a string where 'max' can only order [number] values."#
        ),
        "missing actionable help in: {report}"
    );
    // Rule 14: the coercion steps resolve this failure, so the help has to name them with a
    // query the user can run.
    assert!(
        report.contains(
            r#"Convert the values in the query instead: values("age").as_number(GType.LONG)."#
        ),
        "the help must name the coercion step in: {report}"
    );
    // Rule 14: `property()` can now persist that conversion in place (step 5 of the logical type
    // system spec), so the help must offer the full read-transform-write fix, not just the read
    // side of it.
    assert!(
        report.contains(
            r#"Or fix the data in place: property("age", __.values("age").as_number(GType.LONG))."#
        ),
        "the help must name the property() fix-in-place in: {report}"
    );
    assert!(
        report
            .contains(r#"Find every offending value with values("age").is(typeOf(GType.STRING))."#),
        "the help must name typeOf() in: {report}"
    );
}

/// The same advice reaches a failure that carries no element provenance, such as one on an
/// injected value.
#[test]
fn a_cast_failure_without_provenance_still_names_the_coercion_steps() {
    let graph = GraphSource::tinkerpop_modern();
    let locked = graph.read();
    let err = locked
        .traversal()
        .inject(vec!["not a number"])
        .sum()
        .next()
        .expect_err("a string must not be aggregated");

    let report = format_diagnostic(&err);
    assert!(
        report.contains("Convert the stream first with .as_number(GType.LONG)."),
        "the help must name the coercion step in: {report}"
    );
    assert!(
        report.contains("Find every offending value with .is(typeOf(GType.STRING))."),
        "the help must name typeOf() in: {report}"
    );
}

/// A conversion target no conversion can produce fails loudly and says why.
#[test]
fn an_unsupported_cast_target_is_reported_with_the_supported_ones() {
    let graph = GraphSource::tinkerpop_modern();
    let locked = graph.read();
    let err = locked
        .traversal()
        .v(None)
        .values("age")
        .cast(GType::from(ValueType::Array(Box::new(ValueType::String))))
        .next()
        .expect_err("a container target is not a conversion");

    let report = format_diagnostic(&err);
    assert!(
        report.contains("Caused by: Unsupported cast target 'array<string>'")
            || report.contains("Error: Unsupported cast target 'array<string>'"),
        "the target must be named in: {report}"
    );
    assert!(
        report.contains(
            "'array<string>' cannot be produced by a conversion yet: a container is built by fold() or project(), not by a conversion."
        ),
        "the help must say why in: {report}"
    );
    assert!(
        report.contains(
            "The supported targets are as_string(), as_bool(), as_number(), as_number(GType.LONG) and as_number(GType.DOUBLE)."
        ),
        "the help must list the supported targets in: {report}"
    );
}

/// A UUID cast target is now implemented: a canonical string converts, a malformed one fails with
/// a `CastException` that names the coercion step (design decision D7).
#[test]
fn a_uuid_cast_target_converts_a_canonical_string_and_rejects_a_malformed_one() {
    let graph = GraphSource::tinkerpop_modern();
    let locked = graph.read();
    let ok = locked
        .traversal()
        .inject(vec!["550e8400-e29b-41d4-a716-446655440000"])
        .cast(GType::UUID)
        .next()
        .unwrap();
    assert!(ok.is_some());

    let err = locked
        .traversal()
        .inject(vec!["not-a-uuid"])
        .cast(GType::UUID)
        .next()
        .expect_err("a malformed UUID must not silently pass through");

    let report = format_diagnostic(&err);
    assert!(
        report.contains("uuid"),
        "the target type must be named in: {report}"
    );
}

/// The hint every `uuid` cast failure appends: the canonical form and a runnable literal.
const UUID_CANONICAL_HINT: &str = "A UUID is written in canonical form: 8-4-4-4-12 lowercase \
     hex digits, no braces, for example UUID.from_string(\"550e8400-e29b-41d4-a716-446655440000\").";

/// A malformed UUID literal: the help names the canonical format and a runnable
/// `UUID.fromString(..)` example (type system step 4).
#[test]
fn a_malformed_uuid_literal_help_names_the_canonical_form() {
    let err = ElementProperty::uuid_from_string("550E8400-E29B-41D4-A716-446655440000")
        .expect_err("uppercase is not canonical");
    let TraverserError::Value(value_error) = *err else {
        panic!("expected a value error");
    };
    assert_eq!(
        value_error.help().expect("help"),
        format!(
            "Check the value type before performing conversion or cast operation.\n\
             Convert the stream first with .cast(GType.UUID).\n\
             Find every offending value with .is(typeOf(GType.STRING)).\n\
             {UUID_CANONICAL_HINT}"
        )
    );
}

/// A stored string that fails `cast(GType::UUID)`: the property-origin help also ends with the
/// canonical-form hint.
#[test]
fn a_stored_non_uuid_string_cast_help_names_the_canonical_form() {
    let graph = GraphSource::tinkerpop_modern();
    graph
        .write()
        .traversal_mut()
        .v("1")
        .property("uid", "abc")
        .next()
        .unwrap();
    let locked = graph.read();
    let err = locked
        .traversal()
        .v("1")
        .values("uid")
        .cast(GType::UUID)
        .next()
        .expect_err("\"abc\" is not a UUID");
    let TraverserError::Value(value_error) = err.root_cause() else {
        panic!("expected a value error, got {err:?}");
    };
    let help = value_error.help().expect("help");
    assert!(
        help.starts_with("Property 'uid' holds a string on vertex"),
        "unexpected help: {help}"
    );
    assert!(
        help.ends_with(&format!("\n{UUID_CANONICAL_HINT}")),
        "unexpected help: {help}"
    );
}

#[test]
fn an_unoptimized_query_is_reported_once() {
    let graph = graph_with_string_age();
    let locked = graph.read();
    let err = locked
        .traversal()
        .v(None)
        .values("age")
        .max()
        .next()
        .expect_err("a string age must not be aggregated");

    let report = format_diagnostic(&err);
    assert_eq!(
        err.query(),
        None,
        "an untouched plan needs no separate copy of the query"
    );
    assert!(
        !report.contains("plan:"),
        "an unchanged plan must not be printed twice in: {report}"
    );
    assert!(
        report.contains(r#"at #2: v().values("age").max()"#),
        "the caret must be placed in the query as written in: {report}"
    );
}

#[test]
fn nested_traversal_failure_reports_a_step_path_per_level() {
    let graph = graph_with_string_age();
    let locked = graph.read();
    let err = locked
        .traversal()
        .v(None)
        .has_label("person")
        .where_t(__::values("age").sum().is(P::Gt(30)))
        .count()
        .next()
        .expect_err("a string age must not be aggregated inside where()");

    let report = format_diagnostic(&err);

    assert!(
        report.starts_with("Error: Step #1.0.1 'sum()' execution failed"),
        "missing nested step path in: {report}"
    );
    assert_eq!(
        err.step_trace().len(),
        2,
        "expected one frame per traversal level in: {report}"
    );
    assert!(
        report.contains("at #1:") && report.contains("at #1.0.1:"),
        "missing one labelled frame per level in: {report}"
    );
    assert!(
        report.contains(r#"values("age").sum().is(P.gt(30))"#),
        "missing inner plan in: {report}"
    );
}

#[test]
fn incomparable_kinds_are_reported_with_provenance() {
    let graph = GraphSource::tinkerpop_modern();
    {
        let mut writable = graph.write();
        let _ = writable
            .traversal_mut()
            .v("1")
            .property("score", 5i64)
            .next();
        let _ = writable
            .traversal_mut()
            .v("2")
            .property("score", "high")
            .next();
    }
    let locked = graph.read();
    let err = locked
        .traversal()
        .v(None)
        .values("score")
        .max()
        .next()
        .expect_err("a number and a string are not comparable by max()");

    let report = format_diagnostic(&err);
    assert!(
        report.contains("origin: vertex id=\"2\""),
        "missing origin of the offending value in: {report}"
    );
    assert!(
        report.contains("max() compares numbers with numbers and text with text"),
        "the help must explain the like-with-like rule in: {report}"
    );
    assert!(
        report.contains(r#"g.v().values("name").max()"#)
            && report.contains("Step 'max' cannot order a string value among number"),
        "the help must carry a runnable example in: {report}"
    );
    // The coercion step that resolves the failure is named, too.
    assert!(report.contains("as_number(GType.LONG)"), "{report}");
}

/// TinkerPop 3.7's `max()` (and `sum()`/`mean()`/`min()`/`median()`) is a seedless
/// `ReducingBarrierStep`: an empty, or all-null, numeric stream emits nothing at all rather than
/// failing the query — confirmed against TinkerPop 3.7's `MaxGlobalStep` sources and via `graphersal`
/// (`g.V().hasLabel("nope").values("age").max()` produces no rows, not an error).
#[test]
fn empty_numeric_aggregation_emits_nothing_not_an_error() {
    let graph = GraphSource::tinkerpop_modern();
    {
        let mut writable = graph.write();
        let _ = writable
            .traversal_mut()
            .v("1")
            .property("score", ElementProperty::Null)
            .next();
    }
    let locked = graph.read();
    let result = locked
        .traversal()
        .v(None)
        .values("score")
        .max()
        .next()
        .expect("a stream of null values is not an aggregation error");

    assert!(
        result.is_none(),
        "a stream of null values carries nothing to aggregate, so max() must emit no result, got {result:?}"
    );
}

#[test]
fn integer_overflow_is_reported_instead_of_panicking() {
    let graph = GraphSource::tinkerpop_modern();
    {
        let mut writable = graph.write();
        let _ = writable
            .traversal_mut()
            .v("1")
            .property("score", i64::MAX)
            .next();
        let _ = writable
            .traversal_mut()
            .v("2")
            .property("score", 1i64)
            .next();
    }
    let locked = graph.read();
    let err = locked
        .traversal()
        .v(None)
        .values("score")
        .sum()
        .next()
        .expect_err("an i64 overflow must be reported as an error");

    let report = format_diagnostic(&err);
    assert!(
        report.contains("'sum' overflowed the signed 64-bit integer range"),
        "missing overflow diagnosis in: {report}"
    );
}

/// The Rhai DSL surfaces the same diagnostic the Rust API produces: a data terminal such as
/// `toList()` fails the script with the full `format_diagnostic` report attached, so a broken
/// query is never mistaken for a result.
#[cfg(feature = "script")]
#[test]
fn script_dsl_reports_the_same_diagnostic() {
    use crate::script_host::eval;

    let graph = std::sync::Arc::new(graph_with_string_age());
    let err = eval(
        graph,
        r#"g.v().has_label("person").values("age").max().toList()"#,
    )
    .expect_err("a failing traversal must fail the script");
    let report = err.to_string();

    assert!(
        report.contains("Step #2 'max()' execution failed"),
        "missing step path in: {report}"
    );
    assert!(
        report.contains(r#"origin: vertex id="4" label="person" property="age""#),
        "missing value origin in: {report}"
    );
}

/// A display terminal keeps rendering the diagnostic instead of failing, so the REPL prints the
/// report in place of the result table.
#[cfg(feature = "script")]
#[test]
fn script_dsl_renders_the_diagnostic_for_display_terminals() {
    use crate::script_host::eval;

    let graph = std::sync::Arc::new(graph_with_string_age());
    let report = eval(graph, r#"g.v().has_label("person").values("age").max()"#)
        .expect("a rendered traversal must not fail the script");

    assert!(
        report.contains("Step #2 'max()' execution failed"),
        "missing step path in: {report}"
    );
}

#[test]
fn diagnostics_can_be_turned_off_for_hot_paths() {
    let graph = graph_with_string_age();
    let locked = graph.read();
    let err = locked
        .traversal()
        .without_diagnostics()
        .v(None)
        .has_label("person")
        .values("age")
        .max()
        .next()
        .expect_err("a string age must not be aggregated");

    let report = format_diagnostic(&err);
    assert_eq!(
        err.query(),
        None,
        "provenance must not be tracked once diagnostics are off"
    );
    assert!(
        report.contains(r#"at #2: v(labels: ["person"]).values("age").max()"#),
        "the executed plan is still reported in: {report}"
    );
    assert!(
        report.contains(r#"origin: vertex id="4" label="person" property="age""#),
        "the offending value is still reported in: {report}"
    );
}

/// `property_json()` rejects a JSON integer that overflows `i64` (but fits `u64`) with a
/// `CastException` instead of silently downcasting to a lossy `f64` (design decision D3); its
/// `help()` text is covered like any other diagnostic (AGENTS.md rule 14).
#[cfg(feature = "serde")]
#[test]
fn property_json_u64_overflow_help_names_the_cast_exception() {
    let graph = GraphSource::tinkerpop_modern();
    let mut writable = graph.write();
    let err = writable
        .traversal_mut()
        .v("1")
        .property_json(serde_json::json!({"n": 18446744073709551615u64}))
        .next()
        .expect_err("a u64 beyond i64::MAX must not silently downcast to f64");

    let report = format_diagnostic(&err);
    assert!(
        report.contains("Caused by: Cast exception: expected type [int64 or float64], got type 'u64 (18446744073709551615) exceeding i64::MAX'"),
        "missing root cause in: {report}"
    );
    assert!(
        report
            .contains("Help: Check the value type before performing conversion or cast operation."),
        "missing the generic no-origin help in: {report}"
    );
}

/// A `CastException` whose offending value is a whole nested object (not a plain type mismatch)
/// must point at `.json_path(...)` as the direct way to reach the one field the query actually
/// wanted, instead of chaining `.values(property).values(subkey)` (AGENTS.md rule 14).
#[cfg(feature = "serde")]
#[test]
fn cast_exception_on_nested_object_help_names_json_path() {
    let graph = GraphSource::tinkerpop_modern();
    let mut writable = graph.write();
    let err = writable
        .traversal_mut()
        .v("1")
        .property_json(serde_json::json!({"meta": {"k": 1}}))
        .values("meta")
        .as_number(GType::LONG)
        .next()
        .expect_err("a nested object cannot be cast straight to a number");

    let report = format_diagnostic(&err);
    assert!(
        report.contains("Caused by: Cast exception: expected type [int64], got type 'object'"),
        "missing root cause in: {report}"
    );
    assert!(
        report.contains(
            "Reach one nested field directly instead of chaining values(): \
             json_path(\"meta.subkey\") instead of values(\"meta\").values(\"subkey\")."
        ),
        "the help must name json_path() as the direct route to a nested field:\n{report}"
    );
}

/// A `CastException` whose offending value is a plain scalar (no nested container involved) must
/// stay free of the `json_path()` mention: it would not fix a genuine type mismatch, only a
/// nested-value miss.
#[test]
fn cast_exception_on_scalar_help_omits_json_path() {
    let graph = graph_with_string_age();
    let locked = graph.read();
    let err = locked
        .traversal()
        .v(None)
        .has_label("person")
        .values("age")
        .max()
        .next()
        .expect_err("a string age must not be aggregated");

    let report = format_diagnostic(&err);
    assert!(
        !report.contains("json_path"),
        "a plain scalar cast mismatch has nothing to do with JSONPath:\n{report}"
    );
}

/// `InvalidJsonPath`'s `help()` must give a runnable example using the finished API surface —
/// both the standalone `.json_path(...)` step and the `.by(By::JsonPath(...))` modulator form —
/// and name the unsupported JSONPath constructs concretely (AGENTS.md rule 14).
#[cfg(feature = "serde")]
#[test]
fn invalid_json_path_help_names_both_api_forms_and_unsupported_syntax() {
    let graph = GraphSource::tinkerpop_modern();
    let mut writable = graph.write();
    let err = writable
        .traversal_mut()
        .v("1")
        .property_json(serde_json::json!({"meta": {"k": 1}}))
        .json_path("$..bad")
        .next()
        .expect_err("recursive descent is not part of the supported JSONPath subset");

    let report = format_diagnostic(&err);
    assert!(
        report.contains("Caused by: Invalid JSONPath '$..bad'"),
        "missing root cause in: {report}"
    );
    assert!(
        report.contains(r#"json_path("$.meta.tags[0]")"#),
        "the help must carry a runnable .json_path() example:\n{report}"
    );
    assert!(
        report.contains(r#".by(By::JsonPath("meta.k"))"#),
        "the help must carry a runnable By::JsonPath example:\n{report}"
    );
    assert!(
        report.contains("Recursive descent ('..')")
            && report.contains("wildcards ('*')")
            && report.contains("filter expressions ('[?(...)]')"),
        "the help must name the unsupported constructs concretely:\n{report}"
    );
}

#[cfg(feature = "serde")]
#[test]
fn json_parse_help_does_not_promise_a_graph_json_format() {
    let err = IOError::JsonParse(serde_json::from_str::<serde_json::Value>("{").unwrap_err());
    let rendered = format_diagnostic(&err);

    assert!(
        rendered.contains("Verify that the JSON document is well-formed."),
        "the help must stay about JSON well-formedness:\n{rendered}"
    );
}

#[test]
fn other_v_without_a_preceding_vertex_help_names_both_v_and_out_v() {
    let graph = GraphSource::tinkerpop_modern();
    let locked = graph.read();
    let err = locked
        .traversal()
        .e(None)
        .other_v()
        .next()
        .expect_err("g.e().other_v() has no vertex earlier in its path");

    let report = format_diagnostic(&err);
    assert!(
        report.contains("other_v() found no vertex earlier"),
        "{report}"
    );
    assert!(
        report.contains("g.v(\"1\").both_e(\"knows\").other_v()"),
        "{report}"
    );
    assert!(report.contains("both_v()"), "{report}");
    assert!(report.contains("out_v()/in_v()"), "{report}");
}

/// `select("x")` where "x" is never declared anywhere in the query (no `as("x")`, no
/// `aggregate`/`store`/`tree`/`with_side_effect` key "x") is a plan-time, before-execution error
/// (design spec `2026-09-26-gremlin-conformance-fixes-design.md`, task 1): this is a
/// query-authoring mistake, not the legitimate "declared but not on this branch" case that
/// `union()`/`optional()`/`choose()` produce (see `select_declared_but_absent_label_filters_the_traverser`
/// below, and `crates/graphersal/src/traversal/step/map_steps/step_select.rs`'s module tests for
/// the three branching queries verified against real TinkerPop 3.7/TinkerGraph).
#[test]
fn a_missing_select_label_help_names_as_and_path() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let err = lock
        .traversal()
        .v("1")
        .out(None)
        .select("x")
        .to_list()
        .unwrap_err();
    assert!(matches!(
        err.root_cause(),
        TraverserError::Value(ValueError::UndeclaredLabel { .. })
    ));
    let rendered = format_diagnostic(&err);
    assert!(
        rendered.contains("Step label 'x' is never declared anywhere in this query"),
        "{rendered}"
    );
    assert!(
        rendered.contains("g.v(\"1\").as(\"x\").out().select(\"x\")"),
        "{rendered}"
    );
    assert!(
        rendered.contains("read the whole path with path()"),
        "{rendered}"
    );
    assert!(
        rendered.contains("where(__.as(\"x\").values(\"name\"))"),
        "{rendered}"
    );
    // pop series: the help names the Pop forms for a label set several times
    assert!(rendered.contains("select(Pop.all, \"x\")"), "{rendered}");
    assert!(
        !rendered.contains("select_pop"),
        "DSL prose only: {rendered}"
    );
}

/// The undeclared-label diagnostic also guards the `Pop` forms of `select()` (the `path` help is
/// asserted in the test module of `error.rs`).
#[test]
fn pop_forms_get_the_undeclared_label_diagnostic() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let err = lock
        .traversal()
        .v(None)
        .select_pop(Pop::First, "x")
        .to_list()
        .unwrap_err();
    let rendered = format_diagnostic(&err);
    assert!(
        rendered.contains("Step label 'x' is never declared"),
        "{rendered}"
    );
    assert!(rendered.contains("Pop.first"), "{rendered}");
    assert!(rendered.contains("Pop.mixed"), "{rendered}");
}

/// A malformed `select(Pop, ...)` call is an `ArgumentMismatch` whose help shows the four forms.
#[test]
#[cfg(feature = "script")]
fn select_pop_argument_mismatch_help_shows_the_forms() {
    use crate::script_host::eval;

    let graph = std::sync::Arc::new(GraphSource::tinkerpop_modern());
    let err = eval(graph, r#"g.v().as("a").select(Pop.first)"#).expect_err("a Pop alone");
    let report = err.to_string();
    assert!(
        report.contains("Argument mismatch for step 'select'"),
        "{report}"
    );
    for form in [
        "select(\"a\", \"b\")",
        "select(Pop.all, \"a\")",
        "select(Pop.first, \"a\", \"b\")",
        "select(Column.keys)",
        "g.v().as(\"a\").out().as(\"a\").select(Pop.all, \"a\")",
        "select(Pop.all, __.constant(\"a\"))",
    ] {
        assert!(report.contains(form), "missing {form}: {report}");
    }
}

/// A token `select()` that names no map entry (`T.key`) or is mixed with labels is an
/// `ArgumentMismatch` whose help shows `select(T.label)` and says `select("label")` reads a
/// property.
#[test]
#[cfg(feature = "script")]
fn select_token_argument_mismatch_help_names_the_token_forms() {
    use crate::script_host::eval;

    for script in [
        r#"g.v().element_map().select(T.key)"#,
        r#"g.v().element_map().select(T.label, "name")"#,
        r#"g.v().element_map().select(Direction.BOTH)"#,
    ] {
        let graph = std::sync::Arc::new(GraphSource::tinkerpop_modern());
        let report = eval(graph, script).expect_err(script).to_string();
        for form in [
            "Argument mismatch for step 'select'",
            "select(T.id), select(T.label)",
            "select(\"label\") reads a property named label",
            "g.v().element_map().select(T.label)",
        ] {
            assert!(report.contains(form), "{script}: missing {form}: {report}");
        }
    }
}

/// `by("name")` over a list (what `select(Pop.all, "a")` returns) points at the child traversal
/// that reads the property of each element.
#[test]
fn by_property_over_a_selected_list_help_names_the_unfold_recipe() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let err = lock
        .traversal()
        .v(None)
        .as_("a")
        .out(None)
        .as_("a")
        .select_pop(Pop::All, "a")
        .by("name")
        .to_list()
        .unwrap_err();
    let rendered = format_diagnostic(&err);
    assert!(
        rendered.contains("Property 'name' not found on element type 'list'"),
        "{rendered}"
    );
    assert!(
        rendered.contains(
            "g.v().as(\"a\").out().as(\"a\").select(Pop.all, \"a\").by(__.unfold().values(\"name\").fold())"
        ),
        "{rendered}"
    );
    assert!(rendered.contains("never to each element"), "{rendered}");
    assert!(
        rendered.contains(".properties()"),
        "the existing advice stays: {rendered}"
    );
}

/// The Rhai DSL surfaces the same plan-time static error as the Rust API for the same query, in
/// both spellings (rule 2's DSL parity).
#[test]
#[cfg(feature = "script")]
fn script_dsl_reports_the_undeclared_label_static_error() {
    use crate::script_host::eval;

    for query in [
        r#"g.V("1").out().select("x").toList()"#,
        r#"g.v("1").out().select("x").to_list()"#,
    ] {
        let graph = std::sync::Arc::new(GraphSource::tinkerpop_modern());
        let err = eval(graph, query).expect_err("\"x\" is never declared anywhere in the query");
        let report = err.to_string();
        assert!(
            report.contains("Step label 'x' is never declared anywhere in this query"),
            "{report}"
        );
    }
}

/// A label that *is* declared somewhere in the query, but is absent on this traverser's path
/// (the `union()`/`optional()`/`choose()` branching case TinkerPop's `KeyNotFoundException` ->
/// `EmptyTraverser` covers), filters the traverser instead of erroring — verified against real
/// TinkerPop 3.7/TinkerGraph (see `step_select.rs`'s module tests for the exact reference
/// results).
#[test]
fn select_declared_but_absent_label_filters_the_traverser() {
    // Real TinkerPop 3.7/TinkerGraph: g.V().union(__.as("a").out("knows"),
    // __.out("created")).select("a") -> [v[1], v[1]] (marko, twice: once per "knows" edge). The
    // four traversers from the unlabeled `out("created")` branch are filtered, not erroring, even
    // though "a" is declared (by the left branch) and this branch never sets it.
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let result = lock
        .traversal()
        .v(None)
        .union(vec![__::as_("a").out("knows"), __::out("created")])
        .select("a")
        .id()
        .to_list()
        .expect("a declared-but-absent label filters, it never errors");
    assert_eq!(
        result,
        vec![
            ElementProperty::String("1".into()),
            ElementProperty::String("1".into())
        ]
    );
}

#[test]
fn path_not_defined_help_names_as_select_and_path() {
    let help = ValueError::PathNotDefined.help().expect("help");
    assert!(
        help.contains("g.v(\"1\").as(\"a\").out().select(\"a\")"),
        "{help}"
    );
    assert!(help.contains("g.v(\"1\").out().path()"), "{help}");
    assert!(!help.contains("strategy"), "{help}");
}

#[test]
fn unpopulated_side_effect_key_help_names_aggregate_store_and_cap() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let err = lock
        .traversal()
        .v(None)
        .cap("friends")
        .next()
        .expect_err("\"friends\" was never populated");
    let help = err.help().expect("help");
    assert!(help.contains("aggregate(\"friends\")"), "{help}");
    assert!(help.contains("store(\"friends\")"), "{help}");
    assert!(help.contains("tree(\"friends\")"), "{help}");
    assert!(help.contains("cap(\"friends\")"), "{help}");
    assert!(help.contains("where(within(\"friends\"))"), "{help}");
    assert!(help.contains("ran but collected nothing"), "{help}");
    assert!(help.contains("returns the empty collection"), "{help}");
}

#[test]
fn timeout_help_rewrites_the_failing_query() {
    let graph = GraphSource::large_110k();
    let lock = graph.read();
    let err = lock
        .traversal()
        .with("evaluationTimeout", 1)
        .v(None)
        .out(None)
        .out(None)
        .out(None)
        .count()
        .next()
        .expect_err("1 ms is not enough");
    let help = err.help().expect("help");
    assert_eq!(
        help,
        "The traversal ran longer than its evaluationTimeout budget of 1 ms. The budget is \
         checked between steps, every 1024 traversers and every 1024 values a step materializes \
         (fold(), group(), order(), a terminal list); the step named above is the one that was \
         running.\n\
         Merge duplicate traversers: barrier() merges equal traversers so later steps process \
         each distinct one once, for example \
         g.v().out().barrier().out().barrier().out().count().\n\
         Narrow the start set with has_label() or has_id(), for example \
         g.v().has_label(\"<label>\").out().out().out().count().\n\
         Cap the stream before it fans out with limit(), for example \
         g.v().limit(1000).out().out().out().count().\n\
         Or raise the budget: g.with(\"evaluationTimeout\", 2).v().out().out().out().count(). It \
         is an administrative option: a script whose host restricts it needs Update on that \
         Option to set it.\n\
         To see which step spends the time, run it with execute() and a profile: the profile \
         up to the timed-out step comes back with the error, for example \
         g.v().out().out().out().count().execute(#{profile: true}).\n\
         If the traversal mutates the graph, nothing of it was applied: a failing traversal is \
         rolled back."
    );
    let rendered = format_diagnostic(&err);
    assert!(
        rendered.contains("Caused by: Traversal exceeded evaluationTimeout of 1 ms (ran "),
        "{rendered}"
    );
}

#[test]
fn timeout_help_bounds_a_glob_path_walk_with_max_depth() {
    let graph = GraphSource::large_110k();
    let lock = graph.read();
    let err = lock
        .traversal()
        .with("evaluationTimeout", 1)
        .v(None)
        .glob_path_with("**", GlobPathOptions::default().prune(__::has("name", "x")))
        .by("name")
        .count()
        .next()
        .expect_err("110k '**' walks exceed 1 ms");
    let help = err.help().expect("help");
    assert!(
        help.contains(
            "\nBound the '**' walk of glob_path() with max_depth, the deepest level below the start \
             vertex it visits, for example \
             g.v().glob_path(\"**\", #{max_depth: 3, prune: __.has(\"name\", \"x\")}).by(\"name\").count().\n"
        ),
        "{help}"
    );
    // The other rewrites show the options map as a Rhai map literal too.
    assert!(
        help.contains(
            "Or raise the budget: g.with(\"evaluationTimeout\", 2).v().glob_path(\"**\", \
             #{prune: __.has(\"name\", \"x\")}).by(\"name\").count()."
        ),
        "{help}"
    );
}

#[test]
fn timeout_help_does_not_bound_a_glob_path_walk_that_has_max_depth() {
    let graph = GraphSource::large_110k();
    let lock = graph.read();
    let err = lock
        .traversal()
        .with("evaluationTimeout", 1)
        .v(None)
        .glob_path_with("**", GlobPathOptions::default().max_depth(50))
        .by("name")
        .count()
        .next()
        .expect_err("110k '**' walks exceed 1 ms");
    let help = err.help().expect("help");
    assert!(!help.contains("max_depth, the deepest level"), "{help}");
}

#[test]
fn glob_path_max_depth_help_shows_the_corrected_query() {
    let graph = TraversalGraph::file_tree();
    let err = graph
        .traversal()
        .v("root")
        .as_("a")
        .glob_path_with(
            "**",
            GlobPathOptions::default()
                .max_depth(0)
                .prune(__::has("name", "x")),
        )
        .by("name")
        .to_list()
        .expect_err("max_depth 0 is invalid");
    assert_eq!(
        err.root_cause().to_string(),
        "Invalid value for option 'glob_path.max_depth': must be at least 1, got 0"
    );
    assert_eq!(
        err.help().expect("help"),
        "max_depth is the deepest level below the start vertex that glob_path() visits: the start \
         vertex is level 0 and its children are level 1, so max_depth is an integer from 1 to \
         4294967295. Leave it out to walk without a bound. For example \
         g.v(\"root\").as(\"a\").glob_path(\"**\", #{max_depth: 1, prune: __.has(\"name\", \"x\")}).by(\"name\")."
    );
    let rendered = format_diagnostic(&err);
    assert!(
        rendered.starts_with(
            "Error: Step #2 'glob_path(\"**\", {max_depth: 0, prune: __.has(\"name\", \"x\")}).by(\"name\")' \
             execution failed"
        ),
        "{rendered}"
    );
}

#[cfg(feature = "script")]
#[test]
fn glob_path_max_depth_help_clamps_a_too_large_value() {
    let graph = std::sync::Arc::new(GraphSource::file_tree());
    let mut scope = crate::script_host::graph_scope(graph);
    let err = crate::script_host::engine()
        .eval_with_scope::<crate::script_host::rhai::Dynamic>(
            &mut scope,
            r#"g.v("root").glob_path("**", #{max_depth: 5000000000}).by("name").to_list()"#,
        )
        .expect_err("max_depth above u32::MAX is invalid");
    let crate::script_host::rhai::EvalAltResult::ErrorRuntime(diagnostic, ..) = *err else {
        panic!("not a runtime error: {err}");
    };
    let diagnostic = diagnostic.to_string();
    assert!(
        diagnostic.contains(
            "\nCaused by: Invalid value for option 'glob_path.max_depth': must be at most 4294967295, \
             got 5000000000\n"
        ),
        "{diagnostic}"
    );
    assert!(
        diagnostic.contains(
            "For example g.v(\"root\").glob_path(\"**\", #{max_depth: 4294967295}).by(\"name\")."
        ),
        "{diagnostic}"
    );
}

#[test]
fn timeout_help_uses_the_query_as_written() {
    let graph = GraphSource::large_110k();
    let lock = graph.read();
    // The optimizer folds has_label() into v(); the advice is built on the written query.
    let err = lock
        .traversal()
        .with("evaluationTimeout", 1)
        .v(None)
        .has_label("V_0_0")
        .both(None)
        .both(None)
        .both(None)
        .both(None)
        .both(None)
        .both(None)
        .both(None)
        .count()
        .next()
        .expect_err("5M paths exceed 1 ms");
    let help = err.help().expect("help");
    assert!(
        help.contains("g.v().limit(1000).has_label(\"V_0_0\").both()"),
        "{help}"
    );
}

#[test]
fn invalid_option_help_shows_a_valid_timeout() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let err = lock
        .traversal()
        .with("evaluationTimeout", -1)
        .v(None)
        .count()
        .next()
        .expect_err("a negative timeout is invalid");
    assert_eq!(
        err.to_string(),
        "Invalid value for option 'evaluationTimeout': expected a non-negative number of \
         milliseconds, got -1"
    );
    assert_eq!(
        err.help().expect("help"),
        "Set 'evaluationTimeout' to a non-negative integer number of milliseconds when the \
         traversal source is created, for example g.with(\"evaluationTimeout\", 5000).v().count(); \
         0 disables the timeout."
    );
}

#[test]
fn locked_option_help_explains_the_host_policy_not_a_bad_value() {
    let policy = ExecutionPolicy::permissive().with_locked("evaluationTimeout");
    let graph = TraversalGraph::tinkerpop_modern().with_execution_policy(policy);
    let graph: Graph = Graph::new(graph);
    let lock = graph.read();
    let err = lock
        .traversal()
        .with("evaluationTimeout", 5000)
        .v(None)
        .count()
        .next()
        .expect_err("evaluationTimeout is locked by the policy");
    assert_eq!(
        err.to_string(),
        "Option 'evaluationTimeout' is locked by this instance's execution policy and cannot be \
         set by a query"
    );
    let help = err.help().expect("help");
    // Unlike InvalidOption's help, this never suggests trying a different value.
    assert!(!help.contains("Set 'evaluationTimeout' to"), "{help}");
    assert!(help.contains("execution policy"), "{help}");
    assert!(help.contains("ExecutionPolicy::with_locked"), "{help}");
}

#[test]
fn timeout_help_names_loop_bounds_for_a_repeat() {
    let graph = GraphSource::large_110k();
    let lock = graph.read();
    let err = lock
        .traversal()
        .with("evaluationTimeout", 1)
        .v(None)
        .repeat(__::both(None))
        .times(8)
        .count()
        .next()
        .expect_err("8 hops of both() on the large graph exceed 1 ms");
    let help = err.help().expect("help");
    assert!(
        help.contains(
            "Bound every repeat() loop with times() or until(), and skip cycles with \
             simple_path() in its body, for example g.v().repeat(__.out().simple_path()).times(3) \
             or g.v().repeat(__.out().simple_path()).until(__.has(\"name\", \"<value>\"))."
        ),
        "{help}"
    );
    assert!(
        help.contains("g.with(\"evaluationTimeout\", 2).v().repeat(__.both()).times(8).count()"),
        "{help}"
    );
    assert!(
        help.contains(
            "Equal traversers are merged automatically inside repeat(), so a loop over converging \
             paths stays cheap; path() and simple_path() record full paths, which prevents the \
             merge"
        ),
        "{help}"
    );
    assert!(
        help.contains(
            "A sack merge operator or with_bulk(false) turns automatic merging off: add barrier() \
             inside the loop body, for example g.v().repeat(__.out().barrier()).times(3).count()."
        ),
        "{help}"
    );
}

#[test]
fn repeat_limit_help_rewrites_the_loop() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let err = lock
        .traversal()
        .with("repeat.max_loops", 3)
        .v("1")
        .repeat(__::both(None))
        .to_list()
        .expect_err("both() walks back and forth forever");
    assert_eq!(
        err.root_cause().to_string(),
        "repeat() exceeded the loop limit of 3 iterations"
    );
    assert_eq!(
        err.help().expect("help"),
        "The loop ran 3 iterations without stopping: it walks a cycle, or nothing ends it.\n\
         Bound the number of iterations with times(), for example repeat(__.both()).times(3).\n\
         Stop at a target with until(), for example \
         repeat(__.both()).until(__.has(\"name\", \"<value>\")).\n\
         On a cyclic graph, drop the paths that revisit an element with simple_path(), for \
         example repeat(__.both().simple_path()).\n\
         Equal traversers are merged automatically inside repeat(), but path() and simple_path() \
         record full paths and prevent that merge, so exponential path counts stay expensive \
         with them. A sack merge operator or with_bulk(false) turns automatic merging off: add \
         barrier() inside the loop body, for example repeat(__.both().barrier()).\n\
         Or raise the limit: g.with(\"repeat.max_loops\", 6).v(\"1\").repeat(__.both()). It is \
         an administrative option: a script whose host restricts it needs Update on that Option \
         to set it.\n\
         On a tree-shaped graph (directories, org charts, categories), walk the levels below a \
         vertex with glob_path() instead: it follows outgoing edges along a pattern, prunes \
         branches that cannot match and stops on cycles without a loop limit, for example \
         g.v(\"1\").glob_path(\"**/*.rs\").by(\"name\"), or g.v(\"1\").glob_path(\"**\").by(\"name\") \
         for every level below; bound how deep it walks with max_depth, for example \
         g.v(\"1\").glob_path(\"**\", #{max_depth: 3}).by(\"name\")."
    );
    let rendered = format_diagnostic(&err);
    assert!(
        rendered.starts_with("Error: Step #1 'repeat(__.both())' execution failed"),
        "{rendered}"
    );
}

#[test]
fn unknown_loop_name_help_lists_the_active_loops() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let err = lock
        .traversal()
        .v("1")
        .repeat_named("a", __::out(None).where_t(__::loops_named("b").is(0)))
        .times(1)
        .to_list()
        .expect_err("no loop is named b");
    assert_eq!(
        err.root_cause().to_string(),
        "loops(\"b\") names no enclosing repeat() (active loops: a)"
    );
    assert_eq!(
        err.help().expect("help"),
        "The enclosing named loops are: a. Read one of them, for example loops(\"a\"), or name \
         the loop that loops(\"b\") should read: repeat(\"b\", __.out())."
    );
}

#[test]
fn unknown_loop_name_help_outside_any_loop() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let err = lock
        .traversal()
        .v("1")
        .loops_named("x")
        .to_list()
        .expect_err("no loop at all");
    assert_eq!(
        err.root_cause().to_string(),
        "loops(\"x\") names no enclosing repeat()"
    );
    assert_eq!(
        err.help().expect("help"),
        "No enclosing repeat() is named 'x'. Name the loop, for example \
         repeat(\"x\", __.out()).until(__.loops(\"x\").is(2)), or read the innermost loop with \
         loops()."
    );
}

#[test]
fn repeat_without_body_help_shows_both_placements() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let err = lock
        .traversal()
        .v(None)
        .times(2)
        .to_list()
        .expect_err("times() without repeat()");
    assert_eq!(
        err.root_cause().to_string(),
        "times(2) modulates a repeat() step, but the traversal has no repeat() for it"
    );
    assert_eq!(
        err.help().expect("help"),
        "Write the loop body with repeat(). Before repeat(), times(2) is checked before each \
         iteration, for example g.v().times(2).repeat(__.out()); after it, times(2) is checked \
         after each iteration, for example g.v().repeat(__.out()).times(2)."
    );
}

#[test]
fn repeat_without_body_help_in_a_child_traversal() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let err = lock
        .traversal()
        .v(None)
        .where_t(__::out(None).emit())
        .to_list()
        .expect_err("emit() without repeat()");
    let help = err.help().expect("help");
    assert!(
        help.contains("for example __.out().emit().repeat(__.out())"),
        "{help}"
    );
    assert!(
        help.contains("for example __.out().repeat(__.out()).emit()"),
        "{help}"
    );
}

#[test]
fn option_without_branch_help() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let err = lock
        .traversal()
        .v(None)
        .option(ElementProperty::from("person"), __::identity())
        .to_list()
        .expect_err(".option() without choose()/branch()");
    let help = err.help().expect("help");
    assert!(
        help.contains("must follow choose(traversal) or branch(traversal)"),
        "{help}"
    );
    assert!(
        help.contains("choose(predicate, onTrue, onFalse)")
            && help.contains("does not accept further .option() calls"),
        "{help}"
    );
    assert!(
        help.contains(
            "option(Merge.onCreate|onMatch, ..) must follow merge_v()/merge_e(), for example \
             g.merge_v(#{\"T.label\": \"person\", \"name\": \"marko\"}).option(Merge.onCreate, \
             #{\"age\": 29}); option(Merge.outV|inV, ..) must follow merge_e(), for example \
             g.merge_e(#{\"Direction.OUT\": Merge.outV, \"Direction.IN\": \"2\"}).option(Merge.outV, \
             #{\"T.id\": \"1\"})."
        ),
        "{help}"
    );
    assert!(
        err.root_cause()
            .to_string()
            .contains("or merge_v()/merge_e() step"),
        "{err}"
    );
}

#[test]
fn invalid_repeat_option_help_shows_valid_values() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let err = lock
        .traversal()
        .with("repeat.order", "sideways")
        .v(None)
        .count()
        .next()
        .expect_err("not an order");
    assert_eq!(
        err.to_string(),
        "Invalid value for option 'repeat.order': expected \"bfs\" or \"dfs\", got \"sideways\""
    );
    assert_eq!(
        err.help().expect("help"),
        "Set 'repeat.order' to \"bfs\" (breadth first, the default) or \"dfs\" (depth first) \
         when the traversal source is created, for example \
         g.with(\"repeat.order\", \"dfs\").v(\"1\").repeat(__.out()).times(2)."
    );

    let err = lock
        .traversal()
        .with("repeat.max_loops", 0)
        .v(None)
        .count()
        .next()
        .expect_err("not a positive limit");
    assert_eq!(
        err.to_string(),
        "Invalid value for option 'repeat.max_loops': expected a positive integer number of \
         iterations up to 4294967295, got 0"
    );
    assert_eq!(
        err.help().expect("help"),
        "Set 'repeat.max_loops' to a positive integer number of iterations when the traversal \
         source is created, for example \
         g.with(\"repeat.max_loops\", 100).v(\"1\").repeat(__.out()).until(__.has(\"name\", \"<value>\")); \
         the default is 10000 and times(n) is never limited."
    );
}

#[test]
fn invalid_optimizer_rule_option_help_names_the_valid_rules() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();

    let err = lock
        .traversal()
        .with("optimizer.disabled", vec!["not_a_real_rule"])
        .v(None)
        .count()
        .next()
        .expect_err("not a known rule name");
    assert_eq!(
        err.to_string(),
        "Invalid value for option 'optimizer.disabled': unknown optimizer rule \"not_a_real_rule\"; \
         valid names are: where_unnest, add_property_fold, filter_reorder, has_id_pushdown, source_filter_pushdown, \
         property_index, barrier_pushdown, group_count_pushdown, count_pushdown, lazy_barrier, all"
    );
    let help = err.help().expect("help");
    assert!(
        help.contains("g.with(\"optimizer.disabled\", [\"filter_reorder\"])"),
        "{help}"
    );
    assert!(help.contains("count_pushdown"), "{help}");
    // Every registered rule is named, in registry order (the list is built from the registry,
    // so it cannot go stale when a rule is added).
    assert!(
        help.contains(
            "(where_unnest, add_property_fold, filter_reorder, has_id_pushdown, \
             source_filter_pushdown, property_index, barrier_pushdown, group_count_pushdown, \
             count_pushdown, lazy_barrier — see the book's Query Optimizer chapter"
        ),
        "{help}"
    );
    assert!(help.contains("\"all\""), "{help}");
    assert!(
        help.contains("(optimizer_rules_applied of g.v().has_label(\"person\").count().profile())"),
        "{help}"
    );

    let err = lock
        .traversal()
        .with("optimizer.disabled", vec!["filter_reorder"])
        .with("optimizer.enabled", vec!["filter_reorder"])
        .v(None)
        .count()
        .next()
        .expect_err("a rule cannot be both enabled and disabled");
    assert_eq!(
        err.to_string(),
        "Invalid value for option 'optimizer.enabled': \"filter_reorder\" is in both \
         'optimizer.enabled' and 'optimizer.disabled'; a rule cannot be both enabled and disabled \
         for the same execution"
    );
}

/// D9: a label containing "::" is rejected at `add_vertex` time with a diagnostic that points at
/// both `labels()` (D4) and the GraphML delimiter reason, not just a bare "invalid" message.
#[test]
fn invalid_label_help_points_at_the_graphml_delimiter_and_labels_step() {
    let mut tg = TraversalGraph::new();
    let err = tg
        .add_vertex(
            Some("1".into()),
            vec!["Admin::Role".into()],
            PropertyMap::default(),
        )
        .expect_err("a label containing \"::\" must be rejected");

    assert_eq!(
        err.to_string(),
        "Vertex label 'Admin::Role' is invalid: it contains \"::\", which GraphSON and GraphML vertex label \
         export reserve as the multi-label delimiter"
    );
    assert_eq!(
        err.help().expect("help"),
        "Rename the label to remove \"::\" — e.g. add_v(\"Admin_Role\") instead of \
         add_v(\"Admin::Role\") — since GraphSON and GraphML export join a vertex's multiple labels \
         with \"::\" (see labels()/has_label())."
    );
}

/// `value()`/`key()` review pass (rule 6, property/value/key/valueMap spec): calling `value()`
/// directly on a bare vertex (not a property-object traverser from `properties()`/`values()`)
/// produces a `CastException` whose help() now points at `.properties(...)`/`.value()` as the
/// fix, not just the generic no-origin message.
#[test]
fn value_on_bare_vertex_help_points_at_properties() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let g = lock.traversal();
    let err = g
        .v("1")
        .value()
        .next()
        .expect_err("a bare vertex is not a property object");

    let report = format_diagnostic(&err);
    assert!(
        report
            .contains("Help: Check the value type before performing conversion or cast operation."),
        "missing the generic no-origin help in: {report}"
    );
    assert!(
        report.contains(".properties(\"name\").value()"),
        "missing the properties()/value() pointer in: {report}"
    );
}

/// The generic `PropertyNotFound` diagnostic (raised, among other places, by an empty
/// `select()`'s label lookup) now also points at `.properties()` with no arguments as a way to
/// list the keys actually present on an element — added alongside the property/value/key/
/// valueMap spec's `properties()` step (rule 6 review pass).
#[test]
fn property_not_found_help_points_at_properties_listing() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let g = lock.traversal();
    let err = g
        .v("1")
        .select(Vec::<String>::new())
        .next()
        .expect_err("an empty select() has no label to find");

    let report = format_diagnostic(&err);
    assert!(
        report.contains(
            "Ensure the property key exists on the specified element type or use .has() filter."
        ),
        "missing the existing generic advice in: {report}"
    );
    assert!(
        report.contains("List the keys actually present with .properties() (no arguments)."),
        "missing the new properties() pointer in: {report}"
    );
}

/// Returns the `help()` of a failed traversal's root `ValueError`.
fn value_help(err: &BoxedTraverserError) -> String {
    let TraverserError::Value(value_error) = err.root_cause() else {
        panic!("expected a value error, got {err:?}");
    };
    value_error.help().expect("help")
}

/// `properties()`/`key()`/`value()` on a `group()` map stay an error (TinkerPop semantics), but
/// the help now shows how to read a map's keys and values, and keeps the generic advice.
#[test]
fn element_steps_on_a_map_point_at_select_column() {
    let graph = GraphSource::tinkerpop_modern();
    let locked = graph.read();
    let err = locked
        .traversal()
        .v(None::<()>)
        .group()
        .by("name")
        .by(__::out(None::<()>).count())
        .properties(None::<()>)
        .key()
        .next()
        .expect_err("a map has no element properties");
    let help = value_help(&err);
    assert!(
        help.starts_with("Check the value type before performing conversion or cast operation."),
        "the existing advice must stay: {help}"
    );
    assert!(
        help.contains("unfold().select(Column.keys) / unfold().select(Column.values)"),
        "{help}"
    );
    assert!(
        help.contains(
            r#"g.v().group().by("name").by(__.out().count()).unfold().select(Column.keys)"#
        ),
        "{help}"
    );

    // The same hint on `key()` of an unfolded entry, whose expected kinds are property objects.
    let err = locked
        .traversal()
        .v(None::<()>)
        .group_count()
        .by("name")
        .unfold()
        .key()
        .next()
        .expect_err("an entry is not a property object");
    let help = value_help(&err);
    assert!(
        help.contains(".value()/.key() only read a property object"),
        "{help}"
    );
    assert!(help.contains("unfold().select(Column.keys)"), "{help}");
}

/// `select(Column)` on something that is not a map names what it reads and how to get there.
#[test]
fn select_column_on_a_non_map_names_unfold_and_select_column() {
    let graph = GraphSource::tinkerpop_modern();
    let locked = graph.read();
    let err = locked
        .traversal()
        .v(None::<()>)
        .select(Column::Keys)
        .next()
        .expect_err("a vertex is not a map");
    assert!(
        err.root_cause()
            .to_string()
            .contains("expected type [map or entry], got type 'vertex'"),
        "{err}"
    );
    let help = value_help(&err);
    assert!(help.starts_with("Check the value type"), "{help}");
    assert!(
        help.contains(r#"g.v().group_count().by("name").unfold().select(Column.keys)"#),
        "{help}"
    );
    assert!(help.contains(r#"select("label")"#), "{help}");
}

/// A plain cast error on a scalar gets neither of the map hints.
#[test]
fn scalar_cast_errors_get_no_map_hint() {
    let graph = GraphSource::tinkerpop_modern();
    let locked = graph.read();
    let err = locked
        .traversal()
        .v(None::<()>)
        .fold()
        .out(None::<()>)
        .next()
        .expect_err("a list is not a vertex");
    let help = value_help(&err);
    assert!(!help.contains("Column.keys"), "{help}");
}

/// The display-limit options name `no_limit: true` as the one way to say "unlimited".
#[cfg(all(feature = "script", feature = "display"))]
#[test]
fn display_limit_options_help() {
    use crate::script_host::eval;
    use std::sync::Arc;

    let graph = || Arc::new(GraphSource::tinkerpop_modern());

    let err = eval(graph(), "g.v().to_table(#{max_rows: 0})")
        .unwrap_err()
        .to_string();
    assert!(
        err.contains(
            "Error: Invalid value for option 'max_rows': 0 is not a positive number of rows"
        ),
        "{err}"
    );
    assert!(
        err.contains(
            "Help: Set 'max_rows' to a positive integer for this call, for example \
             g.v().to_table(#{max_rows: 20}). To show everything, remove both display limits \
             with no_limit: true instead, for example g.v().to_table(#{no_limit: true})"
        ),
        "{err}"
    );

    let err = eval(graph(), "g.v().to_table(#{no_limit: true, max_items: 3})")
        .unwrap_err()
        .to_string();
    assert!(
        err.contains(
            "Help: no_limit: true removes both display limits, so it cannot be combined with \
             max_rows or max_items in the same options map."
        ),
        "{err}"
    );

    let err = eval(graph(), "g.v().to_table(42)").unwrap_err().to_string();
    assert!(err.contains("Invalid value for option 'options'"), "{err}");
    assert!(
        err.contains("Help: Pass the display options as a map"),
        "{err}"
    );

    // A session default of the wrong shape is reported when the traversal is rendered.
    let rendered = eval(graph(), r#"g.with("render.max_rows", 0).v().to_table()"#).unwrap();
    assert!(
        rendered.contains("Invalid value for option 'render.max_rows'"),
        "{rendered}"
    );
    assert!(
        rendered.contains("Help: Set 'render.max_rows' to a positive integer, for example g.with(\"render.max_rows\", 20).v()."),
        "{rendered}"
    );
}

#[test]
fn invalid_modulator_help_explains_positional_by_and_suggests_has_label() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let err = lock
        .traversal()
        .v(None)
        .order()
        .by(["name"])
        .to_list()
        .expect_err("order() cannot sort by a label list");
    let help = err.help().expect("help");
    assert_eq!(
        help,
        "A step's by() modulators are positional: the first by() of order() means one thing, \
         the second another, and each position accepts only certain kinds of argument (a \
         property key, T.id/T.label, a traversal such as __.values(\"name\"), ...). \
         A list of vertex labels, such as by([\"person\", \"software\"]), is not a projection \
         order() can use. To keep only elements with certain labels, filter them with \
         has_label(...) before the step instead, for example \
         g.v().has_label(\"person\", \"software\").order().by(\"name\")."
    );
    let rendered = format_diagnostic(&err);
    assert!(
        rendered.contains(
            "Caused by: `order()` cannot use its by() #1: a vertex-label list is not a \
             projection this step can use"
        ),
        "{rendered}"
    );
}

#[test]
fn invalid_modulator_help_for_glob_path_shows_the_corrected_query() {
    let graph = GraphSource::file_tree();
    let lock = graph.read();
    let err = lock
        .traversal()
        .v("root")
        .glob_path("**/*.rs")
        .by(["directory", "file"])
        .to_list()
        .expect_err("a label list cannot be the match key");
    let help = err.help().expect("help");
    assert_eq!(
        help,
        "glob_path()'s by() modulators are positional: the first by() is the required match key \
         a named pattern segment is compared with, a property key such as by(\"name\") or T.id / \
         T.label; the optional second by() is a non-empty list of vertex labels that every \
         visited vertex below the start must carry. The first by() is the key and the label list \
         goes second: put a key such as by(\"name\") before the label list. For example \
         g.v(\"root\").glob_path(\"**/*.rs\").by(\"name\").by([\"directory\",\"file\"])."
    );
    let rendered = format_diagnostic(&err);
    assert!(
        rendered.contains(
            "Caused by: `glob_path()` cannot use its by() #1: the first by() is the match key; \
             the vertex-label list goes into the second by()"
        ),
        "{rendered}"
    );
}

/// The syntax summary every `InvalidGlobPattern` help ends with.
const GLOB_SYNTAX: &str = "\nA pattern is a '/'-separated list of segments matched level by level \
     below the start vertex: '*' (any value), '?' (one character), '**' (any number of levels, a \
     whole segment only), '[a-z]' / '[!a-z]' (character classes), '{a,b}' (alternatives within \
     one segment) and '\\' to escape any of them, for example glob_path(\"**/src/*.{rs,toml}\").";

/// Runs `g.v("root").glob_path(pattern).by("name")` on the file tree and returns the root cause
/// message and the help of the pattern error it must raise.
fn glob_pattern_failure(pattern: &str) -> (String, String) {
    let graph = GraphSource::file_tree();
    let lock = graph.read();
    let err = lock
        .traversal()
        .v("root")
        .glob_path(pattern)
        .by("name")
        .to_list()
        .expect_err("an invalid glob pattern must fail the traversal");
    // A long pattern is cut in the step rendering, like every long literal.
    let total = pattern.chars().count();
    let shown = if total > 64 {
        let head: String = pattern.chars().take(64).collect();
        format!("{}…\" ({total} chars)", head.replace('\\', "\\\\"))
    } else {
        format!("{}\"", pattern.replace('\\', "\\\\"))
    };
    assert!(
        format_diagnostic(&err).starts_with(&format!(
            "Error: Step #1 'glob_path(\"{shown}).by(\"name\")' execution failed"
        )),
        "{}",
        format_diagnostic(&err)
    );
    (err.root_cause().to_string(), err.help().expect("help"))
}

#[test]
fn invalid_glob_pattern_double_star_inside_a_segment_suggests_the_split_pattern() {
    let (message, help) = glob_pattern_failure("a**b");
    assert_eq!(
        message,
        "Invalid glob pattern 'a**b' at position 1: '**' must be a whole segment"
    );
    assert_eq!(
        help,
        format!(
            "'**' spans whole levels, so it must stand alone between slashes. Write \
             glob_path(\"a*/**/*b\") instead, or plain glob_path(\"**\") to match every level \
             below the start vertex. A literal '*' is written '\\*'.{GLOB_SYNTAX}"
        )
    );
}

#[test]
fn invalid_glob_pattern_slash_inside_alternatives_suggests_two_queries() {
    let (message, help) = glob_pattern_failure("{src/a,lib/b}");
    assert_eq!(
        message,
        "Invalid glob pattern '{src/a,lib/b}' at position 4: alternatives '{..}' cannot contain '/'"
    );
    assert_eq!(
        help,
        format!(
            "Alternatives in '{{..}}' choose within one segment and cannot contain '/'. Split the \
             alternative into two queries, for example glob_path(\"src/a\") and \
             glob_path(\"lib/b\") instead of glob_path(\"{{src/a,lib/b}}\"), or use '**' to span \
             levels, for example glob_path(\"**/{{a,b}}\"). A value that itself contains '/' is \
             matched with '\\/'.{GLOB_SYNTAX}"
        )
    );
}

#[test]
fn invalid_glob_pattern_parent_segment_is_reserved() {
    let (message, help) = glob_pattern_failure("..");
    assert_eq!(
        message,
        "Invalid glob pattern '..' at position 0: '.' and '..' are reserved as whole segments"
    );
    assert_eq!(
        help,
        format!(
            "'.' and '..' as whole segments are reserved for relative navigation. The start \
             vertex is already the current level: glob_path(\"docs/*\") matches its children \
             directly. To walk back up use .in() or repeat(__.in()) before the step, for example \
             g.v(\"x\").in().glob_path(\"docs/*\"). A value that is literally '.' is matched with \
             '\\.'.{GLOB_SYNTAX}"
        )
    );
}

#[test]
fn invalid_glob_pattern_leading_slash_is_relative_to_the_start_vertex() {
    let (message, help) = glob_pattern_failure("/src");
    assert_eq!(
        message,
        "Invalid glob pattern '/src' at position 0: a pattern cannot start with '/'"
    );
    assert_eq!(
        help,
        format!(
            "The pattern is relative to the start vertex: drop the leading and trailing '/' and \
             the empty segments, for example glob_path(\"src/main.rs\") instead of \
             glob_path(\"/src//main.rs/\"). Use '**' to match any number of levels.{GLOB_SYNTAX}"
        )
    );
}

#[test]
fn invalid_glob_pattern_unclosed_class_names_the_closing_bracket() {
    let (message, help) = glob_pattern_failure("file[0-9");
    assert_eq!(
        message,
        "Invalid glob pattern 'file[0-9' at position 4: unclosed character class '['"
    );
    assert_eq!(
        help,
        format!(
            "Close the character class with ']' in the same segment, for example \
             glob_path(\"file[0-9].txt\"); match a literal '[' with '\\['.{GLOB_SYNTAX}"
        )
    );
}

#[test]
fn invalid_glob_pattern_with_too_many_segments_suggests_chaining_the_step() {
    let pattern = vec!["a"; 64].join("/");
    let (message, help) = glob_pattern_failure(&pattern);
    assert_eq!(
        message,
        format!(
            "Invalid glob pattern '{pattern}' at position 126: the pattern has more than 63 segments"
        )
    );
    assert_eq!(
        help,
        format!(
            "A pattern can have at most 63 segments (a run of '**/**' counts once). Split the \
             walk into two consecutive steps, each matching below the vertices the previous one \
             emits, for example glob_path(\"a/b/c\").glob_path(\"d/e/f\") instead of \
             glob_path(\"a/b/c/d/e/f\").{GLOB_SYNTAX}"
        )
    );
}

/// The part of every `glob_path()` `InvalidModulator` help that precedes the specific advice.
const GLOB_BY_POSITIONS: &str = "glob_path()'s by() modulators are positional: the first by() is \
     the required match key a named pattern segment is compared with, a property key such as \
     by(\"name\") or T.id / T.label; the optional second by() is a non-empty list of vertex labels \
     that every visited vertex below the start must carry.";

/// The corrected query every `glob_path()` `InvalidModulator` help ends with.
const GLOB_BY_EXAMPLE: &str =
    "For example g.v(\"root\").glob_path(\"**/*.rs\").by(\"name\").by([\"directory\",\"file\"]).";

/// Asserts the step, root cause and help of a `glob_path()` `by()` misuse on the file tree.
fn assert_glob_modulator_failure(err: BoxedTraverserError, step: &str, cause: &str, advice: &str) {
    let rendered = format_diagnostic(&err);
    assert!(
        rendered.starts_with(&format!("Error: Step #1 '{step}' execution failed")),
        "{rendered}"
    );
    assert_eq!(err.root_cause().to_string(), cause);
    assert_eq!(
        err.help().expect("help"),
        format!("{GLOB_BY_POSITIONS} {advice} {GLOB_BY_EXAMPLE}")
    );
}

#[test]
fn glob_path_without_by_asks_for_the_match_key() {
    let graph = GraphSource::file_tree();
    let lock = graph.read();
    let err = lock
        .traversal()
        .v("root")
        .glob_path("**/*.rs")
        .to_list()
        .expect_err("glob_path() needs a match key");
    assert_glob_modulator_failure(
        err,
        "glob_path(\"**/*.rs\")",
        "`glob_path()` cannot use its by() #1: glob_path() needs a first by() naming the match key",
        "Add the match key as the first by().",
    );
}

#[test]
fn glob_path_with_the_label_list_first_asks_for_the_key_before_it() {
    let graph = GraphSource::file_tree();
    let lock = graph.read();
    let err = lock
        .traversal()
        .v("root")
        .glob_path("**/*.rs")
        .by(["directory", "file"])
        .to_list()
        .expect_err("a label list cannot be the match key");
    assert_glob_modulator_failure(
        err,
        "glob_path(\"**/*.rs\").by([\"directory\",\"file\"])",
        "`glob_path()` cannot use its by() #1: the first by() is the match key; the vertex-label \
         list goes into the second by()",
        "The first by() is the key and the label list goes second: put a key such as by(\"name\") \
         before the label list.",
    );
}

#[test]
fn glob_path_with_an_empty_label_list_asks_for_a_label() {
    let graph = GraphSource::file_tree();
    let lock = graph.read();
    let err = lock
        .traversal()
        .v("root")
        .glob_path("**/*.rs")
        .by("name")
        .by(Vec::<&str>::new())
        .to_list()
        .expect_err("an empty label list admits no vertex");
    assert_glob_modulator_failure(
        err,
        "glob_path(\"**/*.rs\").by(\"name\").by([])",
        "`glob_path()` cannot use its by() #2: the vertex-label list is empty",
        "An empty label list would admit no vertex: list at least one label, or drop the second \
         by() to visit every vertex.",
    );
}

#[test]
fn glob_path_with_a_third_by_asks_to_drop_it() {
    let graph = GraphSource::file_tree();
    let lock = graph.read();
    let err = lock
        .traversal()
        .v("root")
        .glob_path("**/*.rs")
        .by("name")
        .by(["file"])
        .by("name")
        .to_list()
        .expect_err("glob_path() takes at most two by() modulators");
    assert_glob_modulator_failure(
        err,
        "glob_path(\"**/*.rs\").by(\"name\").by([\"file\"]).by(\"name\")",
        "`glob_path()` cannot use its by() #3: glob_path() takes at most two by() modulators",
        "There is no third by(): drop it.",
    );
}

#[test]
fn glob_path_with_a_traversal_key_names_the_accepted_keys() {
    let graph = GraphSource::file_tree();
    let lock = graph.read();
    let err = lock
        .traversal()
        .v("root")
        .glob_path("**/*.rs")
        .by(__::values("name"))
        .to_list()
        .expect_err("a traversal cannot be the match key");
    assert_glob_modulator_failure(
        err,
        "glob_path(\"**/*.rs\").by(<traversal>)",
        "`glob_path()` cannot use its by() #1: the match key must be a property key, T.id or T.label",
        "A traversal, an order, a JSON path, Value or Count cannot be the match key.",
    );
}

/// A `repeat()` walking the file tree runs into its `loop` cycle; the help offers `glob_path()`
/// as the tree walk, written on the steps before the loop.
#[test]
fn repeat_limit_help_on_a_tree_suggests_glob_path() {
    let graph = GraphSource::file_tree();
    let lock = graph.read();
    let err = lock
        .traversal()
        .with("repeat.max_loops", 5)
        .v("root")
        .has_label("directory")
        .emit()
        .repeat(__::out(None))
        .values("name")
        .to_list()
        .expect_err("the file tree's loop directory is a cycle");
    assert_eq!(
        err.root_cause().to_string(),
        "repeat() exceeded the loop limit of 5 iterations"
    );
    assert_eq!(
        err.help().expect("help"),
        "The loop ran 5 iterations without stopping: it walks a cycle, or nothing ends it.\n\
         Bound the number of iterations with times(), for example repeat(__.out()).times(3).\n\
         Stop at a target with until(), for example \
         repeat(__.out()).until(__.has(\"name\", \"<value>\")).\n\
         On a cyclic graph, drop the paths that revisit an element with simple_path(), for \
         example repeat(__.out().simple_path()).\n\
         Equal traversers are merged automatically inside repeat(), but path() and simple_path() \
         record full paths and prevent that merge, so exponential path counts stay expensive \
         with them. A sack merge operator or with_bulk(false) turns automatic merging off: add \
         barrier() inside the loop body, for example repeat(__.out().barrier()).\n\
         Or raise the limit: g.with(\"repeat.max_loops\", 10).v(\"root\").has_label(\"directory\")\
         .emit().repeat(__.out()).values(\"name\"). It is an administrative option: a script \
         whose host restricts it needs Update on that Option to set it.\n\
         On a tree-shaped graph (directories, org charts, categories), walk the levels below a \
         vertex with glob_path() instead: it follows outgoing edges along a pattern, prunes \
         branches that cannot match and stops on cycles without a loop limit, for example \
         g.v(\"root\").has_label(\"directory\").glob_path(\"**/*.rs\").by(\"name\"), or \
         g.v(\"root\").has_label(\"directory\").glob_path(\"**\").by(\"name\") for every level \
         below; bound how deep it walks with max_depth, for example \
         g.v(\"root\").has_label(\"directory\").glob_path(\"**\", #{max_depth: 3}).by(\"name\")."
    );
    // The suggested rewrite runs and terminates on the same graph.
    let names = lock
        .traversal()
        .v("root")
        .has_label("directory")
        .glob_path("**/*.rs")
        .by("name")
        .values("name")
        .to_list()
        .expect("the glob_path() rewrite terminates");
    assert_eq!(names.len(), 12);
    // So does the bounded one.
    let names = lock
        .traversal()
        .v("root")
        .has_label("directory")
        .glob_path_with("**", GlobPathOptions::default().max_depth(3))
        .by("name")
        .values("name")
        .to_list()
        .expect("the bounded glob_path() rewrite terminates");
    assert_eq!(names.len(), 28);
}

/// The Rhai DSL reports the same `repeat()` advice, `glob_path()` suggestion included.
#[cfg(feature = "script")]
#[test]
fn script_repeat_limit_help_suggests_glob_path() {
    use crate::script_host::eval;

    let graph = std::sync::Arc::new(GraphSource::file_tree());
    let err = eval(
        graph,
        r#"g.with("repeat.max_loops", 5).v("root").repeat(__.out()).toList()"#,
    )
    .expect_err("the file tree's loop directory is a cycle");
    let report = err.to_string();
    assert!(
        report.contains(
            "for example g.v(\"root\").glob_path(\"**/*.rs\").by(\"name\"), or \
             g.v(\"root\").glob_path(\"**\").by(\"name\") for every level below; bound how deep it \
             walks with max_depth, for example g.v(\"root\").glob_path(\"**\", #{max_depth: 3}).by(\"name\")."
        ),
        "{report}"
    );
}

/// A `valueMap()` argument that is neither a key string, a null nor a leading boolean fails with an
/// argument mismatch whose help explains the accepted forms and names `element_map()` (rule 6).
#[cfg(feature = "script")]
#[test]
fn value_map_bad_argument_help_explains_the_accepted_forms() {
    use crate::script_host::eval;

    let graph = std::sync::Arc::new(GraphSource::tinkerpop_modern());
    let report = eval(graph, "g.v().valueMap(\"name\", true).to_list()")
        .expect_err("a boolean after a key must fail loudly")
        .to_string();

    assert!(
        report.contains(
            "Error: Argument mismatch for step 'valueMap': expected property keys as strings, \
             optionally preceded by a boolean, got a boolean (`true`) that is not the first argument\n\
             Help: value_map() takes property keys as strings; a leading `true` adds id and label: \
             g.v().value_map(true, \"name\"). A boolean is accepted only as the first argument, and a \
             null key (`()`) is ignored. element_map() returns the same shape with plain values \
             instead of lists: g.v().element_map(\"name\")."
        ),
        "unexpected report: {report}"
    );
}

/// `id()` over a `properties()` element: Graphersal has no property ids, and the help names the
/// readers that work (rule 6).
#[test]
fn id_of_a_property_element_help_names_key_and_element() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let err = lock
        .traversal()
        .v(None)
        .properties(None)
        .id()
        .to_list()
        .expect_err("property elements have no id");
    let help = err.help().expect("help");
    assert!(help.contains("Graphersal has no property ids"), "{help}");
    assert!(help.contains("g.v().properties().key()"), "{help}");
    assert!(help.contains("g.v().properties().element().id()"), "{help}");
    assert!(
        help.contains("label() and has_label() work on properties()"),
        "{help}"
    );
}

/// `by(T.label)` / `by(T.id)` over plain values is an error, and over `properties()` the id token
/// shares the no-property-id help (rule 6).
#[test]
fn by_token_on_non_elements_help_names_the_alternatives() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let err = lock
        .traversal()
        .v(None)
        .values("name")
        .order()
        .by(By::Label)
        .to_list()
        .expect_err("a string has no label");
    let help = err.help().expect("help");
    assert!(
        help.contains("T.label reads the label or id of a graph element"),
        "{help}"
    );
    assert!(help.contains(r#"g.v().order().by("name")"#), "{help}");

    let err = lock
        .traversal()
        .v(None)
        .properties(None)
        .order()
        .by(By::Id)
        .to_list()
        .expect_err("property elements have no id");
    let help = err.help().expect("help");
    assert!(help.contains("by(T.id) over properties()"), "{help}");
    assert!(help.contains("by(T.label)"), "{help}");
}

/// The part of every `dedup()` `InvalidModulator` help that precedes the specific advice.
const DEDUP_BY_WHAT: &str = "dedup() takes one by(): the projection each traverser is \
     deduplicated by, a property key such as by(\"name\"), T.id / T.label, or a traversal such as \
     __.out().count() whose first result is the key.";

/// The corrected query every `dedup()` `InvalidModulator` help ends with.
const DEDUP_BY_EXAMPLE: &str =
    "For example g.v().dedup().by(T.label) keeps the first vertex of each label.";

/// Asserts the root cause and help of a `dedup()` `by()` misuse.
fn assert_dedup_modulator_failure(err: BoxedTraverserError, cause: &str, advice: &str) {
    assert_eq!(err.root_cause().to_string(), cause);
    assert_eq!(
        err.help().expect("help"),
        format!("{DEDUP_BY_WHAT} {advice} {DEDUP_BY_EXAMPLE}")
    );
}

#[test]
fn dedup_by_a_label_list_suggests_has_label() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let err = lock
        .traversal()
        .v(None)
        .dedup()
        .by(["person", "software"])
        .to_list()
        .expect_err("a label list is not a projection");
    assert_dedup_modulator_failure(
        err,
        "`dedup()` cannot use its by() #1: a vertex-label list is not a projection this step can use",
        "A list of vertex labels, such as by([\"person\", \"software\"]), is not a projection: to \
         keep only elements with certain labels, filter them with has_label(...) before the step \
         instead, for example g.v().has_label(\"person\", \"software\").dedup().by(\"name\").",
    );
}

#[test]
fn dedup_by_an_order_suggests_order_after_it() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let err = lock
        .traversal()
        .v(None)
        .dedup()
        .by(graphersal::prelude::Order::Desc)
        .to_list()
        .expect_err("an order is not a projection");
    assert_dedup_modulator_failure(
        err,
        "`dedup()` cannot use its by() #1: an order or Count is not a projection dedup() can key by",
        "An order or Count is not a projection: drop it, and sort after dedup() with \
         order().by(...) if an order was meant, for example \
         g.v().dedup().by(T.label).order().by(\"name\").",
    );
}

#[test]
fn dedup_with_two_by_suggests_one_combined_traversal() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let err = lock
        .traversal()
        .v(None)
        .dedup()
        .by("name")
        .by("age")
        .to_list()
        .expect_err("dedup() takes a single by()");
    assert_dedup_modulator_failure(
        err,
        "`dedup()` cannot use its by() #2: dedup() takes a single by()",
        "There is no second by(): to deduplicate by several values at once, combine them in one \
         traversal, for example g.v().dedup().by(__.values(\"name\", \"age\").fold()).",
    );
}

/// The part of the ring-step `InvalidModulator` help that follows the step-specific sentence.
const RING_BY_ADVICE: &str = "Each by() is a projection: a property key such as by(\"name\"), \
     T.id / T.label, or a traversal such as __.out().count(). An order or Count projects nothing: \
     drop it, and sort afterwards with order().by(...) if an order was meant.";

/// `select()`'s `by()`s form TinkerPop's traversal ring; an order in it names the ring and shows
/// one `by()` applied to both keys.
#[test]
fn select_by_an_order_explains_the_ring() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let err = lock
        .traversal()
        .v("1")
        .as_("a")
        .out("knows")
        .as_("b")
        .select(vec!["a", "b"])
        .by("name")
        .by(graphersal::prelude::Order::Desc)
        .to_list()
        .expect_err("an order is not a projection");
    assert_eq!(
        err.root_cause().to_string(),
        "`select()` cannot use its by() #2: an order or Count is not a projection; this step \
         applies each by() to a value"
    );
    assert_eq!(
        err.help().expect("help"),
        format!(
            "select() applies its by() modulators round-robin, like TinkerPop's traversal ring: \
             one by() per selected key, cycling when there are fewer by() than keys. \
             {RING_BY_ADVICE} For example g.v(\"1\").as(\"a\").out(\"knows\").as(\"b\")\
             .select(\"a\", \"b\").by(\"name\") reads the name of both a and b."
        )
    );
}

/// The same help, worded for each other ring step.
#[test]
fn ring_steps_by_an_order_or_count_explain_the_ring() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let cases = [
        (
            lock.traversal()
                .v(None)
                .project(vec!["name", "out"])
                .by("name")
                .by(By::Count)
                .to_list(),
            "project() applies its by() modulators round-robin, like TinkerPop's traversal ring: \
             one by() per key, cycling when there are fewer by() than keys.",
            "g.v().project(\"name\", \"out\").by(\"name\").by(__.out().count())",
        ),
        (
            lock.traversal()
                .v("1")
                .out(None)
                .path()
                .by(graphersal::prelude::Order::Asc)
                .to_list(),
            "path() applies its by() modulators round-robin, like TinkerPop's traversal ring: \
             one by() per path object, cycling along the path.",
            "g.v(\"1\").out().path().by(\"name\")",
        ),
        (
            lock.traversal()
                .v("1")
                .out(None)
                .tree(None::<()>)
                .by(By::Count)
                .to_list(),
            "tree() applies its by() modulators round-robin, like TinkerPop's traversal ring: \
             one by() per tree level, cycling along each path.",
            "g.v(\"1\").out().tree().by(\"name\")",
        ),
        (
            lock.traversal()
                .v(None)
                .as_("a")
                .out("knows")
                .as_("b")
                .math("a + b")
                .by(graphersal::prelude::Order::Desc)
                .to_list(),
            "math() applies its by() modulators round-robin, like TinkerPop's traversal ring: \
             one by() per variable of the equation, cycling when there are fewer by() than \
             variables.",
            "g.v().as(\"a\").out(\"knows\").as(\"b\").math(\"a + b\").by(\"age\")",
        ),
    ];
    for (result, what, example) in cases {
        let err = result.expect_err(what);
        assert!(
            matches!(err.root_cause(), TraverserError::InvalidModulator { .. }),
            "{err}"
        );
        assert_eq!(
            err.help().expect("help"),
            format!("{what} {RING_BY_ADVICE} For example {example}.")
        );
    }
}

/// The part of every `valueMap()` `InvalidModulator` help that precedes the specific advice.
const VALUE_MAP_BY_WHAT: &str = "value_map() wraps each property value in a list, such as {name: \
     [marko], age: [29]}, and its by() is a traversal applied to each of those lists; the first \
     result replaces the list.";

/// The corrected query every `valueMap()` `InvalidModulator` help ends with.
const VALUE_MAP_BY_EXAMPLE: &str = "For example g.v().value_map().by(__.unfold()) returns the \
     plain values, {name: marko, age: 29}; element_map() returns them too, together with id and \
     label.";

/// Asserts the root cause and help of a `valueMap()` `by()` misuse.
fn assert_value_map_modulator_failure(err: BoxedTraverserError, cause: &str, advice: &str) {
    assert_eq!(err.root_cause().to_string(), cause);
    assert_eq!(
        err.help().expect("help"),
        format!("{VALUE_MAP_BY_WHAT} {advice} {VALUE_MAP_BY_EXAMPLE}")
    );
}

#[test]
fn value_map_by_a_label_list_suggests_has_label() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let err = lock
        .traversal()
        .v(None)
        .value_map(None)
        .by(["person"])
        .to_list()
        .expect_err("a label list is not a traversal");
    assert_value_map_modulator_failure(
        err,
        "`value_map()` cannot use its by() #1: a vertex-label list is not a projection this step can use",
        "A list of vertex labels, such as by([\"person\", \"software\"]), is not a traversal: to \
         keep only elements with certain labels, filter them with has_label(...) before the step \
         instead, for example g.v().has_label(\"person\").value_map().",
    );
}

#[test]
fn value_map_by_a_property_key_suggests_a_traversal() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let err = lock
        .traversal()
        .v(None)
        .value_map(None)
        .by("name")
        .to_list()
        .expect_err("a property key cannot take a value list");
    assert_value_map_modulator_failure(
        err,
        "`value_map()` cannot use its by() #1: value_map() applies by() to each property's value \
         list; only a traversal can take it",
        "A property key, T.id / T.label, an order, a JSON path, Value or Count cannot take a \
         list: pass a traversal instead, or select the keys as arguments, for example \
         g.v().value_map(\"name\").by(__.unfold()).",
    );
}

/// The part of every `group()`/`group_count()` `InvalidModulator` help that precedes the
/// specific advice.
const GROUP_BY_WHAT: &str = "group()'s by() modulators are positional: the first by() reads the \
     grouping key from each element, and the second reads the value kept under that key \
     (group_count() and by(Count) ask for a count there instead).";

/// `group().by(Order)` fails with `InvalidModulator`, not `QueryExecution`: an order is not a
/// projection this step can use, consistent with every other `by()` consumer.
#[test]
fn group_by_an_order_is_invalid_modulator_not_query_execution() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let err = lock
        .traversal()
        .v(None)
        .group()
        .by(graphersal::prelude::Order::Desc)
        .to_list()
        .expect_err("an order is not a projection group() can use");
    assert!(
        matches!(err.root_cause(), TraverserError::InvalidModulator { .. }),
        "expected InvalidModulator, got {err:?}"
    );
    assert_eq!(
        err.root_cause().to_string(),
        "`group()` cannot use its by() #1: an order is not a projection this step can use"
    );
    assert_eq!(
        err.help().expect("help"),
        format!(
            "{GROUP_BY_WHAT} An order, such as Order.desc, is not a projection: group() applies \
             each by() to a key or a value, it does not sort the result. Drop it, and sort \
             afterward instead, for example g.v().group().by(\"name\").unfold().order()\
             .by(Order.desc). For example g.v().group().by(\"name\")."
        )
    );
}

/// The same rejection at the second `by()` position, and for `group_count()`.
#[test]
fn group_count_by_an_order_at_the_second_position_is_invalid_modulator() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let err = lock
        .traversal()
        .v(None)
        .group_count()
        .by("name")
        .by(graphersal::prelude::Order::Desc)
        .to_list()
        .expect_err("an order is not a projection group_count() can use");
    assert_eq!(
        err.root_cause().to_string(),
        "`group_count()` cannot use its by() #2: an order is not a projection this step can use"
    );
    assert_eq!(
        err.help().expect("help"),
        format!(
            "{GROUP_BY_WHAT} An order, such as Order.desc, is not a projection: group() applies \
             each by() to a key or a value, it does not sort the result. Drop it, and sort \
             afterward instead, for example g.v().group().by(\"name\").unfold().order()\
             .by(Order.desc). For example g.v().group_count().by(\"name\")."
        )
    );
}

/// The list of `by()`-modulating steps every misplaced-`by()` help ends with.
const BY_STEPS: &str = "The steps that take by() are order(), dedup(), group(), group_count(), \
     select() with step labels, project(), path(), simple_path(), cyclic_path(), where(), tree(), \
     math(), value_map(), aggregate(), store(), sample() (the weight) and glob_path().";

/// The advice for a `by()` after a step that takes none, other than the special-cased steps.
const MOVE_THE_BY: &str = "Move the by() to the step it was meant for, for example \
     g.v().out().order().by(\"name\") sorts the neighbours by name; to read a property of each \
     element, use values(), for example g.v().out().values(\"name\") rather than \
     g.v().out().by(\"name\").";

#[test]
fn a_by_after_a_step_that_takes_none_names_where_it_belongs() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let cases = [
        (
            lock.traversal().v(None).out(None).by("name").to_list(),
            "`out()` cannot use its by() #1: out() does not take by() modulators",
            format!(
                "by() modulates the step right before it, and out() does not take one. {MOVE_THE_BY} {BY_STEPS}"
            ),
        ),
        (
            lock.traversal().v(None).has("age", 29).by("x").to_list(),
            "`has()` cannot use its by() #1: has() does not take by() modulators",
            format!(
                "by() modulates the step right before it, and has() does not take one. {MOVE_THE_BY} {BY_STEPS}"
            ),
        ),
        (
            lock.traversal().v(None).count().by("name").to_list(),
            "`count()` cannot use its by() #1: count() does not take by() modulators",
            format!(
                "by() modulates the step right before it, and count() does not take one. To count \
                 per value, use group_count() with the by() instead, for example \
                 g.v().group_count().by(\"name\") rather than g.v().count().by(\"name\"). {BY_STEPS}"
            ),
        ),
        (
            lock.traversal().v(None).sum().by("age").to_list(),
            "`sum()` cannot use its by() #1: sum() does not take by() modulators",
            format!(
                "by() modulates the step right before it, and sum() does not take one. To \
                 aggregate a property, read it with values() first, for example \
                 g.v().values(\"age\").sum() rather than g.v().sum().by(\"age\"). {BY_STEPS}"
            ),
        ),
        (
            lock.traversal()
                .v(None)
                .select(Column::Keys)
                .by("x")
                .to_list(),
            "`select()` cannot use its by() #1: select(Column.keys) does not take by() modulators",
            format!(
                "by() modulates the step right before it, and select(Column.keys) / \
                 select(Column.values) does not take one. They read a map as it is; put the by() \
                 on the step that builds the map, for example \
                 g.v().group().by(\"name\").select(Column.keys). {BY_STEPS}"
            ),
        ),
        (
            lock.traversal().by("name").to_list(),
            "`g()` cannot use its by() #1: by() modulates the step before it, and there is none",
            format!(
                "by() modulates the step right before it, so it cannot start a traversal. Write \
                 the step first and the by() after it, for example g.v().order().by(\"name\"). \
                 {BY_STEPS}"
            ),
        ),
    ];
    for (result, cause, help) in cases {
        let err = result.expect_err(cause);
        assert_eq!(err.root_cause().to_string(), cause);
        assert_eq!(err.help().expect("help"), help);
    }
}

/// The list of `from()`/`to()`-modulating steps every misplaced-`from()`/`to()` help ends with.
const FROM_TO_STEPS: &str = "The steps that take from()/to() are add_e(), path(), \
     simple_path() and cyclic_path().";

#[test]
fn a_from_to_after_a_step_that_takes_none_names_where_it_belongs() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let cases = [
        (
            lock.traversal().v(None).out(None).from("1").to_list(),
            "`out()` cannot use its from()/to() #1: out() does not take from()/to() modulators",
            format!(
                "from()/to() modulates the step right before it, and out() does not take one. \
                 add_e() accepts from()/to(), to point a new edge at existing \
                 elements or labelled positions, for example g.v(\"1\").add_e(\"knows\").to(\"2\") \
                 or g.v(\"1\").as(\"a\").out(\"knows\").add_e(\"co-developer\").from(\"a\"). \
                 An endpoint can also be a traversal, \
                 g.v(\"1\").add_e(\"knows\").to(__.v(\"2\")), or a side-effect key, \
                 g.with_side_effect(\"b\", \"2\").v(\"1\").add_e(\"knows\").to(\"b\"). \
                 path(), simple_path() and cyclic_path() take them as a window onto the path \
                 between two labelled positions, for example \
                 g.v().as(\"a\").out().as(\"b\").path().from(\"a\").to(\"b\"). \
                 {FROM_TO_STEPS}"
            ),
        ),
        (
            lock.traversal().v(None).count().to("1").to_list(),
            "`count()` cannot use its from()/to() #1: count() does not take from()/to() modulators",
            format!(
                "from()/to() modulates the step right before it, and count() does not take one. \
                 add_e() accepts from()/to(), to point a new edge at existing \
                 elements or labelled positions, for example g.v(\"1\").add_e(\"knows\").to(\"2\") \
                 or g.v(\"1\").as(\"a\").out(\"knows\").add_e(\"co-developer\").from(\"a\"). \
                 An endpoint can also be a traversal, \
                 g.v(\"1\").add_e(\"knows\").to(__.v(\"2\")), or a side-effect key, \
                 g.with_side_effect(\"b\", \"2\").v(\"1\").add_e(\"knows\").to(\"b\"). \
                 path(), simple_path() and cyclic_path() take them as a window onto the path \
                 between two labelled positions, for example \
                 g.v().as(\"a\").out().as(\"b\").path().from(\"a\").to(\"b\"). \
                 {FROM_TO_STEPS}"
            ),
        ),
        (
            lock.traversal().from("1").to_list(),
            "`g()` cannot use its from()/to() #1: from()/to() modulates the step before it, and there is none",
            format!(
                "from()/to() modulates the step right before it, so it cannot start a \
                 traversal. Write the step first and from()/to() after it, for example \
                 g.v(\"1\").add_e(\"knows\").to(\"2\"). {FROM_TO_STEPS}"
            ),
        ),
    ];
    for (result, cause, help) in cases {
        let err = result.expect_err(cause);
        assert_eq!(err.root_cause().to_string(), cause);
        assert_eq!(err.help().expect("help"), help);
    }
}

/// A global reducer fed a folded list names `Scope.local` as the direct fix, with a runnable
/// example, and keeps the existing advice.
#[test]
fn cast_exception_on_a_folded_list_help_names_scope_local() {
    let graph = GraphSource::tinkerpop_modern();
    let locked = graph.read();
    let err = locked
        .traversal()
        .v(None)
        .values("age")
        .fold()
        .sum()
        .next()
        .expect_err("a list is not a number");

    let report = format_diagnostic(&err);
    assert!(
        report.contains("got type 'array'"),
        "missing root cause in: {report}"
    );
    assert!(
        report.contains("Check the value type before performing conversion or cast operation."),
        "the existing advice must stay:\n{report}"
    );
    assert!(
        report.contains(
            "The value is a list (for example from fold()): reduce each traverser's list with \
             Scope.local, for example g.v().values(\"age\").fold().sum(Scope.local) or \
             g.v().fold().count(Scope.local), or unfold() it first."
        ),
        "the help must name Scope.local:\n{report}"
    );
}

/// A global reducer fed a stored list property names the property-shaped `Scope.local` fix.
#[test]
fn cast_exception_on_a_stored_list_help_names_scope_local() {
    let graph = GraphSource::empty();
    {
        let mut writable = graph.write();
        writable
            .traversal_mut()
            .add_v("data")
            .property(
                "list",
                ElementProperty::Array(vec![1_i64.into(), 2_i64.into()]),
            )
            .next()
            .expect("add vertex");
    }
    let locked = graph.read();
    let err = locked
        .traversal()
        .v(None)
        .values("list")
        .sum()
        .next()
        .expect_err("a list is not a number");

    let report = format_diagnostic(&err);
    assert!(
        report.contains(
            "'list' holds a list: reduce each traverser's list with Scope.local, for example \
             values(\"list\").sum(Scope.local) or values(\"list\").count(Scope.local), or unfold() it first."
        ),
        "the help must name Scope.local:\n{report}"
    );
}

/// A global string step fed a folded list names the string-step `Scope.local` form too.
#[test]
fn cast_exception_on_a_folded_list_help_names_a_local_string_step() {
    let graph = GraphSource::tinkerpop_modern();
    let locked = graph.read();
    let err = locked
        .traversal()
        .v(None)
        .values("name")
        .fold()
        .to_upper()
        .next()
        .expect_err("a list is not a string");

    let report = format_diagnostic(&err);
    assert!(
        report.contains("got type 'array'"),
        "missing root cause in: {report}"
    );
    assert!(
        report.contains("g.v().values(\"age\").fold().sum(Scope.local)"),
        "the reducer example must stay:\n{report}"
    );
    assert!(
        report.contains(
            "A string step transforms each element of the list the same way, for example \
             g.v().values(\"name\").fold().to_upper(Scope.local)."
        ),
        "the help must name the string-step Scope.local form:\n{report}"
    );
}

/// A local string step fed a list with a non-string element reports TinkerPop's wording and
/// shows how to convert or filter the elements first.
#[test]
fn local_string_step_on_a_non_string_element_explains_the_fix() {
    let graph = GraphSource::tinkerpop_modern();
    let locked = graph.read();
    let err = locked
        .traversal()
        .v(None)
        .values("age")
        .fold()
        .trim_scoped(Scope::Local)
        .next()
        .expect_err("a number is not a string");

    let report = format_diagnostic(&err);
    assert!(
        report.contains(
            "Cast exception: The trim(local) step can only take string or list of strings, but \
             element 0 of the list has type 'int64'"
        ),
        "missing root cause in: {report}"
    );
    assert!(
        report.contains(
            "trim(local) transforms each element of the list a traverser holds, so every element \
             must be a string (a null element stays null), but element 0 has type 'int64'.\n"
        ),
        "{report}"
    );
    assert!(
        report.contains(
            "Convert the elements first, for example \
             g.v().values(\"name\").fold().unfold().as_string().fold().trim(Scope.local), or keep \
             only the strings with \
             g.v().values(\"name\").fold().unfold().is(typeOf(GType.STRING)).fold().trim(Scope.local)."
        ),
        "{report}"
    );

    // The example names the snake_case spelling of a camelCase step.
    let err = locked
        .traversal()
        .v(None)
        .values("age")
        .fold()
        .l_trim_scoped(Scope::Local)
        .next()
        .expect_err("a number is not a string");
    let report = format_diagnostic(&err);
    assert!(
        report.contains("The lTrim(local) step can only take"),
        "{report}"
    );
    assert!(report.contains(".fold().l_trim(Scope.local)"), "{report}");
}

/// `as_string(local)` fed a list with a `null` element says there is nothing to parse and how
/// to drop the nulls.
#[test]
fn local_as_string_on_a_null_element_explains_the_fix() {
    let graph = GraphSource::empty();
    let locked = graph.read();
    let err = locked
        .traversal()
        .inject(vec![1_i64])
        .constant(ElementProperty::Array(vec![
            1_i64.into(),
            ElementProperty::Null,
        ]))
        .as_string_scoped(Scope::Local)
        .next()
        .expect_err("null has no string form");

    let report = format_diagnostic(&err);
    assert!(
        report.contains(
            "The asString(local) step can only take non-null elements (Can't parse null as a \
             string), but element 1 of the list has type 'null'"
        ),
        "missing root cause in: {report}"
    );
    assert!(
        report.contains(
            "Drop the null elements before converting, for example \
             g.v().values(\"age\").fold().unfold().not(__.is(typeOf(GType.NULL))).fold().as_string(Scope.local), \
             or convert the stream before folding it: g.v().values(\"age\").as_string().fold()."
        ),
        "{report}"
    );
}

/// A scalar mismatch is not a list: no `Scope.local` hint.
#[test]
fn cast_exception_on_a_scalar_help_omits_scope_local() {
    let graph = graph_with_string_age();
    let locked = graph.read();
    let err = locked
        .traversal()
        .v(None)
        .has_label("person")
        .values("age")
        .max()
        .next()
        .expect_err("a string age must not be aggregated");
    let report = format_diagnostic(&err);
    assert!(!report.contains("Scope.local"), "{report}");
}

/// `InvalidRange` raised by the local range family keeps the stream-wide advice and names the
/// `Scope.local` form with a runnable example.
#[test]
fn invalid_local_range_help_names_the_scope_local_form() {
    let graph = GraphSource::tinkerpop_modern();
    let locked = graph.read();
    let err = locked
        .traversal()
        .v(None)
        .fold()
        .range_scoped(Scope::Local, 3, 1)
        .next()
        .expect_err("high below low");

    let report = format_diagnostic(&err);
    assert!(
        report.contains("Invalid range [3, 1): high must be at least low"),
        "{report}"
    );
    assert!(
        report.contains("range(low, high) slices the stream to [low, high)"),
        "the existing advice must stay:\n{report}"
    );
    assert!(
        report.contains(
            "The same bounds apply to the Scope.local form, which slices the list or map each \
             traverser holds, for example g.v().values(\"name\").fold().range(Scope.local, 1, 3); \
             its limit(Scope.local, n), skip(Scope.local, n) and tail(Scope.local, n) take a count \
             of zero or more, for example g.v().values(\"name\").fold().limit(Scope.local, 2)."
        ),
        "the help must name the Scope.local form:\n{report}"
    );

    let err = locked
        .traversal()
        .v(None)
        .fold()
        .limit_scoped(Scope::Local, -1)
        .next()
        .expect_err("negative count");
    let report = format_diagnostic(&err);
    assert!(
        report.contains("limit(local, -1) needs a count of zero or more"),
        "{report}"
    );
}

/// `by(Column.keys)` / `by(Column.values)` on a step other than `order()` fails with
/// `InvalidModulator`, and the help names the supported `order().by(Column.values, Order.desc)`
/// and `order(Scope.local)` forms plus `select(Column)`.
#[test]
fn by_column_outside_order_names_the_order_forms() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let err = lock
        .traversal()
        .v(None)
        .group()
        .by(Column::Keys)
        .to_list()
        .expect_err("group() does not take by(Column)");
    assert_eq!(
        err.root_cause().to_string(),
        "`group()` cannot use its by() #1: a Column (keys/values) is a sort key only order() takes"
    );
    assert_eq!(
        err.help().expect("help"),
        "by(Column.keys) / by(Column.values) reads the key or value of a map entry (or the keys or \
         values of a whole map), and only order() takes it, as a sort key, not group(). Sort map \
         entries with it, for example \
         g.v().has_label(\"person\").group().by(\"name\").by(__.out_e().values(\"weight\").sum()).unfold().order().by(Column.values, Order.desc), \
         or sort the entries inside each map with order(Scope.local).by(Column.values). To read a \
         map's keys or values as data, use select(Column.keys) / select(Column.values) after the \
         step instead, for example g.v().group().by(\"name\").select(Column.keys). The steps that \
         take by() are order(), dedup(), group(), group_count(), select() with step labels, \
         project(), path(), simple_path(), cyclic_path(), where(), tree(), math(), \
         value_map(), aggregate(), store(), sample() (the weight) and glob_path()."
    );
    // The same help for a ring step, in the DSL.
    #[cfg(feature = "script")]
    {
        let err = crate::script_host::eval_value(
            std::sync::Arc::new(GraphSource::tinkerpop_modern()),
            r#"g.V().project("a").by(Column.values).toList()"#,
        )
        .expect_err("project() does not take by(Column)");
        assert!(err.to_string().contains("only order() takes it"), "{err}");
    }
}

/// `property(Cardinality.list, ..)` fails with `UnsupportedCardinality`, leaves the graph
/// unchanged, and its help names `Cardinality.single` and the array-value alternative.
#[test]
fn unsupported_cardinality_help_suggests_single_or_an_array_value() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let err = graph
        .traversal_mut()
        .v("1")
        .property_with(Cardinality::List, "age", 1)
        .next()
        .expect_err("list cardinality");
    assert!(matches!(
        err.root_cause(),
        TraverserError::UnsupportedCardinality { .. }
    ));
    let report = format_diagnostic(&err);
    assert!(
        report.contains(
            "Graphersal stores one value per property key, so only Cardinality.single can be \
             written: it replaces the current value, for example \
             g.v(\"1\").property(Cardinality.single, \"age\", 30) (the same as \
             g.v(\"1\").property(\"age\", 30)). To keep several values, store them as one array \
             value: g.v(\"1\").property(\"tags\", [\"a\", \"b\"])."
        ),
        "{report}"
    );
    let age = graph.traversal().v("1").values("age").next().unwrap();
    assert_eq!(age, Some(ElementProperty::from(29)));
}

/// A cardinality on an edge property explains that edges take none.
#[test]
fn edge_cardinality_help_says_edges_take_none() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let err = graph
        .traversal_mut()
        .e("0")
        .property_with(Cardinality::Single, "weight", 1.0)
        .next()
        .expect_err("edge cardinality");
    let report = format_diagnostic(&err);
    assert!(
        report.contains("Property cardinality can only be set for a Vertex"),
        "{report}"
    );
    assert!(
        report.contains("Edge properties have no cardinality: drop the Cardinality argument"),
        "{report}"
    );
    // The example names an edge that exists in the modern graph (edge ids "0".."5").
    assert!(
        report.contains("for example g.e(\"0\").property(\"weight\", 0.5)"),
        "{report}"
    );
}

/// `property(T.id, ..)` on an existing element points at `addV()` with a runnable example.
#[test]
fn token_property_outside_add_help_points_at_add_v() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let err = graph
        .traversal_mut()
        .v("1")
        .property_id(5)
        .next()
        .expect_err("T.id outside addV");
    let report = format_diagnostic(&err);
    assert!(
        report.contains(
            "property(T.id, ..) can only set the id of an element add_v()/add_e() is creating"
        ),
        "{report}"
    );
    assert!(
        report.contains(
            "property(T.id, v) sets the id of the element add_v()/add_e() is creating; write it right \
             after add_v()/add_e() (other property() calls may come in between), for example \
             g.add_v(\"person\").property(T.id, \"p1\")."
        ),
        "{report}"
    );

    let err = graph
        .traversal_mut()
        .v("1")
        .add_e("likes")
        .to("2")
        .property_label("x")
        .next()
        .expect_err("T.label after addE");
    let report = format_diagnostic(&err);
    assert!(
        report.contains("An edge's label comes from add_e(label), for example g.v(\"1\").add_e(\"knows\").to(\"2\")."),
        "{report}"
    );
}

/// `add_e()` as a start step without endpoints names the self-loop default and the start form.
#[test]
fn add_e_endpoint_help_names_the_self_loop_default() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let err = graph
        .traversal_mut()
        .add_e("x")
        .next()
        .expect_err("no endpoints");
    let report = format_diagnostic(&err);
    assert!(
        report.contains("add_e() as a start step has no incoming vertex"),
        "{report}"
    );
    assert!(
        report.contains("and neither makes a self-loop (g.v(\"1\").add_e(\"self\"))"),
        "{report}"
    );
    assert!(
        report.contains("g.add_e(\"knows\").from(\"1\").to(\"2\")"),
        "{report}"
    );
    assert!(
        report.contains(
            "To create the edge only when it does not exist yet, use mergeE, for example \
             g.merge_e(#{\"T.label\": \"knows\", \"Direction.OUT\": \"1\", \"Direction.IN\": \"2\"})."
        ),
        "{report}"
    );
}

/// A reserved key in `property(map)` names `property(T.id, v)` after `addV()`.
#[test]
fn property_map_reserved_key_help_names_the_token_form() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let mut map = PropertyMap::default();
    map.insert("T.id", 3.into());
    let err = graph
        .traversal_mut()
        .v("1")
        .property_map(ElementProperty::Object(Box::new(map)))
        .next()
        .expect_err("reserved key");
    let report = format_diagnostic(&err);
    assert!(report.contains("map key \"T.id\" is reserved"), "{report}");
    assert!(
        report.contains(
            "property(map) writes one property per map entry, for example \
             g.v(\"1\").property(#{\"name\": \"marko\", \"age\": 29})"
        ),
        "{report}"
    );
    assert!(
        report.contains("g.add_v(\"person\").property(T.id, \"p1\")"),
        "{report}"
    );
}

fn merge_map(entries: &[(&str, ElementProperty)]) -> ElementProperty {
    let mut map = PropertyMap::default();
    for (key, value) in entries {
        map.insert(*key, value.clone());
    }
    ElementProperty::Object(Box::new(map))
}

/// A hidden `~id` key in a `mergeV()` map: rejected before execution, and the help names the
/// reserved `"T.id"` key with a runnable example.
#[test]
fn invalid_merge_map_help_names_the_reserved_key() {
    let mut graph = TraversalGraph::new();
    let err = graph
        .traversal_mut()
        .merge_v(merge_map(&[("~id", "1".into())]))
        .next()
        .expect_err("hidden key");
    assert!(
        matches!(err.root_cause(), TraverserError::InvalidMergeMap { .. }),
        "{err}"
    );
    let report = format_diagnostic(&err);
    assert!(
        report.contains(
            "Property key can not be a hidden key: ~id (in the merge() map of merge_v())"
        ),
        "{report}"
    );
    assert!(
        report.contains(
            "Write the id with the reserved key \"T.id\" instead of \"~id\", for example \
             g.merge_v(#{\"T.id\": \"1\"})."
        ),
        "{report}"
    );
    assert!(
        report.contains(
            "A merge_v() map holds property keys plus the reserved keys \"T.id\" and \"T.label\""
        ),
        "{report}"
    );

    let err = graph
        .traversal_mut()
        .merge_v(ElementProperty::Null)
        .option_merge(Merge::OnMatch, merge_map(&[("T.label", "x".into())]))
        .next()
        .expect_err("label in onMatch");
    let report = format_diagnostic(&err);
    assert!(
        report.contains(
            "option(Merge.onMatch, ..) only updates properties of the matched element, for example \
             g.merge_v(#{\"name\": \"marko\"}).option(Merge.onMatch, #{\"age\": 30})"
        ),
        "{report}"
    );
}

/// Conflicting values for one key in the merge map and the onCreate map.
#[test]
fn merge_override_help_says_on_create_only_adds_keys() {
    let mut graph = TraversalGraph::new();
    let err = graph
        .traversal_mut()
        .merge_v(merge_map(&[("T.label", "a".into())]))
        .option_merge(Merge::OnCreate, merge_map(&[("T.label", "b".into())]))
        .next()
        .expect_err("override");
    assert!(
        matches!(err.root_cause(), TraverserError::MergeOverride { .. }),
        "{err}"
    );
    let report = format_diagnostic(&err);
    assert!(
        report.contains("option(onCreate) cannot override values from merge() argument: T.label is \"a\" in merge_v() but \"b\" in option(onCreate)"),
        "{report}"
    );
    assert!(
        report.contains(
            "option(Merge.onCreate, ..) can only add keys to the merge() map: a key present in both \
             must have the same value there. Remove \"T.label\" from the onCreate map"
        ),
        "{report}"
    );
    assert_eq!(
        graph.traversal().v(None).count().next().unwrap(),
        Some(ElementProperty::from(0))
    );
}

/// A duplicate vertex id suggests `mergeV` as the get-or-create form.
#[test]
fn vertex_id_unique_constraint_help_suggests_merge_v() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let err = graph
        .traversal_mut()
        .add_v("person")
        .property_id("1")
        .next()
        .expect_err("duplicate id");
    let report = format_diagnostic(&err);
    assert!(
        report.contains("Ensure vertex IDs are unique when adding new vertices."),
        "{report}"
    );
    assert!(
        report.contains(
            "use mergeV, for example g.merge_v(#{\"T.id\": \"1\"}).option(Merge.onCreate, #{\"T.label\": \"person\"})."
        ),
        "{report}"
    );
}

/// `mergeE()` without the vertices an edge needs: the error names the end, and the help gives
/// runnable id and placeholder forms.
#[test]
fn merge_vertex_unresolved_help_names_ids_and_placeholders() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let err = graph
        .traversal_mut()
        .merge_e(merge_map(&[("T.label", "x".into())]))
        .next()
        .expect_err("no vertices");
    assert!(
        matches!(
            err.root_cause(),
            TraverserError::MergeVertexUnresolved {
                direction: Direction::Out,
                ..
            }
        ),
        "{err}"
    );
    let report = format_diagnostic(&err);
    assert!(report.contains("Out Vertex not specified"), "{report}");
    assert!(
        report.contains(
            "An edge merge_e() creates needs both vertices: give \"Direction.OUT\" and \
             \"Direction.IN\" an existing vertex id, for example g.merge_e(#{\"T.label\": \"knows\", \
             \"Direction.OUT\": \"1\", \"Direction.IN\": \"2\"})"
        ),
        "{report}"
    );

    let err = graph
        .traversal_mut()
        .merge_e(merge_map(&[
            ("Direction.OUT", "1".into()),
            ("Direction.IN", "100".into()),
        ]))
        .next()
        .expect_err("unknown id");
    let report = format_diagnostic(&err);
    assert!(
        report.contains("Vertex id could not be resolved from mergeE: 100"),
        "{report}"
    );
    assert!(
        report.contains(
            "Give \"Direction.IN\" the id of an existing vertex (check it with g.v(id)), or create \
             the vertex first, for example g.merge_v(#{\"T.id\": \"1\"})."
        ),
        "{report}"
    );

    let err = graph
        .traversal_mut()
        .merge_e(merge_map(&[
            ("Direction.OUT", "Merge.outV".into()),
            ("Direction.IN", "2".into()),
        ]))
        .next()
        .expect_err("no option");
    let report = format_diagnostic(&err);
    assert!(
        report.contains("there is no option(Merge.outV, ..)"),
        "{report}"
    );
    assert!(
        report.contains(
            "\"Direction.OUT\": Merge.outV is resolved from option(Merge.outV, ..), which takes a \
             vertex search map such as #{\"T.id\": \"1\"} or a traversal yielding a vertex"
        ),
        "{report}"
    );

    let err = graph
        .traversal_mut()
        .merge_e(merge_map(&[
            ("Direction.OUT", "Merge.outV".into()),
            ("Direction.IN", "2".into()),
        ]))
        .option_merge(Merge::OutV, merge_map(&[("T.label", "person".into())]))
        .next()
        .expect_err("ambiguous");
    let report = format_diagnostic(&err);
    assert!(
        report.contains("matched 4 vertices; it must identify exactly one vertex"),
        "{report}"
    );
    assert!(
        report.contains(
            "The option(Merge.outV, map) map is a vertex search that must find exactly one \
             vertex: name it by id, for example option(Merge.outV, #{\"T.id\": \"1\"})"
        ),
        "{report}"
    );
}

/// The `mergeE()`-specific malformed maps each get their own advice.
#[test]
fn invalid_merge_map_help_covers_edge_maps() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    type MergeCase = (
        ElementProperty,
        Option<(Merge, ElementProperty)>,
        &'static str,
    );
    let cases: [MergeCase; 3] = [
        (
            merge_map(&[("Direction.BOTH", "1".into())]),
            None,
            "write \"Direction.OUT\" and \"Direction.IN\" instead of \"Direction.BOTH\"",
        ),
        (
            merge_map(&[("Direction.OUT", "Merge.inV".into())]),
            None,
            "Each end takes its own placeholder: \"Direction.OUT\": Merge.outV and \"Direction.IN\": Merge.inV",
        ),
        (
            merge_map(&[("Direction.OUT", "Merge.outV".into())]),
            Some((Merge::OutV, merge_map(&[("Direction.IN", "1".into())]))),
            "The option(Merge.outV|inV, map) map is a vertex search (\"T.id\", \"T.label\" and properties)",
        ),
    ];
    for (map, option, advice) in cases {
        let mut traversal = graph.traversal_mut().merge_e(map);
        if let Some((token, value)) = option {
            traversal = traversal.option_merge(token, value);
        }
        let err = traversal.next().expect_err("invalid edge map");
        assert!(
            matches!(err.root_cause(), TraverserError::InvalidMergeMap { .. }),
            "{err}"
        );
        let report = format_diagnostic(&err);
        assert!(report.contains(advice), "{report}");
        // The general part names the edge keys and gives a mergeE() example, not a mergeV() one.
        assert!(
            report.contains(
                "A merge_e() map holds property keys plus the reserved keys \"T.id\", \
                 \"T.label\", \"Direction.OUT\" and \"Direction.IN\", for example \
                 g.merge_e(#{\"T.label\": \"knows\", \"Direction.OUT\": \"1\", \"Direction.IN\": \
                 \"2\"}).option(Merge.onCreate, #{\"weight\": 0.5})"
            ),
            "{report}"
        );
    }

    let err = graph
        .traversal_mut()
        .merge_v(ElementProperty::Null)
        .option_merge(Merge::OutV, merge_map(&[("T.id", "1".into())]))
        .next()
        .expect_err("outV on mergeV");
    let report = format_diagnostic(&err);
    assert!(
        report.contains(
            "option(Merge.outV|inV, ..) belongs to merge_e(), for example \
             g.merge_e(#{\"T.label\": \"knows\", \"Direction.OUT\": Merge.outV, \"Direction.IN\": \
             \"2\"}).option(Merge.outV, #{\"T.id\": \"1\"})"
        ),
        "{report}"
    );
}

/// `mergeE()` override and cardinality errors give edge examples.
#[test]
fn merge_e_override_and_cardinality_help_use_edge_examples() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let err = graph
        .traversal_mut()
        .merge_e(merge_map(&[("Direction.OUT", "1".into())]))
        .option_merge(Merge::OnCreate, merge_map(&[("Direction.OUT", "2".into())]))
        .next()
        .expect_err("override");
    let report = format_diagnostic(&err);
    assert!(
        report.contains("Direction.OUT is \"1\" in merge_e() but \"2\" in option(onCreate)"),
        "{report}"
    );
    assert!(
        report.contains(
            "for example g.merge_e(#{\"T.label\": \"knows\", \"Direction.OUT\": \"1\", \
             \"Direction.IN\": \"2\"}).option(Merge.onCreate, #{\"weight\": 0.5})"
        ),
        "{report}"
    );

    let err = graph
        .traversal_mut()
        .merge_e(merge_map(&[("Direction.OUT", "1".into())]))
        .option_merge_with(
            Merge::OnMatch,
            merge_map(&[("w", 1.into())]),
            Cardinality::Single,
        )
        .next()
        .expect_err("cardinality");
    let report = format_diagnostic(&err);
    assert!(
        report.contains(
            "Edge properties have no cardinality: drop the Cardinality argument of option() and \
             any Cardinality.single(v) wrapper in the merge_e() maps"
        ),
        "{report}"
    );
}

/// A duplicate edge id names `mergeE()` as the get-or-create form.
#[test]
fn edge_id_unique_constraint_help_names_merge_e() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let err = graph
        .traversal_mut()
        .v("1")
        .add_e("x")
        .to("2")
        .property_id("0")
        .next()
        .expect_err("duplicate edge id");
    let report = format_diagnostic(&err);
    assert!(
        report.contains(
            "To get the edge with this id if it exists and create it otherwise, use mergeE, for \
             example g.merge_e(#{\"T.id\": \"7\"}).option(Merge.onCreate, #{\"T.label\": \"knows\", \
             \"Direction.OUT\": \"1\", \"Direction.IN\": \"2\"})."
        ),
        "{report}"
    );
}

/// Renders the diagnostic of a failed `math()` traversal on the modern graph.
fn math_report(
    build: impl FnOnce(
        GraphTraversalSource<'_, &TraversalGraph>,
    ) -> Result<Vec<ElementProperty>, Box<TraverserError>>,
) -> String {
    let graph = GraphSource::tinkerpop_modern();
    let locked = graph.read();
    let err = build(locked.traversal()).expect_err("math() must fail");
    format_diagnostic(&err)
}

#[test]
fn math_syntax_error_underlines_the_token_before_execution() {
    let report = math_report(|g| g.v(None).has("name", "nobody").math("2 +* 3").to_list());
    assert!(
        report.contains("math(\"2 +* 3\") failed at column 4: unexpected '*'"),
        "{report}"
    );
    assert!(
        report.contains("  equation: math(\"2 +* 3\")\n                     ^"),
        "the caret must sit under '*' in: {report}"
    );
    assert!(
        report.contains(
            "math() evaluates an equation like TinkerPop does: numbers, the operators + - * / % \
             and ^ (power), parentheses, the functions abs, acos,"
        ),
        "{report}"
    );
    assert!(
        report.contains(
            "Fix the equation at the marked position, for example \
             g.v().values(\"age\").math(\"(_ + 1) * 2\")."
        ),
        "{report}"
    );
}

#[test]
fn math_unknown_function_lists_close_names() {
    let report = math_report(|g| g.v("1").values("age").math("sqr(_)").to_list());
    assert!(
        report.contains("  equation: math(\"sqr(_)\")\n                  ^^^"),
        "{report}"
    );
    assert!(
        report.contains(
            "Help: 'sqr' is not a math() function; did you mean sqrt (square root), sin (sine \
             (radians))? For example g.v(\"1\").values(\"age\").math(\"sqrt(_)\")."
        ),
        "{report}"
    );
}

#[test]
fn math_unknown_variable_names_every_scope_and_the_nested_form() {
    let report = math_report(|g| g.v("1").values("age").math("_ + x").to_list());
    assert!(
        report.contains(
            "variable 'x' is not '_', a key of the current map, a side effect or a step label"
        ),
        "{report}"
    );
    assert!(
        report.contains(
            "Help: math() reads a variable other than _ from, in this order, a key of the current \
             map (project(\"x\", ...), value_map(), element_map()), a side effect \
             (g.with_side_effect(\"x\", 10), aggregate(\"x\")) or a step label (as(\"x\")), and \
             'x' is none of these for this traverser. Label the value it should read, for example \
             g.v(\"1\").values(\"age\").as(\"x\").math(\"_ + x\"), or declare it as a side effect, \
             for example g.with_side_effect(\"x\", 10).v().values(\"age\").math(\"_ + x\"). To \
             read a key of the current value itself, use the nested form _.x (a Graphersal \
             extension), for example g.v().element_map().math(\"_ + _.x\")."
        ),
        "{report}"
    );
}

#[test]
fn math_value_points_to_underscore() {
    let report = math_report(|g| g.v("1").values("age").math("value * 2").to_list());
    assert!(
        report.contains(
            "Help: The current value is named _ in math(), as in TinkerPop; value is not a name \
             for it. For example g.v(\"1\").values(\"age\").math(\"_ * 2\")."
        ),
        "{report}"
    );
}

#[test]
fn math_non_numeric_value_suggests_as_number() {
    let report = math_report(|g| g.v("1").values("name").math("_ + 1").to_list());
    assert!(
        report.contains("the variable _ for math() must resolve to a number, but it is a string"),
        "{report}"
    );
    assert!(
        report.contains(
            "Help: math() computes with numbers only, and _ is a string. Convert the value with \
             as_number(GType.DOUBLE) first, for example \
             g.v(\"1\").values(\"name\").as_number(GType.DOUBLE).math(\"_ + 1\"), or inside the \
             by() that reads it, for example by(__.values(\"age\").as_number(GType.DOUBLE))."
        ),
        "{report}"
    );
}

#[test]
fn math_null_value_suggests_filtering() {
    let mut map = PropertyMap::default();
    map.insert("a", ElementProperty::Null);
    let report = math_report(|g| {
        g.inject(vec![ElementProperty::Object(Box::new(map))])
            .math("_.a + 1")
            .to_list()
    });
    assert!(
        report.contains(
            "Help: _.a is null, and math() computes with numbers only. Keep only traversers that \
             have the value before math(), for example \
             g.v().has(\"age\").math(\"_ + 1\").by(\"age\"), or replace a missing value with a \
             default inside the by(), for example \
             by(__.coalesce(__.values(\"age\"), __.constant(0)))."
        ),
        "{report}"
    );
}

#[test]
fn math_bad_path_names_the_available_keys() {
    let report = math_report(|g| {
        g.v("1")
            .element_map(None::<()>)
            .math("_.agee * 2")
            .to_list()
    });
    assert!(
        report.contains(
            "_.agee leads to no value: no key 'agee'; the available keys are: id, label, name, age"
        ),
        "{report}"
    );
    assert!(
        report.contains("  equation: math(\"_.agee * 2\")\n                  ^^^^^^"),
        "{report}"
    );
    assert!(
        report.contains(
            "Help: Nested access such as _.agee is a Graphersal extension of math(): after a \
             variable, .key or [\"key\"] reads a key of a map"
        ),
        "{report}"
    );
}

#[test]
fn math_division_by_zero_suggests_filtering_the_divisor() {
    let report = math_report(|g| g.v("1").values("age").math("_ / 0").to_list());
    assert!(
        report.contains("failed at column 3: Division by zero!"),
        "{report}"
    );
    assert!(
        report.contains(
            "Help: math() follows TinkerPop: a zero divisor of / or % (or cot(x) \
             where tan(x) is 0) is an error, not infinity. Keep only traversers with a non-zero \
             divisor before math(), for example g.v().project(\"a\", \"b\").by(\"age\")\
             .by(__.out().count()).where(__.select(\"b\").is(P.neq(0))).math(\"a / b\")."
        ),
        "{report}"
    );
}

#[test]
fn timeout_help_suggests_a_barrier_between_expansions() {
    let graph = GraphSource::large_110k();
    let lock = graph.read();
    let err = lock
        .traversal()
        .with("evaluationTimeout", 1)
        .v(None)
        .both(None)
        .both(None)
        .both(None)
        .both(None)
        .both(None)
        .count()
        .next()
        .expect_err("5 hops of both() on the large graph exceed 1 ms");
    let help = err.help().expect("help");
    assert!(
        help.contains(
            "Merge duplicate traversers: barrier() merges equal traversers so later steps \
             process each distinct one once, for example \
             g.v().both().barrier().both().barrier().both().barrier().both().barrier().both().count()."
        ),
        "{help}"
    );
    // The suggested query runs.
    assert!(
        lock.traversal()
            .v(None)
            .limit(2)
            .both(None)
            .barrier()
            .both(None)
            .count()
            .next()
            .is_ok()
    );
}

#[test]
fn timeout_help_without_two_expansions_uses_the_example_query() {
    let graph = GraphSource::large_110k();
    let lock = graph.read();
    let err = lock
        .traversal()
        .with("evaluationTimeout", 1)
        .v(None)
        .repeat(__::both(None))
        .times(8)
        .count()
        .next()
        .expect_err("8 hops of both() on the large graph exceed 1 ms");
    let help = err.help().expect("help");
    assert!(
        help.contains("for example g.v().out().barrier().out().barrier().out().count()."),
        "{help}"
    );
}

/// `V()`/`E()`/`hasId()` reject a map that is not an element, with advice on the accepted forms.
#[cfg(feature = "script")]
#[test]
fn id_argument_mismatch_help_lists_the_accepted_forms() {
    let graph = std::sync::Arc::new(GraphSource::tinkerpop_modern());
    let err = crate::script_host::eval_value(graph, "g.E(#{a: 1}).toList()")
        .expect_err("a map without an id is not an id")
        .to_string();
    assert!(
        err.contains(
            "v(), e() and has_id() take ids (strings, numbers, UUIDs), elements returned by \
             next()/to_list(), or lists of them. For example: let e = g.e(\"11\").next(); \
             g.e(e).to_list() or g.v([\"1\", \"2\"]).to_list(). A map is accepted only as an \
             element, that is, with an `id` key."
        ),
        "{err}"
    );
}

/// A second `by()` on `aggregate()`, `store()`, `group_count()` and `valueMap()`.
#[test]
fn a_second_by_help_names_the_single_by_form() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let err = lock
        .traversal()
        .v(None)
        .aggregate("x")
        .by("name")
        .by("age")
        .cap("x")
        .to_list()
        .expect_err("a second by()");
    assert_eq!(
        err.root_cause().to_string(),
        "`aggregate()` cannot use its by() #2: aggregate() and store() take a single by()"
    );
    assert_eq!(
        err.help().expect("help"),
        "aggregate() collects one projection of each traverser, so it takes a single by(), for \
         example g.v().aggregate(\"x\").by(\"name\").cap(\"x\"). To collect several values per \
         traverser, project them in one traversal, for example \
         g.v().aggregate(\"x\").by(__.values(\"name\", \"age\").fold()).cap(\"x\")."
    );
    let err = lock
        .traversal()
        .v(None)
        .group_count()
        .by("name")
        .by("age")
        .to_list()
        .expect_err("a second by()");
    assert!(err.help().expect("help").contains(
        "group_count() counts the elements per key, so it takes a single by(), the key; there is \
         no value projection to give."
    ));
    let err = lock
        .traversal()
        .v(None)
        .value_map(vec!["name"])
        .by(__::unfold())
        .by(__::unfold())
        .to_list()
        .expect_err("a second by()");
    assert!(
        err.help()
            .expect("help")
            .contains("value_map() takes a single by(): it is applied to every property's list.")
    );
}

/// `inject()` as a direct step of a `repeat()` body.
#[test]
fn repeat_inject_help_suggests_union_or_injecting_before_the_loop() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let err = lock
        .traversal()
        .v(None)
        .repeat(__::inject(vec!["y"]))
        .times(2)
        .to_list()
        .expect_err("inject in a repeat body");
    assert!(
        matches!(err.root_cause(), TraverserError::RepeatInject { .. }),
        "{err}"
    );
    let help = err.help().expect("help");
    assert!(
        help.contains("union(__.identity(), __.inject(\"y\"))"),
        "{help}"
    );
    assert!(help.contains("Inject before the loop"), "{help}");
}

#[test]
fn choose_traversal_option_key_help_names_predicates_and_branch() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let err = lock
        .traversal()
        .v(None)
        .choose_switch(__::values("name"))
        .option_traversal(__::is("marko"), __::values("age"))
        .to_list()
        .expect_err("a traversal option key is invalid for choose()");
    assert!(
        matches!(err.root_cause(), TraverserError::InvalidOption { .. }),
        "{err}"
    );
    let help = err.help().expect("help");
    assert!(
        help.contains("option(P.gt(30), __.constant(\"old\"))"),
        "{help}"
    );
    assert!(help.contains("use branch()"), "{help}");
    assert!(
        help.contains("passes the traverser through unchanged"),
        "{help}"
    );
}

/// A string step on a graph element names `as_string()`, which renders an element as text.
#[test]
fn a_string_step_on_an_element_names_as_string() {
    let graph = GraphSource::tinkerpop_modern();
    let locked = graph.read();
    let err = locked
        .traversal()
        .v(None)
        .to_upper()
        .next()
        .expect_err("a vertex is not a string");
    let report = format_diagnostic(&err);
    assert!(
        report.contains(r#"g.v().as_string() gives "v[1]""#),
        "{report}"
    );
    assert!(
        report.contains(r#"g.v().values("name").to_upper()"#),
        "{report}"
    );
}

/// `tree("a")` over a key that already holds a list names the clash and a fix.
#[test]
fn tree_over_a_non_tree_side_effect_names_the_clash() {
    let graph = GraphSource::tinkerpop_modern();
    let locked = graph.read();
    let err = locked
        .traversal()
        .v(None)
        .aggregate("a")
        .tree("a")
        .to_list()
        .expect_err("a list is not a tree");
    assert!(
        matches!(err.root_cause(), TraverserError::QueryExecution { .. }),
        "{err}"
    );
    let help = err.help().expect("help");
    assert!(
        help.contains("side effect 'a' already holds a list"),
        "{help}"
    );
    assert!(help.contains(r#"tree("a") needs an unused key"#), "{help}");
    assert!(help.contains(r#".tree("t").cap("t")"#), "{help}");
}

/// A map with a non-string key cannot be stored: the help names the string-key form and `to_json()`.
#[test]
fn storing_a_map_with_a_non_string_key_names_to_json() {
    let graph = GraphSource::tinkerpop_modern();
    let map = ElementProperty::from_entries([(
        ElementProperty::from(29i64),
        ElementProperty::from(1i64),
    )]);
    let err = graph
        .write()
        .traversal_mut()
        .v("1")
        .property("ages", map)
        .next()
        .expect_err("a map with a non-string key is not a property value");
    assert_eq!(
        err.root_cause().help().expect("help"),
        "A property value that is a map must have string keys, because a stored map is a plain \
         object. Store the map under string keys, for example property(\"ages\", #{\"29\": 1}), or \
         keep the data in the stream and show a map with a non-string key with to_json(), for \
         example g.v().group_count().by(\"age\").to_json()."
    );
}

/// `as_bool()` and `as_number(GType::LONG)` are advised in a cast failure, so the help also says
/// what they really convert.
#[test]
fn cast_help_states_the_rules_of_as_bool_and_as_number() {
    let graph = GraphSource::tinkerpop_modern();
    let locked = graph.read();
    let err = locked
        .traversal()
        .inject(vec!["maybe"])
        .as_bool()
        .next()
        .expect_err("an unknown text is not a boolean");
    let report = format_diagnostic(&err);
    assert!(
        report.contains(
            "as_bool() turns 0 (and NaN) into false, any other number into true, and the texts \
             true/false in any letter case; any other text is an error."
        ),
        "{report}"
    );
    let err = locked
        .traversal()
        .inject(vec!["not a number"])
        .sum()
        .next()
        .expect_err("a string must not be aggregated");
    let report = format_diagnostic(&err);
    assert!(
        report.contains("as_number(GType.LONG) truncates a float toward zero."),
        "{report}"
    );
}

/// `from()`/`to()` of the path steps: the advice after `out()` names `path()`, an absent label gets
/// the path-label help, and a label list is a rejected modulator.
#[test]
fn path_window_diagnostics_name_the_path_steps() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();

    let err = lock
        .traversal()
        .v(None)
        .out(None)
        .from("a")
        .to_list()
        .expect_err("out takes no from");
    let help = err.help().expect("help");
    assert!(
        help.contains("path(), simple_path() and cyclic_path() take them as a window"),
        "{help}"
    );
    assert!(
        help.contains("g.v().as(\"a\").out().as(\"b\").path().from(\"a\").to(\"b\")"),
        "{help}"
    );
    assert!(
        help.contains("add_e() accepts from()/to()"),
        "the add_e advice stays: {help}"
    );

    let err = lock
        .traversal()
        .v(None)
        .out(None)
        .path()
        .from("nowhere")
        .to_list()
        .expect_err("no position carries the label");
    assert_eq!(
        err.root_cause().to_string(),
        "Property 'nowhere' not found on element type 'path'"
    );
    let help = err.help().expect("help");
    assert!(
        help.contains("No step label 'nowhere' is set on the path"),
        "{help}"
    );

    let err = lock
        .traversal()
        .v(None)
        .as_("a")
        .out(None)
        .path()
        .from(vec!["a".to_string(), "b".to_string()])
        .to_list()
        .expect_err("a label list is not a window bound");
    assert_eq!(
        err.root_cause().to_string(),
        "`path()` cannot use its from()/to() #1: from()/to() of a path step takes exactly one step label"
    );
    let help = err.help().expect("help");
    assert!(help.contains("takes exactly one step label"), "{help}");
    assert!(help.contains("path().from(\"a\").to(\"b\")"), "{help}");
}

/// `by()` ring help names the two filter steps next to `path()`.
#[test]
fn ring_help_names_simple_and_cyclic_path() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let err = lock
        .traversal()
        .v(None)
        .both(None)
        .simple_path()
        .by(graphersal::prelude::Order::Desc)
        .to_list()
        .expect_err("an order is not a projection");
    let help = err.help().expect("help");
    assert!(
        help.contains("simple_path() applies its by() modulators round-robin"),
        "{help}"
    );
    assert!(
        help.contains("g.v(\"1\").both().both().simple_path().by(\"name\")"),
        "{help}"
    );
}

/// `select(Column.keys)`/`select(Column.values)` of a path and `where(P).by()` are named by the
/// help texts.
#[test]
fn where_by_and_path_keys_help() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let err = lock
        .traversal()
        .v(None)
        .values("age")
        .select(Column::Values)
        .to_list()
        .expect_err("a number has no values column");
    let help = err.help().expect("help");
    assert!(
        help.contains(r#"select(Column.keys) of a path() lists the step labels of each position, for example g.v().as("a").out().as("b").path().select(Column.keys)"#),
        "{help}"
    );
    assert!(
        help.contains(r#"select(Column.values) of a path() lists its objects, for example g.v("1").out().path().select(Column.values)"#),
        "{help}"
    );

    let err = lock
        .traversal()
        .v(None)
        .as_("a")
        .where_p(P::Gt("a"))
        .by(graphersal::prelude::Order::Desc)
        .to_list()
        .expect_err("an order is not a projection");
    let help = err.help().expect("help");
    assert!(
        help.contains("where() applies its by() modulators round-robin"),
        "{help}"
    );
    assert!(
        help.contains(r#"g.v("1").as("a").out().where(P.gt("a")).by("age")"#),
        "{help}"
    );

    let err = lock
        .traversal()
        .v(None)
        .count()
        .by("x")
        .to_list()
        .expect_err("count takes no by");
    let help = err.help().expect("help");
    assert!(help.contains("cyclic_path(), where(), tree()"), "{help}");
}

/// `add_e()` takes a label string or a traversal; any other argument is a mismatch whose help
/// names the traversal form (rule 6).
#[cfg(feature = "script")]
#[test]
fn add_e_argument_mismatch_help_names_the_traversal_label() {
    use crate::script_host::eval;

    let graph = std::sync::Arc::new(GraphSource::tinkerpop_modern());
    let report = eval(graph, r#"g.v("1").add_e(5).to("2").to_list()"#)
        .expect_err("a number is no label")
        .to_string();
    assert!(
        report.contains(
            "Error: Argument mismatch for step 'add_e': expected a label string or a traversal, got "
        ),
        "{report}"
    );
    assert!(
        report.contains(
            "Help: add_e() takes the edge label as a string, or as a traversal whose first result \
             is the label, for example g.v(\"1\").add_e(\"knows\").to(\"2\") or \
             g.v().as(\"a\").out_e().as(\"b\").in_v().add_e(__.select(\"b\").label()).to(\"a\"). \
             A list of labels is not accepted."
        ),
        "{report}"
    );
}

#[cfg(feature = "script")]
#[test]
fn choose_and_branch_selector_mismatch_names_the_accepted_forms() {
    let graph = std::sync::Arc::new(GraphSource::tinkerpop_modern());
    for script in [
        r#"g.V().choose("x")"#,
        r#"g.V().branch(1)"#,
        r#"g.V().choose(1, __.constant(1))"#,
    ] {
        let err = crate::script_host::eval_value(graph.clone(), script)
            .expect_err("a scalar is not a selector")
            .to_string();
        assert!(
            err.contains("g.v().choose(T.label).option(\"person\""),
            "{script}: {err}"
        );
        assert!(
            err.contains("g.v().values(\"age\").choose(P.gt(30), __.constant(\"old\"), __.constant(\"young\"))"),
            "{script}: {err}"
        );
    }
}

/// `remove_property()` argument mismatch help shows the accepted forms and the null relation.
#[cfg(feature = "script")]
#[test]
fn remove_property_argument_mismatch_help_shows_the_forms() {
    use crate::script_host::eval;

    let graph = std::sync::Arc::new(GraphSource::tinkerpop_modern());
    let rendered = eval(graph, "g.v().remove_property(5).to_list()")
        .expect_err("a number is no key")
        .to_string();
    assert!(
        rendered.contains("Argument mismatch for step 'remove_property'"),
        "{rendered}"
    );
    assert!(
        rendered.contains(r#"g.v().has_label("person").remove_property("age")"#),
        "{rendered}"
    );
    assert!(
        rendered.contains(r#"remove_property(["age", "name"])"#),
        "{rendered}"
    );
    assert!(
        rendered.contains("property(k, ()) stores a null"),
        "{rendered}"
    );
}

#[test]
fn operator_failed_help_in_a_sack_names_the_by_operand_and_the_coercion() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let err = lock
        .traversal()
        .with_sack("a")
        .v(None)
        .sack_with(Operator::Sum)
        .by("age")
        .to_list()
        .expect_err("a text sack cannot be summed");
    let help = err.help().expect("help");
    for needle in [
        "g.with_sack(0).v().out_e().sack(Operator.sum).by(\"weight\")",
        "g.with_sack(0).v().sack(Operator.sum).by(__.values(\"age\").as_number())",
        "as_number()",
    ] {
        assert!(help.contains(needle), "{needle} missing from: {help}");
    }
    assert!(!help.contains("normSack"), "{help}");
}

#[test]
fn norm_sack_failure_help_names_barrier_norm_sack() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let err = lock
        .traversal()
        .with_sack("a")
        .v(None)
        .barrier_with(Barrier::NormSack)
        .to_list()
        .expect_err("a text sack cannot be normalised");
    let help = err.help().expect("help");
    assert!(
        help.contains("g.with_sack(1.0).v().out().barrier(Barrier.normSack).sack()"),
        "{help}"
    );
}

#[test]
fn a_second_sack_by_help_names_the_single_operand_form() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let err = lock
        .traversal()
        .v(None)
        .sack_with(Operator::Assign)
        .by("age")
        .by("name")
        .to_list()
        .expect_err("sack takes one by()");
    let help = err.help().expect("help");
    assert!(
        help.contains("g.with_sack(0).v().out_e().sack(Operator.sum).by(\"weight\")"),
        "{help}"
    );
    assert!(help.contains("by(__.values(\"age\").sum())"), "{help}");
}

#[test]
fn operator_failed_help_names_the_reducer_forms() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let err = lock
        .traversal()
        .v(None)
        .values("age")
        .fold_with("", Operator::Sum)
        .to_list()
        .expect_err("a text seed cannot be summed");
    let help = err.help().expect("help");
    for needle in [
        "g.v().values(\"age\").fold(0, Operator.sum)",
        "g.with_side_effect(\"a\", 0, Operator.sum).v().aggregate(\"a\").by(\"age\").cap(\"a\")",
    ] {
        assert!(help.contains(needle), "{needle} missing from: {help}");
    }
    // The same text through a reducer side effect.
    let err = lock
        .traversal()
        .with_side_effect_reduced("a", "x", Operator::Sum)
        .v(None)
        .aggregate("a")
        .by("age")
        .cap("a")
        .to_list()
        .expect_err("a text seed cannot be summed");
    assert!(err.help().expect("help").contains("Operator.sum"), "{err}");
}

#[test]
fn side_effect_key_helps_name_the_reducer_declaration() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let err = lock
        .traversal()
        .v(None)
        .cap("nope")
        .to_list()
        .expect_err("an undeclared key");
    let help = err.help().expect("help");
    assert!(
        help.contains("g.with_side_effect(\"nope\", 0, Operator.sum).v().aggregate(\"nope\").by(\"age\").cap(\"nope\")"),
        "{help}"
    );
    let err = lock
        .traversal()
        .v(None)
        .aggregate("a")
        .tree("a")
        .to_list()
        .expect_err("a list is not a tree");
    assert!(
        err.help()
            .expect("help")
            .contains("with_side_effect(\"a\", 0, Operator.sum)"),
        "{err}"
    );
}

#[test]
fn reserved_parameter_help_lists_token_classes() {
    use std::collections::HashMap;
    use std::sync::Arc;
    let err = crate::script_host::eval_with_params(
        Arc::new(GraphSource::tinkerpop_modern()),
        "g.v().count().next()",
        HashMap::from([("Order".to_string(), ElementProperty::Boolean(true))]),
    )
    .expect_err("Order is a reserved token name");
    let text = format!("{err:?}{err}");
    assert!(text.contains("reserved by the script scope"), "{text}");
}

#[test]
fn property_path_write_help_texts_name_the_fix() {
    use graphersal::prelude::JsonPath;
    let graph = GraphSource::empty();
    {
        let mut writable = graph.write();
        writable
            .traversal_mut()
            .add_v("p")
            .property("scalar", 5)
            .property(
                "tags",
                ElementProperty::Array(vec![ElementProperty::from("a")]),
            )
            .next()
            .unwrap();
    }
    let path = |text: &str| text.parse::<JsonPath>().unwrap();
    let report = |key: &str| {
        let mut writable = graph.write();
        let err = writable
            .traversal_mut()
            .v(None)
            .property(path(key), 1)
            .next()
            .expect_err("write must fail");
        let root = err.root_cause();
        (root.to_string(), root.help().expect("help"))
    };

    let (message, help) = report("tags[5]");
    assert_eq!(
        message,
        "Cannot write property path '$.tags[5]': index 5 is out of range for the array '$.tags' of length 1"
    );
    assert_eq!(
        help,
        "Use an index inside the array, [len] to append as the last segment, or a negative index [-1] \
         for the last element; arrays are never padded with null. Written path: $.tags[5]."
    );

    let (message, help) = report("tags.x");
    assert_eq!(
        message,
        "Cannot write property path '$.tags.x': '$.tags' is an array, not an object, so a key cannot be set on it"
    );
    assert_eq!(
        help,
        "A key segment needs an object. Read the current value first, for example \
         g.v().values(jpath(\"$.tags.x\")), or write a whole object at a shorter path, for example \
         property(jpath(\"meta\"), #{\"k\": 1}). Missing keys are created, existing scalars and arrays are \
         never overwritten by a deeper path."
    );

    let (message, help) = report("scalar[0]");
    assert_eq!(
        message,
        "Cannot write property path '$.scalar[0]': '$.scalar' is a number, not an array, so an index cannot be used on it"
    );
    assert_eq!(
        help,
        "An index segment needs an array. Use a key segment for an object, for example \
         jpath(\"meta.tags\") instead of jpath(\"meta[0]\"), or write the array first with \
         property(jpath(\"meta.tags\"), [\"x\"]). Written path: $.scalar[0]."
    );

    let (message, help) = report("missing.list[0]");
    assert_eq!(
        message,
        "Cannot write property path '$.missing.list[0]': the array '$.missing.list' does not exist, and an index segment never creates one"
    );
    assert_eq!(
        help,
        "Create the array first, for example property(jpath(\"meta.tags\"), [\"x\"]), then address its \
         elements. Only key segments create missing objects. Written path: $.missing.list[0]."
    );

    let (message, help) = report("[0]");
    assert_eq!(
        message,
        "Cannot write property path '$[0]': the first segment must name the property, not an index"
    );
    assert_eq!(
        help,
        "Start the path with the property name, for example property(jpath(\"meta.tags[0]\"), \"x\"); \
         '$[0]' begins with an index."
    );

    let mut writable = graph.write();
    let err = writable
        .traversal_mut()
        .v(None)
        .property_with(Cardinality::List, path("tags[0]"), 1)
        .next()
        .expect_err("cardinality with a path");
    let root = err.root_cause();
    assert_eq!(
        root.to_string(),
        "Cannot write property path '$.tags[0]': property(Cardinality, jpath(..), value) is not supported"
    );
    assert_eq!(
        root.help().expect("help"),
        "Graphersal stores one value per property; write a nested value with \
         property(jpath(\"meta.tags[0]\"), \"x\") without a Cardinality, or build the whole value and \
         write it with property(Cardinality.single, \"meta\", value)."
    );
}

/// `remove_property()` accepts `jpath(..)`: the help names it and the removal rules, and
/// `properties(jpath(..)).drop()` points at it.
#[cfg(feature = "script")]
#[test]
fn remove_property_jpath_help_and_path_property_drop_help() {
    use crate::script_host::eval;

    let graph = std::sync::Arc::new(GraphSource::tinkerpop_modern());
    let rendered = eval(graph.clone(), "g.v().remove_property(5).to_list()")
        .expect_err("a number is no key")
        .to_string();
    assert!(
        rendered.contains("strings or jpath(..) values"),
        "{rendered}"
    );
    assert!(
        rendered.contains(r#"g.v().remove_property(jpath("meta.tags[0]"))"#),
        "{rendered}"
    );
    assert!(rendered.contains("a missing path is a no-op"), "{rendered}");

    let rendered = eval(
        graph,
        r#"g.v("1").properties(jpath("name")).drop().to_list()"#,
    )
    .expect_err("a path property cannot be dropped")
    .to_string();
    assert!(rendered.contains("got type 'pathProperty'"), "{rendered}");
    assert!(
        rendered.contains("holds no reference to the element"),
        "{rendered}"
    );
    assert!(
        rendered.contains(r#"remove_property(jpath("meta.tags[0]"))"#),
        "{rendered}"
    );
}

/// Nested schema errors (Task 10 of the jpath work) name the full canonical path and their help
/// reads that location with `jpath(..)`, never with a literal key.
#[cfg(feature = "script")]
#[test]
fn nested_schema_error_help_names_the_path_and_reads_it_with_jpath() {
    use crate::script_host::eval;

    let schema = crate::schema_support::schema(
        r#"{"mode": "closed", "vertices": {"P": {"schema": {"type": "object", "properties": {
            "meta": {"type": ["object", "null"], "properties": {"k": {"type": "integer"}},
                     "required": ["k"]}}}}}}"#,
    );
    let graph = std::sync::Arc::new(GraphSource::empty());
    eval(
        graph.clone(),
        r#"g.addV("P").property(T.id, "a").property_json(#{meta: #{k: 1}}).toList()"#,
    )
    .expect("setup");
    graph.write().set_schema(schema).unwrap();

    let mismatch = eval(
        graph.clone(),
        r#"g.V("a").property(jpath("meta.k"), "x").toList()"#,
    )
    .expect_err("type")
    .to_string();
    assert!(mismatch.contains("$.meta.k"), "{mismatch}");
    assert!(
        mismatch.contains(r#"values(jpath("$.meta.k")).as_number(GType.LONG)"#),
        "{mismatch}"
    );
    assert!(!mismatch.contains(r#"values("$.meta.k")"#), "{mismatch}");

    let undeclared = eval(
        graph.clone(),
        r#"g.V("a").property(jpath("meta.extra"), 1).toList()"#,
    )
    .expect_err("undeclared")
    .to_string();
    assert!(
        undeclared.contains("Undeclared property '$.meta.extra'"),
        "{undeclared}"
    );
    assert!(
        undeclared
            .contains(r#"Declare '$.meta.extra' among the "properties" of its enclosing object"#),
        "{undeclared}"
    );
    assert!(
        undeclared.contains(r#""additionalProperties": true"#),
        "{undeclared}"
    );

    let required = eval(
        graph,
        r#"g.V("a").remove_property(jpath("meta.k")).toList()"#,
    )
    .expect_err("required")
    .to_string();
    assert!(
        required.contains("Missing required property '$.meta.k'"),
        "{required}"
    );
    assert!(
        required.contains(
            "A schema \"default\" below the top level is an annotation: it does not fill a \
             missing nested key (only a label's top-level properties get their default when an \
             element is created)."
        ),
        "{required}"
    );
    assert!(
        required.contains(r#"property(jpath("$.meta.k"), value)"#),
        "{required}"
    );
    assert!(
        required.contains("cannot be removed with remove_property(jpath(..))"),
        "{required}"
    );
}

/// Task 11 of the jpath work: every help() where a jpath key is the natural fix names it with a
/// runnable example, and the grammar help describes the RFC 9535 singular query.
#[cfg(all(feature = "script", feature = "serde"))]
#[test]
fn jpath_help_audit_texts_name_the_jpath_fix() {
    use crate::script_host::eval;

    let graph = std::sync::Arc::new(GraphSource::tinkerpop_modern());

    // InvalidJsonPath raised by jpath(..) itself: grammar, bracket form, negative index, literal keys.
    let rendered = eval(graph.clone(), r#"jpath("a..b")"#)
        .expect_err("an empty segment is not a path")
        .to_string();
    for needle in [
        "singular JSONPath (RFC 9535)",
        r#"jpath("$.meta.tags[-1]")"#,
        r#"jpath("meta['a.b'].k")"#,
        r#"g.v().values(jpath("meta.k"))"#,
        r#"g.v().has(jpath("meta.k"), 1)"#,
        "Recursive descent ('..'), wildcards ('*'), slices and filter expressions",
        r#"values("a.b") is never parsed"#,
    ] {
        assert!(
            rendered.contains(needle),
            "missing {needle:?} in: {rendered}"
        );
    }

    // A non-key argument to a key-taking step.
    {
        let query = r#"g.v().property(5, 1).to_list()"#;
        let rendered = eval(graph.clone(), query)
            .expect_err("a number is no key")
            .to_string();
        assert!(
            rendered.contains(r#"g.v().property(jpath("meta.tags[0]"), "x")"#)
                && rendered
                    .contains(r#"A dotted string such as "meta.k" is never parsed as a path"#),
            "{rendered}"
        );
    }

    // valueMap names jpath.
    let rendered = eval(graph.clone(), "g.v().valueMap(5).to_list()")
        .expect_err("a number is no key")
        .to_string();
    assert!(
        rendered.contains(r#"g.v().value_map(jpath("meta.k"))"#),
        "{rendered}"
    );

    // A nested object reached by values() names the jpath read forms (existing json_path advice stays).
    let g = GraphSource::tinkerpop_modern();
    let mut writable = g.write();
    let err = writable
        .traversal_mut()
        .v("1")
        .property_json(serde_json::json!({"meta": {"k": 1}}))
        .values("meta")
        .as_number(GType::LONG)
        .next()
        .expect_err("a nested object cannot be cast to a number");
    let report = format_diagnostic(&err);
    assert!(
        report.contains(r#"values(jpath("meta.subkey")), has(jpath("meta.subkey"), v), order().by(jpath("meta.subkey"))"#),
        "{report}"
    );
    assert!(
        report.contains(r#"json_path("meta.subkey") instead of"#),
        "{report}"
    );
}

/// The generic `PropertyNotFound` help explains that a key is literal and points at jpath.
#[test]
fn property_not_found_help_names_jpath_for_nested_fields() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let err = lock
        .traversal()
        .v("1")
        .select(Vec::<String>::new())
        .next()
        .expect_err("an empty select() has no label to find");
    let report = format_diagnostic(&err);
    assert!(
        report.contains(r#"is not read as a path even when it contains dots"#)
            && report.contains(r#"values(jpath("meta.k")) or by(jpath("meta.k"))"#)
            && report.contains("a path that does not exist yields nothing instead of an error"),
        "{report}"
    );
}

/// `to_graph()` over vertices names the edge forms that work.
#[test]
fn to_graph_over_vertices_names_the_edge_forms() {
    let graph = GraphSource::tinkerpop_modern();
    let locked = graph.read();
    let err = locked
        .traversal()
        .v(None)
        .to_graph()
        .expect_err("vertices are not edges");
    assert!(
        matches!(err.root_cause(), TraverserError::QueryExecution { .. }),
        "{err}"
    );
    let help = err.help().expect("help");
    assert!(
        help.contains("to_graph() needs edges, but the traversal yields a vertex"),
        "{help}"
    );
    assert!(help.contains("g.e(0).to_graph()"), "{help}");
    assert!(
        help.contains(r#"g.v(1).out_e().has_label("knows").to_graph()"#),
        "{help}"
    );
    assert!(help.contains(r#"subgraph("sg").cap("sg")"#), "{help}");
}

/// `subgraph()` over a vertex stream names the edge steps.
#[test]
fn subgraph_over_vertices_names_the_edge_steps() {
    let graph = GraphSource::tinkerpop_modern();
    let locked = graph.read();
    let err = locked
        .traversal()
        .v(None)
        .subgraph("sg")
        .cap("sg")
        .next()
        .expect_err("vertices are not edges");
    let help = err.help().expect("help");
    assert!(
        help.contains(r#"subgraph("sg") collects edges, but the traversal yields a vertex"#),
        "{help}"
    );
    assert!(help.contains("out_e()/in_e()/both_e()"), "{help}");
    assert!(
        help.contains(r#"g.v(1).both_e().subgraph("sg").cap("sg")"#),
        "{help}"
    );
}

/// `subgraph()` into a bucket that is not a subgraph names `withSideEffect("sg", target)`.
#[test]
fn subgraph_over_a_foreign_side_effect_names_the_target_form() {
    let graph = GraphSource::tinkerpop_modern();
    let locked = graph.read();
    let err = locked
        .traversal()
        .with_side_effect("sg", 1)
        .e(None)
        .subgraph("sg")
        .to_list()
        .expect_err("sg holds a number");
    let help = err.help().expect("help");
    assert!(
        help.contains("already holds a value that is not a subgraph"),
        "{help}"
    );
    assert!(help.contains(r#"with_side_effect("sg", target)"#), "{help}");
    assert!(help.contains("let target = g.e(0).to_graph();"), "{help}");
}

/// Schema-audit decision A: a required property that is not supplied at creation (the
/// `add_property_fold` rule disabled, so `property()` runs after an empty `addV()`) fails, and the
/// help says the property must be supplied at creation and names the rule.
#[test]
fn missing_required_property_at_creation_help_names_add_property_fold() {
    let mut schema = GraphSchema::new().with_mode(SchemaMode::Closed);
    schema.vertices.insert(
        "Person".into(),
        VertexSchema::new(
            PropertySchema::object().with_required_property("name", PropertySchema::string()),
        ),
    );
    let mut graph = TraversalGraph::new();
    graph.set_schema(schema).unwrap();

    let err = graph
        .traversal_mut()
        .with("optimizer.disabled", vec!["add_property_fold"])
        .add_v("Person")
        .property("name", "Ann")
        .next()
        .expect_err("the empty creation misses the required name");
    assert!(
        err.root_cause()
            .to_string()
            .contains("Missing required property 'name'"),
        "{err}"
    );
    let help = err.help().expect("help");
    assert!(
        help.contains("supplied when the element is created"),
        "{help}"
    );
    assert!(help.contains("never filled in"), "{help}");
    assert!(help.contains("add_property_fold"), "{help}");
    assert!(
        help.contains(r#"add_v("L").property("name", value)"#),
        "{help}"
    );
    assert_eq!(
        graph.traversal().v(None).count().next().unwrap(),
        Some(ElementProperty::from(0))
    );

    // With the rule on (default) the same query succeeds.
    graph
        .traversal_mut()
        .add_v("Person")
        .property("name", "Ann")
        .next()
        .unwrap();
}

/// Schema-audit decision B: removing a required property is rejected, and help() explains it.
#[test]
fn required_property_removal_help_explains_required_and_alternatives() {
    let mut schema = GraphSchema::new().with_mode(SchemaMode::Open);
    schema.vertices.insert(
        "Person".into(),
        VertexSchema::new(
            PropertySchema::object().with_required_property("name", PropertySchema::string()),
        ),
    );
    let mut graph = TraversalGraph::new();
    graph.set_schema(schema).unwrap();
    graph
        .traversal_mut()
        .add_v("Person")
        .property("name", "Ann")
        .next()
        .unwrap();

    let err = graph
        .traversal_mut()
        .v(None)
        .remove_property("name")
        .next()
        .expect_err("name is required");
    let text = err.root_cause().to_string();
    assert!(
        text.contains("Cannot remove required property 'name' from vertex 'Person'"),
        "{text}"
    );
    let help = err.help().expect("help");
    assert!(
        help.contains("\"required\"") && help.contains("open or closed"),
        "{help}"
    );
}

#[test]
fn applying_a_schema_to_violating_data_help_says_how_to_proceed() {
    let mut schema = GraphSchema::new().with_mode(SchemaMode::Closed);
    schema.vertices.insert(
        "Person".into(),
        VertexSchema::new(
            PropertySchema::object().with_required_property("name", PropertySchema::string()),
        ),
    );
    let mut graph = TraversalGraph::new();
    graph
        .add_vertex(Some("p".into()), vec!["Person".into()], Default::default())
        .unwrap();
    let err = graph
        .traversal_mut()
        .set_schema(schema)
        .expect_err("the vertex lacks the required name");
    assert!(
        err.root_cause()
            .to_string()
            .contains("Person.name is declared mandatory but is missing on 1 vertex"),
        "{err}"
    );
    let help = err.help().expect("help");
    assert!(help.contains("NOT applied"), "{help}");
    assert!(help.contains("Fix or migrate the offending data"), "{help}");
    assert!(help.contains("mode \"none\""), "{help}");
    assert!(help.contains("g.infer_schema()"), "{help}");
    assert!(help.contains("g.validate_schema(schema)"), "{help}");
    assert!(help.contains("backfill first"), "{help}");
    assert!(
        help.contains(
            "A schema \"default\" fills only elements created after it is set, never existing \
             ones (no backfill)."
        ),
        "{help}"
    );
    assert_eq!(graph.get_schema(), None);
}

#[cfg(feature = "script")]
#[test]
fn traverser_cap_error_names_the_option_and_a_runnable_raise() {
    let graph = std::sync::Arc::new(GraphSource::tinkerpop_modern());
    let err = crate::script_host::eval_value(
        graph.clone(),
        r#"g.with("traversal.max_traversers", 3).v().to_list()"#,
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("Resource limit 'traversal.max_traversers' exceeded (limit 3, reached 6)"),
        "{err}"
    );
    assert!(
        err.contains(r#"g.with("traversal.max_traversers", 6).v().out().out().count()"#),
        "{err}"
    );
    // The raised query from the advice runs.
    let ok = crate::script_host::eval_value(
        graph,
        r#"g.with("traversal.max_traversers", 6).v().out().out().count().next()"#,
    );
    assert!(ok.is_ok(), "{ok:?}");
}

#[cfg(feature = "script")]
#[test]
fn script_limit_error_names_the_builder_and_the_flag() {
    let graph = std::sync::Arc::new(GraphSource::tinkerpop_modern());
    let limits = crate::script_host::ScriptLimits::default().with_max_operations(500);
    let err =
        crate::script_host::eval_value_with_limits(graph, "loop { }", Default::default(), &limits)
            .unwrap_err()
            .to_string();
    assert!(
        err.contains("Resource limit 'script.max_operations' exceeded (limit 500)"),
        "{err}"
    );
    assert!(
        err.contains("ScriptLimits::default().with_max_operations(1000)"),
        "{err}"
    );
    assert!(err.contains("--max-operations 1000"), "{err}");
}

#[test]
fn adding_an_edge_to_a_dropped_vertex_explains_stale_handles() {
    let mut graph = TraversalGraph::new();
    let a = graph
        .add_vertex(Some("a".into()), vec![], Default::default())
        .unwrap();
    let b = graph
        .add_vertex(Some("b".into()), vec![], Default::default())
        .unwrap();
    graph.drop_vertex(b).unwrap();
    let err = graph
        .add_edge(None, None, a, b, Default::default())
        .expect_err("rejected");
    assert!(
        err.to_string()
            .contains("The vertex was removed earlier in this query"),
        "{err}"
    );
    let help = err.help().expect("help");
    assert!(
        help.contains("side_effect(__.drop())") && help.contains("element_map()"),
        "{help}"
    );
}

#[test]
fn label_of_an_edge_removed_earlier_in_the_query_explains_the_stale_handle() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let err = graph
        .traversal_mut()
        .v("1")
        .out_e(None)
        .as_("e")
        .side_effect_t(__::drop())
        .select("e")
        .label()
        .to_list()
        .expect_err("label() of a removed edge fails");
    let report = format_diagnostic(&err);
    assert!(
        report.contains("The edge was removed earlier in this query; its id, label and value are no longer available"),
        "{report}"
    );
    assert!(
        report.contains("label()"),
        "the failing step is named: {report}"
    );
    assert!(
        report.contains(
            "g.v(\"1\").out_e().as(\"e\").element_map().as(\"m\").select(\"e\").side_effect(__.drop()).select(\"m\")"
        ),
        "{report}"
    );
    assert!(
        report.contains("through a step label (as()/select()), a side effect (aggregate()/store()/cap()), a path or a sack"),
        "{report}"
    );
}

#[test]
fn id_of_a_vertex_removed_earlier_in_the_query_explains_the_stale_handle() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let err = graph
        .traversal_mut()
        .v("1")
        .as_("a")
        .side_effect_t(__::drop())
        .select("a")
        .id()
        .to_list()
        .expect_err("id() of a removed vertex fails");
    let report = format_diagnostic(&err);
    assert!(
        report.contains("The vertex was removed earlier in this query"),
        "{report}"
    );
    assert!(
        report.contains(
            "g.v(\"1\").as(\"a\").element_map().as(\"m\").select(\"a\").side_effect(__.drop()).select(\"m\")"
        ),
        "{report}"
    );
    assert!(
        report.contains(
            "Not every later read fails: values(), properties() and value_map() of the removed \
             vertex find no properties"
        ),
        "{report}"
    );
}

/// A commit hook's veto: the help says the whole unit was rolled back and the query can be
/// run again once the hook's cause is fixed; an unsupported hook registration points to
/// `capabilities()`.
#[test]
fn commit_rejected_and_unsupported_have_help() {
    struct Refuse;
    impl CommitHook for Refuse {
        fn before_commit(&mut self, _changes: &ChangeSet) -> Result<(), HookError> {
            Err("journal unavailable".into())
        }
    }
    let mut graph = TraversalGraph::tinkerpop_modern();
    graph.register_commit_hook(Box::new(Refuse)).unwrap();
    let err = graph.traversal_mut().add_v("x").to_list().unwrap_err();
    assert!(err.to_string().contains("journal unavailable"), "{err}");
    let help = err.help().expect("help");
    assert!(help.contains("whole unit was rolled back"), "{help}");
    assert!(help.contains("run the query again"), "{help}");
    assert!(help.contains("transaction, whole script"), "{help}");
    assert!(help.contains("Transactional::dry_run"), "{help}");

    // A veto that brings its own advice replaces the generic help.
    struct Frozen;
    impl CommitHook for Frozen {
        fn name(&self) -> &str {
            "frozen"
        }
        fn before_commit(&mut self, _changes: &ChangeSet) -> Result<(), HookError> {
            Err(Box::new(graphersal::changes::HookRefusal::new(
                "the graph is frozen: writes are refused",
                "Write to the live copy instead.",
            )))
        }
    }
    let mut graph = TraversalGraph::tinkerpop_modern();
    graph.register_commit_hook(Box::new(Frozen)).unwrap();
    let err = graph.traversal_mut().add_v("x").to_list().unwrap_err();
    assert!(err.to_string().contains("the graph is frozen"), "{err}");
    let help = err.help().expect("help");
    assert_eq!(
        help,
        "The commit hook 'frozen' refused the commit, so the whole unit was rolled back and \
         left nothing behind. Write to the live copy instead."
    );

    let view = TraversalGraph::new();
    let mut read_only = &view;
    let err = read_only
        .register_commit_hook(Box::new(Refuse))
        .unwrap_err();
    let help = err.help().expect("help");
    assert!(help.contains("capabilities()"), "{help}");
    assert!(help.contains("change_capture"), "{help}");
}

/// A source step inside a child traversal (`__.with_sack(1)`) names the step and shows the fix:
/// the same step on `g`, before the first step.
#[cfg(feature = "script")]
#[test]
fn source_step_in_child_help_moves_the_step_to_g() {
    use crate::script_host::{ScriptGraphTraversalSource, engine, eval_value, graph_scope};

    let graph = std::sync::Arc::new(GraphSource::tinkerpop_modern());
    let cases = [
        (
            "g.V().local(__.withSack(1).out().sack())",
            "with_sack",
            "g.with_sack(1).v().local(__.out().sack()) instead of \
             g.v().local(__.with_sack(1).out().sack())",
        ),
        (
            r#"g.v().where(__.with_side_effect("x", 1).select("x"))"#,
            "with_side_effect",
            "g.with_side_effect(\"x\", 1).v().local(__.select(\"x\")) instead of \
             g.v().local(__.with_side_effect(\"x\", 1).select(\"x\"))",
        ),
        (
            "g.V().local(__.withPath().out().path())",
            "with_path",
            "g.with_path().v().local(__.out().path()) instead of \
             g.v().local(__.with_path().out().path()); with_path() changes nothing in Graphersal \
             (paths are recorded as the query reads them), so it can also simply be left out",
        ),
    ];
    for (script, step, example) in cases {
        let mut scope = graph_scope(graph.clone());
        let gts = engine()
            .eval_with_scope::<ScriptGraphTraversalSource>(&mut scope, script)
            .unwrap_or_else(|e| panic!("`{script}`: {e}"));
        let err = {
            let lock = graph.read();
            gts.ast_convert_to_gts(&*lock).to_list().expect_err(script)
        };
        assert_eq!(
            err.root_cause().to_string(),
            format!(
                "`{step}()` configures the traversal source g and cannot be used inside a child \
                 traversal"
            )
        );
        let help = err.help().expect("help");
        assert_eq!(
            help,
            format!(
                "{step}() sets up the traversal source g: it applies to the whole query, child \
                 traversals included, so it is written once on g, before the first step, for \
                 example {example}. A child traversal sees the sacks and side effects of the \
                 outer traversal."
            )
        );
        // The script's data terminal carries the same diagnostic.
        let report = eval_value(graph.clone(), &format!("{script}.toList()"))
            .expect_err(script)
            .to_string();
        assert!(report.contains(&help), "{report}");
    }
}

/// `PermissionDenied`: the help names the denied request, says who decides and how a host
/// allows it, and gives advice for the resource built from the denied step.
#[cfg(feature = "script")]
#[test]
fn permission_denied_help_names_the_request_the_policy_and_an_alternative() {
    use std::sync::Arc;
    let graph = Arc::new(GraphSource::tinkerpop_modern());
    let run = |script: &str, policy: AccessPolicy| {
        let policy: Arc<dyn Authorizer> = Arc::new(policy);
        graphersal::script::eval_value_with_limits(
            graph.clone(),
            script,
            Default::default(),
            &graphersal::script::ScriptLimits::default(),
            policy,
        )
        .unwrap_err()
        .to_string()
    };
    let err = run(
        r#"g.V().property("x", 1).toList()"#,
        AccessPolicy::read_only(),
    );
    assert!(
        err.contains("Permission denied for 'property' (Update Data(vertex))"),
        "{err}"
    );
    assert!(
        err.contains(
            "'property' needs Update Data(vertex), which the host running this script does not allow"
        ),
        "{err}"
    );
    assert!(
        err.contains("AccessPolicy::read_only().allow(ActionKind::Update, ResourceKind::Data)"),
        "{err}"
    );
    assert!(
        err.contains("GraphSource::empty().add_v(\"x\").next()"),
        "{err}"
    );
    assert!(err.contains("remove property() from the query"), "{err}");

    let err = run(
        r#"g.with("evaluationTimeout", 0).V().count().next()"#,
        AccessPolicy::read_only(),
    );
    assert!(
        err.contains("Update Option(\"evaluationTimeout\")"),
        "{err}"
    );
    assert!(err.contains("remove the g.with(..) call"), "{err}");

    let err = run("g.V().count().next()", AccessPolicy::deny_all());
    assert!(
        err.contains("Without reading data a script can only compute"),
        "{err}"
    );

    #[cfg(feature = "io")]
    {
        let err = run(
            r#"g.export_graphml("x.graphml")"#,
            AccessPolicy::read_only(),
        );
        assert!(
            err.contains("'export_graphml' needs Create File(\"x.graphml\")"),
            "{err}"
        );
        assert!(err.contains("use a relative path inside it"), "{err}");
    }
}

/// `ScriptError::ReturnedClosure`: a whole-script unit that returns a closure.
#[cfg(feature = "script")]
#[test]
fn returned_closure_help_says_to_return_data_or_a_traversal() {
    let graph = std::sync::Arc::new(GraphSource::tinkerpop_modern());
    let err = graphersal::script::eval_value_atomic(
        graph,
        "|x| x + 1",
        Default::default(),
        &graphersal::script::ScriptLimits::default(),
        std::sync::Arc::new(graphersal::auth::AllowAll),
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains(
            "A script run as one unit cannot return a closure or function pointer (the result)"
        ),
        "{err}"
    );
    assert!(
        err.contains("Return data or a traversal instead of a closure"),
        "{err}"
    );
    assert!(
        err.contains("The closure was found at the result."),
        "{err}"
    );
}

/// `drop()` on a value that is not stored graph data names the real kind (never `computed`) and
/// explains what drop() removes and how to reach the elements of a list or a path; the advice of
/// other steps (key()/value(), Scope.local, select(Column)) stays out.
#[test]
fn drop_of_a_non_removable_value_names_its_kind_and_what_drop_removes() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let g = || lock.traversal();
    for (err, kind) in [
        (
            g().v("1").path().drop().to_list().expect_err("path"),
            "path",
        ),
        (
            g().v("1").fold().drop().to_list().expect_err("list"),
            "array",
        ),
        (
            g().v("1")
                .value_map(None)
                .drop()
                .to_list()
                .expect_err("map"),
            "object",
        ),
        (
            g().inject(vec![1]).drop().to_list().expect_err("int"),
            "int64",
        ),
    ] {
        assert!(
            err.root_cause()
                .to_string()
                .contains(&format!("got type '{kind}'")),
            "{err:?}"
        );
        let help = value_help(&err);
        assert!(help.contains("drop() removes stored graph data"), "{help}");
        assert!(
            help.contains(r#"g.v("1").out().fold().unfold().drop()"#),
            "{help}"
        );
        assert!(help.contains(r#"remove_property("key")"#), "{help}");
        assert!(!help.contains(".value()/.key() only read"), "{help}");
        assert!(!help.contains("Scope.local"), "{help}");
        assert!(!help.contains("select(Column.keys)"), "{help}");
    }
}

/// The key filters on a `properties(jpath(..))` element and on an unfolded map entry name the
/// real kind and show the form that works.
#[test]
fn key_filters_on_a_path_property_or_an_entry_show_the_working_form() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let jpath = |text: &str| text.parse::<JsonPath>().unwrap();
    let err = lock
        .traversal()
        .v("1")
        .properties(jpath("name"))
        .has_p("a", P::Gt(0))
        .to_list()
        .expect_err("a path property has no keys");
    assert!(
        err.root_cause()
            .to_string()
            .contains("got type 'pathProperty'")
    );
    let help = value_help(&err);
    assert!(
        help.contains(
            r#"test the path on the element instead, for example g.v().has(jpath("meta.a"), 1)"#
        ),
        "{help}"
    );
    assert!(!help.contains("drop() cannot remove it"), "{help}");

    let err = lock
        .traversal()
        .v("1")
        .value_map(None)
        .unfold()
        .has_not("a")
        .to_list()
        .expect_err("an entry has no keys");
    assert!(err.root_cause().to_string().contains("got type 'entry'"));
    let help = value_help(&err);
    assert!(
        help.contains(r#"g.v().value_map().unfold().where(__.select(Column.keys).is("name"))"#),
        "{help}"
    );
    assert!(
        help.contains(r#"g.v().value_map().has("name").unfold()"#),
        "{help}"
    );
}

/// `inE()` (and the other vertex-to-adjacent steps) on an edge: TinkerPop raises too; the help
/// names outV()/inV()/otherV() instead of the string-step advice.
#[test]
fn adjacency_steps_on_an_edge_point_at_the_edge_vertex_steps() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let errors = [
        lock.traversal().v("1").out_e(None).in_e(None).to_list(),
        lock.traversal().v("1").out_e(None).out(None).to_list(),
        lock.traversal().e(None).both(None).to_list(),
    ];
    for result in errors {
        let err = result.expect_err("an edge has no adjacent edges");
        assert!(
            err.root_cause().to_string().contains("got type 'edge'"),
            "{err:?}"
        );
        let help = value_help(&err);
        assert!(
            help.contains("From an edge, step to its vertices first"),
            "{help}"
        );
        assert!(help.contains("g.v(\"1\").out_e().in_v().in_e()"), "{help}");
        assert!(!help.contains("This step needs a string"), "{help}");
        assert!(!help.contains(".value()/.key() only read"), "{help}");
    }
}

/// The same diagnostics through the Rhai DSL, in both spellings.
#[cfg(feature = "script")]
#[test]
fn drop_key_filter_and_adjacency_help_in_rhai() {
    let graph = std::sync::Arc::new(GraphSource::tinkerpop_modern());
    for (script, needle) in [
        (
            r#"g.V("1").path().drop().toList()"#,
            "drop() removes stored graph data",
        ),
        (
            r#"g.v("1").fold().drop().to_list()"#,
            "drop() removes stored graph data",
        ),
        (
            r#"g.V("1").properties(jpath("name")).has("a", 1).toList()"#,
            "test the path on the element instead",
        ),
        (
            r#"g.v("1").properties(jpath("name")).has_not("a").to_list()"#,
            "test the path on the element instead",
        ),
        (
            r#"g.V("1").valueMap().unfold().has("a").toList()"#,
            "unfold() split the map into single key/value entries",
        ),
        (
            r#"g.V("1").outE().inE().toList()"#,
            "From an edge, step to its vertices first",
        ),
        (
            r#"g.v("1").out_e().in_e().to_list()"#,
            "From an edge, step to its vertices first",
        ),
    ] {
        let err = crate::script_host::eval_value(graph.clone(), script)
            .expect_err(script)
            .to_string();
        assert!(err.contains(needle), "{script}: {err}");
    }
}

/// A string step or a reducer fed a folded list names `conjoin()` and the list functions.
#[test]
fn cast_exception_on_a_folded_list_help_names_conjoin_and_the_list_functions() {
    let graph = GraphSource::tinkerpop_modern();
    let locked = graph.read();
    let err = locked
        .traversal()
        .v(None)
        .values("name")
        .fold()
        .as_string()
        .next()
        .expect_err("a list has no string form");
    let report = format_diagnostic(&err);
    assert!(
        report.contains(
            "To join the elements into one string use conjoin(), for example \
             g.v().values(\"name\").fold().conjoin(\", \"); to combine it with a second list use \
             combine(), merge(), intersect(), difference(), disjunct() or product(), for example \
             g.v().values(\"name\").fold().combine([\"dave\"])."
        ),
        "the help must name conjoin() and the list functions:\n{report}"
    );
    assert!(
        report.contains(
            "reverse() reverses the list itself, for example \
             g.v().values(\"name\").fold().reverse()."
        ),
        "the help must name reverse():\n{report}"
    );
    assert!(
        report.contains("or unfold() it first."),
        "the existing advice must stay:\n{report}"
    );
}

/// The help of a list function names `fold()` for a non-list input, the list argument forms for a
/// bad argument, `fold()` at the end of a child traversal, and the map form of `merge()`.
#[test]
fn list_function_input_help_names_fold_and_the_argument_forms() {
    let graph = GraphSource::tinkerpop_modern();
    let locked = graph.read();
    let g = || locked.traversal();

    let report = format_diagnostic(
        &g().v(None)
            .values("name")
            .combine(vec!["x"])
            .next()
            .expect_err("a string is not a list"),
    );
    assert!(
        report.contains("The combine() step takes a list as its incoming value, but got string"),
        "{report}"
    );
    assert!(
        report.contains(
            "combine() works on the list a traverser holds (a fold(), a path(), a list-valued \
             property). Collect the stream into a list with fold() first, for example \
             g.v().values(\"name\").fold().combine([\"x\"])"
        ),
        "{report}"
    );

    let report = format_diagnostic(
        &g().v(None)
            .values("name")
            .fold()
            .intersect(2)
            .next()
            .expect_err("2 is not a list"),
    );
    assert!(
        report.contains("The intersect() step takes a list as its argument, but got int64"),
        "{report}"
    );
    assert!(
        report.contains(
            "The argument of intersect() is a list constant such as [\"a\", \"b\"] or a child \
             traversal whose first result is a list"
        ),
        "{report}"
    );

    let report = format_diagnostic(
        &g().v(None)
            .fold()
            .product_traversal(__::v(None))
            .next()
            .expect_err("a vertex is not a list"),
    );
    assert!(
        report.contains(
            "The child traversal of product() must yield a list as its first result. End it with \
             fold()"
        ),
        "{report}"
    );

    let report = format_diagnostic(
        &g().v(None)
            .element_map(None)
            .merge_traversal(__::constant("a"))
            .next()
            .expect_err("a string is not a map"),
    );
    assert!(
        report.contains("The merge() step takes a map as its traversal argument, but got string"),
        "{report}"
    );

    let report = format_diagnostic(
        &g().v(None)
            .element_map(None)
            .merge(vec!["a"])
            .next()
            .expect_err("a list is not a map"),
    );
    assert!(
        report.contains("merge() of a map merges another map into it"),
        "{report}"
    );

    let report = format_diagnostic(
        &g().v(None)
            .values("name")
            .conjoin("-")
            .next()
            .expect_err("a string is not a list"),
    );
    assert!(
        report.contains("g.v().values(\"name\").fold().conjoin(\"-\")"),
        "{report}"
    );
}

/// A null incoming value of a list function gets the "keep the nulls out" advice.
#[test]
fn list_function_null_input_help_names_the_null_filter() {
    let graph = GraphSource::tinkerpop_modern();
    let locked = graph.read();
    let report = format_diagnostic(
        &locked
            .traversal()
            .inject(vec![ElementProperty::Null])
            .combine(vec![1])
            .next()
            .expect_err("null is not a list"),
    );
    assert!(
        report.contains("The combine() step takes a list as its incoming value, but got null"),
        "{report}"
    );
    assert!(
        report.contains(
            "Keep the null values out before the step, for example \
             g.v().values(\"tags\").not(__.is(typeOf(GType.NULL))).combine([\"x\"])."
        ),
        "{report}"
    );
}

/// The help of `concat()` names `coalesce()` for a child without a result, `as_string()` and
/// `conjoin()` for a child result that is not a string, and the argument forms for a bad constant.
#[test]
fn concat_argument_help_names_the_fixes() {
    let graph = GraphSource::tinkerpop_modern();
    let locked = graph.read();
    let report = |result: Result<Vec<ElementProperty>, Box<TraverserError>>| {
        format_diagnostic(&result.expect_err("concat must fail"))
    };
    let no_result = report(
        locked
            .traversal()
            .v("3")
            .as_("x")
            .values("name")
            .concat_traversal(vec![__::select("x").values("age")])
            .to_list(),
    );
    assert!(
        no_result.contains(
            "A child traversal of concat() must yield a value for every traverser: its first \
             result is appended. Make it always produce one, for example with coalesce(): \
             g.v().values(\"name\").concat(__.coalesce(__.values(\"lang\"), __.constant(\"\"))), \
             or keep only the traversers it has a value for: \
             g.v().where(__.values(\"lang\")).values(\"name\").concat(__.values(\"lang\"))."
        ),
        "{no_result}"
    );
    let not_a_string = report(
        locked
            .traversal()
            .v("1")
            .as_("x")
            .values("name")
            .concat_traversal(vec![__::select("x").values("age")])
            .to_list(),
    );
    assert!(
        not_a_string.contains(
            "A child traversal of concat() must yield a string (or null, which is skipped). \
             Convert its result with as_string(), for example \
             g.v().values(\"name\").concat(\" \").concat(__.values(\"age\").as_string()); a list \
             is joined into one string with conjoin(), for example \
             g.v().values(\"name\").concat(__.out().values(\"name\").fold().conjoin(\",\"))."
        ),
        "{not_a_string}"
    );
    let constant = report(
        locked
            .traversal()
            .inject(vec!["a"])
            .concat(vec![1])
            .to_list(),
    );
    assert!(
        constant.contains(
            "The arguments of concat() are strings (a null is skipped) or child traversals, for \
             example g.v().values(\"name\").concat(\" is \", \"here\") or \
             g.v().values(\"name\").concat(__.values(\"age\").as_string()); convert a number with \
             as_string() in a child traversal instead of passing it directly."
        ),
        "{constant}"
    );
}

/// An invalid `TextP.regex()` names the Rust `regex` dialect and its syntax page.
#[test]
fn invalid_regex_help_names_the_dialect() {
    let err = P::regex("a{2").expect_err("unclosed repetition");
    let report = format_diagnostic(&err);
    assert!(
        report.contains("Invalid regular expression 'a{2'"),
        "{report}"
    );
    assert!(
        report.contains(
            "TextP.regex()/TextP.not_regex() take a regular expression in the syntax of the Rust \
             `regex` crate (https://docs.rs/regex/latest/regex/#syntax), not Java's"
        ),
        "{report}"
    );
}

#[test]
fn sample_weight_help_names_numeric_by_forms() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let err = lock
        .traversal()
        .v(None)
        .sample(1)
        .by("name")
        .to_list()
        .expect_err("a string weight");
    let report = format_diagnostic(&err);
    assert!(
        report.contains("which must be a number of at least 0, but got string"),
        "{report}"
    );
    assert!(
        report.contains("g.e().sample(2).by(\"weight\")")
            && report.contains("g.v().sample(1).by(__.values(\"score\").as_number())"),
        "{report}"
    );
}

#[test]
fn fail_help_shows_how_to_guard_the_step() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let err = lock
        .traversal()
        .v("1")
        .fail(Some("stop here"))
        .to_list()
        .expect_err("fail() reached");
    let report = format_diagnostic(&err);
    assert!(
        report.contains("stop here (fail() reached by v[1])"),
        "{report}"
    );
    assert!(
        report.contains("g.v().coalesce(__.out(\"knows\"), __.fail(\"no friends\"))"),
        "{report}"
    );
}

#[test]
fn random_seed_option_help_shows_an_integer_seed() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let err = lock
        .traversal()
        .with("random.seed", "x")
        .v(None)
        .sample(1)
        .to_list()
        .expect_err("a string seed");
    let report = format_diagnostic(&err);
    assert!(report.contains("expected an integer seed"), "{report}");
    assert!(
        report.contains("g.with(\"random.seed\", 42).v().sample(2)"),
        "{report}"
    );
}

#[test]
fn render_spelling_option_help_names_both_spellings() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let err = lock
        .traversal()
        .with("render.spelling", "kebab")
        .v(None)
        .to_list()
        .expect_err("an unknown spelling");
    let report = format_diagnostic(&err);
    assert!(
        report.contains(r#"expected "snake" or "camel""#),
        "{report}"
    );
    assert!(
        report.contains(r#"g.with("render.spelling", "camel").v().has_label("person").profile()"#)
            && report.contains("never a result"),
        "{report}"
    );
}

#[test]
fn group_side_effect_key_clash_help_names_the_group_steps() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let err = lock
        .traversal()
        .v(None)
        .aggregate("a")
        .group_count_side_effect("a")
        .to_list()
        .expect_err("a list key");
    let report = format_diagnostic(&err);
    assert!(report.contains("already holds a list"), "{report}");
    assert!(
        report.contains("group(\"a\") and group_count(\"a\") add every traverser to the map")
            && report.contains("g.v().aggregate(\"a\").group_count(\"m\").by(T.label).cap(\"m\")"),
        "{report}"
    );
}

#[test]
fn side_effect_key_help_names_group_count() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let err = lock
        .traversal()
        .v(None)
        .cap("nope")
        .to_list()
        .expect_err("an unknown key");
    let report = format_diagnostic(&err);
    assert!(
        report.contains("g.v().group_count(\"nope\").by(T.label).cap(\"nope\")"),
        "{report}"
    );
}

#[test]
fn with_option_help_names_value_map_and_index() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let err = lock
        .traversal()
        .v(None)
        .out(None)
        .with_option(WithOptions::Tokens, None)
        .to_list()
        .expect_err("out() takes no with()");
    let report = format_diagnostic(&err);
    assert!(
        report.contains("out() cannot take this with() option"),
        "{report}"
    );
    assert!(
        report.contains("g.v().value_map(\"name\").with(WithOptions.tokens, WithOptions.labels)")
            && report.contains("index().with(WithOptions.indexer, WithOptions.map)")
            && report.contains("g.with(\"random.seed\", 7)"),
        "{report}"
    );
}

#[test]
fn sample_second_by_help_names_the_single_weight() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let err = lock
        .traversal()
        .v(None)
        .sample(1)
        .by("age")
        .by(By::Id)
        .to_list()
        .expect_err("two by()");
    let report = format_diagnostic(&err);
    assert!(
        report.contains("sample() takes at most one by() modulator"),
        "{report}"
    );
    assert!(
        report.contains("sample(n) takes at most one by(), the weight")
            && report.contains("g.v().order().by(\"age\").sample(2)"),
        "{report}"
    );
}

/// `traversal.max_value_depth`: the help names the loops that nest, the option with a runnable
/// example, the lock and the option request.
#[test]
fn value_depth_limit_help_names_the_option_and_the_nesting_loops() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let err = lock
        .traversal()
        .inject(vec![1])
        .repeat(__::fold())
        .times(200)
        .to_list()
        .expect_err("the loop nests its value 200 levels deep");
    assert_eq!(
        err.root_cause().to_string(),
        "Resource limit 'traversal.max_value_depth' exceeded (limit 128)"
    );
    let help = err.help().expect("help");
    for needle in [
        "repeat(__.fold())",
        "jpath(..) write with many segments",
        "g.inject(1).repeat(__.fold()).times(10).next()",
        "g.with(\"traversal.max_value_depth\", 256).inject(1).repeat(__.fold()).times(200).count()",
        "ExecutionPolicy::with_locked(\"traversal.max_value_depth\")",
        "needs Update on that Option",
    ] {
        assert!(help.contains(needle), "{needle} missing in: {help}");
    }
}

/// A malformed `traversal.max_value_depth` names a valid value and the default.
#[test]
fn invalid_value_depth_option_help_gives_a_valid_value() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let err = lock
        .traversal()
        .with("traversal.max_value_depth", 0)
        .v(None)
        .count()
        .next()
        .expect_err("0 is not a depth");
    assert_eq!(
        err.help().expect("help"),
        "Set 'traversal.max_value_depth' to a positive integer number of nesting levels when the \
         traversal source is created, for example \
         g.with(\"traversal.max_value_depth\", 256).inject(1).repeat(__.fold()).times(200).count(); \
         the default is 128."
    );
}

/// A jpath with more than 256 segments is a parse error whose help states the bound.
#[test]
fn jpath_help_states_the_segment_and_index_bounds() {
    let long = format!("a{}", ".b".repeat(256));
    let err = long
        .parse::<graphersal::prelude::JsonPath>()
        .expect_err("257 segments");
    assert!(
        err.to_string()
            .contains("too many segments: a path has at most 256"),
        "{err}"
    );
    let help = err.help().expect("help");
    assert!(
        help.contains(
            "A path has at most 256 segments, and an index is at most 9007199254740991 \
             (2^53 - 1) from either end."
        ),
        "{help}"
    );
    let err = "a[-9223372036854775808]"
        .parse::<graphersal::prelude::JsonPath>()
        .expect_err("out of range");
    assert!(
        err.to_string().contains(
            "array index '-9223372036854775808' at position 2 is out of range (at most \
             +/-9007199254740991)"
        ),
        "{err}"
    );
}

/// `traversal.max_materialized_bytes`: the help names the materializing steps and the doubling
/// loops, the alternatives that work on the multiplicity, the option with a runnable example, the
/// lock and the option request.
#[test]
fn materialized_bytes_help_names_the_option_and_the_alternatives() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let err = lock
        .traversal()
        .with("traversal.max_materialized_bytes", 100_000)
        .inject(vec![1])
        .repeat(__::path())
        .times(60)
        .to_list()
        .expect_err("the path doubles past 100 000 bytes");
    let cause = err.root_cause().to_string();
    assert!(
        cause.starts_with(
            "Resource limit 'traversal.max_materialized_bytes' exceeded (limit 100000, reached "
        ),
        "{cause}"
    );
    let help = err.help().expect("help");
    for needle in [
        "'traversal.max_materialized_bytes' allows (100000 bytes); this one needed at least",
        "fold(), aggregate(), store(), group() value lists, a terminal list",
        "repeat(__.path()), repeat(__.group())",
        "g.v().repeat(__.both()).times(8).count()",
        "g.v().repeat(__.both()).times(8).group_count()",
        "g.v().repeat(__.both()).times(8).dedup().fold()",
        "g.inject(1).repeat(__.path()).times(10).count()",
        "g.with(\"traversal.max_materialized_bytes\", 200000).v().repeat(__.both()).times(8).fold()",
        "ExecutionPolicy::with_locked(\"traversal.max_materialized_bytes\")",
        "needs Update on that Option",
    ] {
        assert!(help.contains(needle), "{needle} missing in: {help}");
    }
}

/// `memory.limit`: the help says what is measured, names the alternatives that hold less, the
/// recovery when the graph alone is over the budget, every front end's setting and the option
/// request; an invalid value names both accepted shapes.
#[test]
fn memory_limit_help_names_the_measure_the_alternatives_and_the_front_ends() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let err = lock
        .traversal()
        .with("memory.limit", 1_000)
        .v(None)
        .to_list()
        .expect_err("Modern alone is larger than 1000 bytes");
    let cause = err.root_cause().to_string();
    assert!(
        cause.starts_with("Resource limit 'memory.limit' exceeded (limit 1000, reached "),
        "{cause}"
    );
    let help = err.help().expect("help");
    for needle in [
        "memory budget 'memory.limit' (1000 bytes); it reached",
        "everything it changed was rolled back",
        "live heap when the host installs the tracking allocator",
        "g.memory_usage()",
        "g.v().repeat(__.both()).times(8).count()",
        "g.v().out().dedup().fold()",
        "g.with(\"memory.limit\", 2000).v().has_label(\"tmp\").drop()",
        "--memory-limit <bytes|auto>",
        "Graph(memory_limit=...)",
        "ExecutionPolicy::with_default(\"memory.limit\", bytes)",
        "'memory.headroom'",
        "needs Update on that Option",
    ] {
        assert!(help.contains(needle), "{needle} missing in: {help}");
    }
    let err = lock
        .traversal()
        .with("memory.limit", "plenty")
        .v(None)
        .to_list()
        .expect_err("not a byte count");
    let help = err.help().expect("help");
    assert!(
        help.contains("g.with(\"memory.limit\", \"auto\")"),
        "{help}"
    );
    assert!(help.contains("no memory budget (the default)"), "{help}");
}

/// `traversal.max_string_bytes`: the help names the amplifying steps, the script-side
/// counterpart, runnable examples, the lock and the option request.
#[test]
fn string_bytes_help_names_the_option_and_the_amplifying_steps() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let err = lock
        .traversal()
        .with("traversal.max_string_bytes", 4)
        .inject(vec!["xx"])
        .replace("x", "xxxx")
        .to_list()
        .expect_err("8 bytes are more than 4");
    assert_eq!(
        err.root_cause().to_string(),
        "Resource limit 'traversal.max_string_bytes' exceeded (limit 4, reached 8)"
    );
    assert!(
        err.to_string().contains("'replace(\"x\", \"xxxx\")'"),
        "{err}"
    );
    let help = err.help().expect("help");
    for needle in [
        "'traversal.max_string_bytes' allows (4 bytes); this one would be 8 bytes",
        "replace(), concat(), conjoin() and format()",
        "'script.max_string_size'",
        "g.v().values(\"name\").replace(\"a\", \"b\")",
        "g.inject(\"x\").repeat(__.concat(__.identity())).times(10)",
        "g.with(\"traversal.max_string_bytes\", 8).v().values(\"name\").replace(\"a\", \"aa\")",
        "ExecutionPolicy::with_locked(\"traversal.max_string_bytes\")",
        "needs Update on that Option",
    ] {
        assert!(help.contains(needle), "{needle} missing in: {help}");
    }
}

/// A malformed byte limit names a valid value and the default.
#[test]
fn invalid_byte_limit_options_help_give_a_valid_value() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let err = lock
        .traversal()
        .with("traversal.max_materialized_bytes", 0)
        .v(None)
        .count()
        .next()
        .expect_err("0 is not a budget");
    assert_eq!(
        err.root_cause().to_string(),
        "Invalid value for option 'traversal.max_materialized_bytes': expected a positive integer \
         number of bytes, got 0"
    );
    assert_eq!(
        err.help().expect("help"),
        "Set 'traversal.max_materialized_bytes' to a positive integer number of bytes when the \
         traversal source is created, for example \
         g.with(\"traversal.max_materialized_bytes\", 2147483648).v().repeat(__.both()).times(8).fold(); \
         the default is 1073741824 (1 GiB; 268435456 on wasm32)."
    );
    let err = lock
        .traversal()
        .with("traversal.max_string_bytes", "big")
        .v(None)
        .count()
        .next()
        .expect_err("a string is not a size");
    assert_eq!(
        err.help().expect("help"),
        "Set 'traversal.max_string_bytes' to a positive integer number of bytes when the \
         traversal source is created, for example \
         g.with(\"traversal.max_string_bytes\", 67108864).v().values(\"name\").replace(\"a\", \"aa\"); \
         the default is 16777216 (16 MiB)."
    );
    let err = lock
        .traversal()
        .with("bulk.max_expansion", 0)
        .v(None)
        .count()
        .next()
        .expect_err("0 values");
    assert!(
        err.help()
            .expect("help")
            .ends_with("the default is 10000000."),
        "{err}"
    );
}

/// The script value-depth help names every front end's switch.
#[cfg(feature = "script")]
#[test]
fn script_value_depth_help_names_the_front_end_switches() {
    let graph = std::sync::Arc::new(GraphSource::tinkerpop_modern());
    let err = crate::script_host::eval_value(
        graph,
        "let x = 1; for i in 0..200 { x = [x]; } g.inject(x).count().next()",
    )
    .unwrap_err()
    .to_string();
    for needle in [
        "ScriptLimits::default().with_max_value_depth(256)",
        "--max-value-depth 256",
        "graph.set_limits(max_value_depth=256)",
    ] {
        assert!(err.contains(needle), "{needle} missing in: {err}");
    }
}

/// Errors found before the run (robustness finding 10) carry the location of the step they are
/// about: a rejected modulator, an undeclared step label and a denied step, also inside a child
/// traversal.
#[test]
fn errors_before_the_run_carry_the_step_location() {
    let graph = GraphSource::tinkerpop_modern();
    {
        let lock = graph.read();
        // A from()/to() no step takes, at the step it follows.
        let err = lock
            .traversal()
            .v(None)
            .out(None)
            .to("x")
            .to_list()
            .unwrap_err();
        let location = err.step_location().expect("located");
        assert_eq!(location.step_id, "1");
        assert!(format_diagnostic(&err).contains("at #1: v().out()"));
        // An undeclared label, at the reading step, its own error kind.
        let err = lock
            .traversal()
            .v(None)
            .union(vec![__::select("nope")])
            .to_list()
            .unwrap_err();
        assert_eq!(err.step_location().expect("located").step_id, "1.0.0");
        assert!(matches!(
            err.root_cause(),
            TraverserError::Value(ValueError::UndeclaredLabel { .. })
        ));
        let rendered = format_diagnostic(&err);
        assert!(
            rendered.contains("Caused by: Step label 'nope' is never declared in this query"),
            "{rendered}"
        );
        assert!(!rendered.contains("undeclared_label"), "{rendered}");
        // The help is in the canonical DSL style.
        let help = err.help().expect("help");
        assert!(
            help.contains("g.v(\"1\").as(\"nope\").out().select(\"nope\")"),
            "{help}"
        );
        for rust in ["as_(", "where_t(", "(None)", "select_pop", "Pop::"] {
            assert!(!help.contains(rust), "{rust} in {help}");
        }
    }
    // A denied step is located in the plan as written.
    let mut write = graph.write();
    let err = write
        .traversal_mut()
        .with_authorizer(std::sync::Arc::new(
            graphersal::auth::AccessPolicy::read_only(),
        ))
        .v(None)
        .add_v("x")
        .to_list()
        .unwrap_err();
    assert_eq!(err.step_location().expect("located").step_id, "1");
    assert!(matches!(
        err.root_cause(),
        TraverserError::PermissionDenied { .. }
    ));
}

/// The DSL reports the same locations for the plan checks of its argument errors, and an
/// invalid constant renders as written.
#[test]
#[cfg(feature = "script")]
fn dsl_plan_checks_are_located_and_render_the_argument_as_written() {
    use crate::script_host::eval;
    for (query, at) in [
        (
            r#"g.inject(1).merge_v(1).toList()"#,
            "at #1: inject(1).merge_v(1)",
        ),
        (
            r#"g.inject(1).math("_ +").toList()"#,
            r#"at #1: inject(1).math("_ +")"#,
        ),
        (
            r#"g.inject(1).product(1).toList()"#,
            "at #1: inject(1).product(1)",
        ),
        (
            r#"g.inject(1).local(__.with_sack(1)).toList()"#,
            "at #1.0.0: with_sack()",
        ),
    ] {
        let graph = std::sync::Arc::new(GraphSource::tinkerpop_modern());
        let report = eval(graph, query).expect_err(query).to_string();
        assert!(report.contains(at), "{query}: {report}");
    }
}

/// Under `render.spelling = camel` an error raised before the run is respelled like one inside
/// it: a denied step names its camelCase spelling, and so does its help.
#[test]
#[cfg(feature = "script")]
fn errors_before_the_run_are_respelled() {
    let graph = std::sync::Arc::new(GraphSource::tinkerpop_modern());
    let err = graphersal::script::eval_value_with_limits(
        graph.clone(),
        r#"g.with("render.spelling", "camel").V().addV("x").toList()"#,
        Default::default(),
        &graphersal::script::ScriptLimits::default(),
        std::sync::Arc::new(graphersal::auth::AccessPolicy::read_only()),
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("Permission denied for 'addV' (Create Data(vertex \"x\"))"),
        "{err}"
    );
    assert!(
        err.contains("GraphSource::empty().addV(\"x\").next()"),
        "{err}"
    );
    assert!(!err.contains("add_v"), "{err}");
    // An option error has no step: the spelling travels in `TraverserError::Spelled`.
    let lock = graph.read();
    let err = lock
        .traversal()
        .with("render.spelling", "camel")
        .with("repeat.order", "sideways")
        .v(None)
        .to_list()
        .unwrap_err();
    assert!(matches!(&*err, TraverserError::Spelled { .. }), "{err}");
    assert_eq!(err.spelling(), Spelling::Camel);
    assert!(matches!(
        err.root_cause(),
        TraverserError::InvalidOption { .. }
    ));
    assert!(err.step_location().is_none());
}

/// Type names in argument errors are the display names of the DSL (finding 12), also in Rhai's
/// own "function not found" errors.
#[test]
#[cfg(feature = "script")]
fn argument_errors_name_dsl_types() {
    use crate::script_host::eval;
    for (query, expected) in [
        (
            r#"g.inject(1).and(P.lte(1)).toList()"#,
            "Data type incorrect: Predicate",
        ),
        (r#"g.inject(1).to(1, Merge.inV)"#, "to (int64, Merge)"),
        (r#"g.inject(1).execute(1.5).error"#, "got float64"),
    ] {
        let graph = std::sync::Arc::new(GraphSource::tinkerpop_modern());
        let text = match eval(graph, query) {
            Ok(value) => value.to_string(),
            Err(err) => err.to_string(),
        };
        assert!(text.contains(expected), "{query}: {text}");
        assert!(!text.contains("graphersal::"), "{query}: {text}");
        assert!(
            !text.contains("f64") && !text.contains("i64"),
            "{query}: {text}"
        );
    }
}

/// A result that cannot be materialized (a vertex dropped earlier in the query, returned by the
/// last step) is located at that last step, like an error inside the run.
#[test]
fn a_result_that_cannot_be_materialized_is_located_at_the_last_step() {
    let graph = GraphSource::tinkerpop_modern();
    let mut lock = graph.write();
    let err = lock
        .traversal_mut()
        .v(None)
        .optional(__::drop())
        .to_list()
        .unwrap_err();
    assert_eq!(err.step_location().expect("located").step_id, "1");
    let rendered = format_diagnostic(&err);
    assert!(
        rendered.contains("at #1: v().optional(__.drop())"),
        "{rendered}"
    );
    assert!(
        rendered.contains("Caused by: The vertex was removed earlier in this query"),
        "{rendered}"
    );
}

/// The commit sequence never wraps: the help explains where the high number came from and that
/// the unit left nothing behind.
#[test]
fn commit_sequence_overflow_help_names_the_cause_and_the_rollback() {
    let mut graph = TraversalGraph::new().with_last_commit_seq(u64::MAX);
    let err = graph.traversal_mut().add_v("a").to_list().unwrap_err();
    assert!(err.to_string().contains("commit sequence number"), "{err}");
    let help = err.help().expect("help");
    assert!(
        help.contains("TraversalGraph::with_last_commit_seq(u64::MAX)"),
        "{help}"
    );
    assert!(
        help.contains("rolled back and left nothing behind"),
        "{help}"
    );
}

/// An explicit numeric id at the end of the range exhausts the auto-id sequence: the help shows
/// the id that does it and the explicit-id way out.
#[test]
fn auto_id_sequence_overflow_help_suggests_explicit_ids() {
    let mut graph = TraversalGraph::new();
    let mark = graph.begin_unit();
    graph
        .add_vertex(
            Some(u64::MAX.to_string().into()),
            vec![],
            PropertyMap::default(),
        )
        .unwrap();
    graph.commit_unit(mark).unwrap();
    let err = graph.traversal_mut().add_v("a").to_list().unwrap_err();
    assert!(
        err.root_cause()
            .to_string()
            .contains("vertex auto-id sequence"),
        "{err}"
    );
    let help = err.help().expect("help");
    assert!(
        help.contains("g.add_v(\"person\").property(T.id, \"18446744073709551615\")"),
        "{help}"
    );
    assert!(
        help.contains("g.add_v(\"person\").property(T.id, \"p-1\")"),
        "{help}"
    );
}

/// The help of every saved-query error names the fix with a runnable call
/// (`book/src/users_guide/saved_queries.md`; rule 6).
#[cfg(feature = "script")]
#[test]
fn saved_query_errors_carry_their_help() {
    use crate::script_host::eval_value;

    let graph = std::sync::Arc::new(GraphSource::tinkerpop_modern());
    let fail = |script: &str| -> String {
        eval_value(graph.clone(), script)
            .expect_err(script)
            .to_string()
    };
    let _ = eval_value(
        graph.clone(),
        r#"g.define_query(#{name: "top", params: #{limit: #{schema: #{type: "integer", minimum: 1}, default: 2}},
               body: "fn top(limit) { g.v().limit(limit) }"});
           g.define_query(#{name: "again", body: "fn again() { g.query(\"again\") }"});"#,
    )
    .unwrap();
    // A read-only query (a query that writes runs a returned writing traversal itself).
    graph
        .write()
        .traversal_mut()
        .define_query(
            "writer",
            graphersal::catalog::QueryDefinition::new("fn writer() { g.v().drop() }"),
        )
        .unwrap();

    let cases: [(&str, &[&str]); 10] = [
        (
            r#"g.v().query("top")"#,
            &[
                r#"query("top") can only start a traversal, it cannot follow other steps"#,
                r#"Help: Call a saved query at the start of a traversal, on g: g.query("top")"#,
            ],
        ),
        (
            r#"g.query("again")"#,
            &[
                "Saved query calls nested deeper than 16 levels",
                "Help: Saved queries may call each other at most 16 levels deep",
            ],
        ),
        (
            r#"g.query("top", #{lim: 1})"#,
            &[
                "Saved query 'top' has no parameter 'lim'",
                r#"Help: Arguments are named after the parameters of 'top': limit. For example g.query("top", #{limit: ..})"#,
            ],
        ),
        (
            r#"g.query("top", #{limit: 0})"#,
            &[
                "Invalid argument 'limit' for saved query 'top'",
                r#"Help: An argument is a data value"#,
                r#"g.get_query("top")["params"] shows the schema of 'limit'"#,
            ],
        ),
        (
            r#"g.define_query(#{name: "x", body: "fn y() { 1 }"})"#,
            &[
                "Invalid body of saved query 'x'",
                "Help: The body of a saved query is the text of ONE Rhai function",
            ],
        ),
        (
            r#"g.query("writer")"#,
            &[
                "Saved query 'writer' returned a traversal with a writing step (the result)",
                "Help: A read-only saved query cannot write, also not through a traversal it returns",
            ],
        ),
        (
            r#"g.query("top").property("k", 1).to_list()"#,
            &[
                "a saved query is read-only",
                "Help: A read-only saved query",
            ],
        ),
        (
            r#"g.query("nope")"#,
            &[
                "There is no query named 'nope'",
                "Help: List the existing definitions first: g.queries() in the DSL",
            ],
        ),
        (
            r#"g.query("top", 5)"#,
            &[
                "Argument mismatch for step 'query'",
                r#"Help: Saved queries take maps and strings"#,
            ],
        ),
        (
            r#"g.define_query(#{name: "q", body: "fn q() { 1 }"}); g.query("q", #{x: 1})"#,
            &[
                "Saved query 'q' has no parameter 'x'",
                r#"Help: 'q' takes no arguments: call it as g.query("q")"#,
            ],
        ),
    ];
    for (script, expected) in cases {
        let report = fail(script);
        for part in expected {
            assert!(
                report.contains(part),
                "expected `{part}` for {script} in:\n{report}"
            );
        }
    }

    let _ = eval_value(
        graph.clone(),
        r#"g.define_query(#{name: "need", params: #{n: "integer"}, body: "fn need(n) { n }"})"#,
    )
    .unwrap();
    let report = fail(r#"g.query("need")"#);
    assert!(
        report.contains(r#"Help: Pass every parameter without a default by name, for example g.query("need", #{n: ..})"#),
        "{report}"
    );
}

/// Saved queries written in GQL (`language: "gql"`): a GQL error at define keeps its diagnostic
/// and names the query; undeclared and unused `$parameters`, an unknown language and a stored
/// GQL body this version refuses carry their help.
#[cfg(all(feature = "script", feature = "gql"))]
#[test]
fn gql_saved_query_errors_carry_their_help() {
    use crate::script_host::eval_value;

    let graph = std::sync::Arc::new(GraphSource::tinkerpop_modern());
    let fail = |script: &str| -> String {
        eval_value(graph.clone(), script)
            .expect_err(script)
            .to_string()
    };
    graph
        .write()
        .traversal_mut()
        .define_query(
            "broken",
            graphersal::catalog::QueryDefinition::new("MATCH (p) RETRUN p")
                .with_dialect(graphersal::catalog::GQL_QUERY_DIALECT),
        )
        .unwrap();
    let cases: [(&str, &[&str]); 6] = [
        (
            r#"g.define_query(#{name: "bad", language: "gql", body: "MATCH (p:person) RETRUN p"})"#,
            &[
                "GQL syntax error at line 1, column 18: unexpected `RETRUN`",
                "GQLSTATUS:    42001",
                "  in saved query: \"bad\"\nHelp: Did you mean RETURN?",
            ],
        ),
        (
            r#"g.define_query(#{name: "u", language: "gql", body: "MATCH (p:person {name: $who}) RETURN p.age AS age"})"#,
            &[
                "Saved query 'u' reads the parameter $who, which its params do not declare",
                r#"Help: Declare every $name the GQL query reads in params"#,
                r#"g.define_query(#{name: "u", language: "gql", params: #{who: "string"}, body: ..}); it declares none."#,
            ],
        ),
        (
            r#"g.define_query(#{name: "x", language: "gql", params: #{who: "string", extra: "integer"},
                   body: "MATCH (p:person {name: $who}) RETURN p.age AS age"})"#,
            &[
                "Saved query 'x' declares the parameter 'extra', which its GQL query never reads",
                "Help: Every declared parameter is read by the GQL query as $extra",
            ],
        ),
        (
            r#"g.define_query(#{name: "x", language: "sql", body: "SELECT 1"})"#,
            &[
                r#"expected language "rhai" (the default) or "gql", got the language 'sql'"#,
                r#"A query written in GQL takes language: "gql" and the query text as its body"#,
            ],
        ),
        (
            r#"g.query("broken")"#,
            &[
                "Invalid body of saved query 'broken': it is not a valid GQL query: GQL syntax error",
                r#"A query written in GQL is saved with language: "gql""#,
            ],
        ),
        (
            r#"g.define_query(#{name: "ro", language: "gql", body: "MATCH (p:person) RETURN p"});
               g.query("ro").select("p").drop().iterate()"#,
            &[
                "a saved query is read-only",
                "Help: A read-only saved query (dialect graphersal-rhai/1 or graphersal-gql/1",
            ],
        ),
    ];
    for (script, expected) in cases {
        let report = fail(script);
        for part in expected {
            assert!(
                report.contains(part),
                "expected `{part}` for {script} in:\n{report}"
            );
        }
    }
}

/// Ids are strings: an order predicate on a number in `has_id(P)` is a plan-time error located
/// on the step, whose help shows the string form and the string order; equality keeps numbers.
#[test]
fn numeric_order_predicate_on_ids_is_an_error() {
    let graph = GraphSource::tinkerpop_modern();
    let locked = graph.read();
    let err = locked
        .traversal()
        .v(None)
        .has_id_p(P::Gt(3))
        .id()
        .to_list()
        .expect_err("ids are strings");
    let report = format_diagnostic(&err);
    assert!(
        report.contains(
            "has_id(P.gt(3)) compares element ids with a number, but element ids are strings"
        ),
        "{report}"
    );
    assert!(
        report.contains(r#"Compare with a string instead: has_id(P.gt("3"))"#),
        "{report}"
    );
    assert!(report.contains(r#""10" sorts before "9""#), "{report}");

    // located even inside a child traversal that no traverser reaches
    let err = locked
        .traversal()
        .v("nope")
        .where_t(__::has_id_p(P::Between(1, 5)))
        .to_list()
        .expect_err("ids are strings");
    assert_eq!(err.step_location().expect("located").step_id, "1.0.0");
    let report = format_diagnostic(&err);
    assert!(
        report.contains(r#"has_id(P.between("1", "5"))"#),
        "{report}"
    );

    // string order predicates keep working
    let ids = locked
        .traversal()
        .v(None)
        .has_id_p(P::Gt("3"))
        .to_list()
        .expect("valid");
    assert_eq!(ids.len(), 3);
}

/// The same check through the DSL: both spellings and the `has(T.id, P)` token form.
#[cfg(feature = "script")]
#[test]
fn numeric_order_predicate_on_ids_is_an_error_in_the_dsl() {
    use crate::script_host::eval_value;
    let graph = std::sync::Arc::new(GraphSource::tinkerpop_modern());
    for (script, step, fix) in [
        (
            "g.v().has_id(P.gt(3)).to_list()",
            "has_id(P.gt(3))",
            r#"has_id(P.gt("3"))"#,
        ),
        (
            "g.V().hasId(P.lte(3)).toList()",
            "has_id(P.lte(3))",
            r#"has_id(P.lte("3"))"#,
        ),
        (
            "g.V().has(T.id, P.inside(1, 4)).toList()",
            "has_id(P.inside(1, 4))",
            r#"has_id(P.inside("1", "4"))"#,
        ),
        (
            "g.e().has(T.id, P.outside(1, 4)).to_list()",
            "has_id(P.outside(1, 4))",
            r#"has_id(P.outside("1", "4"))"#,
        ),
        (
            r#"g.with("render.spelling", "camel").V().where(__.hasId(P.not(P.gt(2)))).toList()"#,
            "hasId(P.not(P.gt(2)))",
            r#"hasId(P.not(P.gt("2")))"#,
        ),
    ] {
        let err = eval_value(graph.clone(), script)
            .expect_err("ids are strings")
            .to_string();
        assert!(
            err.contains(&format!("{step} compares element ids with a number")),
            "{script}: {err}"
        );
        assert!(
            err.contains(&format!("Compare with a string instead: {fix}")),
            "{script}: {err}"
        );
    }
    for (script, count) in [
        ("g.v().has_id(3).count().next()", 1),
        ("g.V().hasId(P.eq(3)).count().next()", 1),
        ("g.V().hasId(P.within(1, 2)).count().next()", 2),
        ("g.v().has(T.id, 3).count().next()", 1),
        ("g.v().has(T.id, P.eq(3)).count().next()", 1),
        (r#"g.v().has_id(P.gt("3")).count().next()"#, 3),
    ] {
        let value = eval_value(graph.clone(), script).expect(script);
        assert_eq!(value.as_int().ok(), Some(count), "{script}");
    }
}

/// A label step applied to the wrong element kind names the step for the kind that arrived:
/// `set_label()` on a vertex points to `add_label()`/`drop_label()`, the vertex steps on an edge
/// point to `set_label()`; a value that is no element names both.
#[cfg(feature = "script")]
#[test]
fn label_steps_on_the_wrong_element_kind_name_the_right_step() {
    use crate::script_host::eval_value;

    let graph = std::sync::Arc::new(GraphSource::tinkerpop_modern());
    for (script, message, help) in [
        (
            r#"g.v(1).set_label("x").toList()"#,
            "The set_label() step takes an edge, but got a value of type 'vertex'",
            "set_label() changes the label of an edge; a vertex carries a label set, changed \
             with add_label() and drop_label(), for example \
             g.v().has_label(\"person\").add_label(\"employee\")",
        ),
        (
            r#"g.e(0).add_label("x").toList()"#,
            "The add_label() step takes a vertex, but got a value of type 'edge'",
            "add_label() changes the label set of a vertex; an edge has exactly one label, \
             changed with set_label(), for example g.e().has_label(\"knows\").set_label(\"met\")",
        ),
        (
            r#"g.e(0).dropLabel("x").toList()"#,
            "The drop_label() step takes a vertex, but got a value of type 'edge'",
            "drop_label() changes the label set of a vertex; an edge has exactly one label",
        ),
        (
            r#"g.inject(1).add_label("x").toList()"#,
            "The add_label() step takes a vertex, but got a value of type 'int64'",
            "add_label() changes the label of a graph element, not of a value of type 'int64'",
        ),
    ] {
        let err = eval_value(graph.clone(), script)
            .expect_err(script)
            .to_string();
        assert!(err.contains(message), "{script}: {err}");
        assert!(err.contains(help), "{script}: {err}");
    }
    // Nothing changed: each failing traversal rolled back.
    let labels = eval_value(graph, r#"g.v(1).labels().next()"#).unwrap();
    assert_eq!(labels.to_string(), r#"["person"]"#);
}

/// The help of a `closed` freeze blocked by unlabeled elements names the label steps.
#[cfg(feature = "script")]
#[test]
fn closed_freeze_with_unlabeled_elements_suggests_the_label_steps() {
    use crate::script_host::eval_value;

    let graph = std::sync::Arc::new(GraphSource::empty());
    let err = eval_value(
        graph,
        r#"g.add_v().next(); g.add_v("a").next(); g.set_schema(g.infer_schema(), SchemaMode.closed)"#,
    )
    .expect_err("an unlabeled vertex blocks a closed freeze")
    .to_string();
    for fragment in [
        "g.v(\"id\").add_label(\"Label\") for a vertex",
        "g.e(\"id\").set_label(\"label\") for an edge",
        "g.v().not(__.has_label()).add_label(\"Label\")",
        "then g.set_schema(g.infer_schema(), SchemaMode.closed)",
    ] {
        assert!(err.contains(fragment), "missing `{fragment}` in: {err}");
    }
}

/// `GraphError::Unsupported`'s help fits every operation a storage may refuse: it names the
/// operation, says the storage does not implement it, and points to the storage's documentation
/// and capabilities; it does not talk about one operation (commit hooks) as if it were the
/// only one.
#[test]
fn unsupported_help_names_the_operation_and_fits_every_storage() {
    use graphersal::error::{DiagnosticError, GraphError};
    use graphersal::storage::GraphStorage;

    // A query: a mark through the read-only `&G` view of a graph.
    let graph = GraphSource::tinkerpop_modern();
    let locked = graph.read();
    let mut view: &TraversalGraph = &locked;
    let err = GraphStorage::mark(&mut view, "m").expect_err("a read-only view has no journal");
    let help = err.help().expect("help");
    assert!(
        help.contains("does not implement marks on a read-only view"),
        "{help}"
    );

    // The operations a foreign storage leaves at the trait's defaults.
    for operation in [
        "compressed properties",
        "schemas",
        "definitions",
        "commit hooks",
    ] {
        let help = GraphError::unsupported(operation).help().expect("help");
        for fragment in [
            format!("The graph's storage does not implement {operation}:"),
            "the storage's documentation".to_string(),
            "GraphStorage::capabilities()".to_string(),
            "TraversalGraph, the reference storage".to_string(),
        ] {
            assert!(
                help.contains(&fragment),
                "{operation}: missing `{fragment}` in: {help}"
            );
        }
    }
}

/// A bounded visualizer appends `limit(n + 1) [display]` to the plan it runs; an error location
/// shows the user's query without it, in the query as written, the executed plan and the step
/// location, and the help's rewritten query does not carry it either.
#[test]
fn error_locations_leave_out_the_display_limit() {
    let graph = GraphSource::tinkerpop_modern();
    let locked = graph.read();
    let err = locked
        .traversal()
        .v(None)
        .has_label("person")
        .math("value + 1")
        .to_table_with(&graphersal::exec::RenderOptions::new().with_max_rows(5))
        .expect_err("value is not a math() variable");
    let report = format_diagnostic(&err);
    assert!(
        report.contains("at #1: v().has_label(\"person\").math(\"value + 1\")\n"),
        "{report}"
    );
    assert!(
        report.contains("plan:  v(labels: [\"person\"]).math(\"value + 1\")\n"),
        "{report}"
    );
    assert!(!report.contains("[display]"), "{report}");
    assert!(!report.contains("limit("), "{report}");
    assert!(
        report.contains(r#"g.v().has_label("person").math("_ + 1")"#),
        "{report}"
    );
    let location = err.step_location().expect("located");
    assert_eq!(location.step_id, "1");
    assert!(
        location
            .step_path
            .iter()
            .all(|entry| !entry.name.contains("[display]")),
        "{location:?}"
    );
}

/// The vector search errors (book page "Vectors") are located at their step and carry the help of
/// their cause.
#[test]
fn vector_search_errors_carry_their_help() {
    use graphersal::vector::{VectorElement, VectorMetric};

    let graph = GraphSource::empty();
    {
        let mut lock = graph.write();
        lock.traversal_mut()
            .add_v("doc")
            .property("emb", Vector::from_f32(vec![1.0, 0.0]).unwrap())
            .iterate()
            .unwrap();
    }
    let lock = graph.read();
    let q3 = Vector::from_f32(vec![1.0, 0.0, 0.0]).unwrap();
    let err = lock
        .traversal()
        .v(None)
        .nearest("emb", &q3, 5, None)
        .to_list()
        .expect_err("dimension mismatch");
    let report = format_diagnostic(&err);
    assert!(
        report.contains("nearest(\"emb\", vector([1.0, 0.0, 0.0]), 5)"),
        "{report}"
    );
    assert!(report.contains("vectors of dimensions 2 and 3"), "{report}");
    assert!(report.contains("\"dimension\": 384"), "{report}");
    assert_eq!(err.step_location().expect("located").step_id, "1");

    let bits = Vector::from_f32(vec![1.0, -1.0])
        .unwrap()
        .quantize(VectorElement::Bit)
        .unwrap();
    let err = lock
        .traversal()
        .v(None)
        .similarity("emb", bits, None)
        .to_list()
        .expect_err("bit against f32");
    let report = format_diagnostic(&err);
    assert!(
        report.contains("a bit vector compares only with another bit vector"),
        "{report}"
    );
    assert!(
        report.contains("nearest(\"emb\", q.quantize(\"bit\"), 10)"),
        "{report}"
    );

    let q2 = Vector::from_f32(vec![1.0, 0.0]).unwrap();
    let err = lock
        .traversal()
        .v(None)
        .similarity("emb", q2, Some(VectorMetric::Hamming))
        .to_list()
        .expect_err("hamming on f32");
    let report = format_diagnostic(&err);
    assert!(
        report.contains("the hamming metric compares bit vectors"),
        "{report}"
    );
    assert!(report.contains("quantize(\"bit\")"), "{report}");
}

#[test]
fn property_index_type_errors_carry_their_help() {
    use crate::script_host::eval_value;

    let graph = std::sync::Arc::new(GraphSource::tinkerpop_modern());
    let fail = |script: &str| -> String {
        eval_value(graph.clone(), script)
            .expect_err(script)
            .to_string()
    };
    let _ = eval_value(
        graph.clone(),
        r#"g.set_schema(#{mode: "open", vertices: #{doc: #{schema: #{type: "object", properties: #{
             emb: #{type: "array", format: "vector", element: "f32"},
             tags: #{type: ["array", "null"], items: #{type: "string"}}}}}}})"#,
    )
    .unwrap();

    let vector = fail(r#"g.create_index(#{label: "doc", key: "emb"})"#);
    for part in [
        "Cannot create a property index on vertex 'doc' key 'emb': the schema declares 'emb' \
         as vector<f32>, and an index of type 'memory-ordered' holds none of its values",
        "Help: Vectors are not index keys: search them with nearest() or similarity() (an exact \
         search, book page \"Vectors\"), for example \
         g.v().has_label(\"doc\").nearest(\"emb\", query, 10)",
        "a vector index type is planned",
        "a schema change made after an index exists never drops it",
    ] {
        assert!(vector.contains(part), "{part}\n{vector}");
    }
    let array = fail(r#"g.createIndex(#{label: "doc", key: "tags"})"#);
    for part in [
        "declares 'tags' as array<string>",
        "Help: An index of type 'memory-ordered' holds scalar values only",
        "Index a scalar top-level property instead",
        "g.v().has_label(\"doc\").property(\"tags_main\", ..)",
    ] {
        assert!(array.contains(part), "{part}\n{array}");
    }
    let unknown = fail(r#"g.create_index(#{label: "person", key: "age", type: "btree"})"#);
    for part in [
        "Invalid type of 'person_age': 'btree' is not an index type this version knows (known: \
         memory-ordered)",
        "Help: The index type says how the index keeps its entries. This version knows \
         \"memory-ordered\".",
        "g.create_index(#{label: \"person\", key: \"age\", type: \"memory-ordered\"})",
    ] {
        assert!(unknown.contains(part), "{part}\n{unknown}");
    }
}

#[test]
fn property_index_transform_errors_carry_their_help() {
    use crate::script_host::eval_value;

    let graph = std::sync::Arc::new(GraphSource::tinkerpop_modern());
    let fail = |script: &str| -> String {
        eval_value(graph.clone(), script)
            .expect_err(script)
            .to_string()
    };
    let _ = eval_value(
        graph.clone(),
        r#"g.set_schema(#{mode: "open", vertices: #{person: #{schema: #{type: "object", properties: #{
             age: #{type: ["integer", "null"]}, name: #{type: "string"}}}}}})"#,
    )
    .unwrap();

    let mismatch = fail(r#"g.create_index(#{label: "person", key: "age", transform: "to_lower"})"#);
    for part in [
        "Cannot create a property index on vertex 'person' key 'age': the schema declares 'age' \
         as int64, and an index of type 'memory-ordered' with the transform 'to_lower' holds \
         none of its values",
        "Help: An index with the transform 'to_lower' holds the lowered text of string values \
         only, for case-insensitive search with has(\"age\", __.to_lower().is(..))",
        "g.create_index(#{label: \"person\", key: \"age\"})",
        "a schema change made after an index exists never drops it",
    ] {
        assert!(mismatch.contains(part), "{part}\n{mismatch}");
    }
    // A string key is fine.
    let _ = eval_value(
        graph.clone(),
        r#"g.create_index(#{label: "person", key: "name", transform: "to_lower"})"#,
    )
    .unwrap();
    let unknown = fail(r#"g.createIndex(#{label: "person", key: "name", transform: "upper"})"#);
    for part in [
        "Invalid transform of 'person_name_upper': 'upper' is not an index transform this \
         version knows (known: to_lower)",
        "Help: The transform says what an index keys its values by. This version knows \
         \"to_lower\"",
        "g.v().has_label(\"person\").has(\"name\", __.to_lower().is(\"marko\"))",
    ] {
        assert!(unknown.contains(part), "{part}\n{unknown}");
    }
}

/// The embedding errors a query can trigger (book page "Vectors", section "Embeddings") are
/// located at the `embed` step and carry the help of their cause.
#[test]
fn embed_errors_carry_their_help() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();

    // An unknown embedder: found before the run, located at its step.
    let err = lock
        .traversal()
        .v(None)
        .embed("name", Some("acme/missing"))
        .to_list()
        .expect_err("unknown embedder");
    let report = format_diagnostic(&err);
    assert!(
        report.contains("there is no embedder \"acme/missing\""),
        "{report}"
    );
    assert!(
        report.contains(
            "g.embedders() lists the embedders this host registered (the built-in \
             \"hash-ngram/1\" is always there)"
        ),
        "{report}"
    );
    assert_eq!(err.step_location().expect("located").step_id, "1");

    // Not a string: no implicit conversion, as_string() named.
    let err = lock
        .traversal()
        .v(None)
        .values("age")
        .embed_value(None)
        .to_list()
        .expect_err("an int64 is no text");
    let report = format_diagnostic(&err);
    assert!(
        report.contains("embed() takes a string, not int64"),
        "{report}"
    );
    assert!(
        report.contains("g.v().values(\"age\").as_string().embed()"),
        "{report}"
    );
    assert_eq!(err.step_location().expect("located").step_id, "2");

    // Nothing to embed.
    let err = lock
        .traversal()
        .inject(vec![ElementProperty::from("...")])
        .embed_value(None)
        .to_list()
        .expect_err("no word");
    let report = format_diagnostic(&err);
    assert!(report.contains("has nothing to embed"), "{report}");
    assert!(
        report.contains("g.v().has(\"name\", P.neq(\"\")).embed(\"name\")"),
        "{report}"
    );
    assert!(matches!(
        err.root_cause(),
        TraverserError::Embed(graphersal::vector::EmbedError::NoFeatures { index: 0, .. })
    ));

    // A text where a vector is needed: the help names embed().
    let err = lock
        .traversal()
        .inject(vec![ElementProperty::from("graph")])
        .similarity_value(Vector::from_f32(vec![1.0]).unwrap(), None)
        .to_list()
        .expect_err("a text is no vector");
    let report = format_diagnostic(&err);
    assert!(
        report.contains("embed(\"graph indexes\") or g.v().embed(\"name\")"),
        "{report}"
    );
}

/// The provenance errors of `embed_property` and of the text queries of `nearest` /
/// `similarity` (book page "Vectors") carry the help of their cause; the text-query ones are
/// located at their step.
#[test]
fn embed_property_and_text_query_errors_carry_their_help() {
    use graphersal::exec::CancelToken;
    use graphersal::vector::{EmbedJob, Embedders};

    let embedders = Embedders::default();
    let run_job = |graph: &Graph, job: &EmbedJob| {
        let failed = graph
            .embed_property(job, &embedders, &CancelToken::new(), &mut |_| {})
            .expect_err("the job fails");
        format_diagnostic(&*failed.error)
    };
    let text_query = |graph: &Graph| {
        let lock = graph.read();
        lock.traversal()
            .v(None)
            .nearest("emb", "graph indexes", 2, None)
            .to_list()
            .expect_err("the text query fails")
    };

    // No schema attached.
    let graph = GraphSource::tinkerpop_modern();
    let report = run_job(&graph, &EmbedJob::new("person", "name", "emb"));
    assert!(
        report.contains("embed_property needs a schema attached"),
        "{report}"
    );
    assert!(
        report.contains("g.set_schema(g.infer_schema(), SchemaMode.open)"),
        "{report}"
    );
    let err = text_query(&graph);
    let report = format_diagnostic(&err);
    assert!(
        report.contains("nearest needs a schema attached"),
        "{report}"
    );
    assert!(
        report.contains("g.v().nearest(\"emb\", embed(\"graph indexes\"), 5)"),
        "{report}"
    );
    assert_eq!(err.step_location().expect("located").step_id, "1");

    // Attached, nothing declared.
    {
        let mut lock = graph.write();
        let mut g = lock.traversal_mut();
        let schema = g.infer_schema().unwrap().with_mode(SchemaMode::Open);
        g.set_schema(schema).unwrap();
    }
    let report = format_diagnostic(&text_query(&graph));
    assert!(report.contains("declares no embedder"), "{report}");
    assert!(
        report.contains("g.embed_property(#{label: \"doc\", from: \"text\", to: \"emb\"})"),
        "{report}"
    );

    // Declared for one model per label: another embedder, a text query over both.
    graph
        .embed_property(
            &EmbedJob::new("person", "name", "emb"),
            &embedders,
            &CancelToken::new(),
            &mut |_| {},
        )
        .unwrap();
    {
        let mut lock = graph.write();
        let mut schema = lock.get_schema().unwrap();
        let mut other = schema.vertices["person"].schema.properties["emb"].clone();
        if let Some(vector) = other.vector.as_mut() {
            vector.embedder = Some("acme/other@2".to_string());
        }
        schema.vertices["software"]
            .schema
            .properties
            .insert("emb".to_string(), other);
        lock.traversal_mut().set_schema(schema).unwrap();
    }
    let report = run_job(&graph, &EmbedJob::new("software", "name", "emb"));
    assert!(
        report.contains("vectors of the model \"acme/other@2\""),
        "{report}"
    );
    assert!(report.contains("Embed into another property"), "{report}");
    {
        let mut lock = graph.write();
        lock.traversal_mut()
            .v(None)
            .has_label("software")
            .property("emb", Vector::from_f32(vec![1.0; 256]).unwrap())
            .iterate()
            .unwrap();
    }
    {
        let lock = graph.read();
        let err = lock
            .traversal()
            .v(None)
            .has_label("software")
            .nearest("emb", "lop", 1, None)
            .to_list()
            .expect_err("no embedder of the model");
        let report = format_diagnostic(&err);
        assert!(
            report.contains("no registered embedder has the model \"acme/other@2\""),
            "{report}"
        );
        assert!(report.contains("--embedder NAME=REPO"), "{report}");
        assert!(
            report.contains("g.embedders() lists the registered ones"),
            "{report}"
        );
    }
    let report = format_diagnostic(&text_query(&graph));
    assert!(
        report.contains("one text query cannot search both"),
        "{report}"
    );
    assert!(
        report.contains("g.v().has_label(\"doc\").nearest(\"emb\", \"graph indexes\", 5)"),
        "{report}"
    );

    // The value form names the key form.
    let lock = graph.read();
    let err = lock
        .traversal()
        .v(None)
        .values("emb")
        .similarity_value("graph", None)
        .to_list()
        .expect_err("no key");
    let report = format_diagnostic(&err);
    assert!(
        report.contains("needs a top-level property name"),
        "{report}"
    );
    assert!(
        report.contains("g.v().similarity(\"emb\", \"graph indexes\")"),
        "{report}"
    );
}

/// A traversal given to a step that takes values (ids, labels, names, property keys, property
/// values) is an argument error naming the traversal, never a filter or name made of its type
/// name that silently matches nothing (issue #9); the help names the step's value forms and the
/// step that runs the traversal instead, and every suggested query gives the expected result.
#[cfg(feature = "script")]
#[test]
fn a_traversal_where_a_step_takes_values_is_refused() {
    use crate::script_host::{eval, eval_value};

    let graph = std::sync::Arc::new(GraphSource::tinkerpop_modern());
    let cases = [
        (
            r#"g.v().has_value(__.is("marko")).count().next()"#,
            "'has_value': expected a value or a predicate, got a traversal (__.is(",
            r#"g.v().filter(__.values().is("marko"))"#,
        ),
        (
            r#"g.v().has_value([__.out(), "x"]).count().next()"#,
            "'has_value': expected values, got a traversal (__.out())",
            r#"g.v().has("name", __.is("marko"))"#,
        ),
        (
            r#"g.v().has_label(__.is("person")).count().next()"#,
            "'has_label': expected values or a single predicate, got a traversal",
            r#"g.v().filter(__.label().is("person"))"#,
        ),
        (
            r#"g.v().hasLabel("person", __.out()).count().next()"#,
            "'has_label': expected values or a single predicate, got a traversal (__.out())",
            r#"g.v().filter(__.label().is("person"))"#,
        ),
        (
            r#"g.v().has_id(__.is("1")).count().next()"#,
            "'has_id': expected values or a single predicate, got a traversal",
            r#"g.v().filter(__.id().is("1"))"#,
        ),
        (
            r#"g.v().properties().has_key(__.is("name")).count().next()"#,
            "'has_key': expected values or a single predicate, got a traversal",
            r#"g.v().properties().filter(__.key().is("name"))"#,
        ),
        (
            r#"g.v().has(__.out(), 1).count().next()"#,
            "'has': expected a property key (a string or a jpath(..)), got a traversal",
            r#"g.v().has("name", __.is("marko"))"#,
        ),
        (
            r#"g.v().has_not(__.out()).count().next()"#,
            "'has_not': expected a property key (a string or a jpath(..)), got a traversal",
            r#"g.v().has_not("age")"#,
        ),
        (
            r#"g.v().out(__.out()).count().next()"#,
            "'out': expected names (strings), got a traversal (__.out())",
            r#"g.v().out_e().where(__.in_v().has("name", "josh")).in_v()"#,
        ),
        (
            r#"g.v().inE("knows", __.out()).count().next()"#,
            "'in_e': expected names (strings), got a traversal",
            r#"g.v().out("knows", "created")"#,
        ),
        (
            r#"g.v().to(Direction.OUT, [__.out()]).count().next()"#,
            "'to': expected names (strings), got a traversal",
            r#"g.v().project("n").by(__.out().count())"#,
        ),
        (
            r#"g.v().values(__.out()).count().next()"#,
            "'values': expected a property key (a string or a jpath(..)), got a traversal",
            r#"g.v().project("name", "friends").by("name").by(__.out("knows").count())"#,
        ),
        (
            r#"g.v().properties("name", __.out()).count().next()"#,
            "'properties': expected a property key (a string or a jpath(..)), got a traversal",
            r#"g.v().values(jpath("meta.k"))"#,
        ),
        (
            r#"g.v().element_map(__.out()).next()"#,
            "'element_map': expected a property key (a string or a jpath(..)), got a traversal",
            r#"g.v().values("name")"#,
        ),
        (
            r#"g.v().as(__.out()).count().next()"#,
            "'as': expected names (strings), got a traversal",
            r#"g.v().as("a").out().as("b").select("a", "b")"#,
        ),
        (
            r#"g.v().as("a").select("a", __.out()).next()"#,
            "'select': expected names (strings), got a traversal",
            r#"g.v("1").as("a").select(__.constant("a"))"#,
        ),
        (
            r#"g.v().project(__.out()).by("name").next()"#,
            "'project': expected names (strings), got a traversal",
            r#"g.v().project("n").by(__.out().count())"#,
        ),
        (
            r#"g.v().cap(__.out()).next()"#,
            "'cap': expected names (strings), got a traversal",
            r#"g.v().project("name").by("name")"#,
        ),
        (
            r#"g.add_v([__.out()]).next()"#,
            "'add_v': expected names (strings), got a traversal",
            r#"g.inject(1).add_v(__.constant("person"))"#,
        ),
    ];
    for (query, error, fix) in cases {
        let report = eval(graph.clone(), query)
            .expect_err(&format!("{query} must fail"))
            .to_string();
        assert!(report.contains(error), "{query}: {report}");
        assert!(
            report.contains(fix),
            "{query}: the help must name {fix} in: {report}"
        );
    }

    // The suggested queries run and give what the failing ones meant.
    for (query, expected) in [
        (r#"g.v().filter(__.values().is("marko")).count().next()"#, 1),
        (r#"g.v().has("name", __.is("marko")).count().next()"#, 1),
        (r#"g.v().filter(__.label().is("person")).count().next()"#, 4),
        (r#"g.v().filter(__.id().is("1")).count().next()"#, 1),
        (
            r#"g.v().properties().filter(__.key().is("name")).count().next()"#,
            6,
        ),
        (r#"g.v().has_not("age").count().next()"#, 2),
        (
            r#"g.v().out_e().where(__.in_v().has("name", "josh")).in_v().count().next()"#,
            1,
        ),
        (r#"g.v().out("knows", "created").count().next()"#, 6),
        (
            r#"g.v().project("name", "friends").by("name").by(__.out("knows").count()).count().next()"#,
            6,
        ),
        (
            r#"g.v().as("a").out().as("b").select("a", "b").count().next()"#,
            6,
        ),
        (
            r#"g.v("1").as("a").select(__.constant("a")).count().next()"#,
            1,
        ),
        (
            r#"g.inject(1).add_v(__.constant("person")).count().next()"#,
            1,
        ),
    ] {
        let count = eval_value(graph.clone(), query)
            .unwrap_or_else(|err| panic!("{query}: {err}"))
            .as_int()
            .unwrap_or_else(|_| panic!("{query}: not an integer"));
        assert_eq!(count, expected, "{query}");
    }
}

/// A compression rule's threshold out of range: the help keeps the range and names
/// `estimate_compression` (issue #12), whose suggested call runs.
#[cfg(feature = "script")]
#[test]
fn a_min_bytes_out_of_range_help_names_estimate_compression() {
    let mut graph = TraversalGraph::new();
    let rule = graphersal::catalog::CompressionDefinition::new(
        graphersal::storage::ElementKind::Vertex,
        "customer",
        "cv",
    )
    .unwrap()
    .with_min_bytes(0);
    let err = graph
        .traversal_mut()
        .define_compression("cv", rule)
        .unwrap_err();
    let help = err.help().expect("help");
    assert!(
        help.contains("min_bytes is the shortest string (in UTF-8 bytes) that is compressed"),
        "{help}"
    );
    assert!(
        help.contains(r#"g.estimate_compression(#{label: "customer", path: "cv"})"#),
        "{help}"
    );
    let shared = std::sync::Arc::new(Graph::new(graph));
    let estimate = crate::script_host::eval_value(
        shared,
        r#"g.estimate_compression(#{label: "customer", path: "cv"})"#,
    )
    .unwrap();
    assert!(estimate.is_map());
}

/// A dictionary on a `dedup` rule (issue #13): the help says a dictionary belongs to lz4 and
/// shows the dedup rule without it, and that call runs; the help of an out-of-range threshold
/// names the dedup default and the dedup estimate.
#[cfg(feature = "script")]
#[test]
fn a_dictionary_on_a_dedup_rule_help_names_the_fix() {
    use graphersal::catalog::{Codec, CompressionDefinition};
    use graphersal::storage::ElementKind;
    let mut graph = TraversalGraph::new();
    let rule = CompressionDefinition::new(ElementKind::Vertex, "symbol", "path")
        .unwrap()
        .with_codec(Codec::Dedup)
        .with_dictionary(true);
    let err = graph
        .traversal_mut()
        .define_compression("symbol_path", rule)
        .unwrap_err();
    let help = err.help().expect("help");
    assert!(
        help.contains(r#"A dictionary belongs to codec "lz4""#),
        "{help}"
    );
    let fix = r#"g.define_compression(#{name: "symbol_path", label: "symbol", path: "path", codec: "dedup"})"#;
    assert!(help.contains(fix), "{help}");
    let shared = std::sync::Arc::new(Graph::new(graph));
    let _ = crate::script_host::eval_value(shared.clone(), fix).unwrap();
    assert_eq!(shared.read().definitions().count(), 1);

    let rule = CompressionDefinition::new(ElementKind::Vertex, "symbol", "path")
        .unwrap()
        .with_codec(Codec::Dedup)
        .with_min_bytes(0);
    let err = TraversalGraph::new()
        .traversal_mut()
        .define_compression("p", rule)
        .unwrap_err();
    let help = err.help().expect("help");
    assert!(help.contains(r#"25 for codec "dedup""#), "{help}");
    assert!(help.contains(r#"codec: "dedup""#), "{help}");
}

/// GQL diagnostics (feature `gql`): every error kind with its caret excerpt, GQLSTATUS and the
/// `help()` that names the fix, and the camelCase rendering that leaves the GQL text alone.
#[cfg(feature = "gql")]
mod gql {
    use graphersal::error::GqlErrorKind;
    use graphersal::exec::Spelling;
    use graphersal::prelude::*;

    /// The failed GQL traversal of `build` on the modern graph: the error and its diagnostic.
    fn gql_failure(
        build: impl FnOnce(
            GraphTraversalSource<'_, &TraversalGraph>,
        ) -> Result<Vec<ElementProperty>, Box<TraverserError>>,
    ) -> (Box<TraverserError>, String) {
        let graph = GraphSource::tinkerpop_modern();
        let locked = graph.read();
        let err = build(locked.traversal()).expect_err("the GQL query must fail");
        let report = format_diagnostic(&err);
        (err, report)
    }

    /// The diagnostic of `g.gql(query)` on the modern graph.
    fn gql_report(query: &str) -> String {
        gql_failure(|g| g.gql(query).to_list()).1
    }

    /// The kind and GQLSTATUS of the GQL error at the root of `err`.
    fn kind_and_status(err: &TraverserError) -> (GqlErrorKind, String) {
        match err.root_cause() {
            TraverserError::Gql {
                kind, gqlstatus, ..
            } => (*kind, gqlstatus.clone()),
            other => panic!("expected a GQL error, got {other}"),
        }
    }

    #[test]
    fn gql_syntax_error_on_line_two_lists_the_expected_tokens_under_a_caret() {
        let (err, report) = gql_failure(|g| {
            g.gql("MATCH (p:person)\nWHERE p.age 30\nRETURN p.name")
                .to_list()
        });
        assert_eq!(
            kind_and_status(&err),
            (GqlErrorKind::Syntax, "42001".to_string())
        );
        // The diagnostic leads with the GQL error, not with the internal marker step.
        assert!(
            report.starts_with(
                "Error: GQL syntax error at line 2, column 13: unexpected `30` (step #1 \
                 'gql:error(\"42001\")' refused the query before the traversal ran)"
            ),
            "{report}"
        );
        assert!(!report.contains("Caused by: GQL"), "{report}");
        assert!(
            report.contains("  line 2:    WHERE p.age 30\n                         ^^"),
            "the caret must sit under `30` on line 2: {report}"
        );
        assert!(
            report.contains(
                "  expected:  MATCH, OPTIONAL MATCH, WITH, LET, FILTER, FOR, UNWIND, CALL, RETURN"
            ),
            "{report}"
        );
        assert!(report.contains("  GQLSTATUS: 42001"), "{report}");
        assert!(
            report.contains(
                "Help: Expected MATCH, OPTIONAL MATCH, WITH, LET, FILTER, FOR, UNWIND, CALL, \
                 RETURN at the marked position. Fix the query at the marked position: a GQL read query is \
                 MATCH \
                 <pattern> [WHERE <condition>], then any further MATCH, OPTIONAL MATCH, WITH, \
                 LET or FILTER clauses, then RETURN <items>, for example \
                 g.gql(\"MATCH (p:person) WHERE p.age > 30 RETURN p.name\") or \
                 g.gql(\"MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s) WITH p, count(s) \
                 AS n RETURN p.name, n\")."
            ),
            "{report}"
        );
    }

    #[test]
    fn gql_misspelt_keyword_and_function_get_did_you_mean() {
        let report = gql_report("MATCH (p:person) RETRUN p.name");
        assert!(
            report.contains(
                "  query:        MATCH (p:person) RETRUN p.name\n                                 ^^^^^^"
            ),
            "{report}"
        );
        assert!(report.contains("  did you mean: RETURN"), "{report}");
        assert!(report.contains("Help: Did you mean RETURN? "), "{report}");

        let report = gql_report("MATCH (p:person) RETURN lowr(p.name)");
        assert!(report.contains("unknown function lowr()"), "{report}");
        assert!(report.contains("  did you mean: lower()"), "{report}");
        assert!(report.contains("Help: Did you mean lower()? "), "{report}");
    }

    #[test]
    fn gql_foreach_is_not_yet_supported_with_the_for_form() {
        let (err, report) = gql_failure(|g| {
            g.gql("MATCH (p:person) FOREACH (x IN [1] | SET p.old = true) RETURN p")
                .to_list()
        });
        assert_eq!(
            kind_and_status(&err),
            (GqlErrorKind::NotYet, "0A000".to_string())
        );
        assert!(
            report.contains("FOREACH is not yet supported in this version"),
            "{report}"
        );
        assert!(
            report.contains(
                "Help: write FOR x IN <list> followed by the write clause, for example \
                 FOR name IN ['ann', 'bob'] INSERT (:person {name: name})."
            ),
            "{report}"
        );
    }

    #[test]
    fn gql_undirected_edge_is_refused_with_the_either_direction_form() {
        let (err, report) = gql_failure(|g| g.gql("MATCH (a)~[e]~(b) RETURN a").to_list());
        assert_eq!(kind_and_status(&err).0, GqlErrorKind::Refused);
        assert!(
            report.contains("  query:     MATCH (a)~[e]~(b) RETURN a\n                      ^"),
            "{report}"
        );
        assert!(
            report.contains(
                "Help: edges in Graphersal are directed; use -[e]- for either direction."
            ),
            "{report}"
        );
    }

    #[test]
    fn gql_missing_return_suggests_return_star() {
        let (err, report) = gql_failure(|g| g.gql("MATCH (p:person)").to_list());
        assert_eq!(kind_and_status(&err).0, GqlErrorKind::Syntax);
        assert!(
            report.contains("missing RETURN: a GQL query ends with a RETURN statement"),
            "{report}"
        );
        assert!(
            report.contains(
                "Help: end the query with a RETURN statement, such as RETURN * (every \
                 variable) or RETURN p."
            ),
            "{report}"
        );
    }

    #[test]
    fn gql_cypher_id_names_element_id() {
        let (err, report) = gql_failure(|g| g.gql("MATCH (p:person) RETURN id(p)").to_list());
        assert_eq!(
            kind_and_status(&err),
            (GqlErrorKind::Refused, "0A000".to_string())
        );
        assert!(
            report.contains(
                "  query:     MATCH (p:person) RETURN id(p)\n                                     ^^"
            ),
            "{report}"
        );
        assert!(
            report.contains("Help: use element_id(p), the element's id as a string."),
            "{report}"
        );
    }

    #[test]
    fn gql_quantified_path_pattern_past_the_loop_limit_names_the_pattern() {
        // Stage 2e.T4: parenthesized path patterns run (the "not yet" this test asserted
        // before is gone); a loop limit of one names the pattern, read back from its body.
        let (err, report) = gql_failure(|g| {
            g.with("repeat.max_loops", 1)
                .gql("MATCH (a) ((x)-[e:knows]-(m)-[f]-(y)){1,} (b) RETURN b")
                .to_list()
        });
        assert!(
            matches!(
                err.root_cause(),
                TraverserError::RepeatLimitExceeded { max_loops: 1, .. }
            ),
            "{report}"
        );
        assert!(
            report.contains(
                "Help: The loop of the quantified path pattern (()-[e:knows]-()-[f]-()){1,} of \
                 the gql() query ran 1 iterations, the limit of repeat.max_loops. A quantifier \
                 without an upper bound ({m,}, *, + or Cypher [*]) repeats the pattern until \
                 every path has used all the edges it can reach (trail)"
            ),
            "{report}"
        );
        assert!(
            report.contains("as in (()-[e:knows]-()-[f]-()){1,5}, for example"),
            "{report}"
        );
    }

    /// The MATCH examples (`MATCH .. RETURN <var>`) written in an alternative, in order.
    fn match_examples(help: &str) -> Vec<String> {
        let mut examples = Vec::new();
        let mut rest = help;
        while let Some(at) = rest.find("MATCH (") {
            let tail = &rest[at..];
            let Some(ret) = tail.find(" RETURN ") else {
                break;
            };
            let after = &tail[ret + " RETURN ".len()..];
            let end = after
                .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.'))
                .unwrap_or(after.len());
            let example = tail[..ret + " RETURN ".len() + end].trim_end_matches('.');
            examples.push(example.to_string());
            rest = &tail[ret..];
        }
        examples
    }

    #[test]
    fn gql_help_names_quantified_path_patterns_where_they_fix_the_error() {
        // Stage 2e.T5, rule 6: a quantified path pattern reads an edge together with its nodes
        // in every iteration, and repeats a pattern a quantifier after a node cannot.
        let graph = GraphSource::tinkerpop_modern();
        let locked = graph.read();
        for (query, names) in [
            (
                "MATCH (a)-[e WHERE e.weight > a.age]->{1,3}(b) RETURN b",
                "or read the edge and its nodes together in every iteration of a quantified \
                 path pattern, as in MATCH (a) ((x)-[e:knows]->(y) WHERE x.age < y.age){1,3} \
                 (b) RETURN b, or test the list of edges in the WHERE of the MATCH clause",
            ),
            (
                "MATCH (a){2}-->(b) RETURN b",
                "quantify the edge, for example MATCH (a)-[:knows]->{1,3}(b) RETURN b, or repeat \
                 a pattern with its nodes as a quantified path pattern, for example MATCH (a) \
                 ((x:person)-[:knows]->(y)){1,3} (b) RETURN b",
            ),
        ] {
            let help = gql_help(query);
            assert!(help.contains(names), "{query}: {help}");
            let examples = match_examples(names);
            assert!(!examples.is_empty(), "{names}");
            for example in examples {
                let rows = locked.traversal().gql(&example).to_list();
                assert!(rows.is_ok(), "{example}: {rows:?}");
            }
        }
        // The refusals inside a parenthesized path pattern (E17) stay, each with an alternative
        // that runs as written.
        for (query, what) in [
            (
                "MATCH (a) (((x)-->(y)){2}){1,3} (b) RETURN b",
                "a parenthesized path pattern nested in another one",
            ),
            (
                "MATCH (a) ((x)-[:knows]->{2}(y)){1,3} (b) RETURN b",
                "a quantified edge inside a parenthesized path pattern",
            ),
            (
                "MATCH (a) (p = (x)-->(y)){1,3} (b) RETURN b",
                "a path variable inside a parenthesized path pattern",
            ),
            (
                "MATCH (a) ((x)-->(y), (y)-->(z)){1,3} (b) RETURN b",
                "several path patterns inside a parenthesized path pattern",
            ),
            (
                "MATCH (a) ((x)){1,3} (b) RETURN b",
                "a parenthesized path pattern without an edge",
            ),
        ] {
            let (err, report) = gql_failure(|g| g.gql(query).to_list());
            assert_eq!(
                kind_and_status(&err),
                (GqlErrorKind::NotYet, "0A000".to_string()),
                "{report}"
            );
            assert!(
                report.contains(&format!("{what} is not yet supported")),
                "{report}"
            );
            let help = err.help().expect("a GQL error has help");
            assert!(!help.contains("not yet supported"), "{help}");
            for example in match_examples(&help) {
                let rows = locked.traversal().gql(&example).to_list();
                assert!(rows.is_ok(), "{query}: {example}: {rows:?}");
            }
        }
        // A parenthesized path pattern itself is no longer refused anywhere a pattern stands.
        for query in [
            "MATCH (a) ((x)-->(y)){1,3} (b) RETURN b",
            "MATCH (a) WHERE (a) (()-->()){1,2} RETURN a",
            "MATCH (a) RETURN [(a) ((x)-->(y)){1,2} | y] AS l",
            "MATCH (a) RETURN [((x)-->(y))? (a) | x] AS l",
            "MATCH (a) WHERE EXISTS { (a) ((x)-->(y))+ } RETURN a",
            "MATCH (a) OPTIONAL MATCH (a) ((x)-->(y)){2} (b) RETURN a, b",
            "MATCH (a) CALL (a) { MATCH (a) ((x)-->(y)) (b) RETURN b } RETURN b",
        ] {
            let rows = locked.traversal().gql(query).to_list();
            assert!(rows.is_ok(), "{query}: {rows:?}");
        }
    }

    #[test]
    fn gql_nesting_past_the_bound_is_a_limit_error() {
        let query = format!("MATCH (p) RETURN {}1{}", "(".repeat(2000), ")".repeat(2000));
        let (err, report) = gql_failure(|g| g.gql(&query).to_list());
        assert_eq!(
            kind_and_status(&err),
            (GqlErrorKind::Limit, "54001".to_string())
        );
        assert!(report.contains("the query nests deeper than"), "{report}");
        assert!(
            report.contains("Help: The query nests deeper than gql() accepts."),
            "{report}"
        );
    }

    #[test]
    fn gql_match_past_the_edge_limit_suggests_clauses_or_repeat() {
        // 65 edge patterns in one MATCH, one past the limit.
        let query = format!("MATCH (a){}-[e]->(z) RETURN z", "-->()".repeat(64));
        let (err, report) = gql_failure(|g| g.gql(&query).to_list());
        assert_eq!(
            kind_and_status(&err),
            (GqlErrorKind::Limit, "54001".to_string())
        );
        assert!(
            report.contains("the MATCH clause has more than 64 edge patterns"),
            "{report}"
        );
        assert!(
            report.contains(
                "Help: split the pattern into several MATCH clauses that share a variable, for \
                 example MATCH (a)-->(b)-->(c) MATCH (c)-->(d)-->(e) RETURN e (edge uniqueness \
                 then holds within each clause only), repeat a chain as a quantified path \
                 pattern, whose body edges count once however often it repeats, for example \
                 MATCH (a) (()-->()){65} (z) RETURN z, or walk a long path with Gremlin \
                 repeat(), for example g.v().repeat(__.out()).times(100). The query is larger \
                 than gql() accepts."
            ),
            "{report}"
        );
    }

    #[test]
    fn gql_long_operator_chain_help_leads_with_the_in_advice() {
        let terms: Vec<String> = (0..600).map(|i| format!("p.a{i} = 1")).collect();
        let query = format!("MATCH (p) WHERE {} RETURN p", terms.join(" OR "));
        let (err, report) = gql_failure(|g| g.gql(&query).to_list());
        assert_eq!(
            kind_and_status(&err),
            (GqlErrorKind::Limit, "54001".to_string())
        );
        assert!(
            report.contains("Help: split long AND / OR chains, and test many alternatives with IN"),
            "{report}"
        );
        assert!(
            report.contains("The query is larger than gql() accepts."),
            "{report}"
        );
        assert!(!report.contains("nests deeper"), "{report}");
    }

    #[test]
    fn gql_undefined_variable_suggests_the_declared_one() {
        let (err, report) = gql_failure(|g| {
            g.gql("MATCH (p:person)-[:knows]->(f) RETURN p.name, g.name")
                .to_list()
        });
        assert_eq!(
            kind_and_status(&err),
            (GqlErrorKind::Semantic, "42002".to_string())
        );
        assert!(
            report.contains("the variable `g` is not defined"),
            "{report}"
        );
        assert!(report.contains("  did you mean: p"), "{report}");
        assert!(
            report.contains(
                "Help: Did you mean p? Declare every variable in a MATCH pattern before WHERE, \
                 RETURN or ORDER BY uses it"
            ),
            "{report}"
        );
    }

    #[test]
    fn gql_missing_parameter_help_repeats_the_query() {
        let report = gql_report("MATCH (p:person) RETURN p.name LIMIT $n");
        assert!(
            report.contains("the parameter `$n` has no value"),
            "{report}"
        );
        assert!(
            report.contains(
                "Help: pass it in the parameters of gql(), as in \
                 g.gql(\"MATCH (p:person) RETURN p.name LIMIT $n\", #{n: value})."
            ),
            "{report}"
        );
    }

    #[test]
    fn gql_division_by_zero_fails_at_run_time_with_22012() {
        let (err, report) = gql_failure(|g| {
            g.gql("MATCH (p:person) RETURN 100 / (p.age - 29)")
                .to_list()
        });
        assert_eq!(
            kind_and_status(&err),
            (GqlErrorKind::Data, "22012".to_string())
        );
        // Raised by the row it failed on: located at the projecting step, not before the run.
        assert!(
            report.starts_with(
                "Error: Step #1 'gql:project(\"100 / (p.age - 29)\")' execution failed"
            ),
            "{report}"
        );
        assert!(
            report.contains("Caused by: GQL data exception at line 1, column 25: division by zero"),
            "{report}"
        );
        assert!(
            report.contains(
                "  query:     MATCH (p:person) RETURN 100 / (p.age - 29)\n                                     ^^^^^^^^^^^^^^^^^^"
            ),
            "{report}"
        );
        assert!(report.contains("  GQLSTATUS: 22012"), "{report}");
        assert!(
            report.contains("Help: A row of the data made the marked expression fail."),
            "{report}"
        );
    }

    #[test]
    fn gql_limit_reading_the_row_names_a_constant_expression() {
        let report = gql_report("MATCH (p:person) RETURN p.name LIMIT p.age");
        assert!(
            report.contains(
                "LIMIT must be a non-negative integer, a parameter or a constant expression, \
                 not an expression that reads the row"
            ),
            "{report}"
        );
        assert!(
            report.contains(
                "Help: LIMIT 10, LIMIT $n or an expression without variables, for example \
                 LIMIT toInteger(ceil(1.7))."
            ),
            "{report}"
        );
        assert!(
            report.contains(
                "An expression without variables is evaluated once, as in \
                 g.gql(\"MATCH (p:person) RETURN p.name LIMIT toInteger(ceil(1.7))\"); one \
                 that reads the row (LIMIT p.age) is refused."
            ),
            "{report}"
        );
    }

    #[test]
    fn gql_bad_limit_parameter_names_a_runnable_value() {
        let (err, report) = gql_failure(|g| {
            g.gql_with("MATCH (p:person) RETURN p.name LIMIT $n", [("n", -1)])
                .to_list()
        });
        assert_eq!(
            kind_and_status(&err),
            (GqlErrorKind::Semantic, "22G02".to_string())
        );
        assert!(
            report.contains("LIMIT must be a non-negative integer, but $n is negative"),
            "{report}"
        );
        assert!(
            report.contains(
                "Help: give $n a value such as 10, as in \
                 g.gql(\"MATCH (p:person) RETURN p.name LIMIT $n\", #{n: 10}). OFFSET and \
                 LIMIT take a non-negative integer: a literal, or a $parameter given such a \
                 value in the parameters of gql(), for example \
                 g.gql(\"MATCH (p:person) RETURN p.name ORDER BY p.name LIMIT $n\", #{n: 2})."
            ),
            "{report}"
        );
        // The advice does not talk about undeclared variables.
        assert!(!report.contains("Declare every variable"), "{report}");
        // A literal of the wrong kind gets the same advice.
        let report = gql_report("MATCH (p:person) RETURN p.name LIMIT 1.5");
        assert!(
            report.contains("OFFSET and LIMIT take a non-negative integer"),
            "{report}"
        );
    }

    #[test]
    fn gql_after_other_steps_names_the_query_as_a_start() {
        let (err, report) = gql_failure(|g| g.v(None).gql("MATCH (p) RETURN p").to_list());
        assert_eq!(kind_and_status(&err).0, GqlErrorKind::NotYet);
        assert!(
            report.contains(
                "Help: start the traversal with it, as in g.gql(\"MATCH (p) RETURN p\"), and \
                 write what the steps before it selected as the MATCH pattern and its WHERE \
                 condition; Gremlin steps may follow it, as in \
                 g.gql(\"MATCH (p:person) RETURN p\").select(\"p\").out(\"created\")."
            ),
            "{report}"
        );
    }

    /// Under `render.spelling = camel` the GQL excerpt, the GQL message and the GQL functions
    /// in the advice stay as written (`element_id(p)`, never `elementId(p)`), so the caret stays
    /// under its span; the Gremlin steps of the advice and the plan are respelled.
    #[test]
    fn gql_text_is_not_respelled_under_camel_case() {
        let camel = |query: &'static str| {
            gql_failure(move |g| g.with("render.spelling", "camel").gql(query).to_list())
        };
        // Before the run (the marker).
        let (err, report) = camel("MATCH (p:person) WHERE element_id(p) = '1' RETRUN p");
        assert_eq!(err.spelling(), Spelling::Camel);
        assert!(
            report.contains(
                "  query:        MATCH (p:person) WHERE element_id(p) = '1' RETRUN p\n                                                           ^^^^^^"
            ),
            "{report}"
        );
        assert!(!report.contains("elementId"), "{report}");
        // At run time (a row fails): the plan is respelled, the GQL text is not.
        let (_, report) = camel("MATCH (p:person) RETURN element_id(p), 1 / (p.age - 29)");
        assert!(
            report.contains("v().hasLabel(\"person\").gql:project("),
            "{report}"
        );
        assert!(
            report.contains(
                "  query:     MATCH (p:person) RETURN element_id(p), 1 / (p.age - 29)\n                                                    ^^^^^^^^^^^^^^^^"
            ),
            "{report}"
        );
        assert!(!report.contains("elementId"), "{report}");
        // The advice: GQL functions as written, Gremlin steps respelled.
        let (err, _) = camel("MATCH (p:person) RETURN id(p)");
        let help = err.help().expect("help");
        assert!(
            help.starts_with("use element_id(p), the element's id"),
            "{help}"
        );
        let (err, _) = camel("MATCH (p:person) RETURN p EXCEPT MATCH (p:software) RETURN p");
        let help = err.help().expect("help");
        assert!(help.contains("hasLabel("), "{help}");
    }

    /// The same through Rhai: `g.with("render.spelling", "camel").gql(..)`.
    #[test]
    #[cfg(feature = "script")]
    fn gql_text_is_not_respelled_under_camel_case_in_rhai() {
        let graph = std::sync::Arc::new(GraphSource::tinkerpop_modern());
        let report = crate::script_host::eval(
            graph,
            r#"g.with("render.spelling", "camel").gql("MATCH (p:person) RETURN element_id(p), char_length(p.name) / 0").toList()"#,
        )
        .expect_err("division by zero")
        .to_string();
        assert!(
            report.contains("RETURN element_id(p), char_length(p.name) / 0\n"),
            "{report}"
        );
        assert!(!report.contains("elementId"), "{report}");
        assert!(!report.contains("charLength"), "{report}");
    }

    /// A parameter map of the wrong type names both forms of `gql()`.
    #[test]
    #[cfg(feature = "script")]
    fn gql_argument_mismatch_help_names_the_parameter_map() {
        for script in [
            r#"g.gql("MATCH (p) RETURN p", 5)"#,
            r#"g.gql("MATCH (p) RETURN p", #{x: g.v()})"#,
        ] {
            let graph = std::sync::Arc::new(GraphSource::tinkerpop_modern());
            let report = crate::script_host::eval(graph, script)
                .expect_err(script)
                .to_string();
            assert!(
                report.contains("Argument mismatch for step 'gql'"),
                "{report}"
            );
            assert!(
                report.contains(
                    "Help: gql() takes the query as a string and, optionally, the values of \
                     its $parameters as a map from name to value, for example \
                     g.gql(\"MATCH (p:person {name: $name}) RETURN p.age\", #{name: \
                     \"marko\"})"
                ),
                "{report}"
            );
        }
        // Both examples of the advice run.
        let graph = std::sync::Arc::new(GraphSource::tinkerpop_modern());
        let ages = crate::script_host::eval_value(
            graph.clone(),
            r#"g.gql("MATCH (p:person {name: $name}) RETURN p.age", #{name: "marko"}).to_list()"#,
        )
        .unwrap();
        assert_eq!(ages.into_array().unwrap().len(), 1);
        let names = crate::script_host::eval_value(
            graph,
            r#"let n = g.v().count().next(); g.gql("MATCH (p:person) RETURN p.name LIMIT $n", #{n: n}).to_list()"#,
        )
        .unwrap();
        assert_eq!(names.into_array().unwrap().len(), 4);
    }

    // ----- stage 1b: help sweep (rule 6) -----

    /// The help of the GQL error `g.gql(query)` raises on the modern graph.
    fn gql_help(query: &str) -> String {
        let (err, _) = gql_failure(|g| g.gql(query).to_list());
        err.help().expect("a GQL error has help")
    }

    /// Every `g.gql("..")` example in `help`, with the `g.with(key, n)` options before it.
    fn gql_examples(help: &str) -> Vec<(Vec<(String, i64)>, String)> {
        let mut examples = Vec::new();
        let mut rest = help;
        while let Some(at) = rest.find("g.") {
            let mut tail = &rest[at + 2..];
            rest = tail;
            let mut options = Vec::new();
            while let Some(after) = tail.strip_prefix("with(\"") {
                let key_end = after.find('"').expect("a closed option key");
                let key = after[..key_end].to_string();
                let after = after[key_end + 1..].trim_start_matches(", ");
                let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
                let after = after[digits.len()..]
                    .strip_prefix(").")
                    .expect("an option value");
                options.push((key, digits.parse().expect("a number")));
                tail = after;
            }
            let Some(query) = tail.strip_prefix("gql(\"") else {
                continue;
            };
            let end = query.find("\")").expect("a closed example");
            examples.push((options, query[..end].replace("\\\"", "\"")));
        }
        examples
    }

    /// Runs every `g.gql(..)` example of `help` on the modern graph: each must succeed.
    fn assert_examples_run(help: &str) {
        let examples = gql_examples(help);
        assert!(!examples.is_empty(), "no g.gql(..) example in: {help}");
        let graph = GraphSource::tinkerpop_modern();
        let locked = graph.read();
        for (options, query) in examples {
            let mut g = locked.traversal();
            for (key, value) in &options {
                g = g.with(key.as_str(), *value);
            }
            let rows = g.gql(&query).to_list();
            assert!(rows.is_ok(), "{options:?} {query}: {rows:?}");
        }
    }

    #[test]
    fn gql_expected_tokens_name_every_clause_and_the_quantifiers() {
        let (err, report) = gql_failure(|g| g.gql("MATCH (a)-[e]->(b) RETRN a").to_list());
        assert_eq!(kind_and_status(&err).0, GqlErrorKind::Syntax);
        assert!(
            report.contains(
                "  expected:     `,`, WHERE, MATCH, OPTIONAL MATCH, WITH, LET, FILTER, FOR, \
                 UNWIND, CALL, RETURN"
            ),
            "{report}"
        );
        assert!(report.contains("  did you mean: RETURN"), "{report}");
        // The syntax advice describes the whole clause pipeline, and its examples run.
        let help = err.help().unwrap();
        assert!(
            help.contains(
                "a GQL read query is MATCH <pattern> [WHERE <condition>], then any further \
                 MATCH, OPTIONAL MATCH, WITH, LET or FILTER clauses, then RETURN <items>"
            ),
            "{help}"
        );
        assert_examples_run(&help);
        // After a node pattern too.
        let help = gql_help("MATCH (p:person) RETRUN p");
        assert!(
            help.contains(
                "Expected `,`, WHERE, MATCH, OPTIONAL MATCH, WITH, LET, FILTER, FOR, UNWIND, \
                 CALL, ... at the marked position."
            ),
            "{help}"
        );
        // A misspelt 1b clause gets its suggestion from the same list.
        let report = gql_report("MATCH (a) OPTINAL MATCH (a)-->(b) RETURN b");
        assert!(
            report.contains("  did you mean: OPTIONAL MATCH"),
            "{report}"
        );
        // WITH lists its own tail, then the clauses.
        let help = gql_help("MATCH (a) WITH a b RETURN a");
        assert!(
            help.starts_with(
                "Expected `,`, AS, ORDER BY, OFFSET, SKIP, LIMIT, WHERE, MATCH, OPTIONAL MATCH, \
                 WITH, ... at the marked position."
            ),
            "{help}"
        );
        // Cypher `*` inside the brackets, a GQL quantifier after the edge.
        let help = gql_help("MATCH (a)-[e:knows RETURN a");
        assert!(
            help.starts_with("Expected `*`, `{`, WHERE, `]` at the marked position."),
            "{help}"
        );
        let help = gql_help("MATCH (a)-[e]->x RETURN a");
        assert!(
            help.starts_with("Expected `{`, `*`, `+`, `(` at the marked position."),
            "{help}"
        );
    }

    #[test]
    fn gql_group_by_explains_implicit_grouping() {
        for (query, text) in [
            // Stage 1c accepts ISO GQL `GROUP BY` on RETURN; on WITH it stays explained.
            (
                "MATCH (p:person)-[:created]->(s) WITH p.name AS n, count(s) AS c GROUP BY n \
                 RETURN n, c",
                "GROUP BY",
            ),
            (
                "MATCH (p:person) WITH p.name AS n, count(*) AS c group by n RETURN n, c",
                "group by",
            ),
        ] {
            let (err, report) = gql_failure(|g| g.gql(query).to_list());
            assert_eq!(
                kind_and_status(&err),
                (GqlErrorKind::Syntax, "42001".to_string())
            );
            assert!(
                report.contains(
                    "GROUP BY is not needed: grouping is implicit, the RETURN or WITH items \
                     without an aggregate are the grouping keys"
                ),
                "{report}"
            );
            assert!(
                report.contains(&format!("{text} ")) && report.contains(" ^^^^^^^^\n  GQLSTATUS"),
                "the caret must underline {text}: {report}"
            );
            let help = err.help().unwrap();
            assert!(
                help.starts_with(
                    "remove GROUP BY and list the keys as items next to the aggregates, as in \
                     MATCH (p:person)-[:created]->(s) RETURN p.name, count(s). "
                ),
                "{help}"
            );
            assert_examples_run(&help);
        }
        let graph = GraphSource::tinkerpop_modern();
        let rows = graph
            .read()
            .traversal()
            .gql("MATCH (p:person)-[:created]->(s) RETURN p.name, count(s)")
            .to_list()
            .unwrap();
        assert_eq!(rows.len(), 3);
    }

    /// ISO GQL `GROUP BY` on RETURN (stage 1c): a key that is no plain item names the fix.
    #[test]
    fn gql_group_by_keys_name_the_fix() {
        let query = "MATCH (p:person)-[:created]->(s) RETURN p.name AS n, p.age AS a, count(s) \
                     GROUP BY n";
        let (err, report) = gql_failure(|g| g.gql(query).to_list());
        assert_eq!(
            kind_and_status(&err),
            (GqlErrorKind::Semantic, "42002".to_string())
        );
        assert!(
            report.contains("the RETURN item `a` has no aggregate"),
            "{report}"
        );
        let help = err.help().unwrap();
        assert!(
            help.starts_with(
                "add it to GROUP BY (GROUP BY ..., a), or remove GROUP BY: grouping is implicit, \
                 the items without an aggregate are the keys. "
            ),
            "{help}"
        );
        assert_examples_run(&help);
    }

    #[test]
    fn gql_with_and_let_scope_errors_explain_the_scope() {
        let scope = "WITH passes on only its columns: a variable it does not list is out of \
                     scope after it (only the WHERE of a WITH without an aggregate still reads \
                     it), so list every variable a later clause reads, as in \
                     g.gql(\"MATCH (p:person)-[:created]->(s) WITH p, count(s) AS n RETURN \
                     p.name, n\"); a WITH item other than a variable needs a name (AS). LET \
                     adds variables and keeps all others in scope, each with a new name, as in \
                     g.gql(\"MATCH (p:person) LET name = upper(p.name) RETURN p.age, name\").";
        let (err, report) = gql_failure(|g| g.gql("MATCH (a) WITH a.name AS n RETURN a").to_list());
        assert_eq!(
            kind_and_status(&err),
            (GqlErrorKind::Semantic, "42002".to_string())
        );
        assert!(
            report.contains("the variable `a` is not in scope: the WITH before does not carry it"),
            "{report}"
        );
        let help = err.help().unwrap();
        assert_eq!(
            help,
            format!("add `a` to the WITH items, as in WITH ..., a. {scope}")
        );
        // The generic advice about MATCH declarations is not given.
        assert!(!help.contains("Declare every variable"), "{help}");
        assert_examples_run(&help);

        let help = gql_help("MATCH (a) WITH a.age + 1 RETURN a");
        assert_eq!(
            help,
            format!("name it with AS, as in WITH a.age + 1 AS value. {scope}")
        );
        let help = gql_help("MATCH (a) LET x = 1 LET x = 2 RETURN x");
        assert_eq!(
            help,
            format!("choose another name, as in LET x2 = <value>. {scope}")
        );
        let help = gql_help("MATCH (a) LET x = 1, y = x + 1 RETURN y");
        assert!(help.ends_with(scope), "{help}");
    }

    /// E14 (a list-comprehension variable reusing a name) has its own advice, with a runnable
    /// example, not the WITH / LET scope advice.
    #[test]
    fn gql_comprehension_variable_errors_explain_new_names() {
        let advice = "A list comprehension, [x IN <list> WHERE <condition> | <expression>], \
                      binds its variable x to each item of the list, inside the comprehension \
                      only; x must be a new name, used nowhere else in the query: not a \
                      variable or column in scope, not the variable of a comprehension around \
                      it, and not a name that an earlier clause bound and dropped, as in \
                      g.gql(\"MATCH (p:person) RETURN p.name, [a IN [p.age, p.age + 1] WHERE a \
                      > 30 | a * 2] AS l\"). The same holds for the variable of all(), any(), \
                      none() and single() and for both variables of reduce(), as in \
                      g.gql(\"MATCH (p:person) RETURN p.name, all(a IN [p.age] WHERE a > 30) AS \
                      old, reduce(s = 0, a IN [p.age, 1] | s + a) AS total\").";
        for (query, name) in [
            ("MATCH (p:person) RETURN [p IN [1, 2] | p * 2] AS l", "p"),
            (
                "MATCH (p:person) WITH p.name AS n RETURN [p IN [1] | p] AS l",
                "p",
            ),
            ("RETURN [x IN [1] | [x IN [2] | x]] AS l", "x"),
        ] {
            let (err, _) = gql_failure(|g| g.gql(query).to_list());
            assert_eq!(
                kind_and_status(&err),
                (GqlErrorKind::Semantic, "42002".to_string())
            );
            let help = err.help().unwrap();
            assert_eq!(
                help,
                format!(
                    "give the comprehension a variable of its own, as in [{name}2 IN list WHERE \
                     {name}2 > 0 | {name}2]. {advice}"
                ),
                "{query}"
            );
            assert!(!help.contains("WITH passes on only"), "{help}");
            assert_examples_run(&help);
        }
    }

    #[test]
    fn gql_grouping_errors_explain_grouping_keys() {
        let grouping = "A RETURN or WITH with an aggregate groups the rows by its other items \
                        (the grouping keys): read a variable outside an aggregate only in such a \
                        key, and after it (as after DISTINCT) only its columns remain, so ORDER \
                        BY names a column or writes an item again inside an expression, as in \
                        g.gql(\"MATCH (p:person)-[:created]->(s) RETURN p.name, count(s) AS n \
                        ORDER BY n\") or g.gql(\"MATCH (p:person)-[:created]->(s) RETURN \
                        p.name, count(s) AS n ORDER BY count(s) * 10 + size(p.name)\"). An \
                        aggregate takes no \
                        aggregate as its argument; aggregate in two steps with WITH, as in \
                        g.gql(\"MATCH (p:person)-[:created]->(s) WITH p, count(s) AS n RETURN \
                        max(n)\").";
        let help = gql_help("MATCH (a) RETURN count(*), a.name ORDER BY a.age");
        assert_eq!(
            help,
            format!(
                "return the sort key too (RETURN ..., a.age) and order by it, or group by it \
                 too. {grouping}"
            )
        );
        assert_examples_run(&help);
        for query in [
            "MATCH (a)-->(b) RETURN a.name, count(b) + b.age",
            "MATCH (a) RETURN a.age, max(count(a))",
            "MATCH (a) WHERE count(a) > 1 RETURN a",
            "MATCH (a) RETURN a.name ORDER BY count(a)",
        ] {
            let help = gql_help(query);
            assert!(help.ends_with(grouping), "{query}: {help}");
            assert!(!help.contains("Declare every variable"), "{query}: {help}");
        }
    }

    #[test]
    fn gql_a_value_of_another_kind_in_a_pattern_names_the_fix() {
        // 3f.T3 B11: a LET / WITH value written as a node fails when a row reaches it.
        for (query, kind, example) in [
            (
                "MATCH (p:person) WITH collect(p.age) AS l WITH l[0] AS x MATCH (x)-->(y) \
                 RETURN y",
                "a node",
                "WITH n AS x MATCH (x)-->(m)",
            ),
            (
                "MATCH (p:person) WITH collect(p.age) AS l LET x = head(l) MATCH (y)<--(x) \
                 RETURN y",
                "a node",
                "WITH n AS x MATCH (x)-->(m)",
            ),
        ] {
            let (err, report) = gql_failure(|g| g.gql(query).to_list());
            assert_eq!(
                kind_and_status(&err),
                (GqlErrorKind::Data, "22G03".to_string()),
                "{query}"
            );
            assert!(
                report.contains(&format!(
                    "`x` is written as {kind} of the pattern but holds a value that is not {kind}"
                )),
                "{report}"
            );
            let help = err.help().unwrap();
            assert!(
                help.starts_with(&format!(
                    "give it {kind} (or NULL, which matches nothing), for example the node \
                     variable itself, as in {example}."
                )),
                "{help}"
            );
            assert!(help.contains("A row of the data made"), "{help}");
            assert_examples_run(&help);
        }
    }

    #[test]
    fn gql_pattern_variable_errors_explain_what_a_variable_names() {
        let pattern = "In a MATCH clause a node variable names one node (written twice, it joins \
                       on the same node), an edge variable one edge, a path variable the path \
                       after its `=`, the variable of a quantified edge the list of its \
                       edges, and a variable inside a quantified path pattern the list of its \
                       nodes or edges, one per iteration, as in g.gql(\"MATCH p = \
                       (a:person)-[e:knows]->(b) RETURN p, e, b.name\") and g.gql(\"MATCH \
                       (a:person) ((x)-[e:knows]->(y)){1,2} (b) RETURN a.name, y\"); a name \
                       defined by LET or WITH is a value, which a pattern may continue from \
                       when it holds a node or an edge, as in g.gql(\"MATCH (p:person) WITH \
                       collect(p) AS ps WITH ps[0] AS first MATCH (first)-->(x) RETURN \
                       x.name\").";
        let help = gql_help("MATCH (a)-[e]->(b), (c)-[e]->(d) RETURN a");
        assert_eq!(
            help,
            format!(
                "give the second edge its own variable, or put the second pattern in a MATCH \
                 clause of its own to join on the same edge `e`. {pattern}"
            )
        );
        assert_examples_run(&help);
        for query in [
            "MATCH (a)-[e]->(e) RETURN a",
            "MATCH p = (a)-->(b) MATCH p = (c)-->(d) RETURN p",
            "MATCH (a)-[e]->{1,2}(b) RETURN e.weight",
            "MATCH (a) WITH a.name AS n MATCH (n) RETURN n",
        ] {
            let help = gql_help(query);
            assert!(help.ends_with(pattern), "{query}: {help}");
        }
    }

    #[test]
    fn gql_duplicate_names_ask_for_a_name_of_its_own() {
        let names = "Give every column of a RETURN or WITH, every variable of a LET and every \
                     key of a map a name of its own, as in \
                     g.gql(\"MATCH (p:person) RETURN p.name AS name, p.age AS age\").";
        let help = gql_help("MATCH (a) RETURN a.name AS n, a.age AS n");
        assert_eq!(
            help,
            format!(
                "give one of the columns another name with AS, as in RETURN ... AS n2. {names}"
            )
        );
        assert_examples_run(&help);
        for query in [
            "MATCH (a) LET x = 1, x = 2 RETURN x",
            "MATCH (a) RETURN {k: 1, k: 2}",
        ] {
            let help = gql_help(query);
            assert!(help.ends_with(names), "{query}: {help}");
        }
    }

    #[test]
    fn gql_aggregate_of_the_wrong_kind_names_numeric_aggregates() {
        let (err, report) = gql_failure(|g| g.gql("MATCH (a) RETURN sum(a.name)").to_list());
        assert_eq!(
            kind_and_status(&err),
            (GqlErrorKind::Data, "22G03".to_string())
        );
        assert!(
            report.contains("sum() takes numbers, not a string"),
            "{report}"
        );
        let help = err.help().unwrap();
        assert_eq!(
            help,
            "A row of the data gave the marked aggregate a value it cannot add: sum() and avg() \
             take numbers only (count() and collect_list() take values of any kind). Aggregate \
             a numeric property, as in g.gql(\"MATCH (p:person) RETURN sum(p.age)\"), and keep \
             values of another kind out with a comparison to a number, which is NULL for text \
             and so drops the row, as in g.gql(\"MATCH (p) WHERE p.age >= 0 RETURN \
             avg(p.age)\"), or inside the aggregate, as in g.gql(\"MATCH (p) RETURN sum(CASE \
             WHEN p.age >= 0 THEN p.age END)\") (an aggregate skips NULL)."
        );
        assert_examples_run(&help);
        // avg() too; a failing argument keeps the general data advice.
        let help = gql_help("MATCH (a) RETURN avg(a.lang)");
        assert!(
            help.starts_with("A row of the data gave the marked aggregate"),
            "{help}"
        );
        let help = gql_help("MATCH (a:person) RETURN sum(1 / (a.age - 29))");
        assert!(
            help.starts_with("A row of the data made the marked expression fail."),
            "{help}"
        );
    }

    #[test]
    fn gql_repeat_limit_of_a_quantifier_names_the_quantifier() {
        let (err, report) = gql_failure(|g| {
            g.with("repeat.max_loops", 1)
                .gql("MATCH (a:person)-[e:knows]->{2,}(b) RETURN b")
                .to_list()
        });
        assert!(
            report.contains("Caused by: repeat() exceeded the loop limit of 1 iterations"),
            "{report}"
        );
        let help = err.help().unwrap();
        assert_eq!(
            help,
            "The loop of the quantified edge -[e:knows]->{2,} of the gql() query ran 1 \
             iterations, the limit of repeat.max_loops. A quantifier without an upper bound \
             ({m,}, *, + or Cypher [*]) repeats the edge until every path has used all the \
             edges it can reach (trail), which takes more iterations than the limit on a large \
             or densely connected graph; the result is never cut short. Give the quantifier an \
             upper bound, as in -[e:knows]->{2,10}, for example \
             g.gql(\"MATCH (a:person)-[:knows]->{1,3}(b) RETURN b.name\"). Or raise the limit \
             for the query, g.with(\"repeat.max_loops\", 2).gql(..), for example \
             g.with(\"repeat.max_loops\", 2).gql(\"MATCH (a)-[:knows]->{1,}(b) RETURN \
             b.name\"). It is an administrative option: a script whose host restricts it needs \
             Update on that Option to set it."
        );
        // The internal loop body is not offered as advice.
        assert!(!help.contains("gql:trail"), "{help}");
        assert!(!help.contains("glob_path"), "{help}");
        assert_examples_run(&help);
        // Other forms: `*` (lower bound 0), either direction, `<-`, Cypher `[*]`, a filter.
        for (query, edge) in [
            ("MATCH (a)-[:knows]->*(b) RETURN b", "-[:knows]->{0,}"),
            ("MATCH (a)-[e]-+(b) RETURN b", "-[e]-{1,}"),
            (
                "MATCH (a)<-[:knows|created]-{1,}(b) RETURN b",
                "<-[:knows|created]-{1,}",
            ),
            ("MATCH (a)-[e:knows*]->(b) RETURN b", "-[e:knows]->{1,}"),
            (
                "MATCH (a)-[e WHERE e.weight > 0.1]->{1,}(b) RETURN b",
                "-[e WHERE ..]->{1,}",
            ),
        ] {
            let (err, _) = gql_failure(|g| g.with("repeat.max_loops", 1).gql(query).to_list());
            let help = err.help().unwrap();
            assert!(
                help.starts_with(&format!("The loop of the quantified edge {edge} of")),
                "{query}: {help}"
            );
        }
        // Under camel spelling the advice reads the same.
        let (err, _) = gql_failure(|g| {
            g.with("render.spelling", "camel")
                .with("repeat.max_loops", 1)
                .gql("MATCH (a:person)-[e:knows]->{2,}(b) RETURN b")
                .to_list()
        });
        let help = err.help().unwrap();
        assert!(
            help.starts_with("The loop of the quantified edge -[e:knows]->{2,} of"),
            "{help}"
        );
    }

    #[test]
    fn gql_traverser_limit_gives_gql_advice() {
        let tail = "Narrow the pattern with labels and conditions, for example \
                    g.gql(\"MATCH (p:person)-[:knows]->(f) WHERE f.age > 30 RETURN f.name\"), \
                    or raise the limit for the query, g.with(\"traversal.max_traversers\", \
                    40).gql(..), for example g.with(\"traversal.max_traversers\", \
                    40).gql(\"MATCH (a)-->(b) RETURN count(*)\"). A host can lock this option \
                    with ExecutionPolicy::with_locked(\"traversal.max_traversers\"); \
                    9223372036854775807 is as good as unlimited. Setting it is an \
                    administrative option: a script whose host restricts it needs Update on \
                    that Option.";
        let (err, report) = gql_failure(|g| {
            g.with("traversal.max_traversers", 20)
                .gql("MATCH (a)-[e]-{1,}(b) RETURN count(*)")
                .to_list()
        });
        assert!(
            report.contains("Resource limit 'traversal.max_traversers' exceeded (limit 20"),
            "{report}"
        );
        let help = err.help().unwrap();
        assert_eq!(
            help,
            format!(
                "The gql() query kept more traversers (rows in the making) alive between two \
                 steps than 'traversal.max_traversers' allows (20). Each path a quantified edge \
                 (-[e]-{{1,}}) matches is a traverser of its own (trail keeps them apart, so \
                 they do not merge), and their number grows with every iteration: give it a \
                 lower upper bound, as in -[e]-{{1,5}}. {tail}"
            )
        );
        assert!(!help.contains("v().out().out()"), "{help}");
        assert_examples_run(&help);
        // A query without a quantifier gets the GQL advice without the quantifier sentence.
        let (err, _) = gql_failure(|g| {
            g.with("traversal.max_traversers", 3)
                .gql("MATCH (a)-->(b) RETURN b")
                .to_list()
        });
        let help = err.help().unwrap();
        assert!(
            help.starts_with(
                "The gql() query kept more traversers (rows in the making) alive between two \
                 steps than 'traversal.max_traversers' allows (3). Narrow the pattern"
            ),
            "{help}"
        );
        assert!(
            help.contains("g.with(\"traversal.max_traversers\", 6).gql(..)"),
            "{help}"
        );
    }

    /// D21 reverses C12 (stage 1c): `nodes(p)` / `relationships(p)` no longer carry the
    /// "over a path" refusal; their own diagnostics come with their implementation.
    #[test]
    fn gql_nodes_and_relationships_are_no_longer_refused() {
        for function in ["nodes", "relationships"] {
            let query = format!("MATCH p = (a)-->(b) RETURN {function}(p)");
            let graph = GraphSource::tinkerpop_modern();
            if let Err(error) = graph.read().traversal().gql(&query).to_list() {
                let text = error.to_string();
                assert!(!text.contains("over a path"), "{query}: {text}");
                assert!(!text.contains("gql:error"), "{query}: {text}");
            }
        }
    }

    /// Stage 2d.T5c: `COUNT { }` and `COLLECT { }` run (they were refused "not yet" with the
    /// `OPTIONAL MATCH` + `count()` / `collect_list()` forms as advice); each gives the rows of
    /// that form.
    #[test]
    fn gql_count_and_collect_subqueries_run_like_their_optional_match_forms() {
        let graph = GraphSource::tinkerpop_modern();
        let locked = graph.read();
        for (subquery, form) in [
            (
                "MATCH (p:person) RETURN p.name AS n, COUNT { (p)-[:created]->() } AS c",
                "MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s) RETURN p.name AS n, \
                 count(s) AS c",
            ),
            (
                "MATCH (p:person) RETURN p.name AS n, COLLECT { MATCH (p)-[:created]->(s) \
                 RETURN s.name } AS c",
                "MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s) RETURN p.name AS n, \
                 collect_list(s.name) AS c",
            ),
        ] {
            let mut left: Vec<String> = locked
                .traversal()
                .gql(subquery)
                .to_list()
                .unwrap()
                .iter()
                .map(|row| format!("{row:?}"))
                .collect();
            let mut right: Vec<String> = locked
                .traversal()
                .gql(form)
                .to_list()
                .unwrap()
                .iter()
                .map(|row| format!("{row:?}"))
                .collect();
            left.sort();
            right.sort();
            assert_eq!(left.len(), 4, "{subquery}");
            assert_eq!(left, right, "{subquery}");
        }
    }

    // ----- stage 1c: help sweep (rule 6) -----

    /// One test per new GQLSTATUS of stage 1c: a data exception (raised by the row it failed
    /// on), the caret under the offending argument, and a help that leads with the function
    /// that fixes it, keeps the general data advice, and whose examples run.
    #[test]
    fn gql_1c_data_statuses_name_the_fix() {
        for (expr, status, caret, advice) in [
            (
                "CAST(p.name AS INTEGER)",
                "22018",
                "p.name",
                "CAST is strict: text that is not a value of the target type fails the query. \
                 The Cypher conversions are lenient: toInteger(x), toFloat(x) and toBoolean(x) \
                 give NULL for such text, and toIntegerOrNull(x), toFloatOrNull(x) and \
                 toBooleanOrNull(x) give NULL for a value of any other kind too",
            ),
            (
                "substring(p.name, -1)",
                "22011",
                "-1",
                "substring(s, start, length), left(s, n) and right(s, n) take non-negative \
                 integers",
            ),
            (
                "left(p.name, p.age - 30)",
                "22011",
                "p.age - 30",
                "substring(s, start, length), left(s, n) and right(s, n)",
            ),
            (
                "TRIM('ab' FROM p.name)",
                "22027",
                "'ab'",
                "TRIM(c FROM s) removes one character c; to remove any of several characters \
                 use btrim(s, chars), ltrim(s, chars) or rtrim(s, chars)",
            ),
            (
                "ln(p.age - 29)",
                "2201E",
                "p.age - 29",
                "ln(x), log(x) and log10(x) take a positive number, and log(base, x) also a \
                 positive base other than 1",
            ),
            (
                "log(1, p.age)",
                "2201E",
                "1",
                "ln(x), log(x) and log10(x) take a positive number",
            ),
            (
                "sqrt(p.age - 30)",
                "2201F",
                "p.age - 30",
                "sqrt(x) takes a number >= 0, and power(x, y) cannot raise 0 to a negative power",
            ),
            (
                "power(p.age - 29, -1)",
                "2201F",
                "power(p.age - 29, -1)",
                "sqrt(x) takes a number >= 0",
            ),
            (
                "range(1, p.age, 0)",
                "22023",
                "0",
                "An argument is outside the values the function accepts: range() takes a step \
                 other than 0, round() a precision >= 0 and one of the modes UP, DOWN, CEILING, \
                 FLOOR, HALF_UP, HALF_DOWN or HALF_EVEN (upper case), replace() a limit >= 0 \
                 and the list trim(list, n) an n >= 0",
            ),
            (
                "round(p.age, 1, 'half_up')",
                "22023",
                "'half_up'",
                "An argument is outside",
            ),
            (
                "replace(p.name, 'a', 'b', -1)",
                "22023",
                "-1",
                "An argument is outside",
            ),
            ("trim([1, 2], -1)", "22023", "-1", "An argument is outside"),
        ] {
            let query = format!("MATCH (p:person {{name: 'marko'}}) RETURN {expr} AS x");
            let (err, report) = gql_failure(|g| g.gql(&query).to_list());
            assert_eq!(
                kind_and_status(&err),
                (GqlErrorKind::Data, status.to_string()),
                "{query}: {report}"
            );
            // The caret sits under the offending argument (or the call).
            let caret_line = report
                .lines()
                .skip_while(|line| !line.starts_with("  query:"))
                .take(2)
                .collect::<Vec<_>>();
            let [source, carets] = caret_line[..] else {
                panic!("no excerpt in {report}");
            };
            // Columns count characters (the excerpt may start with `…`).
            let start = carets.find('^').expect("a caret");
            let marked: String = source
                .chars()
                .skip(start)
                .take(caret.chars().count())
                .collect();
            assert_eq!(marked, caret, "{query}: {report}");
            let help = err.help().expect("help");
            assert!(help.starts_with(advice), "{query}: {help}");
            // The general data advice stays.
            assert!(
                help.ends_with(
                    "and compare values of one kind only (no implicit conversion between text \
                     and numbers)."
                ),
                "{query}: {help}"
            );
            assert_examples_run(&help);
        }
    }

    /// A subscript of the wrong kind (stage 3f: `x[i]`, `m[k]`, `x[a..b]`) names what each form
    /// reads and the conversions that fix the index (rule 6), with runnable examples; the
    /// caret marks the index or the whole subscript.
    #[test]
    fn gql_subscript_errors_name_to_integer_and_the_forms() {
        for (query, marked) in [
            ("WITH [1, 2] AS xs RETURN xs[1.5] AS x", "1.5"),
            ("MATCH (p:person) RETURN p[1] AS x", "1"),
            ("WITH {a: 1} AS m RETURN m[0] AS x", "0"),
            ("WITH 'abc' AS s RETURN s[0] AS x", "s[0]"),
            ("WITH 'abc' AS s RETURN s[0..1] AS x", "s[0..1]"),
            ("WITH [1] AS xs RETURN xs[..'a'] AS x", "'a'"),
        ] {
            let (err, report) = gql_failure(|g| g.gql(query).to_list());
            assert_eq!(
                kind_and_status(&err),
                (GqlErrorKind::Data, "22G03".to_string()),
                "{report}"
            );
            let caret_line = report
                .lines()
                .skip_while(|line| !line.trim_start().starts_with("query:"))
                .nth(1)
                .unwrap_or("");
            assert_eq!(
                caret_line.trim().len(),
                marked.chars().count(),
                "{query}: {report}"
            );
            let help = err.help().expect("help");
            assert!(
                help.starts_with(
                    "x[i] reads the item i of a list, with an integer i counted from 0 (negative \
                     from the end: x[-1] is the last item; out of range gives NULL); convert \
                     another number or a text first with toInteger(i), as in \
                     g.gql(\"RETURN [10, 20, 30][toInteger('1')] AS x\"). m[k] reads an entry of \
                     a map, or a property of a node or an edge, by a string key"
                ),
                "{query}: {help}"
            );
            assert!(help.contains("substring(s, start, length)"), "{help}");
            assert!(
                help.contains("A row of the data made the marked expression fail."),
                "{help}"
            );
            assert_examples_run(&help);
        }
    }

    /// `+` / `||` of text and a number or a boolean names the conversions to text (rule 6:
    /// stage 1c functions fix it); other kinds keep the general advice.
    #[test]
    fn gql_text_and_number_name_to_string_and_cast() {
        for expr in [
            "p.name + p.age",
            "p.age + p.name",
            "p.name || p.age",
            "p.name || true",
            "1.5 || p.name",
        ] {
            let query = format!("MATCH (p:person {{name: 'marko'}}) RETURN {expr} AS x");
            let (err, report) = gql_failure(|g| g.gql(&query).to_list());
            assert_eq!(
                kind_and_status(&err),
                (GqlErrorKind::Data, "22G03".to_string()),
                "{report}"
            );
            let help = err.help().expect("help");
            assert!(
                help.starts_with(
                    "Convert the number or boolean to text first, with toString(x) or CAST(x AS \
                     STRING), and join text with ||, as in g.gql(\"MATCH (p:person) RETURN \
                     p.name || ' is ' || toString(p.age) AS s\"); or compute with numbers only, \
                     converting text with toInteger(x) or toFloat(x). A row of the data made"
                ),
                "{query}: {help}"
            );
            assert_examples_run(&help);
        }
        // A list and a number: no text involved, the general advice only.
        for expr in ["[1] + p.age", "[1] || p.name", "p.age || 1"] {
            let help = gql_help(&format!("MATCH (p:person) RETURN {expr} AS x"));
            assert!(
                help.starts_with("A row of the data made the marked expression fail."),
                "{expr}: {help}"
            );
        }
        // The message itself is unchanged.
        let (_, report) = gql_failure(|g| {
            g.gql("MATCH (p:person {name: 'marko'}) RETURN p.name + 1")
                .to_list()
        });
        assert!(
            report.contains(
                "+ joins two strings or adds two numbers, not a string and a number; a number is \
                 never turned into text implicitly"
            ),
            "{report}"
        );
    }

    /// A value a GQL function or `collect_list()` would build over
    /// `traversal.max_materialized_bytes` is the resource-limit error (not a GQL root cause:
    /// the limit and its option are the engine's), with advice on the GQL query instead of
    /// the Gremlin materializing steps; the GQL function names survive camelCase.
    #[test]
    fn gql_materialization_limit_gives_gql_advice() {
        let tail = |raised: u64| {
            format!(
                "or raise the limit for the query, \
                 g.with(\"traversal.max_materialized_bytes\", {raised}).gql(..), for example \
                 g.with(\"traversal.max_materialized_bytes\", {raised}).gql(\"MATCH (p:person) \
                 RETURN collect_list(p.name) AS names\"). A host can lock this option with \
                 ExecutionPolicy::with_locked(\"traversal.max_materialized_bytes\"). Setting it \
                 is an administrative option: a script whose host restricts it needs Update on \
                 that Option."
            )
        };
        for query in [
            "MATCH (p:person {name: 'marko'}) RETURN range(1, 100000) AS r",
            "MATCH (p:person {name: 'marko'}) RETURN size(split(replace(p.name, '', p.name || \
             p.name, 9223372036854775807), '')) AS n",
            "MATCH (p), (q) RETURN collect_list(p.name) AS r",
            "MATCH (p) WHERE size(range(1, 100000)) > 0 RETURN p",
            "MATCH (p) WITH p ORDER BY toString(range(1, 100000)) RETURN p",
            // gql:aggregate: a grouping key, an aggregate's argument, an item over aggregates.
            "MATCH (p) RETURN size(range(1, 100000)) AS k, count(*) AS c",
            "MATCH (p) RETURN count(range(1, 100000)) AS c",
            "MATCH (p) RETURN size(range(1, count(*) * 100000)) AS c",
        ] {
            let (err, report) = gql_failure(|g| {
                g.with("traversal.max_materialized_bytes", 1_000)
                    .gql(query)
                    .to_list()
            });
            match err.root_cause() {
                TraverserError::ResourceLimitExceeded { limit, max, .. } => {
                    assert_eq!(limit, "traversal.max_materialized_bytes", "{query}");
                    assert_eq!(*max, Some(1_000), "{query}");
                }
                other => panic!("{query}: expected the limit error, got {other}"),
            }
            assert!(report.contains("Step #"), "{report}");
            let help = err.help().expect("help");
            assert!(
                help.starts_with(
                    "The gql() query built a value larger than \
                     'traversal.max_materialized_bytes' allows (1000 bytes); this one needed at \
                     least "
                ),
                "{query}: {help}"
            );
            assert!(
                help.contains(
                    "A function whose result can be larger than its arguments (range(), \
                     split(), replace(), || and toString() of a list or map) and the list of \
                     collect_list() are checked before they are built"
                ),
                "{help}"
            );
            assert!(help.ends_with(&tail(2_000)), "{query}: {help}");
            assert!(!help.contains("fold()"), "{help}");
            assert_examples_run(&help);
        }
        // Under camel spelling: the GQL function names as written, the Gremlin API respelled.
        let (err, _) = gql_failure(|g| {
            g.with("render.spelling", "camel")
                .with("traversal.max_materialized_bytes", 1_000)
                .gql("MATCH (p) RETURN collect_list(p.name) AS n, range(1, 1000) AS r")
                .to_list()
        });
        let help = err.help().expect("help");
        assert!(
            help.contains("the list of collect_list() are checked"),
            "{help}"
        );
        assert!(help.contains("ExecutionPolicy::withLocked("), "{help}");
        assert!(!help.contains("collectList"), "{help}");
        // A Gremlin step after gql() that materializes keeps the Gremlin advice.
        let (err, _) = gql_failure(|g| {
            g.with("traversal.max_materialized_bytes", 100_000)
                .gql("MATCH (p:person {name: 'marko'}) RETURN p.name AS n")
                .select("n")
                .repeat(__::path())
                .times(60)
                .to_list()
        });
        let help = err.help().expect("help");
        assert!(
            help.starts_with("A step had to materialize a value or buffer larger"),
            "{help}"
        );
    }

    /// The targeted not-yet / refused functions of stage 1c lead their help with the
    /// alternative (an LLM reads the Help line and retries), and a misspelt conversion is
    /// corrected.
    #[test]
    fn gql_1c_not_yet_functions_name_the_alternative() {
        for (expr, kind, message, advice) in [
            (
                "rand()",
                GqlErrorKind::Refused,
                "rand() is not supported: a non-deterministic function",
                "gql() functions are deterministic; sample rows with Gremlin after gql(), for \
                 example g.gql(\"MATCH (n) RETURN n\").sample(1), or coin(0.5).",
            ),
            (
                "sin(p.age)",
                GqlErrorKind::NotYet,
                "sin() (a trigonometric function) is not yet supported",
                "Gremlin math() after gql(), for example g.gql(\"MATCH (n:person) RETURN n.age \
                 AS a\").select(\"a\").math(\"sin(_)\").",
            ),
            (
                "date('2020-01-01')",
                GqlErrorKind::NotYet,
                "date() (a date or time function) is not yet supported",
                "dates and times are planned for version 0.2.0",
            ),
            (
                "toIntegerList(p.tags)",
                GqlErrorKind::NotYet,
                "toIntegerList() (a list conversion function) is not yet supported",
                "convert the items one by one in Gremlin after gql()",
            ),
            (
                "normalize(p.name)",
                GqlErrorKind::NotYet,
                "normalize() (Unicode normalization) is not yet supported",
                "store normalized text, or compare with lower() / upper().",
            ),
            (
                "elements(p)",
                GqlErrorKind::NotYet,
                "elements() (a GQL path function pending verification) is not yet supported",
                "nodes(p) and relationships(p) return the vertices and the edges of a path.",
            ),
            (
                "percentileCont(p.age, 0.5)",
                GqlErrorKind::NotYet,
                "percentileCont() (a statistical aggregate function) is not yet supported",
                "collect the values with collect_list(x) and compute it after gql().",
            ),
            (
                "p.age ^ 2",
                GqlErrorKind::NotYet,
                "the exponentiation operator `^` is not yet supported",
                "use power(x, y), as in power(2, 10).",
            ),
            (
                "exists(p.age)",
                GqlErrorKind::Refused,
                "exists(value), the Cypher 4 property test, is not supported",
                "test a property with n.p IS NOT NULL",
            ),
            (
                "SUBSTRING(p.name FROM 1 FOR 2)",
                GqlErrorKind::Syntax,
                "SUBSTRING(s FROM a FOR b) is SQL, not GQL",
                "use substring(s, start, length) with a 0-based start, as in \
                 substring('graph', 1, 3) = 'rap', or left(s, n) / right(s, n).",
            ),
        ] {
            let query = format!("MATCH (p:person) RETURN {expr} AS x");
            let (err, report) = gql_failure(|g| g.gql(&query).to_list());
            let (actual, _) = kind_and_status(&err);
            assert_eq!(actual, kind, "{query}: {report}");
            assert!(report.contains(message), "{query}: {report}");
            let help = err.help().expect("help");
            assert!(help.starts_with(advice), "{query}: {help}");
        }
        // The forms the advice names run.
        let graph = GraphSource::tinkerpop_modern();
        let locked = graph.read();
        for query in [
            "MATCH (p:person) RETURN power(2, 10) AS x",
            "MATCH (p:person) WHERE p.age IS NOT NULL RETURN p.name",
            "MATCH (p:person) RETURN substring('graph', 1, 3) = 'rap' AS x, left(p.name, 2) AS l",
            "MATCH x = (a)-->(b) RETURN nodes(x), relationships(x)",
        ] {
            assert!(locked.traversal().gql(query).to_list().is_ok(), "{query}");
        }
        // A misspelt conversion: "did you mean" names the Cypher spelling.
        let (err, report) = gql_failure(|g| g.gql("MATCH (p) RETURN toInt(p.age)").to_list());
        assert_eq!(
            kind_and_status(&err),
            (GqlErrorKind::Syntax, "42001".to_string())
        );
        assert!(report.contains("  did you mean: toInteger()"), "{report}");
        let help = err.help().expect("help");
        assert!(help.starts_with("Did you mean toInteger()? "), "{help}");
    }

    /// One camelCase rendering of the stage 1c advice: the GQL function names (also those that
    /// are Gremlin steps, `replace`, `split`, `range`, and the Cypher ones such as
    /// `toIntegerOrNull`) stay as written, the Gremlin steps of an alternative are respelled.
    #[test]
    fn gql_1c_advice_under_camel_case() {
        let camel = |query: &'static str| {
            let (err, _) =
                gql_failure(move |g| g.with("render.spelling", "camel").gql(query).to_list());
            err.help().expect("help")
        };
        let snake = |query: &'static str| gql_help(query);
        for query in [
            "MATCH (p:person {name: 'marko'}) RETURN CAST(p.name AS INTEGER)",
            "MATCH (p:person {name: 'marko'}) RETURN TRIM('ab' FROM p.name)",
            "MATCH (p:person {name: 'marko'}) RETURN range(1, 2, 0)",
            "MATCH (p:person {name: 'marko'}) RETURN p.name + 1",
        ] {
            assert_eq!(camel(query), snake(query), "{query}");
        }
        let help = camel("MATCH (p:person {name: 'marko'}) RETURN CAST(p.name AS INTEGER)");
        assert!(help.contains("toIntegerOrNull(x)"), "{help}");
        assert!(help.contains("toInteger(p.name)"), "{help}");
        // An alternative with Gremlin steps: respelled outside the GQL text.
        let help = camel("MATCH (p) RETURN toIntegerList(p.tags)");
        assert!(
            help.contains(".select(\"t\").unfold().asNumber()"),
            "{help}"
        );
        let help = camel("MATCH (p) RETURN percentileCont(p.age, 0.5)");
        assert!(help.contains("collect_list(x)"), "{help}");
    }

    /// Stage 2a: the errors of `FOR` / `UNWIND` explain what `FOR` defines (rule 6), the syntax
    /// advice and the expected tokens name `FOR` / `UNWIND` and the clauses a query may start
    /// with, and every `g.gql(..)` example of that advice runs.
    #[test]
    fn gql_for_errors_explain_what_for_defines() {
        let advice = "FOR x IN <list> (Cypher UNWIND <list> AS x) gives one row per item of the \
                      list, which it reads on the row before the FOR; the item x and, with WITH \
                      ORDINALITY i (from 1) or WITH OFFSET i (from 0), its position i are two \
                      new variables with names of their own; a node item continues a pattern by \
                      its name, as in FOR n IN nodes(p) MATCH (n)-->(m), and any other item is \
                      compared in a property map or WHERE, as in g.gql(\"FOR n IN ['marko', \
                      'josh'] WITH ORDINALITY i MATCH (p:person {name: n}) RETURN i, p.age\").";
        for (query, message, alternative) in [
            (
                "MATCH (a) FOR a IN [1] RETURN a",
                "the variable `a` is already defined; FOR defines a new variable",
                "choose another name, as in FOR a2 IN <list>",
            ),
            (
                "MATCH (a) UNWIND [1] AS a RETURN a",
                "the variable `a` is already defined; UNWIND defines a new variable",
                "choose another name, as in UNWIND <list> AS a2",
            ),
            (
                "LET i = 1 FOR x IN [1] WITH ORDINALITY i RETURN x",
                "the variable `i` is already defined; FOR defines a new variable",
                "choose another name, as in FOR i2 IN <list>",
            ),
            (
                "FOR x IN [1] WITH ORDINALITY x RETURN x",
                "`x` names both the item and its position; WITH ORDINALITY defines a second \
                 variable",
                "give the position its own name, as in FOR x IN ... WITH ORDINALITY x_index",
            ),
            (
                "FOR x IN [1] WITH OFFSET i MATCH (i)-->(b) RETURN b",
                "the variable `i` is the position of a FOR item (a number), not a node",
                "give the pattern element its own name, as in (i2), and compare the position \
                 in WHERE",
            ),
        ] {
            let (err, report) = gql_failure(|g| g.gql(query).to_list());
            assert_eq!(
                kind_and_status(&err),
                (GqlErrorKind::Semantic, "42002".to_string()),
                "{query}"
            );
            assert!(report.contains(message), "{query}: {report}");
            let help = err.help().unwrap();
            assert_eq!(help, format!("{alternative}. {advice}"), "{query}");
        }
        let help = gql_help("MATCH (a) FOR a IN [1] RETURN a");
        assert_examples_run(&help);
        // An aggregate in the list keeps the grouping advice: aggregate in a WITH first.
        let help = gql_help("MATCH (p)-->(f) FOR x IN collect_list(f) RETURN x");
        assert!(
            help.starts_with(
                "aggregate in a WITH first and iterate over its column, as in MATCH (p)-->(f) \
                 WITH p, collect_list(f) AS fs FOR x IN fs RETURN p, x. A RETURN or WITH with \
                 an aggregate"
            ),
            "{help}"
        );
        // An undefined variable: FOR defines variables too.
        let help = gql_help("FOR x IN [1] RETURN y");
        assert!(
            help.contains("(LET, WITH and FOR define variables too)"),
            "{help}"
        );
    }

    #[test]
    fn gql_syntax_advice_names_for_and_the_clauses_a_query_may_start_with() {
        let all = "MATCH, OPTIONAL MATCH, WITH, LET, FILTER, FOR, UNWIND, CALL, RETURN";
        for query in ["FOO 1", "", "(a) RETURN a"] {
            let (err, report) = gql_failure(|g| g.gql(query).to_list());
            assert_eq!(kind_and_status(&err).0, GqlErrorKind::Syntax, "{query}");
            assert!(
                report.contains(&format!("  expected:     {all}"))
                    || report.contains(&format!("  expected:  {all}")),
                "{query}: {report}"
            );
            let help = err.help().unwrap();
            assert!(
                help.contains(&format!("Expected {all} at the marked position.")),
                "{query}: {help}"
            );
            assert!(
                help.ends_with(
                    "FOR x IN <list> (Cypher UNWIND <list> AS x) gives one row per item of a \
                     list, and a query may also start with FOR, LET, WITH, FILTER or RETURN, as \
                     in g.gql(\"FOR n IN ['marko', 'josh'] MATCH (p:person {name: n}) RETURN \
                     n, p.age\")."
                ),
                "{query}: {help}"
            );
            assert_examples_run(&help);
        }
        // A misspelt FOR / UNWIND gets its suggestion from the same list.
        let report = gql_report("UNWIDN [1] AS x RETURN x");
        assert!(report.contains("  did you mean: UNWIND"), "{report}");
        let report = gql_report("MATCH (a) UNWIDN [1] AS x RETURN x");
        assert!(report.contains("  did you mean: UNWIND"), "{report}");
        // FOR / UNWIND after RETURN: RETURN ends the query.
        let report = gql_report("MATCH (a) RETURN a UNWIND [1] AS x");
        assert!(report.contains("RETURN ends the query"), "{report}");
    }

    /// Every clause a query may start with runs, a leading `FILTER` included (the 2a "not yet"
    /// texts of a query without `MATCH` are gone).
    #[test]
    fn gql_queries_without_match_and_for_are_no_longer_refused() {
        let graph = GraphSource::tinkerpop_modern();
        let locked = graph.read();
        for query in [
            "RETURN 1 AS x",
            "FOR x IN [1, 2] RETURN x",
            "UNWIND [1, 2] AS x RETURN x",
            "LET x = 1 RETURN x",
            "WITH 1 AS x RETURN x",
            "FILTER true RETURN 1 AS x",
            "FILTER WHERE 1 = 1 MATCH (p:person) RETURN p.name",
            "MATCH (p:person) FOR x IN [p.age] RETURN x",
        ] {
            let rows = locked.traversal().gql(query).to_list();
            assert!(rows.is_ok(), "{query}: {rows:?}");
        }
        let count = |query: &str| {
            locked
                .traversal()
                .gql(query)
                .to_list()
                .map(|rows| rows.len())
                .unwrap_or(usize::MAX)
        };
        assert_eq!(count("FILTER 1 > 2 RETURN 1 AS x"), 0);
        assert_eq!(count("FILTER 1 < 2 MATCH (p:person) RETURN p"), 4);
        assert_eq!(count("FILTER 1 > 2 MATCH (p:person) RETURN p"), 0);
    }

    /// The not-yet list constructs name FOR in gql() beside the Gremlin form (rule 6), and the
    /// FOR examples run.
    #[test]
    fn gql_not_yet_list_constructs_name_for() {
        let query = "MATCH (p) RETURN toIntegerList(p.tags)";
        let keeps = "convert the items one by one in Gremlin after gql()";
        let names = "or with FOR in gql(), as in g.gql(\"FOR t IN ['1', '2'] RETURN \
                     collect_list(toInteger(t)) AS l\")";
        let (err, _) = gql_failure(|g| g.gql(query).to_list());
        assert_eq!(kind_and_status(&err).0, GqlErrorKind::NotYet, "{query}");
        let help = err.help().unwrap();
        assert!(help.starts_with(keeps), "{query}: {help}");
        assert!(help.contains(names), "{query}: {help}");
        assert_examples_run(&help);
    }

    // ----- stage 2b: help sweep (rule 6) -----

    /// The general advice of the subquery errors (`GqlHelpTopic::Subquery`).
    const SUBQUERY_ADVICE: &str = "A subquery, EXISTS { MATCH <pattern> [WHERE <condition>] }, \
         EXISTS { <pattern> } or a Cypher pattern predicate such as (p)-[:created]->(), is TRUE \
         when its body has a match and FALSE otherwise, never NULL; its body reads the variables \
         bound before it, and the variables it binds itself stay inside it. A pattern predicate \
         binds no new variable: name new elements in an EXISTS { MATCH .. }, as in \
         g.gql(\"MATCH (p:person) WHERE NOT EXISTS { MATCH (p)-[:created]->(s) WHERE s.lang = \
         'java' } RETURN p.name\"), and bind an element in the MATCH clause of the query to read \
         it after the subquery, as in g.gql(\"MATCH (p:person)-[:created]->(s) RETURN p.name, \
         s.name\").";

    /// The scope errors of a subquery (a pattern predicate naming a new variable, a local read
    /// after the subquery, a body variable bound by a later clause of the body) explain what a
    /// subquery imports and keeps, and their examples run.
    #[test]
    fn gql_subquery_scope_errors_explain_imports_and_locals() {
        for (query, text, message, alternative) in [
            (
                "MATCH (p:person) WHERE (p)-[:created]->(s) RETURN p.name",
                "s",
                "the pattern predicate introduces the new variable `s`; a pattern used as a \
                 condition may only name variables bound before it",
                "write it as EXISTS { MATCH (p)-[:created]->(s) }, whose MATCH may bind new \
                 variables, or leave `s` out of the pattern",
            ),
            (
                "MATCH (p:person) WHERE EXISTS { MATCH (p)-->(x) } RETURN x",
                "x",
                "the variable `x` is not in scope: it is bound inside a subquery (EXISTS or a \
                 pattern predicate), whose variables are visible only inside it",
                "bind `x` in the MATCH clause of the query to use it here, or test it inside the \
                 subquery",
            ),
            (
                "MATCH (p:person) WHERE EXISTS { MATCH (p)-->(a) WHERE b.age > 1 MATCH \
                 (a)-->(b) } RETURN p",
                "b",
                "the variable `b` is not defined here: it is bound by a later MATCH clause of \
                 the subquery",
                "move the condition to the WHERE of that later MATCH clause inside the subquery",
            ),
            (
                // Nested: the later clause of the enclosing body.
                "MATCH (p) WHERE EXISTS { MATCH (p)-->(a) WHERE EXISTS { (a)-->(c) WHERE c = \
                 b } MATCH (a)-->(b) } RETURN p",
                "b",
                "it is bound by a later MATCH clause of the subquery",
                "move the condition to the WHERE of that later MATCH clause inside the subquery",
            ),
        ] {
            let (err, report) = gql_failure(|g| g.gql(query).to_list());
            assert_eq!(
                kind_and_status(&err),
                (GqlErrorKind::Semantic, "42002".to_string()),
                "{query}"
            );
            assert!(report.contains(message), "{query}: {report}");
            assert!(report.contains(&format!("`{text}`")), "{query}: {report}");
            let help = err.help().unwrap();
            assert_eq!(help, format!("{alternative}. {SUBQUERY_ADVICE}"), "{query}");
            assert_examples_run(&help);
        }
        // The fixes the alternatives name run.
        let graph = GraphSource::tinkerpop_modern();
        let locked = graph.read();
        for (query, rows) in [
            (
                "MATCH (p:person) WHERE EXISTS { MATCH (p)-[:created]->(s) } RETURN p.name",
                3,
            ),
            (
                "MATCH (p:person) WHERE EXISTS { MATCH (p)-->(a) MATCH (a)-->(b) WHERE b.age > \
                 1 } RETURN p",
                0,
            ),
            ("MATCH (p:person)-->(x) RETURN x", 6),
        ] {
            assert_eq!(
                locked.traversal().gql(query).to_list().unwrap().len(),
                rows,
                "{query}"
            );
        }
        // An undefined name in a body without a later clause keeps the plain message.
        let report =
            gql_report("MATCH (p) WHERE EXISTS { MATCH (p)-->(a) WHERE zz.x = 1 } RETURN p");
        assert!(
            report.contains("the variable `zz` is not defined"),
            "{report}"
        );
        assert!(!report.contains("later MATCH"), "{report}");
    }

    /// Both subquery limits (subqueries written, bodies run once LET values are substituted)
    /// are 54001 with the advice naming both bounds and a runnable EXISTS that holds several
    /// MATCH clauses.
    #[test]
    fn gql_subquery_limits_name_both_bounds() {
        let advice = "The query is larger than gql() accepts: one query holds at most 64 \
             subqueries (EXISTS, pattern predicates, pattern comprehensions, COUNT { } and \
             COLLECT { }, nested ones included) and runs at most 1024 subquery bodies, where a \
             LET or WITH variable holding a subquery counts once for every place that reads it; \
             a CALL { } body counts toward neither (CALL bodies nest at most 16 levels). One \
             EXISTS may hold several MATCH clauses and patterns, as in g.gql(\"MATCH (p:person) \
             WHERE EXISTS { MATCH (p)-[:knows]->(f) MATCH (f)-[:created]->(s) } RETURN \
             p.name\"), and one CALL may compute what several COUNT { } and COLLECT { } over the \
             same pattern do, as in g.gql(\"MATCH (p:person) CALL (p) { OPTIONAL MATCH \
             (p)-[:created]->(s) RETURN count(s) AS n, collect_list(s.name) AS names } RETURN \
             p.name, n, names\").";
        let many = vec!["(p)-->()"; 65].join(" OR ");
        let written = format!("MATCH (p) WHERE {many} RETURN p");
        let a = vec!["a"; 40].join(" AND ");
        let b = vec!["b"; 40].join(" AND ");
        let substituted = format!(
            "MATCH (p) LET a = EXISTS {{ (p)-->() }} LET b = EXISTS {{ MATCH (p)-->(q) WHERE \
             {a} }} FILTER {b} RETURN p.name AS n"
        );
        for (query, message) in [
            (
                written.as_str(),
                "the query holds more than 64 subqueries (EXISTS and pattern predicates)",
            ),
            (
                substituted.as_str(),
                "the query runs more than 1024 subquery bodies once its LET variables are \
                 substituted",
            ),
        ] {
            let (err, report) = gql_failure(|g| g.gql(query).to_list());
            assert_eq!(
                kind_and_status(&err),
                (GqlErrorKind::Limit, "54001".to_string())
            );
            assert!(report.contains(message), "{report}");
            let help = err.help().unwrap();
            assert!(help.ends_with(advice), "{help}");
            assert_examples_run(&help);
        }
    }

    /// Stage 2d.T5c: `COUNT { } > 0` runs (it was refused "not yet" with EXISTS as advice) and
    /// keeps the rows of `EXISTS`.
    #[test]
    fn gql_count_subquery_above_zero_keeps_the_exists_rows() {
        let graph = GraphSource::tinkerpop_modern();
        let run = |query: &str| {
            graph
                .read()
                .traversal()
                .gql(query)
                .to_list()
                .unwrap_or_else(|e| panic!("{query}: {e}"))
        };
        let exists = run("MATCH (p:person) WHERE EXISTS { (p)-[:created]->() } RETURN p.name");
        assert_eq!(exists.len(), 3);
        assert_eq!(
            run("MATCH (p:person) WHERE COUNT { (p)-[:created]->() } > 0 RETURN p.name"),
            exists
        );
    }

    /// A loop limit or a traverser limit reached by a quantified edge inside a subquery names
    /// the quantifier and says it is inside EXISTS, on the fast path (`filter()` / `not()`) and
    /// in a hosted child (`gql:project`, `gql:filter`, `gql:order`); a quantifier outside a
    /// subquery, also in an OPTIONAL MATCH child, keeps the plain text.
    #[test]
    fn gql_limits_inside_exists_name_the_quantifier() {
        let body = "EXISTS { MATCH (p)-[]-{1,}(q) WHERE q.name = 'zzz' }";
        let inside = [
            format!("MATCH (p:person) WHERE {body} RETURN p.name"),
            format!("MATCH (p:person) WHERE NOT {body} RETURN p.name"),
            format!("MATCH (p:person) WHERE p.age > 100 OR {body} RETURN p.name"),
            format!("MATCH (p:person) RETURN p.name, {body} AS e"),
            format!("MATCH (p:person) RETURN p.name AS n ORDER BY {body}"),
            format!(
                "MATCH (p:person) WHERE EXISTS {{ MATCH (p)-->(x) WHERE {} }} RETURN p",
                body.replace("(p)", "(x)")
            ),
        ];
        let outside = [
            "MATCH (p:person)-[]-{1,}(q) RETURN p.name".to_string(),
            "MATCH (p:person) OPTIONAL MATCH (p)-[]-{1,}(q) WHERE q.name = 'zzz' RETURN p.name, q"
                .to_string(),
        ];
        let loops = "The loop of the quantified edge -[]-{1,} inside an EXISTS subquery (or \
                     pattern predicate) of the gql() query ran 2 iterations, the limit of \
                     repeat.max_loops.";
        let early = "A subquery stops at its first match, so its loop runs this long on a row \
                     that has no match, or only a late one. Give the quantifier an upper bound, \
                     as in -[]-{1,5}, for example";
        let traversers = "Each path a quantified edge (-[]-{1,}), inside an EXISTS subquery (or \
                          pattern predicate), matches is a traverser of its own";
        for query in &inside {
            let (err, _) = gql_failure(|g| g.with("repeat.max_loops", 2).gql(query).to_list());
            let help = err.help().unwrap();
            assert!(help.starts_with(loops), "{query}: {help}");
            assert!(help.contains(early), "{query}: {help}");
            assert_examples_run(&help);
            let (err, _) =
                gql_failure(|g| g.with("traversal.max_traversers", 5).gql(query).to_list());
            let help = err.help().unwrap();
            assert!(help.contains(traversers), "{query}: {help}");
            assert_examples_run(&help);
        }
        for query in &outside {
            let (err, _) = gql_failure(|g| g.with("repeat.max_loops", 2).gql(query).to_list());
            let help = err.help().unwrap();
            assert!(
                help.starts_with(
                    "The loop of the quantified edge -[]-{1,} of the gql() query ran 2 \
                     iterations"
                ),
                "{query}: {help}"
            );
            assert!(!help.contains("EXISTS"), "{query}: {help}");
        }
    }

    /// EXISTS and pattern predicates run in every position: no help recommends the Gremlin
    /// `where()` instead of them any more.
    #[test]
    fn gql_exists_is_no_longer_refused() {
        let graph = GraphSource::tinkerpop_modern();
        let locked = graph.read();
        for (query, rows) in [
            (
                "MATCH (p:person) WHERE NOT (p)-[:created]->() RETURN p.name",
                1,
            ),
            (
                "MATCH (p:person) WHERE EXISTS { (p)-[:created]->() } OR p.age > 30 RETURN p",
                3,
            ),
            ("MATCH (p:person) RETURN p.name, (p)-[:knows]->() AS k", 4),
            (
                "MATCH (p:person) RETURN p.name ORDER BY EXISTS ((p)-->()) DESC",
                4,
            ),
            ("FOR b IN [EXISTS { MATCH (s:software) }] RETURN b", 1),
            (
                "MATCH (p:person) RETURN count(CASE WHEN (p)-->() THEN 1 END) AS c",
                1,
            ),
        ] {
            assert_eq!(
                locked.traversal().gql(query).to_list().unwrap().len(),
                rows,
                "{query}"
            );
        }
    }

    // ----- stage 2c: help sweep (rule 6) -----

    // ----- stage 2d.T5d: list predicates and reduce (D23) -----

    /// A variable of `all()` / `any()` / `none()` / `single()` / `reduce()` that reuses a name
    /// is E14 like a comprehension variable: the message names the form, the advice gives it a
    /// variable of its own and its examples run.
    #[test]
    fn gql_list_function_variables_must_be_new_names() {
        for (query, message, alternative) in [
            (
                "MATCH (p:person) RETURN all(p IN [1] WHERE p > 0) AS a",
                "the variable `p` of all() shadows the variable `p` in scope; it must be a new \
                 name",
                "give all() a variable of its own, as in all(p2 IN list WHERE p2 > 0).",
            ),
            (
                "MATCH (p:person) RETURN single(p IN [1] WHERE p > 0) AS a",
                "the variable `p` of single() shadows the variable `p` in scope",
                "give single() a variable of its own, as in single(p2 IN list WHERE p2 > 0).",
            ),
            (
                "MATCH (p:person) RETURN reduce(p = 0, x IN [1] | x) AS a",
                "the accumulator `p` of reduce() shadows the variable `p` in scope",
                "give reduce() an accumulator of its own, as in reduce(p2 = 0, item IN list | \
                 p2 + item).",
            ),
            (
                "RETURN reduce(s = 0, s IN [1] | s) AS a",
                "the variable `s` of reduce() shadows the variable `s` in scope",
                "give reduce() a variable of its own, as in reduce(total = 0, s2 IN list | \
                 total + s2).",
            ),
            (
                "MATCH (p:person) WITH p.name AS n RETURN any(p IN [1] WHERE p > 0) AS a",
                "the variable `p` of any() is a name this query already uses",
                "give any() a variable of its own, as in any(p2 IN list WHERE p2 > 0).",
            ),
        ] {
            let (err, text) = gql_failure(|g| g.gql(query).to_list());
            assert_eq!(
                kind_and_status(&err),
                (GqlErrorKind::Semantic, "42002".to_string()),
                "{query}"
            );
            assert!(text.contains(message), "{query}: {text}");
            let help = err.help().unwrap();
            assert!(help.starts_with(alternative), "{query}: {help}");
            assert!(help.contains("both variables of reduce()"), "{help}");
            assert_examples_run(&help);
        }
    }

    /// An aggregate inside the condition of a list predicate or the `|` part of `reduce()` is
    /// refused with a WITH example of that form; its initial value and its list lie outside.
    #[test]
    fn gql_list_function_aggregates_name_the_form() {
        for (query, place, example) in [
            (
                "MATCH (p:person) RETURN any(x IN [1] WHERE x > count(*)) AS a",
                "inside the WHERE part of all(), any(), none() or single()",
                "aggregate in a WITH first and use its column in the condition, as in MATCH \
                 (p)-->(f) WITH p, count(f.age) AS n RETURN all(x IN [1, 2, 3] WHERE x < n)",
            ),
            (
                "MATCH (p:person) RETURN reduce(s = 0, x IN [1] | s + max(p.age)) AS a",
                "inside the | part of reduce()",
                "aggregate in a WITH first and use its column in the | part, as in MATCH \
                 (p)-->(f) WITH p, max(f.age) AS n RETURN reduce(s = 0, x IN [1, 2, 3] | s + x \
                 * n)",
            ),
        ] {
            let (err, text) = gql_failure(|g| g.gql(query).to_list());
            assert_eq!(kind_and_status(&err).0, GqlErrorKind::Semantic, "{query}");
            assert!(text.contains(place), "{query}: {text}");
            let help = err.help().unwrap();
            assert!(help.starts_with(example), "{query}: {help}");
            assert_examples_run(&help);
        }
        let graph = GraphSource::tinkerpop_modern();
        let locked = graph.read();
        let rows = locked
            .traversal()
            .gql(
                "MATCH (p:person) RETURN reduce(s = count(*), x IN collect_list(p.age) | s + x) \
                 AS a",
            )
            .to_list()
            .unwrap();
        assert_eq!(rows.len(), 1);
    }

    /// A list function over a value that is not a list is a data error naming the function;
    /// a missing `WHERE` or `|` is a syntax error naming the expected token.
    #[test]
    fn gql_list_function_errors_name_the_function() {
        for (query, message) in [
            (
                "RETURN any(x IN 5 WHERE x > 1) AS a",
                "any() iterates over a list, not a number",
            ),
            (
                "RETURN all(x IN 'ab' WHERE x > 1) AS a",
                "all() iterates over a list, not a string",
            ),
            (
                "RETURN reduce(s = 0, x IN 5 | s + x) AS a",
                "reduce() iterates over a list, not a number",
            ),
        ] {
            let (err, text) = gql_failure(|g| g.gql(query).to_list());
            assert_eq!(kind_and_status(&err).1, "22G03", "{query}");
            assert!(text.contains(message), "{query}: {text}");
        }
        for (query, expected) in [
            ("RETURN all(x IN [1]) AS a", "Expected WHERE"),
            ("RETURN reduce(s = 0, x IN [1]) AS a", "Expected `|`"),
            ("RETURN reduce(s, x IN [1] | s) AS a", "Expected `=`"),
            (
                "RETURN none(1 IN [1] WHERE true) AS a",
                "Expected a variable",
            ),
        ] {
            let (err, _) = gql_failure(|g| g.gql(query).to_list());
            assert_eq!(kind_and_status(&err).0, GqlErrorKind::Syntax, "{query}");
            let help = err.help().unwrap();
            assert!(help.starts_with(expected), "{query}: {help}");
        }
    }

    /// A loop or traverser limit reached by a quantified edge inside a pattern comprehension
    /// names the pattern comprehension (it runs to its last match, it does not stop at the
    /// first); an EXISTS inside a comprehension's `|` part keeps the EXISTS text, a
    /// comprehension inside an EXISTS body names the comprehension.
    #[test]
    fn gql_limits_inside_a_pattern_comprehension_name_it() {
        let comprehension = "[(p)-[]-{1,}(q) WHERE q.name = 'zzz' | q.name]";
        let inside = [
            format!("MATCH (p:person) RETURN p.name, {comprehension} AS e"),
            format!("MATCH (p:person) WHERE size({comprehension}) > 0 RETURN p.name"),
            format!("MATCH (p:person) RETURN p.name AS n ORDER BY size({comprehension})"),
            format!("MATCH (p:person) RETURN collect_list({comprehension}) AS c"),
            format!("MATCH (p:person) FOR x IN {comprehension} RETURN x"),
            format!(
                "MATCH (p:person) RETURN [(p)-->(x) | {}] AS e",
                comprehension.replace("(p)", "(x)")
            ),
            format!(
                "MATCH (p:person) WHERE EXISTS {{ MATCH (p)-->(x) WHERE size({}) > 0 }} RETURN p",
                comprehension.replace("(p)", "(x)")
            ),
        ];
        let loops = "The loop of the quantified edge -[]-{1,} inside a pattern comprehension \
                     [(a)-->(b) | ..] of the gql() query ran 2 iterations, the limit of \
                     repeat.max_loops.";
        let collects = "A pattern comprehension collects every match of its pattern for each \
                        row, so its loop runs until the row's last match is found. Give the \
                        quantifier an upper bound, as in -[]-{1,5}, for example";
        let traversers = "Each path a quantified edge (-[]-{1,}), inside a pattern \
                          comprehension [(a)-->(b) | ..], matches is a traverser of its own";
        for query in &inside {
            let (err, _) = gql_failure(|g| g.with("repeat.max_loops", 2).gql(query).to_list());
            let help = err.help().unwrap();
            assert!(help.starts_with(loops), "{query}: {help}");
            assert!(help.contains(collects), "{query}: {help}");
            assert!(!help.contains("EXISTS"), "{query}: {help}");
            assert_examples_run(&help);
            let (err, _) =
                gql_failure(|g| g.with("traversal.max_traversers", 5).gql(query).to_list());
            let help = err.help().unwrap();
            assert!(help.contains(traversers), "{query}: {help}");
            assert_examples_run(&help);
        }
        // An EXISTS in the `|` part: the loop is the EXISTS body's.
        let query = "MATCH (p:person) RETURN [(p)-->(x) | EXISTS { MATCH (x)-[]-{1,}(q) WHERE \
                     q.name = 'zzz' }] AS e";
        let (err, _) = gql_failure(|g| g.with("repeat.max_loops", 2).gql(query).to_list());
        let help = err.help().unwrap();
        assert!(
            help.starts_with(
                "The loop of the quantified edge -[]-{1,} inside an EXISTS subquery (or pattern \
                 predicate) of the gql() query"
            ),
            "{help}"
        );
        assert!(!help.contains("pattern comprehension"), "{help}");
    }

    /// A pattern comprehension's list over `traversal.max_materialized_bytes` gets the GQL
    /// advice naming the comprehension and counting its matches instead.
    #[test]
    fn gql_pattern_comprehension_list_limit_names_it() {
        let (err, _) = gql_failure(|g| {
            g.with("traversal.max_materialized_bytes", 300)
                .gql("MATCH (p:person) RETURN [(p)-[]-{1,3}(f) | f.name] AS l")
                .to_list()
        });
        let help = err.help().expect("help");
        for part in [
            "as is the list of a pattern comprehension [(a)-->(b) | b.name], so the query stops",
            "or, for the matches of a pattern, g.gql(\"MATCH (p:person) OPTIONAL MATCH (p)-->(f) \
             RETURN p.name, count(f) AS n\")",
        ] {
            assert!(help.contains(part), "{help}");
        }
        assert_examples_run(&help);
    }

    /// A variable bound inside a pattern comprehension and read after it names the
    /// comprehension (not EXISTS), with the comprehension advice; EXISTS keeps its text.
    /// Stage 2d.T5c: a loop or traverser limit reached inside a `COUNT { }` or `COLLECT { }`
    /// subquery names the construct (each runs its body to the end for every row, also when
    /// the optimizer fuses the closing `count()`); the examples run.
    #[test]
    fn gql_limits_inside_count_and_collect_name_them() {
        for (query, place, why) in [
            (
                "MATCH (p:person) RETURN p.name, COUNT { MATCH (p)-[]-{1,}(q) WHERE q.name = \
                 'zzz' } AS c",
                "inside a COUNT { } subquery of the gql() query",
                "A COUNT { } subquery counts every match of its body for each row",
            ),
            (
                "MATCH (p:person) WHERE COUNT { (p)-[]-{1,}(q) } > 2 RETURN p.name",
                "inside a COUNT { } subquery of the gql() query",
                "A COUNT { } subquery counts every match of its body for each row",
            ),
            (
                "MATCH (p:person) RETURN COLLECT { MATCH (p)-[]-{1,}(q) RETURN q.name } AS l",
                "inside a COLLECT { } subquery of the gql() query",
                "A COLLECT { } subquery collects every row of its body for each row",
            ),
            (
                "MATCH (p:person) RETURN COLLECT { MATCH (p)-[]-{1,}(q) WITH q WHERE q.age > 1 \
                 RETURN q.name ORDER BY q.name } AS l",
                "inside a COLLECT { } subquery of the gql() query",
                "A COLLECT { } subquery collects every row of its body for each row",
            ),
        ] {
            let (err, _) = gql_failure(|g| g.with("repeat.max_loops", 2).gql(query).to_list());
            let help = err.help().unwrap();
            assert!(
                help.starts_with(&format!(
                    "The loop of the quantified edge -[]-{{1,}} {place} ran 2 iterations"
                )),
                "{query}: {help}"
            );
            assert!(help.contains(why), "{query}: {help}");
            assert!(!help.contains("EXISTS"), "{query}: {help}");
            assert!(!help.contains("pattern comprehension"), "{query}: {help}");
            assert_examples_run(&help);
        }
        // A traverser limit in an uncorrelated body (a child-leading scan).
        let (err, _) = gql_failure(|g| {
            g.with("traversal.max_traversers", 5)
                .gql("MATCH (p:person) RETURN COUNT { MATCH (s)-[]-{1,}(q) } AS c")
                .to_list()
        });
        let help = err.help().unwrap();
        assert!(help.contains("inside a COUNT { } subquery"), "{help}");
        assert_examples_run(&help);
    }

    /// Stage 2d.T5c: a `COLLECT { }` list over `traversal.max_materialized_bytes` is refused
    /// before it is built; the advice names `COLLECT { }` and shows the `COUNT { }` form of
    /// counting the matches instead.
    #[test]
    fn gql_collect_subquery_list_limit_names_it() {
        let (err, _) = gql_failure(|g| {
            g.with("traversal.max_materialized_bytes", 300)
                .gql(
                    "MATCH (p:person) RETURN COLLECT { MATCH (p)-[]-{1,3}(f) RETURN f.name } \
                     AS l",
                )
                .to_list()
        });
        let help = err.help().expect("help");
        for part in [
            "The list of a COLLECT { .. RETURN x } subquery is checked the same way.",
            "or its openCypher form g.gql(\"MATCH (p:person) RETURN p.name, COUNT { (p)-->() } \
             AS n\")",
        ] {
            assert!(help.contains(part), "{help}");
        }
        assert_examples_run(&help);
    }

    #[test]
    fn gql_pattern_comprehension_scope_errors_name_it() {
        for query in [
            "MATCH (a) RETURN [(a)-->(c) | c] AS l, c",
            "MATCH (a) WHERE size([(a)-->(c) | c]) > 0 RETURN c",
            "MATCH (a) WITH a, [(a)-->(c) | c.name] AS l RETURN c",
        ] {
            let (err, report) = gql_failure(|g| g.gql(query).to_list());
            assert_eq!(
                kind_and_status(&err),
                (GqlErrorKind::Semantic, "42002".to_string()),
                "{query}"
            );
            assert!(
                report.contains(
                    "the variable `c` is not in scope: it is bound inside a pattern \
                     comprehension, whose variables are visible only inside it"
                ),
                "{query}: {report}"
            );
            let help = err.help().unwrap();
            assert!(
                help.starts_with(
                    "bind `c` in the MATCH clause of the query to use it here, or read it in \
                     the | part of the comprehension, as in [(a)-->(c) | c.name]. A pattern \
                     comprehension, [(a)-[:knows]->(f) WHERE <condition> | <expression>], is \
                     the list of its | part evaluated for every match of its pattern"
                ),
                "{query}: {help}"
            );
            assert!(!help.contains("EXISTS"), "{help}");
            assert_examples_run(&help);
        }
        let report = gql_report("MATCH (a) WHERE EXISTS { (a)-->(c) } RETURN c");
        assert!(
            report.contains("it is bound inside a subquery (EXISTS or a pattern predicate)"),
            "{report}"
        );
    }

    /// Pattern comprehensions count against both subquery limits, and the messages say so.
    #[test]
    fn gql_subquery_limits_count_pattern_comprehensions() {
        let many = vec!["size([(p)-->(q) | q])"; 65].join(" + ");
        let written = format!("MATCH (p) RETURN {many} AS n");
        let (err, report) = gql_failure(|g| g.gql(&written).to_list());
        assert_eq!(
            kind_and_status(&err),
            (GqlErrorKind::Limit, "54001".to_string())
        );
        assert!(
            report.contains(
                "the query holds more than 64 subqueries (EXISTS and pattern predicates), \
                 pattern comprehensions included"
            ),
            "{report}"
        );
        let help = err.help().unwrap();
        assert!(
            help.contains(
                "a pattern comprehension is a subquery too: collect the matches of several \
                 patterns with OPTIONAL MATCH and collect_list() instead"
            ),
            "{help}"
        );
        assert_examples_run(&help);
        // A LET holding a pattern comprehension, read in 40 conditions of an EXISTS body that
        // a LET holds and 40 conditions read: 1600 bodies.
        let a = vec!["a"; 40].join(" AND ");
        let b = vec!["b"; 40].join(" AND ");
        let substituted = format!(
            "MATCH (p) LET a = size([(p)-->(x) | x]) > 0 LET b = EXISTS {{ MATCH (p)-->(q) WHERE \
             {a} }} FILTER {b} RETURN p.name AS n"
        );
        let (err, report) = gql_failure(|g| g.gql(&substituted).to_list());
        assert_eq!(
            kind_and_status(&err),
            (GqlErrorKind::Limit, "54001".to_string()),
            "{report}"
        );
        assert!(
            report.contains(
                "(each place that reads a LET holding EXISTS or a pattern comprehension runs its \
                 own copy)"
            ),
            "{report}"
        );
        let help = err.help().unwrap();
        assert!(
            help.starts_with(
                "write the EXISTS or the pattern comprehension where it is used instead of in a \
                 LET that is read many times"
            ),
            "{help}"
        );
        assert_examples_run(&help);
    }

    /// Stage 2d.T6: `UNION` and `NEXT` errors carry the composite-query advice (same column
    /// names in any order, one kind of `UNION` per chain, `NEXT` passes on only the columns
    /// of the `RETURN` before it); the column mismatch names both column sets.
    #[test]
    fn gql_union_and_next_errors_explain_composite_queries() {
        let advice = "UNION combines the records of queries that return the same column names, \
             in any order (the result keeps the order of the first query): UNION (or UNION \
             DISTINCT) removes duplicate records, UNION ALL keeps them, and one chain of UNIONs \
             uses one kind. Each query of a UNION has its own variables, ORDER BY, OFFSET and \
             LIMIT. NEXT passes the records of the statement before it on by their column names \
             (only those), so it sorts, filters or aggregates a UNION as a whole, as in \
             g.gql(\"MATCH (p:person) RETURN p.name AS name UNION MATCH (s:software) RETURN \
             s.name AS name NEXT RETURN name ORDER BY name LIMIT 3\").";
        for (query, message, alternative) in [
            (
                "MATCH (p:person) RETURN p.name AS name, p.age AS age UNION MATCH (s:software) \
                 RETURN s.name AS n",
                "the operands of UNION must return the same column names: the first returns \
                 `name`, `age`, this one returns `n` (`name` is missing)",
                "name the columns alike with AS in every operand",
            ),
            (
                "RETURN 1 AS x UNION RETURN 2 AS x UNION ALL RETURN 3 AS x",
                "UNION ALL is mixed with UNION in one query",
                "write the same kind between every operand, or combine the parts with NEXT",
            ),
            (
                "MATCH (p:person) RETURN p.name AS n NEXT RETURN p",
                "the variable `p` is not in scope: the RETURN before NEXT does not return it",
                "add `p` to the RETURN items before NEXT",
            ),
        ] {
            let (err, report) = gql_failure(|g| g.gql(query).to_list());
            assert_eq!(
                kind_and_status(&err),
                (GqlErrorKind::Semantic, "42002".to_string()),
                "{query}"
            );
            assert!(report.contains(message), "{query}: {report}");
            let help = err.help().unwrap();
            assert!(help.starts_with(alternative), "{query}: {help}");
            assert!(help.ends_with(advice), "{query}: {help}");
            assert_examples_run(&help);
        }
    }

    /// Stage 2d.T6: `CALL { }` errors carry the CALL advice (imports by the scope clause, new
    /// returned names, per-row semantics, OPTIONAL CALL), the refused body without `RETURN` and
    /// `IN TRANSACTIONS` included.
    #[test]
    fn gql_call_errors_explain_imports_and_returned_columns() {
        let advice = "A CALL subquery, CALL (a, b) { <query ending in RETURN, or in a write \
             (INSERT, SET, REMOVE, DELETE, MERGE / FINISH)> }, runs its query once for each incoming row: the body reads only the variables its scope clause \
             names (CALL (*) imports all of them, CALL { } none, and Cypher's CALL { WITH a .. } \
             those of its first WITH), and passes on only the columns of its RETURN, which must \
             be new names; its aggregates, ORDER BY and LIMIT apply to each row's records, as \
             in g.gql(\"MATCH (p:person) CALL (p) { MATCH (p)-[:created]->(s) RETURN count(s) \
             AS made } RETURN p.name, made\"). A row whose body returns no record is dropped; \
             OPTIONAL CALL keeps it with NULL columns.";
        for (query, kind, message) in [
            (
                "MATCH (p:person) CALL (p) { RETURN 1 AS p } RETURN p",
                GqlErrorKind::Semantic,
                "the CALL body returns `p`, which is already defined outside it",
            ),
            (
                "MATCH (p:person) CALL (p) { MATCH (p)-->(f) RETURN f } RETURN f, x",
                GqlErrorKind::Semantic,
                "the variable `x` is not defined",
            ),
            (
                "MATCH (p:person) CALL (p) { MATCH (f) WHERE f.age > p.age RETURN f.name AS n } \
                 RETURN f",
                GqlErrorKind::Semantic,
                "the variable `f` is not in scope: it is bound inside a CALL body",
            ),
            (
                "MATCH (p:person), (q:person) CALL (p) { RETURN q.name AS n } RETURN n",
                GqlErrorKind::Semantic,
                "the variable `q` is not imported into the CALL body",
            ),
            (
                "MATCH (p:person) CALL (p, p) { RETURN p.name AS n } RETURN n",
                GqlErrorKind::Semantic,
                "the variable `p` is imported twice by this CALL",
            ),
            (
                "MATCH (p:person) CALL (p) { MATCH (p)-->(f) } RETURN p",
                GqlErrorKind::Refused,
                "a CALL { } body without RETURN (a unit subquery) only makes sense for writes",
            ),
            (
                "MATCH (p:person) CALL (p) { RETURN 1 AS x } IN TRANSACTIONS RETURN x",
                GqlErrorKind::Refused,
                "CALL { } IN TRANSACTIONS commits writes in batches",
            ),
        ] {
            let (err, report) = gql_failure(|g| g.gql(query).to_list());
            assert_eq!(kind_and_status(&err).0, kind, "{query}: {report}");
            assert!(report.contains(message), "{query}: {report}");
            let help = err.help().unwrap();
            if !message.ends_with("is not defined") {
                assert!(help.ends_with(advice), "{query}: {help}");
            }
            assert_examples_run(&help);
        }
    }

    /// Stage 2d.T6: the expected tokens name `CALL` among the clauses and `UNION` / `NEXT`
    /// after a complete result statement; a clause after `RETURN` names `NEXT` with a runnable
    /// fix, and the syntax advice shows CALL, UNION and NEXT.
    #[test]
    fn gql_expected_tokens_name_call_union_and_next() {
        let report = gql_report("MATCH (p:person) RETURN p.name AS n x");
        assert!(
            report.contains(
                "  expected:  `,`, ORDER BY, OFFSET, SKIP, LIMIT, UNION, NEXT, end of input"
            ),
            "{report}"
        );
        let report = gql_report("MATCH (p:person) RETURN p LIMIT 1 x");
        assert!(
            report.contains("  expected:  UNION, NEXT, end of input"),
            "{report}"
        );
        let report = gql_report("MATCH (p:person) CALL (p) { RETURN p.name AS n x } RETURN n");
        assert!(report.contains("UNION, NEXT, `}`"), "{report}");
        let report = gql_report("FOO");
        assert!(
            report.contains("MATCH, OPTIONAL MATCH, WITH, LET, FILTER, FOR, UNWIND, CALL, RETURN"),
            "{report}"
        );
        let report = gql_report("MATCH (p:person) RETURN p NEX MATCH (p)-->(f) RETURN f");
        assert!(report.contains("  did you mean: NEXT"), "{report}");
        let help = gql_help("MATCH (p:person) RETURN p MATCH (p)-[:created]->(s) RETURN s.name");
        assert!(
            help.starts_with(
                "put every MATCH before the RETURN, or continue with NEXT, which passes the \
                 columns of the RETURN on to the next statement, as in MATCH (p:person) RETURN \
                 p NEXT MATCH (p)-[:created]->(s) RETURN p.name, s.name, or chain Gremlin steps \
                 after gql()."
            ),
            "{help}"
        );
        assert!(
            help.contains(
                "CALL (p) { .. RETURN .. } runs a query for each row, UNION combines the records \
                 of two queries and NEXT continues after a RETURN, as in g.gql(\"MATCH (p:person) \
                 RETURN p NEXT MATCH (p)-[:created]->(s) RETURN p.name, s.name\")."
            ),
            "{help}"
        );
        assert_examples_run(&help);
        assert!(
            run_ok(
                "MATCH (p:person) RETURN p NEXT MATCH (p)-[:created]->(s) RETURN p.name, s.name"
            ) > 0
        );
    }

    fn run_ok(query: &str) -> usize {
        let graph = GraphSource::tinkerpop_modern();
        let rows = graph.read().traversal().gql(query).to_list();
        rows.unwrap_or_else(|e| panic!("{query}: {e}")).len()
    }

    /// Stage 2d.T6: a resource limit inside a `CALL { }` body names the CALL (also inside a
    /// `COUNT { }` / `EXISTS` body); one inside a `union()` operand of a `COLLECT { }` /
    /// `COUNT { }` body names that subquery; a top-level `UNION` operand is the query itself.
    #[test]
    fn gql_limits_inside_call_and_union_name_the_construct() {
        let call = "inside a CALL { } subquery of the gql() query";
        let why_call = "A CALL { } subquery runs its body for each incoming row and keeps every \
             record it returns, so its loop runs until the row's last match is found.";
        for (query, place, why) in [
            (
                "MATCH (p:person) CALL (p) { MATCH (p)-[]-{1,}(q) RETURN q } RETURN q.name",
                call,
                why_call,
            ),
            (
                "MATCH (p:person) OPTIONAL CALL (p) { MATCH (p)-[]-{1,}(q) RETURN q } RETURN \
                 q.name",
                call,
                why_call,
            ),
            (
                "MATCH (p:person) RETURN COUNT { CALL (p) { MATCH (p)-[]-{1,}(q) RETURN q } \
                 RETURN q } AS c",
                call,
                why_call,
            ),
            (
                "MATCH (p:person) WHERE EXISTS { CALL (p) { MATCH (p)-[]-{1,}(q) RETURN q } \
                 RETURN q } RETURN p.name",
                call,
                why_call,
            ),
            (
                "MATCH (p:person) RETURN COLLECT { MATCH (p)-[]-{1,}(q) RETURN q.name AS n \
                 UNION RETURN p.name AS n } AS l",
                "inside a COLLECT { } subquery of the gql() query",
                "A COLLECT { } subquery collects every row of its body for each row",
            ),
            (
                "MATCH (p:person) RETURN COUNT { RETURN p AS q UNION MATCH (p)-[]-{1,}(q) \
                 RETURN q } AS c",
                "inside a COUNT { } subquery of the gql() query",
                "A COUNT { } subquery counts every match of its body for each row",
            ),
        ] {
            let (err, _) = gql_failure(|g| g.with("repeat.max_loops", 2).gql(query).to_list());
            let help = err.help().unwrap();
            assert!(
                help.starts_with(&format!(
                    "The loop of the quantified edge -[]-{{1,}} {place} ran 2 iterations"
                )),
                "{query}: {help}"
            );
            assert!(help.contains(why), "{query}: {help}");
            assert!(!help.contains("EXISTS subquery"), "{query}: {help}");
            assert_examples_run(&help);
        }
        // A top-level UNION operand and a NEXT block are the query itself.
        for query in [
            "RETURN 1 AS q UNION MATCH (p:person)-[]-{1,}(q) RETURN q",
            "MATCH (p:person) RETURN p NEXT MATCH (p)-[]-{1,}(q) RETURN q",
        ] {
            let (err, _) = gql_failure(|g| g.with("repeat.max_loops", 2).gql(query).to_list());
            let help = err.help().unwrap();
            assert!(
                help.starts_with(
                    "The loop of the quantified edge -[]-{1,} of the gql() query ran 2 iterations"
                ),
                "{query}: {help}"
            );
        }
        // A traverser limit inside a CALL body.
        let (err, _) = gql_failure(|g| {
            g.with("traversal.max_traversers", 5)
                .gql("MATCH (p:person) CALL (p) { MATCH (s)-[]-{1,}(q) RETURN q } RETURN q")
                .to_list()
        });
        let help = err.help().unwrap();
        assert!(help.contains("inside a CALL { } subquery"), "{help}");
        assert_examples_run(&help);
    }

    /// Stage 2d.T6: a variable of `all()` / `any()` / `none()` / `single()` or `reduce()`
    /// named inside a subquery body is "not yet" with a message naming the function.
    #[test]
    fn gql_list_function_variables_inside_a_subquery_name_the_function() {
        for (query, message) in [
            (
                "MATCH (p) WHERE all(x IN [1] WHERE EXISTS { MATCH (x)-->() }) RETURN p",
                "reading the variable `x` of all() inside a subquery",
            ),
            (
                "MATCH (p) WHERE any(x IN [1] WHERE EXISTS { MATCH (p)-->(x) }) RETURN p",
                "reading the variable `x` of any() inside a subquery",
            ),
            (
                "MATCH (p) WHERE none(x IN [1] WHERE COUNT { MATCH (x)-->() } > 0) RETURN p",
                "reading the variable `x` of none() inside a subquery",
            ),
            (
                "MATCH (p) WHERE single(x IN [1] WHERE EXISTS { MATCH (x) }) RETURN p",
                "reading the variable `x` of single() inside a subquery",
            ),
            (
                "MATCH (p) RETURN reduce(s = 0, x IN [1] | s + COUNT { MATCH (x)-->() }) AS r",
                "reading the variable `x` of reduce() inside a subquery",
            ),
            (
                "MATCH (p) RETURN reduce(s = 0, x IN [1] | s + COUNT { MATCH (s)-->() }) AS r",
                "reading the accumulator `s` of reduce() inside a subquery",
            ),
            (
                "MATCH (p) RETURN [x IN [1] WHERE EXISTS { MATCH (x)-->() }] AS l",
                "reading the comprehension variable `x` inside a subquery",
            ),
        ] {
            let (err, report) = gql_failure(|g| g.gql(query).to_list());
            assert_eq!(
                kind_and_status(&err),
                (GqlErrorKind::NotYet, "0A000".to_string()),
                "{query}: {report}"
            );
            assert!(report.contains(message), "{query}: {report}");
        }
    }

    /// Stage 2d.T6: COUNT { } and COLLECT { } count against both subquery limits, and the
    /// messages say so; the bodies-run advice names CALL as the form that is not copied.
    #[test]
    fn gql_subquery_limits_name_count_and_collect() {
        let many = vec!["COUNT { (p)-->(q) }"; 65].join(" + ");
        let (err, report) =
            gql_failure(|g| g.gql(&format!("MATCH (p) RETURN {many} AS n")).to_list());
        assert_eq!(
            kind_and_status(&err),
            (GqlErrorKind::Limit, "54001".to_string())
        );
        assert!(
            report.contains(
                "the query holds more than 64 subqueries (EXISTS and pattern predicates), \
                 pattern comprehensions included, as are COUNT { } and COLLECT { }"
            ),
            "{report}"
        );
        let a = vec!["a"; 40].join(" AND ");
        let b = vec!["b"; 40].join(" AND ");
        let substituted = format!(
            "MATCH (p) LET a = COUNT {{ (p)-->(x) }} > 0 LET b = EXISTS {{ MATCH (p)-->(q) WHERE \
             {a} }} FILTER {b} RETURN p.name AS n"
        );
        let (err, report) = gql_failure(|g| g.gql(&substituted).to_list());
        assert_eq!(
            kind_and_status(&err),
            (GqlErrorKind::Limit, "54001".to_string()),
            "{report}"
        );
        assert!(
            report.contains("runs its own copy), COUNT { } and COLLECT { } included"),
            "{report}"
        );
        let help = err.help().unwrap();
        assert!(
            help.contains(
                "(the same holds for COUNT { } and COLLECT { }), or join the patterns into fewer \
                 subqueries; a CALL (a) { .. } body is not copied: it runs once per row, and \
                 later clauses read its columns, as in MATCH (p) CALL (p) { OPTIONAL MATCH \
                 (p)-->(f) RETURN count(f) AS n } FILTER n > 0 RETURN p, n"
            ),
            "{help}"
        );
        assert_eq!(
            run_ok(
                "MATCH (p) CALL (p) { OPTIONAL MATCH (p)-->(f) RETURN count(f) AS n } FILTER n > \
                 0 RETURN p, n"
            ),
            3
        );
    }

    /// A name bound inside `COUNT { }` / `COLLECT { }` and read outside: the message and the
    /// help name the construct, and the help's examples run (stage 2d.T7b).
    #[test]
    fn gql_count_and_collect_scope_errors_name_them() {
        for (query, construct, fix) in [
            (
                "MATCH (a:person) WHERE COUNT { MATCH (a)-->(b) } > 0 RETURN b",
                "COUNT { }",
                "bind `b` in the MATCH clause of the query to use it here, or test it inside \
                 the COUNT { } body. ",
            ),
            (
                "MATCH (a:person) RETURN COLLECT { MATCH (a)-->(b) RETURN b.name } AS l, b",
                "COLLECT { }",
                "bind `b` in the MATCH clause of the query to use it here, or return what you \
                 need of it from the body, as in COLLECT { MATCH (a)-->(b) RETURN b.name }. ",
            ),
        ] {
            let (err, report) = gql_failure(|g| g.gql(query).to_list());
            assert_eq!(
                kind_and_status(&err),
                (GqlErrorKind::Semantic, "42002".to_string()),
                "{query}"
            );
            assert!(
                report.contains(&format!(
                    "the variable `b` is not in scope: it is bound inside a {construct} \
                     subquery, whose variables are visible only inside it"
                )),
                "{query}: {report}"
            );
            let help = err.help().unwrap();
            assert!(help.starts_with(fix), "{query}: {help}");
            assert!(
                help.contains(
                    "COUNT { <body> } is the number of matches of its body (or of the records \
                     its RETURN gives), and COLLECT { <body> RETURN <expression> } the list of \
                     the values it returns"
                ),
                "{help}"
            );
            assert!(!help.contains("EXISTS"), "{help}");
            assert_examples_run(&help);
        }
    }

    /// A first `WITH` of a `CALL { }` body that is no importing `WITH` (here with a `WHERE`)
    /// gets the targeted message, not "bound by a later MATCH clause" (stage 2d.T7b).
    #[test]
    fn gql_call_with_where_is_no_importing_with() {
        let query = "MATCH (p:person) CALL { WITH p WHERE p.age > 30 MATCH (p)-->(x) RETURN x } \
                     RETURN x.name";
        let (err, report) = gql_failure(|g| g.gql(query).to_list());
        assert_eq!(
            kind_and_status(&err),
            (GqlErrorKind::Semantic, "42002".to_string())
        );
        assert!(
            report.contains(
                "the variable `p` is not imported into the CALL body: its first WITH is not an \
                 importing WITH, which lists plain variables only (no WHERE, alias, expression, \
                 DISTINCT, ORDER BY, OFFSET or LIMIT), so the body imports nothing"
            ),
            "{report}"
        );
        assert!(!report.contains("later MATCH"), "{report}");
        let help = err.help().unwrap();
        assert!(
            help.starts_with(
                "import `p` with a variable scope clause, as in CALL (p) { MATCH ... WHERE ... }, \
                 or in Cypher with a WITH of plain variables first and the rest of the body \
                 after it, as in CALL { WITH p MATCH ... WHERE ... }. A CALL subquery"
            ),
            "{help}"
        );
        assert_examples_run(&help);
        // Both suggested forms run.
        let graph = GraphSource::tinkerpop_modern();
        let locked = graph.read();
        for fixed in [
            "MATCH (p:person) CALL (p) { MATCH (p)-->(x) WHERE p.age > 30 RETURN x } RETURN x.name",
            "MATCH (p:person) CALL { WITH p MATCH (p)-->(x) WHERE p.age > 30 RETURN x } \
             RETURN x.name",
        ] {
            let rows = locked.traversal().gql(fixed).to_list();
            assert_eq!(rows.map(|r| r.len()).ok(), Some(3), "{fixed}");
        }
    }

    /// `OTHERWISE` is "not yet" (0A000) like `EXCEPT` / `INTERSECT` (stage 2d.T7b).
    #[test]
    fn gql_otherwise_is_not_yet_supported() {
        let (err, report) = gql_failure(|g| {
            g.gql("MATCH (n) RETURN n OTHERWISE MATCH (m) RETURN m")
                .to_list()
        });
        assert_eq!(
            kind_and_status(&err),
            (GqlErrorKind::NotYet, "0A000".to_string())
        );
        assert!(
            report.contains("OTHERWISE (a conditional query) is not yet supported in this version"),
            "{report}"
        );
        let help = err.help().unwrap();
        assert!(
            help.starts_with(
                "take the first non-empty result in Gremlin with coalesce(), for example \
                 g.inject(1).coalesce(__.v().has_label(\"robot\"), \
                 __.v().has_label(\"person\")).values(\"name\")."
            ),
            "{help}"
        );
    }

    // ----- stage 3a: help sweep for INSERT / CREATE (rule 6) -----

    /// The general advice of every error about an INSERT pattern.
    const INSERT_ADVICE: &str = "INSERT (Cypher CREATE) creates its pattern once for each row: \
         a new node with the labels written, joined by & or :, and a property map; a new edge \
         with a direction and exactly one label; and a variable bound before, written alone as \
         (p), connects the existing node, as in g.gql(\"MATCH (p:person {name: 'marko'}) INSERT \
         (p)-[:likes]->(f:fruit&food {name: 'apple'}) RETURN f.name\"). A query may end with the \
         INSERT (it then returns no rows), and FOR gives it one row per item, as in \
         g.gql(\"FOR n IN ['ann', 'bob'] INSERT (:person {name: n})\"). To create a pattern \
         only where it is missing, MERGE finds it or creates it, as in \
         g.gql(\"FOR n IN ['ann', 'marko'] MERGE (:person {name: n})\").";

    /// The example of a correct INSERT in the errors of a wrong pattern.
    const INSERT_EXAMPLE: &str =
        "; for example g.gql(\"INSERT (a:person {name: 'ann'})-[:knows]->(b:person:admin)\"). ";

    /// The error of the GQL `query` run as a write on a fresh modern graph; the graph afterwards.
    fn gql_write_failure(query: &str) -> (Box<TraverserError>, TraversalGraph) {
        let mut graph = TraversalGraph::tinkerpop_modern();
        let err = graph
            .traversal_mut()
            .gql(query)
            .to_list()
            .expect_err("the GQL write must fail");
        (err, graph)
    }

    /// Runs every `g.gql(..)` example of `help` as a write, each on a fresh modern graph.
    fn assert_write_examples_run(help: &str) {
        let examples = gql_examples(help);
        assert!(!examples.is_empty(), "no g.gql(..) example in: {help}");
        for (_, query) in examples {
            let mut graph = TraversalGraph::tinkerpop_modern();
            let rows = graph.traversal_mut().gql(&query).to_list();
            assert!(rows.is_ok(), "{query}: {rows:?}");
        }
    }

    /// Element counts of a graph.
    fn counts(graph: &TraversalGraph) -> (usize, usize) {
        (graph.vertices().count(), graph.edges().count())
    }

    #[test]
    fn gql_insert_pattern_errors_build_the_fix_from_the_failing_pattern() {
        for (query, status, fix) in [
            (
                "INSERT (a)-[e:T]-(b)",
                "42001",
                "give the new edge its direction, from the node before it (-[e:T]->) or to it \
                 (<-[e:T]-)",
            ),
            (
                "CREATE (a)<-[:T|U]-(b)",
                "42001",
                "give the new edge one label, as in <-[:T]-",
            ),
            (
                "INSERT (a)-[e]->(b)",
                "42001",
                "give the new edge its type, as in -[e:T]->",
            ),
            (
                "INSERT (a)-[:T*2]->(b)",
                "42001",
                "write one edge pattern for each edge to create, as in (a)-[:T]->(m)-[:T]->(b)",
            ),
            (
                "INSERT (a:A|!B)",
                "42001",
                "write the labels the new node gets, as in (a:A&B)",
            ),
            (
                "INSERT (a {x: 1} WHERE a.x > 1)",
                "42001",
                "give the new node its properties in its map instead, as in (a:Label {k: 1}), \
                 and test conditions in a WHERE or FILTER before the INSERT",
            ),
            (
                "INSERT ((a)-[:T]->(b)){2}",
                "42001",
                "write the nodes and edges one after the other, each edge with its direction \
                 and one label",
            ),
        ] {
            let (err, graph) = gql_write_failure(query);
            let (kind, code) = kind_and_status(&err);
            assert_eq!(
                (kind, code.as_str()),
                (GqlErrorKind::Syntax, status),
                "{query}"
            );
            let help = err.help().expect("help");
            // The example keeps the keyword of the query.
            let example = INSERT_EXAMPLE.replace("INSERT", &query[..6]);
            assert_eq!(help, format!("{fix}{example}{INSERT_ADVICE}"), "{query}");
            assert!(!help.contains("a GQL read query"), "{help}");
            assert_write_examples_run(&help);
            assert_eq!(counts(&graph), (6, 6), "{query}");
        }
    }

    #[test]
    fn gql_insert_reference_errors_name_the_gremlin_steps_for_a_bound_node() {
        for (query, start) in [
            (
                "MATCH (p:person) INSERT (p:x)",
                "write (p) to connect the node, or give a new node its own name, as in INSERT \
                 (p)-[:T]->(p2:Label {k: 1}); to change the bound node itself use SET on it, as \
                 in g.gql(\"MATCH (p:person) SET p:x\"), or the Gremlin steps add_label() and \
                 property() on it, as in g.gql(\"MATCH (p) RETURN \
                 p\").select(\"p\").add_label(\"x\"). ",
            ),
            (
                "MATCH (p:person) CREATE (p {age: 1})",
                "write (p) to connect the node, or give a new node its own name, as in CREATE \
                 (p)-[:T]->(p2:Label {k: 1}); to change the bound node itself use SET on it, as \
                 in g.gql(\"MATCH (p:person) SET p += {age: 1}\"), or the Gremlin steps \
                 add_label() and property() on it, as in g.gql(\"MATCH (p) RETURN \
                 p\").select(\"p\").property(\"age\", 1). ",
            ),
            (
                "INSERT (a:x), (a:y)",
                "write (a) to connect the node, or give a new node its own name, as in INSERT \
                 (a)-[:T]->(a2:Label {k: 1}). ",
            ),
            (
                "MATCH (p:person)-[e:knows]->(q) INSERT (p)-[e:knows]->(q)",
                "give the new edge its own name, as in -[e2:T]->. ",
            ),
        ] {
            let (err, graph) = gql_write_failure(query);
            assert_eq!(
                kind_and_status(&err),
                (GqlErrorKind::Semantic, "42002".to_string()),
                "{query}"
            );
            let help = err.help().expect("help");
            assert_eq!(help, format!("{start}{INSERT_ADVICE}"), "{query}");
            assert_write_examples_run(&help);
            assert_eq!(counts(&graph), (6, 6), "{query}");
        }
        // The Gremlin form of the advice runs: it labels every node.
        let graph = std::sync::Arc::new(GraphSource::tinkerpop_modern());
        let labelled = crate::script_host::eval(
            graph,
            r#"g.gql("MATCH (p) RETURN p").select("p").add_label("x").iterate(); g.v().has_label("x").count().next()"#,
        )
        .expect("the advice runs");
        assert_eq!(labelled, "6");
        // camelCase respells the Gremlin steps of the advice, not the GQL text.
        let help = gql_failure(|g| {
            g.with("render.spelling", "camel")
                .gql("MATCH (p:person) INSERT (p:x)")
                .to_list()
        })
        .0
        .help()
        .expect("help");
        assert!(help.contains(".select(\"p\").addLabel(\"x\")"), "{help}");
        assert!(help.contains("g.gql(\"MATCH (p) RETURN p\")"), "{help}");
    }

    #[test]
    fn gql_write_inside_a_subquery_expression_names_the_clause_form() {
        for (query, message) in [
            (
                "MATCH (p) WHERE EXISTS { INSERT (:x) } RETURN p",
                "INSERT inside EXISTS { }: a subquery expression or a comprehension cannot write",
            ),
            (
                "MATCH (p) RETURN COUNT { CREATE (:x) } AS c",
                "CREATE inside COUNT { }: a subquery expression or a comprehension cannot write",
            ),
        ] {
            let (err, report) = gql_failure(|g| g.gql(query).to_list());
            assert_eq!(
                kind_and_status(&err),
                (GqlErrorKind::Syntax, "42001".to_string())
            );
            assert!(report.contains(message), "{report}");
            let help = err.help().expect("help");
            assert_eq!(
                help,
                format!(
                    "write the change as a clause of the query itself, after the clauses that \
                     find its rows, for example g.gql(\"MATCH (p:person) WHERE EXISTS {{ \
                     (p)-[:created]->() }} INSERT (:maker {{name: p.name}})\"). {INSERT_ADVICE}"
                )
            );
            assert_write_examples_run(&help);
        }
    }

    #[test]
    fn gql_null_insert_endpoint_names_the_filter_and_changes_nothing() {
        let (err, graph) = gql_write_failure(
            "MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s) INSERT (p)-[:made]->(s)",
        );
        assert_eq!(
            kind_and_status(&err),
            (GqlErrorKind::Data, "22004".to_string())
        );
        let report = format_diagnostic(&err);
        assert!(
            report.contains("the destination node `s` of the inserted `:made` edge is NULL"),
            "{report}"
        );
        let help = err.help().expect("help");
        assert_eq!(
            help,
            "keep only the rows where it is bound before the INSERT: FILTER s IS NOT NULL. A \
             row of the data gave the marked endpoint of the new edge no node: an edge connects \
             two nodes, so a variable that OPTIONAL MATCH may leave NULL is filtered first, as \
             in g.gql(\"MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s) FILTER s IS NOT \
             NULL INSERT (p)-[:made]->(s)\"), and a value that is no node is turned into one by \
             a MATCH, as in g.gql(\"FOR n IN ['lop'] MATCH (s:software {name: n}) INSERT \
             (:review {stars: 5})-[:about]->(s)\")."
        );
        assert_write_examples_run(&help);
        // The rows before the failing one (marko created lop) wrote nothing either.
        assert_eq!(counts(&graph), (6, 6));
        // The filtered query of the advice inserts one edge per creator.
        let mut graph = TraversalGraph::tinkerpop_modern();
        graph
            .traversal_mut()
            .gql(
                "MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s) FILTER s IS NOT NULL \
                 INSERT (p)-[:made]->(s)",
            )
            .to_list()
            .expect("the fix runs");
        assert_eq!(counts(&graph), (6, 10));
    }

    #[test]
    fn gql_schema_violation_of_an_insert_names_the_gql_fix() {
        let schema = GraphSchema::from_json(
            r#"{"mode": "closed",
              "vertices": {"person": {"schema": {"type": "object", "properties": {
                  "name": {"type": "string"}, "age": {"type": "integer"}},
                "required": ["name"]}}},
              "edges": {"knows": {"schema": {"type": "object", "properties": {
                  "weight": {"type": "number"}}},
                "connections": [{"from": "person", "to": "person"}]}}}"#,
        )
        .expect("schema");
        let graph_with_schema = || {
            let mut graph = TraversalGraph::new();
            graph.set_schema(schema.clone()).expect("set");
            graph
                .traversal_mut()
                .gql("INSERT (:person {name: 'marko'}), (:person {name: 'josh'})")
                .to_list()
                .expect("people");
            graph
        };
        let failure = |query: &str| {
            let mut graph = graph_with_schema();
            let err = graph
                .traversal_mut()
                .gql(query)
                .to_list()
                .expect_err("a schema violation");
            assert_eq!(counts(&graph), (2, 0), "{query}");
            err.help().expect("help")
        };
        let help = failure("INSERT (:person {name: 'ann', age: 'x'})");
        assert_eq!(
            help,
            "Convert the value to the type 'int64' where the INSERT writes it, with \
             toInteger(), as in g.gql(\"INSERT (:person {age: toInteger($v)})\", #{v: \"29\"}). \
             toInteger() gives NULL for a value it cannot convert, and a NULL value is not \
             stored; CAST(x AS ..) fails the query for it instead."
        );
        let help = failure(
            "MATCH (a {name: 'marko'}), (b {name: 'josh'}) INSERT (a)-[:knows {weight: 'x'}]->(b)",
        );
        assert!(
            help.starts_with(
                "Convert the value to the type 'float64' where the INSERT writes it, with \
                 toFloat(), as in g.gql(\"MATCH (a {name: 'marko'}), (b {name: 'josh'}) INSERT \
                 (a)-[:knows {weight: toFloat($v)}]->(b)\", #{v: \"1.5\"})."
            ),
            "{help}"
        );
        let help = failure("INSERT (:person {age: 3})");
        assert_eq!(
            help,
            "Give 'name' a value in the property map of the INSERT, as in \
             g.gql(\"INSERT (:person {name: 'value'})\"); a NULL value (such as a property the \
             matched element lacks) is not stored and so is missing too: give it a fallback, as \
             in {name: coalesce(n.name, 'unknown')}. Or remove it from the label's \"required\" \
             in the schema, or give it a \"default\" (or \"defaultFunction\") in the schema: it \
             is stored when an element is created without the property."
        );
        // The suggested forms run on the same graph.
        let mut graph = graph_with_schema();
        for (query, value) in [
            ("INSERT (:person {name: 'ann', age: toInteger($v)})", "29"),
            (
                "MATCH (a {name: 'marko'}), (b {name: 'josh'}) INSERT (a)-[:knows {weight: \
                 toFloat($v)}]->(b)",
                "1.5",
            ),
        ] {
            graph
                .traversal_mut()
                .gql_with(query, [("v", ElementProperty::from(value))])
                .to_list()
                .unwrap_or_else(|e| panic!("{query}: {}", format_diagnostic(&e)));
        }
        assert_eq!(counts(&graph), (3, 1));
        // A violation without Gremlin advice keeps its own help; the Gremlin write keeps the
        // Gremlin advice.
        let help = failure("INSERT (:robot)");
        assert!(help.starts_with("A closed schema admits only declared vertex labels"));
        let mut graph = graph_with_schema();
        let err = graph
            .traversal_mut()
            .add_v("person")
            .property("name", "ann")
            .property("age", "x")
            .to_list()
            .expect_err("a schema violation");
        let help = err.help().expect("help");
        assert!(
            help.contains("values(\"age\").as_number(GType.LONG)"),
            "{help}"
        );
    }

    #[test]
    fn gql_refused_constructs_no_longer_call_gql_read_only() {
        let (err, _) = gql_write_failure("CREATE INDEX FOR (n:person) ON (n.age)");
        assert_eq!(kind_and_status(&err).0, GqlErrorKind::Refused);
        let help = err.help().expect("help");
        assert!(
            help.ends_with(
                "gql() does not support this construct; write this part of the query as \
                 Gremlin steps."
            ),
            "{help}"
        );
        assert!(!help.contains("read"), "{help}");
        let (err, _) = gql_write_failure("MATCH (n) RETURN n.name ORDER BY id(n)");
        let help = err.help().expect("help");
        assert!(!help.contains("read queries"), "{help}");
    }

    // ----- stage 3c: help sweep for SET / REMOVE (rule 6) -----

    /// The general advice of every error about a SET / REMOVE item.
    const SET_ADVICE: &str = "SET changes the nodes and edges of each row, item by item from \
         left to right: n.k = <value> sets a property (a NULL value removes it), n = {map} \
         replaces every property, n += {map} merges the map into them, and n:A:B adds labels to \
         a node (an edge has exactly one label); REMOVE n.k removes a property and REMOVE n:A a \
         label, as in g.gql(\"MATCH (p:person {name: 'marko'}) SET p.age = p.age + 1, \
         p:employee REMOVE p.nick RETURN p.age\"). A variable that OPTIONAL MATCH leaves NULL \
         is skipped, and a query may end with the SET or REMOVE (it then returns no rows), as in \
         g.gql(\"MATCH (s:software) SET s += {open: true}\").";

    /// The advice a data error of a SET / REMOVE item adds before [`SET_ADVICE`].
    const SET_DATA_ADVICE: &str = "A row of the data gave the marked SET or REMOVE item a value \
         it cannot write; keep such rows away from it with FILTER or WHERE, as in \
         g.gql(\"MATCH (p:person) FILTER p.age IS NOT NULL SET p.next = p.age + 1\"). ";

    /// The canonical state of a graph: every vertex and edge with its labels and properties,
    /// sorted (a failed write must leave it as it was).
    fn state(graph: &TraversalGraph) -> Vec<String> {
        let props = |map: Option<&graphersal::storage::PropertyMap>| {
            let mut entries: Vec<String> = map
                .map(|m| m.iter().map(|(k, v)| format!("{k}={v:?}")).collect())
                .unwrap_or_default();
            entries.sort();
            entries.join(",")
        };
        let mut lines: Vec<String> = graph
            .vertices()
            .map(|v| {
                format!(
                    "V {:?} {}",
                    graph.vertex_labels(v),
                    props(graph.vertex_properties(v))
                )
            })
            .collect();
        lines.extend(graph.edges().map(|e| {
            format!(
                "E {:?} {}",
                graph.edge_label(e),
                props(graph.edge_properties(e))
            )
        }));
        lines.sort();
        lines
    }

    /// D4: `SET n = NULL` / `SET n += NULL` is 22004; the help names `SET n = {}` and a
    /// `coalesce()` default built into the failing query, and the rewritten queries run (the
    /// `=` one keeps every property).
    #[test]
    fn gql_set_of_a_null_map_names_the_empty_map_and_a_coalesce_default() {
        let before = state(&TraversalGraph::tinkerpop_modern());
        for (query, fix) in [
            (
                "MATCH (p:person) SET p = NULL",
                "SET p = {} removes every property; to keep them when the map is NULL, give it \
                 the element's own properties as a default with coalesce($p, properties(p)), \
                 as in g.gql(\"MATCH (p:person) SET p = coalesce(NULL, properties(p))\"). ",
            ),
            (
                "MATCH (p:person) SET p += NULL",
                "SET p += {} changes nothing: give a NULL map that default with coalesce($p, \
                 {}), as in g.gql(\"MATCH (p:person) SET p += coalesce(NULL, {})\"). ",
            ),
        ] {
            let (err, graph) = gql_write_failure(query);
            assert_eq!(
                kind_and_status(&err),
                (GqlErrorKind::Data, "22004".to_string()),
                "{query}"
            );
            let help = err.help().expect("help");
            assert_eq!(
                help,
                format!("{fix}{SET_DATA_ADVICE}{SET_ADVICE}"),
                "{query}"
            );
            assert_write_examples_run(&help);
            assert_eq!(state(&graph), before, "{query}");
        }
        // The fixes change nothing on the modern graph.
        for query in [
            "MATCH (p:person) SET p = coalesce(NULL, properties(p))",
            "MATCH (p:person) SET p += coalesce(NULL, {})",
        ] {
            let mut graph = TraversalGraph::tinkerpop_modern();
            graph.traversal_mut().gql(query).to_list().expect(query);
            assert_eq!(state(&graph), before, "{query}");
        }
    }

    /// The other run-time errors of SET / REMOVE items: the fix rewrites the failing query
    /// where it can (`p.k = 1` for a map item given a single value, `element_id()` for an
    /// element stored as a property), names the label of an edge's label item, and finds the
    /// node with a MATCH for a value target. Each failure leaves the graph as it was.
    #[test]
    fn gql_set_and_remove_data_errors_build_the_fix_from_the_failing_query() {
        let before = state(&TraversalGraph::tinkerpop_modern());
        for (query, status, fix) in [
            (
                "MATCH (p:person) SET p = 1",
                "22G03",
                "set one property with SET p.k = <value>, as in g.gql(\"MATCH (p:person) SET \
                 p.k = 1\"), or give a map, as in SET p = {k: 1}. ",
            ),
            (
                "MATCH (p) SET p.k = p",
                "22G03",
                "store a value of it instead, such as its id, as in g.gql(\"MATCH (p) SET p.k = \
                 element_id(p)\"), or its properties with properties(..). ",
            ),
            (
                "FOR x IN [1] REMOVE x.k",
                "22G03",
                "keep only the rows where `x` holds an element, or find the node first with a \
                 MATCH and remove it, as in g.gql(\"FOR n IN ['marko'] MATCH (x {name: n}) \
                 REMOVE x.age RETURN x.name\"). ",
            ),
            (
                "FOR x IN ['a'] SET x.k = 1",
                "22G03",
                "keep only the rows where `x` holds an element, or find the node first with a \
                 MATCH and set it, as in g.gql(\"FOR n IN ['marko'] MATCH (x {name: n}) SET \
                 x.k = 1 RETURN x.name\"). ",
            ),
            (
                "MATCH ()-[e]->() WITH collect(e) AS es FOR x IN es SET x:met",
                "22G03",
                "an edge has exactly one label, changed with the Gremlin step set_label(), for \
                 example g.e().has_label(\"knows\").set_label(\"met\"). ",
            ),
        ] {
            let (err, graph) = gql_write_failure(query);
            assert_eq!(
                kind_and_status(&err),
                (GqlErrorKind::Data, status.to_string()),
                "{query}"
            );
            let help = err.help().expect("help");
            assert_eq!(
                help,
                format!("{fix}{SET_DATA_ADVICE}{SET_ADVICE}"),
                "{query}"
            );
            assert_write_examples_run(&help);
            assert_eq!(state(&graph), before, "{query}");
        }
        // A query too long to quote keeps the general form of the fix.
        let long = format!(
            "MATCH (p:person) WHERE p.name <> '{}' SET p = 1",
            "x".repeat(1200)
        );
        let (err, _) = gql_write_failure(&long);
        let help = err.help().expect("help");
        assert!(
            help.starts_with(
                "set one property with SET p.k = <value>, or give a map, as in SET p = {k: 1}. "
            ),
            "{help}"
        );
    }

    /// A label item on a variable the binder knows is an edge (42002): the Gremlin fix reads
    /// the edge from the failing query's own clauses (or a plain MATCH inside a CALL body), and
    /// runs.
    #[test]
    fn gql_edge_label_item_names_set_label_on_the_failing_query() {
        for (query, fix) in [
            (
                "MATCH (a:person)-[e:knows]->(b) WHERE b.age > 30 SET e:met",
                "change the type of the edge with the Gremlin step set_label(), as in \
                 g.gql(\"MATCH (a:person)-[e:knows]->(b) WHERE b.age > 30 RETURN \
                 e\").select(\"e\").set_label(\"met\"). ",
            ),
            (
                "MATCH (a)-[e:created]->(b) REMOVE e:created",
                "change the type of the edge with the Gremlin step set_label(), as in \
                 g.gql(\"MATCH (a)-[e:created]->(b) RETURN e\").select(\"e\").set_label(\"created\"). ",
            ),
            (
                "MATCH (a)-[e]->(b) CALL (e) { SET e:x }",
                "change the type of the edge with the Gremlin step set_label(), as in \
                 g.gql(\"MATCH ()-[e]->() RETURN e\").select(\"e\").set_label(\"x\"). ",
            ),
        ] {
            let (err, graph) = gql_write_failure(query);
            assert_eq!(
                kind_and_status(&err),
                (GqlErrorKind::Semantic, "42002".to_string()),
                "{query}"
            );
            let help = err.help().expect("help");
            assert!(help.starts_with(fix), "{query}: {help}");
            assert_eq!(counts(&graph), (6, 6), "{query}");
            // The Gremlin form runs.
            let script = fix
                .trim_start_matches(
                    "change the type of the edge with the Gremlin step set_label(), as in ",
                )
                .trim_end_matches(". ");
            let graph = std::sync::Arc::new(GraphSource::tinkerpop_modern());
            crate::script_host::eval(graph, &format!("{script}.iterate(); 1"))
                .unwrap_or_else(|e| panic!("{script}: {e}"));
        }
    }

    /// Syntax errors of the form of a SET / REMOVE item, and a token after the clause, carry
    /// the SET advice; an error inside an item's value keeps the general advice.
    #[test]
    fn gql_set_and_remove_syntax_errors_carry_the_set_advice() {
        for (query, start) in [
            ("MATCH (p) SET p.k", "Expected `=` at the marked position. "),
            (
                "MATCH (p) SET",
                "Expected a variable at the marked position. ",
            ),
            (
                "MATCH (p) SET p 1",
                "Expected `.`, `=`, `+=`, `:`, IS at the marked position. ",
            ),
            ("MATCH (p) SET p:A|B", "write the labels, as in SET p:A:B. "),
            (
                "MATCH (p) REMOVE p.k = 1",
                "Expected `,`, MATCH, OPTIONAL MATCH, WITH, LET, FILTER, FOR, UNWIND, CALL, \
                 RETURN at the marked position. ",
            ),
            (
                "MATCH (p) REMOVE p",
                "REMOVE removes a property (REMOVE p.k) or a label (REMOVE p:A); to remove the \
                 element itself, write DETACH DELETE p, as in g.gql(\"MATCH (p) DETACH DELETE \
                 p\"). Expected `.`, `:`, IS at the marked position. ",
            ),
        ] {
            let (err, _) = gql_write_failure(query);
            assert_eq!(kind_and_status(&err).0, GqlErrorKind::Syntax, "{query}");
            let help = err.help().expect("help");
            assert_eq!(help, format!("{start}{SET_ADVICE}"), "{query}");
            assert_write_examples_run(&help);
        }
        let (err, _) = gql_write_failure("MATCH (p) SET p.k = (1 +");
        let help = err.help().expect("help");
        assert!(
            help.contains("a GQL read query is MATCH <pattern>"),
            "{help}"
        );
        assert!(!help.contains(SET_ADVICE), "{help}");
    }

    /// SET / REMOVE inside a subquery expression: the fix is the clause of the query itself,
    /// with a SET or REMOVE example.
    #[test]
    fn gql_set_inside_a_subquery_expression_names_the_clause_form() {
        for (query, change) in [
            (
                "MATCH (p) WHERE EXISTS { MATCH (p) SET p.x = 1 } RETURN p",
                "SET p.maker = true INSERT (:maker {name: p.name})",
            ),
            (
                "MATCH (p) RETURN COUNT { MATCH (p) REMOVE p.x RETURN p } AS c",
                "REMOVE p.age INSERT (:maker {name: p.name})",
            ),
        ] {
            let (err, graph) = gql_write_failure(query);
            assert_eq!(kind_and_status(&err).0, GqlErrorKind::Syntax, "{query}");
            let help = err.help().expect("help");
            assert_eq!(
                help,
                format!(
                    "write the change as a clause of the query itself, after the clauses that \
                     find its rows, for example g.gql(\"MATCH (p:person) WHERE EXISTS {{ \
                     (p)-[:created]->() }} {change}\"). {SET_ADVICE}"
                ),
                "{query}"
            );
            assert_write_examples_run(&help);
            assert_eq!(counts(&graph), (6, 6));
        }
    }

    /// Schema violations raised by `gql:set` / `gql:remove`: GQL fixes naming the label and
    /// key (a conversion, keeping a required key, setting it before the label that requires
    /// it); the suggested forms run on the same graph; a violation without a GQL fix keeps its
    /// own help. Each failure leaves the graph as it was.
    #[test]
    fn gql_schema_violation_of_a_set_names_the_gql_fix() {
        let schema = |mode: &str| {
            GraphSchema::from_json(&format!(
                r#"{{"mode": "{mode}",
                  "vertices": {{"person": {{"schema": {{"type": "object", "properties": {{
                      "name": {{"type": "string"}}, "age": {{"type": "integer"}}}},
                    "required": ["name"]}}}}}},
                  "edges": {{"knows": {{"schema": {{"type": "object", "properties": {{
                      "weight": {{"type": "number"}}}}}},
                    "connections": [{{"from": "person", "to": "person"}}]}}}}}}"#
            ))
            .expect("schema")
        };
        let graph_with = |mode: &str| {
            let mut graph = TraversalGraph::new();
            graph.set_schema(schema(mode)).expect("set");
            graph
                .traversal_mut()
                .gql("INSERT (:person {name: 'marko'})-[:knows]->(:person {name: 'josh'})")
                .to_list()
                .expect("people");
            if mode == "open" {
                graph
                    .traversal_mut()
                    .gql("INSERT (:thing {k: 1})")
                    .to_list()
                    .expect("thing");
            }
            graph
        };
        let failure = |mode: &str, query: &str| {
            let mut graph = graph_with(mode);
            let before = state(&graph);
            let err = graph
                .traversal_mut()
                .gql(query)
                .to_list()
                .expect_err("a schema violation");
            assert_eq!(state(&graph), before, "{query}");
            err.help().expect("help")
        };
        assert_eq!(
            failure("closed", "MATCH (p:person) SET p.age = 'x'"),
            "Convert the value to the type 'int64' where the SET writes it, with toInteger(), \
             as in g.gql(\"MATCH (n:person) SET n.age = toInteger($v)\", #{v: \"29\"}). \
             toInteger() gives NULL for a value it cannot convert, and SET of a NULL value \
             removes the property; CAST(x AS ..) fails the query for it instead."
        );
        assert!(
            failure("closed", "MATCH ()-[e:knows]->() SET e += {weight: 'h'}").starts_with(
                "Convert the value to the type 'float64' where the SET writes it, with \
                 toFloat(), as in g.gql(\"MATCH ()-[e:knows]->() SET e.weight = toFloat($v)\", \
                 #{v: \"1.5\"})."
            )
        );
        let required = "'name' is in \"required\" of vertex label 'person': SET n.name = NULL \
             and REMOVE n.name remove it, and SET n = {map} removes every key the map lacks. Keep \
             it in the map, as in g.gql(\"MATCH (n:person) SET n = {name: n.name}\"), or merge \
             with +=, which keeps the keys the map lacks, as in g.gql(\"MATCH (n:person) SET n \
             += {name: 'value'}\"), and give a value that may be NULL a fallback, as in SET \
             n.name = coalesce($v, n.name). Or remove it from \"required\" in the schema \
             (g.patch_schema(..)), or set the schema mode to \"none\".";
        for query in [
            "MATCH (p:person) REMOVE p.name",
            "MATCH (p:person) SET p.name = NULL",
            "MATCH (p:person) SET p = {age: 3}",
        ] {
            assert_eq!(failure("closed", query), required, "{query}");
        }
        assert_eq!(
            failure("open", "MATCH (p:thing) SET p:person"),
            "The label 'person' requires 'name': set it before the label in the same SET (the \
             items apply from left to right), as in g.gql(\"MATCH (n) SET n.name = 'value', \
             n:person\"). Or remove it from the label's \"required\" in the schema (a \
             \"default\" fills only elements created without the property, not a label added \
             later)."
        );
        // The suggested forms run on the same graphs.
        let mut graph = graph_with("closed");
        for (query, value) in [
            ("MATCH (n:person) SET n.age = toInteger($v)", "29"),
            ("MATCH ()-[e:knows]->() SET e.weight = toFloat($v)", "1.5"),
            ("MATCH (n:person) SET n = {name: n.name}", ""),
            ("MATCH (n:person) SET n += {name: 'value'}", ""),
        ] {
            graph
                .traversal_mut()
                .gql_with(query, [("v", ElementProperty::from(value))])
                .to_list()
                .unwrap_or_else(|e| panic!("{query}: {}", format_diagnostic(&e)));
        }
        let mut graph = graph_with("open");
        graph
            .traversal_mut()
            .gql("MATCH (n:thing) SET n.name = 'value', n:person")
            .to_list()
            .unwrap_or_else(|e| panic!("{}", format_diagnostic(&e)));
        // Without a GQL fix the violation keeps its own help.
        assert!(
            failure("closed", "MATCH (p:person) SET p:robot")
                .starts_with("A closed schema admits only declared vertex labels")
        );
        assert!(failure("closed", "MATCH (p:person) SET p.zzz = 1").starts_with("Declare 'zzz'"));
    }

    /// The general advice of an error about a `DELETE` clause (stage 3d).
    const DELETE_ADVICE: &str = "DELETE removes, for each row, the nodes and edges its targets \
         give: a node or edge variable, a path (all its nodes and edges) or a list of them; a \
         NULL target is skipped, and an element given twice is removed once. A node that still \
         has edges is removed only together with them: DETACH DELETE removes its edges too, as \
         in g.gql(\"MATCH (s:software {name: 'lop'}) DETACH DELETE s\"). The rest of the query \
         can no longer read what it deleted, so read what it needs first, as in \
         g.gql(\"MATCH (p:person {name: 'vadas'}) WITH p, p.name AS name DETACH DELETE p \
         RETURN name\"); a query may end with the DELETE (it then returns no rows).";

    /// G1001: a plain or NODETACH `DELETE` of a node that still has edges names `DETACH
    /// DELETE` built from the failing query and the same-clause form, without the generic
    /// data-row sentence; both fixes run, and the failure changes nothing.
    #[test]
    fn gql_delete_of_a_node_with_edges_names_detach_delete_on_the_failing_query() {
        let before = state(&TraversalGraph::tinkerpop_modern());
        let same_clause = "g.gql(\"MATCH (s:software {name: 'ripple'})<-[r]-() DELETE r, s\")";
        for (query, fixed) in [
            (
                "MATCH (n:person {name: 'marko'}) DELETE n",
                "MATCH (n:person {name: 'marko'}) DETACH DELETE n",
            ),
            (
                "MATCH (n:person {name: 'marko'}) NODETACH DELETE n RETURN count(*) AS c",
                "MATCH (n:person {name: 'marko'}) DETACH DELETE n RETURN count(*) AS c",
            ),
            (
                "MATCH (n) CALL (n) { DELETE n RETURN 1 AS y } RETURN y",
                "MATCH (n) CALL (n) { DETACH DELETE n RETURN 1 AS y } RETURN y",
            ),
        ] {
            let (err, graph) = gql_write_failure(query);
            assert_eq!(
                kind_and_status(&err),
                (GqlErrorKind::Data, "G1001".to_string()),
                "{query}"
            );
            let help = err.help().expect("help");
            assert_eq!(
                help,
                format!(
                    "remove the node together with its edges with DETACH DELETE, as in \
                     g.gql(\"{fixed}\"), or delete its edges in the same clause, as in \
                     {same_clause}."
                ),
                "{query}"
            );
            assert!(!help.contains("A row of the data"), "{help}");
            assert_write_examples_run(&help);
            assert_eq!(state(&graph), before, "{query}");
        }
    }

    /// W27 / W28: a read, a write or a `RETURN` of an element deleted earlier names reading it
    /// with `properties(..)` before the `DELETE` (an edge also gets its own example); the
    /// examples run, and the failure changes nothing.
    #[test]
    fn gql_reads_of_a_deleted_element_name_properties_before_the_delete() {
        let before = state(&TraversalGraph::tinkerpop_modern());
        let node = "The node was deleted earlier in this query (by DELETE or DETACH DELETE, or \
             together with a node a DETACH DELETE removed), but a later part of the query still \
             reads it: a deleted node has no properties, labels or id any more and cannot be \
             returned or changed, and a pattern from it matches nothing; only count(n) and n IS \
             NULL still work. Read what you need before the DELETE and keep that instead, for \
             example g.gql(\"MATCH (n:person {name: 'marko'}) WITH n, properties(n) AS p DETACH \
             DELETE n RETURN p\"), or run the DELETE as a separate query after reading.";
        let edge = "The edge was deleted earlier in this query (by DELETE or DETACH DELETE, or \
             together with a node a DETACH DELETE removed), but a later part of the query still \
             reads it: a deleted edge has no properties, labels or id any more and cannot be \
             returned or changed, and a pattern from it matches nothing; only count(r), r IS NULL \
             and type(r) still work. Read what you need before the DELETE and keep that instead, \
             for example g.gql(\"MATCH (:person)-[r:knows]->() WITH r, properties(r) AS p DELETE \
             r RETURN p\"), or for a node g.gql(\"MATCH (n:person {name: 'marko'}) WITH n, \
             properties(n) AS p DETACH DELETE n RETURN p\"), or run the DELETE as a separate \
             query after reading.";
        for (query, expected) in [
            ("MATCH (n:software) DETACH DELETE n RETURN n.name", node),
            ("MATCH (n:software) DETACH DELETE n RETURN n", node),
            ("MATCH (n:software) DETACH DELETE n SET n.k = 1", node),
            ("MATCH (n:software) DETACH DELETE n REMOVE n.lang", node),
            (
                "MATCH (n:software) DETACH DELETE n INSERT (n)-[:k]->(:x)",
                node,
            ),
            ("MATCH ()-[r:created]->() DELETE r RETURN r.weight", edge),
            ("MATCH ()-[r:created]->() DELETE r SET r.k = 1", edge),
        ] {
            let (err, graph) = gql_write_failure(query);
            let help = err.help().expect("help");
            assert_eq!(help, expected, "{query}");
            assert_write_examples_run(&help);
            assert_eq!(state(&graph), before, "{query}");
        }
    }

    /// E1 / E2 and the run-time 22G03: a DELETE target that is no element gets a runnable fix
    /// built from the failing query, then the DELETE advice; every example runs.
    #[test]
    fn gql_delete_target_errors_build_a_runnable_fix() {
        let before = state(&TraversalGraph::tinkerpop_modern());
        for (query, kind, status, fix) in [
            (
                "MATCH (s:software {name: 'ripple'})<-[r]-() DELETE r, s:software",
                GqlErrorKind::Syntax,
                "42001",
                "delete the element itself with DELETE s, as in g.gql(\"MATCH (s:software \
                 {name: 'ripple'})<-[r]-() DELETE r, s\"), or remove the label and keep the \
                 element with REMOVE s:software, as in g.gql(\"MATCH (s:software {name: \
                 'ripple'})<-[r]-() DELETE r REMOVE s:software\"). ",
            ),
            (
                "MATCH (n:person {name: 'vadas'}) DETACH DELETE n IS person",
                GqlErrorKind::Syntax,
                "42001",
                "delete the element itself with DETACH DELETE n, as in g.gql(\"MATCH \
                 (n:person {name: 'vadas'}) DETACH DELETE n\"), or remove the label and keep \
                 the element with REMOVE n:person, as in g.gql(\"MATCH (n:person {name: \
                 'vadas'}) REMOVE n:person\"). ",
            ),
            (
                "MATCH (n:person) DELETE 1 + 1",
                GqlErrorKind::Semantic,
                "42002",
                "name the element to remove by its variable, as in g.gql(\"MATCH (n:person) \
                 DETACH DELETE n\"). ",
            ),
            (
                "MATCH (a)-[e:knows]->(b) DELETE 'x'",
                GqlErrorKind::Semantic,
                "42002",
                "name the element to remove by its variable, as in g.gql(\"MATCH \
                 (a)-[e:knows]->(b) DELETE e\"). ",
            ),
            (
                "RETURN 1 AS x NEXT DELETE 2",
                GqlErrorKind::Semantic,
                "42002",
                "name the element to remove by its variable, as in g.gql(\"MATCH \
                 (p:person)-[e:knows]->() DELETE e\"). ",
            ),
            (
                "MATCH (n:person) WITH 1 AS x DELETE x",
                GqlErrorKind::Data,
                "22G03",
                "give DELETE a node, an edge, a path or a list of them, and keep the rows where \
                 the target holds something else away from it with FILTER or WHERE, as in \
                 g.gql(\"MATCH (n:person {name: 'marko'}) DETACH DELETE n\"). ",
            ),
            (
                "MATCH (n:person) DELETE n,",
                GqlErrorKind::Syntax,
                "42001",
                "Expected an expression at the marked position. ",
            ),
        ] {
            let (err, graph) = gql_write_failure(query);
            assert_eq!(kind_and_status(&err), (kind, status.to_string()), "{query}");
            let help = err.help().expect("help");
            assert_eq!(help, format!("{fix}{DELETE_ADVICE}"), "{query}");
            assert!(!help.contains("a GQL read query is"), "{help}");
            assert_write_examples_run(&help);
            assert_eq!(state(&graph), before, "{query}");
        }
    }

    /// DELETE inside a subquery expression: the fix is the clause of the query itself, with a
    /// DELETE as written (a node without edges for a plain or NODETACH DELETE), then the DELETE
    /// advice.
    #[test]
    fn gql_delete_inside_a_subquery_expression_names_the_clause_form() {
        for (query, change) in [
            (
                "MATCH (p) RETURN COUNT { MATCH (p) DELETE p } AS c",
                "WHERE NOT EXISTS { (p)--() } INSERT (:maker {name: p.name}) DELETE p",
            ),
            (
                "MATCH (p) WHERE EXISTS { MATCH (p) NODETACH DELETE p } RETURN p",
                "WHERE NOT EXISTS { (p)--() } INSERT (:maker {name: p.name}) NODETACH DELETE p",
            ),
            (
                "MATCH (p) RETURN [x IN [1] WHERE EXISTS { MATCH (p) DETACH DELETE p }] AS l",
                "WHERE EXISTS { (p)-[:created]->() } INSERT (:maker {name: p.name}) DETACH \
                 DELETE p",
            ),
        ] {
            let (err, graph) = gql_write_failure(query);
            assert_eq!(
                kind_and_status(&err),
                (GqlErrorKind::Syntax, "42001".to_string()),
                "{query}"
            );
            let help = err.help().expect("help");
            assert_eq!(
                help,
                format!(
                    "write the change as a clause of the query itself, after the clauses that \
                     find its rows, for example g.gql(\"MATCH (p:person) {change}\"). \
                     {DELETE_ADVICE}"
                ),
                "{query}"
            );
            assert_write_examples_run(&help);
            assert_eq!(counts(&graph), (6, 6));
        }
    }

    /// The forms a Gremlin or Cypher user may write to delete: `REMOVE n` (the item forms and
    /// `DETACH DELETE n`), `DROP n` (no such clause: `DETACH DELETE`), `FOREACH` over the
    /// nodes of a path (DELETE takes the list); the rewritten queries run.
    #[test]
    fn gql_remove_and_drop_of_an_element_name_detach_delete() {
        let (err, _) = gql_write_failure("MATCH (p:person) REMOVE p");
        let help = err.help().expect("help");
        assert!(
            help.starts_with(
                "REMOVE removes a property (REMOVE p.k) or a label (REMOVE p:A); to remove the \
                 element itself, write DETACH DELETE p, as in g.gql(\"MATCH (p:person) DETACH \
                 DELETE p\"). Expected `.`, `:`, IS at the marked position. "
            ),
            "{help}"
        );
        assert_write_examples_run(&help);
        for (query, fixed) in [
            (
                "MATCH (p:person) DROP p",
                "MATCH (p:person) DETACH DELETE p",
            ),
            (
                "MATCH (s:software) drop s RETURN count(*) AS n",
                "MATCH (s:software) DETACH DELETE s RETURN count(*) AS n",
            ),
        ] {
            let (err, graph) = gql_write_failure(query);
            assert_eq!(
                kind_and_status(&err),
                (GqlErrorKind::Syntax, "42001".to_string()),
                "{query}"
            );
            let report = format_diagnostic(&err);
            assert!(
                report.contains("GQL has no DROP clause: DELETE removes nodes and edges"),
                "{report}"
            );
            let help = err.help().expect("help");
            assert_eq!(
                help,
                format!(
                    "write DETACH DELETE (the GQL form of the Gremlin step drop()), as in \
                     g.gql(\"{fixed}\"). {DELETE_ADVICE}"
                ),
                "{query}"
            );
            assert_write_examples_run(&help);
            assert_eq!(counts(&graph), (6, 6));
        }
        let (_, report) = gql_failure(|g| {
            g.gql("MATCH p = (:person)-->() FOREACH (n IN nodes(p) | DELETE n)")
                .to_list()
        });
        assert!(
            report.contains(
                "DELETE takes a list itself, as in MATCH p = (:person)-[:knows]->() DETACH \
                 DELETE nodes(p)."
            ),
            "{report}"
        );
        let mut graph = TraversalGraph::tinkerpop_modern();
        graph
            .traversal_mut()
            .gql("MATCH p = (:person)-[:knows]->() DETACH DELETE nodes(p)")
            .to_list()
            .expect("the FOREACH alternative runs");
        assert_eq!(counts(&graph), (3, 1));
    }

    // ----- stage 3e: help sweep for MERGE (rule 6) -----

    /// The general advice of an error about a `MERGE` clause (stage 3e).
    const MERGE_ADVICE: &str = "MERGE looks for its whole pattern for each row: every match is \
         a row, and when nothing matches the whole pattern is created, reusing the nodes bound \
         before. Its one pattern has nodes with the labels written, joined by & or :, and a \
         property map, edges with exactly one label (an edge without a direction matches either \
         direction and is created from left to right), and a node bound before written alone, \
         as (p); ON CREATE SET and ON MATCH SET items change what was created or matched, as in \
         g.gql(\"MERGE (p:person {name: 'ann'}) ON CREATE SET p.new = true ON MATCH SET p.new \
         = false RETURN p.new\"). Each row sees what the rows before it created, so FOR creates \
         a repeated item once, as in g.gql(\"FOR n IN ['ann', 'ann'] MERGE (:person {name: \
         n})\").";

    /// The sentence a run-time data error of a `MERGE` pattern adds before the advice.
    const MERGE_DATA: &str = "A row of the data gave the marked part of the MERGE pattern a \
         value it can neither match nor create: a property value that is NULL or a graph \
         element, or a node bound before that is NULL or no node. A variable that OPTIONAL \
         MATCH may leave NULL is filtered first, and a value that is no node is turned into one \
         by a MATCH, as in g.gql(\"FOR n IN ['lop'] MATCH (s:software {name: n}) MERGE \
         (:review {stars: 5})-[:about]->(s)\"). ";

    /// E1 (a NULL property value, literal or computed) and a NULL or non-node bound end of a
    /// MERGE edge: MERGE wording (not the INSERT's, the creator being the INSERT routine), the
    /// fix built from the failing query, the MERGE advice; the fixes run, the failure (after
    /// earlier rows wrote) changes nothing.
    #[test]
    fn gql_merge_data_errors_name_the_merge_fix_built_from_the_failing_query() {
        for (query, status, message, fix) in [
            (
                "MERGE (p:person {name: NULL})",
                "22004",
                "MERGE cannot match or create `{name: NULL}`: a property value is NULL",
                "MERGE matches and creates only property values that are not NULL: leave `name` \
                 out of the pattern, and find the elements that lack it with a MATCH, as in \
                 g.gql(\"MATCH (n) FILTER n.name IS NULL RETURN n\"). ",
            ),
            (
                "FOR x IN [1, NULL] MERGE (:n {k: x})",
                "22004",
                "MERGE cannot match or create `{k: NULL}`: a property value is NULL",
                "keep only the rows where it has a value with FILTER x IS NOT NULL before the \
                 MERGE, or give it a default with coalesce(), as in g.gql(\"FOR x IN [1, NULL] \
                 MERGE (:n {k: coalesce(x, 0)})\"). ",
            ),
            (
                "MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s) MERGE (p)-[:made]->(s)",
                "22004",
                "the destination node `s` of the merged `:made` edge is NULL",
                "keep only the rows where it is bound before the MERGE: FILTER s IS NOT NULL, as \
                 in g.gql(\"MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s) FILTER s IS NOT \
                 NULL MERGE (p)-[:made]->(s)\"). ",
            ),
            (
                "FOR x IN [1] MATCH (p:person) WITH p, x MERGE (p)-[:k]->(x)",
                "22G03",
                "the destination node `x` of the merged `:k` edge is not a node",
                "",
            ),
            (
                "MATCH (p:person) MERGE (q:x {k: p})",
                "22G03",
                "the value of property `k` is a node",
                "store a value of it instead, such as {k: element_id(..)} or {k: \
                 properties(..)}. ",
            ),
        ] {
            let (err, graph) = gql_write_failure(query);
            assert_eq!(
                kind_and_status(&err),
                (GqlErrorKind::Data, status.to_string()),
                "{query}"
            );
            let report = format_diagnostic(&err);
            assert!(report.contains(message), "{query}: {report}");
            assert!(!report.contains("INSERT"), "{query}: {report}");
            let help = err.help().expect("help");
            assert_eq!(help, format!("{fix}{MERGE_DATA}{MERGE_ADVICE}"), "{query}");
            assert_write_examples_run(&help);
            assert_eq!(counts(&graph), (6, 6), "{query}");
        }
    }

    /// E2 (a lone bound node) and W30 (a second pattern): the fixes are the failing query with
    /// the pattern replaced, and they run.
    #[test]
    fn gql_merge_pattern_errors_build_the_fix_from_the_failing_query() {
        for (query, status, fix) in [
            (
                "MATCH (a) MERGE (a)",
                "42002",
                "to find or create a connection, give the pattern an edge, as in \
                 g.gql(\"MATCH (a) MERGE (a)-[:T]->(:Label)\"); to find or create another node, \
                 give it its own name, as in g.gql(\"MATCH (a) MERGE (a2:Label)\"). ",
            ),
            (
                "MERGE (a:x), (b:y)",
                "42001",
                "write one MERGE for each pattern, as in g.gql(\"MERGE (a:x) MERGE (b:y)\"), or \
                 connect the patterns with an edge into one. ",
            ),
        ] {
            let (err, graph) = gql_write_failure(query);
            assert_eq!(kind_and_status(&err).1, status, "{query}");
            let help = err.help().expect("help");
            assert_eq!(help, format!("{fix}{MERGE_ADVICE}"), "{query}");
            assert_write_examples_run(&help);
            assert_eq!(counts(&graph), (6, 6), "{query}");
        }
    }

    /// A schema violation of the create of a MERGE names the MERGE (with the node variable of
    /// the failing pattern), a `required` property only ON CREATE SET gives explains C8 (the
    /// items run after the create); one of an ON item gives the SET fix in MERGE form. The
    /// suggested forms run; the failure changes nothing.
    #[test]
    fn gql_schema_violation_of_a_merge_names_the_merge_fix() {
        let schema = GraphSchema::from_json(
            r#"{"mode": "closed",
              "vertices": {"person": {"schema": {"type": "object", "properties": {
                  "name": {"type": "string"}, "age": {"type": "integer"}},
                "required": ["name"]}}}}"#,
        )
        .expect("schema");
        let graph_with_schema = || {
            let mut graph = TraversalGraph::new();
            graph.set_schema(schema.clone()).expect("set");
            graph
                .traversal_mut()
                .gql("INSERT (:person {name: 'marko'})")
                .to_list()
                .expect("people");
            graph
        };
        let failure = |query: &str| {
            let mut graph = graph_with_schema();
            let err = graph
                .traversal_mut()
                .gql(query)
                .to_list()
                .expect_err("a schema violation");
            assert_eq!(counts(&graph), (1, 0), "{query}");
            err.help().expect("help")
        };
        let help = failure("FOR k IN [1, 2] MERGE (p:person {age: k}) ON CREATE SET p.name = 'x'");
        assert!(
            help.starts_with(
                "Give 'name' a value in the property map of the MERGE pattern (the ON CREATE SET \
                 items run after the create, so they cannot give it; to match on other keys \
                 only, give it a \"default\" in the schema), as in g.gql(\"MERGE (p:person \
                 {name: 'value'})\"); "
            ),
            "{help}"
        );
        let help = failure("MERGE (q:person {name: 'ann', age: 'x'})");
        assert!(
            help.starts_with(
                "Convert the value to the type 'int64' where the MERGE writes it, with \
                 toInteger(), as in g.gql(\"MERGE (q:person {age: toInteger($v)})\", #{v: \
                 \"29\"})."
            ),
            "{help}"
        );
        let help = failure("MERGE (p:person {name: 'ann'}) ON CREATE SET p.age = 'x'");
        assert!(
            help.starts_with(
                "Convert the value to the type 'int64' where the ON CREATE SET or ON MATCH SET \
                 item writes it, with toInteger(), as in g.gql(\"MERGE (n:person) ON MATCH SET \
                 n.age = toInteger($v)\", #{v: \"29\"})."
            ),
            "{help}"
        );
        let help = failure("MERGE (p:person {name: 'marko'}) ON MATCH SET p.name = NULL");
        assert!(
            help.contains(
                "Keep it in the map, as in g.gql(\"MERGE (n:person) ON MATCH SET n = {name: \
                 n.name}\"), or merge with +=, which keeps the keys the map lacks, as in \
                 g.gql(\"MERGE (n:person) ON MATCH SET n += {name: 'value'}\")"
            ),
            "{help}"
        );
        // The suggested forms run on the same graph.
        let mut graph = graph_with_schema();
        for (query, value) in [
            ("MERGE (p:person {name: 'value'})", "x"),
            ("MERGE (q:person {name: 'ann', age: toInteger($v)})", "29"),
            ("MERGE (n:person) ON MATCH SET n.age = toInteger($v)", "29"),
            ("MERGE (n:person) ON MATCH SET n = {name: n.name}", "x"),
        ] {
            graph
                .traversal_mut()
                .gql_with(query, [("v", ElementProperty::from(value))])
                .to_list()
                .unwrap_or_else(|e| panic!("{query}: {}", format_diagnostic(&e)));
        }
        assert_eq!(counts(&graph), (3, 0));
        // An INSERT keeps the INSERT wording.
        assert!(failure("INSERT (:person {age: 3})").starts_with(
            "Give 'name' a value in the property map of the INSERT, as in \
             g.gql(\"INSERT (:person {name: 'value'})\")"
        ));
    }
}
