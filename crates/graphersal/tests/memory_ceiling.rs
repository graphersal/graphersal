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
