//! The single-file store (feature `persist`, native only; format spec section 14): every Store
//! operation on all three backends (a directory, memory, one file) and mixed, relocation,
//! conversion between a directory and a file, `compact()`, the compaction advice (thresholds,
//! free disk space, no data reads), the lock, and crash images of the file: every cut of the
//! last update, bit flips in the header slots, the table and the records, damage of the
//! container's own records (maintenance mode).
#![cfg(all(feature = "persist", not(target_arch = "wasm32")))]

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use graphersal::persist::{
    AdviceRule, CompactionPolicy, DamageKind, DirCheck, DirCompaction, DirEntry, DirLockGuard,
    EntryKind, FsDir, LockKind, MemDir, PersistError, RecoveryTarget, RollbackMode, SingleFileDir,
    SpaceUsage, Store, StoreDir, StoreFile, StoreOptions,
};
use graphersal::prelude::*;

use crate::persist_tests::{dump, names, varied_graph};

/// A fresh directory under the system temp dir, removed on drop.
struct Temp(PathBuf);

impl Temp {
    fn new(name: &str) -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "graphersal-single-{}-{}-{name}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn join(&self, part: &str) -> PathBuf {
        self.0.join(part)
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
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

fn graph_names(store: &Store) -> Vec<String> {
    names(&store.graph().read())
}

fn options() -> StoreOptions {
    StoreOptions::new()
        .with_wal_segment_bytes(600)
        .with_checkpoint_after_bytes(0)
}

/// The three backends.
#[derive(Debug, Clone, Copy)]
enum Backend {
    Dir,
    Memory,
    File,
}

impl Backend {
    fn make(self, temp: &Temp, name: &str) -> Arc<dyn StoreDir> {
        match self {
            Backend::Dir => Arc::new(FsDir::new(temp.join(name))),
            Backend::Memory => Arc::new(MemDir::named(name)),
            Backend::File => Arc::new(SingleFileDir::new(temp.join(&format!("{name}.gstore")))),
        }
    }
}

/// Everything a store does, on `main` (fork into `fork`, backups into `backup`).
fn does_everything(main: Arc<dyn StoreDir>, fork: Arc<dyn StoreDir>, backup: Arc<dyn StoreDir>) {
    let store = Store::create_with(&main, varied_graph(), options()).unwrap();
    let original = dump(&store.graph().read());
    add_person(&store, "ann");
    store.mark("after-ann").unwrap();
    let snapshot = store.checkpoint(Some("named")).unwrap();
    assert_eq!(snapshot.name, "named");
    for name in ["bob", "cy", "dee", "eve"] {
        add_person(&store, name);
    }
    let at = store.graph().read().last_commit_seq();
    assert!(matches!(
        Store::open(&main, options()),
        Err(PersistError::Locked { .. })
    ));
    let info = store.info().unwrap();
    assert_eq!(info.snapshots.len(), 2);
    assert!(info.wal_segments > 1, "the WAL rotated");
    assert_eq!(info.marks[0].name(), "after-ann");
    // Point-in-time views while the store is open.
    let view = store
        .read_only_at(RecoveryTarget::Mark("after-ann".into()))
        .unwrap();
    assert!(names(&view.graph).contains(&"ann".to_string()));
    assert!(!names(&view.graph).contains(&"bob".to_string()));
    let past = Store::open_read_only(&main, RecoveryTarget::CommitSeq(0)).unwrap();
    assert_eq!(dump(&past.graph), original);
    let changes: Vec<u64> = store
        .changes_since(snapshot.commit_seq)
        .unwrap()
        .map(|c| c.unwrap().commit_seq())
        .collect();
    assert_eq!(changes.len(), 4);
    let mut packed = Vec::new();
    store
        .export_packed(Some(snapshot.commit_seq), &mut packed)
        .unwrap();
    assert_eq!(
        TraversalGraph::read_snapshot(packed.as_slice())
            .unwrap()
            .last_commit_seq(),
        snapshot.commit_seq
    );
    // Fork.
    let forked = store
        .fork(RecoveryTarget::Mark("after-ann".into()), &fork)
        .unwrap();
    assert_eq!(forked.snapshot.commit_seq, snapshot.commit_seq);
    {
        let branch = Store::open(&fork, options()).unwrap();
        assert_eq!(branch.info().unwrap().lineage.len(), 2);
        add_person(&branch, "dan");
        assert!(graph_names(&branch).contains(&"dan".to_string()));
    }
    // Rollback into the attic and back.
    let report = store
        .rollback_to(RecoveryTarget::Mark("after-ann".into()), RollbackMode::Move)
        .unwrap();
    assert_eq!(report.previous_commit_seq, at);
    assert!(!graph_names(&store).contains(&"bob".to_string()));
    let id = report.attic.unwrap();
    assert_eq!(store.attic_changes(&id).unwrap().count(), 4);
    store.attic_can_restore(&id).unwrap();
    assert_eq!(store.attic_restore(&id).unwrap(), at);
    assert!(graph_names(&store).contains(&"eve".to_string()));
    // Prune, verify.
    store.checkpoint(None).unwrap();
    add_person(&store, "fay");
    store.checkpoint(None).unwrap();
    let pruned = store.prune(store.graph().read().last_commit_seq()).unwrap();
    assert!(!pruned.snapshots_removed.is_empty() || !pruned.segments_removed.is_empty());
    let verified = store.verify().unwrap();
    assert!(verified.is_ok(), "{:?}", verified.problems);
    // A full backup and an increment.
    store.backup(&backup).unwrap();
    add_person(&store, "gus");
    let increment = store.backup(&backup).unwrap();
    assert!(increment.incremental);
    let expected = dump(&store.graph().read());
    let compacted = store.compact().unwrap();
    assert_eq!(
        compacted.compaction.is_some(),
        store.info().unwrap().space.is_some()
    );
    assert_eq!(dump(&store.graph().read()), expected);
    add_person(&store, "hal");
    let expected_after = dump(&store.graph().read());
    store.close().unwrap();
    let reopened = Store::open(&main, options()).unwrap();
    assert!(reopened.open_report().clean_close);
    assert_eq!(dump(&reopened.graph().read()), expected_after);
    assert!(reopened.verify().unwrap().is_ok());
    drop(reopened);
    // The backup is marked: read-only until it is restored.
    assert!(matches!(
        Store::open(&backup, options()),
        Err(PersistError::IsBackup { .. })
    ));
    let read_only = Store::open_backup(&backup, options()).unwrap();
    assert_eq!(dump(&read_only.graph().read()), expected);
    drop(read_only);
    Store::restore_backup(&backup).unwrap();
    let restored = Store::open(&backup, options()).unwrap();
    assert_eq!(dump(&restored.graph().read()), expected);
}

#[test]
fn every_store_operation_works_on_every_backend() {
    for backend in [Backend::Dir, Backend::Memory, Backend::File] {
        let temp = Temp::new("all");
        does_everything(
            backend.make(&temp, "main"),
            backend.make(&temp, "fork"),
            backend.make(&temp, "backup"),
        );
    }
}

#[test]
fn a_single_file_store_forks_and_backs_up_into_other_backends_and_back() {
    let temp = Temp::new("mixed");
    does_everything(
        Backend::File.make(&temp, "main"),
        Backend::Dir.make(&temp, "fork"),
        Backend::Memory.make(&temp, "backup"),
    );
    let temp = Temp::new("mixed2");
    does_everything(
        Backend::Dir.make(&temp, "main"),
        Backend::File.make(&temp, "fork"),
        Backend::File.make(&temp, "backup"),
    );
}

#[test]
fn a_gstore_path_is_a_single_file_and_the_file_is_relocatable() {
    let temp = Temp::new("path");
    let path = temp.join("graph.gstore");
    let store = Store::create(&path, options()).unwrap();
    add_person(&store, "ann");
    store.mark("m").unwrap();
    let expected = dump(&store.graph().read());
    assert!(store.dir().local_path() == Some(path.as_path()));
    store.close().unwrap();
    assert!(path.is_file(), "one file, not a directory");
    assert!(SingleFileDir::is_single_file(&path).unwrap());
    assert_eq!(
        fs::read_dir(&temp.0).unwrap().count(),
        1,
        "nothing next to it"
    );
    // Moved elsewhere (and renamed), it opens as the same graph; any extension works once it
    // exists.
    fs::create_dir_all(temp.join("elsewhere")).unwrap();
    let moved = temp.join("elsewhere/renamed.bin");
    fs::rename(&path, &moved).unwrap();
    let store = Store::open(&moved, options()).unwrap();
    assert_eq!(dump(&store.graph().read()), expected);
    assert_eq!(store.marks().unwrap()[0].name(), "m");
    drop(store);
    // `open_file`/`create_file` take any name.
    let explicit = temp.join("plain-name");
    let store = Store::create_file(&explicit, options()).unwrap();
    add_person(&store, "bob");
    drop(store);
    assert!(explicit.is_file());
    let store = Store::open_file(&explicit, options()).unwrap();
    assert_eq!(graph_names(&store), ["bob"]);
}

#[test]
fn one_writer_per_file_and_readers_while_it_writes() {
    let temp = Temp::new("lock");
    let path = temp.join("graph.gstore");
    let store = Store::create(&path, options()).unwrap();
    add_person(&store, "ann");
    assert!(matches!(
        Store::open(&path, options()),
        Err(PersistError::Locked { .. })
    ));
    let info = Store::inspect(&path).unwrap();
    assert_eq!(info.commit_seq, 1);
    add_person(&store, "bob");
    let latest = Store::open_read_only(&path, RecoveryTarget::Latest).unwrap();
    assert_eq!(names(&latest.graph), ["ann", "bob"]);
    assert!(Store::verify_dir(&path).unwrap().is_ok());
}

#[test]
fn convert_copies_a_store_between_a_directory_and_a_file() {
    let temp = Temp::new("convert");
    let dir = temp.join("dir");
    let store = Store::create_with(&dir, varied_graph(), options()).unwrap();
    add_person(&store, "ann");
    store.mark("m").unwrap();
    store.checkpoint(None).unwrap();
    add_person(&store, "bob");
    store
        .rollback_to(RecoveryTarget::Mark("m".into()), RollbackMode::Move)
        .unwrap();
    add_person(&store, "cy");
    let expected = dump(&store.graph().read());
    let attic = store.attic_list().unwrap();
    // A store in use is not copied.
    assert!(matches!(
        Store::convert(&dir, temp.join("x.gstore")),
        Err(PersistError::Locked { .. })
    ));
    store.close().unwrap();
    let file = temp.join("graph.gstore");
    let (files, bytes) = Store::convert(&dir, &file).unwrap();
    assert!(files > 5 && bytes > 0);
    let back = temp.join("back");
    Store::convert(&file, &back).unwrap();
    for location in [file.as_path(), back.as_path()] {
        let store = Store::open(location, options()).unwrap();
        assert_eq!(dump(&store.graph().read()), expected, "{location:?}");
        assert_eq!(store.attic_list().unwrap(), attic, "{location:?}");
        assert_eq!(store.marks().unwrap()[0].name(), "m");
        assert!(store.verify().unwrap().is_ok());
    }
    // Into a non-empty target: refused, nothing written.
    assert!(matches!(
        Store::convert(&dir, &back),
        Err(PersistError::DirectoryNotEmpty { .. })
    ));
}

// ---------------------------------------------------------------------------------------------
// Compaction and its advice
// ---------------------------------------------------------------------------------------------

/// A backend wrapper: counts data reads, and can report a chosen free space.
#[derive(Debug)]
struct Probe {
    inner: Arc<dyn StoreDir>,
    reads: AtomicUsize,
    free: AtomicU64,
}

impl Probe {
    fn new(inner: Arc<dyn StoreDir>) -> Arc<Self> {
        Arc::new(Self {
            inner,
            reads: AtomicUsize::new(0),
            free: AtomicU64::new(u64::MAX),
        })
    }

