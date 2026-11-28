//! The Store over a `MemDir` (feature `persist`; the backend of the browser playground): the
//! same functionality as on disk (create, reopen, checkpoint, mark, point-in-time views,
//! rollback and the attic, fork into another `MemDir`, verify, packed export, backup), and
//! crash simulations that are simpler in memory (a crash image is `deep_copy`; every cut and
//! flip works on the bytes, no directories are copied).
#![cfg(feature = "persist")]

use graphersal::persist::{
    MemDir, PersistError, RecoveryTarget, RollbackMode, Store, StoreDir, StoreOptions,
};
use graphersal::prelude::*;

use crate::persist_tests::{dump, names, varied_graph};

fn add_person(store: &Store, name: &str) {
    store
        .graph()
        .write()
        .traversal_mut()
        .add_v("person")
        .property("name", name)
        .to_list()
        .unwrap();
}

fn graph_names(store: &Store) -> Vec<String> {
    names(&store.graph().read())
}

fn last_wal(dir: &MemDir) -> String {
    dir.file_names()
        .into_iter()
        .rfind(|n| n.starts_with("wal/"))
        .unwrap()
}

/// `create_with_dir` writes the store `create_with` writes without opening it (nothing is
/// loaded back): it opens later as a cleanly closed store holding the graph, and a directory
/// that is not empty is refused without a change.
#[test]
fn create_with_dir_writes_the_graph_without_opening_the_store() {
    let dir = MemDir::named("memory:closed-create");
    let expected = dump(&varied_graph());
    Store::create_with_dir(&dir, varied_graph(), StoreOptions::new()).unwrap();
    assert!(
        !dir.file_names().iter().any(|n| n.starts_with("wal/")),
        "never opened"
    );
    let store = Store::open(&dir, StoreOptions::new()).unwrap();
    assert!(store.open_report().clean_close);
    assert_eq!(dump(&store.graph().read()), expected);
    add_person(&store, "ann");
    store.close().unwrap();
    let before = dir.file_names();
    assert!(Store::create_with_dir(&dir, varied_graph(), StoreOptions::new()).is_err());
    assert_eq!(dir.file_names(), before, "a refused create changes nothing");
}

#[test]
fn a_store_in_memory_does_everything_a_directory_store_does() {
    let dir = MemDir::named("memory:main");
    let store = Store::create_with(&dir, varied_graph(), StoreOptions::new()).unwrap();
    assert_eq!(store.location(), "memory:main");
    let original = dump(&store.graph().read());
    add_person(&store, "ann");
    store.mark("after-ann").unwrap();
    let snapshot = store.checkpoint(Some("named")).unwrap();
    assert_eq!(snapshot.name, "named");
    add_person(&store, "bob");
    add_person(&store, "cy");
    let at = store.graph().read().last_commit_seq();

    // A second writer on the same directory is refused, like the OS lock on disk.
    assert!(matches!(
        Store::open(&dir, StoreOptions::new()),
        Err(PersistError::Locked { .. })
    ));
    // Listing and a read-only view while the store is open.
    let info = store.info().unwrap();
    assert_eq!(info.location, "memory:main");
    assert_eq!(info.snapshots.len(), 2);
    assert_eq!(info.marks[0].name(), "after-ann");
    assert!(info.snapshot_bytes > 0 && info.wal_bytes > 0);
    let view = store
        .read_only_at(RecoveryTarget::Mark("after-ann".into()))
        .unwrap();
    assert!(names(&view.graph).contains(&"ann".to_string()));
    assert!(!names(&view.graph).contains(&"bob".to_string()));
    let past = Store::open_read_only(&dir, RecoveryTarget::CommitSeq(0)).unwrap();
    assert_eq!(dump(&past.graph), original);

    // Packed export of a stored snapshot.
    let mut packed = Vec::new();
    store
        .export_packed(Some(snapshot.commit_seq), &mut packed)
        .unwrap();
    let loaded = TraversalGraph::read_snapshot(packed.as_slice()).unwrap();
    assert_eq!(loaded.last_commit_seq(), snapshot.commit_seq);

    // Fork into another MemDir: a new lineage from the mark.
    let fork = MemDir::named("memory:fork");
    let forked = store
        .fork(RecoveryTarget::Mark("after-ann".into()), &fork)
        .unwrap();
    assert_eq!(forked.snapshot.commit_seq, snapshot.commit_seq);
    let branch = Store::open(&fork, StoreOptions::new()).unwrap();
    assert_eq!(branch.info().unwrap().lineage.len(), 2);
    add_person(&branch, "dan");
    assert!(graph_names(&branch).contains(&"dan".to_string()));
    assert!(!graph_names(&store).contains(&"dan".to_string()));

    // Roll back in place, the attic keeps the history, restore it.
    let report = store
        .rollback_to(RecoveryTarget::Mark("after-ann".into()), RollbackMode::Move)
        .unwrap();
    assert_eq!(report.previous_commit_seq, at);
    assert!(!graph_names(&store).contains(&"bob".to_string()));
    let id = report.attic.unwrap();
    let moved: Vec<u64> = store
        .attic_changes(&id)
        .unwrap()
        .map(|c| c.unwrap().commit_seq())
        .collect();
    assert_eq!(moved.len(), 2);
    store.attic_can_restore(&id).unwrap();
    assert_eq!(store.attic_restore(&id).unwrap(), at);
    assert!(graph_names(&store).contains(&"cy".to_string()));
    assert!(store.verify().unwrap().is_ok());

    // Backup into a third MemDir opens as the same graph.
    let backup = MemDir::new();
    store.backup(&backup).unwrap();
    let expected = dump(&store.graph().read());
    store.close().unwrap();
    let reopened = Store::open(&dir, StoreOptions::new()).unwrap();
    assert!(reopened.open_report().clean_close);
    assert_eq!(dump(&reopened.graph().read()), expected);
    drop(reopened);
    // A backup is marked: read-only until it is restored.
    assert!(Store::open(&backup, StoreOptions::new()).is_err());
    let read_only = Store::open_backup(&backup, StoreOptions::new()).unwrap();
    assert_eq!(dump(&read_only.graph().read()), expected);
    drop(read_only);
    Store::restore_backup(&backup).unwrap();
    let restored = Store::open(&backup, StoreOptions::new()).unwrap();
    assert_eq!(dump(&restored.graph().read()), expected);
    assert!(dir.total_bytes() > 0);
}

