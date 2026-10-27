//! Backups of a Store (feature `persist`; format spec 12.6 and 12.7): the backup marker (read-only
//! until restored), restore in place, full and incremental backups (new snapshots, new WAL
//! segments, the tail of the last segment), point-in-time reads from a backup, increments
//! after a checkpoint and after an in-place rollback of the store, refusals (another graph, a
//! different store with a shared history: a fork, repair or copy; a rollback behind the backup,
//! a pruned gap), the store id kept or renewed by every copy path, the backup's own prune, and an increment
//! interrupted at every step (a crash image after each write) that leaves a valid store and is
//! rolled back by the next backup or restore.
#![cfg(feature = "persist")]

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use graphersal::persist::{
    BackupMode, DirEntry, DirLockGuard, EntryKind, LockKind, MemDir, PersistError, RecoveryTarget,
    RollbackMode, Store, StoreDir, StoreFile, StoreOptions,
};
use graphersal::storage::PersistentStorage;

use crate::persist_tests::dump;

fn options() -> StoreOptions {
    StoreOptions::new()
        .with_wal_segment_bytes(400)
        .with_checkpoint_after_bytes(0)
}

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

fn add_people(store: &Store, prefix: &str, n: usize) {
    for i in 0..n {
        add_person(store, &format!("{prefix}{i}"));
    }
}

/// The state of `store` at `commit` (a point-in-time read of its files).
fn state_at(store: &Store, commit: u64) -> Vec<String> {
    dump(
        &store
            .read_only_at(RecoveryTarget::CommitSeq(commit))
            .unwrap()
            .graph,
    )
}

/// The latest state of the backup in `dir`: `(commit, dump)`.
fn backup_state(dir: &MemDir) -> (u64, Vec<String>) {
    let recovered = Store::open_read_only(dir, RecoveryTarget::Latest).unwrap();
    (recovered.graph.last_commit_seq(), dump(&recovered.graph))
}

/// Checks that the backup in `dir` holds exactly the store's state at the backup's commit,
/// verifies cleanly, and that every commit it holds is a point-in-time target.
fn assert_backup_matches(store: &Store, dir: &MemDir, commit: u64) {
    let (at, state) = backup_state(dir);
    assert_eq!(at, commit);
    assert_eq!(state, state_at(store, commit));
    let verify = Store::verify_dir(dir).unwrap();
    assert!(verify.is_ok(), "{:?}", verify.problems);
    let marker = Store::backup_marker_dir(dir).unwrap().unwrap();
    assert_eq!(marker.commit_seq, commit);
    let oldest = Store::snapshots_dir(dir).unwrap()[0].commit_seq;
    for c in oldest..=commit {
        let past = Store::open_read_only(dir, RecoveryTarget::CommitSeq(c)).unwrap();
        assert_eq!(dump(&past.graph), state_at(store, c), "commit {c}");
    }
}

