//! Units held across locks (`Graph::begin_unit` and its closes): the owning thread reads and
//! writes through views of the held lock, other threads keep reading until the unit's first
//! write and never see uncommitted changes, writers wait, nested units are savepoints, closing
//! from another thread fails, and the whole-script units of the script layer
//! (`eval_value_atomic`, `eval_value_dry_run`) are built on it.
//!
//! Waits use channels with timeouts. Where a test checks that another thread is BLOCKED, it
//! gives that thread a moment (`BLOCKED_CHECK`) and asserts it has not finished: a check, not a
//! synchronization (a wrong lock makes it finish, a right one can never finish there).

use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use graphersal::changes::RollbackReason;
use graphersal::error::GraphError;
use graphersal::prelude::*;

/// How long a test waits for another thread that must make progress.
const PROGRESS: Duration = Duration::from_secs(20);

/// How long a test gives another thread that must stay blocked before checking it did.
const BLOCKED_CHECK: Duration = Duration::from_millis(200);

fn modern() -> Arc<Graph> {
    Arc::new(Graph::new(TraversalGraph::tinkerpop_modern()))
}

fn add_person(graph: &Graph, name: &str) {
    GraphTraversalSource::new(&mut *graph.write())
        .add_v("person")
        .property("name", name)
        .next()
        .unwrap();
}

fn reason() -> RollbackReason {
    RollbackReason::failed(&"the test rolls back")
}

/// Runs `f` on another thread and returns the receiver of its result.
fn on_thread<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> mpsc::Receiver<T> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx
}

#[test]
fn another_thread_reads_while_a_unit_has_only_read() {
    let graph = modern();
    let mark = graph.begin_unit();
    assert!(graph.holds_unit());
    assert_eq!(
        graph.read().vertex_count(),
        6,
        "the owner reads inside its unit"
    );
    {
        // Even while the owner holds a read view.
        let _view = graph.read();
        let other = graph.clone();
        let count = on_thread(move || (other.holds_unit(), other.read().vertex_count()));
        assert_eq!(count.recv_timeout(PROGRESS).unwrap(), (false, 6));
    }
    graph.commit_unit(mark).unwrap();
    assert!(!graph.holds_unit());
}

#[test]
fn after_the_first_write_other_readers_wait_and_see_only_the_committed_state() {
    for commit in [true, false] {
        let graph = modern();
        let mark = graph.begin_unit();
        add_person(&graph, "uncommitted");
        assert_eq!(
            graph.read().vertex_count(),
            7,
            "the owner sees its own write"
        );

        let other = graph.clone();
        let (started_tx, started_rx) = mpsc::channel();
        let count = on_thread(move || {
            started_tx.send(()).unwrap();
            other.read().vertex_count()
        });
        started_rx.recv_timeout(PROGRESS).unwrap();
        std::thread::sleep(BLOCKED_CHECK);
        assert!(
            count.try_recv().is_err(),
            "a reader of another thread waits while the unit has written"
        );
        match commit {
            true => graph.commit_unit(mark).unwrap(),
            false => graph.rollback_unit(mark, &reason()).unwrap(),
        }
        let expected = if commit { 7 } else { 6 };
        assert_eq!(count.recv_timeout(PROGRESS).unwrap(), expected);
        assert_eq!(graph.read().vertex_count(), expected);
    }
}

#[test]
fn writers_of_other_threads_wait_for_a_unit_that_only_reads() {
    let graph = modern();
    let mark = graph.begin_unit();
    assert!(
        on_thread({
            let other = graph.clone();
            move || other.try_write().is_none()
        })
        .recv_timeout(PROGRESS)
        .unwrap(),
        "another thread's try_write fails while the unit is open"
    );
    let other = graph.clone();
    let written = on_thread(move || add_person(&other, "after"));
    std::thread::sleep(BLOCKED_CHECK);
    assert!(written.try_recv().is_err(), "the writer waits for the unit");
    let other = graph.clone();
    let second_unit = on_thread(move || {
        let mark = other.begin_unit();
        other.commit_unit(mark)
    });
    std::thread::sleep(BLOCKED_CHECK);
    assert!(
        second_unit.try_recv().is_err(),
        "a second unit waits for the first"
    );
    assert_eq!(graph.read().vertex_count(), 6);
    graph.commit_unit(mark).unwrap();
    written.recv_timeout(PROGRESS).unwrap();
    second_unit.recv_timeout(PROGRESS).unwrap().unwrap();
    assert_eq!(graph.read().vertex_count(), 7);
}

