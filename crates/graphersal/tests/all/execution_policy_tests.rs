//! Integration tests for [`ExecutionPolicy`]: a host-set, immutable-after-construction ceiling
//! on which `g.with()` execution options a query may set, and what their defaults are when it
//! doesn't.
//!
//! The threat this exists for: `evaluationTimeout` and `repeat.max_loops` bound a runaway
//! query's resource use, but both are ordinary `g.with()` keys the query itself can freely
//! override. A host embedding this engine somewhere untrusted input can reach (the stated
//! example: `graphersal-playground`) needs a ceiling the query cannot touch. These tests build a
//! `TraversalGraph` with a locked, low `evaluationTimeout`/`repeat.max_loops` policy (the way a
//! trusted host would, at construction time, before ever handing the graph to untrusted code —
//! `g.with()` itself has no way to set a policy, by design) and confirm a query cannot override
//! either bound, and that a genuine `repeat()`-bomb on a cyclic graph is stopped quickly and
//! safely even when it tries to.

use compact_str::CompactString;
use graphersal::exec::ExecutionPolicy;
use graphersal::prelude::*;

/// A 3-vertex cycle (a -> b -> c -> a), the same shape a `repeat()`-without-`times()` "bomb"
/// query would exploit to run forever, wrapped for sharing exactly as a host would: policy set
/// first via [`TraversalGraph::with_execution_policy`], only then wrapped in the `Graph`
/// (a `TraversalGraph` behind a lock) type queries actually execute against.
fn cyclic_graph(policy: ExecutionPolicy) -> Graph {
    let mut graph = TraversalGraph::new();
    let mut ids = std::collections::HashMap::new();
    for id in ["a", "b", "c"] {
        let mut props = PropertyMap::default();
        props.insert("name", ElementProperty::String(id.into()));
        ids.insert(
            id,
            graph
                .add_vertex(Some(id.into()), vec!["node".into()], props)
                .unwrap(),
        );
    }
    for (from, to) in [("a", "b"), ("b", "c"), ("c", "a")] {
        graph
            .add_edge(
                None,
                Some("child".into()),
                ids[from],
                ids[to],
                PropertyMap::default(),
            )
            .unwrap();
    }
    let graph = graph.with_execution_policy(policy);
    Graph::new(graph)
}

/// A ~110k-element, 5-level fan-out (10 top-level vertices, each fanning out 10x per level),
/// the same fixture shape as `GraphSource::large_110k()` — large enough that `out().out().out()`
/// takes measurable wall-clock time, so an `evaluationTimeout` bound can be genuinely tested
/// rather than merely asserted. Built directly on [`TraversalGraph`] (not `GraphSource`, which
/// only exposes this fixture already wrapped in `Graph`) so a policy can be attached before the
/// graph is ever wrapped for sharing.
fn fanout_graph(policy: ExecutionPolicy) -> Graph {
    let top_level_count = 10;
    let second_level_count = 10;
    let inner_level = 5;

    let mut g = TraversalGraph::new();
    let mut prev_vertex_ids = vec![];
    for level in 0..inner_level {
        let num_of_nodes = if level == 0 {
            top_level_count
        } else {
            second_level_count
        };
        if prev_vertex_ids.is_empty() {
            for index in 0..num_of_nodes {
                let id = format!("V_{level}_{index}");
                let mut value = PropertyMap::default();
                value.insert("id", ElementProperty::String(id.clone().into()));
                let node_index = g
                    .add_vertex(Some(id.clone().into()), vec![id.into()], value)
                    .unwrap();
                prev_vertex_ids.push(node_index);
            }
        } else {
            let mut current_vertex_ids = vec![];
            let mut node_counter = 0;
            for prev_node_index in prev_vertex_ids.into_iter() {
                for _ in 0..num_of_nodes {
                    let id = format!("V_{level}_{node_counter}");
                    node_counter += 1;
                    let mut value = PropertyMap::default();
                    value.insert("id", ElementProperty::String(id.clone().into()));
                    let node_index = g
                        .add_vertex(Some(id.clone().into()), vec![id.into()], value)
                        .unwrap();
                    g.add_edge(
                        None::<CompactString>,
                        None::<CompactString>,
                        prev_node_index,
                        node_index,
                        PropertyMap::default(),
                    )
                    .unwrap();
                    current_vertex_ids.push(node_index);
                }
            }
            prev_vertex_ids = current_vertex_ids;
        }
    }
    let g = g.with_execution_policy(policy);
    Graph::new(g)
}