#[test]
fn a_backup_is_marked_read_only_and_a_restore_makes_it_the_live_store() {
    let dir = MemDir::new();
    let store = Store::create(&dir, options()).unwrap();
    add_people(&store, "p", 3);
    store.mark("three").unwrap();
    let backup = MemDir::new();
    let report = store.backup(&backup).unwrap();
    assert!(!report.incremental);
    assert_eq!(report.commit_seq, 3);
    assert_eq!(report.snapshots_copied, [0]);
    let graph_id = store.graph().read().graph_id();

    // Read-write open: refused, with the way out in the help.
    let err = Store::open(&backup, StoreOptions::new()).unwrap_err();
    assert!(
        matches!(err, PersistError::IsBackup { commit_seq: 3, .. }),
        "{err}"
    );
    let help = graphersal::error::DiagnosticError::help(&err).unwrap();
    for hint in ["store restore", "store fork", "read-only"] {
        assert!(help.contains(hint), "{help}");
    }

    // Read-only: as a Store (listing, info, views, verify, export) that refuses every write.
    let read_only = Store::open_backup(&backup, StoreOptions::new()).unwrap();
    assert_eq!(read_only.backup_marker().unwrap().commit_seq, 3);
    assert_eq!(dump(&read_only.graph().read()), state_at(&store, 3));
    let info = read_only.info().unwrap();
    assert_eq!(info.backup.unwrap().commit_seq, 3);
    assert_eq!(info.graph_id, graph_id);
    assert_eq!(read_only.marks().unwrap()[0].name(), "three");
    assert!(read_only.verify().unwrap().is_ok());
    let mut packed = Vec::new();
    read_only.export_packed(None, &mut packed).unwrap();
    assert_eq!(
        dump(
            &read_only
                .read_only_at(RecoveryTarget::CommitSeq(1))
                .unwrap()
                .graph
        ),
        state_at(&store, 1)
    );
    let write = read_only
        .graph()
        .write()
        .traversal_mut()
        .add_v("x")
        .to_list();
    let write = graphersal::error::format_diagnostic(&write.unwrap_err());
    assert!(write.contains("backup"), "{write}");
    // Its own help (restore or fork the backup), not the generic commit-hook help.
    assert!(write.contains("graphersal store restore"), "{write}");
    assert!(!write.contains("write-ahead"), "{write}");
    for refused in [
        read_only.mark("m").err(),
        read_only.checkpoint(None).err(),
        read_only.prune(10).err(),
        read_only
            .rollback_to(RecoveryTarget::CommitSeq(1), RollbackMode::Move)
            .err(),
    ] {
        assert!(matches!(refused, Some(PersistError::IsBackup { .. })));
    }
    drop(read_only);
    // Closing a read-only backup writes nothing.
    assert!(Store::backup_marker_dir(&backup).unwrap().is_some());

    // A store is not a backup.
    assert!(Store::open_backup(&dir, StoreOptions::new()).is_err());
    assert!(Store::restore_backup(&dir).is_err());
    assert!(Store::prune_backup(&dir, 1).is_err());

    // Restore: the same lineage, live, writable.
    let marker = Store::restore_backup(&backup).unwrap();
    assert_eq!(marker.commit_seq, 3);
    assert!(Store::backup_marker_dir(&backup).unwrap().is_none());
    let live = Store::open(&backup, StoreOptions::new()).unwrap();
    assert_eq!(live.graph().read().graph_id(), graph_id);
    add_person(&live, "after-restore");
    assert_eq!(live.graph().read().last_commit_seq(), 4);
}

#[test]
fn increments_copy_only_what_is_new_and_the_backup_equals_the_store() {
    let dir = MemDir::new();
    let store = Store::create(&dir, options()).unwrap();
    add_people(&store, "a", 2);
    let backup = MemDir::new();
    store.backup(&backup).unwrap();
    assert_backup_matches(&store, &backup, 2);

    // 1: a few commits in the same WAL segment: only its tail is appended.
    add_person(&store, "b");
    let report = store.backup(&backup).unwrap();
    assert!(report.incremental);
    assert_eq!(report.previous_commit_seq, 2);
    assert_eq!(report.commit_seq, 3);
    assert!(report.tail_bytes > 0);
    assert!(report.snapshots_copied.is_empty());
    assert_backup_matches(&store, &backup, 3);

    // 2: enough commits to rotate WAL segments: new segments are copied whole.
    add_people(&store, "c", 8);
    let report = store.backup(&backup).unwrap();
    assert!(!report.segments_copied.is_empty(), "{report:?}");
    assert_backup_matches(&store, &backup, 11);

    // 3: a checkpoint: its snapshot is copied too.
    store.checkpoint(Some("cp")).unwrap();
    add_people(&store, "d", 2);
    store.mark("after-d").unwrap();
    let report = store.backup(&backup).unwrap();
    assert_eq!(report.snapshots_copied, [11]);
    assert_backup_matches(&store, &backup, 13);
    assert_eq!(
        Store::marks_dir(&backup).unwrap()[0].name(),
        "after-d",
        "the marks file follows"
    );

    // 4: nothing new: nothing is copied, the marker's time moves on.
    let before = Store::backup_marker_dir(&backup).unwrap().unwrap();
    let report = store.backup(&backup).unwrap();
    assert_eq!(report.bytes, 0);
    let after = Store::backup_marker_dir(&backup).unwrap().unwrap();
    assert!(after.taken_at > before.taken_at);
    assert_eq!(after.commit_seq, 13);

    // A backup by another process (no durable end there) continues it too.
    add_person(&store, "e");
    let report = Store::backup_dir(&dir, &backup).unwrap();
    assert!(report.incremental);
    assert_backup_matches(&store, &backup, 14);

    // --full needs an empty directory; a fresh full backup of the same state equals it.
    assert!(store.backup_with(&backup, BackupMode::Full).is_err());
    let fresh = MemDir::new();
    store.backup_with(&fresh, BackupMode::Full).unwrap();
    assert_eq!(backup_state(&fresh), backup_state(&backup));

    // The backup's own retention (by the store's rules: the last two snapshots stay).
    store.checkpoint(None).unwrap();
    add_person(&store, "f");
    let report = store.backup(&backup).unwrap();
    assert_eq!(report.snapshots_copied, [14]);
    let pruned = Store::prune_backup(&backup, 14).unwrap();
    assert_eq!(pruned.snapshots_removed, [0]);
    assert!(!pruned.segments_removed.is_empty());
    assert_eq!(backup_state(&backup), (15, state_at(&store, 15)));
    assert!(Store::verify_dir(&backup).unwrap().is_ok());
    // ... and it is continued as before.
    add_person(&store, "g");
    store.backup(&backup).unwrap();
    assert_eq!(backup_state(&backup), (16, state_at(&store, 16)));
}

