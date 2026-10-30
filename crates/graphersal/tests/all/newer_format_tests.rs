//! The forward-version guard (format spec 15): a store, backup, single-file store, packed
//! snapshot or journal written by a NEWER Graphersal (a newer store format version in `GRAPH`,
//! a newer file format version in a manifest, snapshot segment or WAL segment, a newer
//! single-file container version) is refused with `PersistError::UnsupportedVersion` by every
//! operation, before anything is written or locked: never damage, never maintenance mode, never
//! a (lossy) repair.
//!
//! The newer inputs are built by patching the version word of a valid store's files and
//! recomputing the header checksum (so they are intact, not damaged): no newer Graphersal
//! exists to write real ones. "Newer" is one above this build: store format version
//! `STORE_FORMAT_VERSION + 1`, file format version `FORMAT_VERSION + 1`, container version 2.
#![cfg(all(feature = "persist", not(target_arch = "wasm32")))]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use graphersal::persist::{
    self, CompactionPolicy, PersistError, ReadOptions, RecoveryTarget, RollbackMode, Store,
    StoreOptions,
};

/// A store format version one above this build's.
const NEWER_STORE: u32 = persist::STORE_FORMAT_VERSION + 1;
/// A file format version one above this build's.
const NEWER_FILE: u32 = persist::FORMAT_VERSION + 1;
/// A single-file container version one above this build's (1).
const NEWER_CONTAINER: u32 = 2;

/// A fresh directory path under the system temp dir, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "graphersal-newer-{}-{}-{name}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&path);
        let _ = fs::remove_file(&path);
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn join(&self, part: &str) -> PathBuf {
        self.0.join(part)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
        let _ = fs::remove_file(&self.0);
    }
}

/// CRC-32C (Castagnoli), bitwise: the test computes header checksums itself.
fn crc32c(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0x82F6_3B78
            } else {
                crc >> 1
            };
        }
    }
    !crc
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

/// A closed store with two snapshots, a WAL after the newest one, a mark and an attic entry
/// (with its own WAL).
fn build_store(dir: &Path) {
    let store = Store::create(dir, StoreOptions::new()).unwrap();
    add_person(&store, "ann");
    store.mark("m1").unwrap();
    add_person(&store, "bob");
    store.checkpoint(None).unwrap();
    add_person(&store, "cid");
    store
        .rollback_to(RecoveryTarget::Mark("m1".into()), RollbackMode::Move)
        .unwrap();
    add_person(&store, "dan");
    store.checkpoint(None).unwrap();
    add_person(&store, "eve");
    store.close().unwrap();
}

