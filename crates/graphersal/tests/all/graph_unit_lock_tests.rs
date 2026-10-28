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
    let mark = graph.begin_unit().unwrap();
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
        let mark = graph.begin_unit().unwrap();
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
    let mark = graph.begin_unit().unwrap();
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
        let mark = other.begin_unit().unwrap();
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
    let outer = graph.begin_unit().unwrap();
    // A savepoint while the unit has only read: other threads still read.
    let inner = graph.begin_unit().unwrap();
    graph.commit_unit(inner).unwrap();
    let other = graph.clone();
    assert_eq!(
        on_thread(move || other.read().vertex_count())
            .recv_timeout(PROGRESS)
            .unwrap(),
        6
    );
    add_person(&graph, "outer");
    let inner = graph.begin_unit().unwrap();
    add_person(&graph, "inner");
    assert_eq!(graph.read().vertex_count(), 8);
    graph.rollback_unit(inner, &reason()).unwrap();
    assert_eq!(
        graph.read().vertex_count(),
        7,
        "only the savepoint is rolled back"
    );
    assert!(graph.holds_unit(), "the outer unit is still held");
    let inner = graph.begin_unit().unwrap();
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
    let mark = graph.begin_unit().unwrap();
    add_person(&graph, "previewed");
    let changes = graph.discard_unit(mark).unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(graph.read().vertex_count(), 6);
    assert!(!graph.holds_unit());

    let mark = graph.begin_unit().unwrap();
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

    let mark = graph.begin_unit().unwrap();
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
    let mark = graph.begin_unit().unwrap();
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
    let mark = graph.begin_unit().unwrap();
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _view = graph.read();
        let _write = graph.write();
    }));
    let message = panicked.unwrap_err();
    let text = message
        .downcast_ref::<String>()
        .cloned()
        .unwrap_or_default();
    assert!(
        text.contains("already holds another view of this graph"),
        "{text}"
    );
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
    let mark = graph.begin_unit().unwrap();
    let _ = graph.read().vertex_count();
    add_person(&graph, "x");
    graph.commit_unit(mark).unwrap();
    let report = format!("{:?}", stats.report(i64::MAX / 2));
    assert!(report.contains("write.hold"), "{report}");
}

