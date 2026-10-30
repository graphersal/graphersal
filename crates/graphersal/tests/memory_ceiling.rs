//! The process memory ceiling as a PROCESS setting: `graphersal::exec::set_memory_limit`,
//! `set_memory_headroom` and `memory_limit`. A separate target (an exception to the one
//! integration-test binary, listed in `tests/all/main.rs`): the tests here change process-global
//! state, which would affect the tests of `tests/all` running in parallel. The tests of this
//! target serialize on [`LOCK`] and restore the default (`auto`) when they end.
//!
//! Built with the feature `alloc-tracking`, this target installs the tracking allocator, so the
//! same tests check the TRACKED mode (the process's live heap is compared); without it they check
//! the ESTIMATED mode (the graph's footprint plus what a query holds and adds):
//! `cargo test -p graphersal --features script,alloc-tracking --test memory_ceiling`.

use std::sync::{Mutex, MutexGuard};

use graphersal::exec::{self, MemoryLimit, MemoryLimitSource};
use graphersal::prelude::*;

#[cfg(feature = "alloc-tracking")]
#[global_allocator]
static ALLOCATOR: graphersal::alloc_tracking::TrackingAllocator =
    graphersal::alloc_tracking::TrackingAllocator::new();

/// Serializes the tests of this target: they share the process setting.
static LOCK: Mutex<()> = Mutex::new(());

