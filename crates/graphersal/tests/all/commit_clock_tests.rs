//! The instance clock (`graphersal::storage::CommitClock`): commit, mark and checkpoint times of
//! one instance share one monotonic clock (`max(now, floor)`), with or without commit hooks;
//! the floor survives a reload in place and comes back at an open from the latest time in the
//! files (marks included). A system clock "stepping back" is simulated by raising the floor an
//! hour into the future: the system clock is then an hour behind it.

use std::sync::{Arc, Mutex};

use graphersal::changes::{ChangeSet, CommitHook, HookError};
use graphersal::prelude::*;
use graphersal::storage::{CommitClock, GraphStorage, Transactional};

const HOUR_US: i64 = 3_600_000_000;

fn clock_of<S: GraphStorage>(storage: &S) -> CommitClock {
    storage
        .commit_clock()
        .unwrap()
        .expect("TraversalGraph hands out its clock")
}

fn wall_clock_us() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_micros() as i64
}

fn add_one(graph: &mut TraversalGraph) {
    graph
        .transaction(|g| {
            g.insert_vertex(None, vec!["person".into()], PropertyMap::default())
                .map(|_| ())
        })
        .unwrap();
}

struct Times(Arc<Mutex<Vec<i64>>>);

impl CommitHook for Times {
    fn before_commit(&mut self, changes: &ChangeSet) -> Result<(), HookError> {
        self.0.lock().unwrap().push(changes.committed_at());
        Ok(())
    }
}

#[test]
fn commit_times_are_monotonic_with_a_clock_stepping_back() {
    let mut graph = TraversalGraph::new();
    let times = Arc::new(Mutex::new(Vec::new()));
    graph
        .register_commit_hook(Box::new(Times(times.clone())))
        .unwrap();
    add_one(&mut graph);
    let first = graph.last_committed_at();
    assert!(first > 0);

    let future = wall_clock_us() + HOUR_US;
    clock_of(&graph).raise(future);
    add_one(&mut graph);
    add_one(&mut graph);
    let times = times.lock().unwrap().clone();
    assert_eq!(
        times,
        [first, future, future],
        "never before the floor, equal allowed"
    );
    assert_eq!(graph.last_committed_at(), future);
    // A unit opened now never starts before the floor either (`defaultFunction` `now`).
    let next = graph.next_unit_started_at().unwrap().unwrap();
    assert!(next >= i128::from(future) * 1000, "{next}");
}

#[test]
fn a_store_without_hooks_still_uses_the_clock() {
    let mut graph = TraversalGraph::new();
    let future = wall_clock_us() + HOUR_US;
    clock_of(&graph).raise(future);
    add_one(&mut graph);
    assert_eq!(graph.last_committed_at(), future);
    // The clock handed out is the one the storage times with (clones share the floor).
    assert_eq!(clock_of(&graph).floor(), future);
}

#[test]
fn a_shared_graph_and_its_wrappers_hand_out_the_same_clock() {
    let graph = Graph::new(TraversalGraph::new());
    let future = wall_clock_us() + HOUR_US;
    clock_of(&*graph.read()).raise(future);
    graph
        .write()
        .traversal_mut()
        .add_v("person")
        .to_list()
        .unwrap();
    assert_eq!(graph.read().last_committed_at(), future);
    let boxed = Box::new(TraversalGraph::new());
    clock_of(&boxed).raise(future);
    assert_eq!(clock_of(&*boxed).floor(), future);
    let shared = Arc::new(TraversalGraph::new());
    clock_of(&shared).raise(future);
    assert_eq!(clock_of(&*shared).floor(), future);
}

#[cfg(feature = "persist")]
mod persisted {
    use super::*;
    use graphersal::persist::{
        Durability, Journal, MemDir, Recovery, RecoveryTarget, RollbackMode, SharedBuffer, Store,
        StoreOptions,
    };

    fn options() -> StoreOptions {
        StoreOptions::new()
            .with_checkpoint_after_bytes(0)
            .with_checkpoint_on_close(false)
    }

    fn add_person(store: &Store) {
        store
            .graph()
            .write()
            .traversal_mut()
            .add_v("person")
            .to_list()
            .unwrap();
    }

    fn store_clock(store: &Store) -> CommitClock {
        clock_of(&*store.graph().read())
    }