#[test]
fn a_crash_image_in_memory_recovers_from_every_cut_of_the_last_record() {
    let dir = MemDir::new();
    let store = Store::create(&dir, StoreOptions::new()).unwrap();
    add_person(&store, "ann");
    let wal = last_wal(&dir);
    let complete = dir.file_len(&wal).unwrap();
    add_person(&store, "bob");
    let crashed = dir.deep_copy();
    drop(store);
    let full = crashed.file_len(&wal).unwrap();
    for cut in complete..full {
        let attempt = crashed.deep_copy();
        let mut bytes = attempt.file(&wal).unwrap();
        bytes.truncate(cut as usize);
        attempt.put_file(&wal, bytes);
        let store = Store::open(&attempt, StoreOptions::new())
            .unwrap_or_else(|err| panic!("cut at {cut}: {err}"));
        assert_eq!(graph_names(&store), ["ann"], "cut at {cut}");
        assert_eq!(
            store.open_report().torn_tail.is_some(),
            cut > complete,
            "cut at {cut}"
        );
        assert_eq!(attempt.file_len(&wal).unwrap(), complete, "cut at {cut}");
        add_person(&store, "cy");
        drop(store);
        let store = Store::open(&attempt, StoreOptions::new()).unwrap();
        assert_eq!(graph_names(&store), ["ann", "cy"], "cut at {cut}");
    }
}

#[test]
fn every_flipped_bit_of_the_wal_in_memory_is_damage_never_silent() {
    let dir = MemDir::new();
    let store = Store::create(&dir, StoreOptions::new()).unwrap();
    add_person(&store, "ann");
    add_person(&store, "bob");
    store.close().unwrap();
    let wal = last_wal(&dir);
    let len = dir.file_len(&wal).unwrap();
    // Past the 64-byte header: every byte of the two records, one bit each.
    for at in 64..len {
        let attempt = dir.deep_copy();
        let mut bytes = attempt.file(&wal).unwrap();
        bytes[at as usize] ^= 0x10;
        attempt.put_file(&wal, bytes);
        let report = Store::verify_dir(&attempt).unwrap();
        assert!(!report.is_ok(), "flip at {at} went unnoticed");
        let store = Store::open(&attempt, StoreOptions::new()).unwrap();
        assert!(
            store.maintenance().is_some(),
            "flip at {at}: no maintenance mode"
        );
    }
}

#[test]
fn a_sequence_only_commit_keeps_the_raised_sequences_across_reopen_and_checkpoint() {
    let changes = crate::changeset_position_tests::compacted_away_change_set();
    assert!(changes.is_empty());
    let dir = MemDir::named("memory:sequence-only");
    let store = Store::create_with(&dir, TraversalGraph::new(), StoreOptions::new()).unwrap();
    let before = store.graph().read().last_commit_seq();
    store.graph().write().apply_change_set(&changes).unwrap();
    assert_eq!(store.graph().read().last_commit_seq(), before + 1);
    store.close().unwrap();

    // Reopen: the WAL's sequence-only commit raises the sequences again.
    let store = Store::open(&dir, StoreOptions::new()).unwrap();
    {
        let graph = store.graph().read();
        assert_eq!((graph.node_sequence(), graph.edge_sequence()), (2, 1));
        assert_eq!(graph.last_commit_seq(), before + 1);
    }
    // A merge checkpoint over that WAL keeps them in the new snapshot.
    let snapshot = store.checkpoint(None).unwrap();
    assert_eq!(snapshot.commit_seq, before + 1);
    let mut packed = Vec::new();
    store
        .export_packed(Some(snapshot.commit_seq), &mut packed)
        .unwrap();
    let loaded = TraversalGraph::read_snapshot(packed.as_slice()).unwrap();
    assert_eq!((loaded.node_sequence(), loaded.edge_sequence()), (2, 1));
    store.close().unwrap();
    let store = Store::open(&dir, StoreOptions::new()).unwrap();
    {
        let graph = store.graph().read();
        assert_eq!((graph.node_sequence(), graph.edge_sequence()), (2, 1));
    }
    add_person(&store, "ann");
    let ids = store
        .graph()
        .read()
        .traversal()
        .v(None::<()>)
        .id()
        .to_list()
        .unwrap();
    assert_eq!(ids, [ElementProperty::String("3".into())]);
    store.close().unwrap();
}