/// Holds [`LOCK`] and restores the default ceiling when dropped (also after a failed assertion).
struct Guard(#[allow(dead_code)] MutexGuard<'static, ()>);

impl Drop for Guard {
    fn drop(&mut self) {
        exec::set_memory_limit(MemoryLimit::Auto).unwrap();
        exec::set_memory_headroom(None).unwrap();
    }
}

fn exclusive() -> Guard {
    Guard(
        LOCK.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    )
}

/// Whether this binary counts the live heap (feature `alloc-tracking`).
fn tracked() -> bool {
    cfg!(feature = "alloc-tracking")
}

/// The error under the step frames of `err`.
fn root(err: &TraverserError) -> &TraverserError {
    let mut current = err;
    while let TraverserError::StepFailed { source, .. } = current {
        current = source;
    }
    current
}

#[test]
fn the_default_is_auto_and_on() {
    let _guard = exclusive();
    let limit = exec::memory_limit().unwrap();
    assert_eq!(limit.setting(), MemoryLimit::Auto);
    assert_eq!(limit.headroom(), None);
    if cfg!(any(target_os = "linux", target_os = "macos", windows)) {
        assert!(
            matches!(
                limit.source(),
                MemoryLimitSource::Cgroup | MemoryLimitSource::PhysicalMemory
            ),
            "{limit:?}"
        );
        assert!(limit.bytes().unwrap() >= 64 << 20, "{limit:?}");
    }
    // Ordinary queries run under it.
    let graph = TraversalGraph::tinkerpop_modern();
    assert_eq!(
        graph.traversal().v(None).count().next().unwrap(),
        Some(6.into())
    );
}

#[test]
fn a_configured_ceiling_is_reported_and_enforced() {
    let _guard = exclusive();
    let graph = TraversalGraph::tinkerpop_modern();
    // Far below anything the process holds (tracked) or the graph's footprint (estimated).
    exec::set_memory_limit(MemoryLimit::Bytes(1_000)).unwrap();
    let limit = exec::memory_limit().unwrap();
    assert_eq!(limit.bytes(), Some(1_000));
    assert_eq!(limit.source(), MemoryLimitSource::Configured);
    assert_eq!(limit.source().as_str(), "configured");
    let err = graph.traversal().v(None).count().next().unwrap_err();
    match root(&err) {
        TraverserError::ResourceLimitExceeded {
            limit,
            max,
            observed,
            ..
        } => {
            assert_eq!(limit, "memory.limit");
            assert_eq!(*max, Some(1_000));
            assert!(observed.unwrap() > 1_000);
        }
        other => panic!("expected ResourceLimitExceeded, got {other:?}"),
    }
    let help = err.help().unwrap();
    let measure = if tracked() {
        "The figure is tracked: the process's exact live heap"
    } else {
        "The figure is estimated (no tracking allocator in this process)"
    };
    assert!(help.contains(measure), "{help}");
    assert!(help.contains("--memory-limit"), "{help}");
    // A write over the ceiling leaves nothing.
    let mut writable = TraversalGraph::tinkerpop_modern();
    assert!(writable.traversal_mut().add_v("x").next().is_err());
    assert_eq!(writable.vertex_count(), 6);
    // Raised again, the same graph answers.
    exec::set_memory_limit(MemoryLimit::Bytes(1 << 40)).unwrap();
    assert_eq!(
        graph.traversal().v(None).count().next().unwrap(),
        Some(6.into())
    );
}

#[test]
fn none_turns_the_ceiling_off() {
    let _guard = exclusive();
    exec::set_memory_limit(MemoryLimit::Unlimited).unwrap();
    let limit = exec::memory_limit().unwrap();
    assert_eq!(limit.bytes(), None);
    assert_eq!(limit.source(), MemoryLimitSource::Unlimited);
    assert_eq!(limit.source().as_str(), "none");
    let graph = TraversalGraph::tinkerpop_modern();
    assert_eq!(
        graph.traversal().v(None).count().next().unwrap(),
        Some(6.into())
    );
    // A test scope still lowers the ceiling for its thread.
    assert!(exec::memory_limit_scope(1_000, || graph.traversal().v(None).count().next()).is_err());
}

#[test]
fn invalid_values_are_refused_and_change_nothing() {
    let _guard = exclusive();
    exec::set_memory_limit(MemoryLimit::Bytes(5 << 30)).unwrap();
    let err = exec::set_memory_limit(MemoryLimit::Bytes(0)).unwrap_err();
    assert!(
        matches!(&*err, TraverserError::InvalidOption { key, .. } if key == "memory.limit"),
        "{err:?}"
    );
    assert_eq!(exec::memory_limit().unwrap().bytes(), Some(5 << 30));
    assert!("-1".parse::<MemoryLimit>().is_err());
}

#[test]
fn the_headroom_is_kept_and_reported() {
    let _guard = exclusive();
    exec::set_memory_headroom(Some(256 << 20)).unwrap();
    let limit = exec::memory_limit().unwrap();
    assert_eq!(limit.headroom(), Some(256 << 20));
    // It applies to a cgroup limit only; on a machine without one the physical default stays.
    if limit.source() == MemoryLimitSource::PhysicalMemory {
        exec::set_memory_headroom(None).unwrap();
        assert_eq!(exec::memory_limit().unwrap().bytes(), limit.bytes());
    }
}

#[test]
fn a_thread_scope_lowers_the_process_ceiling() {
    let _guard = exclusive();
    exec::set_memory_limit(MemoryLimit::Bytes(1 << 40)).unwrap();
    // The process ceiling is far above; the lower scope of this thread wins.
    let graph = TraversalGraph::tinkerpop_modern();
    let err =
        exec::memory_limit_scope(1_000, || graph.traversal().v(None).count().next()).unwrap_err();
    assert!(
        matches!(
            root(&err),
            TraverserError::ResourceLimitExceeded {
                max: Some(1_000),
                ..
            }
        ),
        "{err:?}"
    );
}

/// A Store whose graph does not fit under the PROCESS ceiling refuses the open before any
/// element is read, and its help names the minimum to raise the ceiling to (tracked: the live
/// heap plus the graph's estimate). The same store opens once the ceiling is raised.
#[test]
#[cfg(feature = "persist")]
fn a_store_open_over_the_process_ceiling_names_the_minimum_needed() {
    use graphersal::error::DiagnosticError;
    use graphersal::persist::{MemDir, PersistError, Store, StoreDir, StoreOptions};
    let _guard = exclusive();
    let dir: std::sync::Arc<dyn StoreDir> = std::sync::Arc::new(MemDir::named("ceiling-open"));
    let mut graph = TraversalGraph::new();
    for i in 0..2000 {
        graph
            .traversal_mut()
            .add_v("person")
            .property("name", format!("person {i}"))
            .iterate()
            .unwrap();
    }
    let options = || StoreOptions::new().with_checkpoint_after_bytes(0);
    Store::create_with(&dir, graph, options())
        .unwrap()
        .close()
        .unwrap();
    let live = graphersal::exec::memory_room().unwrap().live_bytes();
    assert_eq!(live.is_some(), tracked());
    // A ceiling just above what the process holds now: the graph does not fit.
    let ceiling = live.unwrap_or(0) + 10_000;
    exec::set_memory_limit(MemoryLimit::Bytes(ceiling)).unwrap();
    let err = match Store::open(&dir, options()) {
        Err(err) => err,
        Ok(_) => panic!("the open was not refused"),
    };
    let PersistError::MemoryBudget {
        needed,
        limit,
        operation,
        in_use,
        setting,
        ..
    } = &err
    else {
        panic!("expected MemoryBudget, got {err:?}");
    };
    assert_eq!(
        (*operation, *setting, *limit),
        ("store open", "memory.limit", ceiling)
    );
    assert_eq!(in_use.is_some(), tracked());
    let minimum = in_use.unwrap_or(0) + needed;
    let help = err.help().unwrap();
    assert!(
        help.contains(&format!("at least {minimum} bytes")),
        "{help}"
    );
    assert!(help.contains("--memory-limit"), "{help}");
    exec::set_memory_limit(MemoryLimit::Bytes(minimum.max(ceiling) * 4 + (64 << 20))).unwrap();
    let store = Store::open(&dir, options()).unwrap();
    assert_eq!(store.graph().read().vertex_count(), 2000);
    store.close().unwrap();
}

/// The per-query limit `traversal.max_materialized_bytes` counts the LIVE bytes the query holds
/// at once (tracked: the live heap of its thread since it started; estimated: the engine's
/// count), never a sum of allocations: a chain of twenty `fold().unfold()` holds no more at
/// once than one (within allocator noise), and runs under a limit far below the sum of its
/// lists. What a writing query adds to the GRAPH does not count against it.
#[test]
fn the_per_query_limit_counts_live_bytes_when_tracked() {
    const LIMIT: &str = "traversal.max_materialized_bytes";
    let _guard = exclusive();
    let mut graph = TraversalGraph::new();
    for _ in 0..5000 {
        graph.traversal_mut().add_v("x").iterate().unwrap();
    }
    let peak = |graph: &TraversalGraph, folds: usize| {
        let mut traversal = graph.traversal().with(LIMIT, 1i64 << 40).v(None);
        for _ in 0..folds {
            traversal = traversal.fold().unfold();
        }
        let metrics = traversal.count().profile().unwrap();
        let memory = metrics.memory().unwrap();
        let expected = if tracked() {
            graphersal::profile::MemorySource::Tracked
        } else {
            graphersal::profile::MemorySource::Estimated
        };
        assert_eq!(memory.source(), expected);
        memory.peak_bytes()
    };
    let one = peak(&graph, 1);
    let twenty = peak(&graph, 20);
    assert!(
        one >= 5000 * 80,
        "the stream of 5000 traversers is held: {one}"
    );
    assert!(twenty <= one + one / 2, "{twenty} vs {one}");
    // Twenty lists of 5000 values (at least 32 bytes each) sum to 3.2 MB; the chain runs under
    // twice what one fold holds.
    let limit = one * 2;
    assert!(20 * 5000 * 32 > limit);
    let mut traversal = graph.traversal().with(LIMIT, limit as i64).v(None);
    for _ in 0..20 {
        traversal = traversal.fold().unfold();
    }
    assert_eq!(traversal.count().to_list().unwrap().len(), 1);
    // Below what one fold holds, the query fails on the per-query limit.
    let err = graph
        .traversal()
        .with(LIMIT, (one / 4) as i64)
        .v(None)
        .fold()
        .unfold()
        .count()
        .to_list()
        .unwrap_err();
    assert!(
        matches!(root(&err), TraverserError::ResourceLimitExceeded { limit, .. } if limit == LIMIT),
        "{err}"
    );
    // A query that adds 20 000 vertices one at a time: its own memory is far below what the
    // graph gained.
    let before = graph.memory_usage().unwrap().unwrap().total_bytes();
    let metrics = graph
        .traversal_mut()
        .with(LIMIT, 1i64 << 40)
        .inject(vec![1i64])
        .repeat(__::add_v("y"))
        .times(20_000)
        .count()
        .profile()
        .unwrap();
    let grown = graph
        .memory_usage()
        .unwrap()
        .unwrap()
        .total_bytes()
        .saturating_sub(before);
    let peak = metrics.memory().unwrap().peak_bytes();
    assert!(grown > 1_000_000, "{grown}");
    assert!(
        peak < grown / 2,
        "the graph's growth is not the query's: {peak} vs {grown}"
    );
}
