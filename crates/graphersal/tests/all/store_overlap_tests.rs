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

// Issue #28: a fork and a packed export read the files too. What moves them must not fail them with
// an I/O error (or call a manifest corrupt).

fn fork_is_sound(store: &Store) -> Result<(), PersistError> {
    let new_dir = MemDir::new();
    store.fork(RecoveryTarget::Latest, &new_dir)?;
    let report = Store::verify_dir(&new_dir)?;
    assert!(report.is_ok(), "{:?}", report.problems);
    Ok(())
}

fn export_is_sound(store: &Store) -> Result<(), PersistError> {
    let mut bytes = Vec::new();
    let info = store.export_packed(None, &mut bytes)?;
    let graph = graphersal::persist::read_snapshot(bytes.as_slice(), &Default::default())?;
    assert_eq!(graph.last_commit_seq(), info.commit_seq);
    Ok(())
}

/// A store in its own directory of the file system, removed when the test ends: the files really
/// move there, as in the issue's report.
struct OnDisk(std::path::PathBuf);

impl OnDisk {
    fn new(test: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "graphersal-store-overlap-{test}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        Self(path)
    }
}

impl Drop for OnDisk {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The report's shape: what reads runs over and over while the store rolls back on another thread.
/// Each read succeeds, or the rollback was refused; never anything else.
fn reads_survive_rollbacks_on_disk(test: &str, read: fn(&Store) -> Result<(), PersistError>) {
    let disk = OnDisk::new(test);
    let store = Arc::new(Store::create(disk.0.as_path(), StoreOptions::new()).unwrap());
    for round in 0..12 {
        let target = store_with_history(&store);
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reader = {
            let (store, stop) = (store.clone(), stop.clone());
            std::thread::spawn(move || -> Result<(), PersistError> {
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    read(&store)?;
                }
                Ok(())
            })
        };
        std::thread::sleep(std::time::Duration::from_millis(1));
        let rollback = store.rollback_to(RecoveryTarget::CommitSeq(target), RollbackMode::Move);
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        reader
            .join()
            .unwrap()
            .unwrap_or_else(|err| panic!("round {round}: {err}"));
        if let Err(err) = &rollback {
            assert_eq!(cause_of(err), Some(StoreFailure::FilesInUse), "{err}");
        }
    }
    assert!(store.verify().unwrap().is_ok());
    commits(&store, 1);
}

#[test]
fn a_fork_beside_a_rollback_succeeds() {
    overlap_never_freezes(|store, _| fork_is_sound(store));
    reads_survive_rollbacks_on_disk("fork", fork_is_sound);
}

#[test]
fn an_export_beside_a_rollback_succeeds() {
    overlap_never_freezes(|store, _| export_is_sound(store));
    reads_survive_rollbacks_on_disk("export", export_is_sound);
}

/// While files move (the pin held exclusively) a fork, an attic fork and an export wait, and
/// succeed afterwards.
#[test]
fn a_fork_and_an_export_wait_while_files_move() {
    let disk = OnDisk::new("wait");
    let dir: Arc<dyn StoreDir> = Arc::new(FsDir::new(&disk.0));
    let store = Arc::new(Store::create(dir.clone(), StoreOptions::new()).unwrap());
    let target = store_with_history(&store);
    store
        .rollback_to(RecoveryTarget::CommitSeq(target), RollbackMode::Move)
        .unwrap();
    let entry = store.attic_list().unwrap()[0].id.clone();

    let moving = dir.lock(LockKind::BackupExclusive).unwrap().unwrap();
    let fork = {
        let store = store.clone();
        std::thread::spawn(move || fork_is_sound(&store))
    };
    let attic_fork = {
        let store = store.clone();
        std::thread::spawn(move || store.attic_fork(&entry, MemDir::new()).map(|_| ()))
    };
    let export = {
        let store = store.clone();
        std::thread::spawn(move || export_is_sound(&store))
    };
    std::thread::sleep(std::time::Duration::from_millis(150));
    assert!(!fork.is_finished() && !attic_fork.is_finished() && !export.is_finished());
    drop(moving);
    fork.join().unwrap().unwrap();
    attic_fork.join().unwrap().unwrap();
    export.join().unwrap().unwrap();
    assert!(store.damage_found().unwrap().is_empty());
}

/// A writer that stops inside the export until the test lets it go on.
struct HeldWriter {
    entered: std::sync::mpsc::Sender<()>,
    go_on: std::sync::mpsc::Receiver<()>,
    bytes: Vec<u8>,
    held: bool,
}

impl std::io::Write for HeldWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if !self.held {
            self.held = true;
            let _ = self.entered.send(());
            let _ = self.go_on.recv();
        }
        self.bytes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The pin lasts as long as the export: what moves or deletes files is refused while it runs,
/// nothing changed, and goes through afterwards; the export is whole.
#[test]
fn what_changes_files_is_refused_while_an_export_runs() {
    let store = Arc::new(Store::create(MemDir::new(), StoreOptions::new()).unwrap());
    let target = store_with_history(&store);
    let last = store.graph().read().last_commit_seq();

    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (go_on_tx, go_on_rx) = std::sync::mpsc::channel();
    let export = {
        let store = store.clone();
        std::thread::spawn(move || {
            let mut writer = HeldWriter {
                entered: entered_tx,
                go_on: go_on_rx,
                bytes: Vec::new(),
                held: false,
            };
            let info = store.export_packed(None, &mut writer).unwrap();
            (info, writer.bytes)
        })
    };
    entered_rx.recv().unwrap();

    let err = store
        .rollback_to(RecoveryTarget::CommitSeq(target), RollbackMode::Move)
        .unwrap_err();
    assert_eq!(cause_of(&err), Some(StoreFailure::FilesInUse), "{err}");
    assert!(err.to_string().contains("export"), "{err}");
    let err = store.prune(last).unwrap_err();
    assert_eq!(cause_of(&err), Some(StoreFailure::BackupRunning), "{err}");
    assert!(err.to_string().contains("export"), "{err}");
    assert_eq!(store.graph().read().last_commit_seq(), last);
    // What only reads goes on beside it.
    fork_is_sound(&store).unwrap();
    export_is_sound(&store).unwrap();

    go_on_tx.send(()).unwrap();
    let (info, bytes) = export.join().unwrap();
    let graph = graphersal::persist::read_snapshot(bytes.as_slice(), &Default::default()).unwrap();
    assert_eq!(graph.last_commit_seq(), info.commit_seq);
    store
        .rollback_to(RecoveryTarget::CommitSeq(target), RollbackMode::Move)
        .unwrap();
}

/// A store that cannot change its files takes no pin: nothing is written into its directory.
#[test]
fn an_opened_backup_forks_and_exports_without_a_pin_file() {
    let disk = OnDisk::new("backup");
    let store = Store::create(MemDir::new(), StoreOptions::new()).unwrap();
    store_with_history(&store);
    store.backup(disk.0.as_path()).unwrap();

    let backup = Store::open_backup(disk.0.as_path(), StoreOptions::new()).unwrap();
    fork_is_sound(&backup).unwrap();
    export_is_sound(&backup).unwrap();
    assert!(!disk.0.join("BACKUP").exists());
}