#[test]
fn nested_units_are_savepoints() {
    let graph = modern();
    let outer = graph.begin_unit();
    // A savepoint while the unit has only read: other threads still read.
    let inner = graph.begin_unit();
    graph.commit_unit(inner).unwrap();
    let other = graph.clone();
    assert_eq!(
        on_thread(move || other.read().vertex_count())
            .recv_timeout(PROGRESS)
            .unwrap(),
        6
    );
    add_person(&graph, "outer");
    let inner = graph.begin_unit();
    add_person(&graph, "inner");
    assert_eq!(graph.read().vertex_count(), 8);
    graph.rollback_unit(inner, &reason()).unwrap();
    assert_eq!(
        graph.read().vertex_count(),
        7,
        "only the savepoint is rolled back"
    );
    assert!(graph.holds_unit(), "the outer unit is still held");
    let inner = graph.begin_unit();
    add_person(&graph, "kept");
    let changes = graph.unit_changes(inner).unwrap();
    assert!(changes.data);
    graph.commit_unit(inner).unwrap();
    graph.commit_unit(outer).unwrap();
    assert!(!graph.holds_unit());
    let named = |name: &str| {
        graph
            .read()
            .traversal()
            .v(None)
            .has("name", name)
            .to_list()
            .unwrap()
            .len()
    };
    assert_eq!((named("outer"), named("kept"), named("inner")), (1, 1, 0));
}

#[test]
fn the_unit_carries_commit_metadata_and_a_dry_run_discards() {
    let graph = modern();
    let mark = graph.begin_unit();
    add_person(&graph, "previewed");
    let changes = graph.discard_unit(mark).unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(graph.read().vertex_count(), 6);
    assert!(!graph.holds_unit());

    let mark = graph.begin_unit();
    add_person(&graph, "committed");
    graph
        .commit_unit_with(
            mark,
            graphersal::changes::CommitMetadata::new().with_attribute("who", "test"),
        )
        .unwrap();
    assert_eq!(graph.read().vertex_count(), 7);
    assert_eq!(graph.read().last_commit_seq(), 1);
}

#[test]
fn closing_from_another_thread_or_without_a_unit_fails() {
    let graph = modern();
    let err = graph.commit_unit(Default::default()).unwrap_err();
    assert!(matches!(err, GraphError::UnitNotHeld { .. }), "{err:?}");
    let help = graphersal::error::DiagnosticError::help(&err).unwrap();
    assert!(
        help.contains("Graph::begin_unit") && help.contains("thread"),
        "{help}"
    );
    assert!(graph.unit_changes(Default::default()).is_err());

    let mark = graph.begin_unit();
    add_person(&graph, "mine");
    let other = graph.clone();
    let closed = on_thread(move || {
        (
            other.commit_unit(mark).map_err(|err| err.to_string()),
            other
                .rollback_unit(mark, &reason())
                .map_err(|err| err.to_string()),
            other
                .discard_unit(mark)
                .map(|_| ())
                .map_err(|err| err.to_string()),
        )
    })
    .recv_timeout(PROGRESS)
    .unwrap();
    for result in [closed.0, closed.1, closed.2] {
        let err = result.unwrap_err();
        assert!(err.contains("the current thread holds no unit"), "{err}");
    }
    // Still the owner's, still open.
    assert!(graph.holds_unit());
    graph.rollback_unit(mark, &reason()).unwrap();
    assert_eq!(graph.read().vertex_count(), 6);
}

#[test]
fn try_write_inside_a_unit_never_aliases_and_never_waits() {
    let graph = modern();
    let mark = graph.begin_unit();
    {
        let _view = graph.read();
        assert!(
            graph.try_write().is_none(),
            "a read view of the owner is alive"
        );
    }
    {
        let mut write = graph.try_write().expect("nobody else reads");
        GraphTraversalSource::new(&mut *write)
            .add_v("x")
            .next()
            .unwrap();
        assert!(graph.try_write().is_none(), "a write view is alive");
    }
    assert!(graph.try_write().is_some(), "exclusive already");
    graph.rollback_unit(mark, &reason()).unwrap();
    assert_eq!(graph.read().vertex_count(), 6);
}

#[test]
fn a_write_view_beside_a_read_view_is_refused_by_a_panic() {
    let graph = modern();
    let mark = graph.begin_unit();
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _view = graph.read();
        let _write = graph.write();
    }));
    let message = panicked.unwrap_err();
    let text = message
        .downcast_ref::<String>()
        .cloned()
        .unwrap_or_default();
    assert!(text.contains("engine bug"), "{text}");
    // The views are gone with the unwinding; the unit goes on.
    add_person(&graph, "after");
    graph.rollback_unit(mark, &reason()).unwrap();
    assert_eq!(graph.read().vertex_count(), 6);
}

