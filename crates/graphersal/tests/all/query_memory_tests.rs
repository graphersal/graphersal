//! The per-execution limit `traversal.max_materialized_bytes` counts the LIVE bytes a query holds
//! at once (memory a step frees is given back), and the profile reports the peak of it
//! (`TraversalMetrics::memory`, `QueryMemory`). This binary installs no tracking allocator, so
//! the measure is the engine's estimate (its step buffers, the values it sizes, the `repeat()`
//! frontier and the path records); the tracked measure is covered by `tests/memory_ceiling.rs`.
#![cfg(feature = "script")]

use crate::script_host::{ScriptLimits, eval_value_with_limits};
use graphersal::exec::memory_limit_scope;
use graphersal::prelude::*;
use graphersal::profile::{MemorySource, TraversalMetrics};
use std::collections::HashMap;
use std::sync::Arc;

const LIMIT: &str = "traversal.max_materialized_bytes";

fn modern() -> Arc<Graph> {
    Arc::new(GraphSource::tinkerpop_modern())
}

fn run(graph: &Arc<Graph>, script: &str) -> Result<rhai::Dynamic, String> {
    eval_value_with_limits(
        graph.clone(),
        script,
        HashMap::new(),
        &ScriptLimits::unlimited(),
    )
    .map_err(|err| err.to_string())
}

/// A script that injects the numbers `0..1000` as a stream, then runs `rest` on it.
fn numbers(rest: &str) -> String {
    format!("let xs = []; for i in 0..1000 {{ xs.push(i); }} g.inject(xs).unfold(){rest}")
}

/// The same with the per-execution limit set to `limit`.
fn numbers_limited(limit: u64, rest: &str) -> String {
    format!(
        "let xs = []; for i in 0..1000 {{ xs.push(i); }} \
         g.with(\"{LIMIT}\", {limit}).inject(xs).unfold(){rest}"
    )
}

/// The peak the profile reports for `script` (a traversal ending in `.profile()`).
fn peak_of(graph: &Arc<Graph>, script: &str) -> u64 {
    let metrics = run(graph, script).unwrap();
    let map = metrics.cast::<rhai::Map>();
    let memory = map["query_memory"].clone().cast::<rhai::Map>();
    memory["peak_bytes"].as_int().unwrap() as u64
}

/// `fold().unfold()` builds a list of 1000 values and drops it again: twenty of them in a row
/// hold no more at once than one, so a limit a SUM of the lists would exceed many times over
/// lets the chain run.
#[test]
fn freed_memory_is_given_back() {
    let graph = modern();
    let one = peak_of(
        &graph,
        &numbers(".fold().unfold().count().profile().to_map()"),
    );
    let twenty = peak_of(
        &graph,
        &numbers(&format!(
            "{}.count().profile().to_map()",
            ".fold().unfold()".repeat(20)
        )),
    );
    assert_eq!(one, twenty, "the chain holds one list at a time");
    let list = 1000 * 32; // at least 32 bytes per value slot
    let limit = one + one / 4;
    assert!(
        20 * list > limit,
        "a sum of the lists would be over the limit"
    );
    let count = run(
        &graph,
        &numbers_limited(
            limit,
            &format!("{}.count().next()", ".fold().unfold()".repeat(20)),
        ),
    )
    .unwrap();
    assert_eq!(count.as_int().unwrap(), 1000);
}