    fn read(&self) {
        self.reads.fetch_add(1, Ordering::SeqCst);
    }
}

impl StoreDir for Probe {
    fn location(&self) -> String {
        self.inner.location()
    }
    fn local_path(&self) -> Option<&Path> {
        self.inner.local_path()
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
        Probe::read(self);
        self.inner.read(name, max)
    }
    fn read_range(&self, name: &str, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        Probe::read(self);
        self.inner.read_range(name, offset, len)
    }
    fn open_read(&self, name: &str) -> io::Result<Box<dyn io::Read + Send>> {
        Probe::read(self);
        self.inner.open_read(name)
    }
    fn create_dir_all(&self, name: &str) -> io::Result<()> {
        self.inner.create_dir_all(name)
    }
    fn create(&self, name: &str, exclusive: bool) -> io::Result<Box<dyn StoreFile>> {
        self.inner.create(name, exclusive)
    }
    fn open_write(&self, name: &str, offset: u64) -> io::Result<Box<dyn StoreFile>> {
        self.inner.open_write(name, offset)
    }
    fn append(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        self.inner.append(name, bytes)
    }
    fn write_atomic(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        self.inner.write_atomic(name, bytes)
    }
    fn rename(&self, from: &str, to: &str) -> io::Result<()> {
        self.inner.rename(from, to)
    }
    fn truncate(&self, name: &str, len: u64) -> io::Result<()> {
        self.inner.truncate(name, len)
    }
    fn remove_file(&self, name: &str) -> io::Result<()> {
        self.inner.remove_file(name)
    }
    fn remove_tree(&self, name: &str) -> io::Result<()> {
        self.inner.remove_tree(name)
    }
    fn sync_dir(&self, name: &str) -> io::Result<()> {
        self.inner.sync_dir(name)
    }
    fn lock(&self, kind: LockKind) -> io::Result<Option<DirLockGuard>> {
        self.inner.lock(kind)
    }
    fn space_usage(&self) -> io::Result<Option<SpaceUsage>> {
        self.inner.space_usage()
    }
    fn compact(&self) -> io::Result<Option<DirCompaction>> {
        self.inner.compact()
    }
    fn free_space(&self) -> io::Result<Option<u64>> {
        match self.free.load(Ordering::SeqCst) {
            u64::MAX => self.inner.free_space(),
            FREE_SPACE_FAILS => Err(io::Error::other("statvfs failed")),
            free => Ok(Some(free)),
        }
    }
    fn check(&self) -> io::Result<DirCheck> {
        self.inner.check()
    }
}

/// The `Probe::free` value that makes `free_space` fail like a failing `statvfs`.
const FREE_SPACE_FAILS: u64 = u64::MAX - 1;

/// A backend whose free-space query fails: the compaction advice, which consults it, fails
/// with that error instead of reading it as "unknown".
#[test]
fn a_failing_free_space_query_fails_the_compaction_advice() {
    let temp = Temp::new("advice-free-space-fails");
    let probe = Probe::new(Arc::new(SingleFileDir::new(temp.join("graph.gstore"))));
    let dir: Arc<dyn StoreDir> = probe.clone();
    let store = history(&dir, policy());
    probe.free.store(FREE_SPACE_FAILS, Ordering::SeqCst);
    let err = store.compaction_advice().unwrap_err();
    assert!(matches!(err, PersistError::Io { .. }), "{err:?}");
    assert!(err.to_string().contains("free space"), "{err}");
    probe.free.store(u64::MAX, Ordering::SeqCst);
    assert!(store.compaction_advice().is_ok());
    store.close().unwrap();
}

/// A store with history to prune: three snapshots of a graph that shrinks.
fn history(dir: &Arc<dyn StoreDir>, policy: CompactionPolicy) -> Store {
    let store = Store::create(dir, options().with_compaction_policy(policy)).unwrap();
    {
        let mut graph = store.graph().write();
        let mut t = graph.traversal_mut();
        for i in 0..300 {
            t = t
                .add_v("person")
                .property("name", format!("person-{i:04}-{}", "x".repeat(40)));
        }
        t.to_list().unwrap();
    }
    store.checkpoint(None).unwrap();
    store
        .graph()
        .write()
        .traversal_mut()
        .v(None)
        .limit(250)
        .drop()
        .to_list()
        .unwrap();
    store.checkpoint(None).unwrap();
    add_person(&store, "last");
    store.checkpoint(None).unwrap();
    store
}

fn policy() -> CompactionPolicy {
    CompactionPolicy::new()
        .with_min_total_bytes(0)
        .with_min_reclaimable_bytes(4096)
        .with_min_garbage_ratio(0.3)
}

#[test]
fn the_advice_flips_after_prunes_and_back_after_a_compaction() {
    let temp = Temp::new("advice");
    let probe = Probe::new(Arc::new(SingleFileDir::new(temp.join("graph.gstore"))));
    let dir: Arc<dyn StoreDir> = probe.clone();
    let store = history(&dir, policy());
    let expected = dump(&store.graph().read());
    let before = store.compaction_advice().unwrap();
    assert!(before.compactable);
    assert!(before.prune_first && before.prune_bytes > 0, "{before:?}");
    assert!(!before.prune.snapshots_removed.is_empty());
    assert!(before.recommended, "{before:?}");
    assert_eq!(before.decided_by, AdviceRule::ThresholdsMet);
    assert!(before.reason.contains("pruning"), "{}", before.reason);
    assert_eq!(
        before.reclaimable_bytes,
        before.garbage_bytes + before.prune_bytes
    );
    // After the prune the space is garbage in the file, nothing more to prune.
    store.prune(store.graph().read().last_commit_seq()).unwrap();
    let pruned = store.compaction_advice().unwrap();
    assert!(!pruned.prune_first, "{pruned:?}");
    assert!(pruned.garbage_bytes >= before.garbage_bytes + before.prune_bytes / 2);
    assert!(pruned.recommended);
    // Cheap: the advice reads no data.
    probe.reads.store(0, Ordering::SeqCst);
    for _ in 0..10 {
        store.compaction_advice().unwrap();
    }
    assert_eq!(
        probe.reads.load(Ordering::SeqCst),
        0,
        "the advice read data"
    );
    // Not enough free disk space: not recommended, and the advice says why.
    probe.free.store(10, Ordering::SeqCst);
    let full = store.compaction_advice().unwrap();
    assert!(!full.recommended);
    assert_eq!(full.decided_by, AdviceRule::DiskSpace);
    assert_eq!(full.free_space, Some(10));
    assert!(full.space_needed > 10);
    probe.free.store(u64::MAX, Ordering::SeqCst);
    // The compaction gives the garbage back.
    let size = fs::metadata(temp.join("graph.gstore")).unwrap().len();
    let report = store.compact().unwrap();
    let compaction = report.compaction.unwrap();
    assert_eq!(compaction.bytes_before, size);
    assert!(report.bytes_freed() > 0);
    assert!(
        fs::metadata(temp.join("graph.gstore")).unwrap().len() < size,
        "the file shrank"
    );
    let after = store.compaction_advice().unwrap();
    assert!(!after.recommended, "{after:?}");
    assert!(matches!(
        after.decided_by,
        AdviceRule::ReclaimableBytes | AdviceRule::GarbageRatio
    ));
    assert!(after.garbage_bytes < 4096, "{after:?}");
    // The store goes on as before.
    assert_eq!(dump(&store.graph().read()), expected);
    add_person(&store, "more");
    store.close().unwrap();
    let reopened = Store::open(&dir, options()).unwrap();
    assert!(graph_names(&reopened).contains(&"more".to_string()));
    assert!(reopened.verify().unwrap().is_ok());
    // The defaults are conservative: nothing for a small store.
    let default = reopened
        .compaction_advice_with(CompactionPolicy::new())
        .unwrap();
    assert!(!default.recommended);
    assert_eq!(default.decided_by, AdviceRule::TotalBytes);
}

#[test]
fn the_advice_of_a_directory_store_is_about_prune_only() {
    let temp = Temp::new("advice-dir");
    let dir: Arc<dyn StoreDir> = Arc::new(FsDir::new(temp.join("store")));
    let store = history(&dir, policy());
    let advice = store.compaction_advice().unwrap();
    assert!(!advice.compactable);
    assert_eq!(advice.garbage_bytes, 0);
    assert_eq!(advice.space_needed, 0);
    assert!(advice.prune_bytes > 0 && advice.reclaimable_bytes == advice.prune_bytes);
    let report = store.compact().unwrap();
    assert!(report.compaction.is_none());
    assert!(!report.prune.snapshots_removed.is_empty());
    let after = store.compaction_advice().unwrap();
    assert!(!after.recommended);
    assert_eq!(after.prune_bytes, 0);
    // Without a lock, while the writer has it open.
    let outside = Store::compaction_advice_dir(&dir, policy()).unwrap();
    assert_eq!(outside.prune_bytes, 0);
}

/// A WAL-heavy history (report D2): one small vertex whose 16 KiB `blob` is rewritten by every
/// commit, a checkpoint after every second commit, so the old WAL segments hold almost all the
/// bytes and the snapshots stay small.
fn wal_heavy_history(dir: &Arc<dyn StoreDir>) -> Store {
    let store = Store::create(dir, options()).unwrap();
    store
        .graph()
        .write()
        .traversal_mut()
        .add_v("doc")
        .property("blob", "")
        .to_list()
        .unwrap();
    for i in 0..12u64 {
        store
            .graph()
            .write()
            .traversal_mut()
            .v(None)
            .has_label("doc")
            .property("blob", incompressible(i, 16 * 1024))
            .to_list()
            .unwrap();
        if i % 2 == 1 {
            store.checkpoint(None).unwrap();
        }
    }
    store
}

#[test]
fn the_prune_estimate_counts_the_wal_and_equals_what_prune_removes() {
    let temp = Temp::new("advice-wal");
    let policy = CompactionPolicy::new()
        .with_min_total_bytes(0)
        .with_min_reclaimable_bytes(64 * 1024)
        .with_min_garbage_ratio(0.5);
    for backend in [Backend::Dir, Backend::Memory, Backend::File] {
        let dir = backend.make(&temp, "wal-heavy");
        let store = wal_heavy_history(&dir);
        let before = store.compaction_advice_with(policy).unwrap();
        assert!(
            !before.prune.segments_removed.is_empty(),
            "{backend:?}: {before:?}"
        );
        // The WAL segments count: ten rewritten 16 KiB blobs lie before the kept snapshots.
        assert!(
            before.prune_bytes > 8 * 16 * 1024,
            "{backend:?}: the WAL is counted: {before:?}"
        );
        assert!(before.recommended, "{backend:?}: {before:?}");
        assert_eq!(before.decided_by, AdviceRule::ThresholdsMet, "{backend:?}");
        let live_before = dir.space_usage().unwrap().map(|usage| usage.live_bytes);
        let report = store.prune(store.graph().read().last_commit_seq()).unwrap();
        assert_eq!(report.segments_removed, before.prune.segments_removed);
        assert_eq!(report.snapshots_removed, before.prune.snapshots_removed);
        assert_eq!(report.files_retained, before.prune.files_retained);
        let after = store.compaction_advice_with(policy).unwrap();
        assert_eq!(after.prune_bytes, 0, "{backend:?}: {after:?}");
        match (live_before, dir.space_usage().unwrap()) {
            // A single file: the pruned bytes became garbage; the live data shrank by them,
            // within the container's own bookkeeping (the name table, record framing: a few bytes per extent, well below 2 %).
            (Some(live_before), Some(usage)) => {
                let freed = live_before - usage.live_bytes;
                assert!(
                    freed.abs_diff(before.prune_bytes) <= before.prune_bytes / 50 + 4096,
                    "{backend:?}: estimate {} freed {freed}",
                    before.prune_bytes
                );
                assert!(
                    after.recommended,
                    "the garbage is still to compact: {after:?}"
                );
                store.compact().unwrap();
                assert!(!store.compaction_advice_with(policy).unwrap().recommended);
            }
            // A directory: exactly the bytes removed.
            _ => {
                assert_eq!(
                    before.total_bytes - after.total_bytes,
                    before.prune_bytes,
                    "{backend:?}"
                );
                assert!(!after.recommended, "{backend:?}: {after:?}");
            }
        }
        assert!(store.verify().unwrap().is_ok(), "{backend:?}");
    }
}

#[test]
fn a_store_with_nothing_to_prune_says_so() {
    let temp = Temp::new("advice-none");
    for backend in [Backend::Dir, Backend::Memory, Backend::File] {
        let dir = backend.make(&temp, "fresh");
        let store = Store::create(&dir, options()).unwrap();
        add_person(&store, "ann");
        let advice = store.compaction_advice_with(policy()).unwrap();
        assert_eq!(advice.prune_bytes, 0, "{backend:?}: {advice:?}");
        assert!(!advice.prune_first);
        assert!(advice.prune.segments_removed.is_empty());
        assert!(!advice.recommended, "{backend:?}: {advice:?}");
        assert!(!advice.reason.contains("pruning"), "{}", advice.reason);
    }
}

#[test]
fn the_advice_is_false_while_the_store_cannot_change_and_auto_compaction_follows_it() {
    let temp = Temp::new("advice-auto");
    let path = temp.join("graph.gstore");
    let dir: Arc<dyn StoreDir> = Arc::new(SingleFileDir::new(&path));
    let store = history(&dir, policy());
    store.close().unwrap();
    // A backup opened read-only cannot be compacted.
    let backup_path = temp.join("backup.gstore");
    {
        let store = Store::open(&path, options()).unwrap();
        store.backup(&backup_path).unwrap();
    }
    let backup = Store::open_backup(&backup_path, options()).unwrap();
    let advice = backup.compaction_advice_with(policy()).unwrap();
    assert_eq!(advice.decided_by, AdviceRule::NotWritable);
    assert!(!advice.recommended);
    assert!(backup.compact().is_err());
    drop(backup);
    // With auto compaction, a prune that leaves enough garbage compacts the file.
    let store = Store::open(
        &path,
        options()
            .with_compaction_policy(policy())
            .with_auto_compact(true),
    )
    .unwrap();
    let size = fs::metadata(&path).unwrap().len();
    store.prune(store.graph().read().last_commit_seq()).unwrap();
    assert!(fs::metadata(&path).unwrap().len() < size);
    assert!(!store.compaction_advice().unwrap().recommended);
}

// ---------------------------------------------------------------------------------------------
// Crash images of the file
// ---------------------------------------------------------------------------------------------

/// The bytes of a store file that is still open, as a crash would leave them. Byte 0 (the first
/// byte of the preamble magic `GRSLFILE`, written once) is not read: on Windows the writer's OS
/// lock covers it and is mandatory, so another handle cannot read it while the store is open.
fn crash_image(path: &std::path::Path) -> Vec<u8> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = fs::File::open(path).unwrap();
    file.seek(SeekFrom::Start(1)).unwrap();
    let mut bytes = vec![b'G'];
    file.read_to_end(&mut bytes).unwrap();
    bytes
}