/// Starts a reader on another thread that holds a read guard of `graph` until the returned
/// sender is dropped (or sends); returns once the reader holds its guard.
fn reader_in_flight(graph: &Arc<Graph>) -> (mpsc::Sender<()>, std::thread::JoinHandle<usize>) {
    let (holding_tx, holding_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let other = graph.clone();
    let reader = std::thread::spawn(move || {
        let guard = other.read();
        holding_tx.send(()).unwrap();
        let _ = release_rx.recv_timeout(PROGRESS);
        guard.vertex_count()
    });
    holding_rx.recv_timeout(PROGRESS).unwrap();
    (release_tx, reader)
}

/// Opening a unit and closing one that never wrote take no exclusive moment: they finish while
/// another thread's reader still holds its read guard (a brief upgrade would wait for it).
#[test]
fn begin_and_the_close_of_a_read_only_unit_never_wait_for_a_reader_in_flight() {
    let graph = modern();
    let (release, reader) = reader_in_flight(&graph);
    let owner = graph.clone();
    let done = on_thread(move || {
        let mut closed = Vec::new();
        // Commit, roll back, nested levels: none of them waits for the reader.
        let mark = owner.begin_unit().unwrap();
        assert_eq!(owner.read().vertex_count(), 6);
        let inner = owner.begin_unit().unwrap();
        assert!(owner.unit_changes(inner).unwrap() == Default::default());
        closed.push(owner.rollback_unit(inner, &reason()).is_ok());
        let inner = owner.begin_unit().unwrap();
        closed.push(owner.commit_unit(inner).is_ok());
        closed.push(owner.commit_unit(mark).is_ok());
        let mark = owner.begin_unit().unwrap();
        closed.push(owner.rollback_unit(mark, &reason()).is_ok());
        closed.push(!owner.holds_unit());
        closed
    });
    // The reader still holds its guard: the owner must finish without it being released.
    assert_eq!(
        done.recv_timeout(PROGRESS)
            .expect("begin/close waited for the reader in flight"),
        [true; 5]
    );
    drop(release);
    assert_eq!(reader.join().unwrap(), 6);
    // The lock is free again.
    assert!(graph.try_write().is_some());
}

/// A read-only unit that commits sends no hooks and moves no commit sequence.
#[test]
fn committing_a_unit_that_never_wrote_sends_no_hooks() {
    use graphersal::changes::{ChangeSet, CommitHook, HookError};
    struct Count(Arc<std::sync::atomic::AtomicUsize>);
    impl CommitHook for Count {
        fn before_commit(&mut self, _: &ChangeSet) -> Result<(), HookError> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
    }
    let graph = modern();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    graph
        .write()
        .register_commit_hook(Box::new(Count(calls.clone())))
        .unwrap();
    let mark = graph.begin_unit().unwrap();
    let _ = graph.read().vertex_count();
    graph
        .commit_unit_with(
            mark,
            graphersal::changes::CommitMetadata::new().with_attribute("who", "test"),
        )
        .unwrap();
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(graph.read().last_commit_seq(), 0);
    let mark = graph.begin_unit().unwrap();
    add_person(&graph, "x");
    graph.commit_unit(mark).unwrap();
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}

/// A dry run that never wrote still gets the storage's (empty) change set.
#[test]
fn discarding_a_unit_that_never_wrote_returns_an_empty_change_set() {
    let graph = modern();
    let mark = graph.begin_unit().unwrap();
    let changes = graph.discard_unit(mark).unwrap();
    assert_eq!(changes.len(), 0);
    assert!(!graph.holds_unit());
    assert!(graph.try_write().is_some());
}

/// Opening, nesting and closing while a view of the graph is alive on the owning thread are
/// errors (nothing opened or closed), never panics; the unit goes on once the view is gone.
#[test]
fn a_view_alive_makes_begin_and_the_closes_errors_not_panics() {
    let graph = modern();
    let mark = graph.begin_unit().unwrap();
    {
        let _view = graph.read();
        let err = graph.begin_unit().unwrap_err();
        assert!(matches!(err, GraphError::GraphViewHeld { .. }), "{err:?}");
        let err = graph.commit_unit(mark).unwrap_err();
        assert!(matches!(err, GraphError::GraphViewHeld { .. }), "{err:?}");
        let err = graph.rollback_unit(mark, &reason()).unwrap_err();
        assert!(matches!(err, GraphError::GraphViewHeld { .. }), "{err:?}");
        assert!(graph.discard_unit(mark).is_err());
        assert!(graph.holds_unit(), "nothing was closed");
    }
    add_person(&graph, "written");
    {
        let _write = graph.write();
        let err = graph.begin_unit().unwrap_err();
        assert!(matches!(err, GraphError::GraphViewHeld { .. }), "{err:?}");
        assert!(graph.commit_unit(mark).is_err());
    }
    // The same after the unit wrote: a nested unit opens on the storage now.
    let inner = graph.begin_unit().unwrap();
    add_person(&graph, "inner");
    graph.rollback_unit(inner, &reason()).unwrap();
    graph.commit_unit(mark).unwrap();
    assert_eq!(graph.read().vertex_count(), 7);
}

/// The checked views report what `read()`/`write()` would panic on.
#[test]
fn the_checked_views_report_an_aliasing_view_as_an_error() {
    let graph = modern();
    // Outside a unit they lock like read()/write().
    assert_eq!(graph.read_checked().unwrap().vertex_count(), 6);
    drop(graph.write_checked().unwrap());
    let mark = graph.begin_unit().unwrap();
    {
        let read = graph.read_checked().unwrap();
        let second = graph.read_checked().unwrap();
        assert_eq!((read.vertex_count(), second.vertex_count()), (6, 6));
        let err = graph.write_checked().unwrap_err();
        assert!(matches!(err, GraphError::GraphViewHeld { .. }), "{err:?}");
        assert!(err.to_string().contains("write the graph"), "{err}");
    }
    {
        let _write = graph.write_checked().unwrap();
        let err = graph.read_checked().unwrap_err();
        assert!(err.to_string().contains("read the graph"), "{err}");
        assert!(graph.write_checked().is_err());
        assert!(graph.unit_changes(mark).is_err());
    }
    graph.rollback_unit(mark, &reason()).unwrap();
}

/// `try_read` never blocks and never panics.
#[test]
fn try_read_never_blocks_and_never_panics() {
    let graph = modern();
    assert_eq!(graph.try_read().unwrap().vertex_count(), 6);
    {
        let _write = graph.write();
        assert!(
            graph.try_read().is_none(),
            "this thread holds the write lock"
        );
        let other = graph.clone();
        assert!(
            on_thread(move || other.try_read().is_none())
                .recv_timeout(PROGRESS)
                .unwrap(),
            "another thread holds the write lock"
        );
    }
    let mark = graph.begin_unit().unwrap();
    let other = graph.clone();
    assert_eq!(
        on_thread(move || other.try_read().map(|g| g.vertex_count()))
            .recv_timeout(PROGRESS)
            .unwrap(),
        Some(6),
        "a unit that has only read lets other threads read"
    );
    {
        let _view = graph.read();
        assert_eq!(graph.try_read().map(|g| g.vertex_count()), Some(6));
    }
    {
        let _write = graph.write();
        assert!(
            graph.try_read().is_none(),
            "a write view of the owner is alive"
        );
    }
    let other = graph.clone();
    assert!(
        on_thread(move || other.try_read().is_none())
            .recv_timeout(PROGRESS)
            .unwrap(),
        "a unit that has written keeps other threads' readers out"
    );
    graph.rollback_unit(mark, &reason()).unwrap();
    assert!(graph.try_read().is_some());
}

/// `begin_unit_at` opens the outermost unit at the given time; `next_unit_started_at` is the
/// time the next unit would get (never earlier than the last one).
#[test]
fn the_storage_opens_a_unit_at_the_time_it_is_given() {
    let mut storage = TraversalGraph::tinkerpop_modern();
    let at = storage.next_unit_started_at().unwrap().unwrap();
    let mark = storage.begin_unit_at(at).unwrap();
    assert_eq!(storage.unit_started_at().unwrap(), Some(at));
    let inner = storage.begin_unit_at(at + 1_000_000_000).unwrap();
    assert_eq!(
        storage.unit_started_at().unwrap(),
        Some(at),
        "a nested unit keeps it"
    );
    storage.commit_unit(inner).unwrap();
    storage.commit_unit(mark).unwrap();
    assert_eq!(storage.unit_started_at().unwrap(), None);
    assert!(storage.next_unit_started_at().unwrap().unwrap() >= at);
    // A time before the floor is raised to it.
    let mark = storage.begin_unit_at(at - 1_000_000_000).unwrap();
    assert!(storage.unit_started_at().unwrap().unwrap() >= at);
    storage.rollback_unit(mark, &reason()).unwrap();
}

/// The unit's clock is pinned when the unit opens: the owner's read view reports it before the
/// first write, and the elements the unit creates store it (`defaultFunction` `now`).
#[test]
fn the_unit_clock_is_one_value_before_and_after_the_first_write() {
    let graph = modern();
    let schema = GraphSchema::from_json(
        r#"{"mode": "open", "vertices": {"event": {"schema": {"type": "object",
            "properties": {"at": {"type": "string", "format": "date-time",
                                  "defaultFunction": "now"}}}}}}"#,
    )
    .unwrap();
    GraphTraversalSource::new(&mut *graph.write())
        .set_schema(schema)
        .unwrap();
    let mark = graph.begin_unit().unwrap();
    let before = graph
        .read()
        .unit_started_at()
        .unwrap()
        .expect("pinned at the begin");
    // Other threads see no unit of theirs.
    let other = graph.clone();
    assert_eq!(
        on_thread(move || other.read().unit_started_at().unwrap())
            .recv_timeout(PROGRESS)
            .unwrap(),
        None
    );
    std::thread::sleep(Duration::from_millis(5));
    GraphTraversalSource::new(&mut *graph.write())
        .add_v("event")
        .iterate()
        .unwrap();
    assert_eq!(graph.read().unit_started_at().unwrap(), Some(before));
    let stored = graph
        .read()
        .traversal()
        .v(None)
        .has_label("event")
        .values("at")
        .next()
        .unwrap();
    let expected = graphersal::temporal::DateTime::from_nanos(before, 0).unwrap();
    assert_eq!(stored, Some(ElementProperty::Temporal(expected.into())));
    graph.commit_unit(mark).unwrap();
    assert_eq!(graph.read().unit_started_at().unwrap(), None);
}

