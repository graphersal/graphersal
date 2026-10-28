//! The memory budget (`memory.limit`, `memory.headroom`): off by default; an explicit budget
//! fails an oversized traversal with `ResourceLimitExceeded` and rolls back what it wrote, and the
//! process (and the graph) carry on. This binary installs no tracking allocator, so the budget
//! runs on the engine's estimate (the graph footprint at the start of the run plus what the run
//! holds and adds).
#![cfg(feature = "script")]

use crate::script_host::{ScriptLimits, eval_value_with_limits};
use graphersal::exec::ExecutionPolicy;
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
fn there_is_no_budget_by_default() {
    let graph = modern();
    let count = run(&graph, &format!("g.{BIG_FOLD}")).unwrap();
    assert!(count.as_int().unwrap() > 0);
}

#[test]
fn an_explicit_budget_fails_the_query_and_the_process_carries_on() {
    let graph = modern();
    let budget = footprint(&graph) + 2_000;
    let err = run(
        &graph,
        &format!("g.with(\"memory.limit\", {budget}).{BIG_FOLD}"),
    )
    .unwrap_err();
    assert!(
        err.contains("Resource limit 'memory.limit' exceeded"),
        "{err}"
    );
    assert!(err.contains(&format!("limit {budget}")), "{err}");
    assert!(err.contains("--memory-limit"), "{err}");
    // The same session runs a small query under the same budget, and a large one without it.
    let small = run(
        &graph,
        &format!("g.with(\"memory.limit\", {budget}).v().count().next()"),
    )
    .unwrap();
    assert_eq!(small.as_int().unwrap(), 6);
    assert!(run(&graph, &format!("g.{BIG_FOLD}")).is_ok());
}

#[test]
fn the_error_is_structured_and_located() {
    let graph = modern();
    let budget = footprint(&graph) + 2_000;
    let lock = graph.read();
    let err = lock
        .traversal()
        .with("memory.limit", budget as i64)
        .v(None)
        .both(None)
        .both(None)
        .both(None)
        .fold()
        .next()
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
    let err = run(
        &graph,
        &format!(
            "g.with(\"memory.limit\", {budget}).inject({}).unfold().addV(\"tmp\").toList()",
            format_args!("{:?}", (0..40).collect::<Vec<_>>())
        ),
    )
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
    let added = run(
        &graph,
        &format!("g.with(\"memory.limit\", {budget}).addV(\"tmp\").count().next()"),
    )
    .unwrap();
    assert_eq!(added.as_int().unwrap(), 1);
    assert_eq!(graph.read().vertex_count(), 7);
}

#[test]
fn a_host_policy_sets_and_locks_the_budget() {
    let policy = ExecutionPolicy::permissive()
        .with_default("memory.limit", 1_000)
        .with_locked("memory.limit");
    let graph = TraversalGraph::tinkerpop_modern().with_execution_policy(policy);
    // The graph alone is over 1000 bytes: every traversal fails at its first step.
    let err = graph.traversal().v(None).count().next().unwrap_err();
    assert!(
        matches!(root(&err), TraverserError::ResourceLimitExceeded { limit, .. } if limit == "memory.limit"),
        "{err:?}"
    );
    // A query cannot raise a locked budget.
    let err = graph
        .traversal()
        .with("memory.limit", i64::MAX)
        .v(None)
        .count()
        .next()
        .unwrap_err();
    assert!(
        matches!(&*err, TraverserError::LockedOption { key, .. } if key == "memory.limit"),
        "{err:?}"
    );
}

#[test]
fn auto_without_a_cgroup_limit_is_no_budget_and_bad_values_are_rejected() {
    let graph = modern();
    // On a host without a cgroup memory limit (a developer machine) "auto" is no budget; on a
    // host with one, this small query fits anyway.
    let count = run(
        &graph,
        "g.with(\"memory.limit\", \"auto\").with(\"memory.headroom\", 0).v().count().next()",
    )
    .unwrap();
    assert_eq!(count.as_int().unwrap(), 6);
    for (value, key) in [
        ("\"lots\"", "memory.limit"),
        ("0", "memory.limit"),
        ("-5", "memory.limit"),
    ] {
        let err = run(
            &graph,
            &format!("g.with(\"memory.limit\", {value}).v().count().next()"),
        )
        .unwrap_err();
        assert!(
            err.contains(&format!("Invalid value for option '{key}'")),
            "{err}"
        );
    }
    let err = run(
        &graph,
        "g.with(\"memory.limit\", \"auto\").with(\"memory.headroom\", -1).v().count().next()",
    )
    .unwrap_err();
    assert!(err.contains("memory.headroom"), "{err}");
}

