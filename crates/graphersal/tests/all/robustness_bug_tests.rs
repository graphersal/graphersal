//! Minimized regressions for the findings of the robustness campaign
//! (`robustness_fuzz_tests`, `robustness_input_tests`). Each test states the behaviour rule 1
//! (arbitrary user queries must never crash the engine) and the diagnostics rules ask for; all
//! findings of the campaign are fixed (RB1-RB3). A new finding gets a test here marked
//! `#[ignore = "BUG: ..."]` until its fix (and, for a panic, a `KNOWN_PANICS` entry in
//! `robustness_fuzz_tests.rs`).
//!
//! A stack overflow aborts the whole process, so those cases run the script in a child process
//! (this test binary itself, filtered to the `child_runner` helper, with the script in an
//! environment variable of the child only) and check its exit status.
#![cfg(feature = "script")]

use std::sync::Arc;
use std::time::{Duration, Instant};

use graphersal::prelude::*;
use graphersal::script::rhai::Dynamic;

use crate::script_host::{ScriptLimits, eval_value, eval_value_atomic, eval_value_with_limits};

/// Environment variable carrying the script for [`child_runner`] (set on the child only).
const CHILD_SCRIPT: &str = "GRAPHERSAL_ROBUSTNESS_CHILD_SCRIPT";

/// Helper of [`survives_in_child`]: runs the script of `CHILD_SCRIPT` on the modern graph in a
/// thread with the 8 MiB stack of a typical main thread. Does nothing without the variable.
#[test]
#[ignore = "helper: run as a child process by survives_in_child()"]
fn child_runner() {
    let Ok(script) = std::env::var(CHILD_SCRIPT) else {
        return;
    };
    let worker = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let graph = Arc::new(GraphSource::tinkerpop_modern());
            let _ = eval_value(graph, &script);
        })
        .expect("spawn");
    // An error or even a panic is a clean exit here; only an abort fails the child.
    let _ = worker.join();
}

/// `true` when `script` runs in a child process without killing it (no stack-overflow abort).
fn survives_in_child(script: &str) -> bool {
    let exe = std::env::current_exe().expect("test binary path");
    let status = std::process::Command::new(exe)
        .args([
            "--exact",
            "robustness_bug_tests::child_runner",
            "--ignored",
            "--test-threads=1",
            "--quiet",
        ])
        .env(CHILD_SCRIPT, script)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("run the child");
    status.success()
}

/// The child mechanism itself works: a harmless script survives.
#[test]
fn child_process_harness_runs_a_harmless_script() {
    assert!(survives_in_child("g.v().count().next()"));
}

fn modern() -> Arc<Graph> {
    Arc::new(GraphSource::tinkerpop_modern())
}

fn run(script: &str) -> Result<Dynamic, String> {
    eval_value(modern(), script).map_err(|err| err.to_string())
}

// ---------------------------------------------------------------------------------------------
// Panics
// ---------------------------------------------------------------------------------------------