/// The GQL clock read through the owner's views is the same before and after the unit's first
/// write.
#[cfg(feature = "gql")]
#[test]
fn the_gql_clock_is_the_same_before_and_after_the_first_write() {
    let graph = modern();
    let clock = |graph: &Graph| {
        GraphTraversalSource::new(&*graph.read())
            .gql("RETURN datetime() AS t")
            .to_list()
            .unwrap()
    };
    let mark = graph.begin_unit().unwrap();
    let before = clock(&graph);
    std::thread::sleep(Duration::from_millis(5));
    GraphTraversalSource::new(&mut *graph.write())
        .gql("INSERT (:x {n: 1})")
        .iterate()
        .unwrap();
    let after = clock(&graph);
    assert_eq!(before, after);
    graph.rollback_unit(mark, &reason()).unwrap();
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
        let mark = graph.begin_unit().unwrap();
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

    /// A commit hook that runs queries on the graph it is committing, on the owning thread
    /// (inside the close of a held unit): each query fails with `GraphViewHeld` instead of
    /// panicking (or deadlocking), and the commit itself goes through.
    #[test]
    fn a_commit_hook_querying_its_own_graph_gets_an_error_not_a_panic() {
        use graphersal::changes::{ChangeSet, CommitHook, HookError};
        use graphersal::script::eval_value;

        struct QueryingHook {
            graph: std::sync::Weak<Graph>,
            seen: Arc<std::sync::Mutex<Vec<String>>>,
        }
        impl CommitHook for QueryingHook {
            fn before_commit(&mut self, _: &ChangeSet) -> Result<(), HookError> {
                let Some(graph) = self.graph.upgrade() else {
                    return Ok(());
                };
                let read = eval_value(graph.clone(), "g.V().count().next()")
                    .map(|value| value.to_string())
                    .map_err(|err| err.to_string());
                let unit = eval_value_atomic(
                    graph.clone(),
                    r#"g.addV("person").iterate(); 1"#,
                    Default::default(),
                    &ScriptLimits::default(),
                    Arc::new(graphersal::auth::AllowAll),
                )
                .map(|value| value.to_string())
                .map_err(|err| err.to_string());
                let mut seen = self.seen.lock().unwrap();
                seen.push(format!("{read:?}"));
                seen.push(format!("{unit:?}"));
                Ok(())
            }
        }

        let graph = modern();
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        graph
            .write()
            .register_commit_hook(Box::new(QueryingHook {
                graph: Arc::downgrade(&graph),
                seen: seen.clone(),
            }))
            .unwrap();
        // The unit is a whole script, as in the playground.
        let value = eval_value_atomic(
            graph.clone(),
            r#"g.addV("person").property("name", "new").iterate(); g.V().count().next()"#,
            Default::default(),
            &ScriptLimits::default(),
            Arc::new(graphersal::auth::AllowAll),
        )
        .unwrap();
        assert_eq!(value.as_int().unwrap(), 7);
        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 2, "{seen:?}");
        for result in &seen {
            assert!(result.starts_with("Err("), "{result}");
            assert!(
                result.contains("already holds another view of this graph"),
                "{result}"
            );
        }
        assert!(!graph.holds_unit());
        assert_eq!(graph.read().vertex_count(), 7, "the commit went through");
        assert_eq!(graph.read().last_commit_seq(), 1);
    }

    /// A script inside a held unit that has not written yet reads the unit's pinned time
    /// through every terminal (the script layer pins its traversals to it).
    #[test]
    fn a_script_in_a_held_unit_reads_the_pinned_clock_before_the_first_write() {
        let graph = modern();
        let mark = graph.begin_unit().unwrap();
        let pinned = graph.read().unit_started_at().unwrap().unwrap();
        let read = eval_value_atomic(
            graph.clone(),
            r#"g.V().count().next()"#,
            Default::default(),
            &ScriptLimits::default(),
            Arc::new(graphersal::auth::AllowAll),
        )
        .unwrap();
        assert_eq!(read.as_int().unwrap(), 6);
        assert_eq!(graph.read().unit_started_at().unwrap(), Some(pinned));
        add_person(&graph, "x");
        assert_eq!(graph.read().unit_started_at().unwrap(), Some(pinned));
        graph.commit_unit(mark).unwrap();
    }
}