/// Two buckets held at the same time (the 1000 copies of a merged traverser in two side
/// effects, kept to the end of the query) exceed a limit that one fits in.
#[test]
fn what_is_held_at_once_is_limited() {
    let graph = modern();
    let single = peak_of(
        &graph,
        &numbers(
            r#".constant("xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx").barrier().aggregate("a").cap("a").count(Scope.local).profile().to_map()"#,
        ),
    );
    let double = peak_of(
        &graph,
        &numbers(
            r#".constant("xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx").barrier().aggregate("a").aggregate("b").cap("a", "b").count(Scope.local).profile().to_map()"#,
        ),
    );
    assert!(double > single + 1000 * 200, "{double} vs {single}");
    let limit = single + (double - single) / 2;
    let alone = run(
        &graph,
        &numbers_limited(
            limit,
            r#".constant("xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx").barrier().aggregate("a").cap("a").count(Scope.local).next()"#,
        ),
    )
    .unwrap();
    assert_eq!(alone.as_int().unwrap(), 1000);
    let err = run(
        &graph,
        &numbers_limited(
            limit,
            r#".constant("xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx").barrier().aggregate("a").aggregate("b").cap("a", "b").count(Scope.local).next()"#,
        ),
    )
    .unwrap_err();
    assert!(
        err.contains(&format!("Resource limit '{LIMIT}' exceeded (limit {limit}")),
        "{err}"
    );
}

/// A `repeat()` whose body multiplies its traversers stops inside the iteration that crosses
/// the limit, not after it built the whole next frontier (or, as before, at the traverser
/// count limit once gigabytes were held).
#[test]
fn the_repeat_frontier_is_counted_per_iteration() {
    let graph = modern();
    let limit: u64 = 4 << 20;
    let err = run(
        &graph,
        &format!(
            r#"g.with("bulk.merge", false).with("{LIMIT}", {limit}).inject(1).repeat(__.constant([1, 2, 3, 4]).unfold()).times(20).count().next()"#
        ),
    )
    .unwrap_err();
    assert!(
        err.contains(&format!(
            "Resource limit '{LIMIT}' exceeded (limit {limit}, reached "
        )),
        "{err}"
    );
    assert!(err.contains("Step #1"), "inside the loop: {err}");
    let reached: u64 = err
        .split("reached ")
        .nth(1)
        .and_then(|rest| rest.split(')').next())
        .and_then(|n| n.parse().ok())
        .unwrap();
    assert!(
        reached < limit * 2,
        "stopped within the crossing iteration: {reached}"
    );
}

/// A scheduled job's memory limit is its traversals' per-execution limit, with the same
/// measure: a chain that builds and frees lists runs, two buckets held at once do not.
#[test]
fn a_job_memory_limit_is_cumulative() {
    use graphersal::jobs::{Job, JobKind, JobLimits, Scheduler};
    let graph = modern();
    let one = peak_of(
        &graph,
        &numbers(".fold().unfold().count().profile().to_map()"),
    );
    let job_error = |bytes: u64, script: String| {
        let target = graph.clone();
        let mut scheduler = Scheduler::new();
        scheduler
            .add(
                Job::new(
                    "bounded",
                    JobKind::User,
                    "* * * * *".parse().unwrap(),
                    move |_run| run(&target, &script).map(|_| ()),
                )
                .with_limits(JobLimits::new().with_memory_limit(bytes)),
            )
            .unwrap();
        scheduler.run_now("bounded", 1_800_000_000_000_000).unwrap();
        scheduler.jobs()[0].last.as_ref().unwrap().error.clone()
    };
    let limit = one + one / 4;
    assert_eq!(
        job_error(
            limit,
            numbers(&format!("{}.count().next()", ".fold().unfold()".repeat(20)))
        ),
        None
    );
    let error = job_error(
        limit,
        numbers(r#".constant("xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx").barrier().aggregate("a").aggregate("b").cap("a", "b").count(Scope.local).next()"#),
    )
    .expect("two buckets at once are over the job's limit");
    assert!(error.contains(LIMIT), "{error}");
}

/// Every profile carries the run's peak against the per-execution limit (Rust getter, map and
/// JSON key); the text table shows it as one line only with `ProfileType::Memory`.
#[test]
fn profile_reports_the_peak_against_the_per_query_limit() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let plain = lock
        .traversal()
        .with(LIMIT, 1_000_000)
        .v(None)
        .both(None)
        .fold()
        .profile()
        .unwrap();
    let memory = plain.memory().expect("every profile has the run's memory");
    assert_eq!(memory.limit_bytes(), 1_000_000);
    assert_eq!(memory.source(), MemorySource::Estimated);
    // At least the twelve traversers of `both()` (80 bytes each) were held at once.
    assert!(memory.peak_bytes() >= 12 * 80, "{memory:?}");
    assert!(memory.peak_bytes() <= memory.limit_bytes());
    let json = plain.to_json();
    assert!(
        json.contains(&format!(
            "\"query_memory\":{{\"peak_bytes\":{},\"limit_bytes\":1000000,\"limit\":\"{LIMIT}\",\"source\":\"estimated\"}}",
            memory.peak_bytes()
        )),
        "{json}"
    );
    assert!(!plain.to_string().contains("Peak memory"), "{plain}");
    let with_memory = lock
        .traversal()
        .with(LIMIT, 1_000_000)
        .v(None)
        .both(None)
        .fold()
        .profile_with(ProfileType::Memory)
        .unwrap();
    let text = with_memory.to_string();
    assert!(
        text.contains(&format!(
            "Peak memory: ~{} of 976.6 KiB ({LIMIT}, estimated)",
            binary(with_memory.memory().unwrap().peak_bytes())
        )),
        "{text}"
    );
}