#[test]
fn the_lock_wait_of_a_unit_counts_in_the_workload_statistics() {
    use graphersal::workload::{WorkloadLevel, WorkloadStats};
    let graph = modern();
    let stats = Arc::new(WorkloadStats::new(WorkloadLevel::Memory));
    assert!(graph.attach_workload_stats(stats.clone()));
    let mark = graph.begin_unit();
    let _ = graph.read().vertex_count();
    add_person(&graph, "x");
    graph.commit_unit(mark).unwrap();
    let report = format!("{:?}", stats.report(i64::MAX / 2));
    assert!(report.contains("write.hold"), "{report}");
}

#[cfg(feature = "script")]
mod script {
    use std::sync::atomic::{AtomicBool, Ordering};

    use graphersal::auth::{AccessRequest, Authorizer, Denied};
    use graphersal::script::{ScriptLimits, eval_value_atomic, eval_value_dry_run};

    use super::*;

    /// An authorizer that, at the first request of a script's `nth` traversal, starts a reader
    /// of `graph` on another thread and waits up to `wait` for it; `read_done` tells whether the
    /// reader finished meanwhile. Allows everything.
    struct ReaderProbe {
        graph: Arc<Graph>,
        nth: usize,
        seen: std::sync::Mutex<usize>,
        wait: Duration,
        read_done: AtomicBool,
        started: AtomicBool,
        reader: std::sync::Mutex<Option<mpsc::Receiver<usize>>>,
    }

    impl ReaderProbe {
        fn new(graph: &Arc<Graph>, nth: usize, wait: Duration) -> Arc<Self> {
            Arc::new(ReaderProbe {
                graph: graph.clone(),
                nth,
                seen: std::sync::Mutex::new(0),
                wait,
                read_done: AtomicBool::new(false),
                started: AtomicBool::new(false),
                reader: std::sync::Mutex::new(None),
            })
        }
    }

    impl Authorizer for ReaderProbe {
        fn authorize(&self, request: &AccessRequest<'_>) -> Result<(), Denied> {
            let _ = request;
            // Every execution starts its requests with a fresh count of its own; the probe
            // counts the calls and fires once.
            let mut seen = self.seen.lock().unwrap();
            *seen += 1;
            if *seen == self.nth && !self.started.swap(true, Ordering::SeqCst) {
                let other = self.graph.clone();
                let reader = on_thread(move || other.read().vertex_count());
                if reader.recv_timeout(self.wait).is_ok() {
                    self.read_done.store(true, Ordering::SeqCst);
                } else {
                    *self.reader.lock().unwrap() = Some(reader);
                }
            }
            Ok(())
        }
    }

    #[test]
    fn a_read_only_atomic_script_does_not_block_a_concurrent_reader() {
        let graph = modern();
        let probe = ReaderProbe::new(&graph, 1, PROGRESS);
        let count = eval_value_atomic(
            graph.clone(),
            r#"g.V().count().next()"#,
            Default::default(),
            &ScriptLimits::default(),
            probe.clone(),
        )
        .unwrap();
        assert_eq!(count.as_int().unwrap(), 6);
        assert!(probe.started.load(Ordering::SeqCst));
        assert!(
            probe.read_done.load(Ordering::SeqCst),
            "the reader finished while the script ran"
        );
    }

    #[test]
    fn a_writing_atomic_script_blocks_readers_from_its_first_write_until_its_commit() {
        let graph = modern();
        // The probe fires at the first request of the script's second traversal, after its
        // write; the request count of the first traversal is found by a dry pass.
        let counter = ReaderProbe::new(&graph, usize::MAX, Duration::ZERO);
        let _ = eval_value_dry_run(
            graph.clone(),
            r#"g.addV("person").iterate();"#,
            Default::default(),
            &ScriptLimits::default(),
            counter.clone(),
        )
        .unwrap();
        let first = *counter.seen.lock().unwrap();
        let probe = ReaderProbe::new(&graph, first + 1, BLOCKED_CHECK);
        let count = eval_value_atomic(
            graph.clone(),
            r#"g.addV("person").iterate(); g.V().count().next()"#,
            Default::default(),
            &ScriptLimits::default(),
            probe.clone(),
        )
        .unwrap();
        assert_eq!(count.as_int().unwrap(), 7);
        assert!(probe.started.load(Ordering::SeqCst));
        assert!(
            !probe.read_done.load(Ordering::SeqCst),
            "the reader waited while the script had written"
        );
        let reader = probe.reader.lock().unwrap().take().unwrap();
        assert_eq!(
            reader.recv_timeout(PROGRESS).unwrap(),
            7,
            "the reader sees the committed state"
        );
    }