#[test]
fn an_increment_follows_an_in_place_rollback_at_or_after_the_backup() {
    // The rollback target is after the backup's position: the segment holding it is split,
    // the new lineage's segment follows.
    let dir = MemDir::new();
    let store = Store::create(&dir, options()).unwrap();
    add_people(&store, "a", 3);
    let backup = MemDir::new();
    store.backup(&backup).unwrap();
    add_people(&store, "b", 3);
    store
        .rollback_to(RecoveryTarget::CommitSeq(4), RollbackMode::Move)
        .unwrap();
    add_people(&store, "c", 2);
    let report = store.backup(&backup).unwrap();
    assert!(report.lineage_changed);
    let new_id = store.graph().read().graph_id();
    assert_eq!(Store::inspect(&backup).unwrap().graph_id, new_id);
    assert_eq!(backup_state(&backup), (6, dump(&store.graph().read())));
    assert!(Store::verify_dir(&backup).unwrap().is_ok());

    // The target IS the backup's position, and the backup's last segment holds only a mark
    // there: the new lineage's segment of that name (with the mark) replaces it.
    let dir = MemDir::new();
    let store = Store::create(&dir, options()).unwrap();
    add_people(&store, "a", 3);
    store.checkpoint(None).unwrap();
    store.mark("m").unwrap();
    let backup = MemDir::new();
    store.backup(&backup).unwrap();
    add_people(&store, "b", 2);
    store
        .rollback_to(RecoveryTarget::Mark("m".into()), RollbackMode::Move)
        .unwrap();
    add_person(&store, "c");
    let report = store.backup(&backup).unwrap();
    assert!(report.lineage_changed, "{report:?}");
    // The segment named 4 (the mark at commit 3) was replaced, nothing appended.
    assert_eq!(
        (report.segments_copied.as_slice(), report.tail_bytes),
        (&[4][..], 0)
    );
    assert_eq!(backup_state(&backup), (4, dump(&store.graph().read())));
    assert_eq!(Store::marks_dir(&backup).unwrap()[0].name(), "m");
    assert!(Store::verify_dir(&backup).unwrap().is_ok());
    Store::restore_backup(&backup).unwrap();
    let live = Store::open(&backup, StoreOptions::new()).unwrap();
    assert_eq!(dump(&live.graph().read()), dump(&store.graph().read()));
}