#[test]
fn every_cut_of_the_last_update_recovers_the_store() {
    let temp = Temp::new("cuts");
    let path = temp.join("graph.gstore");
    let store = Store::create(&path, options()).unwrap();
    add_person(&store, "ann");
    let complete = fs::metadata(&path).unwrap().len();
    add_person(&store, "bob");
    // The process dies: the file as it is (everything was written to the OS).
    let crashed = crash_image(&path);
    drop(store);
    let image = temp.join("image.gstore");
    let last = crashed.len() as u64 - 1;
    for cut in complete..crashed.len() as u64 {
        fs::write(&image, &crashed[..cut as usize]).unwrap();
        // Every cut: a read (no lock, nothing written) sees the state before the commit.
        let latest = Store::open_read_only(&image, RecoveryTarget::Latest)
            .unwrap_or_else(|e| panic!("cut {cut}: {e}"));
        assert_eq!(names(&latest.graph), ["ann"], "cut {cut}");
        // A sample of the cuts: the store opens, cuts the tail, and goes on (each open, commit
        // and close syncs, so not every offset).
        if cut % 16 != 0 && cut != complete && cut != last {
            continue;
        }
        let store = Store::open(&image, options()).unwrap_or_else(|e| panic!("cut {cut}: {e}"));
        assert!(store.maintenance().is_none(), "cut {cut}");
        assert_eq!(graph_names(&store), ["ann"], "cut {cut}");
        add_person(&store, "cy");
        drop(store);
        let store = Store::open(&image, options()).unwrap();
        assert_eq!(graph_names(&store), ["ann", "cy"], "cut {cut}");
        assert!(store.verify().unwrap().is_ok(), "cut {cut}");
    }
    fs::write(&image, &crashed).unwrap();
    let store = Store::open(&image, options()).unwrap();
    assert_eq!(graph_names(&store), ["ann", "bob"]);
}

