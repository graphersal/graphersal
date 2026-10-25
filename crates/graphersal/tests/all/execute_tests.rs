//! `profile()` as structured [`TraversalMetrics`], the step-id numbering it shares with error
//! locations, and the `execute()` terminal ([`Execution`]), in the Rust API and the Rhai DSL.
#![cfg(feature = "script")]

use std::sync::Arc;

use crate::script_host::{ScriptExecution, eval, eval_value, rhai};
use graphersal::exec::RenderOptions;
use graphersal::prelude::*;
use graphersal::profile::Metrics;

fn modern() -> Arc<Graph> {
    Arc::new(GraphSource::tinkerpop_modern())
}

/// Every `(id, name)` of a profile, depth first.
fn ids(metrics: &TraversalMetrics) -> Vec<(String, String)> {
    fn walk(list: &[Metrics], out: &mut Vec<(String, String)>) {
        for m in list {
            out.push((m.id().to_string(), m.name().to_string()));
            walk(m.nested(), out);
        }
    }
    let mut out = Vec::new();
    walk(metrics.metrics(), &mut out);
    out
}

/// The profile of a DSL query (without the trailing `.profile()`).
fn profile_of(query: &str) -> TraversalMetrics {
    eval_value(modern(), &format!("{query}.profile()"))
        .unwrap_or_else(|err| panic!("{query}: {err}"))
        .cast::<TraversalMetrics>()
}

/// The name of the step with `id` in the profile of `query`.
fn name_at(query: &str, id: &str) -> String {
    let metrics = profile_of(query);
    metrics
        .find(id)
        .unwrap_or_else(|| panic!("{query}: no step {id} in {:?}", ids(&metrics)))
        .name()
        .to_string()
}

/// The failing step id of `query`, run through `execute()`.
fn failing_id(query: &str) -> String {
    let exec = eval_value(modern(), &format!("{query}.execute()"))
        .unwrap()
        .cast::<ScriptExecution>();
    let mut exec = exec;
    let error = exec.error();
    let map = error
        .try_cast::<rhai::Map>()
        .unwrap_or_else(|| panic!("{query}: expected an error"));
    map.get("step_id")
        .unwrap_or_else(|| panic!("{query}: no step_id in {map:?}"))
        .clone()
        .into_string()
        .unwrap()
}

// ---------------------------------------------------------------------------------------------
// Step ids: one numbering for every child-holding step kind.