#[test]
fn increments_are_refused_for_another_graph_a_rollback_behind_and_a_pruned_gap() {
    let dir = MemDir::new();
    let store = Store::create(&dir, options()).unwrap();
    add_people(&store, "a", 4);
    let backup = MemDir::new();
    store.backup(&backup).unwrap();

    // Another graph.
    let other = Store::create(MemDir::new(), options()).unwrap();
    add_person(&other, "x");
    let err = other.backup(&backup).unwrap_err().to_string();
    assert!(err.contains("another graph"), "{err}");
    // A store that is not a backup is never written to.
    let plain = MemDir::new();
    Store::create(&plain, options()).unwrap().close().unwrap();
    let err = store.backup(&plain).unwrap_err().to_string();
    assert!(err.contains("not a backup"), "{err}");

    // A rollback behind the backup's position: the backup has history the store lost.
    let behind = MemDir::new();
    store.backup_with(&behind, BackupMode::Full).unwrap();
    store
        .rollback_to(RecoveryTarget::CommitSeq(2), RollbackMode::Move)
        .unwrap();
    add_person(&store, "z");
    let err = store.backup(&behind).unwrap_err().to_string();
    assert!(err.contains("rolled back"), "{err}");
    assert_eq!(
        backup_state(&behind).0,
        4,
        "the refused backup is unchanged"
    );

    // A pruned gap: the store removed WAL the backup still needs.
    let dir = MemDir::new();
    let store = Store::create(&dir, options()).unwrap();
    add_people(&store, "a", 2);
    let backup = MemDir::new();
    store.backup(&backup).unwrap();
    for round in 0..3 {
        add_people(&store, &format!("r{round}-"), 6);
        store.checkpoint(None).unwrap();
    }
    add_person(&store, "last");
    let pruned = store.prune(u64::MAX).unwrap();
    assert!(!pruned.segments_removed.is_empty());
    let before = backup_state(&backup);
    let err = store.backup(&backup).unwrap_err().to_string();
    assert!(err.contains("full backup is needed"), "{err}");
    assert_eq!(backup_state(&backup), before);
    let full = MemDir::new();
    store.backup_with(&full, BackupMode::Full).unwrap();
    assert_eq!(backup_state(&full).1, dump(&store.graph().read()));
}

/// The store id (format spec 5.5) of the store in `dir`, read from its files.
fn store_id_of(dir: &MemDir) -> u128 {
    Store::inspect(dir).unwrap().store_id
}

/// Asserts that an increment of `copy` into `backup` (a backup of another store with a shared
/// history) is refused as a different store, with the way out, and leaves the backup as it was.
fn assert_refused_as_different_store(copy: &Store, backup: &MemDir) {
    let before = backup_state(backup);
    let err = copy.backup(backup).unwrap_err();
    assert!(matches!(err, PersistError::BackupRefused { .. }), "{err}");
    let text = err.to_string();
    for hint in [
        "different store",
        "a fork, repair or copy",
        "new directory",
        "full",
    ] {
        assert!(text.contains(hint), "{text}");
    }
    let help = graphersal::error::DiagnosticError::help(&err).unwrap();
    assert!(help.contains("different store"), "{help}");
    assert_eq!(
        backup_state(backup),
        before,
        "the refused backup is unchanged"
    );
}