#[test]
fn bit_flips_in_slots_tables_and_extents_are_never_silent() {
    let temp = Temp::new("flips");
    let path = temp.join("graph.gstore");
    let store = Store::create_with(&path, varied_graph(), options()).unwrap();
    add_person(&store, "ann");
    store.checkpoint(None).unwrap();
    add_person(&store, "bob");
    let expected = dump(&store.graph().read());
    store.close().unwrap();
    let clean = fs::read(&path).unwrap();
    let image = temp.join("image.gstore");
    // Header slots and the preamble: every byte; the records: every 41st byte.
    let offsets = (0..16u64)
        .chain(4096..4160)
        .chain(8192..8256)
        .chain((12288..clean.len() as u64).step_by(41));
    let (mut silent_same, mut detected) = (0, 0);
    for at in offsets {
        let mut bytes = clean.clone();
        bytes[at as usize] ^= 0x08;
        fs::write(&image, &bytes).unwrap();
        // Read-only (an open writes and syncs `GRAPH`): the scrub and the latest state.
        let verified = Store::verify_dir(&image).is_ok_and(|v| v.is_ok());
        match Store::open_read_only(&image, RecoveryTarget::Latest) {
            Ok(latest) if verified => {
                // Nothing detected: the bit must have changed nothing that counts.
                assert_eq!(dump(&latest.graph), expected, "flip at {at} went unnoticed");
                silent_same += 1;
            }
            _ => detected += 1,
        }
    }
    assert!(detected > 0 && silent_same > 0, "{detected} {silent_same}");
}

