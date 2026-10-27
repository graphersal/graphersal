//! Verified backups, backups from memory and the damage policy (the BK1 test matrix; format
//! spec 5.2 bit 5, 12.6-12.8, 12.11, 13.3): every backup (full, increment, ZIP) reads every
//! checksum of what it copies in the store and again in the copy; damage stops it with a
//! report, removes a new backup or rolls an increment back. The ORACLE of every sunny case: the
//! backup, loaded, is exactly the store's state at the backup's position (identity, lineage,
//! position, time, auto-id sequences, schema, catalog, every element with its labels and
//! properties, the statistics). A backup from memory writes nothing to the store. The damage
//! policy is fixed at creation and carried by every copy.
#![cfg(all(feature = "persist", feature = "script"))]

use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use graphersal::persist::{
    BackupMode, BackupReport, DamagePolicy, DamageSide, DirEntry, DirLockGuard, EntryKind,
    LockKind, MemDir, PersistError, RecoveryTarget, RollbackMode, Store, StoreDir, StoreFile,
    StoreOptions,
};
use graphersal::prelude::*;

use crate::merge_checkpoint_tests::{
    Faults, Hooked, Rng, exact, fake_data, history, listing, oracle, same,
};

// ---------------------------------------------------------------------------------------------
// Fixtures and the oracle
// ---------------------------------------------------------------------------------------------

/// Small WAL segments and chunks: a store of a few KB spans several of each.
pub(crate) fn options() -> StoreOptions {
    StoreOptions::new()
        .with_wal_segment_bytes(700)
        .with_chunk_bytes(600)
        .with_snapshot_segment_bytes(2_000)
        .with_checkpoint_after_bytes(0)
}

pub(crate) fn shared(dir: &MemDir) -> Arc<dyn StoreDir> {
    Arc::new(dir.clone())
}

/// A store with history: data, a checkpoint, more commits, a mark, more commits (the WAL after
/// the snapshot spans several segments and holds a Mark and a Checkpoint record).
pub(crate) fn populated(dir: &MemDir, options: StoreOptions) -> Store {
    let store = Store::create(dir, options).unwrap();
    fake_data(store.graph(), 7, 12);
    store.checkpoint(None).unwrap();
    let mut rng = Rng(11);
    history(store.graph(), &mut rng, 12, 0);
    store.mark("half").unwrap();
    history(store.graph(), &mut rng, 12, 12);
    store
}

/// The backup in `dir` loaded at its latest state.
/// The backup kinds a test runs: a directory always, a ZIP archive with `persist-zip`.
#[cfg(feature = "persist-zip")]
const ZIP_MODES: &[bool] = &[false, true];
#[cfg(not(feature = "persist-zip"))]
const ZIP_MODES: &[bool] = &[false];

fn loaded(dir: &Arc<dyn StoreDir>) -> TraversalGraph {
    Store::open_read_only(dir, RecoveryTarget::Latest)
        .unwrap()
        .graph
}

/// The backup in `backup` is exactly `expected`, verifies, and is marked at its position.
fn assert_backup(backup: &Arc<dyn StoreDir>, expected: &TraversalGraph, label: &str) {
    same(&loaded(backup), expected, label);
    let verify = Store::verify_dir(backup).unwrap();
    assert!(verify.is_ok(), "{label}: {:?}", verify.problems);
    let marker = Store::backup_marker_dir(backup).unwrap().unwrap();
    assert_eq!(marker.commit_seq, expected.last_commit_seq(), "{label}");
}

/// [`assert_backup`] against the [`exact`] text of the expected graph.
fn assert_backup_exact(backup: &Arc<dyn StoreDir>, expected: &[String], label: &str) {
    assert_eq!(exact(&loaded(backup)), expected, "{label}");
    let verify = Store::verify_dir(backup).unwrap();
    assert!(verify.is_ok(), "{label}: {:?}", verify.problems);
    assert!(
        Store::backup_marker_dir(backup).unwrap().is_some(),
        "{label}"
    );
}

/// The `BackupDamaged` details of a result (panics on anything else).
fn damaged(
    result: Result<BackupReport, PersistError>,
    label: &str,
) -> Box<graphersal::persist::BackupDamage> {
    match result {
        Err(PersistError::BackupDamaged(damage)) => damage,
        Err(other) => panic!("{label}: expected BackupDamaged, got {other}"),
        Ok(report) => panic!("{label}: expected BackupDamaged, got {report:?}"),
    }
}

/// The flag word of a `GRAPH` file (format spec 5.1, offset 48).
fn graph_flags(dir: &MemDir) -> u32 {
    let bytes = dir.file("GRAPH").unwrap();
    u32::from_le_bytes(bytes[48..52].try_into().unwrap())
}

/// Commits one vertex; the error as its diagnostic text.
fn commit(store: &Store, name: &str) -> Result<(), String> {
    store
        .graph()
        .write()
        .traversal_mut()
        .add_v("x")
        .property("name", name)
        .to_list()
        .map(|_| ())
        .map_err(|err| graphersal::error::format_diagnostic(&err))
}

// ---------------------------------------------------------------------------------------------
// Sunny day: the oracle on every backend and kind
// ---------------------------------------------------------------------------------------------