// --- ExecutionPolicy itself -------------------------------------------------------------------

#[test]
fn permissive_locks_and_defaults_nothing() {
    let policy = ExecutionPolicy::permissive();
    assert!(!policy.is_locked("evaluationTimeout"));
    assert!(policy.default_for("evaluationTimeout").is_none());
}

// --- A GraphStorage that never calls with_execution_policy() behaves exactly as before ---------

#[test]
fn a_graph_without_a_policy_behaves_exactly_as_before() {
    // No with_execution_policy() call at all: GraphSource::tinkerpop_modern() is the ordinary,
    // pre-this-spec construction path every existing test already exercises.
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    // A query can freely set evaluationTimeout and repeat.max_loops, exactly as it always could.
    let count = lock
        .traversal()
        .with("evaluationTimeout", 60_000)
        .with("repeat.max_loops", 100)
        .v(None)
        .count()
        .next()
        .unwrap();
    assert_eq!(count, Some(6.into()));
}

// --- Locking evaluationTimeout ------------------------------------------------------------------

#[test]
fn a_locked_evaluation_timeout_rejects_a_querys_own_override() {
    let policy = ExecutionPolicy::permissive()
        .with_locked("evaluationTimeout")
        .with_default("evaluationTimeout", 2000);
    let graph = fanout_graph(policy);
    let lock = graph.read();
    let err = lock
        .traversal()
        .with("evaluationTimeout", 999_999_999)
        .v(None)
        .count()
        .next()
        .expect_err("the query's own evaluationTimeout override must be rejected");
    match &*err {
        TraverserError::LockedOption { key, .. } => assert_eq!(key, "evaluationTimeout"),
        other => panic!("expected LockedOption, got {other:?}"),
    }
    let help = err.help().expect("help");
    assert!(help.contains("evaluationTimeout"), "{help}");
    assert!(help.contains("execution policy"), "{help}");
}

#[test]
fn a_locked_evaluation_timeouts_own_default_is_genuinely_enforced() {
    // A fan-out slow enough to blow a 2 ms budget, on a graph whose policy locks the timeout to
    // 2 ms: the query never gets to raise it, so the timeout must genuinely fire.
    let policy = ExecutionPolicy::permissive()
        .with_locked("evaluationTimeout")
        .with_default("evaluationTimeout", 2);
    let graph = fanout_graph(policy);
    let lock = graph.read();
    // The query tries to disable the timeout entirely (0 means "no timeout") - locked, so this
    // must have no effect at all.
    let err = lock
        .traversal()
        .with("evaluationTimeout", 0)
        .v(None)
        .out(None)
        .out(None)
        .out(None)
        .count()
        .next()
        .expect_err(
            "a locked 2 ms budget must still be enforced despite the query's override attempt",
        );
    // The query's attempt itself is rejected before the policy's own default even gets a chance
    // to run - so the observed error is LockedOption, not (yet) Timeout.
    assert!(
        matches!(&*err, TraverserError::LockedOption { key, .. } if key == "evaluationTimeout")
    );

    // A query that doesn't touch evaluationTimeout at all still hits the policy's own bound.
    let err = lock
        .traversal()
        .v(None)
        .out(None)
        .out(None)
        .out(None)
        .count()
        .next()
        .expect_err("the policy's own 2 ms default must be enforced");
    assert!(
        matches!(
            err.root_cause(),
            TraverserError::Timeout { limit_ms: 2, .. }
        ),
        "expected a 2 ms timeout, got {err:?}"
    );
}

// --- Locking repeat.max_loops --------------------------------------------------------------------

#[test]
fn a_locked_repeat_max_loops_rejects_a_querys_own_override() {
    let policy = ExecutionPolicy::permissive()
        .with_locked("repeat.max_loops")
        .with_default("repeat.max_loops", 25);
    let graph = cyclic_graph(policy);
    let lock = graph.read();
    let err = lock
        .traversal()
        .with("repeat.max_loops", 999_999_999)
        .v("a")
        .repeat(__::out(None))
        .to_list()
        .expect_err("the query's own repeat.max_loops override must be rejected");
    assert!(matches!(&*err, TraverserError::LockedOption { key, .. } if key == "repeat.max_loops"));
}