/// `lex_index` (`value/json_path.rs`) took `n.abs()` of the parsed index: `i64::MIN` overflowed
/// (a panic in debug builds; in release it wrapped and the out-of-range check passed). Fixed:
/// checked negation, an out-of-range index is a parse error with its position.
#[test]
fn jpath_index_i64_min_does_not_panic() {
    let parsed =
        std::panic::catch_unwind(|| "a[-9223372036854775808]".parse::<JsonPath>().is_err());
    assert!(
        matches!(parsed, Ok(true)),
        "must be a clean out-of-range error"
    );
    let script = std::panic::catch_unwind(|| {
        run(r#"g.v().values(jpath("a[-9223372036854775808]")).toList()"#)
    });
    assert!(matches!(script, Ok(Err(_))), "must be a clean script error");
}

// ---------------------------------------------------------------------------------------------
// Stack overflow (process abort)
// ---------------------------------------------------------------------------------------------

/// A script builds a 5 000-deep array with a plain loop (well inside the SAFE limits); handing
/// it to a step converted it recursively (`script/dynamic.rs` `dynamic_to_element_property`) and
/// overflowed the stack: the whole host process aborted. (Rhai itself aborts at about 20 000
/// levels even without a step, e.g. `x.len()` in the debug CLI.) Fixed: the conversion stops at
/// `ScriptLimits::max_value_depth` with a `script.max_value_depth` error.
///
/// Nested maps are tested at 3 000 levels: Rhai itself copies a map value recursively on every
/// read (`x = #{a: x}`) and, in a debug build on an 8 MiB stack, aborts between 3 500 and 4 200
/// levels without any graphersal code involved (measured); at 3 000 Rhai survives and the
/// unbounded conversion did not (measured with the limit raised to 1 000 000).
#[test]
fn deeply_nested_script_value_does_not_abort() {
    assert!(survives_in_child(
        "let x = 1; for i in 0..5000 { x = [x]; } g.inject(x).count().next()"
    ));
    assert!(survives_in_child(
        r#"let x = 1; for i in 0..3000 { x = #{a: x}; } g.addV("a").property("k", x).count().next()"#
    ));
}

/// A jpath key path with N segments creates N nested objects on write, so without a bound a
/// stored value could be arbitrarily deep, and every recursive consumer of it (the conversion
/// back to a script value, hashing in `dedup()`, schema inference) overflowed the stack. Fixed:
/// a path has at most 256 segments, and the depth a write leaves behind is bounded by
/// `traversal.max_value_depth`.
#[test]
fn deep_jpath_write_does_not_abort_later_readers() {
    let write = r#"let s = "a"; for i in 0..10000 { s += ".b"; } g.v(1).property(jpath(s), 1).count().next();"#;
    for read in [
        r#"g.v(1).values("a").toList()"#,
        r#"g.v(1).values("a").dedup().count().next()"#,
        r#"g.v(1).schema_step()"#,
    ] {
        assert!(survives_in_child(&format!("{write} {read}")), "{read}");
    }
}

// ---------------------------------------------------------------------------------------------
// Unbounded memory / time
// ---------------------------------------------------------------------------------------------

/// `replace()` multiplied string sizes with no cap: two 8 KiB strings made 64 MiB, beyond the
/// SAFE 16 MiB string limit (two 128 KiB strings would make 16 GiB). Fixed: the result length is
/// computed from the match count and checked against `traversal.max_string_bytes` (16 MiB by
/// default) before the string is built.
#[test]
fn replace_output_is_bounded_by_the_string_limit() {
    let result = eval_value_with_limits(
        modern(),
        r#"let s = "x"; for i in 0..13 { s += s; } g.inject(s).replace("x", s).length().next()"#,
        Default::default(),
        &ScriptLimits::default(),
    );
    let err = match result {
        Ok(value) => panic!("a 64 MiB string was built: {:?}", value.as_int().ok()),
        Err(err) => err.to_string(),
    };
    assert!(err.contains("traversal.max_string_bytes"), "{err}");
}

/// `fold()` expanded bulk into a list without checking the deadline: with a 20 ms
/// `evaluationTimeout` the query ran ~280 ms (debug) and allocated the whole list before the
/// next step boundary noticed. Fixed: the expansion counts every copy against the deadline.
#[test]
fn evaluation_timeout_interrupts_a_long_fold_expansion() {
    if crate::ci_support::skip_on_ci("timing") {
        return;
    }
    let started = Instant::now();
    let result = run(
        r#"g.with("evaluationTimeout", 20).v().repeat(__.both()).times(16).fold().count(Scope.local).next()"#,
    );
    let elapsed = started.elapsed();
    assert!(result.is_err(), "the timeout must fire");
    assert!(
        elapsed < Duration::from_millis(150),
        "ran {elapsed:?} past a 20 ms evaluationTimeout"
    );
}

/// The timeout of the fold() case above was reported at the NEXT step (`count(local)`, step #3),
/// not at `fold()` (step #2) where the time was spent. Fixed with the check inside the expansion.
#[test]
fn a_timeout_is_reported_at_the_step_that_spent_the_time() {
    if crate::ci_support::skip_on_ci("timing") {
        return;
    }
    let err = run(
        r#"g.with("evaluationTimeout", 20).v().repeat(__.both()).times(16).fold().count(Scope.local).next()"#,
    )
    .expect_err("the timeout must fire");
    assert!(err.contains("Step #2 'fold()'"), "{err}");
}

/// Under the default options a 60-character query materialized ~16 M values (3.2 GB RSS
/// measured at times(18), debug CLI): `bulk.max_expansion` defaulted to 100 M values, a count,
/// not a memory budget. Fixed (decision 2): the byte budget `traversal.max_materialized_bytes`
/// (1 GiB) and `bulk.max_expansion` lowered to 10 M values as a second line.
#[test]
fn default_options_bound_the_memory_of_a_bulk_expansion() {
    let err = run(r#"g.v().repeat(__.both()).times(17).fold().count(Scope.local).next()"#)
        .expect_err("~16 M values were materialized under the default options");
    assert!(err.contains("bulk.max_expansion"), "{err}");
    // A path value that doubles on every iteration hits the byte budget long before its depth
    // (iteration 40) or the loop's end.
    let err = run(r#"g.inject(1).repeat(__.path()).times(40).count().next()"#)
        .expect_err("the path doubles past 1 GiB");
    assert!(err.contains("traversal.max_materialized_bytes"), "{err}");
}

// ---------------------------------------------------------------------------------------------
// Diagnostics: locations and leaked internals
// ---------------------------------------------------------------------------------------------

/// Plan-time validation errors (modulator misuse, an undeclared `select` label, a malformed
/// `mergeV()` map) carried no `at #N` step location, unlike run-time step failures, and the
/// undeclared-label text named an internal pseudo element type (`'undeclared_label'`). Fixed:
/// the validation pass locates its error at the step that raised it (`StepFailed`), and an
/// undeclared label is `ValueError::UndeclaredLabel`. An invalid `execute()` options map has no
/// step: it names its option key (`execute.options`) instead.
#[test]
fn plan_time_errors_carry_a_step_location() {
    for script in [
        r#"g.v().to("x").toList()"#,
        r#"g.v().select("a").toList()"#,
        r#"g.inject(1).merge_v(1).toList()"#,
        r#"g.inject(1).where(__.by(1)).toList()"#,
        r#"g.inject(1).repeat(__.with_side_effect("x", 1)).toList()"#,
        r#"g.inject(1).math("_ +").toList()"#,
        r#"g.inject(1).product(1).toList()"#,
        r#"g.inject(1).local(__.with_sack(1)).toList()"#,
    ] {
        let err = run(script).expect_err(script);
        assert!(err.contains("at #"), "{script}: {err}");
        assert!(!err.contains("undeclared_label"), "{script}: {err}");
    }
    let err = run(r#"g.inject(1).execute(1.5).error"#)
        .map(|value| value.to_string())
        .unwrap_or_else(|err| err);
    assert!(err.contains("'execute.options'"), "{err}");
}

/// Step rendering in diagnostics used `Debug` for some arguments:
/// `Traversal(AnonymousTraversal { steps: [...] })`, `from(None)`, `Eq(String("x"))`,
/// `Eq(Boolean(true))`. Fixed by the canonical step rendering (wave 5) and the Rhai literals of
/// `value/literal.rs`.
#[test]
fn diagnostics_do_not_render_debug_text() {
    for script in [
        r#"g.withSack(1).inject(1).sack(Operator.div).by(__.constant(0)).sack().next()"#,
        r#"g.v().addE("x").to(__.v().drop()).toList()"#,
        r#"g.v().min().all("x").toList()"#,
        r#"g.inject(1).drop().where(true).toList()"#,
    ] {
        let err = run(script).expect_err(script);
        for marker in ["AnonymousTraversal", "(None)", "String(\"", "Boolean("] {
            assert!(!err.contains(marker), "{script}: {marker} in {err}");
        }
    }
}

/// An argument of the wrong type to a step that wants a traversal surfaced Rhai's raw type
/// error with a Rust type path (`Data type incorrect: graphersal::script::tokens::predicate::ArgP
/// (expecting ScriptGraphTraversalSource)`); `execute(1.5)` reported `got a f64` / `got a
/// graphersal::script::source::ScriptGraphTraversalSource`. Fixed: every type name in a
/// diagnostic is the display name of the DSL (`Predicate`, `GraphTraversalSource`, `float64`),
/// for Rhai's own errors through `map_eval_error`.
#[test]
fn wrong_argument_types_do_not_leak_rust_type_paths() {
    for script in [
        r#"g.inject(1).and(P.lte(1)).toList()"#,
        r#"g.inject(1).where_p(Cardinality.list).toList()"#,
        // `execute()` never throws: its options error is the execution's `error`.
        r#"g.inject(1).execute(1.5).error"#,
        r#"g.inject(1).execute(__.out()).error"#,
    ] {
        let err = match run(script) {
            Ok(value) => value.to_string(),
            Err(err) => err,
        };
        assert!(!err.is_empty(), "{script}");
        assert!(!err.contains("graphersal::"), "{script}: {err}");
        assert!(
            !err.contains("ScriptGraphTraversalSource"),
            "{script}: {err}"
        );
        assert!(!err.contains("f64"), "{script}: {err}");
    }
    let err =
        run(r#"g.inject(1).and(P.lte(1)).toList()"#).expect_err("a predicate is no traversal");
    assert!(err.contains("Predicate"), "{err}");
    let err = run(r#"g.inject(1).to(1, Merge.inV)"#).expect_err("no such overload");
    assert!(err.contains("to (int64, Merge)"), "{err}");
}

// ---------------------------------------------------------------------------------------------
// Unwind safety
// ---------------------------------------------------------------------------------------------

/// A panic inside an atomic script unwound past `eval_value_atomic` without rolling its unit
/// back: the guard put the private graph back, but the writes the script made before the panic
/// stayed, so "all or nothing" broke. Fixed: every unit rolls back on unwind
/// (`close_on_unwind`; the unit tests of `traversal/unwind_probe.rs` inject a real panic into
/// every unit kind). The jpath trigger used here is a clean error now, which rolls back too.
#[test]
fn a_panic_in_an_atomic_script_rolls_its_writes_back() {
    let graph = modern();
    let shared = graph.clone();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        eval_value_atomic(
            shared,
            r#"g.addV("ghost").next(); g.v().values(jpath("a[-9223372036854775808]")).toList()"#,
            Default::default(),
            &ScriptLimits::default(),
        )
    }));
    // Once the jpath panic is fixed this is a clean error; either way nothing may stay.
    assert!(!matches!(outcome, Ok(Ok(_))));
    assert_eq!(graph.read().vertex_count(), 6, "the ghost vertex survived");
}

/// A panic inside an explicit transaction closure rolls the whole transaction back (also the
/// traversals that already committed their savepoints) before it reaches the caller, and leaves
/// no unit open: the graph keeps working.
#[test]
fn a_panic_in_a_transaction_closure_rolls_it_back() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        graph.transaction(|g| -> TraverserResult<()> {
            g.traversal_mut().add_v("ghost").to_list()?;
            panic!("a bug in the caller's closure");
        })
    }));
    assert!(outcome.is_err(), "the panic continues to the caller");
    assert_eq!(graph.vertex_count(), 6, "the ghost vertex survived");
    graph.traversal_mut().add_v("kept").to_list().unwrap();
    assert_eq!(graph.vertex_count(), 7, "no unit is left open");
}