/// The store id is a creation parameter (CLAUDE.md rule 22): every copy path either renews it
/// (fork, attic fork, repair, convert: another store, whose increment into the original's
/// backup is refused even when it continues the backup's lineage) or keeps it (backup full,
/// increment and ZIP, in-place rollback, attic restore, restore of a backup: the same store,
/// which continues the original's backups).
#[test]
fn the_store_id_is_renewed_by_every_copy_and_kept_by_every_restore() {
    let dir = MemDir::new();
    let store = Store::create(&dir, options()).unwrap();
    let id = store.store_id().unwrap();
    assert_ne!(id, 0);
    assert_eq!(store.info().unwrap().store_id, id);
    add_people(&store, "a", 3);
    let backup = MemDir::new();
    store.backup(&backup).unwrap();
    assert_eq!(
        store_id_of(&backup),
        id,
        "a full backup records the source's id"
    );

    // Fork at the backup's position: the same lineage history, but another store.
    let fork_dir = MemDir::new();
    store.fork(RecoveryTarget::Latest, &fork_dir).unwrap();
    assert_ne!(store_id_of(&fork_dir), id);
    let fork = Store::open(&fork_dir, options()).unwrap();
    add_person(&fork, "fork");
    assert_refused_as_different_store(&fork, &backup);
    drop(fork);
    // A fork of a closed store (fork_dir) too.
    let fork2 = MemDir::new();
    Store::fork_dir(&fork_dir, RecoveryTarget::Latest, &fork2).unwrap();
    assert_ne!(store_id_of(&fork2), store_id_of(&fork_dir));

    // Repair: a new store as well.
    let repaired = MemDir::new();
    store.repair_to(&repaired).unwrap();
    assert_ne!(store_id_of(&repaired), id);
    let repaired_store = Store::open(&repaired, options()).unwrap();
    add_person(&repaired_store, "repaired");
    assert_refused_as_different_store(&repaired_store, &backup);
    drop(repaired_store);

    // The original continues its backup (an increment).
    add_person(&store, "b");
    let report = store.backup(&backup).unwrap();
    assert!(report.incremental);
    assert_eq!(store_id_of(&backup), id, "an increment keeps it");
    add_person(&store, "b2");
    // A backup of the history the rollback below moves to the attic.
    let pre = MemDir::new();
    store.backup_with(&pre, BackupMode::Full).unwrap();

    // An in-place rollback at the backup's position keeps it; the increment follows.
    store
        .rollback_to(RecoveryTarget::CommitSeq(4), RollbackMode::Move)
        .unwrap();
    assert_eq!(store.store_id().unwrap(), id);
    add_person(&store, "c");
    assert!(store.backup(&backup).unwrap().incremental);
    // The attic fork is another store.
    let entry = store.attic_list().unwrap().remove(0);
    let attic_fork = MemDir::new();
    store.attic_fork(&entry.id, &attic_fork).unwrap();
    assert_ne!(store_id_of(&attic_fork), id);
    let attic_fork_store = Store::open(&attic_fork, options()).unwrap();
    add_person(&attic_fork_store, "attic");
    assert_refused_as_different_store(&attic_fork_store, &pre);
    drop(attic_fork_store);
    // An attic restore keeps it.
    store
        .rollback_to(RecoveryTarget::CommitSeq(4), RollbackMode::Move)
        .unwrap();
    let newest = store.attic_list().unwrap().pop().unwrap();
    store.attic_restore(&newest.id).unwrap();
    assert_eq!(store.store_id().unwrap(), id);
    assert_eq!(store_id_of(&dir), id);

    // Convert (a copy of a closed store) gives the copy a new id.
    let second = MemDir::new();
    store.backup_with(&second, BackupMode::Full).unwrap();
    let fourth = MemDir::new();
    store.backup_with(&fourth, BackupMode::Full).unwrap();
    let position = store.graph().read().last_commit_seq();
    store.close().unwrap();
    let converted = MemDir::new();
    Store::convert(&dir, &converted).unwrap();
    assert_ne!(store_id_of(&converted), id);
    assert_eq!(store_id_of(&dir), id, "the source is unchanged");
    let converted_store = Store::open(&converted, options()).unwrap();
    assert_eq!(converted_store.graph().read().last_commit_seq(), position);
    add_person(&converted_store, "converted");
    assert_refused_as_different_store(&converted_store, &second);
    drop(converted_store);

    // A restored backup IS the store: it keeps the id and continues its backups.
    Store::restore_backup(&second).unwrap();
    assert_eq!(store_id_of(&second), id);
    let restored = Store::open(&second, options()).unwrap();
    add_person(&restored, "restored");
    assert!(restored.backup(&fourth).unwrap().incremental);
    assert_eq!(store_id_of(&fourth), id);
    drop(restored);

    // ZIP: the archive records the id, an unpacked archive keeps it and continues a backup.
    #[cfg(feature = "persist-zip")]
    {
        let live = Store::open(&dir, options()).unwrap();
        let third = MemDir::new();
        live.backup(&third).unwrap();
        add_person(&live, "zip");
        let mut zip = std::io::Cursor::new(Vec::new());
        live.backup_zip(&mut zip).unwrap();
        drop(live);
        let unpacked = MemDir::new();
        Store::restore_zip(zip.into_inner().as_slice(), &unpacked).unwrap();
        assert_eq!(store_id_of(&unpacked), id);
        let from_zip = Store::open(&unpacked, options()).unwrap();
        add_person(&from_zip, "after-zip");
        assert!(from_zip.backup(&third).unwrap().incremental);
    }
}