#[test]
fn a_repeat_bomb_is_stopped_quickly_by_a_locked_max_loops_even_when_it_tries_to_override_it() {
    // The actual threat scenario: g.with("evaluationTimeout", huge).with("repeat.max_loops",
    // huge).repeat(out()) with no times()/until() on a cycle - a genuine bomb that would
    // otherwise run forever. A locked, low policy on both keys must stop it quickly and safely
    // regardless of what the query asks for.
    let policy = ExecutionPolicy::permissive()
        .with_locked("evaluationTimeout")
        .with_default("evaluationTimeout", 5000)
        .with_locked("repeat.max_loops")
        .with_default("repeat.max_loops", 25);
    let graph = cyclic_graph(policy);
    let lock = graph.read();
    let start = std::time::Instant::now();
    let err = lock
        .traversal()
        .with("evaluationTimeout", 999_999_999)
        .with("repeat.max_loops", 999_999_999)
        .v("a")
        .repeat(__::out(None))
        .to_list()
        .expect_err("a repeat() bomb on a cycle must be stopped, not run forever");
    let elapsed = start.elapsed();
    // Rejected at option-parse time, long before 25 loops could even matter for wall-clock time -
    // this is the point: the bomb never gets to run at all.
    assert!(
        matches!(&*err, TraverserError::LockedOption { .. }),
        "expected the locked-option check to fire before execution, got {err:?}"
    );
    assert!(
        elapsed.as_secs() < 5,
        "stopped in {elapsed:?}, expected near-instant rejection"
    );
}

#[test]
fn a_repeat_bomb_that_does_not_touch_max_loops_still_hits_the_policys_bound() {
    let policy = ExecutionPolicy::permissive()
        .with_locked("repeat.max_loops")
        .with_default("repeat.max_loops", 25);
    let graph = cyclic_graph(policy);
    let lock = graph.read();
    let err = lock
        .traversal()
        .v("a")
        .repeat(__::out(None))
        .to_list()
        .expect_err("a -> b -> c -> a never ends without a stop condition");
    assert!(
        matches!(
            err.root_cause(),
            TraverserError::RepeatLimitExceeded { max_loops: 25, .. }
        ),
        "expected the policy's own 25-loop bound, got {err:?}"
    );
}

// --- Defaults without locking: the query can still override --------------------------------------

#[test]
fn a_default_with_nothing_locked_can_still_be_overridden_by_the_query() {
    let policy = ExecutionPolicy::permissive().with_default("repeat.max_loops", 5);
    let graph = cyclic_graph(policy);
    let lock = graph.read();

    // The policy default applies when the query says nothing: 5 loops is not enough for a
    // 3-cycle repeat() with no stop condition.
    let err = lock
        .traversal()
        .v("a")
        .repeat(__::out(None))
        .to_list()
        .expect_err("the policy's default of 5 loops is not locked but still applies by default");
    assert!(matches!(
        err.root_cause(),
        TraverserError::RepeatLimitExceeded { max_loops: 5, .. }
    ));

    // But since it isn't locked, the query can raise it itself.
    let err = lock
        .traversal()
        .with("repeat.max_loops", 50)
        .v("a")
        .repeat(__::out(None))
        .to_list()
        .expect_err("the query's own override replaces the unlocked policy default");
    assert!(matches!(
        err.root_cause(),
        TraverserError::RepeatLimitExceeded { max_loops: 50, .. }
    ));
}

// --- No setter after construction ---------------------------------------------------------------

#[test]
fn there_is_no_way_to_change_the_policy_of_an_already_constructed_graph() {
    // with_execution_policy() consumes and returns Self, so it can only run before the graph is
    // wrapped in Graph (a TraversalGraph behind a lock) for sharing. Once wrapped, nothing in this
    // crate's public API - not even a caller holding the write lock - can call it again: there is
    // no `fn set_execution_policy(&mut self, ..)`. This test documents that guarantee by
    // demonstrating the write-lock-holding caller has no such method available; it would fail to
    // compile if one were ever (re)added without updating this test's surrounding comment.
    let policy = ExecutionPolicy::permissive().with_locked("evaluationTimeout");
    let graph = TraversalGraph::new().with_execution_policy(policy.clone());
    let graph = Graph::new(graph);
    let mut write_lock = graph.write();
    // write_lock derefs to &mut TraversalGraph - only shrink_to_fit()/set_schema()/mutation methods
    // are available, no policy setter.
    write_lock.shrink_to_fit();
    drop(write_lock);
    let read_lock = graph.read();
    assert_eq!(read_lock.execution_policy().unwrap(), policy);
}