/// The pinned unit clock of a held unit on a graph whose storage wraps a `TraversalGraph`
/// (boxed, or inline in a foreign storage): the owner's read view reports it before the first
/// write, the elements the unit creates store it, other threads see none (the same checks as
/// `the_unit_clock_is_one_value_before_and_after_the_first_write`).
fn assert_the_unit_clock_is_pinned<S>(graph: Arc<Graph<S>>)
where
    S: GraphStorage + Send + Sync + 'static,
{
    let schema = GraphSchema::from_json(
        r#"{"mode": "open", "vertices": {"event": {"schema": {"type": "object",
            "properties": {"at": {"type": "string", "format": "date-time",
                                  "defaultFunction": "now"}}}}}}"#,
    )
    .unwrap();
    GraphTraversalSource::new(&mut *graph.write())
        .set_schema(schema)
        .unwrap();
    let mark = graph.begin_unit().unwrap();
    let before = graph
        .read()
        .unit_started_at()
        .unwrap()
        .expect("pinned at the begin");
    let other = Arc::clone(&graph);
    assert_eq!(
        on_thread(move || other.read().unit_started_at().unwrap())
            .recv_timeout(PROGRESS)
            .unwrap(),
        None
    );
    std::thread::sleep(Duration::from_millis(5));
    GraphTraversalSource::new(&mut *graph.write())
        .add_v("event")
        .iterate()
        .unwrap();
    assert_eq!(graph.read().unit_started_at().unwrap(), Some(before));
    let stored = GraphTraversalSource::new(&*graph.read())
        .v(None)
        .has_label("event")
        .values("at")
        .next()
        .unwrap();
    let expected = graphersal::temporal::DateTime::from_nanos(before, 0).unwrap();
    assert_eq!(stored, Some(ElementProperty::Temporal(expected.into())));
    graph.commit_unit(mark).unwrap();
    assert_eq!(graph.read().unit_started_at().unwrap(), None);
}

#[test]
fn the_unit_clock_is_pinned_for_a_boxed_storage() {
    assert_the_unit_clock_is_pinned(Arc::new(Graph::new(Box::new(
        TraversalGraph::tinkerpop_modern(),
    ))));
}

#[test]
fn the_unit_clock_is_pinned_for_a_forwarding_storage() {
    assert_the_unit_clock_is_pinned(Arc::new(Graph::new(crate::forwarding::Forwarding::new(
        TraversalGraph::tinkerpop_modern(),
    ))));
}