#[test]
fn damage_of_the_containers_records_opens_in_maintenance_mode() {
    let temp = Temp::new("container-damage");
    let path = temp.join("graph.gstore");
    let store = Store::create(&path, options()).unwrap();
    let before = fs::metadata(&path).unwrap().len();
    add_person(&store, "ann");
    let ann_record = before;
    add_person(&store, "bob");
    add_person(&store, "cy");
    // A crash image: the records after the last table, synced (EveryCommit).
    let mut crashed = crash_image(&path);
    drop(store);
    // A flipped bit in the frame header of the record of commit 1, which later durable
    // records follow.
    crashed[ann_record as usize + 10] ^= 0x01;
    let image = temp.join("image.gstore");
    fs::write(&image, &crashed).unwrap();
    let store = Store::open(&image, options()).unwrap();
    let report = store.maintenance().expect("maintenance mode");
    assert!(
        report
            .damage
            .iter()
            .any(|d| d.kind == DamageKind::Container),
        "{report}"
    );
    assert!(store.open_report().maintenance);
    let verify = Store::verify_dir(&image).unwrap();
    assert!(
        verify
            .problems
            .iter()
            .any(|p| p.reason.starts_with("container:"))
    );
    // Repair into a new store: the lost commit is reported, the rest is there.
    let repaired = temp.join("repaired.gstore");
    let result = store.repair_to(&repaired).unwrap();
    assert!(!result.lost_commits.is_empty(), "{result:?}");
    drop(store);
    let fixed = Store::open(&repaired, options()).unwrap();
    assert!(fixed.maintenance().is_none());
    assert!(fixed.verify().unwrap().is_ok());
}