    #[test]
    fn a_dry_run_leaves_nothing_and_releases_the_lock() {
        let graph = modern();
        let (count, changes) = eval_value_dry_run(
            graph.clone(),
            r#"g.addV("person").property("name", "ann").next(); g.V().count().next()"#,
            Default::default(),
            &ScriptLimits::default(),
            Arc::new(graphersal::auth::AllowAll),
        )
        .unwrap();
        assert_eq!(count.as_int().unwrap(), 7);
        assert_eq!(changes.len(), 1);
        assert_eq!(graph.read().vertex_count(), 6);
        assert!(!graph.holds_unit());
        let other = graph.clone();
        assert!(
            on_thread(move || other.try_write().is_some())
                .recv_timeout(PROGRESS)
                .unwrap(),
            "the lock is free again"
        );
        // A failing dry run too.
        let failed = eval_value_dry_run(
            graph.clone(),
            r#"g.addV("person").next(); throw "no""#,
            Default::default(),
            &ScriptLimits::default(),
            Arc::new(graphersal::auth::AllowAll),
        );
        assert!(failed.is_err());
        assert_eq!(graph.read().vertex_count(), 6);
        assert!(graph.try_write().is_some());
    }

    #[test]
    fn an_atomic_script_inside_a_held_unit_is_a_savepoint() {
        let graph = modern();
        let mark = graph.begin_unit();
        let failed = eval_value_atomic(
            graph.clone(),
            r#"g.addV("person").next(); throw "no""#,
            Default::default(),
            &ScriptLimits::default(),
            Arc::new(graphersal::auth::AllowAll),
        );
        assert!(failed.is_err());
        let _ = eval_value_atomic(
            graph.clone(),
            r#"g.addV("person").property("name", "kept").next()"#,
            Default::default(),
            &ScriptLimits::default(),
            Arc::new(graphersal::auth::AllowAll),
        )
        .unwrap();
        assert!(graph.holds_unit());
        assert_eq!(graph.read().vertex_count(), 7);
        assert_eq!(graph.read().last_commit_seq(), 0, "nothing committed yet");
        graph.commit_unit(mark).unwrap();
        assert_eq!(graph.read().vertex_count(), 7);
        assert_eq!(graph.read().last_commit_seq(), 1);
    }

    /// An authorizer that panics at the first request of the script's second traversal.
    struct PanicAt {
        nth: usize,
        seen: std::sync::Mutex<usize>,
    }

    impl Authorizer for PanicAt {
        fn authorize(&self, _request: &AccessRequest<'_>) -> Result<(), Denied> {
            let mut seen = self.seen.lock().unwrap();
            *seen += 1;
            if *seen == self.nth {
                drop(seen);
                panic!("injected panic");
            }
            Ok(())
        }
    }

    #[test]
    fn a_panic_in_the_script_rolls_back_and_releases_the_lock() {
        let graph = modern();
        let counter = ReaderProbe::new(&graph, usize::MAX, Duration::ZERO);
        let _ = eval_value_dry_run(
            graph.clone(),
            r#"g.addV("person").iterate();"#,
            Default::default(),
            &ScriptLimits::default(),
            counter.clone(),
        )
        .unwrap();
        let first = *counter.seen.lock().unwrap();
        for dry in [false, true] {
            let authorizer = Arc::new(PanicAt {
                nth: first + 1,
                seen: std::sync::Mutex::new(0),
            });
            let script = r#"g.addV("person").iterate(); g.V().count().next()"#;
            let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match dry {
                false => eval_value_atomic(
                    graph.clone(),
                    script,
                    Default::default(),
                    &ScriptLimits::default(),
                    authorizer.clone(),
                )
                .map(|_| ()),
                true => eval_value_dry_run(
                    graph.clone(),
                    script,
                    Default::default(),
                    &ScriptLimits::default(),
                    authorizer.clone(),
                )
                .map(|_| ()),
            }));
            assert!(panicked.is_err(), "the panic continues to the caller");
            assert!(!graph.holds_unit(), "the unit is closed");
            assert_eq!(graph.read().vertex_count(), 6, "the write is rolled back");
            let other = graph.clone();
            assert!(
                on_thread(move || other.try_write().is_some())
                    .recv_timeout(PROGRESS)
                    .unwrap(),
                "the lock is free again"
            );
            // The graph stays usable.
            let _ = eval_value_atomic(
                graph.clone(),
                r#"g.V().count().next()"#,
                Default::default(),
                &ScriptLimits::default(),
                Arc::new(graphersal::auth::AllowAll),
            )
            .unwrap();
        }
    }
}
