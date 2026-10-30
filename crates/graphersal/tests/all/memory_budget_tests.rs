//! The process memory ceiling (`memory.limit`, `memory.headroom`): ON by default; a ceiling
//! fails an oversized traversal with `ResourceLimitExceeded` and rolls back what it wrote, and the
//! process (and the graph) carry on. This binary installs no tracking allocator, so the ceiling
//! runs on the engine's estimate (the graph footprint at the start of the run plus what the run
//! holds and adds).
//!
//! The ceiling is ONE process-wide setting, and the tests of this binary run in parallel: a test
//! here never sets it (`graphersal::exec::set_memory_limit`; that is the separate target
//! `tests/memory_ceiling.rs`). It lowers the ceiling for its own thread with the test-only
//! `graphersal::exec::memory_limit_scope` instead.
#![cfg(feature = "script")]

use crate::script_host::{ScriptLimits, eval_value_with_limits};
use graphersal::exec::{ExecutionPolicy, memory_limit_scope};
use graphersal::prelude::*;
use std::collections::HashMap;
use std::sync::Arc;

fn modern() -> Arc<Graph> {
    Arc::new(GraphSource::tinkerpop_modern())
}

/// The estimated footprint of `graph` (what the budget starts from without a tracking
/// allocator).
fn footprint(graph: &Graph) -> u64 {
    graph.read().memory_usage().unwrap().unwrap().total_bytes()
}