    #[test]
    fn a_mark_never_precedes_its_commit() {
        let mut graph = TraversalGraph::new();
        let wal = SharedBuffer::new();
        Journal::new(wal.clone(), Durability::EveryCommit)
            .attach(&mut graph)
            .unwrap();
        add_one(&mut graph);
        let mark = GraphStorage::mark(&mut graph, "after-a").unwrap();
        assert!(mark.time() >= graph.last_committed_at());

        let future = wall_clock_us() + HOUR_US;
        clock_of(&graph).raise(future);
        let mark = GraphStorage::mark(&mut graph, "future").unwrap();
        assert_eq!(mark.time(), future);
        add_one(&mut graph);
        assert!(graph.last_committed_at() >= mark.time());

        // A recovery raises the recovered graph's clock to the latest time it read: the
        // marks included, also when the last commit is earlier than one of them.
        let wal = wal.contents();
        let recovered = Recovery::new().journal(wal.as_slice()).run().unwrap();
        assert!(recovered.report.clock_floor >= future);
        assert!(clock_of(&recovered.graph).floor() >= future);
        // A target stops the reading: the floor is what was read up to it (a Store's view
        // takes the Store's instance floor instead).
        let first = Recovery::new()
            .journal(wal.as_slice())
            .target(RecoveryTarget::CommitSeq(1))
            .run()
            .unwrap();
        assert!(first.report.clock_floor < future);
        assert_eq!(first.report.clock_floor, first.report.committed_at);
    }

    #[test]
    fn the_floor_comes_back_from_the_wal_tail_marks_included() {
        let dir = MemDir::new();
        let store = Store::create(&dir, options()).unwrap();
        add_person(&store);
        let committed = store.graph().read().last_committed_at();
        let future = wall_clock_us() + HOUR_US;
        assert!(committed < future);
        // Only the mark carries the future time: the last commit is earlier.
        store_clock(&store).raise(future);
        let mark = store.mark("future").unwrap();
        assert_eq!(mark.time(), future);
        store.close().unwrap();

        let store = Store::open(&dir, options()).unwrap();
        assert!(store_clock(&store).floor() >= future);
        add_person(&store);
        assert!(store.graph().read().last_committed_at() >= future);
        store.close().unwrap();
    }

    #[test]
    fn a_reload_in_place_keeps_the_floor() {
        let dir = MemDir::new();
        let store = Store::create(&dir, options()).unwrap();
        add_person(&store);
        add_person(&store);
        let clock = store_clock(&store);
        let future = wall_clock_us() + HOUR_US;
        clock.raise(future);
        store
            .rollback_to(RecoveryTarget::CommitSeq(1), RollbackMode::Delete)
            .unwrap();
        // The same clock after the reload, its floor untouched.
        assert!(store_clock(&store).floor() >= future);
        add_person(&store);
        assert!(store.graph().read().last_committed_at() >= future);
        // A view of the past keeps the instance floor, not the target's commit time.
        let view = store.read_only_at(RecoveryTarget::CommitSeq(1)).unwrap();
        assert!(clock_of(&view.graph).floor() >= future);
        store.close().unwrap();
    }

    #[test]
    fn a_checkpoint_record_is_timed_by_the_instance_clock() {
        let dir = MemDir::new();
        let store = Store::create(&dir, options()).unwrap();
        add_person(&store);
        let future = wall_clock_us() + HOUR_US;
        store_clock(&store).raise(future);
        store.checkpoint(None).unwrap();
        // The checkpoint record went into the WAL with the instance clock's time.
        let times: Vec<i64> = wal_checkpoint_times(&store);
        assert!(times.iter().any(|t| *t >= future), "{times:?}");
        store.close().unwrap();
    }

    /// The times of every checkpoint record in the store's WAL segments.
    fn wal_checkpoint_times(store: &Store) -> Vec<i64> {
        use graphersal::persist::{EntryKind, JournalRecord, read_journal};
        let dir = store.dir();
        let mut times = Vec::new();
        let mut names: Vec<String> = dir
            .list("wal")
            .unwrap()
            .into_iter()
            .filter(|entry| entry.kind == EntryKind::File)
            .map(|entry| entry.name)
            .collect();
        names.sort();
        for name in names {
            let bytes = dir.read(&format!("wal/{name}"), u64::MAX).unwrap();
            for record in read_journal(bytes.as_slice()).unwrap() {
                if let JournalRecord::Checkpoint { time, .. } = record.unwrap() {
                    times.push(time);
                }
            }
        }
        times
    }
}