#[test]
fn top_level_ids_are_plan_positions_of_the_optimized_plan() {
    let metrics = profile_of(r#"g.V().hasLabel("person").out("knows").values("name")"#);
    let all = ids(&metrics);
    // `V().hasLabel()` is fused into one step: the ids follow the optimized plan.
    assert_eq!(
        all[0],
        ("0".to_string(), r#"v(labels: ["person"])"#.to_string())
    );
    assert_eq!(all[1].0, "1");
    assert_eq!(all[2].0, "2");
}

#[test]
fn union_branches_are_children_in_argument_order() {
    let q = r#"g.V("1").union(__.out("knows"), __.in("created").values("name"))"#;
    assert_eq!(name_at(q, "1.0.0"), r#"out("knows")"#);
    assert_eq!(name_at(q, "1.1.0"), r#"in("created")"#);
    assert_eq!(name_at(q, "1.1.1"), r#"values("name")"#);
}

#[test]
fn where_filter_not_local_optional_hold_one_child() {
    for (step, q) in [
        ("where", r#"g.V().where(__.out("knows"))"#),
        ("filter", r#"g.V().filter(__.out("knows"))"#),
        ("not", r#"g.V().not(__.out("knows"))"#),
        ("local", r#"g.V().local(__.out("knows"))"#),
        ("optional", r#"g.V().optional(__.out("knows"))"#),
        ("sideEffect", r#"g.V().sideEffect(__.out("knows"))"#),
    ] {
        assert_eq!(name_at(q, "1.0.0"), r#"out("knows")"#, "{step}");
    }
}

#[test]
fn and_or_coalesce_number_their_branches() {
    for q in [
        r#"g.V().and(__.out("knows"), __.values("age"))"#,
        r#"g.V().or(__.out("knows"), __.values("age"))"#,
        r#"g.V().coalesce(__.out("knows"), __.values("age"))"#,
    ] {
        assert_eq!(name_at(q, "1.0.0"), r#"out("knows")"#, "{q}");
    }
    // `or()` / `coalesce()` short-circuit: the second branch runs only for some traversers but
    // keeps its id.
    assert_eq!(
        name_at(r#"g.V().or(__.out("knows"), __.values("age"))"#, "1.1.0"),
        r#"values("age")"#
    );
    assert_eq!(
        name_at(
            r#"g.V().coalesce(__.out("knows"), __.values("age"))"#,
            "1.1.0"
        ),
        r#"values("age")"#
    );
}

#[test]
fn repeat_numbers_body_then_until_and_emit_in_attachment_order() {
    let emit_first =
        r#"g.V("1").repeat(__.out()).emit(__.hasLabel("person")).until(__.hasLabel("software"))"#;
    assert_eq!(name_at(emit_first, "1.0.0"), "out()");
    assert!(name_at(emit_first, "1.1.0").contains("person"));
    assert!(name_at(emit_first, "1.2.0").contains("software"));

    let until_first =
        r#"g.V("1").repeat(__.out()).until(__.hasLabel("software")).emit(__.hasLabel("person"))"#;
    assert!(name_at(until_first, "1.1.0").contains("software"));
    assert!(name_at(until_first, "1.2.0").contains("person"));

    // `times(n)` holds no traversal: the condition after it is child 1.
    let times = r#"g.V("1").repeat(__.out()).times(2).emit(__.hasLabel("person"))"#;
    assert!(name_at(times, "1.1.0").contains("person"));
}

#[test]
fn by_modulators_are_children_in_attachment_order() {
    let q = r#"g.V().hasLabel("person").order().by(__.values("age")).by(__.values("name"))"#;
    assert_eq!(name_at(q, "1.0.0"), r#"values("age")"#);
    assert_eq!(name_at(q, "1.1.0"), r#"values("name")"#);
    let q =
        r#"g.V().hasLabel("person").project("a", "b").by(__.values("age")).by(__.out().count())"#;
    assert_eq!(name_at(q, "1.0.0"), r#"values("age")"#);
    assert_eq!(name_at(q, "1.1.0"), "out()");
    let q = r#"g.V().group().by(__.label()).by(__.values("name").fold())"#;
    assert_eq!(name_at(q, "1.0.0"), "label()");
    assert_eq!(name_at(q, "1.1.0"), r#"values("name")"#);
}

#[test]
fn choose_numbers_selector_then_options() {
    // Predicate form: selector, then the true and the false branch.
    let q = r#"g.V().choose(__.hasLabel("person"), __.out("knows"), __.in("created"))"#;
    assert!(name_at(q, "1.0.0").contains("person"));
    assert_eq!(name_at(q, "1.1.0"), r#"out("knows")"#);
    assert_eq!(name_at(q, "1.2.0"), r#"in("created")"#);

    // Option form (`branch()` takes traversal keys too): selector, then per option its key traversal (if any) and its branch. The
    // branch of option 29 runs for marko only, the 99 branch never runs: no gap either way.
    let q = r#"g.V().hasLabel("person").branch(__.values("age")).option(29, __.out("knows")).option(99, __.in("created")).option(__.is(P.gt(30)), __.values("name"))"#;
    assert_eq!(name_at(q, "1.0.0"), r#"values("age")"#);
    assert_eq!(name_at(q, "1.1.0"), r#"out("knows")"#);
    assert!(
        profile_of(q).find("1.2.0").is_none(),
        "the 99 branch never runs"
    );
    assert!(name_at(q, "1.3.0").starts_with("is("));
    assert_eq!(name_at(q, "1.4.0"), r#"values("name")"#);
}

#[test]
fn select_traversal_numbers_the_key_before_the_by() {
    let q = r#"g.V("1").as("a").select(__.constant("a")).by(__.values("name"))"#;
    let metrics = profile_of(q);
    let all = ids(&metrics);
    let select = all
        .iter()
        .find(|(_, name)| name.starts_with("select("))
        .expect("select step")
        .0
        .clone();
    assert!(name_at(q, &format!("{select}.0.0")).starts_with("constant("));
    assert_eq!(name_at(q, &format!("{select}.1.0")), r#"values("name")"#);
}

#[test]
fn list_functions_number_their_traversal_argument() {
    for step in [
        "combine",
        "difference",
        "disjunct",
        "intersect",
        "merge",
        "product",
    ] {
        let q = format!(r#"g.V().values("name").fold().{step}(__.V().values("age").fold())"#);
        let metrics = profile_of(&q);
        let all = ids(&metrics);
        let id = all
            .iter()
            .find(|(_, name)| name.starts_with(&format!("{step}(")))
            .unwrap_or_else(|| panic!("{step}: {all:?}"))
            .0
            .clone();
        assert_eq!(name_at(&q, &format!("{id}.0.0")), "v()", "{step}");
        assert_eq!(
            name_at(&q, &format!("{id}.0.1")),
            r#"values("age")"#,
            "{step}"
        );
        assert_eq!(
            failing_id(&format!(r#"g.V().values("name").{step}([1])"#)),
            "2",
            "{step}"
        );
    }
}

#[test]
fn concat_and_format_number_their_children() {
    let q = r#"g.V().values("name").concat(__.V().values("name"), __.constant("x"))"#;
    assert_eq!(name_at(q, "2.0.0"), "v()");
    assert_eq!(name_at(q, "2.0.1"), r#"values("name")"#);
    assert!(name_at(q, "2.1.0").starts_with("constant("));
    let q = r#"g.V().format("%{_} %{_}").by(__.values("name")).by(__.outE().count())"#;
    assert_eq!(name_at(q, "1.0.0"), r#"values("name")"#);
    assert_eq!(name_at(q, "1.1.0"), "out_e()");
    assert_eq!(
        failing_id(r#"g.V().values("name").concat(__.values("age"))"#),
        "2.0.0"
    );
}

#[test]
fn map_flat_map_and_sample_number_their_children() {
    let q = r#"g.V().map(__.out().values("name"))"#;
    assert_eq!(name_at(q, "1.0.0"), "out()");
    assert_eq!(name_at(q, "1.0.1"), r#"values("name")"#);
    let q = r#"g.V().flatMap(__.out().out())"#;
    assert_eq!(name_at(q, "1.0.0"), "out()");
    let q = r#"g.E().sample(1).by(__.values("weight"))"#;
    assert_eq!(name_at(q, "1.0.0"), r#"values("weight")"#);
    assert_eq!(failing_id(r#"g.V().map(__.fail("stop"))"#), "1.0.0");
}

#[test]
fn wave_four_forms_number_their_children() {
    // group("a") side effect: key by() first, value by() second.
    let q = r#"g.V().group("a").by(__.label()).by(__.out().count()).cap("a")"#;
    assert_eq!(name_at(q, "1.0.0"), "label()");
    assert_eq!(name_at(q, "1.1.0"), "out()");
    // Infix and(): the operands become the children of one and().
    let q = r#"g.V().out("knows").and().out("created")"#;
    assert_eq!(name_at(q, "1.0.0"), r#"out("knows")"#);
    assert_eq!(name_at(q, "1.1.0"), r#"out("created")"#);
    // dedup("a", "b").by(..) keeps its by() child.
    let q = r#"g.V().as("a").out().as("b").dedup("a", "b").by(__.label())"#;
    assert!(
        ids(&profile_of(q))
            .iter()
            .any(|(_, name)| name == "label()")
    );
    // A bare by() is by(__.identity()).
    let q = r#"g.V().order().by()"#;
    assert_eq!(name_at(q, "1.0.0"), "identity()");
    assert_eq!(
        failing_id(r#"g.V().group("a").by(__.fail("stop"))"#),
        "1.0.0"
    );
}

#[test]
fn nested_children_extend_the_id() {
    let q = r#"g.V().where(__.out().where(__.in("created")))"#;
    assert_eq!(name_at(q, "1.0.0"), "out()");
    assert_eq!(name_at(q, "1.0.1.0.0"), r#"in("created")"#);
}

#[test]
fn path_recording_is_data_beside_the_plain_name() {
    let metrics = profile_of(r#"g.V("1").out().path()"#);
    let first = &metrics.metrics()[0];
    // The name is plain step text; what the step records is its own field.
    assert_eq!(first.name(), r#"v("1")"#);
    assert_eq!(first.path_recording(), Some("full"));
    assert!(metrics.metrics()[2].path_recording().is_none());
    let json = metrics.to_json();
    assert!(
        json.contains(r#""name":"v(\"1\")","path_recording":"full""#),
        "{json}"
    );
    // Only the table renders the suffix.
    assert!(metrics.to_string().contains(r#"v("1") [path: full]"#));
}

// ---------------------------------------------------------------------------------------------
// Error locations use the same numbering.

#[test]
fn error_location_joins_the_profile_by_id() {
    let q = r#"g.V().union(__.out(), __.values("name").sum())"#;
    assert_eq!(failing_id(q), "1.1.1");
    assert_eq!(failing_id(r#"g.V().values("name").sum()"#), "2");
    assert_eq!(
        failing_id(r#"g.V().where(__.out().where(__.values("name").sum()))"#),
        "1.0.1.0.1"
    );
    assert_eq!(
        failing_id(r#"g.V().order().by(__.values("name").sum())"#),
        "1.0.1"
    );

    // In the Rust API the location is a property of the error.
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let exec = lock
        .traversal()
        .v(None::<()>)
        .union(vec![__::out(None::<()>), __::values("name").sum()])
        .execute_with(ExecOptions::profile(ProfileType::none()));
    let location = exec.error().unwrap().step_location().unwrap();
    assert_eq!(location.step_id, "1.1.1");
    let path: Vec<&str> = location.step_path.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(path, ["1", "1.1.1"]);
    assert_eq!(location.step_path[1].name, "sum()");
    // The partial profile holds the failing step under the same id, with no completed call.
    let failed = exec.profile().unwrap().find("1.1.1").unwrap();
    assert_eq!(failed.calls(), 0);
    assert!(exec.results().is_empty());
}

#[test]
fn the_error_map_has_a_plain_message_and_the_rendered_diagnostic() {
    let mut exec = eval_value(modern(), r#"g.V().values("name").sum().execute()"#)
        .unwrap()
        .cast::<ScriptExecution>();
    let map = exec.error().cast::<rhai::Map>();
    let message = map["message"].clone().into_string().unwrap();
    let diagnostic = map["diagnostic"].clone().into_string().unwrap();
    assert_eq!(
        message,
        "Step #2 'sum()' execution failed: Cast exception: expected type [number], got type 'string'"
    );
    assert!(
        diagnostic.starts_with("Error: Step #2 'sum()' execution failed\n"),
        "{diagnostic}"
    );
    assert!(diagnostic.contains("Help: "), "{diagnostic}");
}

#[test]
fn an_error_before_any_step_has_no_location() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let exec = lock
        .traversal()
        .with("optimizer.disabled", vec!["no_such_rule"])
        .v(None::<()>)
        .execute();
    let err = exec
        .error()
        .expect("an invalid option is an execution error");
    assert!(matches!(err, TraverserError::InvalidOption { .. }), "{err}");
    assert!(err.step_location().is_none());
    assert!(exec.into_result().is_err());
}

// ---------------------------------------------------------------------------------------------
// Execution in the Rust API.

#[test]
fn execute_returns_what_to_list_returns() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let listed = lock
        .traversal()
        .v(None::<()>)
        .has_label("person")
        .values("name")
        .to_list()
        .unwrap();
    let exec = lock
        .traversal()
        .v(None::<()>)
        .has_label("person")
        .values("name")
        .execute();
    assert!(exec.is_ok());
    assert!(exec.profile().is_none());
    assert_eq!(exec.results(), listed.as_slice());
    assert_eq!(exec.into_result().unwrap(), listed);

    let profiled = lock
        .traversal()
        .v(None::<()>)
        .has_label("person")
        .values("name")
        .execute_with(ExecOptions::profile(ProfileType::none()));
    assert_eq!(profiled.results(), listed.as_slice());
    let metrics = profiled.profile().unwrap();
    assert_eq!(metrics.metrics()[1].counts().traverser_count(), 4);
    assert_eq!(metrics.metrics()[1].count_in(), 4);
}

#[test]
fn counts_map_to_tinkerpop_traverser_and_element_counts() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    // `out().out()` merges equal traversers (bulk): fewer objects than logical traversers.
    let metrics = lock
        .traversal()
        .v(None::<()>)
        .out(None::<()>)
        .out(None::<()>)
        .profile()
        .unwrap();
    let total_elements: u64 = metrics
        .metrics()
        .iter()
        .last()
        .map(|m| m.counts().element_count())
        .unwrap();
    assert_eq!(total_elements, 2);
    assert!(
        metrics
            .metrics()
            .iter()
            .all(|m| m.counts().element_count() >= m.counts().traverser_count())
    );
    let sum: f64 = metrics
        .metrics()
        .iter()
        .map(Metrics::percent_duration)
        .sum();
    assert!(sum <= 100.0 + 1e-9, "{sum}");
}

// ---------------------------------------------------------------------------------------------
// The extensibility rule: a profile type only adds an optional section.

#[cfg(feature = "serde")]
fn json_of(metrics: &TraversalMetrics) -> serde_json::Value {
    serde_json::from_str(&metrics.to_json()).unwrap()
}

/// Every key of `base` exists in `with` with a value of the same JSON type, recursively; `with`
/// may only add keys.
#[cfg(feature = "serde")]
fn assert_only_adds(base: &serde_json::Value, with: &serde_json::Value, path: &str) {
    use serde_json::Value;
    match (base, with) {
        (Value::Object(a), Value::Object(b)) => {
            for (key, value) in a {
                let other = b
                    .get(key)
                    .unwrap_or_else(|| panic!("{path}.{key} disappeared"));
                assert_only_adds(value, other, &format!("{path}.{key}"));
            }
        }
        (Value::Array(a), Value::Array(b)) => {
            assert_eq!(a.len(), b.len(), "{path}: length changed");
            for (index, (x, y)) in a.iter().zip(b).enumerate() {
                assert_only_adds(x, y, &format!("{path}[{index}]"));
            }
        }
        (Value::Number(_), Value::Number(_))
        | (Value::String(_), Value::String(_))
        | (Value::Bool(_), Value::Bool(_))
        | (Value::Null, Value::Null) => {}
        (a, b) => panic!("{path}: type changed from {a} to {b}"),
    }
}

#[cfg(feature = "serde")]
#[test]
fn a_profile_type_only_adds_an_optional_section() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let plain = lock
        .traversal()
        .v(None::<()>)
        .where_t(__::out(None::<()>))
        .group()
        .by("name")
        .profile()
        .unwrap();
    let memory = lock
        .traversal()
        .v(None::<()>)
        .where_t(__::out(None::<()>))
        .group()
        .by("name")
        .profile_with(ProfileType::Memory)
        .unwrap();

    // Without the type, no step has the section (not even as null).
    let plain_json = json_of(&plain);
    assert!(!plain.to_json().contains("\"memory\""));
    assert!(plain.metrics().iter().all(|m| m.memory().is_none()));
    // With it, every step has it, and nothing else changed shape.
    let memory_json = json_of(&memory);
    assert!(memory_json["metrics"][0]["memory"]["source"].is_string());
    assert!(memory_json["metrics"][1]["nested"][0]["memory"].is_object());
    assert_only_adds(&plain_json, &memory_json, "$");
    // Snake-case keys and integer nanoseconds.
    assert!(plain_json["duration_ns"].is_u64());
    for key in [
        "id",
        "name",
        "duration_ns",
        "percent_duration",
        "counts",
        "calls",
        "count_in",
        "nested",
    ] {
        assert!(plain_json["metrics"][0].get(key).is_some(), "missing {key}");
    }
    assert!(plain_json["metrics"][0]["counts"]["traverser_count"].is_u64());
    assert!(plain_json["metrics"][0]["counts"]["element_count"].is_u64());
    assert!(plain_json["optimizer_rules_applied"].is_array());
}

// ---------------------------------------------------------------------------------------------
// Rhai: TraversalMetrics and Execution values.

#[test]
fn rhai_profile_is_data_with_field_access() {
    let script = r#"
        let p = g.V().hasLabel("person").out("knows").profile();
        [p.duration_ns > 0, p.metrics.len(), p.metrics[0].id, p.metrics[0].name,
         p.metrics[1].counts.traverser_count, p.to_map().metrics[1].count_in,
         type_of(p.optimizer_rules_applied), p.to_json().starts_with("{\"duration_ns\":")]
    "#;
    let values = eval_value(modern(), script).unwrap().cast::<rhai::Array>();
    assert!(values[0].as_bool().unwrap());
    assert_eq!(values[1].as_int().unwrap(), 2);
    assert_eq!(values[2].clone().into_string().unwrap(), "0");
    assert_eq!(
        values[3].clone().into_string().unwrap(),
        r#"v(labels: ["person"])"#
    );
    assert_eq!(values[4].as_int().unwrap(), 2);
    assert_eq!(values[5].as_int().unwrap(), 4);
    assert_eq!(values[6].clone().into_string().unwrap(), "array");
    assert!(values[7].as_bool().unwrap());
}

#[test]
fn rhai_profile_without_memory_has_no_memory_key() {
    let script = r#"
        let plain = g.V().group().by("name").profile();
        let memory = g.V().group().by("name").profile(ProfileType.Memory);
        let memory2 = g.V().group().by("name").profile_with(ProfileType::Memory);
        [plain.metrics[1].contains("memory"), memory.metrics[1].contains("memory"),
         memory2.metrics[1].memory.source]
    "#;
    let values = eval_value(modern(), script).unwrap().cast::<rhai::Array>();
    assert!(!values[0].as_bool().unwrap());
    assert!(values[1].as_bool().unwrap());
    let source = values[2].clone().into_string().unwrap();
    assert!(source == "estimated" || source == "tracked", "{source}");
}

#[test]
fn rhai_profile_renders_as_the_table_and_fails_as_an_error() {
    let text = eval(modern(), r#"g.V().where(__.out()).profile()"#).unwrap();
    assert!(text.starts_with("Traversal Metrics"), "{text}");
    assert!(text.contains(r"\> out()"), "{text}");

    // A failure inside profile() is a script error, not a string result.
    let err = eval_value(modern(), r#"g.V().values("name").sum().profile()"#).unwrap_err();
    let message = err.to_string();
    assert!(
        message.contains("Step #2 'sum()' execution failed"),
        "{message}"
    );
    let caught = eval_value(
        modern(),
        r#"let r = "no"; try { g.V().values("name").sum().profile(); } catch (e) { r = "caught"; } r"#,
    )
    .unwrap()
    .into_string()
    .unwrap();
    assert_eq!(caught, "caught");
}

#[test]
fn rhai_execute_never_throws() {
    let script = r#"
        let ok = g.V().hasLabel("person").values("name").execute();
        let failed = g.V().union(__.out(), __.values("name").sum()).execute(#{profile: true});
        let bad_option = g.V().execute(#{profile: true, colour: "red"});
        let bad_type = g.V().execute(#{profile_types: ["network"]});
        [ok.results.len(), ok.error == (), ok.profile == (), ok.is_ok,
         failed.isOk, failed.results.len(), failed.error.step_id,
         failed.error.step_path.map(|s| s.id), failed.profile.metrics[1].nested[1].id,
         bad_option.error.message, bad_type.error.message, failed.error.contains("help")]
    "#;
    let values = eval_value(modern(), script).unwrap().cast::<rhai::Array>();
    assert_eq!(values[0].as_int().unwrap(), 4);
    assert!(values[1].as_bool().unwrap());
    assert!(values[2].as_bool().unwrap());
    assert!(values[3].as_bool().unwrap());
    assert!(!values[4].as_bool().unwrap());
    assert_eq!(values[5].as_int().unwrap(), 0);
    assert_eq!(values[6].clone().into_string().unwrap(), "1.1.1");
    let path: Vec<String> = values[7]
        .clone()
        .cast::<rhai::Array>()
        .into_iter()
        .map(|v| v.into_string().unwrap())
        .collect();
    assert_eq!(path, ["1", "1.1.1"]);
    assert_eq!(values[8].clone().into_string().unwrap(), "1.1.0");
    let bad_option = values[9].clone().into_string().unwrap();
    assert!(bad_option.contains("execute.colour"), "{bad_option}");
    let bad_type = values[10].clone().into_string().unwrap();
    assert!(bad_type.contains("execute.profile_types"), "{bad_type}");
    assert!(values[11].as_bool().unwrap());
}

#[test]
fn rhai_execute_with_memory_and_to_map_and_to_json() {
    let script = r#"
        let r = g.V().group().by("name").execute(#{profile_types: [ProfileType.Memory]});
        let m = r.to_map();
        [m.results.len(), m.error == (), m.profile.metrics[1].contains("memory"),
         r.to_json().starts_with("{\"results\":"), r.to_json() == r.to_json()]
    "#;
    let values = eval_value(modern(), script).unwrap().cast::<rhai::Array>();
    assert_eq!(values[0].as_int().unwrap(), 1);
    assert!(values[1].as_bool().unwrap());
    assert!(values[2].as_bool().unwrap());
    assert!(values[3].as_bool().unwrap());
    assert!(values[4].as_bool().unwrap());
}

#[test]
fn rhai_execute_mutates_like_to_list() {
    let graph = Arc::new(GraphSource::empty());
    let count = eval_value(
        graph,
        r#"g.addV("person").property("name", "ann").execute(); g.V().count().next()"#,
    )
    .unwrap()
    .as_int()
    .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn session_renders_results_then_the_profile_table() {
    let text = eval(
        modern(),
        r#"g.V().hasLabel("person").values("name").execute(#{profile: true})"#,
    )
    .unwrap();
    let table_at = text.find("marko").expect("results first");
    let profile_at = text.find("Traversal Metrics").expect("then the profile");
    assert!(table_at < profile_at, "{text}");

    let without = eval(
        modern(),
        r#"g.V().hasLabel("person").values("name").execute()"#,
    )
    .unwrap();
    assert!(without.contains("marko"));
    assert!(!without.contains("Traversal Metrics"));

    // A failed execution renders as its diagnostic (the host reports it as a failure).
    let value = eval_value(
        modern(),
        r#"g.V().union(__.out(), __.values("name").sum()).execute(#{profile: true})"#,
    )
    .unwrap();
    let err = crate::script_host::try_render_value(
        value,
        VisualizeFormat::Auto,
        &RenderOptions::unlimited(),
    )
    .unwrap_err();
    assert!(
        err.starts_with("Error: Step #1.1.1 'sum()' execution failed"),
        "{err}"
    );
    assert!(err.contains("Traversal Metrics"), "{err}");
}

/// `ScriptExecution::render_results`: the results alone (no profile table), in the requested
/// fallback format and within the display limits; a failed run is its diagnostic.
#[test]
fn render_results_shows_the_results_without_the_profile() {
    let exec = eval_value(
        modern(),
        r#"g.with("render.max_rows", 2).V().values("name").execute(#{profile: true})"#,
    )
    .unwrap()
    .cast::<ScriptExecution>();
    let rendered = exec
        .render_results(VisualizeFormat::Json, &RenderOptions::default())
        .expect("renders");
    assert!(rendered.text.contains("marko"), "{}", rendered.text);
    assert!(
        !rendered.text.contains("Traversal Metrics"),
        "{}",
        rendered.text
    );
    assert!(rendered.notice().is_some(), "two of six rows are shown");

    let failed = eval_value(modern(), r#"g.V().values("name").sum().execute()"#)
        .unwrap()
        .cast::<ScriptExecution>();
    let diagnostic = failed
        .render_results(VisualizeFormat::Json, &RenderOptions::default())
        .expect_err("a failed run has no results to render");
    assert!(diagnostic.contains("Error"), "{diagnostic}");
}

/// `ScriptGraphTraversalSource::is_mutating`: decided from the plan, child traversals included.
#[test]
fn is_mutating_reports_mutating_plans() {
    use crate::script_host::ScriptGraphTraversalSource;
    let plan = |script: &str| {
        eval_value(modern(), script)
            .unwrap()
            .cast::<ScriptGraphTraversalSource>()
    };
    assert!(!plan(r#"g.V().out("knows")"#).is_mutating());
    assert!(plan(r#"g.add_v("x")"#).is_mutating());
    assert!(plan(r#"g.V().union(__.values("name"), __.property("x", 1))"#).is_mutating());
}

/// `script_value_to_property`: a terminal's script value converts back to the same data.
#[test]
fn script_value_to_property_inverts_the_terminal_conversion() {
    use crate::script_host::script_value_to_property;
    let list = eval_value(
        modern(),
        r#"g.V("1").project("name", "uid").by("name").by(__.constant(UUID.fromString("550e8400-e29b-41d4-a716-446655440000"))).to_list()"#,
    )
    .unwrap();
    let Ok(ElementProperty::Array(items)) = script_value_to_property(list, 0) else {
        panic!("a list");
    };
    let ElementProperty::Object(map) = &items[0] else {
        panic!("a map: {items:?}");
    };
    assert_eq!(map.get("name"), Some(&ElementProperty::from("marko")));
    assert_eq!(
        map.get("uid"),
        Some(&ElementProperty::uuid_from_string("550e8400-e29b-41d4-a716-446655440000").unwrap())
    );
    let count = eval_value(modern(), "g.V().count().next()").unwrap();
    assert_eq!(
        script_value_to_property(count, 0).unwrap(),
        ElementProperty::from(6i64)
    );
}

/// Stage 3h: `gql:call_query` holds the called query's plan as its one child.
#[cfg(feature = "gql")]
#[test]
fn gql_call_query_holds_the_called_query_as_child_0() {
    let metrics = eval_value(
        modern(),
        r#"g.define_query(#{name: "people", language: "gql",
               body: "MATCH (p:person) RETURN p.name AS name"});
           g.gql("MATCH (s:software) CALL people() YIELD name RETURN name").profile()"#,
    )
    .unwrap()
    .cast::<TraversalMetrics>();
    let all = ids(&metrics);
    let call = all
        .iter()
        .position(|(_, name)| name.starts_with("gql:call_query(\"people\""))
        .unwrap_or_else(|| panic!("{all:?}"));
    let id = &all[call].0;
    assert_eq!(
        all[call + 1],
        (format!("{id}.0.0"), r#"v(labels: ["person"])"#.to_string()),
        "{all:?}"
    );
}