/// What [`SeenCommits`] saw: the commit sequence numbers it accepted.
type Seen = std::sync::Arc<std::sync::Mutex<Vec<u64>>>;

/// An application's commit hook: records every commit it accepts, vetoes while `veto` is set.
struct SeenCommits {
    seen: Seen,
    veto: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl graphersal::changes::CommitHook for SeenCommits {
    fn before_commit(
        &mut self,
        changes: &graphersal::changes::ChangeSet,
    ) -> Result<(), graphersal::changes::HookError> {
        if self.veto.load(std::sync::atomic::Ordering::Acquire) {
            return Err("vetoed by the application".into());
        }
        self.seen.lock().unwrap().push(changes.commit_seq());
        Ok(())
    }
}

/// A rollback and an attic restore reload the Store's graph IN PLACE (cleared and loaded into
/// the same instance): a commit hook the application registered before them stays registered,
/// sees every later commit, and its veto still keeps a commit out of the WAL (it runs before
/// the journal).
#[test]
fn a_hook_registered_before_a_rollback_keeps_running_after_the_reload() {
    let dir = MemDir::named("memory:hooks");
    let store = Store::create(&dir, StoreOptions::new()).unwrap();
    let seen = Seen::default();
    let veto = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    store
        .graph()
        .write()
        .register_commit_hook(Box::new(SeenCommits {
            seen: seen.clone(),
            veto: veto.clone(),
        }))
        .unwrap();
    add_person(&store, "ann");
    store.mark("after-ann").unwrap();
    add_person(&store, "bob");
    assert_eq!(*seen.lock().unwrap(), [1, 2]);

    // Rollback, then restore: the hook sees the commit after both.
    let report = store
        .rollback_to(RecoveryTarget::Mark("after-ann".into()), RollbackMode::Move)
        .unwrap();
    assert_eq!(graph_names(&store), ["ann"]);
    store.attic_restore(&report.attic.unwrap()).unwrap();
    assert_eq!(graph_names(&store), ["ann", "bob"]);
    add_person(&store, "cy");
    assert_eq!(*seen.lock().unwrap(), [1, 2, 3]);

    // Rollback again and commit on the new lineage.
    store
        .rollback_to(RecoveryTarget::Mark("after-ann".into()), RollbackMode::Move)
        .unwrap();
    add_person(&store, "dan");
    assert_eq!(*seen.lock().unwrap(), [1, 2, 3, 2]);

    // The hook's veto still rolls the commit back before the journal writes it.
    veto.store(true, std::sync::atomic::Ordering::Release);
    let refused = store
        .graph()
        .write()
        .traversal_mut()
        .add_v("person")
        .property("name", "eve")
        .to_list();
    assert!(refused.is_err());
    veto.store(false, std::sync::atomic::Ordering::Release);
    assert_eq!(graph_names(&store), ["ann", "dan"]);
    store.close().unwrap();

    let reopened = Store::open(&dir, StoreOptions::new()).unwrap();
    assert_eq!(graph_names(&reopened), ["ann", "dan"]);
    assert!(reopened.verify().unwrap().is_ok());
    reopened.close().unwrap();
}

#[test]
fn a_hook_panicking_before_commit_writes_nothing_to_the_store() {
    use graphersal::changes::{ChangeSet, CommitHook, HookError};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct PanicBeforeCommit(Arc<AtomicBool>);
    impl CommitHook for PanicBeforeCommit {
        fn before_commit(&mut self, _: &ChangeSet) -> Result<(), HookError> {
            if self.0.load(Ordering::SeqCst) {
                panic!("a hook panics");
            }
            Ok(())
        }
    }

    let dir = MemDir::named("memory:hook-panic");
    let store = Store::create(&dir, StoreOptions::new()).unwrap();
    let panic = Arc::new(AtomicBool::new(false));
    store
        .graph()
        .write()
        .register_commit_hook(Box::new(PanicBeforeCommit(panic.clone())))
        .unwrap();
    add_person(&store, "ann");
    panic.store(true, Ordering::SeqCst);
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        add_person(&store, "lost");
    }));
    assert!(outcome.is_err());
    assert_eq!(graph_names(&store), ["ann"]);
    panic.store(false, Ordering::SeqCst);
    add_person(&store, "bob");
    assert_eq!(store.graph().read().last_commit_seq(), 2);
    store.close().unwrap();

    // The WAL holds exactly the two committed units.
    let store = Store::open(&dir, StoreOptions::new()).unwrap();
    assert_eq!(graph_names(&store), ["ann", "bob"]);
    assert_eq!(store.graph().read().last_commit_seq(), 2);
    store.close().unwrap();
}