/// The same for a dry run: the graph stays as it was.
#[test]
fn a_panic_in_a_dry_run_closure_leaves_the_graph_unchanged() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        graph.dry_run(|g| -> TraverserResult<()> {
            g.traversal_mut().v(None).drop().to_list()?;
            panic!("a bug in the caller's closure");
        })
    }));
    assert!(outcome.is_err());
    assert_eq!(graph.vertex_count(), 6);
    assert_eq!(graph.edge_count(), 6);
    graph.traversal_mut().add_v("kept").to_list().unwrap();
    assert_eq!(graph.vertex_count(), 7);
}

// ---------------------------------------------------------------------------------------------
// Hangs
// ---------------------------------------------------------------------------------------------

/// The host's preset `evaluationTimeout` (`graph_scope_with_options`) was applied to `g` and
/// `_g` but not to the anonymous source `__`, so a script escaped it by starting a traversal from
/// `__`. Combined with a loop whose value doubles every iteration (`repeat(__.group())`:
/// `{v: [v]}`) and `repeat.max_loops` = 10 000, the query never returned (fuzzer hang, killed by
/// the 10 s watchdog). Fixed: the presets reach every source of the session, `__` and the
/// sources a script creates (`GraphSource::empty()`, with `engine_with_options`) included.
#[test]
fn host_timeout_applies_to_anonymous_source_traversals() {
    use crate::robustness_fuzz_tests::guarded;
    use crate::script_host::{engine_with_options, graph_scope_with_options};
    for source in ["__", "_g", "GraphSource::empty()"] {
        let result = guarded(Duration::from_secs(10), move || {
            let presets = [("evaluationTimeout", ElementProperty::from(500))];
            let engine = engine_with_options(&ScriptLimits::default(), &presets);
            let mut scope = graph_scope_with_options(modern(), &presets);
            engine
                .eval_with_scope::<Dynamic>(
                    &mut scope,
                    &format!("{source}.inject(1).repeat(__.group()).times(40).count().next()"),
                )
                .map(|value| value.to_string())
                .map_err(|err| err.to_string())
        });
        match result {
            Ok(Err(err)) => assert!(err.contains("evaluationTimeout"), "{source}: {err}"),
            Ok(Ok(value)) => panic!("{source}: finished with {value}; expected the host timeout"),
            Err(outcome) => {
                panic!("{source}: {outcome:?}: still running 10 s after a 500 ms host timeout")
            }
        }
    }
}