/// Matrix S1 + S3: a full backup (directory, single file, memory as target; ZIP) of an open
/// store and of a closed one equals the store at the backup's position, verifies, restores.
#[test]
fn a_full_backup_equals_the_store_on_every_backend() {
    let temp = std::env::temp_dir().join(format!("bk1-full-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(&temp).unwrap();
    let source = MemDir::new();
    let store = populated(&source, options());
    let expected = oracle(&shared(&source));
    let targets: Vec<Arc<dyn StoreDir>> = vec![
        shared(&MemDir::new()),
        graphersal::persist::store_dir_at(temp.join("dir")),
        Arc::new(graphersal::persist::SingleFileDir::new(
            temp.join("one.gstore"),
        )),
    ];
    for target in &targets {
        let report = store.backup(target).unwrap();
        assert!(!report.incremental && !report.from_memory);
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        assert_backup(target, &expected, &target.location());
    }
    // ZIP: written, read back and checked; restored it is the live store.
    #[cfg(feature = "persist-zip")]
    {
        let mut zip = io::Cursor::new(Vec::new());
        store.backup_zip(&mut zip).unwrap();
        let restored = MemDir::new();
        Store::restore_zip(zip.get_ref().as_slice(), &restored).unwrap();
        same(&oracle(&shared(&restored)), &expected, "zip");
    }
    // A closed store (backup by another process: no durable end).
    store.close().unwrap();
    let closed = MemDir::new();
    Store::backup_dir(&source, &closed).unwrap();
    assert_backup(&shared(&closed), &expected, "closed");
    #[cfg(feature = "persist-zip")]
    {
        let mut zip = io::Cursor::new(vec![7u8; 3]);
        zip.set_position(3);
        Store::backup_zip_dir(&source, &mut zip).unwrap();
        let restored = MemDir::new();
        Store::restore_zip(&zip.get_ref()[3..], &restored).unwrap();
        same(&oracle(&shared(&restored)), &expected, "zip at an offset");
    }
    // Restored in place, the backup is the live store.
    Store::restore_backup(&closed).unwrap();
    let live = Store::open(&closed, options()).unwrap();
    same(&live.graph().read(), &expected, "restored");
    drop(live);
    std::fs::remove_dir_all(&temp).unwrap();
}

/// Matrix S2: increments (a tail, new segments, new snapshots incl. incremental ones with
/// referenced files, after a rollback, nothing new) equal the store at their position.
#[test]
fn increments_equal_the_store_at_every_position() {
    let source = MemDir::new();
    let store = populated(&source, options());
    let backup = MemDir::new();
    store.backup(&backup).unwrap();
    let mut rng = Rng(5);
    for round in 0..6 {
        history(store.graph(), &mut rng, 8, 100 + round * 8);
        match round {
            1 | 3 => {
                store.checkpoint(None).unwrap();
            }
            4 => {
                let at = store.graph().read().last_commit_seq() - 3;
                store
                    .rollback_to(RecoveryTarget::CommitSeq(at), RollbackMode::Move)
                    .unwrap();
                history(store.graph(), &mut rng, 4, 500);
            }
            _ => {}
        }
        let expected = oracle(&shared(&source));
        let report = store.backup(&backup).unwrap();
        assert!(report.incremental, "round {round}");
        assert_backup(&shared(&backup), &expected, &format!("round {round}"));
    }
    // Nothing new: the backup is current, nothing copied.
    let report = store.backup(&backup).unwrap();
    assert_eq!(report.bytes, 0);
}

/// Matrix S2 (rainy): a rollback of a later lineage to BEFORE its own start branches from the
/// backup's lineage at an earlier commit than the backup holds: the increment is refused as a
/// rollback behind the backup (found by the randomized test: only the next branch point was
/// compared before), and a backup from memory is refused the same way.
#[test]
fn an_increment_after_a_rollback_behind_the_backup_through_two_lineages_is_refused() {
    let source = MemDir::new();
    let store = Store::create(&source, options()).unwrap();
    fake_data(store.graph(), 1, 6);
    store.checkpoint(None).unwrap();
    let mut rng = Rng(2);
    history(store.graph(), &mut rng, 10, 0);
    let backup = MemDir::new();
    store.backup(&backup).unwrap();
    let position = store.graph().read().last_commit_seq();
    history(store.graph(), &mut rng, 10, 10);
    store
        .rollback_to(RecoveryTarget::CommitSeq(position + 3), RollbackMode::Move)
        .unwrap();
    history(store.graph(), &mut rng, 3, 20);
    store
        .rollback_to(RecoveryTarget::CommitSeq(position - 2), RollbackMode::Move)
        .unwrap();
    history(store.graph(), &mut rng, 8, 30);
    assert!(store.graph().read().last_commit_seq() > position);
    let before = listing(&backup);
    match store.backup(&backup) {
        Err(PersistError::BackupRefused { reason, .. }) => {
            assert!(reason.contains("rolled back in place"), "{reason}")
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(listing(&backup), before);
}

/// Matrix S4: a store another process writes, whose last record is torn: the backup takes the
/// WAL up to the last complete record.
#[test]
fn a_torn_last_record_of_a_store_in_use_is_left_out() {
    let source = MemDir::new();
    let store = populated(&source, options());
    let expected = oracle(&shared(&source));
    store.close().unwrap();
    let (last, _) = wal_files(&source).pop().unwrap();
    let mut bytes = source.file(&last).unwrap();
    for cut in 1..25 {
        let image = source.deep_copy();
        let mut torn = bytes.clone();
        torn.extend_from_slice(&[0x57, 0x53, 0x52, 0x47, 9, 9, 9][..cut.min(7)]);
        torn.resize(bytes.len() + cut, 0);
        image.put_file(&last, torn);
        let target = MemDir::new();
        Store::backup_dir(&image, &target).unwrap();
        assert_backup(&shared(&target), &expected, &format!("cut {cut}"));
    }
    bytes.truncate(bytes.len() - 1);
    // A cut INSIDE the last record of a closed store: the record is lost (a torn tail).
    let image = source.deep_copy();
    image.put_file(&last, bytes);
    let target = MemDir::new();
    let report = Store::backup_dir(&image, &target).unwrap();
    assert_eq!(report.commit_seq, oracle(&shared(&image)).last_commit_seq());
    // A newest segment shorter than its header that GRAPH does not record (a crash while it
    // was being started, format spec 9.8): it holds nothing and is left out.
    let image = source.deep_copy();
    image.put_file("wal/00000000000000099999.wal", vec![0x47, 0x52]);
    let target = MemDir::new();
    Store::backup_dir(&image, &target).unwrap();
    assert_backup(&shared(&target), &expected, "a segment being started");
}

/// The WAL segment files of a store, in order.
fn wal_files(dir: &MemDir) -> Vec<(String, usize)> {
    let mut out: Vec<(String, usize)> = dir
        .file_names()
        .into_iter()
        .filter(|n| n.starts_with("wal/") && n.ends_with(".wal"))
        .map(|n| {
            let len = dir.file(&n).unwrap().len();
            (n, len)
        })
        .collect();
    out.sort();
    out
}

/// Matrix S5: damage of redundant metadata (one GRAPH copy, one copy of the newest manifest's
/// fixed part, the derived marks file) is a warning: the backup is intact (fresh GRAPH copies,
/// marks rebuilt from the WAL) and equals the store.
#[test]
fn damaged_redundant_metadata_is_a_warning() {
    let source = MemDir::new();
    let store = populated(&source, options());
    let expected = oracle(&shared(&source));
    store.close().unwrap();
    let newest_manifest = source
        .file_names()
        .into_iter()
        .filter(|n| n.ends_with("/manifest") && n.starts_with("snapshots/"))
        .max()
        .unwrap();
    let manifest_len = source.file(&newest_manifest).unwrap().len();
    for (file, at) in [
        ("GRAPH".to_string(), 20),
        ("GRAPH.copy".to_string(), 20),
        ("marks".to_string(), 9),
        (newest_manifest.clone(), manifest_len - 100),
    ] {
        let image = source.deep_copy();
        let mut bytes = image.file(&file).unwrap();
        bytes[at] ^= 0x10;
        image.put_file(&file, bytes);
        for zip in ZIP_MODES {
            let report = if *zip {
                #[cfg(feature = "persist-zip")]
                {
                    let mut archive = io::Cursor::new(Vec::new());
                    let report = Store::backup_zip_dir(&image, &mut archive).unwrap();
                    let restored = MemDir::new();
                    Store::restore_zip(archive.get_ref().as_slice(), &restored).unwrap();
                    same(&oracle(&shared(&restored)), &expected, &file);
                    report
                }
                #[cfg(not(feature = "persist-zip"))]
                unreachable!("ZIP_MODES has no ZIP mode without persist-zip")
            } else {
                let target = MemDir::new();
                let report = Store::backup_dir(&image, &target).unwrap();
                assert_backup(&shared(&target), &expected, &file);
                if file == "marks" {
                    let marks = Store::marks_dir(&target).unwrap();
                    assert_eq!(marks.len(), 1, "marks rebuilt from the WAL");
                }
                report
            };
            assert!(!report.warnings.is_empty(), "{file}: no warning");
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Rainy day: damage in the store, of every file and region
// ---------------------------------------------------------------------------------------------

/// The byte positions flipped in a file of `len` bytes: every one of the first and last 96
/// (headers, frames, footers, the tail copy of a manifest), every 7th in between.
fn positions(len: usize) -> Vec<usize> {
    (0..len)
        .filter(|&at| at < 96 || at + 96 >= len || at % 7 == 0)
        .collect()
}

/// The files a backup of the store in `dir` reads: GRAPH, its copy, marks, the newest
/// snapshot's files (with the files it references), the WAL from its replay set on.
fn scope(dir: &MemDir) -> Vec<String> {
    let files = dir.file_names();
    let newest = files
        .iter()
        .filter(|n| n.starts_with("snapshots/") && n.ends_with("/manifest"))
        .max()
        .unwrap()
        .trim_end_matches("/manifest")
        .to_string();
    let seq: u64 = newest.trim_start_matches("snapshots/").parse().unwrap();
    let wal = wal_files(dir);
    let first = wal
        .iter()
        .rposition(|(n, _)| {
            n.trim_start_matches("wal/")
                .trim_end_matches(".wal")
                .parse::<u64>()
                .unwrap()
                <= seq + 1
        })
        .unwrap_or(0);
    let mut out = vec!["GRAPH".to_string(), "GRAPH.copy".into(), "marks".into()];
    out.extend(
        files
            .iter()
            .filter(|n| n.starts_with(&format!("{newest}/")))
            .cloned(),
    );
    out.extend(wal[first..].iter().map(|(n, _)| n.clone()));
    out
}

/// Matrix R1: every region of every file a backup reads, flipped in a closed store: the backup
/// is refused with the damage of the STORE named (and recommends repairing the store), and
/// nothing of it is left; or the damage is in redundant metadata or in bytes a backup does not
/// need (a warning, or none) and the backup equals the store. Never a wrong backup, never a
/// panic. Directory and ZIP.
#[test]
fn every_damaged_region_of_the_store_stops_the_backup() {
    let source = MemDir::new();
    let store = populated(&source, options());
    let expected = oracle(&shared(&source));
    store.close().unwrap();
    let (mut refused, mut accepted) = (0, 0);
    for file in scope(&source) {
        let bytes = source.file(&file).unwrap();
        for at in positions(bytes.len()) {
            let image = source.deep_copy();
            let mut flipped = bytes.clone();
            flipped[at] ^= 1 << (at % 8);
            image.put_file(&file, flipped);
            let label = format!("{file} byte {at}");
            let target = MemDir::new();
            match Store::backup_dir(&image, &target) {
                Err(PersistError::BackupDamaged(damage)) => {
                    assert_eq!(damage.side, DamageSide::Store, "{label}");
                    assert!(!damage.problems.is_empty(), "{label}");
                    assert!(target.file_names().is_empty(), "{label}: leftovers");
                    let text =
                        graphersal::error::format_diagnostic(&PersistError::BackupDamaged(damage));
                    assert!(text.contains("store repair"), "{label}: {text}");
                    refused += 1;
                }
                Ok(report) => {
                    assert_backup(&shared(&target), &expected, &label);
                    if file.starts_with("GRAPH") || file == "marks" {
                        assert!(!report.warnings.is_empty(), "{label}: no warning");
                    }
                    accepted += 1;
                }
                Err(other) => panic!("{label}: {other}"),
            }
            // The ZIP of the same image: the same verdict, nothing to keep on refusal.
            #[cfg(feature = "persist-zip")]
            let mut archive = io::Cursor::new(Vec::new());
            #[cfg(feature = "persist-zip")]
            match Store::backup_zip_dir(&image, &mut archive) {
                Err(PersistError::BackupDamaged(damage)) => {
                    assert_eq!(damage.side, DamageSide::Store, "zip {label}");
                    assert!(archive.get_ref().is_empty(), "zip {label}: bytes written");
                }
                Ok(_) => {
                    let restored = MemDir::new();
                    Store::restore_zip(archive.get_ref().as_slice(), &restored).unwrap();
                    same(
                        &oracle(&shared(&restored)),
                        &expected,
                        &format!("zip {label}"),
                    );
                }
                Err(other) => panic!("zip {label}: {other}"),
            }
        }
    }
    assert!(
        refused > 500 && accepted > 10,
        "{refused} refused, {accepted} accepted"
    );
}

/// Matrix R1 for increments: every region of every file an increment reads, flipped: refused
/// with the store's damage and the backup exactly as it was, or accepted and equal to the
/// store.
#[test]
fn every_damaged_region_stops_an_increment_and_leaves_the_backup() {
    let source = MemDir::new();
    let store = populated(&source, options());
    let base = MemDir::new();
    store.backup(&base).unwrap();
    let mut rng = Rng(3);
    history(store.graph(), &mut rng, 10, 50);
    store.checkpoint(None).unwrap();
    history(store.graph(), &mut rng, 10, 60);
    let expected = oracle(&shared(&source));
    store.close().unwrap();
    let before = listing(&base);
    let mut files = scope(&source);
    // The tail of the backup's last segment comes from the store's segment of that name.
    for (name, _) in wal_files(&base) {
        if !files.contains(&name) {
            files.push(name);
        }
    }
    let mut refused = 0;
    for file in files {
        let bytes = source.file(&file).unwrap();
        for at in positions(bytes.len()).into_iter().step_by(3) {
            let image = source.deep_copy();
            let mut flipped = bytes.clone();
            flipped[at] ^= 0x04;
            image.put_file(&file, flipped);
            let label = format!("{file} byte {at}");
            let backup = base.deep_copy();
            match Store::backup_dir(&image, &backup) {
                Err(PersistError::BackupDamaged(damage)) => {
                    assert_eq!(damage.side, DamageSide::Store, "{label}");
                    assert_eq!(listing(&backup), before, "{label}: the backup changed");
                    refused += 1;
                }
                // A prefix check or a refusal before anything is read (a damaged header of
                // the backup's last segment in the store).
                Err(PersistError::BackupRefused { .. }) => {
                    assert_eq!(listing(&backup), before, "{label}: the backup changed");
                }
                Ok(_) => assert_backup(&shared(&backup), &expected, &label),
                Err(other) => panic!("{label}: {other}"),
            }
        }
    }
    assert!(refused > 100, "{refused}");
}

/// Matrix R2: missing and cut files of the store: a missing segment file, schema, the newest
/// WAL segment (GRAPH records it), a cut non-last WAL segment, a gap in the commits.
#[test]
fn missing_and_cut_files_stop_the_backup() {
    let source = MemDir::new();
    let store = populated(&source, options());
    store.close().unwrap();
    let files = scope(&source);
    let segment = files.iter().find(|n| n.ends_with(".seg")).unwrap().clone();
    let schema = files
        .iter()
        .find(|n| n.ends_with("schema.json"))
        .unwrap()
        .clone();
    let wal = wal_files(&source);
    assert!(wal.len() >= 3, "{wal:?}");
    let image_without = |name: &str| {
        let image = source.deep_copy();
        let kept: Vec<(String, Vec<u8>)> = listing(&image)
            .into_iter()
            .filter(|(n, _)| n != name)
            .collect();
        let fresh = MemDir::new();
        for (n, bytes) in kept {
            fresh.put_file(&n, bytes);
        }
        fresh
    };
    for missing in [segment, schema, wal.last().unwrap().0.clone()] {
        let image = image_without(&missing);
        let target = MemDir::new();
        let damage = damaged(Store::backup_dir(&image, &target), &missing);
        assert_eq!(damage.side, DamageSide::Store);
        assert!(target.file_names().is_empty(), "{missing}");
    }
    // A WAL segment that is not the last one, cut anywhere inside its last record.
    let (middle, len) = wal[wal.len() - 2].clone();
    let bytes = source.file(&middle).unwrap();
    for cut in (len - 30)..len {
        let image = source.deep_copy();
        image.put_file(&middle, bytes[..cut].to_vec());
        let damage = damaged(
            Store::backup_dir(&image, MemDir::new()),
            &format!("{middle} cut at {cut}"),
        );
        assert!(
            damage.problems.iter().any(|p| p.path == middle),
            "{:?}",
            damage.problems
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Rainy day: damage of the copy, failures while writing
// ---------------------------------------------------------------------------------------------

/// A [`MemDir`] that flips one bit of the `at`-th write (a file write or a whole-file
/// replacement) it sees: a disk that returns other bytes than were written.
#[derive(Debug, Clone)]
struct Corrupting {
    inner: MemDir,
    at: u64,
    count: Arc<AtomicU64>,
    fired: Arc<AtomicBool>,
}

impl Corrupting {
    fn new(inner: MemDir, at: u64) -> Self {
        Self {
            inner,
            at,
            count: Arc::new(AtomicU64::new(0)),
            fired: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Flips a bit of `bytes` when this is the chosen write.
    fn maybe_flip(&self, bytes: &[u8]) -> Vec<u8> {
        let mut out = bytes.to_vec();
        if !out.is_empty() && self.count.fetch_add(1, Ordering::SeqCst) == self.at {
            let at = out.len() / 2;
            out[at] ^= 0x20;
            self.fired.store(true, Ordering::SeqCst);
        }
        out
    }
}

struct CorruptingFile {
    inner: Box<dyn StoreFile>,
    dir: Corrupting,
}

impl io::Write for CorruptingFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let bytes = self.dir.maybe_flip(buf);
        self.inner.write_all(&bytes)?;
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl StoreFile for CorruptingFile {
    fn sync(&mut self) -> io::Result<()> {
        self.inner.sync()
    }
    fn set_len(&mut self, len: u64) -> io::Result<()> {
        self.inner.set_len(len)
    }
}

impl StoreDir for Corrupting {
    fn location(&self) -> String {
        "memory:corrupting".into()
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
        self.inner.create_dir_all(name)
    }
    fn create(&self, name: &str, exclusive: bool) -> io::Result<Box<dyn StoreFile>> {
        Ok(Box::new(CorruptingFile {
            inner: self.inner.create(name, exclusive)?,
            dir: self.clone(),
        }))
    }
    fn open_write(&self, name: &str, offset: u64) -> io::Result<Box<dyn StoreFile>> {
        Ok(Box::new(CorruptingFile {
            inner: self.inner.open_write(name, offset)?,
            dir: self.clone(),
        }))
    }
    fn append(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        self.inner.append(name, &self.maybe_flip(bytes))
    }
    fn write_atomic(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        self.inner.write_atomic(name, &self.maybe_flip(bytes))
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
}

/// A backup into a target directory.
type BackupInto<'a> = dyn Fn(&Arc<dyn StoreDir>) -> Result<BackupReport, PersistError> + 'a;

/// `dir` holds exactly the files of `before` (names the ones that differ).
fn assert_same_files(dir: &MemDir, before: &[(String, Vec<u8>)], label: &str) {
    let now = listing(dir);
    if now != before {
        let names = |l: &[(String, Vec<u8>)]| -> Vec<String> {
            l.iter()
                .map(|(n, b)| format!("{n} ({} bytes)", b.len()))
                .collect()
        };
        let changed: Vec<String> = now
            .iter()
            .filter(|f| !before.contains(f))
            .map(|(n, _)| n.clone())
            .collect();
        panic!(
            "{label}: the files changed: {changed:?}\nnow {:?}\nbefore {:?}",
            names(&now),
            names(before)
        );
    }
}

/// Runs `backup` into a corrupting copy of `base` (a fresh directory when `base` is empty),
/// flipping a bit in each write in turn: every damaged copy is refused as the BACKUP's damage
/// (the store is fine) and leaves the target as it was; an undamaged result equals the oracle.
fn corrupt_every_write(base: &MemDir, expected: &[String], backup: &BackupInto, label: &str) {
    let before = listing(base);
    let mut refused = 0;
    for at in 0.. {
        let image = base.deep_copy();
        let corrupting = Corrupting::new(image.clone(), at);
        let target: Arc<dyn StoreDir> = Arc::new(corrupting.clone());
        let result = backup(&target);
        if !corrupting.fired.load(Ordering::SeqCst) {
            result.unwrap();
            assert_backup_exact(&shared(&image), expected, label);
            break;
        }
        match result {
            Err(PersistError::BackupDamaged(damage)) => {
                assert_eq!(damage.side, DamageSide::Backup, "{label} write {at}");
                assert_same_files(&image, &before, &format!("{label} write {at}"));
                let text =
                    graphersal::error::format_diagnostic(&PersistError::BackupDamaged(damage));
                assert!(!text.contains("store repair"), "{label}: {text}");
                refused += 1;
            }
            // A damaged write of a file that is replaced or removed later (the INTENT, an
            // intermediate GRAPH): the backup is intact.
            Ok(_) => assert_backup_exact(&shared(&image), expected, &format!("{label} write {at}")),
            Err(other) => panic!("{label} write {at}: {other}"),
        }
    }
    assert!(refused > 3, "{label}: {refused}");
}

/// Matrix R3: a copy that does not read back intact (a bit flipped in each write in turn) is
/// refused as the backup's damage: a full backup leaves nothing, an increment is rolled back,
/// a backup from memory leaves nothing.
#[test]
fn a_damaged_copy_is_refused_and_removed() {
    let source = MemDir::new();
    let store = populated(&source, options());
    let expected = oracle(&shared(&source));
    corrupt_every_write(
        &MemDir::new(),
        &exact(&expected),
        &|t| store.backup(t),
        "full",
    );
    let base = MemDir::new();
    let older = MemDir::new();
    store.backup(&older).unwrap();
    store.backup(&base).unwrap();
    let mut rng = Rng(9);
    history(store.graph(), &mut rng, 10, 70);
    store.checkpoint(None).unwrap();
    history(store.graph(), &mut rng, 6, 80);
    let expected = oracle(&shared(&source));
    corrupt_every_write(&base, &exact(&expected), &|t| store.backup(t), "increment");
    // A ZIP archive that does not read back intact.
    #[cfg(feature = "persist-zip")]
    for at in [100usize, 300, 2_000, 5_000] {
        let mut archive = FlippingCursor {
            inner: io::Cursor::new(Vec::new()),
            at,
            written: 0,
        };
        let damage = damaged(store.backup_zip(&mut archive), &format!("zip byte {at}"));
        assert_eq!(damage.side, DamageSide::Backup);
    }
}

/// A ZIP target whose byte `at` reads back flipped (a disk returning other bytes).
#[cfg(feature = "persist-zip")]
struct FlippingCursor {
    inner: io::Cursor<Vec<u8>>,
    at: usize,
    written: usize,
}

#[cfg(feature = "persist-zip")]
impl io::Write for FlippingCursor {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut bytes = buf.to_vec();
        if (self.written..self.written + bytes.len()).contains(&self.at) {
            bytes[self.at - self.written] ^= 0x01;
        }
        self.written += bytes.len();
        self.inner.write(&bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(feature = "persist-zip")]
impl io::Read for FlippingCursor {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }
}

#[cfg(feature = "persist-zip")]
impl io::Seek for FlippingCursor {
    fn seek(&mut self, pos: io::SeekFrom) -> io::Result<u64> {
        self.inner.seek(pos)
    }
}

/// Matrix R4: a full backup and a backup from memory whose target fails after every write step
/// (a crash: everything later fails too, the clean-up included): the leftover is either no
/// backup (no GRAPH: refused as an interrupted full backup) or a complete one equal to the
/// store; with a single failing step (a full disk for a moment) nothing is left.
#[test]
fn a_full_backup_interrupted_at_every_step() {
    let source = MemDir::new();
    let store = populated(&source, options());
    let expected = oracle(&shared(&source));
    // Damage found first, so the backup from memory is allowed.
    let mut bytes = source.file("GRAPH").unwrap();
    let real = bytes.clone();
    bytes[20] ^= 1;
    source.put_file("GRAPH", bytes.clone());
    source.put_file("GRAPH.copy", bytes);
    let _ = store.verify();
    source.put_file("GRAPH", real.clone());
    source.put_file("GRAPH.copy", real);
    assert!(!store.damage_found().unwrap().is_empty());
    for from_memory in [false, true] {
        for crash in [true, false] {
            let mut steps = 0;
            for at in 0.. {
                let image = MemDir::new();
                let faults = Faults::new(crash, io::ErrorKind::Other);
                let target: Arc<dyn StoreDir> = Arc::new(Hooked {
                    inner: image.clone(),
                    hook: faults.clone(),
                });
                faults.arm(at);
                let result = if from_memory {
                    store.backup_from_memory(&target, BackupMode::Auto)
                } else {
                    store.backup(&target)
                };
                let label = format!("memory {from_memory} crash {crash} at {at}");
                if !faults.fired() {
                    result.unwrap();
                    break;
                }
                assert!(result.is_err(), "{label}");
                steps += 1;
                if !crash {
                    assert!(
                        image.file_names().is_empty(),
                        "{label}: {:?}",
                        image.file_names()
                    );
                } else if image.file("GRAPH").is_some() && image.file("GRAPH.copy").is_some() {
                    assert_backup(&shared(&image), &expected, &label);
                } else {
                    let again = store.backup(&image);
                    assert!(
                        matches!(again, Err(PersistError::BackupRefused { .. })) || again.is_ok(),
                        "{label}: {again:?}"
                    );
                }
            }
            assert!(steps > 5, "{steps}");
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Backups from memory
// ---------------------------------------------------------------------------------------------

/// A store on a hooked directory: `faults` armed at 0 with `crash` fails (and records) every
/// write to the store from then on.
fn hooked_store(policy: DamagePolicy) -> (MemDir, Arc<Faults>, Store) {
    let inner = MemDir::new();
    let faults = Faults::new(true, io::ErrorKind::PermissionDenied);
    let dir: Arc<dyn StoreDir> = Arc::new(Hooked {
        inner: inner.clone(),
        hook: faults.clone(),
    });
    let store = Store::create(&dir, options().with_damage_policy(policy)).unwrap();
    fake_data(store.graph(), 3, 10);
    store.checkpoint(None).unwrap();
    let mut rng = Rng(21);
    history(store.graph(), &mut rng, 15, 0);
    (inner, faults, store)
}

/// Flips a byte in the middle of the newest WAL segment's last record (behind the store's
/// back, as a failing disk would).
fn damage_wal(inner: &MemDir) -> String {
    let (last, len) = wal_files(inner).pop().unwrap();
    let mut bytes = inner.file(&last).unwrap();
    bytes[len - 5] ^= 0x40;
    inner.put_file(&last, bytes);
    last
}

/// Matrix S6 + rainy: a backup finds the store damaged (and freezes it); a backup from memory
/// then writes the intact graph into a new directory and as an increment of an older backup,
/// WITHOUT a single write to the store's directory; the result restores to the live graph. A
/// normal increment of that backup afterwards needs a full backup and says so.
#[test]
fn a_backup_from_memory_saves_a_damaged_store_and_never_writes_to_it() {
    let (inner, faults, store) = hooked_store(DamagePolicy::Maintenance);
    let older = MemDir::new();
    store.backup(&older).unwrap();
    commit(&store, "after-older").unwrap();
    // Refused while no damage is known.
    assert!(matches!(
        store.backup_from_memory(MemDir::new(), BackupMode::Auto),
        Err(PersistError::BackupRefused { .. })
    ));
    let damaged_file = damage_wal(&inner);
    let damage = damaged(store.backup(MemDir::new()), "backup of a damaged store");
    assert_eq!(damage.side, DamageSide::Store);
    assert!(damage.from_memory);
    assert!(
        damage.problems.iter().any(|p| p.path == damaged_file),
        "{:?}",
        damage.problems
    );
    let text = graphersal::error::format_diagnostic(&PersistError::BackupDamaged(damage));
    assert!(
        text.contains("store repair") && text.contains("from memory"),
        "{text}"
    );
    assert!(store.is_frozen_by_damage());
    let live = exact(&store.graph().read());

    // From here on, every write to the store's directory fails and is recorded.
    faults.arm(0);
    let full = MemDir::new();
    let report = store.backup_from_memory(&full, BackupMode::Auto).unwrap();
    assert!(report.from_memory && !report.incremental);
    assert_eq!(exact(&loaded(&shared(&full))), live);
    assert!(Store::verify_dir(&full).unwrap().is_ok());
    let report = store.backup_from_memory(&older, BackupMode::Auto).unwrap();
    assert!(report.from_memory && report.incremental);
    assert_eq!(exact(&loaded(&shared(&older))), live);
    assert!(Store::verify_dir(&older).unwrap().is_ok());
    // Once more: nothing new.
    let again = store.backup_from_memory(&older, BackupMode::Auto).unwrap();
    assert_eq!(again.bytes, 0);
    assert!(
        faults.ops.lock().unwrap().is_empty(),
        "writes to the damaged store: {:?}",
        faults.ops.lock().unwrap()
    );
    faults.disarm();

    // Restored, the backup is the live store with the same graph id.
    Store::restore_backup(&full).unwrap();
    let restored = Store::open(&full, options()).unwrap();
    assert_eq!(exact(&restored.graph().read()), live);
    restored.close().unwrap();
    // A normal increment of the backup from memory needs a full backup.
    drop(store);
    let (_, _, other) = hooked_store(DamagePolicy::Maintenance);
    assert!(other.backup(&older).is_err());
}

/// Matrix R7: refusals of a backup from memory: a store opened in maintenance mode, a backup
/// opened read-only, a target that is not empty and no backup, a backup of another graph,
/// `Full` into an existing backup.
#[test]
fn a_backup_from_memory_is_refused_where_it_does_not_apply() {
    let (inner, _faults, store) = hooked_store(DamagePolicy::Continue);
    damage_wal(&inner);
    assert!(store.verify().is_ok());
    assert_eq!(store.damage_found().unwrap().len(), 1);
    // Not empty, no backup.
    let stray = MemDir::new();
    stray.put_file("x", vec![1]);
    assert!(matches!(
        store.backup_from_memory(&stray, BackupMode::Auto),
        Err(PersistError::BackupRefused { .. })
    ));
    // Full into an existing backup.
    let first = MemDir::new();
    store.backup_from_memory(&first, BackupMode::Auto).unwrap();
    assert!(matches!(
        store.backup_from_memory(&first, BackupMode::Full),
        Err(PersistError::BackupRefused { .. })
    ));
    // A backup of another graph.
    let other = MemDir::new();
    let other_store = Store::create(&other, options()).unwrap();
    let foreign = MemDir::new();
    other_store.backup(&foreign).unwrap();
    assert!(matches!(
        store.backup_from_memory(&foreign, BackupMode::Auto),
        Err(PersistError::BackupRefused { .. })
    ));
    // A backup opened read-only.
    let opened = Store::open_backup(&first, options()).unwrap();
    assert!(matches!(
        opened.backup_from_memory(MemDir::new(), BackupMode::Auto),
        Err(PersistError::IsBackup { .. })
    ));
    // A store opened in maintenance mode (damage found at open).
    drop(store);
    let reopened = Store::open(&inner, options()).unwrap();
    assert!(reopened.maintenance().is_some());
    assert!(matches!(
        reopened.backup_from_memory(MemDir::new(), BackupMode::Auto),
        Err(PersistError::BackupRefused { .. })
    ));
}

// ---------------------------------------------------------------------------------------------
// The damage policy
// ---------------------------------------------------------------------------------------------

/// Matrix R5: with the default policy `maintenance`, damage found by a backup, a checkpoint, a
/// verification and the automatic checkpoint freezes the open store: commits, marks (also
/// through the graph), checkpoints, prune and rollback are refused with the reason; reads go
/// on; the close writes nothing.
#[test]
fn damage_found_while_open_freezes_a_maintenance_store() {
    for trigger in ["backup", "checkpoint", "verify"] {
        let (inner, faults, store) = hooked_store(DamagePolicy::Maintenance);
        assert_eq!(store.damage_policy().unwrap(), DamagePolicy::Maintenance);
        commit(&store, "before").unwrap();
        damage_wal(&inner);
        match trigger {
            "backup" => assert!(store.backup(MemDir::new()).is_err()),
            "checkpoint" => assert!(store.checkpoint(None).is_err()),
            _ => assert!(!store.verify().unwrap().is_ok()),
        }
        let found = store.damage_found().unwrap();
        assert_eq!(found.len(), 1, "{trigger}");
        assert_eq!(found[0].found_by, trigger);
        assert!(store.is_frozen_by_damage());
        let refused = |err: &str| err.contains("read-only") && err.contains(trigger);
        let err = commit(&store, "after").unwrap_err().to_string();
        assert!(refused(&err), "{trigger}: {err}");
        // Its own help (back up from memory, repair), not the generic commit-hook help.
        assert!(
            err.contains("Help: The commit hook 'damage' refused"),
            "{err}"
        );
        assert!(err.contains("backup from memory"), "{err}");
        assert!(!err.contains("write-ahead"), "{err}");
        assert!(refused(&store.mark("m").unwrap_err().to_string()));
        assert!(store.graph().write().mark("m2").is_err());
        assert!(refused(&store.checkpoint(None).unwrap_err().to_string()));
        assert!(refused(&store.prune(u64::MAX).unwrap_err().to_string()));
        assert!(matches!(
            store.rollback_to(RecoveryTarget::CommitSeq(1), RollbackMode::Move),
            Err(PersistError::DamageFound { .. })
        ));
        assert!(store.run_due_checkpoint().unwrap().is_none());
        // Reads go on; the info says why.
        assert!(store.graph().read().vertex_count() > 0);
        let info = store.info().unwrap();
        assert!(info.frozen_by_damage && info.damage_found.len() == 1);
        // Nothing written on close (the WAL is synced: what it holds was written before).
        faults.arm(u64::MAX);
        store.close().unwrap();
        let ops = faults.ops.lock().unwrap().clone();
        assert!(
            ops.iter()
                .all(|op| *op == crate::merge_checkpoint_tests::Op::Sync),
            "{trigger}: {ops:?}"
        );
    }
}

/// Matrix R5 (automatic checkpoint): the background checkpoint that merges a damaged WAL
/// freezes the store; commits are refused from then on.
#[test]
fn the_automatic_checkpoint_finding_damage_freezes_the_store() {
    let dir = MemDir::new();
    let store = Store::create(&dir, options().with_checkpoint_after_bytes(1_500)).unwrap();
    fake_data(store.graph(), 5, 4);
    damage_wal(&dir);
    let mut rng = Rng(1);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut round = 0;
    while !store.is_frozen_by_damage() && std::time::Instant::now() < deadline {
        history(store.graph(), &mut rng, 1, round);
        round += 1;
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert!(store.is_frozen_by_damage(), "after {round} rounds");
    assert_eq!(store.damage_found().unwrap()[0].found_by, "checkpoint");
    assert!(store.last_auto_checkpoint_error().unwrap().is_some());
    let err = commit(&store, "late").unwrap_err();
    assert!(err.contains("checkpoint found damage"), "{err}");
}

/// Matrix R5 (continue): a store created with `continue` keeps committing after damage was
/// found; the damage stays reported.
#[test]
fn a_continue_store_keeps_committing_and_reports_the_damage() {
    let (inner, _faults, store) = hooked_store(DamagePolicy::Continue);
    damage_wal(&inner);
    let damage = damaged(store.backup(MemDir::new()), "backup");
    assert!(damage.from_memory);
    assert!(!store.is_frozen_by_damage());
    commit(&store, "still").unwrap();
    store.mark("still-marked").unwrap();
    assert!(!store.verify().unwrap().is_ok());
    let info = store.info().unwrap();
    assert_eq!(info.damage_policy, DamagePolicy::Continue);
    assert_eq!(info.damage_found.len(), 2);
    assert!(!info.frozen_by_damage);
    // A backup from memory works here too.
    let saved = MemDir::new();
    store.backup_from_memory(&saved, BackupMode::Auto).unwrap();
    assert_eq!(
        exact(&loaded(&shared(&saved))),
        exact(&store.graph().read())
    );
}

/// Matrix R6: damage found when the store is OPENED always means maintenance mode, whatever the
/// policy.
#[test]
fn damage_at_open_is_maintenance_mode_whatever_the_policy() {
    for policy in [DamagePolicy::Maintenance, DamagePolicy::Continue] {
        let dir = MemDir::new();
        let store = Store::create(&dir, options().with_damage_policy(policy)).unwrap();
        fake_data(store.graph(), 1, 5);
        store.close().unwrap();
        damage_wal(&dir);
        let reopened = Store::open(&dir, options()).unwrap();
        assert!(reopened.maintenance().is_some(), "{policy}");
        assert!(commit(&reopened, "x").is_err());
    }
}

/// Matrix S7: the policy is fixed at creation (GRAPH flag bit 5), and EVERY path that writes a
/// store or a copy of it keeps it: open with another policy, commits, checkpoints (live and
/// closed), marks, prune, rollback and attic restore, attic fork, fork, repair, convert (into
/// a single file and back), compact, backups (full, increment, ZIP, from memory), restore
/// (in place and from a ZIP), close and reopen. There is no API to change it.
#[test]
fn the_damage_policy_is_fixed_at_creation_and_carried_everywhere() {
    for policy in [DamagePolicy::Continue, DamagePolicy::Maintenance] {
        let other = match policy {
            DamagePolicy::Continue => DamagePolicy::Maintenance,
            _ => DamagePolicy::Continue,
        };
        let bit = u32::from(policy == DamagePolicy::Continue) << 5;
        let check = |dir: &MemDir, label: &str| {
            assert_eq!(
                graph_flags(dir) & (1 << 5),
                bit,
                "{policy} {label}: GRAPH bit 5"
            );
            assert_eq!(
                Store::inspect(dir).unwrap().damage_policy,
                policy,
                "{policy} {label}"
            );
        };
        let dir = MemDir::new();
        let store = Store::create(&dir, options().with_damage_policy(policy)).unwrap();
        check(&dir, "create");
        fake_data(store.graph(), 2, 8);
        store.checkpoint(None).unwrap();
        store.mark("m").unwrap();
        commit(&store, "a").unwrap();
        commit(&store, "b").unwrap();
        let at = store.graph().read().last_commit_seq() - 1;
        let rolled = store
            .rollback_to(RecoveryTarget::CommitSeq(at), RollbackMode::Move)
            .unwrap();
        check(&dir, "rollback");
        let attic_fork = MemDir::new();
        store
            .attic_fork(rolled.attic.as_deref().unwrap(), &attic_fork)
            .unwrap();
        check(&attic_fork, "attic fork");
        store
            .attic_restore(rolled.attic.as_deref().unwrap())
            .unwrap();
        check(&dir, "attic restore");
        store.prune(u64::MAX).unwrap();
        store.compact().unwrap();
        check(&dir, "prune + compact");
        let fork = MemDir::new();
        store.fork(RecoveryTarget::Latest, &fork).unwrap();
        check(&fork, "fork");
        let repaired = MemDir::new();
        store.repair_to(&repaired).unwrap();
        check(&repaired, "repair");
        let backup = MemDir::new();
        store.backup(&backup).unwrap();
        check(&backup, "backup");
        commit(&store, "c").unwrap();
        store.backup(&backup).unwrap();
        check(&backup, "increment");
        #[cfg(feature = "persist-zip")]
        {
            let mut zip = io::Cursor::new(Vec::new());
            store.backup_zip(&mut zip).unwrap();
            let unzipped = MemDir::new();
            Store::restore_zip(zip.get_ref().as_slice(), &unzipped).unwrap();
            check(&unzipped, "zip restore");
        }
        Store::restore_backup(&backup).unwrap();
        check(&backup, "restore");
        store.close().unwrap();
        check(&dir, "close");
        // An open with the other policy changes nothing.
        let reopened = Store::open(&dir, options().with_damage_policy(other)).unwrap();
        assert_eq!(reopened.damage_policy().unwrap(), policy);
        commit(&reopened, "d").unwrap();
        reopened.checkpoint(None).unwrap();
        reopened.close().unwrap();
        check(&dir, "reopen with the other policy");
        Store::checkpoint_dir(&dir, options().with_damage_policy(other), None).unwrap();
        check(&dir, "closed checkpoint");
        // Convert into a single file and back.
        let temp = std::env::temp_dir().join(format!(
            "bk1-policy-{}-{}.gstore",
            std::process::id(),
            policy
        ));
        let _ = std::fs::remove_file(&temp);
        Store::convert(&dir, graphersal::persist::SingleFileDir::new(&temp)).unwrap();
        let back = MemDir::new();
        Store::convert(graphersal::persist::SingleFileDir::new(&temp), &back).unwrap();
        check(&back, "convert");
        std::fs::remove_file(&temp).unwrap();
        // A backup from memory carries it.
        let (inner, _faults, store) = hooked_store(policy);
        damage_wal(&inner);
        let _ = store.verify();
        let saved = MemDir::new();
        store.backup_from_memory(&saved, BackupMode::Auto).unwrap();
        check(&saved, "backup from memory");
        Store::restore_backup(&saved).unwrap();
        check(&saved, "restored backup from memory");
    }
}

/// Matrix S9: backups while commits and automatic checkpoints run: every backup equals the
/// store's state at its own position.
#[test]
fn backups_during_commits_and_checkpoints_equal_the_store_at_their_position() {
    let dir = MemDir::new();
    let store =
        Arc::new(Store::create(&dir, options().with_checkpoint_after_bytes(3_000)).unwrap());
    fake_data(store.graph(), 4, 10);
    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
        let (store, stop) = (store.clone(), stop.clone());
        std::thread::spawn(move || {
            let mut rng = Rng(77);
            let mut round = 0;
            while !stop.load(Ordering::SeqCst) {
                history(store.graph(), &mut rng, 1, round);
                round += 1;
            }
            round
        })
    };
    let backups: Mutex<Vec<(MemDir, u64)>> = Mutex::new(Vec::new());
    let increment = MemDir::new();
    for i in 0..12 {
        let target = if i % 2 == 0 {
            increment.clone()
        } else {
            MemDir::new()
        };
        let report = store.backup(&target).unwrap();
        if i % 2 == 1 {
            backups.lock().unwrap().push((target, report.commit_seq));
        } else {
            let position = loaded(&shared(&target)).last_commit_seq();
            assert_eq!(position, report.commit_seq);
            backups
                .lock()
                .unwrap()
                .push((increment.deep_copy(), report.commit_seq));
        }
    }
    stop.store(true, Ordering::SeqCst);
    assert!(writer.join().unwrap() > 0);
    for (backup, commit) in backups.into_inner().unwrap() {
        let expected = store
            .read_only_at(RecoveryTarget::CommitSeq(commit))
            .unwrap()
            .graph;
        assert_eq!(
            exact(&loaded(&shared(&backup))),
            exact(&expected),
            "commit {commit}"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// A source that changes under the copy, foreign and damaged backups, refusals
// ---------------------------------------------------------------------------------------------

/// How a [`Decaying`] directory changes what it returns once armed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decay {
    /// One bit of every file read is flipped (a disk returning other bytes).
    Flip,
    /// Every file read ends one byte early (a file that shrank).
    Truncate,
}

/// A [`MemDir`] whose reads change once `armed` is set: the store's files change between the
/// backup's checks and its copy.
#[derive(Debug, Clone)]
struct Decaying {
    inner: MemDir,
    armed: Arc<AtomicBool>,
    decay: Decay,
}

impl Decaying {
    fn decayed(&self, bytes: Vec<u8>) -> Vec<u8> {
        if !self.armed.load(Ordering::SeqCst) || bytes.is_empty() {
            return bytes;
        }
        let mut bytes = bytes;
        match self.decay {
            Decay::Flip => {
                let at = bytes.len() / 2;
                bytes[at] ^= 0x08;
            }
            Decay::Truncate => {
                bytes.pop();
            }
        }
        bytes
    }
}

impl StoreDir for Decaying {
    fn location(&self) -> String {
        "memory:decaying".into()
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
        Ok(self.decayed(self.inner.read(name, max)?))
    }
    fn read_range(&self, name: &str, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        self.inner.read_range(name, offset, len)
    }
    fn open_read(&self, name: &str) -> io::Result<Box<dyn io::Read + Send>> {
        let mut bytes = Vec::new();
        io::Read::read_to_end(&mut self.inner.open_read(name)?, &mut bytes)?;
        Ok(Box::new(io::Cursor::new(self.decayed(bytes))))
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
}

/// Arms a flag at the first change of the target (the copy starts).
struct ArmOnWrite(Arc<AtomicBool>);

impl crate::merge_checkpoint_tests::Hook for ArmOnWrite {
    fn on(&self, _: crate::merge_checkpoint_tests::Op, _: &str) -> io::Result<()> {
        self.0.store(true, Ordering::SeqCst);
        Ok(())
    }
}

/// A ZIP target that arms a flag at its first write.
#[cfg(feature = "persist-zip")]
struct ArmingCursor {
    inner: io::Cursor<Vec<u8>>,
    armed: Arc<AtomicBool>,
}

#[cfg(feature = "persist-zip")]
impl io::Write for ArmingCursor {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.armed.store(true, Ordering::SeqCst);
        self.inner.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(feature = "persist-zip")]
impl io::Read for ArmingCursor {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }
}

#[cfg(feature = "persist-zip")]
impl io::Seek for ArmingCursor {
    fn seek(&mut self, pos: io::SeekFrom) -> io::Result<u64> {
        self.inner.seek(pos)
    }
}

/// Rainy day: the store's files change between the backup's checks and its copy (a disk that
/// returns other bytes, a file that shrinks). A flipped byte reads back wrong in the copy, and
/// since the store's files no longer pass their checks either, it is the STORE's damage; a
/// shrunk file fails the copy. Nothing of the backup is kept: full, increment and ZIP.
#[test]
fn a_store_that_changes_under_the_copy_stops_the_backup() {
    let source = MemDir::new();
    let store = populated(&source, options());
    let base = MemDir::new();
    store.backup(&base).unwrap();
    let mut rng = Rng(8);
    history(store.graph(), &mut rng, 8, 40);
    store.checkpoint(None).unwrap();
    history(store.graph(), &mut rng, 8, 50);
    store.close().unwrap();
    for decay in [Decay::Flip, Decay::Truncate] {
        for increment in [false, true] {
            let armed = Arc::new(AtomicBool::new(false));
            let decaying = Decaying {
                inner: source.clone(),
                armed: armed.clone(),
                decay,
            };
            let image = if increment {
                base.deep_copy()
            } else {
                MemDir::new()
            };
            let before = listing(&image);
            let target: Arc<dyn StoreDir> = Arc::new(Hooked {
                inner: image.clone(),
                hook: Arc::new(ArmOnWrite(armed.clone())),
            });
            let result = Store::backup_dir(Arc::new(decaying) as Arc<dyn StoreDir>, &target);
            let label = format!("{decay:?} increment {increment}");
            match (decay, result) {
                (Decay::Flip, Err(PersistError::BackupDamaged(damage))) => {
                    assert_eq!(damage.side, DamageSide::Store, "{label}");
                }
                (Decay::Truncate, Err(_)) => {}
                (_, other) => panic!("{label}: {other:?}"),
            }
            assert_same_files(&image, &before, &label);
        }
        // ZIP.
        #[cfg(feature = "persist-zip")]
        {
            let armed = Arc::new(AtomicBool::new(false));
            let decaying: Arc<dyn StoreDir> = Arc::new(Decaying {
                inner: source.clone(),
                armed: armed.clone(),
                decay,
            });
            let archive = ArmingCursor {
                inner: io::Cursor::new(Vec::new()),
                armed,
            };
            let result = Store::backup_zip_dir(&decaying, archive);
            assert!(result.is_err(), "zip {decay:?}");
            if decay == Decay::Flip {
                let damage = damaged(result, "zip");
                assert_eq!(damage.side, DamageSide::Store);
            }
        }
    }
}

/// Rainy day: both `GRAPH` copies of the store unusable stop every kind of backup with both
/// named; the newest WAL segment missing stops an increment (the WAL set of spec 9.8).
#[test]
fn unusable_graph_files_and_a_lost_wal_segment_stop_every_backup() {
    let source = MemDir::new();
    let store = populated(&source, options());
    let base = MemDir::new();
    store.backup(&base).unwrap();
    history(store.graph(), &mut Rng(4), 6, 90);
    store.close().unwrap();
    let image = source.deep_copy();
    for name in ["GRAPH", "GRAPH.copy"] {
        let mut bytes = image.file(name).unwrap();
        bytes[30] ^= 2;
        image.put_file(name, bytes);
    }
    let names = |damage: &graphersal::persist::BackupDamage| -> Vec<String> {
        damage.problems.iter().map(|p| p.path.clone()).collect()
    };
    let full = damaged(Store::backup_dir(&image, MemDir::new()), "full");
    assert_eq!(names(&full), ["GRAPH", "GRAPH.copy"]);
    let backup = base.deep_copy();
    let increment = damaged(Store::backup_dir(&image, &backup), "increment");
    assert_eq!(names(&increment), ["GRAPH", "GRAPH.copy"]);
    assert_eq!(listing(&backup), listing(&base));
    #[cfg(feature = "persist-zip")]
    {
        let mut archive = io::Cursor::new(Vec::new());
        let zip = damaged(Store::backup_zip_dir(&image, &mut archive), "zip");
        assert_eq!(names(&zip), ["GRAPH", "GRAPH.copy"]);
    }
    // The newest WAL segment lost.
    let (newest, _) = wal_files(&source).pop().unwrap();
    let image = MemDir::new();
    for (name, bytes) in listing(&source) {
        if name != newest {
            image.put_file(&name, bytes);
        }
    }
    let backup = base.deep_copy();
    let lost = damaged(Store::backup_dir(&image, &backup), "lost segment");
    assert_eq!(lost.side, DamageSide::Store);
    assert!(
        lost.problems.iter().any(|p| p.path == newest),
        "{:?}",
        lost.problems
    );
    assert_eq!(listing(&backup), listing(&base));
}

/// Rainy day: a damaged backup is not continued (its last WAL segment fails its checksum): the
/// backup's damage, the store is fine; back up into a new directory.
#[test]
fn a_damaged_backup_is_not_continued() {
    let source = MemDir::new();
    let store = populated(&source, options());
    let backup = MemDir::new();
    store.backup(&backup).unwrap();
    history(store.graph(), &mut Rng(6), 4, 70);
    let (last, len) = wal_files(&backup).pop().unwrap();
    let mut bytes = backup.file(&last).unwrap();
    bytes[len - 3] ^= 0x10;
    backup.put_file(&last, bytes);
    let before = listing(&backup);
    let damage = damaged(store.backup(&backup), "damaged backup");
    assert_eq!(damage.side, DamageSide::Backup);
    assert_eq!(damage.problems[0].path, last);
    assert_eq!(listing(&backup), before);
    assert!(
        store.damage_found().unwrap().is_empty(),
        "the store's damage policy is not involved"
    );
}

/// Rainy day: a backup of the store's history is not continued from a copy of the store that
/// committed something else after it (the same graph id, a diverged history): the backup's
/// last WAL segment is not a prefix of the copy's, whether the copy's segment is shorter,
/// as long, or was rotated since.
#[test]
fn an_increment_from_a_diverged_copy_of_the_store_is_refused() {
    // One WAL segment for everything: the backup's last segment is the copy's too.
    let wide = options().with_wal_segment_bytes(1 << 20);
    let source = MemDir::new();
    let store = populated(&source, wide.clone());
    store.close().unwrap();
    let diverged = source.deep_copy();
    let store = Store::open(&source, wide.clone()).unwrap();
    let other = Store::open(&diverged, wide).unwrap();
    // The store commits more; the backup holds it.
    commit(&store, "only-in-the-store").unwrap();
    commit(&store, "and-this").unwrap();
    let backup = MemDir::new();
    store.backup(&backup).unwrap();
    let before = listing(&backup);
    // The copy commits other things: shorter, as long (another body), much longer (rotated).
    for (label, commits) in [("shorter", 0), ("longer", 3)] {
        for i in 0..commits {
            commit(&other, &format!("{label}-{i:02}")).unwrap();
        }
        match other.backup(&backup) {
            Err(PersistError::BackupRefused { reason, .. }) => {
                assert!(reason.contains("not a prefix"), "{label}: {reason}")
            }
            result => panic!("{label}: {result:?}"),
        }
        assert_eq!(listing(&backup), before, "{label}");
    }
}

/// Rainy day: restore and prune of a store that is not a backup are refused.
#[test]
fn restore_and_prune_refuse_a_store_that_is_not_a_backup() {
    let dir = MemDir::new();
    Store::create(&dir, options()).unwrap().close().unwrap();
    for result in [
        Store::restore_backup(&dir).map(|_| ()),
        Store::prune_backup(&dir, 1).map(|_| ()),
    ] {
        let err = result.unwrap_err().to_string();
        assert!(err.contains("is not a backup"), "{err}");
    }
}

/// Rainy day for backups from memory: a copy that does not read back (a bit flipped in each
/// write in turn) is refused as the backup's damage and leaves the target as it was (full and
/// increment); a backup directory in use, a live store and an increment that follows a rollback
/// since the backup (the new lineage written first).
#[test]
fn a_backup_from_memory_refuses_a_damaged_copy_and_follows_a_rollback() {
    let (inner, _faults, store) = hooked_store(DamagePolicy::Continue);
    let older = MemDir::new();
    store.backup(&older).unwrap();
    commit(&store, "a").unwrap();
    let mark = store.graph().read().last_commit_seq();
    commit(&store, "b").unwrap();
    store
        .rollback_to(RecoveryTarget::CommitSeq(mark), RollbackMode::Move)
        .unwrap();
    commit(&store, "c").unwrap();
    damage_wal(&inner);
    let _ = store.verify();
    assert!(!store.damage_found().unwrap().is_empty());
    let expected = exact(&store.graph().read());
    let from_memory = |t: &Arc<dyn StoreDir>| store.backup_from_memory(t, BackupMode::Auto);
    corrupt_every_write(&MemDir::new(), &expected, &from_memory, "memory full");
    corrupt_every_write(
        &older,
        &expected,
        &from_memory,
        "memory increment after a rollback",
    );
    // In use: another backup holds the directory's writer lock.
    let busy = older.deep_copy();
    let _held = busy.lock(LockKind::Writer).unwrap().unwrap();
    match store.backup_from_memory(&busy, BackupMode::Auto) {
        Err(PersistError::BackupRefused { reason, .. }) => assert!(reason.contains("in use")),
        other => panic!("{other:?}"),
    }
    // A live store is never written to.
    let live = MemDir::new();
    Store::create(&live, options()).unwrap().close().unwrap();
    match store.backup_from_memory(&live, BackupMode::Auto) {
        Err(PersistError::BackupRefused { reason, .. }) => {
            assert!(reason.contains("not a backup"), "{reason}")
        }
        other => panic!("{other:?}"),
    }
}

/// Rainy day: an interrupted increment whose `INTENT` names a path outside the backup (a foreign
/// or damaged file) is refused, nothing is removed.
#[test]
fn an_intent_with_an_unsafe_path_is_refused() {
    let source = MemDir::new();
    let store = populated(&source, options());
    let backup = MemDir::new();
    store.backup(&backup).unwrap();
    backup.put_file(
        "INTENT",
        br#"{"op":"backup_increment","files":["../escape"],"started_at":1,"graph_before":"00","taken_at":"1"}"#
            .to_vec(),
    );
    let before = listing(&backup);
    let err = store.backup(&backup).unwrap_err().to_string();
    assert!(err.contains("unsafe path"), "{err}");
    assert_eq!(listing(&backup), before);
}