#[test]
fn store_damage_and_its_repair_work_on_every_backend() {
    for backend in [Backend::Dir, Backend::Memory, Backend::File] {
        let temp = Temp::new("repair");
        let main = backend.make(&temp, "main");
        let store = Store::create_with(&main, varied_graph(), options()).unwrap();
        add_person(&store, "ann");
        store.checkpoint(None).unwrap();
        add_person(&store, "bob");
        let expected = dump(&store.graph().read());
        store.close().unwrap();
        // A flipped bit in the newest snapshot's first chunk, written through the backend.
        let mut snapshots: Vec<String> = main
            .list("snapshots")
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        snapshots.sort();
        let segment = format!("snapshots/{}/v-000000.seg", snapshots.last().unwrap());
        let mut bytes = main.read(&segment, 1 << 30).unwrap();
        bytes[100] ^= 0x10;
        main.write_atomic(&segment, &bytes).unwrap();
        let damaged = Store::open(&main, options()).unwrap();
        let report = damaged.maintenance().expect("maintenance mode");
        assert!(
            report.damage.iter().any(|d| d.kind == DamageKind::Chunk),
            "{backend:?}: {report}"
        );
        assert_eq!(dump(&damaged.graph().read()), expected, "{backend:?}");
        let repaired = backend.make(&temp, "repaired");
        let result = damaged.repair_to(&repaired).unwrap();
        assert!(result.lost_commits.is_empty(), "{backend:?}: {result:?}");
        drop(damaged);
        let fixed = Store::open(&repaired, options()).unwrap();
        assert!(fixed.maintenance().is_none());
        assert_eq!(dump(&fixed.graph().read()), expected, "{backend:?}");
    }
}

// ---------------------------------------------------------------------------------------------
// The newest WAL segment (format spec 9.8): never a silently shorter history
// ---------------------------------------------------------------------------------------------

