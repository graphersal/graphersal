//! Resource limits: `ScriptLimits` on the Rhai engine and `traversal.max_traversers` on the
//! traversal, all reported as `ResourceLimitExceeded` diagnostics.
#![cfg(feature = "script")]

use crate::script_host::{
    ScriptLimits, engine_with_limits, eval_value, eval_value_with_limits, resource_limit_from_rhai,
};
use graphersal::exec::ExecutionPolicy;
use graphersal::prelude::*;
use rhai::Dynamic;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn modern() -> Arc<Graph> {
    Arc::new(GraphSource::tinkerpop_modern())
}

/// Runs `script` under `limits` and returns the error text; fails the test when it succeeds or
/// takes longer than ten seconds (a limit that does not stop the script quickly is no limit;
/// the time is not checked on CI runners, `crate::ci_support`).
fn limit_error(script: &str, limits: &ScriptLimits) -> String {
    let started = Instant::now();
    let result = eval_value_with_limits(modern(), script, HashMap::new(), limits);
    if !crate::ci_support::skip_on_ci("timing") {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "limit took too long for: {script}"
        );
    }
    match result {
        Ok(value) => panic!("expected a resource limit error for `{script}`, got {value:?}"),
        Err(err) => err.to_string(),
    }
}

#[test]
fn infinite_loop_stops_at_the_operation_limit() {
    let limits = ScriptLimits::default().with_max_operations(100_000);
    let text = limit_error("let n = 0; loop { n += 1; }", &limits);
    assert!(text.contains("script.max_operations"), "{text}");
    assert!(text.contains("limit 100000"), "{text}");
    assert!(text.contains("with_max_operations"), "{text}");
}

#[test]
fn infinite_loop_stops_under_the_default_limits() {
    let text = limit_error("loop { }", &ScriptLimits::default());
    assert!(text.contains("script.max_operations"), "{text}");
}