/// Every file (relative path -> bytes) and directory of a tree: what "nothing was changed"
/// compares.
fn tree(root: &Path) -> BTreeMap<String, Option<Vec<u8>>> {
    fn walk(root: &Path, at: &Path, out: &mut BTreeMap<String, Option<Vec<u8>>>) {
        let Ok(entries) = fs::read_dir(at) else {
            return;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            let relative = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .to_string();
            if path.is_dir() {
                out.insert(relative, None);
                walk(root, &path, out);
            } else {
                out.insert(relative, Some(fs::read(&path).unwrap()));
            }
        }
    }
    let mut out = BTreeMap::new();
    if root.is_file() {
        out.insert(".".into(), Some(fs::read(root).unwrap()));
    } else {
        walk(root, root, &mut out);
    }
    out
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// Files below `dir` (recursively) whose name satisfies `pick`, sorted.
fn files_where(dir: &Path, pick: &dyn Fn(&Path) -> bool) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(at) = stack.pop() {
        let Ok(entries) = fs::read_dir(&at) else {
            continue;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if pick(&path) {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// Sets the store format version of both `GRAPH` copies (or only `GRAPH.copy`), checksum
/// recomputed (fixed part 256 bytes, CRC at 252).
fn bump_graph(dir: &Path, version: u32, only_copy: bool) {
    let names: &[&str] = if only_copy {
        &["GRAPH.copy"]
    } else {
        &["GRAPH", "GRAPH.copy"]
    };
    for name in names {
        let path = dir.join(name);
        let mut bytes = fs::read(&path).unwrap();
        bytes[8..12].copy_from_slice(&version.to_le_bytes());
        let crc = crc32c(&bytes[..252]);
        bytes[252..256].copy_from_slice(&crc.to_le_bytes());
        fs::write(&path, bytes).unwrap();
    }
}

/// Sets the file format version of a snapshot or WAL segment (64-byte header, CRC of bytes
/// 0..60 at 60).
fn bump_header64(path: &Path, version: u32) {
    let mut bytes = fs::read(path).unwrap();
    bytes[8..12].copy_from_slice(&version.to_le_bytes());
    let crc = crc32c(&bytes[..60]);
    bytes[60..64].copy_from_slice(&crc.to_le_bytes());
    fs::write(path, bytes).unwrap();
}

/// Sets the file format version of both fixed-part copies of a manifest (4096 bytes each,
/// CRC at 4092).
fn bump_manifest(path: &Path, version: u32) {
    let mut bytes = fs::read(path).unwrap();
    let tail = bytes.len() - 4096;
    for at in [0, tail] {
        bytes[at + 8..at + 12].copy_from_slice(&version.to_le_bytes());
        let crc = crc32c(&bytes[at..at + 4092]);
        bytes[at + 4092..at + 4096].copy_from_slice(&crc.to_le_bytes());
    }
    fs::write(path, bytes).unwrap();
}

/// The ways a store can be newer, each applied to a fresh copy of a valid store.
const VARIANTS: &[&str] = &[
    "graph",
    "graph_copy_only",
    "manifest",
    "segment",
    "wal",
    "attic_wal",
    "attic_manifest",
];

fn apply_variant(dir: &Path, variant: &str) {
    let in_attic = |p: &Path| p.components().any(|c| c.as_os_str() == "attic");
    let newest = |pick: &dyn Fn(&Path) -> bool| -> PathBuf {
        let found = files_where(dir, pick);
        found
            .into_iter()
            .last()
            .unwrap_or_else(|| panic!("no file for {variant}"))
    };
    match variant {
        "graph" => bump_graph(dir, NEWER_STORE, false),
        "graph_copy_only" => bump_graph(dir, NEWER_STORE, true),
        "manifest" => bump_manifest(
            &newest(&|p| p.ends_with("manifest") && !in_attic(p)),
            NEWER_FILE,
        ),
        "segment" => bump_header64(
            &newest(&|p| p.extension().is_some_and(|e| e == "seg") && !in_attic(p)),
            NEWER_FILE,
        ),
        "wal" => bump_header64(
            &newest(&|p| p.extension().is_some_and(|e| e == "wal") && !in_attic(p)),
            NEWER_FILE,
        ),
        "attic_wal" => bump_header64(
            &newest(&|p| p.extension().is_some_and(|e| e == "wal") && in_attic(p)),
            NEWER_FILE,
        ),
        "attic_manifest" => bump_manifest(
            &newest(&|p| p.ends_with("manifest") && in_attic(p)),
            NEWER_FILE,
        ),
        other => panic!("unknown variant {other}"),
    }
}

/// Asserts a refusal as written by a newer Graphersal (never damage, never maintenance).
#[track_caller]
fn assert_newer(what: &str, variant: &str, err: PersistError) {
    assert!(
        matches!(err, PersistError::UnsupportedVersion { .. }),
        "{what} on {variant}: expected UnsupportedVersion, got {err:?}"
    );
    let text = graphersal::error::format_diagnostic(&err);
    assert!(text.contains("newer Graphersal"), "{what}: {text}");
    assert!(
        !text.contains("repair <"),
        "{what}: no repair offered: {text}"
    );
}

fn err_of<T>(result: Result<T, PersistError>) -> PersistError {
    match result {
        Ok(_) => panic!("expected a refusal"),
        Err(err) => err,
    }
}

#[test]
fn every_store_operation_refuses_a_newer_store_and_changes_nothing() {
    let base = TempDir::new("base");
    build_store(base.path());
    assert!(!Store::attic_list_dir(base.path()).unwrap().is_empty());
    let attic_id = Store::attic_list_dir(base.path()).unwrap()[0].id.clone();
    for variant in VARIANTS {
        let dir = TempDir::new(variant);
        copy_tree(base.path(), dir.path());
        apply_variant(dir.path(), variant);
        // The guard runs before the writer lock: no LOCK file is (re)created.
        let _ = fs::remove_file(dir.join("LOCK"));
        let _ = fs::remove_file(dir.join("BACKUP"));
        let before = tree(dir.path());
        let new_dir = TempDir::new("new");
        let d = dir.path();
        let checks: Vec<(&str, PersistError)> = vec![
            ("open", err_of(Store::open(d, StoreOptions::new()))),
            (
                "open_read_only",
                err_of(Store::open_read_only(d, RecoveryTarget::Latest)),
            ),
            (
                "open_backup",
                err_of(Store::open_backup(d, StoreOptions::new())),
            ),
            ("inspect", err_of(Store::inspect(d))),
            ("verify_dir", err_of(Store::verify_dir(d))),
            ("damage_report_dir", err_of(Store::damage_report_dir(d))),
            ("repair_dir", err_of(Store::repair_dir(d, new_dir.path()))),
            (
                "fork_dir",
                err_of(Store::fork_dir(d, RecoveryTarget::Latest, new_dir.path())),
            ),
            (
                "checkpoint_dir",
                err_of(Store::checkpoint_dir(d, StoreOptions::new(), None)),
            ),
            (
                "compaction_advice_dir",
                err_of(Store::compaction_advice_dir(d, CompactionPolicy::new())),
            ),
            (
                "prune_estimate_dir",
                err_of(Store::prune_estimate_dir(d, 1)),
            ),
            ("snapshots_dir", err_of(Store::snapshots_dir(d))),
            ("marks_dir", err_of(Store::marks_dir(d))),
            (
                "export_packed_dir",
                err_of(Store::export_packed_dir(d, None, Vec::new())),
            ),
            ("attic_list_dir", err_of(Store::attic_list_dir(d))),
            (
                "attic_changes_dir",
                err_of(Store::attic_changes_dir(d, &attic_id)),
            ),
            (
                "attic_fork_dir",
                err_of(Store::attic_fork_dir(d, &attic_id, new_dir.path())),
            ),
            ("backup_dir", err_of(Store::backup_dir(d, new_dir.path()))),
            #[cfg(feature = "persist-zip")]
            (
                "backup_zip_dir",
                err_of(Store::backup_zip_dir(d, std::io::Cursor::new(Vec::new()))),
            ),
            ("convert", err_of(Store::convert(d, new_dir.path()))),
            ("restore_backup", err_of(Store::restore_backup(d))),
            ("prune_backup", err_of(Store::prune_backup(d, 1))),
            ("backup_marker_dir", err_of(Store::backup_marker_dir(d))),
            ("upgrade", err_of(Store::upgrade(d, new_dir.path()))),
            ("upgrade_in_place", err_of(Store::upgrade_in_place(d))),
        ];
        for (what, err) in checks {
            assert_newer(what, variant, err);
        }
        assert!(
            !new_dir.path().exists(),
            "{variant}: nothing created at the target"
        );
        assert_eq!(
            tree(d),
            before,
            "{variant}: the store's files are unchanged"
        );
        assert!(!dir.join("LOCK").exists(), "{variant}: no LOCK file left");
    }
}

#[test]
fn a_newer_backup_is_refused_as_a_backup_and_as_an_increment_target() {
    let store_dir = TempDir::new("source");
    build_store(store_dir.path());
    let backup = TempDir::new("backup");
    Store::backup_dir(store_dir.path(), backup.path()).unwrap();
    for variant in ["graph", "manifest", "segment", "wal"] {
        let dir = TempDir::new(variant);
        copy_tree(backup.path(), dir.path());
        apply_variant(dir.path(), variant);
        let _ = fs::remove_file(dir.join("LOCK"));
        let before = tree(dir.path());
        let source_before = tree(store_dir.path());
        let d = dir.path();
        for (what, err) in [
            (
                "open_backup",
                err_of(Store::open_backup(d, StoreOptions::new())),
            ),
            ("restore_backup", err_of(Store::restore_backup(d))),
            ("prune_backup", err_of(Store::prune_backup(d, 1))),
            ("backup_marker_dir", err_of(Store::backup_marker_dir(d))),
            ("verify_dir", err_of(Store::verify_dir(d))),
            // An increment into a backup a newer Graphersal wrote (or upgraded).
            (
                "backup increment",
                err_of(Store::backup_dir(store_dir.path(), d)),
            ),
        ] {
            assert_newer(what, variant, err);
        }
        assert_eq!(tree(d), before, "{variant}: the backup is unchanged");
        assert_eq!(tree(store_dir.path()), source_before);
        assert!(!dir.join("LOCK").exists(), "{variant}: no LOCK file left");
    }
    // The open store's own backup into a newer backup: refused too.
    let dir = TempDir::new("open-target");
    copy_tree(backup.path(), dir.path());
    bump_graph(dir.path(), NEWER_STORE, false);
    let store = Store::open(store_dir.path(), StoreOptions::new()).unwrap();
    assert_newer("Store::backup", "graph", err_of(store.backup(dir.path())));
    store.close().unwrap();
}

#[test]
fn a_newer_file_in_a_current_store_is_not_damage_and_never_repaired() {
    // GRAPH is current, one snapshot segment is of a newer file format version: before the
    // guard this opened in maintenance mode and offered a repair that lost data.
    let dir = TempDir::new("mixed");
    build_store(dir.path());
    apply_variant(dir.path(), "segment");
    let err = err_of(Store::open(dir.path(), StoreOptions::new()));
    assert_newer("open", "segment", err);
    let err = err_of(Store::damage_report_dir(dir.path()));
    assert_newer("damage_report_dir", "segment", err);
    let text = err_of(Store::verify_dir(dir.path())).to_string();
    assert!(text.contains("file format version"), "{text}");
    assert!(text.contains("written by a newer Graphersal"), "{text}");
    assert!(!text.contains("Corrupt"), "{text}");
}

#[test]
fn the_current_versions_still_open_and_verify() {
    // The guard reads only headers: a valid store of this version is untouched by it.
    let dir = TempDir::new("current");
    build_store(dir.path());
    assert!(Store::verify_dir(dir.path()).unwrap().is_ok());
    let info = Store::inspect(dir.path()).unwrap();
    assert_eq!(info.format_version, persist::STORE_FORMAT_VERSION);
    let store = Store::open(dir.path(), StoreOptions::new()).unwrap();
    assert!(store.maintenance().is_none());
    store.close().unwrap();
}

#[test]
fn a_newer_single_file_container_is_refused() {
    let dir = TempDir::new("container");
    fs::create_dir_all(dir.path()).unwrap();
    let file = dir.join("graph.gstore");
    let store = Store::create_file(&file, StoreOptions::new()).unwrap();
    add_person(&store, "ann");
    store.close().unwrap();
    // The preamble: magic (8 bytes), container version u32 at 8 (no checksum).
    let mut bytes = fs::read(&file).unwrap();
    bytes[8..12].copy_from_slice(&NEWER_CONTAINER.to_le_bytes());
    fs::write(&file, &bytes).unwrap();
    let before = tree(&file);
    for (what, err) in [
        (
            "open_file",
            err_of(Store::open_file(&file, StoreOptions::new())),
        ),
        (
            "inspect",
            err_of(Store::inspect(persist::store_dir_at(&file))),
        ),
        (
            "verify_dir",
            err_of(Store::verify_dir(persist::store_dir_at(&file))),
        ),
        (
            "damage_report_dir",
            err_of(Store::damage_report_dir(persist::store_dir_at(&file))),
        ),
        (
            "repair_dir",
            err_of(Store::repair_dir(
                persist::store_dir_at(&file),
                dir.join("repaired"),
            )),
        ),
    ] {
        assert_newer(what, "container", err);
    }
    assert_eq!(tree(&file), before);
    assert!(!dir.join("repaired").exists());
}

#[test]
fn a_newer_packed_snapshot_and_journal_are_refused() {
    let dir = TempDir::new("packed");
    build_store(dir.path());
    let mut packed = Vec::new();
    Store::export_packed_dir(dir.path(), None, &mut packed).unwrap();
    // The packed header: magic, file format version u32 at 8.
    packed[8..12].copy_from_slice(&NEWER_FILE.to_le_bytes());
    let err = err_of(persist::read_snapshot(&packed[..], &ReadOptions::default()));
    assert_newer("read_snapshot", "packed", err);
    let target = TempDir::new("from-packed");
    let err = err_of(Store::create_from_packed(
        target.path(),
        &packed[..],
        StoreOptions::new(),
    ));
    assert_newer("create_from_packed", "packed", err);
    // A WAL segment of a newer file format version, read as a journal stream.
    let wal = files_where(dir.path(), &|p| {
        p.extension().is_some_and(|e| e == "wal")
            && !p.components().any(|c| c.as_os_str() == "attic")
    })
    .pop()
    .unwrap();
    bump_header64(&wal, NEWER_FILE);
    let err = err_of(persist::read_journal(fs::File::open(&wal).unwrap()));
    assert_newer("read_journal", "wal", err);
}

/// An OLDER store (store format version 2, which this build upgrades) that holds a file of a
/// NEWER file format version: the converter never works on it (no lock, no write), in place or
/// into a copy, and the repair and damage report refuse it too.
#[test]
fn the_upgrade_refuses_an_older_store_with_a_newer_file() {
    for variant in ["segment", "wal", "manifest", "attic_wal"] {
        let dir = TempDir::new(variant);
        build_store(dir.path());
        bump_graph(dir.path(), 2, false);
        apply_variant(dir.path(), variant);
        let _ = fs::remove_file(dir.join("LOCK"));
        let before = tree(dir.path());
        let new_dir = TempDir::new("upgraded");
        let d = dir.path();
        for (what, err) in [
            ("upgrade", err_of(Store::upgrade(d, new_dir.path()))),
            ("upgrade_in_place", err_of(Store::upgrade_in_place(d))),
            ("repair_dir", err_of(Store::repair_dir(d, new_dir.path()))),
            ("damage_report_dir", err_of(Store::damage_report_dir(d))),
        ] {
            assert_newer(what, variant, err);
        }
        assert_eq!(tree(d), before, "{variant}: unchanged");
        assert!(!dir.join("LOCK").exists(), "{variant}: no LOCK file");
        assert!(!new_dir.path().exists(), "{variant}: no copy");
    }
}

/// Commits in parts (record type 4, format spec 9.3.1) belong to file format 4: a reader of an
/// older file format version meets them only in a WAL segment whose header says a newer
/// version, and refuses the whole store as newer before reading a record, never as damage.
/// Simulated as every test here: a segment holding commits in parts, its version word one
/// above this build's.
#[test]
fn a_wal_with_commits_in_parts_of_a_newer_version_is_newer_not_damage() {
    let dir = TempDir::new("parts");
    let options = StoreOptions::new().with_commit_part_bytes(32);
    let store = Store::create(dir.path(), options.clone()).unwrap();
    add_person(&store, "a person with a name long enough for several parts");
    add_person(&store, "another person with a name long enough for parts");
    store.close().unwrap();
    let wal = files_where(dir.path(), &|p| p.extension().is_some_and(|e| e == "wal"))
        .pop()
        .unwrap();
    // The segment holds commits in parts (the record type at frame byte 16).
    let bytes = fs::read(&wal).unwrap();
    assert_eq!(bytes[64 + 16], 4);
    bump_header64(&wal, NEWER_FILE);
    let before = fs::read(&wal).unwrap();
    assert_newer("open", "parts", err_of(Store::open(dir.path(), options)));
    assert_newer(
        "open_read_only",
        "parts",
        err_of(Store::open_read_only(dir.path(), RecoveryTarget::Latest)),
    );
    assert_newer("verify_dir", "parts", err_of(Store::verify_dir(dir.path())));
    assert_newer(
        "damage_report_dir",
        "parts",
        err_of(Store::damage_report_dir(dir.path())),
    );
    let err = err_of(persist::read_journal(fs::File::open(&wal).unwrap()));
    assert_newer("read_journal", "parts", err);
    assert_eq!(fs::read(&wal).unwrap(), before);
}