/// Copies every name of a store (an image of it as it is now, also while it is open) into
/// `to`, through the backends.
fn image_into(from: &dyn StoreDir, to: &dyn StoreDir, at: &str) {
    if at.is_empty() {
        to.create_dir_all("").unwrap();
    }
    let mut entries = from.list(at).unwrap();
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    for entry in entries {
        let name = if at.is_empty() {
            entry.name.clone()
        } else {
            format!("{at}/{}", entry.name)
        };
        if entry.kind == EntryKind::Dir {
            to.create_dir_all(&name).unwrap();
            image_into(from, to, &name);
        } else if name != "LOCK" && name != "BACKUP" {
            let bytes = from.read(&name, u64::MAX).unwrap();
            to.write_atomic(&name, &bytes).unwrap();
        }
    }
}

/// The WAL segment names of a store, oldest first.
fn segments(dir: &dyn StoreDir) -> Vec<String> {
    let mut names: Vec<String> = dir
        .list("wal")
        .unwrap()
        .into_iter()
        .map(|e| format!("wal/{}", e.name))
        .collect();
    names.sort();
    names
}

/// Opens `dir` and says how: `Ok(commit)` writable without damage, `Err(text)` maintenance
/// mode or a refused open.
fn open_outcome(dir: &Arc<dyn StoreDir>) -> Result<(u64, Option<u64>), String> {
    match Store::open(dir, options()) {
        Ok(store) => match store.maintenance() {
            Some(report) => Err(format!("maintenance: {report}")),
            None => Ok((
                store.graph().read().last_commit_seq(),
                store.open_report().torn_tail.map(|t| t.segment),
            )),
        },
        Err(err) => Err(format!("refused: {err}")),
    }
}

#[test]
fn a_lost_emptied_or_replaced_newest_wal_segment_is_damage_on_every_backend() {
    for backend in [Backend::Dir, Backend::Memory, Backend::File] {
        let temp = Temp::new("wal-head");
        let main = backend.make(&temp, "main");
        let store = Store::create(&main, options()).unwrap();
        for i in 0..30 {
            add_person(&store, &format!("p{i}"));
            if i == 5 || i == 15 {
                store.checkpoint(None).unwrap();
            }
        }
        let last = store.graph().read().last_commit_seq();
        // A crash image (the store still open) and, after the close, a clean one.
        let crashed = backend.make(&temp, "crashed");
        image_into(main.as_ref(), crashed.as_ref(), "");
        store.close().unwrap();
        let closed = backend.make(&temp, "closed");
        image_into(main.as_ref(), closed.as_ref(), "");
        let names = segments(main.as_ref());
        assert!(names.len() >= 3, "{backend:?}: {names:?}");
        let newest = names.last().unwrap().clone();
        let older = names[names.len() - 2].clone();

        let mut n = 0;
        for (image_kind, image) in [("crashed", &crashed), ("closed", &closed)] {
            type Damage = fn(&dyn StoreDir, &str, &str);
            let damages: [(&str, Damage); 3] = [
                ("remove", |dir, newest, _| dir.remove_file(newest).unwrap()),
                ("empty", |dir, newest, _| {
                    dir.write_atomic(newest, &[]).unwrap()
                }),
                ("replace", |dir, newest, older| {
                    let bytes = dir.read(older, u64::MAX).unwrap();
                    dir.write_atomic(newest, &bytes).unwrap();
                }),
            ];
            for (what, damage) in damages {
                n += 1;
                let copy = backend.make(&temp, &format!("copy{n}"));
                image_into(image.as_ref(), copy.as_ref(), "");
                damage(copy.as_ref(), &newest, &older);
                let outcome = open_outcome(&copy);
                assert!(
                    outcome.is_err(),
                    "{backend:?} {image_kind} {what} {newest}: opened {outcome:?}"
                );
                let verify = Store::verify_dir(&copy).unwrap();
                assert!(
                    !verify.is_ok(),
                    "{backend:?} {image_kind} {what}: verify ok"
                );
                let report = Store::damage_report_dir(&copy).unwrap();
                assert!(!report.damage.is_empty(), "{backend:?} {image_kind} {what}");
            }
        }
        // A clean close records its last commit: a cut at a record boundary is damage too.
        let copy = backend.make(&temp, "cut-closed");
        image_into(closed.as_ref(), copy.as_ref(), "");
        let len = copy.file_len(&newest).unwrap();
        let bytes = copy.read(&newest, u64::MAX).unwrap();
        // The last record's start: the newest segment re-read up to its last frame.
        let last_record = (64..len as usize)
            .rev()
            .find(|&at| bytes[at..].starts_with(&0x4752_5357u32.to_le_bytes()))
            .unwrap();
        copy.truncate(&newest, last_record as u64).unwrap();
        assert!(
            open_outcome(&copy).is_err(),
            "{backend:?}: a cut clean store opened"
        );

        // Legitimate: an interrupted last write of a crashed store is a torn tail.
        let copy = backend.make(&temp, "torn");
        image_into(crashed.as_ref(), copy.as_ref(), "");
        copy.truncate(&newest, len - 5).unwrap();
        let (commit, torn) = open_outcome(&copy).unwrap_or_else(|e| panic!("{backend:?}: {e}"));
        assert_eq!(commit, last - 1, "{backend:?}");
        assert!(torn.is_some(), "{backend:?}");
        // Legitimate: a segment created but never written (a crash right after it was
        // started, before GRAPH recorded it) is a torn tail.
        let copy = backend.make(&temp, "fresh-segment");
        image_into(crashed.as_ref(), copy.as_ref(), "");
        let next = format!("wal/{:020}.wal", last + 1);
        copy.write_atomic(&next, b"GRSLWALS\x01\x00").unwrap();
        let (commit, torn) = open_outcome(&copy).unwrap_or_else(|e| panic!("{backend:?}: {e}"));
        assert_eq!((commit, torn), (last, Some(last + 1)), "{backend:?}");
        // The undamaged images open at the last commit.
        assert_eq!(open_outcome(&crashed).map(|o| o.0), Ok(last));
        assert_eq!(open_outcome(&closed).map(|o| o.0), Ok(last));
    }
}