#[test]
fn string_doubling_stops_at_the_string_limit() {
    let limits = ScriptLimits::default().with_max_string_size(1 << 20);
    let text = limit_error(r#"let s = "x"; loop { s += s; }"#, &limits);
    assert!(text.contains("script.max_string_size"), "{text}");
}

#[test]
fn huge_array_stops_at_the_array_limit() {
    let limits = ScriptLimits::default().with_max_array_size(10_000);
    let text = limit_error("let a = []; loop { a.push(1); }", &limits);
    assert!(text.contains("script.max_array_size"), "{text}");
}

#[test]
fn huge_map_stops_at_the_map_limit() {
    let limits = ScriptLimits::default().with_max_map_size(1_000);
    // `m[k] = v` is not size-checked by Rhai itself (only the operation limit bounds it), method
    // calls such as `mixin` are.
    let text = limit_error(
        r#"let m = #{}; let i = 0; loop { let o = #{}; o[`k${i}`] = i; m.mixin(o); i += 1; }"#,
        &limits,
    );
    assert!(text.contains("script.max_map_size"), "{text}");
}

#[test]
fn deep_recursion_stops_at_the_call_level_limit() {
    let text = limit_error("fn f(n) { f(n + 1) } f(0)", &ScriptLimits::default());
    assert!(text.contains("script.max_call_levels"), "{text}");
    // Recursion within the limit still works.
    let ok = eval_value(
        modern(),
        "fn f(n) { if n == 0 { 0 } else { 1 + f(n - 1) } } f(20)",
    )
    .unwrap();
    assert_eq!(ok.as_int().unwrap(), 20);
}

#[test]
fn unlimited_lifts_every_counter_for_a_moderately_large_script() {
    let script = r#"
        let a = [];
        for i in 0..50000 { a.push(i); }
        let s = "x";
        for i in 0..17 { s += s; }
        a.len() + s.len()
    "#;
    // The same script fails under tight limits and passes unlimited.
    let tight = ScriptLimits::default()
        .with_max_operations(10_000)
        .with_max_array_size(100);
    assert!(eval_value_with_limits(modern(), script, HashMap::new(), &tight).is_err());
    let value =
        eval_value_with_limits(modern(), script, HashMap::new(), &ScriptLimits::unlimited())
            .unwrap();
    assert_eq!(value.as_int().unwrap(), 50_000 + (1 << 17));
}

#[test]
fn default_limits_leave_ordinary_queries_alone() {
    let value = eval_value(
        modern(),
        r#"g.v().has_label("person").values("name").to_list().len()"#,
    )
    .unwrap();
    assert_eq!(value.as_int().unwrap(), 4);
}

#[test]
fn a_host_engine_can_classify_the_rhai_error_itself() {
    let limits = ScriptLimits::default().with_max_operations(1_000);
    let engine = engine_with_limits(&limits);
    let err = engine.eval::<Dynamic>("loop { }").unwrap_err();
    match resource_limit_from_rhai(&err, &limits) {
        Some(TraverserError::ResourceLimitExceeded { limit, max, .. }) => {
            assert_eq!(limit, "script.max_operations");
            assert_eq!(max, Some(1_000));
        }
        other => panic!("expected ResourceLimitExceeded, got {other:?}"),
    }
    // An unrelated error is not a limit.
    let other = engine.eval::<Dynamic>("1 +").unwrap_err();
    assert!(resource_limit_from_rhai(&other, &limits).is_none());
}

#[test]
fn traversal_exceeding_the_traverser_cap_fails_with_the_option_in_the_message() {
    let err = eval_value(
        modern(),
        r#"g.with("traversal.max_traversers", 3).v().to_list()"#,
    )
    .unwrap_err();
    let text = err.to_string();
    assert!(text.contains("traversal.max_traversers"), "{text}");
    assert!(text.contains("limit 3, reached 6"), "{text}");
    assert!(
        text.contains(r#"g.with("traversal.max_traversers", 6).v().out().out().count()"#),
        "help carries a runnable example: {text}"
    );
    // At the cap it passes, and the option is also accepted in snake and camel spelling of `with`.
    let ok = eval_value(
        modern(),
        r#"g.with("traversal.max_traversers", 6).v().to_list().len()"#,
    )
    .unwrap();
    assert_eq!(ok.as_int().unwrap(), 6);
}

#[test]
fn traverser_cap_counts_what_a_step_expands_to() {
    let text = eval_value(
        modern(),
        r#"g.with("traversal.max_traversers", 5).v().out().to_list()"#,
    )
    .unwrap_err()
    .to_string();
    assert!(text.contains("traversal.max_traversers"), "{text}");
}

#[test]
fn traverser_cap_rejects_a_malformed_value() {
    let text = eval_value(
        modern(),
        r#"g.with("traversal.max_traversers", 0).v().count().next()"#,
    )
    .unwrap_err()
    .to_string();
    assert!(text.contains("traversal.max_traversers"), "{text}");
    assert!(text.contains("positive integer"), "{text}");
}

#[test]
fn execution_policy_sets_and_locks_the_traverser_cap() {
    let policy = ExecutionPolicy::permissive()
        .with_default("traversal.max_traversers", 3)
        .with_locked("traversal.max_traversers");
    let mut graph = TraversalGraph::new();
    for index in 0..5 {
        graph
            .add_vertex(
                Some(format!("v{index}").into()),
                vec!["node".into()],
                PropertyMap::default(),
            )
            .unwrap();
    }
    let graph = Arc::new(Graph::new(graph.with_execution_policy(policy)));

    // The policy default applies to a query that sets nothing.
    let text = eval_value(graph.clone(), "g.v().to_list()")
        .unwrap_err()
        .to_string();
    assert!(text.contains("limit 3, reached 5"), "{text}");
    // A locked key cannot be raised by the query.
    let text = eval_value(
        graph.clone(),
        r#"g.with("traversal.max_traversers", 100).v().to_list()"#,
    )
    .unwrap_err()
    .to_string();
    assert!(text.contains("locked"), "{text}");
    // Within the cap the query runs.
    let ok = eval_value(graph, "g.v().count().next()").unwrap();
    assert_eq!(ok.as_int().unwrap(), 5);
}

#[test]
fn a_limit_abort_rolls_back_the_writes_of_a_mutating_traversal() {
    // Implicit auto-commit: the abort rolls back what the traversal wrote (book: transactions.md).
    let graph = Arc::new(GraphSource::empty());
    let result = eval_value(
        graph.clone(),
        r#"g.with("traversal.max_traversers", 2).inject(1, 2).addV("n").inject(7, 8, 9).to_list()"#,
    );
    assert!(
        result.is_err(),
        "the inject after the mutation exceeds the cap"
    );
    let count = eval_value(graph, "g.v().count().next()").unwrap();
    assert_eq!(
        count.as_int().unwrap(),
        0,
        "the two vertices added before the abort are rolled back"
    );
}

// ---------------------------------------------------------------------------------------------
// Value depth: `ScriptLimits::max_value_depth` and `traversal.max_value_depth`
// ---------------------------------------------------------------------------------------------

/// A script that builds an array nested `depth` levels deep and injects it.
fn nested_inject(depth: usize) -> String {
    format!("let x = 1; for i in 0..{depth} {{ x = [x]; }} g.inject(x).count().next()")
}

/// An `ElementProperty` array nested `depth` levels deep.
fn nested_property(depth: usize) -> ElementProperty {
    let mut value = ElementProperty::from(1);
    for _ in 0..depth {
        value = ElementProperty::Array(vec![value]);
    }
    value
}

#[test]
fn a_script_value_deeper_than_the_default_fails_with_the_value_depth_limit() {
    let ok = eval_value(modern(), &nested_inject(128)).unwrap();
    assert_eq!(ok.as_int().unwrap(), 1);
    let text = eval_value(modern(), &nested_inject(129))
        .unwrap_err()
        .to_string();
    assert!(
        text.contains("Resource limit 'script.max_value_depth' exceeded (limit 128)"),
        "{text}"
    );
    assert!(text.contains("with_max_value_depth(256)"), "{text}");
    // Maps count like arrays, also inside property() and other value-taking steps.
    let graph = modern();
    let text = eval_value(
        graph.clone(),
        r#"let x = 1; for i in 0..200 { x = #{a: x}; } g.addV("a").property("k", x).next()"#,
    )
    .unwrap_err()
    .to_string();
    assert!(text.contains("script.max_value_depth"), "{text}");
    let count = eval_value(graph, "g.v().count().next()").unwrap();
    assert_eq!(count.as_int().unwrap(), 6);
}

#[test]
fn script_limits_raise_the_value_depth_and_unlimited_keeps_the_default() {
    let raised = ScriptLimits::default().with_max_value_depth(300);
    let ok = eval_value_with_limits(modern(), &nested_inject(250), HashMap::new(), &raised);
    assert_eq!(ok.unwrap().as_int().unwrap(), 1);
    let text = limit_error(&nested_inject(301), &raised);
    assert!(text.contains("limit 300"), "{text}");
    // `unlimited()` keeps the bound (it protects the native stack), and `0` reads as the default.
    for limits in [
        ScriptLimits::unlimited(),
        ScriptLimits::default().with_max_value_depth(0),
    ] {
        let text = limit_error(&nested_inject(129), &limits);
        assert!(text.contains("script.max_value_depth"), "{text}");
        assert!(text.contains("limit 128"), "{text}");
    }
}

#[test]
fn a_parameter_deeper_than_the_value_depth_limit_is_rejected() {
    use crate::script_host::{eval_value_with_params, eval_value_with_positional_params};
    let mut params = HashMap::new();
    params.insert("deep".to_string(), nested_property(129));
    let text = eval_value_with_params(modern(), "g.inject(deep).count().next()", params)
        .unwrap_err()
        .to_string();
    assert!(text.contains("script.max_value_depth"), "{text}");
    let text = eval_value_with_positional_params(
        modern(),
        "g.inject($1).count().next()",
        vec![nested_property(129)],
    )
    .unwrap_err()
    .to_string();
    assert!(text.contains("script.max_value_depth"), "{text}");
    let mut params = HashMap::new();
    params.insert("ok".to_string(), nested_property(128));
    let ok = eval_value_with_params(modern(), "g.inject(ok).count().next()", params);
    assert_eq!(ok.unwrap().as_int().unwrap(), 1);
}

#[cfg(feature = "serde")]
#[test]
fn property_json_is_bounded_by_the_value_depth_limit() {
    let text = eval_value(
        modern(),
        r#"let x = 1; for i in 0..200 { x = [x]; } g.v(1).property_json(#{deep: x}).next()"#,
    )
    .unwrap_err()
    .to_string();
    assert!(text.contains("script.max_value_depth"), "{text}");
}

/// The value arguments of the list functions, `merge(map)`, `concat(..)` and
/// `property(traversal, value)` are converted under the same limit as `inject()`.
#[test]
fn list_function_and_map_arguments_are_bounded_by_the_value_depth_limit() {
    let deep_list = "let x = 1; for i in 0..200 { x = [x]; }";
    let deep_map = "let x = 1; for i in 0..200 { x = #{a: x}; }";
    for query in [
        format!("{deep_list} g.inject([1]).combine([x]).next()"),
        format!("{deep_list} g.inject([1]).intersect(x).next()"),
        format!("{deep_list} g.inject([1]).product([x]).next()"),
        format!("{deep_map} g.inject(#{{b: 1}}).merge(x).next()"),
        format!("{deep_list} g.inject(\"a\").concat(\"b\", x).next()"),
        format!("{deep_map} g.v(1).property(__.constant(\"k\"), x).next()"),
    ] {
        let text = eval_value(modern(), &query).unwrap_err().to_string();
        assert!(text.contains("script.max_value_depth"), "{query}: {text}");
    }
    // Within the limit the same forms run.
    let ok = eval_value(
        modern(),
        "let x = 1; for i in 0..100 { x = [x]; } g.inject([1]).combine([x]).count(Scope.local).next()",
    )
    .unwrap();
    assert_eq!(ok.as_int().unwrap(), 2);
    let ok = eval_value(
        modern(),
        "let x = 1; for i in 0..100 { x = #{a: x}; } g.inject(#{b: 1}).merge(x).count(Scope.local).next()",
    )
    .unwrap();
    assert_eq!(ok.as_int().unwrap(), 2);
}

#[test]
fn a_loop_that_nests_its_value_stops_at_the_traversal_value_depth() {
    let text = eval_value(
        modern(),
        "g.inject(1).repeat(__.fold()).times(200).count().next()",
    )
    .unwrap_err()
    .to_string();
    assert!(
        text.contains("Resource limit 'traversal.max_value_depth' exceeded (limit 128)"),
        "{text}"
    );
    assert!(text.contains("repeat("), "{text}");
    // The option raises it for one query.
    let ok = eval_value(
        modern(),
        r#"g.with("traversal.max_value_depth", 300).inject(1).repeat(__.fold()).times(200).count().next()"#,
    )
    .unwrap();
    assert_eq!(ok.as_int().unwrap(), 1);
    let text = eval_value(
        modern(),
        r#"g.with("traversal.max_value_depth", 0).v().count().next()"#,
    )
    .unwrap_err()
    .to_string();
    assert!(text.contains("traversal.max_value_depth"), "{text}");
    assert!(text.contains("positive integer"), "{text}");
}

#[test]
fn a_jpath_write_is_bounded_by_segments_and_by_the_resulting_depth() {
    let path = |segments: usize| {
        format!(
            "let s = \"a\"; for i in 1..{segments} {{ s += \".b\"; }} \
             g.v(1).property(jpath(s), 1).count().next()"
        )
    };
    // 129 segments nest the value 128 levels below the stored property: the limit.
    let graph = modern();
    let written = eval_value(graph.clone(), &path(129)).unwrap();
    assert_eq!(written.as_int().unwrap(), 1);
    let read = eval_value(
        graph.clone(),
        r#"g.v(1).values("a").dedup().count().next()"#,
    );
    assert_eq!(read.unwrap().as_int().unwrap(), 1);
    let text = eval_value(graph.clone(), &path(130))
        .unwrap_err()
        .to_string();
    assert!(text.contains("traversal.max_value_depth"), "{text}");
    // A deep value written at a shallow path counts too.
    let text = eval_value(
        graph.clone(),
        r#"g.v(1).property(jpath("a.b"), __.inject(1).repeat(__.fold()).times(127)).count().next()"#,
    )
    .unwrap_err()
    .to_string();
    assert!(text.contains("traversal.max_value_depth"), "{text}");
    // Raised for one query, up to the parser's 256 segments.
    let raised = "let s = \"a\"; for i in 1..256 { s += \".b\"; } \
                  g.with(\"traversal.max_value_depth\", 300).v(2).property(jpath(s), 1).count().next()";
    let written = eval_value(graph.clone(), raised).unwrap();
    assert_eq!(written.as_int().unwrap(), 1);
    let text = eval_value(graph, &path(257)).unwrap_err().to_string();
    assert!(
        text.contains("too many segments: a path has at most 256"),
        "{text}"
    );
}

#[test]
fn execution_policy_sets_and_locks_the_value_depth() {
    let policy = ExecutionPolicy::permissive()
        .with_default("traversal.max_value_depth", 5)
        .with_locked("traversal.max_value_depth");
    let graph = Arc::new(Graph::new(
        TraversalGraph::new().with_execution_policy(policy),
    ));
    let text = eval_value(
        graph.clone(),
        "g.inject(1).repeat(__.fold()).times(10).count().next()",
    )
    .unwrap_err()
    .to_string();
    assert!(text.contains("traversal.max_value_depth"), "{text}");
    assert!(text.contains("limit 5"), "{text}");
    let text = eval_value(
        graph.clone(),
        r#"g.with("traversal.max_value_depth", 100).inject(1).count().next()"#,
    )
    .unwrap_err()
    .to_string();
    assert!(text.contains("locked"), "{text}");
    let ok = eval_value(
        graph,
        "g.inject(1).repeat(__.fold()).times(4).count().next()",
    );
    assert_eq!(ok.unwrap().as_int().unwrap(), 1);
}

#[test]
fn the_host_depth_bounds_nested_id_lists_and_materialized_results() {
    // A nested id list flattens up to the engine's value depth: the default rejects 200 levels,
    // a host that raised the depth accepts them.
    let ids = "let x = 1; for i in 0..200 { x = [x]; }";
    let text = eval_value(modern(), &format!("{ids} g.V(x).count().next()"))
        .unwrap_err()
        .to_string();
    assert!(text.contains("script.max_value_depth"), "{text}");
    let raised = ScriptLimits::default().with_max_value_depth(300);
    for step in ["V(x)", "E(x)", "V().hasId(x)"] {
        let ok = eval_value_with_limits(
            modern(),
            &format!("{ids} g.{step}.count().next()"),
            HashMap::new(),
            &raised,
        );
        assert_eq!(ok.unwrap().as_int().unwrap(), 1, "{step}");
    }
    // A whole-script unit materializes its result under the host's depth, not the default.
    let (value, _) = crate::script_host::eval_value_dry_run(
        modern(),
        "let x = 1; for i in 0..200 { x = [x]; } x",
        HashMap::new(),
        &raised,
    )
    .expect("200 levels are within a raised depth of 300");
    assert!(value.is_array());
    let value = eval_value(modern(), "let x = 1; for i in 0..200 { x = [x]; } x").unwrap();
    let text = graphersal::script::materialize_traversals(value.clone(), 0)
        .unwrap_err()
        .to_string();
    assert!(text.contains("script.max_value_depth"), "{text}");
    assert!(graphersal::script::materialize_traversals(value, 300).is_ok());
}

// ---------------------------------------------------------------------------------------------
// Memory: `traversal.max_materialized_bytes` and `bulk.max_expansion`
// ---------------------------------------------------------------------------------------------

/// The error text of `script` on the modern graph (the test fails when it succeeds).
fn query_error(script: &str) -> String {
    eval_value(modern(), script).unwrap_err().to_string()
}

#[test]
fn a_bulk_expansion_larger_than_the_byte_budget_fails_before_it_allocates() {
    // `repeat(__.both()).times(3)` merges into a few traversers with a bulk; `fold()` expands
    // them into 30 copies of 80 bytes each. The limit counts what the query holds at once (the
    // loop's buffers too), so it is set where the loop fits and the expansion does not.
    let text = query_error(
        r#"g.with("traversal.max_materialized_bytes", 4000).v().repeat(__.both()).times(3).fold().next()"#,
    );
    assert!(
        text.contains("Resource limit 'traversal.max_materialized_bytes' exceeded (limit 4000"),
        "{text}"
    );
    assert!(text.contains("Step #2 'fold()'"), "{text}");
    assert!(
        text.contains("g.with(\"traversal.max_materialized_bytes\", 8000)"),
        "{text}"
    );
    // A terminal list expands under the same budget.
    let text = query_error(
        r#"g.with("traversal.max_materialized_bytes", 4000).v().repeat(__.both()).times(3).toList()"#,
    );
    assert!(text.contains("traversal.max_materialized_bytes"), "{text}");
    // Raised, the same queries run.
    let ok = eval_value(
        modern(),
        r#"g.with("traversal.max_materialized_bytes", 1000000).v().repeat(__.both()).times(3).fold().count(Scope.local).next()"#,
    )
    .unwrap();
    let plain = eval_value(modern(), "g.v().repeat(__.both()).times(3).count().next()").unwrap();
    assert_eq!(ok.as_int().unwrap(), plain.as_int().unwrap());
}

#[test]
fn bucket_copies_of_a_merged_traverser_are_bounded_by_the_byte_budget() {
    for step in [
        r#"aggregate("x").cap("x")"#,
        r#"store("x").cap("x")"#,
        r#"group("x").by(T.label).cap("x")"#,
        "group().by(T.label)",
    ] {
        let text = query_error(&format!(
            r#"g.with("traversal.max_materialized_bytes", 100).v().repeat(__.both()).times(6).barrier().{step}.next()"#
        ));
        assert!(
            text.contains("traversal.max_materialized_bytes"),
            "{step}: {text}"
        );
    }
}

#[test]
fn a_loop_that_doubles_its_value_stops_at_the_byte_budget() {
    // Where each loop stops is deterministic: `path()` at its own boundary in the body (the
    // path records it keeps count at every check), `group()` at the loop's per-traverser check
    // of the new frontier (its doubled map is sized there).
    for (body, stops_at) in [
        ("__.path()", "Step #1.0.0 'path()' execution failed"),
        (
            "__.group()",
            "Step #1 'repeat(__.group()).times(60)' execution failed",
        ),
    ] {
        let text = query_error(&format!(
            r#"g.with("traversal.max_materialized_bytes", 100000).inject(1).repeat({body}).times(60).count().next()"#
        ));
        assert!(
            text.contains("traversal.max_materialized_bytes"),
            "{body}: {text}"
        );
        assert!(text.contains(stops_at), "{body}: {text}");
    }
    // A bounded loop within the budget runs.
    let ok = eval_value(
        modern(),
        "g.inject(1).repeat(__.path()).times(8).count().next()",
    )
    .unwrap();
    assert_eq!(ok.as_int().unwrap(), 1);
}

#[test]
fn the_byte_budget_rejects_a_malformed_value_and_a_policy_sets_and_locks_it() {
    for bad in ["0", "-1", r#""1 GiB""#, "1.5"] {
        let text = query_error(&format!(
            r#"g.with("traversal.max_materialized_bytes", {bad}).v().fold().next()"#
        ));
        assert!(
            text.contains("traversal.max_materialized_bytes"),
            "{bad}: {text}"
        );
        assert!(
            text.contains("positive integer number of bytes"),
            "{bad}: {text}"
        );
    }
    let policy = ExecutionPolicy::permissive()
        .with_default("traversal.max_materialized_bytes", 1000)
        .with_locked("traversal.max_materialized_bytes");
    let graph = Arc::new(Graph::new(
        TraversalGraph::tinkerpop_modern().with_execution_policy(policy),
    ));
    let text = eval_value(
        graph.clone(),
        "g.v().repeat(__.both()).times(3).fold().next()",
    )
    .unwrap_err()
    .to_string();
    assert!(text.contains("limit 1000"), "{text}");
    let text = eval_value(
        graph.clone(),
        r#"g.with("traversal.max_materialized_bytes", 100000000).v().fold().next()"#,
    )
    .unwrap_err()
    .to_string();
    assert!(text.contains("locked"), "{text}");
    let ok = eval_value(graph, "g.v().fold().count(Scope.local).next()").unwrap();
    assert_eq!(ok.as_int().unwrap(), 6);
}

#[test]
fn bulk_max_expansion_defaults_to_ten_million_values() {
    // 16 465 260 values at times(17): more than the default 10 M, so `fold()` refuses before it
    // allocates (1.3 GB of traversers).
    let text = query_error("g.v().repeat(__.both()).times(17).fold().next()");
    assert!(
        text.contains("would have to materialize 16465260 values, more than the limit of 10000000"),
        "{text}"
    );
}

// ---------------------------------------------------------------------------------------------
// Strings built by steps: `traversal.max_string_bytes`
// ---------------------------------------------------------------------------------------------

#[test]
fn every_string_building_step_is_bounded_by_the_string_limit() {
    let limited = r#"g.with("traversal.max_string_bytes", 6)"#;
    for query in [
        r#".inject("abc").replace("b", "bbbbbbb")"#,
        r#".inject("abc").concat("defg")"#,
        r#".inject("abc").concat(__.constant("defg"))"#,
        r#".inject(["abc", "defg"]).conjoin("-")"#,
        r#".inject("abc").format("%{_}%{_}%{_}")"#,
        ".inject(1234567).asString()",
        r#".inject("abcdefg").toUpper()"#,
        r#".inject("abcdefg").toLower()"#,
        r#".inject("abcdefg").substring(0)"#,
        r#".inject("abcdefg").reverse()"#,
        r#".inject(" abcdefg ").trim()"#,
        r#".inject(["abcdefg"]).toUpper(Scope.local)"#,
        r#".inject(["abc"]).replace(Scope.local, "b", "bbbbbbb")"#,
    ] {
        let text = query_error(&format!("{limited}{query}.next()"));
        assert!(
            text.contains("Resource limit 'traversal.max_string_bytes' exceeded (limit 6"),
            "{query}: {text}"
        );
    }
    // Within the limit (and under the default) the steps run.
    let ok = eval_value(
        modern(),
        r#"g.with("traversal.max_string_bytes", 7).inject("abc").replace("b", "bbbbb").next()"#,
    )
    .unwrap();
    assert_eq!(ok.into_string().unwrap(), "abbbbbc");
    let ok = eval_value(modern(), r#"g.inject("abc").concat("defg").next()"#).unwrap();
    assert_eq!(ok.into_string().unwrap(), "abcdefg");
}

#[test]
fn a_loop_that_doubles_a_string_stops_at_the_string_limit() {
    let text =
        query_error(r#"g.inject("x").repeat(__.concat(__.identity())).times(40).count().next()"#);
    assert!(
        text.contains("Resource limit 'traversal.max_string_bytes' exceeded (limit 16777216"),
        "{text}"
    );
    assert!(
        text.contains("g.with(\"traversal.max_string_bytes\", 33554432)"),
        "{text}"
    );
}

#[test]
fn the_string_limit_rejects_a_malformed_value_and_a_policy_sets_and_locks_it() {
    let text =
        query_error(r#"g.with("traversal.max_string_bytes", 0).inject("a").toUpper().next()"#);
    assert!(text.contains("positive integer number of bytes"), "{text}");
    let policy = ExecutionPolicy::permissive()
        .with_default("traversal.max_string_bytes", 3)
        .with_locked("traversal.max_string_bytes");
    let graph = Arc::new(Graph::new(
        TraversalGraph::tinkerpop_modern().with_execution_policy(policy),
    ));
    let text = eval_value(graph.clone(), r#"g.inject("ab").concat("cd").next()"#)
        .unwrap_err()
        .to_string();
    assert!(text.contains("limit 3"), "{text}");
    let text = eval_value(
        graph.clone(),
        r#"g.with("traversal.max_string_bytes", 100).inject("ab").concat("cd").next()"#,
    )
    .unwrap_err()
    .to_string();
    assert!(text.contains("locked"), "{text}");
    let ok = eval_value(graph, r#"g.inject("a").concat("b").next()"#).unwrap();
    assert_eq!(ok.into_string().unwrap(), "ab");
}

// ---------------------------------------------------------------------------------------------
// Host presets reach every source of a session
// ---------------------------------------------------------------------------------------------

#[test]
fn host_presets_reach_every_source_of_the_session() {
    use crate::script_host::{engine_with_options, graph_scope_with_options};
    let presets = [("traversal.max_string_bytes", ElementProperty::from(3))];
    let engine = engine_with_options(&ScriptLimits::default(), &presets);
    let mut scope = graph_scope_with_options(modern(), &presets);
    for source in [
        "g",
        "_g",
        "__",
        "GraphSource::empty()",
        "GraphSource::tinkerpop_modern()",
    ] {
        let err = engine
            .eval_with_scope::<Dynamic>(
                &mut scope,
                &format!(r#"{source}.inject("ab").concat("cd").next()"#),
            )
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("traversal.max_string_bytes"),
            "{source}: {err}"
        );
        assert!(err.contains("limit 3"), "{source}: {err}");
    }
    // A host preset is the host's: setting the same value again needs no Admin, and the query's
    // own `with()` replaces it (the scope here allows everything).
    let ok = engine
        .eval_with_scope::<Dynamic>(
            &mut scope,
            r#"__.with("traversal.max_string_bytes", 100).inject("ab").concat("cd").next()"#,
        )
        .unwrap();
    assert_eq!(ok.into_string().unwrap(), "abcd");
    // Without the engine presets, a source the script creates keeps the engine defaults.
    let engine = engine_with_limits(&ScriptLimits::default());
    let ok = engine
        .eval_with_scope::<Dynamic>(
            &mut scope,
            r#"GraphSource::empty().inject("ab").concat("cd").next()"#,
        )
        .unwrap();
    assert_eq!(ok.into_string().unwrap(), "abcd");
}