/// The error under the step frames of `err`.
fn root(err: &TraverserError) -> &TraverserError {
    let mut current = err;
    while let TraverserError::StepFailed { source, .. } = current {
        current = source;
    }
    current
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

/// A query that holds ~1.7 thousand traversers in a fold (30 x 30 x 2 paths on Modern).
const BIG_FOLD: &str = "v().both().both().both().fold().count(Scope.local).next()";

#[test]
fn the_default_ceiling_lets_ordinary_queries_run() {
    let graph = modern();
    let count = run(&graph, &format!("g.{BIG_FOLD}")).unwrap();
    assert!(count.as_int().unwrap() > 0);
}

#[test]
fn a_lower_ceiling_fails_the_query_and_the_process_carries_on() {
    let graph = modern();
    let budget = footprint(&graph) + 2_000;
    let err = memory_limit_scope(budget, || run(&graph, &format!("g.{BIG_FOLD}"))).unwrap_err();
    assert!(
        err.contains("Resource limit 'memory.limit' exceeded"),
        "{err}"
    );
    assert!(err.contains(&format!("limit {budget}")), "{err}");
    assert!(err.contains("--memory-limit"), "{err}");
    assert!(err.contains("The figure is estimated"), "{err}");
    // The same graph runs a small query under the same ceiling, and a large one without it.
    let small = memory_limit_scope(budget, || run(&graph, "g.v().count().next()")).unwrap();
    assert_eq!(small.as_int().unwrap(), 6);
    assert!(run(&graph, &format!("g.{BIG_FOLD}")).is_ok());
}

#[test]
fn the_error_is_structured_and_located() {
    let graph = modern();
    let budget = footprint(&graph) + 2_000;
    let lock = graph.read();
    let err = memory_limit_scope(budget, || {
        lock.traversal()
            .v(None)
            .both(None)
            .both(None)
            .both(None)
            .fold()
            .next()
    })
    .unwrap_err();
    let location = err
        .step_location()
        .expect("a step-boundary failure is located");
    assert!(!location.step_id.is_empty());
    match root(&err) {
        TraverserError::ResourceLimitExceeded {
            limit,
            max,
            observed,
            ..
        } => {
            assert_eq!(limit, "memory.limit");
            assert_eq!(*max, Some(budget));
            assert!(observed.unwrap() > budget);
        }
        other => panic!("expected ResourceLimitExceeded, got {other:?}"),
    }
}

#[test]
fn writes_that_grow_the_graph_past_the_budget_roll_back() {
    let graph = modern();
    let budget = footprint(&graph) + 4_000;
    // Each new vertex counts at the graph's average element size; 40 of them exceed 4000 bytes.
    let err = memory_limit_scope(budget, || {
        run(
            &graph,
            &format!(
                "g.inject({}).unfold().addV(\"tmp\").toList()",
                format_args!("{:?}", (0..40).collect::<Vec<_>>())
            ),
        )
    })
    .unwrap_err();
    assert!(err.contains("memory.limit"), "{err}");
    assert_eq!(
        graph.read().vertex_count(),
        6,
        "the failed write was rolled back"
    );
    // Under the budget, a small write commits. (The rollback keeps the capacity the failed write
    // grew, which the footprint counts, so the budget is taken again.)
    let budget = footprint(&graph) + 4_000;
    let added =
        memory_limit_scope(budget, || run(&graph, "g.addV(\"tmp\").count().next()")).unwrap();
    assert_eq!(added.as_int().unwrap(), 1);
    assert_eq!(graph.read().vertex_count(), 7);
}

#[test]
fn the_ceiling_is_not_an_execution_option() {
    let graph = modern();
    // A query cannot set it: it is a setting of the whole process.
    for script in [
        "g.with(\"memory.limit\", 1000).v().count().next()",
        "g.with(\"memory.limit\", \"auto\").v().count().next()",
        "g.with(\"memory.headroom\", 0).v().count().next()",
    ] {
        let err = run(&graph, script).unwrap_err();
        assert!(err.contains("Invalid value for option 'memory."), "{err}");
        assert!(err.contains("setting of the whole process"), "{err}");
    }
    // Nor can a host's execution policy (a lock alone is harmless).
    let policy = ExecutionPolicy::permissive().with_default("memory.limit", 1_000);
    let graph = TraversalGraph::tinkerpop_modern().with_execution_policy(policy);
    let err = graph.traversal().v(None).count().next().unwrap_err();
    assert!(
        matches!(&*err, TraverserError::InvalidOption { key, .. } if key == "memory.limit"),
        "{err:?}"
    );
    let help = err.help().unwrap();
    assert!(help.contains("--memory-limit 4294967296"), "{help}");
    assert!(
        help.contains("graphersal::exec::set_memory_limit(MemoryLimit::Bytes(4294967296))"),
        "{help}"
    );
    let policy = ExecutionPolicy::permissive().with_locked("memory.limit");
    let graph = TraversalGraph::tinkerpop_modern().with_execution_policy(policy);
    assert_eq!(
        graph.traversal().v(None).count().next().unwrap(),
        Some(6.into())
    );
}

#[test]
fn the_ceiling_in_effect_has_a_source() {
    use graphersal::exec::{MemoryLimit, MemoryLimitSource};
    let limit = graphersal::exec::memory_limit().unwrap();
    // Nothing in this binary sets the process ceiling: the default applies.
    assert_eq!(limit.setting(), MemoryLimit::Auto);
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    {
        assert!(
            matches!(
                limit.source(),
                MemoryLimitSource::Cgroup | MemoryLimitSource::PhysicalMemory
            ),
            "{limit:?}"
        );
        assert!(limit.bytes().unwrap() > 0);
    }
    assert!(!limit.source().as_str().is_empty());
}

#[test]
fn a_graph_over_the_ceiling_fails_every_traversal_at_its_first_step() {
    let graph = TraversalGraph::tinkerpop_modern();
    // The graph alone is over 1000 bytes.
    let err = memory_limit_scope(1_000, || graph.traversal().v(None).count().next()).unwrap_err();
    assert!(
        matches!(root(&err), TraverserError::ResourceLimitExceeded { limit, .. } if limit == "memory.limit"),
        "{err:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// Units that are not traversals (the same ceiling): GraphML/GraphSON imports into a live graph and `apply_change_set` are checked every
// 4096 elements and at their end, and every outermost unit at its commit (a `transaction`
// closure). Over the budget the whole unit rolls back with `GraphError::ResourceLimitExceeded`
// naming the operation.
// ---------------------------------------------------------------------------------------------

/// How many elements a non-traversal unit handles between two budget checks (the engine's
/// `UNIT_CHECK_INTERVAL`).
const UNIT_CHECK_INTERVAL: u64 = 4096;

/// Modern and a ceiling that leaves `room` bytes above the graph's footprint (the test runs
/// under it with [`memory_limit_scope`]).
fn modern_with_room(room: u64) -> (TraversalGraph, u64) {
    let graph = TraversalGraph::tinkerpop_modern();
    let limit = graph.memory_usage().unwrap().unwrap().total_bytes() + room;
    (graph, limit)
}

/// The estimated bytes of one added element on `graph` (the budget's average element size).
fn element_bytes(graph: &TraversalGraph) -> u64 {
    let base = graph.memory_usage().unwrap().unwrap().total_bytes();
    (base / (graph.vertex_count() + graph.edge_count()) as u64).max(128)
}

/// GraphML text with `n` vertices labelled `tmp`, ids `<prefix><i>`.
#[cfg(feature = "io")]
fn graphml_vertices(prefix: &str, n: usize) -> String {
    let mut text = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <graphml xmlns=\"http://graphml.graphdrawing.org/xmlns\">\n\
         <key id=\"labelV\" for=\"node\" attr.name=\"labelV\" attr.type=\"string\"/>\n\
         <graph id=\"G\" edgedefault=\"directed\">\n",
    );
    for i in 0..n {
        text.push_str(&format!(
            "<node id=\"{prefix}{i}\"><data key=\"labelV\">tmp</data></node>\n"
        ));
    }
    text.push_str("</graph>\n</graphml>\n");
    text
}

/// GraphSON (line form) with `n` vertices labelled `tmp`, ids `<prefix><i>`.
#[cfg(feature = "io")]
fn graphson_vertices(prefix: &str, n: usize) -> String {
    (0..n)
        .map(|i| format!("{{\"id\":\"{prefix}{i}\",\"label\":\"tmp\"}}\n"))
        .collect()
}

/// Adds `n` vertices labelled `tmp`, ids `<prefix><i>`.
fn add_vertices(g: &mut TraversalGraph, prefix: &str, n: usize) -> Result<(), GraphError> {
    for i in 0..n {
        g.add_vertex(
            Some(format!("{prefix}{i}").into()),
            vec!["tmp".into()],
            Default::default(),
        )?;
    }
    Ok(())
}

/// A change set that adds `n` vertices labelled `tmp`, ids `<prefix><i>`.
fn change_set_adding(prefix: &str, n: usize) -> graphersal::changes::ChangeSet {
    let mut source = TraversalGraph::new();
    let ((), changes) = source.dry_run(|g| add_vertices(g, prefix, n)).unwrap();
    changes
}

/// The operation, budget and observed bytes of a unit's memory-budget error.
fn unit_limit(err: &GraphError) -> (&str, u64, u64) {
    match err {
        GraphError::ResourceLimitExceeded {
            operation,
            limit,
            max: Some(max),
            observed: Some(observed),
            ..
        } => {
            assert_eq!(limit, "memory.limit");
            (operation, *max, *observed)
        }
        other => panic!("expected ResourceLimitExceeded, got {other:?}"),
    }
}

/// `graph` is Modern exactly as built: nothing of a failed unit is left, no commit happened.
fn assert_untouched(graph: &TraversalGraph) {
    assert_eq!(graph.vertex_count(), 6);
    assert_eq!(graph.edge_count(), 6);
    assert_eq!(graph.last_commit_seq(), 0);
}

#[cfg(feature = "io")]
#[test]
fn a_graphml_import_over_the_budget_rolls_back_fully() {
    let (mut graph, limit) = modern_with_room(4_000);
    memory_limit_scope(limit, || {
        let base = graph.memory_usage().unwrap().unwrap().total_bytes();
        let per = element_bytes(&graph);
        let err = graph
            .import_graphml_reader(graphml_vertices("tmp", 5_000).as_bytes())
            .unwrap_err();
        let IOError::Graph(err) = err else {
            panic!("expected IOError::Graph, got {err:?}");
        };
        let (operation, max, observed) = unit_limit(&err);
        assert_eq!(operation, "GraphML import");
        assert_eq!(max, limit);
        // Stopped by the periodic check, after 4096 of the 5000 vertices, not at the end.
        assert_eq!(observed, base + UNIT_CHECK_INTERVAL * per);
        assert_untouched(&graph);
        let text = format_diagnostic(&err);
        assert!(
            text.contains("Resource limit 'memory.limit' exceeded by the GraphML import"),
            "{text}"
        );
        assert!(text.contains("The GraphML import would take"), "{text}");
        assert!(text.contains("--memory-limit"), "{text}");
        // A small import fits and commits.
        graph
            .import_graphml_reader(graphml_vertices("tmp", 3).as_bytes())
            .unwrap();
        assert_eq!(graph.vertex_count(), 9);
    });
}

#[cfg(feature = "io")]
#[test]
fn a_graphson_import_over_the_budget_rolls_back_fully() {
    let (mut graph, limit) = modern_with_room(4_000);
    memory_limit_scope(limit, || {
        // 100 vertices: under the check interval, so the check at the end of the import stops it.
        let err = graph
            .import_graphson_reader(graphson_vertices("tmp", 100).as_bytes())
            .unwrap_err();
        let IOError::Graph(err) = err else {
            panic!("expected IOError::Graph, got {err:?}");
        };
        let (operation, max, observed) = unit_limit(&err);
        assert_eq!(operation, "GraphSON import");
        assert_eq!(max, limit);
        assert!(observed > limit);
        assert_untouched(&graph);
        graph
            .import_graphson_reader(graphson_vertices("tmp", 3).as_bytes())
            .unwrap();
        assert_eq!(graph.vertex_count(), 9);
    });
}

#[test]
fn an_applied_change_set_over_the_budget_rolls_back_fully() {
    let (mut graph, limit) = modern_with_room(4_000);
    memory_limit_scope(limit, || {
        // The figures the budget starts from: the footprint is cached by the commit position, and
        // a rolled-back unit keeps that position.
        let base = graph.memory_usage().unwrap().unwrap().total_bytes();
        let per = element_bytes(&graph);
        let err = graph
            .apply_change_set(&change_set_adding("tmp", 100))
            .unwrap_err();
        let (operation, max, observed) = unit_limit(&err);
        assert_eq!(operation, "change set apply");
        assert_eq!(max, limit);
        assert!(observed > limit);
        assert_untouched(&graph);
        // A large one is stopped by the periodic check.
        let err = graph
            .apply_change_set(&change_set_adding("tmp", 5_000))
            .unwrap_err();
        assert_eq!(unit_limit(&err).2, base + UNIT_CHECK_INTERVAL * per);
        assert_untouched(&graph);
        graph
            .apply_change_set(&change_set_adding("tmp", 3))
            .unwrap();
        assert_eq!(graph.vertex_count(), 9);
    });
}

#[test]
fn a_transaction_over_the_budget_rolls_back_at_its_commit() {
    let (mut graph, limit) = modern_with_room(4_000);
    memory_limit_scope(limit, || {
        let err = graph
            .transaction(|g| add_vertices(g, "tmp", 100))
            .unwrap_err();
        let (operation, max, observed) = unit_limit(&err);
        assert_eq!(operation, "commit");
        assert_eq!(max, limit);
        assert!(observed > limit);
        assert_untouched(&graph);
        // A savepoint is not checked on its own: the outermost commit decides.
        let err = graph
            .transaction(|g| g.transaction(|inner| add_vertices(inner, "tmp", 100)))
            .unwrap_err();
        assert_eq!(unit_limit(&err).0, "commit");
        assert_untouched(&graph);
        graph.transaction(|g| add_vertices(g, "tmp", 3)).unwrap();
        assert_eq!(graph.vertex_count(), 9);
        assert_eq!(graph.last_commit_seq(), 1);
    });
}

#[cfg(feature = "io")]
#[test]
fn under_the_budget_or_without_one_units_are_unchanged() {
    for (mut graph, limit) in [
        modern_with_room(1 << 30),
        (TraversalGraph::tinkerpop_modern(), u64::MAX),
    ] {
        memory_limit_scope(limit, || {
            graph
                .import_graphml_reader(graphml_vertices("ml", 5_000).as_bytes())
                .unwrap();
            graph
                .import_graphson_reader(graphson_vertices("son", 10).as_bytes())
                .unwrap();
            graph
                .apply_change_set(&change_set_adding("cs", 5_000))
                .unwrap();
            graph.transaction(|g| add_vertices(g, "tx", 3)).unwrap();
            assert_eq!(graph.vertex_count(), 6 + 5_000 + 10 + 5_000 + 3);
            assert_eq!(graph.last_commit_seq(), 4);
        });
    }
}

#[test]
fn a_unit_that_does_not_grow_the_graph_commits_over_the_budget() {
    // The graph alone is over this ceiling: units that add fail, units that remove commit.
    let mut graph = TraversalGraph::tinkerpop_modern();
    memory_limit_scope(1_000, || {
        let err = graph
            .transaction(|g| add_vertices(g, "tmp", 1))
            .unwrap_err();
        assert_eq!(unit_limit(&err).0, "commit");
        assert_untouched(&graph);
        let vertex = graph.get_vertex_by_id("1").unwrap().unwrap();
        graph.transaction(|g| g.drop_vertex(vertex)).unwrap();
        assert_eq!(graph.vertex_count(), 5);
    });
}

#[cfg(feature = "io")]
#[test]
fn a_script_import_is_checked_against_the_ceiling() {
    use crate::script_host::{engine_with_options, graph_scope_with_options};
    let graph = modern();
    let path = std::env::temp_dir().join(format!(
        "graphersal-unit-budget-{}.graphml",
        std::process::id()
    ));
    std::fs::write(&path, graphml_vertices("tmp", 100)).unwrap();
    let script = format!("g.import_graphml({:?})", path.to_string_lossy());
    let mut scope = graph_scope_with_options(graph.clone(), &[]);
    let err = memory_limit_scope(footprint(&graph) + 4_000, || {
        engine_with_options(&ScriptLimits::unlimited(), &[])
            .eval_with_scope::<rhai::Dynamic>(&mut scope, &script)
    })
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("Resource limit 'memory.limit' exceeded by the GraphML import"),
        "{err}"
    );
    assert_eq!(graph.read().vertex_count(), 6);
    // Under the default ceiling the same import commits.
    let mut scope = graph_scope_with_options(graph.clone(), &[]);
    let imported = engine_with_options(&ScriptLimits::unlimited(), &[])
        .eval_with_scope::<rhai::Dynamic>(&mut scope, &script)
        .unwrap();
    assert!(imported.to_string().starts_with("Imported GraphML"));
    assert_eq!(graph.read().vertex_count(), 106);
    let _ = std::fs::remove_file(&path);
}

/// Runs `script` on `graph` as a job with a memory limit of `bytes` per run; the run's error.
fn job_error(graph: &Arc<Graph>, bytes: u64, script: &'static str) -> Option<String> {
    use graphersal::jobs::{Job, JobKind, JobLimits, Scheduler};
    let target = graph.clone();
    let mut scheduler = Scheduler::new();
    scheduler
        .add(
            Job::new(
                "bounded",
                JobKind::User,
                "* * * * *".parse().unwrap(),
                move |_run| run(&target, script).map(|_| ()),
            )
            .with_limits(JobLimits::new().with_memory_limit(bytes)),
        )
        .unwrap();
    scheduler.run_now("bounded", 1_800_000_000_000_000).unwrap();
    let jobs = scheduler.jobs();
    jobs[0].last.as_ref().unwrap().error.clone()
}

#[test]
fn a_job_memory_limit_is_the_per_query_limit_not_the_process_ceiling() {
    let graph = modern();
    let footprint = footprint(&graph);
    // Below the memory the graph (and so the process) already holds: as a process ceiling it
    // would fail every query at once; as the per-query limit it bounds what the query holds.
    let below_the_graph = (footprint / 2).max(1);
    assert_eq!(
        job_error(&graph, below_the_graph, "g.v().count().next()"),
        None
    );
    // A query that materializes more than the limit fails on the per-query key.
    let error = job_error(&graph, 1024, "g.v().both().both().both().fold().next()")
        .expect("the fold is over the job's limit");
    assert!(
        error.contains("traversal.max_materialized_bytes"),
        "{error}"
    );
    assert!(!error.contains("memory.limit"), "{error}");
    // A query's own stricter value stays.
    let error = job_error(
        &graph,
        1 << 30,
        r#"g.with("traversal.max_materialized_bytes", 1024).v().both().both().both().fold().next()"#,
    )
    .expect("the query's own limit applies");
    assert!(
        error.contains("traversal.max_materialized_bytes"),
        "{error}"
    );
    // ... and a larger own value does not lift the job's limit.
    let error = job_error(
        &graph,
        1024,
        r#"g.with("traversal.max_materialized_bytes", 1 << 30).v().both().both().both().fold().next()"#,
    )
    .expect("the job's limit caps the query's own value");
    assert!(
        error.contains("traversal.max_materialized_bytes"),
        "{error}"
    );
}