/// Rewrites `GRAPH` and `GRAPH.copy` of a closed store as a store of format version 1 wrote
/// them (before the store id: version word 1, offset 100 zero), with a valid checksum.
fn make_older_format(dir: &Arc<dyn StoreDir>) {
    for name in ["GRAPH", "GRAPH.copy"] {
        let mut bytes = dir.read(name, 1 << 20).unwrap();
        bytes[8..12].copy_from_slice(&1u32.to_le_bytes());
        bytes[100..116].fill(0);
        let crc = crate::store_tests::crc32c(&bytes[..252]);
        bytes[252..256].copy_from_slice(&crc.to_le_bytes());
        dir.write_atomic(name, &bytes).unwrap();
    }
}

fn assert_older<T: std::fmt::Debug>(result: Result<T, PersistError>, what: &str) {
    match result {
        Err(err @ PersistError::OlderFormat { found: 1, .. }) => {
            assert_eq!(graphersal::persist::STORE_FORMAT_VERSION, 3);
            let text = graphersal::error::format_diagnostic(&err);
            assert!(text.contains("older format version 1"), "{what}: {text}");
            for part in [
                "g.export_snapshot(\"graph.gsnap\")",
                "graphersal store create <new_dir> --from graph.gsnap",
                "marks, attic entries and backups are not carried",
            ] {
                assert!(text.contains(part), "{what}: {part} missing in {text}");
            }
        }
        other => panic!("{what}: expected OlderFormat, got {other:?}"),
    }
}

#[test]
fn a_store_of_an_older_format_is_refused_with_a_migration_recipe_on_every_backend() {
    for backend in [Backend::Dir, Backend::Memory, Backend::File] {
        let temp = Temp::new("older");
        let main = backend.make(&temp, "main");
        let backup = backend.make(&temp, "backup");
        let store = Store::create_with(&main, varied_graph(), options()).unwrap();
        add_person(&store, "ann");
        store.backup(&backup).unwrap();
        store.close().unwrap();
        make_older_format(&main);
        make_older_format(&backup);
        let other = |name: &str| backend.make(&temp, name);
        let what = |op: &str| format!("{backend:?} {op}");

        assert_older(Store::open(&main, options()).map(|_| ()), &what("open"));
        assert_older(
            Store::open_read_only(&main, RecoveryTarget::Latest).map(|_| ()),
            &what("open_read_only"),
        );
        assert_older(Store::inspect(&main), &what("inspect (store info)"));
        assert_older(Store::verify_dir(&main), &what("verify"));
        assert_older(Store::damage_report_dir(&main), &what("damage report"));
        assert_older(Store::snapshots_dir(&main), &what("list"));
        assert_older(Store::marks_dir(&main), &what("marks"));
        assert_older(Store::attic_list_dir(&main), &what("attic"));
        assert_older(
            Store::export_packed_dir(&main, None, Vec::new()),
            &what("export"),
        );
        assert_older(
            Store::checkpoint_dir(&main, options(), None),
            &what("checkpoint"),
        );
        assert_older(
            Store::compaction_advice_dir(&main, CompactionPolicy::default()),
            &what("compaction advice"),
        );
        assert_older(
            Store::fork_dir(&main, RecoveryTarget::Latest, other("fork")),
            &what("fork"),
        );
        assert_older(Store::repair_dir(&main, other("repair")), &what("repair"));
        assert_older(Store::convert(&main, other("convert")), &what("convert"));
        assert_older(Store::backup_dir(&main, other("bk2")), &what("backup"));
        assert_older(
            Store::open_backup(&backup, options()).map(|_| ()),
            &what("open_backup"),
        );
        assert_older(Store::restore_backup(&backup), &what("restore_backup"));
        assert_older(Store::prune_backup(&backup, 1), &what("prune_backup"));
    }
}

/// Text LZ4 cannot shrink much (a commit of it is about `len` WAL bytes).
fn incompressible(seed: u64, len: usize) -> String {
    let mut x = seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
    (0..len)
        .map(|_| {
            x = x
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (b'a' + ((x >> 33) % 26) as u8) as char
        })
        .collect()
}

/// Several back-to-back commits, each larger than the automatic checkpoint's threshold: the
/// background checkpoint eventually covers the last one without any further commit (the
/// user-reported WAL growth: commits landing while a checkpoint ran never woke it again).
#[test]
fn back_to_back_large_commits_are_all_checkpointed_automatically_on_every_backend() {
    let temp = Temp::new("auto-checkpoint");
    for backend in [Backend::Dir, Backend::Memory, Backend::File] {
        let dir = backend.make(&temp, &format!("auto-{backend:?}"));
        let store =
            Store::create(&dir, StoreOptions::new().with_checkpoint_after_bytes(4096)).unwrap();
        for n in 0..12 {
            add_person(&store, &incompressible(n, 20_000));
        }
        let last = store.graph().read().last_commit_seq();
        let start = std::time::Instant::now();
        // The snapshot appears before its checkpoint lowers the WAL counter: wait for both.
        while store.snapshots().unwrap().last().map(|s| s.commit_seq) != Some(last)
            || store.wal_bytes_since_checkpoint() >= 4096
        {
            assert!(
                start.elapsed() < std::time::Duration::from_secs(30),
                "{backend:?}: no snapshot at commit {last}; WAL since the last one: {} bytes, \
                 error: {:?}",
                store.wal_bytes_since_checkpoint(),
                store.last_auto_checkpoint_error()
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            store.last_auto_checkpoint_error().unwrap().is_none(),
            "{backend:?}"
        );
        store.close().unwrap();
        let reopened = Store::open(&dir, options()).unwrap();
        assert_eq!(graph_names(&reopened).len(), 12, "{backend:?}");
    }
}