#[cfg(feature = "persist-zip")]
#[test]
fn a_zip_backup_is_full_and_restores_to_the_live_store() {
    let store = Store::create(MemDir::new(), options()).unwrap();
    add_people(&store, "a", 3);
    let mut zip = std::io::Cursor::new(Vec::new());
    let report = store.backup_zip(&mut zip).unwrap();
    let zip = zip.into_inner();
    assert!(!report.incremental);
    let restored = MemDir::new();
    Store::restore_zip(zip.as_slice(), &restored).unwrap();
    assert!(Store::backup_marker_dir(&restored).unwrap().is_none());
    let live = Store::open(&restored, StoreOptions::new()).unwrap();
    assert_eq!(dump(&live.graph().read()), dump(&store.graph().read()));
}

// ---------------------------------------------------------------------------------------------
// Interrupted increments
// ---------------------------------------------------------------------------------------------

/// A [`MemDir`] that "crashes" after a number of writes: from then on every write fails (also
/// the clean-up of the failed operation), so the bytes are what a crash at that point leaves.
#[derive(Debug, Clone)]
pub(crate) struct Crashing {
    pub(crate) inner: MemDir,
    pub(crate) budget: Arc<AtomicI64>,
}

impl Crashing {
    fn tick(&self) -> io::Result<()> {
        if self.budget.fetch_sub(1, Ordering::SeqCst) <= 0 {
            return Err(io::Error::other("simulated crash"));
        }
        Ok(())
    }

    pub(crate) fn crashed(&self) -> bool {
        self.budget.load(Ordering::SeqCst) < 0
    }
}

struct CrashingFile {
    inner: Box<dyn StoreFile>,
    dir: Crashing,
}

impl io::Write for CrashingFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.dir.tick()?;
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl StoreFile for CrashingFile {
    fn sync(&mut self) -> io::Result<()> {
        self.dir.tick()?;
        self.inner.sync()
    }

    fn set_len(&mut self, len: u64) -> io::Result<()> {
        self.dir.tick()?;
        self.inner.set_len(len)
    }
}

impl StoreDir for Crashing {
    fn location(&self) -> String {
        "memory:crashing".into()
    }
    fn entry_kind(&self, name: &str) -> io::Result<Option<EntryKind>> {
        self.inner.entry_kind(name)
    }
    fn list(&self, dir: &str) -> io::Result<Vec<DirEntry>> {
        self.inner.list(dir)
    }
    fn file_len(&self, name: &str) -> io::Result<u64> {
        self.inner.file_len(name)
    }
    fn read(&self, name: &str, max: u64) -> io::Result<Vec<u8>> {
        self.inner.read(name, max)
    }
    fn read_range(&self, name: &str, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        self.inner.read_range(name, offset, len)
    }
    fn open_read(&self, name: &str) -> io::Result<Box<dyn io::Read + Send>> {
        self.inner.open_read(name)
    }
    fn create_dir_all(&self, name: &str) -> io::Result<()> {
        self.tick()?;
        self.inner.create_dir_all(name)
    }
    fn create(&self, name: &str, exclusive: bool) -> io::Result<Box<dyn StoreFile>> {
        self.tick()?;
        Ok(Box::new(CrashingFile {
            inner: self.inner.create(name, exclusive)?,
            dir: self.clone(),
        }))
    }
    fn open_write(&self, name: &str, offset: u64) -> io::Result<Box<dyn StoreFile>> {
        self.tick()?;
        Ok(Box::new(CrashingFile {
            inner: self.inner.open_write(name, offset)?,
            dir: self.clone(),
        }))
    }
    fn append(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        self.tick()?;
        self.inner.append(name, bytes)
    }
    fn write_atomic(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        self.tick()?;
        self.inner.write_atomic(name, bytes)
    }
    fn rename(&self, from: &str, to: &str) -> io::Result<()> {
        self.tick()?;
        self.inner.rename(from, to)
    }
    fn truncate(&self, name: &str, len: u64) -> io::Result<()> {
        self.tick()?;
        self.inner.truncate(name, len)
    }
    fn remove_file(&self, name: &str) -> io::Result<()> {
        self.tick()?;
        self.inner.remove_file(name)
    }
    fn remove_tree(&self, name: &str) -> io::Result<()> {
        self.tick()?;
        self.inner.remove_tree(name)
    }
    fn sync_dir(&self, name: &str) -> io::Result<()> {
        self.tick()?;
        self.inner.sync_dir(name)
    }
    fn lock(&self, kind: LockKind) -> io::Result<Option<DirLockGuard>> {
        self.inner.lock(kind)
    }
}