/// The text table's binary units (`512 B`, `1.2 KiB`).
fn binary(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let text = format!("{:.1}", bytes as f64 / 1024.0);
    format!("{} KiB", text.strip_suffix(".0").unwrap_or(&text))
}

/// The estimated process ceiling counts what a run holds (values and buffers), not only its
/// traversers: a loop whose traversers each carry a list of 100 numbers is stopped by a ceiling
/// the bare traverser count stays far below.
#[test]
fn the_estimated_process_check_counts_values_and_buffers() {
    let graph = modern();
    let footprint = graph.read().memory_usage().unwrap().unwrap().total_bytes();
    let script = "let xs = []; for i in 0..100 { xs.push(i); } \
                  g.with(\"bulk.merge\", false).inject(1).repeat(__.constant(xs).unfold().constant(xs)).times(2).count().next()";
    // 10 000 traversers of 80 bytes: 800 KB; their lists: about 50 MB.
    let err = memory_limit_scope(footprint + 8_000_000, || run(&graph, script)).unwrap_err();
    assert!(
        err.contains("Resource limit 'memory.limit' exceeded"),
        "{err}"
    );
    // Without the lowered ceiling the loop runs.
    assert_eq!(run(&graph, script).unwrap().as_int().unwrap(), 10_000);
}

/// An execution that failed on the limit carries the partial profile with the peak at the
/// failure: the figure of the failing check.
#[test]
fn a_failed_execution_reports_the_peak_at_failure() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let execution = lock
        .traversal()
        .with(LIMIT, 1_000)
        .v(None)
        .both(None)
        .both(None)
        .fold()
        .execute_with(ExecOptions::profile(ProfileType::none()));
    let observed = match execution.error().map(root) {
        Some(TraverserError::ResourceLimitExceeded {
            limit,
            max,
            observed,
            ..
        }) if limit == LIMIT => {
            assert_eq!(*max, Some(1_000));
            observed.expect("the figure")
        }
        other => panic!("expected the per-execution limit, got {other:?}"),
    };
    let memory = execution
        .profile()
        .and_then(TraversalMetrics::memory)
        .expect("the partial profile has the run's memory");
    assert!(observed > 1_000);
    assert_eq!(memory.peak_bytes(), observed);
    assert_eq!(memory.limit_bytes(), 1_000);
}

/// The error under the step frames of `err`.
fn root(err: &TraverserError) -> &TraverserError {
    let mut current = err;
    while let TraverserError::StepFailed { source, .. } = current {
        current = source;
    }
    current
}