// ---------------------------------------------------------------------------------------------
// Units that are not traversals (`memory.limit` of the graph's execution policy, or a session's
// preset): GraphML/GraphSON imports into a live graph and `apply_change_set` are checked every
// 4096 elements and at their end, and every outermost unit at its commit (a `transaction`
// closure). Over the budget the whole unit rolls back with `GraphError::ResourceLimitExceeded`
// naming the operation.
// ---------------------------------------------------------------------------------------------

/// How many elements a non-traversal unit handles between two budget checks (the engine's
/// `UNIT_CHECK_INTERVAL`).
const UNIT_CHECK_INTERVAL: u64 = 4096;

/// Modern under an execution policy whose memory budget leaves `room` bytes above the graph's
/// footprint; also returns the budget.
fn modern_with_room(room: u64) -> (TraversalGraph, u64) {
    let base = TraversalGraph::tinkerpop_modern()
        .memory_usage()
        .unwrap()
        .unwrap()
        .total_bytes();
    let limit = base + room;
    let policy = ExecutionPolicy::permissive().with_default("memory.limit", limit as i64);
    (
        TraversalGraph::tinkerpop_modern().with_execution_policy(policy),
        limit,
    )
}

/// The estimated bytes of one added element on `graph` (the budget's average element size).
fn element_bytes(graph: &TraversalGraph) -> u64 {
    let base = graph.memory_usage().unwrap().unwrap().total_bytes();
    (base / (graph.vertex_count() + graph.edge_count()) as u64).max(128)
}

/// GraphML text with `n` vertices labelled `tmp`, ids `<prefix><i>`.
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
}

#[cfg(feature = "io")]
#[test]
fn a_graphson_import_over_the_budget_rolls_back_fully() {
    let (mut graph, limit) = modern_with_room(4_000);
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
}

#[test]
fn an_applied_change_set_over_the_budget_rolls_back_fully() {
    let (mut graph, limit) = modern_with_room(4_000);
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
}

#[test]
fn a_transaction_over_the_budget_rolls_back_at_its_commit() {
    let (mut graph, limit) = modern_with_room(4_000);
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
}

#[cfg(feature = "io")]
#[test]
fn under_the_budget_or_without_one_units_are_unchanged() {
    for mut graph in [
        modern_with_room(1 << 30).0,
        TraversalGraph::tinkerpop_modern(),
    ] {
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
    }
}

#[test]
fn a_unit_that_does_not_grow_the_graph_commits_over_the_budget() {
    // The graph alone is over this budget: units that add fail, units that remove commit.
    let policy = ExecutionPolicy::permissive().with_default("memory.limit", 1_000);
    let mut graph = TraversalGraph::tinkerpop_modern().with_execution_policy(policy);
    let err = graph
        .transaction(|g| add_vertices(g, "tmp", 1))
        .unwrap_err();
    assert_eq!(unit_limit(&err).0, "commit");
    assert_untouched(&graph);
    let vertex = graph.get_vertex_by_id("1").unwrap().unwrap();
    graph.transaction(|g| g.drop_vertex(vertex)).unwrap();
    assert_eq!(graph.vertex_count(), 5);
}

#[cfg(feature = "io")]
#[test]
fn a_script_import_honours_the_session_preset() {
    use crate::script_host::{engine_with_options, graph_scope_with_options};
    let graph = modern();
    let path = std::env::temp_dir().join(format!(
        "graphersal-unit-budget-{}.graphml",
        std::process::id()
    ));
    std::fs::write(&path, graphml_vertices("tmp", 100)).unwrap();
    let options = [(
        "memory.limit",
        ElementProperty::from((footprint(&graph) + 4_000) as i64),
    )];
    let script = format!("g.import_graphml({:?})", path.to_string_lossy());
    let mut scope = graph_scope_with_options(graph.clone(), &options);
    let err = engine_with_options(&ScriptLimits::unlimited(), &options)
        .eval_with_scope::<rhai::Dynamic>(&mut scope, &script)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("Resource limit 'memory.limit' exceeded by the GraphML import"),
        "{err}"
    );
    assert_eq!(graph.read().vertex_count(), 6);
    // Without the preset the same import commits.
    let mut scope = graph_scope_with_options(graph.clone(), &[]);
    let imported = engine_with_options(&ScriptLimits::unlimited(), &[])
        .eval_with_scope::<rhai::Dynamic>(&mut scope, &script)
        .unwrap();
    assert!(imported.to_string().starts_with("Imported GraphML"));
    assert_eq!(graph.read().vertex_count(), 106);
    let _ = std::fs::remove_file(&path);
}