/// Runs the increment of `store` into a copy of `base` (a backup of it) crashing after every
/// number of writes in turn: each crash image is a valid store holding a state of the store
/// (the old one or a later one), the next increment completes it, and a restore instead rolls
/// it back to the old state.
fn crash_at_every_step(store: &Store, base: &MemDir) {
    let old = backup_state(base);
    let mut steps = 0;
    for budget in 0.. {
        let image = base.deep_copy();
        let crashing = Crashing {
            inner: image.clone(),
            budget: Arc::new(AtomicI64::new(budget)),
        };
        let shared: Arc<dyn StoreDir> = Arc::new(crashing.clone());
        let result = store.backup(&shared);
        if !crashing.crashed() {
            result.unwrap();
            break;
        }
        assert!(result.is_err());
        steps += 1;
        // The crash image is a valid store: a state of the store at some commit.
        let (commit, state) = backup_state(&image);
        assert_eq!(
            state,
            state_at(store, commit),
            "crash after {budget} writes"
        );
        // The next increment rolls the interrupted one back and completes.
        let again = image.deep_copy();
        let report = store.backup(&again).unwrap();
        assert!(report.incremental);
        let end = store.graph().read().last_commit_seq();
        assert_backup_matches(store, &again, end);
        // A restore instead rolls it back to the old state and opens read-write.
        Store::restore_backup(&image).unwrap();
        let live = Store::open(&image, StoreOptions::new()).unwrap();
        // (Rolled back to the old state; or, when the crash came after the increment's last
        // step, its GRAPH file, completed.)
        let restored = (
            live.graph().read().last_commit_seq(),
            dump(&live.graph().read()),
        );
        assert!(
            restored == old || restored == (end, state_at(store, end)),
            "restore after a crash after {budget} writes"
        );
    }
    assert!(steps > 10, "only {steps} crash points");
}

#[test]
fn an_increment_interrupted_at_every_step_leaves_a_valid_backup() {
    let dir = MemDir::new();
    let store = Store::create(&dir, options()).unwrap();
    add_people(&store, "a", 3);
    let base = MemDir::new();
    store.backup(&base).unwrap();
    // A tail, new segments, a new snapshot and a mark.
    add_people(&store, "b", 6);
    store.checkpoint(None).unwrap();
    add_people(&store, "c", 2);
    store.mark("m").unwrap();
    crash_at_every_step(&store, &base);
}

#[test]
fn an_increment_after_a_rollback_interrupted_at_every_step_leaves_a_valid_backup() {
    let dir = MemDir::new();
    let store = Store::create(&dir, options()).unwrap();
    add_people(&store, "a", 3);
    store.checkpoint(None).unwrap();
    store.mark("m").unwrap();
    let base = MemDir::new();
    store.backup(&base).unwrap();
    add_people(&store, "b", 2);
    store
        .rollback_to(RecoveryTarget::Mark("m".into()), RollbackMode::Move)
        .unwrap();
    add_people(&store, "c", 4);
    crash_at_every_step(&store, &base);
}
