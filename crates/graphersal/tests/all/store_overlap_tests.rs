//! Issue #27: an operation that reads the store's files for damage (a backup, a verification)
//! beside one that moves them (a rollback, the attic's restore and remove). What moves must never
//! be taken for damage: what moves is refused while files are read, what reads waits while files
//! move, nothing freezes.

use std::sync::Arc;

use graphersal::persist::{
    FsDir, LockKind, MemDir, PersistError, RecoveryTarget, RollbackMode, Store, StoreDir,
    StoreFailure, StoreOptions,
};

fn commits(store: &Store, n: usize) {
    for i in 0..n {
        store
            .graph()
            .write()
            .traversal_mut()
            .add_v("x")
            .property("i", i as i64)
            .to_list()
            .unwrap();
    }
}

fn store_with_history(store: &Store) -> u64 {
    commits(store, 20);
    store.checkpoint(None).unwrap();
    let target = store.graph().read().last_commit_seq();
    commits(store, 20);
    store.checkpoint(None).unwrap();
    commits(store, 20);
    target
}

fn cause_of(err: &PersistError) -> Option<StoreFailure> {
    match err {
        PersistError::Store { cause, .. } => Some(*cause),
        _ => None,
    }
}

/// The race itself: whatever the order, what reads succeeds, the rollback runs or is refused, and
/// the store stays writable.
fn overlap_never_freezes(
    read: impl Fn(&Store, u32) -> Result<(), PersistError> + Send + Sync + 'static,
) {
    let read = Arc::new(read);
    let dir = MemDir::new();
    let store = Arc::new(Store::create(&dir, StoreOptions::new()).unwrap());
    for round in 0..30 {
        let target = store_with_history(&store);
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let reader = {
            let (store, read, barrier) = (store.clone(), read.clone(), barrier.clone());
            std::thread::spawn(move || {
                barrier.wait();
                read(&store, round)
            })
        };
        barrier.wait();
        // Each is free to come first.
        for _ in 0..(round % 4) * 500 {
            std::hint::spin_loop();
        }
        let rollback = store.rollback_to(RecoveryTarget::CommitSeq(target), RollbackMode::Move);
        let read_result = reader.join().unwrap();
        assert!(
            !store.is_frozen_by_damage(),
            "round {round}: {:?}",
            store.damage_found().unwrap()
        );
        read_result.unwrap_or_else(|err| panic!("round {round}: {err}"));
        if let Err(err) = &rollback {
            assert_eq!(cause_of(err), Some(StoreFailure::FilesInUse), "{err}");
        }
    }
    assert!(store.verify().unwrap().is_ok());
    commits(&store, 1);
}

#[test]
fn a_backup_beside_a_rollback_never_freezes_the_store() {
    overlap_never_freezes(|store, _| store.backup(MemDir::new()).map(|_| ()));
}

#[test]
fn a_verification_beside_a_rollback_never_freezes_the_store() {
    overlap_never_freezes(|store, _| {
        let report = store.verify()?;
        assert!(report.is_ok(), "{:?}", report.problems);
        Ok(())
    });
}

/// Every operation that moves files is refused at once while a backup holds the pin, and changes
/// nothing; the store goes on. (The pin is what a running backup holds, in this or another
/// process.)
#[test]
fn what_moves_files_is_refused_while_a_backup_copies_them() {
    let dir = MemDir::new();
    let store = Store::create(&dir, StoreOptions::new()).unwrap();
    let target = store_with_history(&store);
    store
        .rollback_to(RecoveryTarget::CommitSeq(target), RollbackMode::Move)
        .unwrap();
    let entry = store.attic_list().unwrap()[0].id.clone();
    commits(&store, 0);
    let last = store.graph().read().last_commit_seq();
    let before = store.graph().read().vertex_count();

    let pin = dir.lock(LockKind::BackupShared).unwrap().unwrap();
    let refusals = [
        (
            "roll",
            store
                .rollback_to(RecoveryTarget::CommitSeq(1), RollbackMode::Move)
                .map(|_| ()),
        ),
        ("restore", store.attic_restore(&entry).map(|_| ())),
        ("remove", store.attic_remove(&entry).map(|_| ())),
    ];
    for (verb, result) in refusals {
        let err = result.expect_err(verb);
        assert_eq!(cause_of(&err), Some(StoreFailure::FilesInUse), "{err}");
        let text = err.to_string();
        assert!(text.contains(verb), "{text}");
        assert!(!text.contains("prune"), "{text}");
    }
    assert_eq!(store.graph().read().last_commit_seq(), last);
    assert_eq!(store.graph().read().vertex_count(), before);
    assert_eq!(store.attic_list().unwrap().len(), 1);
    // A backup and a verification go on beside a backup.
    store.backup(MemDir::new()).unwrap();
    assert!(store.verify().unwrap().is_ok());
    drop(pin);

    // Free again; a refused one left nothing behind.
    store.attic_remove(&entry).unwrap();
    store
        .rollback_to(RecoveryTarget::CommitSeq(1), RollbackMode::Move)
        .unwrap();
    assert!(!store.is_frozen_by_damage());
}

/// The other direction: while files move (the pin held exclusively) a backup and a verification
/// wait, and find a consistent store afterwards; waiting is no damage. On every backend with a
/// file system.
#[test]
fn a_backup_and_a_verification_wait_while_files_move() {
    let path =
        std::env::temp_dir().join(format!("graphersal-store-overlap-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    let dir: Arc<dyn StoreDir> = Arc::new(FsDir::new(&path));
    let store = Arc::new(Store::create(dir.clone(), StoreOptions::new()).unwrap());
    store_with_history(&store);

    let moving = dir.lock(LockKind::BackupExclusive).unwrap().unwrap();
    let backup = {
        let store = store.clone();
        std::thread::spawn(move || store.backup(MemDir::new()).map(|_| ()))
    };
    let verify = {
        let store = store.clone();
        std::thread::spawn(move || store.verify().map(|report| report.is_ok()))
    };
    std::thread::sleep(std::time::Duration::from_millis(150));
    assert!(!backup.is_finished() && !verify.is_finished());
    drop(moving);
    backup.join().unwrap().unwrap();
    assert!(verify.join().unwrap().unwrap());
    assert!(!store.is_frozen_by_damage());
    assert!(store.damage_found().unwrap().is_empty());
    commits(&store, 1);
    drop(store);
    let _ = std::fs::remove_dir_all(&path);
}
