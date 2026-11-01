//! The store converter (format spec 15.1): stores, backups, ZIP backups and single-file stores
//! written by the RELEASED Graphersal 0.1.0 (the golden fixtures in `tests/data/v0.1.0/`, see
//! the README there) are upgraded to the current format, in a copy and in place, with
//! everything carried (graph, schema, catalog, history, marks, lineage, attic, creation
//! parameters); the upgraded stores take new commits (a new WAL segment of the current file
//! format), verify clean and reopen; every refusal; a crash after every write of both modes.
#![cfg(all(feature = "persist", not(target_arch = "wasm32")))]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

use graphersal::persist::{
    DamagePolicy, LockKind, MemDir, PersistError, RecoveryTarget, RollbackMode, SingleFileDir,
    Store, StoreDir, StoreFailure, StoreInfo, StoreOptions, UpgradeReport,
};
use graphersal::prelude::*;
use graphersal::temporal::Date;

use crate::backup_tests::Crashing;
use crate::persist_tests::{dump, names};

/// The fixtures (never written: every test copies them first).
fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/v0.1.0")
}

/// A fresh directory under the system temp dir, removed on drop.
struct Temp(PathBuf);

impl Temp {
    fn new(name: &str) -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "graphersal-upgrade-{}-{}-{name}",
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

/// Every file below `root`: `(relative name, bytes)`, sorted.
fn files_of(root: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(root: &Path, at: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        for entry in fs::read_dir(at).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                let name = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push((name, fs::read(&path).unwrap()));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

/// Every file of a store directory (any backend): `(name, bytes)`, sorted; the lock files and
/// temporary files left out.
fn store_files(dir: &Arc<dyn StoreDir>) -> Vec<(String, Vec<u8>)> {
    fn walk(dir: &Arc<dyn StoreDir>, at: &str, out: &mut Vec<(String, Vec<u8>)>) {
        for entry in dir.list(at).unwrap() {
            let name = if at.is_empty() {
                entry.name.clone()
            } else {
                format!("{at}/{}", entry.name)
            };
            match entry.kind {
                graphersal::persist::EntryKind::Dir => walk(dir, &name, out),
                _ => {
                    if !matches!(name.as_str(), "LOCK" | "BACKUP") && !name.ends_with(".tmp") {
                        out.push((name.clone(), dir.read(&name, 1 << 30).unwrap()));
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, "", &mut out);
    out.sort();
    out
}

/// The fixture `name` (a directory) copied into `temp`.
fn fixture_copy(temp: &Temp, name: &str, as_name: &str) -> PathBuf {
    let to = temp.join(as_name);
    for (relative, bytes) in files_of(&fixtures().join(name)) {
        let path = to.join(&relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    to
}

/// The fixture `name` (a directory) in memory.
fn fixture_mem(name: &str) -> MemDir {
    let dir = MemDir::named(name);
    for (relative, bytes) in files_of(&fixtures().join(name)) {
        if let Some((parent, _)) = relative.rsplit_once('/') {
            dir.create_dir_all(parent).unwrap();
        }
        dir.write_atomic(&relative, &bytes).unwrap();
    }
    dir
}

fn shared(dir: &MemDir) -> Arc<dyn StoreDir> {
    Arc::new(dir.clone())
}

/// The identity and creation parameters in a `GRAPH` file's bytes (format spec 5.1).
#[derive(Debug, PartialEq, Eq)]
struct Identity {
    graph_id: u128,
    created_at: i64,
    chunk_bytes: u32,
    store_id: u128,
    continue_on_damage: bool,
    backup_commit: Option<u64>,
    lineage: Vec<(u128, u64, i64)>,
}

fn identity(graph: &[u8]) -> Identity {
    let word = |at: usize| u32::from_le_bytes(graph[at..at + 4].try_into().unwrap());
    let long = |at: usize| u64::from_le_bytes(graph[at..at + 8].try_into().unwrap());
    let id = |at: usize| u128::from_be_bytes(graph[at..at + 16].try_into().unwrap());
    let flags = word(48);
    let count = word(40) as usize;
    Identity {
        graph_id: id(16),
        created_at: long(32) as i64,
        chunk_bytes: word(44),
        store_id: id(100),
        continue_on_damage: flags & (1 << 5) != 0,
        backup_commit: (flags & (1 << 2) != 0).then(|| long(68)),
        lineage: (0..count)
            .map(|i| {
                let at = 256 + 32 * i;
                (id(at), long(at + 16), long(at + 24) as i64)
            })
            .collect(),
    }
}

fn version_of(graph: &[u8]) -> u32 {
    u32::from_le_bytes(graph[8..12].try_into().unwrap())
}

/// What a store holds, as this build reads it after the upgrade.
#[derive(Debug, PartialEq)]
struct Holds {
    commit: u64,
    names: Vec<String>,
    marko_age: Option<ElementProperty>,
    data: Vec<String>,
    schema: Option<String>,
    definitions: Vec<(u8, String)>,
    marks: Vec<(String, u64)>,
    attic: Vec<(String, u64, u64)>,
    snapshots: Vec<u64>,
    graph_id: u128,
    store_id: u128,
    created_at: i64,
    chunk_bytes: u32,
    damage_policy: DamagePolicy,
    lineage: Vec<(u128, u64, i64)>,
}

fn holds(info: &StoreInfo, graph: &TraversalGraph) -> Holds {
    Holds {
        commit: graph.last_commit_seq(),
        names: names(graph),
        marko_age: age_of(graph, "marko"),
        data: dump(graph),
        schema: graph.get_schema().unwrap().map(|s| s.to_json()),
        definitions: graph
            .definitions()
            .unwrap()
            .map(|d| (d.kind().code(), d.name().to_string()))
            .collect(),
        marks: info
            .marks
            .iter()
            .map(|m| (m.name().to_string(), m.commit_seq()))
            .collect(),
        attic: info
            .attic
            .iter()
            .map(|e| (e.id.clone(), e.target, e.last_commit_seq))
            .collect(),
        snapshots: info.snapshots.iter().map(|s| s.commit_seq).collect(),
        graph_id: info.graph_id,
        store_id: info.store_id,
        created_at: info.created_at,
        chunk_bytes: info.chunk_bytes,
        damage_policy: info.damage_policy,
        lineage: info
            .lineage
            .iter()
            .map(|e| (e.graph_id, e.branched_at, e.time))
            .collect(),
    }
}

/// Opens the upgraded store and reads what it holds (closes it again).
fn open_holds(dir: impl graphersal::persist::IntoStoreDir + Clone) -> Holds {
    let store = Store::open(dir.clone(), StoreOptions::new()).unwrap();
    let held = holds(&store.info().unwrap(), &store.graph().read());
    store.close().unwrap();
    held
}

fn age_of(graph: &TraversalGraph, name: &str) -> Option<ElementProperty> {
    graph.vertices().unwrap().find_map(|v| {
        let props = graph.vertex_properties(v).unwrap()?;
        (props.get("name")? == &ElementProperty::from(name))
            .then(|| props.get("age").cloned())
            .flatten()
    })
}

/// The pinned content of the `store/` fixture (README), plus its identity from its own GRAPH.
fn assert_store_fixture(held: &Holds, graph_bytes: &[u8]) {
    let identity = identity(graph_bytes);
    assert_eq!(held.commit, 7);
    assert_eq!(
        held.names,
        [
            "ada", "alan", "grace", "josh", "lop", "marko", "peter", "ripple", "vadas"
        ]
    );
    // The rolled-back commits (vertex `engine`, marko's age 30) are not in the state.
    assert_eq!(held.marko_age, Some(ElementProperty::from(29i64)));
    assert!(
        held.schema
            .as_deref()
            .is_some_and(|s| s.contains("\"open\"")),
        "{:?}",
        held.schema
    );
    let mut definitions = held.definitions.clone();
    definitions.sort();
    assert_eq!(
        definitions,
        [(1u8, "older".to_string()), (3u8, "bio".to_string())]
    );
    assert_eq!(
        held.marks,
        [
            ("first".to_string(), 4),
            ("before-rollback".to_string(), 6),
            ("last".to_string(), 7)
        ]
    );
    assert_eq!(held.attic.len(), 1, "{:?}", held.attic);
    assert_eq!((held.attic[0].1, held.attic[0].2), (6, 8));
    assert_eq!(held.snapshots, [0, 4]);
    // Identity and creation parameters exactly as the 0.1.0 store had them (rule 22).
    assert_eq!(held.graph_id, identity.graph_id);
    assert_eq!(held.store_id, identity.store_id);
    assert_eq!(held.created_at, identity.created_at);
    assert_eq!(held.chunk_bytes, identity.chunk_bytes);
    assert_eq!(
        held.damage_policy == DamagePolicy::Continue,
        identity.continue_on_damage
    );
    assert_eq!(held.lineage, identity.lineage);
    assert_eq!(held.lineage.len(), 2);
}

/// The pinned content of the `backup/` fixture and of the store `backup.zip` restores: the
/// state of `store/` at commit 7 without its attic (a backup carries no attic) and with only the
/// newest snapshot.
#[cfg(feature = "persist-zip")]
fn assert_backup_fixture(held: &Holds, graph_bytes: &[u8]) {
    let identity = identity(graph_bytes);
    assert_eq!(held.commit, 7);
    assert_eq!(
        held.names,
        [
            "ada", "alan", "grace", "josh", "lop", "marko", "peter", "ripple", "vadas"
        ]
    );
    assert_eq!(held.marko_age, Some(ElementProperty::from(29i64)));
    assert_eq!(held.definitions.len(), 2);
    assert_eq!(held.marks.len(), 3);
    assert!(held.attic.is_empty());
    assert_eq!(held.snapshots, [4]);
    assert_eq!(held.store_id, identity.store_id);
    assert_eq!(held.graph_id, identity.graph_id);
    assert_eq!(held.lineage, identity.lineage);
}

fn graph_bytes(dir: &Arc<dyn StoreDir>) -> Vec<u8> {
    dir.read("GRAPH", 1 << 20).unwrap()
}

/// Every file but `GRAPH` and `GRAPH.copy` is the same, byte for byte; both `GRAPH` copies
/// are of the current version and equal to the older ones but for the version word and its
/// checksum.
fn assert_only_graph_rewritten(before: &[(String, Vec<u8>)], after: &[(String, Vec<u8>)]) {
    let others = |files: &[(String, Vec<u8>)]| -> Vec<(String, Vec<u8>)> {
        files
            .iter()
            .filter(|(n, _)| n != "GRAPH" && n != "GRAPH.copy")
            .cloned()
            .collect()
    };
    assert_eq!(others(before), others(after));
    for name in ["GRAPH", "GRAPH.copy"] {
        let old = &before.iter().find(|(n, _)| n == name).unwrap().1;
        let new = &after.iter().find(|(n, _)| n == name).unwrap().1;
        assert_eq!(version_of(old), 2);
        assert_eq!(version_of(new), persist_version());
        assert_eq!(old.len(), new.len());
        assert_eq!(old[12..252], new[12..252]);
        assert_eq!(old[256..], new[256..]);
    }
}

fn persist_version() -> u32 {
    graphersal::persist::STORE_FORMAT_VERSION
}

fn assert_upgrade_help(err: &PersistError, what: &str) {
    assert!(
        matches!(err, PersistError::OlderFormat { found: 2, .. }),
        "{what}: {err}"
    );
    let text = graphersal::error::format_diagnostic(err);
    for part in [
        "graphersal store upgrade",
        "--in-place",
        "Store::upgrade",
        "graphersal.Store.upgrade",
        "no older Graphersal is needed",
    ] {
        assert!(text.contains(part), "{what}: {part} missing in {text}");
    }
}

// ---------------------------------------------------------------------------------------------
// The fixtures as they are: refused with the converter's help
// ---------------------------------------------------------------------------------------------

#[test]
fn every_fixture_is_refused_as_older_with_the_command_that_upgrades_it() {
    let temp = Temp::new("refused");
    let store = fixture_copy(&temp, "store", "store");
    let backup = fixture_copy(&temp, "backup", "backup");
    let single = temp.join("single.gstore");
    fs::copy(fixtures().join("single.gstore"), &single).unwrap();

    assert_upgrade_help(
        &Store::open(&store, StoreOptions::new())
            .map(|_| ())
            .unwrap_err(),
        "open",
    );
    assert_upgrade_help(
        &Store::open_read_only(&store, RecoveryTarget::Latest)
            .map(|_| ())
            .unwrap_err(),
        "open_read_only",
    );
    assert_upgrade_help(&Store::verify_dir(&store).unwrap_err(), "verify");
    assert_upgrade_help(&Store::inspect(&store).unwrap_err(), "inspect");
    assert_upgrade_help(
        &Store::open(&single, StoreOptions::new())
            .map(|_| ())
            .unwrap_err(),
        "open single file",
    );
    assert_upgrade_help(
        &Store::open_backup(&backup, StoreOptions::new())
            .map(|_| ())
            .unwrap_err(),
        "open_backup",
    );
    assert_upgrade_help(
        &Store::restore_backup(&backup).unwrap_err(),
        "restore_backup",
    );
    // Nothing of them changed (an open's lock file aside).
    let lockless = |files: Vec<(String, Vec<u8>)>| -> Vec<(String, Vec<u8>)> {
        files
            .into_iter()
            .filter(|(n, _)| n != "LOCK" && n != "BACKUP")
            .collect()
    };
    assert_eq!(
        lockless(files_of(&store)),
        files_of(&fixtures().join("store"))
    );
    assert_eq!(
        lockless(files_of(&backup)),
        files_of(&fixtures().join("backup"))
    );
    assert_eq!(
        fs::read(&single).unwrap(),
        fs::read(fixtures().join("single.gstore")).unwrap()
    );
}

// ---------------------------------------------------------------------------------------------
// Upgrades
// ---------------------------------------------------------------------------------------------

#[test]
fn a_directory_store_upgrades_into_a_copy_with_everything_carried() {
    let temp = Temp::new("copy");
    let source = fixture_copy(&temp, "store", "store");
    let before = files_of(&source);
    let target = temp.join("upgraded");
    let report: UpgradeReport = Store::upgrade(&source, &target).unwrap();
    assert_eq!(
        (report.from_version, report.to_version),
        (2, persist_version())
    );
    assert!(!report.in_place && !report.already_current);
    assert!(report.verify.is_ok() && report.verify.snapshots_checked >= 2);
    assert!(report.backup.is_none());
    assert_eq!(report.files_copied, before.len());
    // The source is untouched (its lock file aside); the copy differs only in GRAPH.
    let lockless = |files: Vec<(String, Vec<u8>)>| -> Vec<(String, Vec<u8>)> {
        files.into_iter().filter(|(n, _)| n != "LOCK").collect()
    };
    assert_eq!(lockless(files_of(&source)), before);
    assert_only_graph_rewritten(&before, &lockless(files_of(&target)));
    let held = open_holds(&target);
    assert_store_fixture(&held, &fs::read(source.join("GRAPH")).unwrap());
    // The source stays an older store.
    assert!(matches!(
        Store::open(&source, StoreOptions::new()),
        Err(PersistError::OlderFormat { found: 2, .. })
    ));
}

#[test]
fn a_store_upgrades_in_place_on_every_backend() {
    let temp = Temp::new("in-place");
    // A directory, memory, and the directory fixture converted... in place on each.
    let dir_path = fixture_copy(&temp, "store", "store");
    let original = fs::read(dir_path.join("GRAPH")).unwrap();
    let mem = fixture_mem("store");
    let mem_dir = shared(&mem);
    for (what, dir) in [
        (
            "directory",
            Arc::new(graphersal::persist::FsDir::new(&dir_path)) as Arc<dyn StoreDir>,
        ),
        ("memory", mem_dir.clone()),
    ] {
        let before = store_files(&dir);
        let report = Store::upgrade_in_place(&dir).unwrap();
        assert!(report.in_place && !report.already_current, "{what}");
        assert_eq!(report.from_version, 2, "{what}");
        assert_eq!((report.files_copied, report.bytes_copied), (0, 0), "{what}");
        assert_only_graph_rewritten(&before, &store_files(&dir));
        assert_store_fixture(&open_holds(&dir), &original);
        // Again: nothing to do.
        let again = Store::upgrade_in_place(&dir).unwrap();
        assert!(again.already_current, "{what}");
        assert_eq!(again.from_version, persist_version());
    }
}

#[test]
fn a_single_file_store_upgrades_in_a_copy_in_place_and_into_a_directory() {
    let temp = Temp::new("single");
    let source = temp.join("single.gstore");
    fs::copy(fixtures().join("single.gstore"), &source).unwrap();
    let fixture_bytes = fs::read(&source).unwrap();
    let source_dir: Arc<dyn StoreDir> = Arc::new(SingleFileDir::new(&source));
    let original = graph_bytes(&source_dir);
    let before = store_files(&source_dir);

    let check = |held: Holds| {
        assert_eq!(held.commit, 2);
        assert_eq!(held.marks, [("m1".to_string(), 1)]);
        assert_eq!(held.store_id, identity(&original).store_id);
        assert_eq!(held.lineage, identity(&original).lineage);
        assert!(held.names.contains(&"ada".to_string()));
    };
    // Into a new single file, and into a directory.
    let file_copy = temp.join("upgraded.gstore");
    Store::upgrade(&source, &file_copy).unwrap();
    let copy_dir: Arc<dyn StoreDir> = Arc::new(SingleFileDir::new(&file_copy));
    assert_only_graph_rewritten(&before, &store_files(&copy_dir));
    check(open_holds(&copy_dir));
    let dir_copy = temp.join("upgraded-dir");
    Store::upgrade(&source, &dir_copy).unwrap();
    check(open_holds(&dir_copy));
    // The source file is unchanged (a copy only reads it).
    drop(source_dir);
    assert_eq!(fs::read(&source).unwrap(), fixture_bytes);
    // In place.
    let report = Store::upgrade_in_place(&source).unwrap();
    assert!(report.in_place && report.from_version == 2);
    let source_dir: Arc<dyn StoreDir> = Arc::new(SingleFileDir::new(&source));
    assert_only_graph_rewritten(&before, &store_files(&source_dir));
    check(open_holds(&source_dir));
}

#[test]
fn an_upgraded_store_takes_new_commits_in_a_new_wal_segment_and_keeps_its_history() {
    let temp = Temp::new("continue");
    let source = fixture_copy(&temp, "store", "store");
    Store::upgrade_in_place(&source).unwrap();
    let wal_before = files_of(&source.join("wal"));

    let store = Store::open(&source, StoreOptions::new()).unwrap();
    // A temporal value: a record of file format version 2, never appended to a version 1
    // segment.
    store
        .graph()
        .write()
        .transaction(|g| {
            let mut props = PropertyMap::default();
            props.insert("name", ElementProperty::from("hedy"));
            props.insert("age", ElementProperty::from(105i64));
            props.insert(
                "born",
                ElementProperty::from("1914-11-09".parse::<Date>().unwrap()),
            );
            g.add_vertex(None, vec!["person".into()], props)?;
            Ok::<_, GraphError>(())
        })
        .unwrap();
    store.mark("after-upgrade").unwrap();
    store.close().unwrap();
    let wal_after = files_of(&source.join("wal"));
    // The older segments are unchanged; the new commit is in a new segment of the current
    // file format version.
    for (name, bytes) in &wal_before {
        assert_eq!(
            wal_after.iter().find(|(n, _)| n == name).map(|(_, b)| b),
            Some(bytes),
            "{name} changed"
        );
    }
    let new: Vec<&(String, Vec<u8>)> = wal_after
        .iter()
        .filter(|(n, _)| !wal_before.iter().any(|(m, _)| m == n))
        .collect();
    assert_eq!(
        new.len(),
        1,
        "{:?}",
        wal_after.iter().map(|f| &f.0).collect::<Vec<_>>()
    );
    assert_eq!(new[0].0, "00000000000000000008.wal");
    assert_eq!(
        u32::from_le_bytes(new[0].1[8..12].try_into().unwrap()),
        graphersal::persist::FORMAT_VERSION
    );

    // Reopen: everything is there; verify is clean; a checkpoint merges across both file
    // versions; the old marks and the attic still work.
    let report = Store::verify_dir(&source).unwrap();
    assert!(report.is_ok(), "{report:?}");
    let store = Store::open(&source, StoreOptions::new()).unwrap();
    assert_eq!(store.graph().read().last_commit_seq(), 8);
    assert!(names(&store.graph().read()).contains(&"hedy".to_string()));
    store.checkpoint(None).unwrap();
    let marks: Vec<String> = store
        .marks()
        .unwrap()
        .iter()
        .map(|m| m.name().to_string())
        .collect();
    assert_eq!(marks, ["first", "before-rollback", "last", "after-upgrade"]);
    let past = store
        .read_only_at(RecoveryTarget::Mark("first".into()))
        .unwrap();
    assert!(names(&past.graph).contains(&"ada".to_string()));
    assert!(!names(&past.graph).contains(&"alan".to_string()));
    store.close().unwrap();
    assert!(Store::verify_dir(&source).unwrap().is_ok());
    let store = Store::open(&source, StoreOptions::new()).unwrap();
    assert!(names(&store.graph().read()).contains(&"hedy".to_string()));
    // A rollback across the upgrade, into the attic and back.
    store
        .rollback_to(RecoveryTarget::Mark("last".into()), RollbackMode::Move)
        .unwrap();
    assert!(!names(&store.graph().read()).contains(&"hedy".to_string()));
    let entries = store.attic_list().unwrap();
    assert_eq!(entries.len(), 2);
    let newest = entries.iter().max_by_key(|e| e.time).unwrap().id.clone();
    store.attic_restore(&newest).unwrap();
    assert!(names(&store.graph().read()).contains(&"hedy".to_string()));
    store.close().unwrap();
    assert!(Store::verify_dir(&source).unwrap().is_ok());
}

#[test]
fn an_older_wal_segment_without_a_record_is_replaced_by_one_of_the_current_version() {
    // The `fresh/` fixture: opened once by 0.1.0 without a commit, so its WAL segment 1 holds
    // only a version 1 header, and the next commit (1) would go into a segment of that name.
    let dir = fixture_mem("fresh");
    let segment = "wal/00000000000000000001.wal";
    let old = dir.read(segment, 1 << 20).unwrap();
    assert_eq!(old.len(), 64);
    assert_eq!(u32::from_le_bytes(old[8..12].try_into().unwrap()), 1);
    Store::upgrade_in_place(&dir).unwrap();
    let store = Store::open(&dir, StoreOptions::new()).unwrap();
    assert_eq!(store.graph().read().last_commit_seq(), 0);
    store
        .graph()
        .write()
        .traversal_mut()
        .add_v("person")
        .property("name", "ada")
        .to_list()
        .unwrap();
    store.close().unwrap();
    let new = dir.read(segment, 1 << 20).unwrap();
    assert_eq!(
        u32::from_le_bytes(new[8..12].try_into().unwrap()),
        graphersal::persist::FORMAT_VERSION
    );
    assert!(new.len() > 64);
    assert_eq!(
        dir.list("wal").unwrap().len(),
        1,
        "no second segment was needed"
    );
    assert!(Store::verify_dir(&dir).unwrap().is_ok());
    let store = Store::open(&dir, StoreOptions::new()).unwrap();
    assert_eq!(store.graph().read().last_commit_seq(), 1);
    assert!(names(&store.graph().read()).contains(&"ada".to_string()));
}

/// Rule 22: every creation parameter, also those the fixtures have at their defaults (the
/// damage policy `continue`, a chunk size), is carried by both modes.
#[test]
fn an_upgrade_carries_every_creation_parameter() {
    let dir = MemDir::new();
    let store = Store::create(
        &dir,
        StoreOptions::new()
            .with_damage_policy(DamagePolicy::Continue)
            .with_chunk_bytes(65_536),
    )
    .unwrap();
    store
        .graph()
        .write()
        .traversal_mut()
        .add_v("person")
        .property("name", "ada")
        .to_list()
        .unwrap();
    let info = store.info().unwrap();
    store.close().unwrap();
    // Made older: store format version 2 (Graphersal 0.1.0) has the same GRAPH layout.
    for name in ["GRAPH", "GRAPH.copy"] {
        let mut bytes = dir.read(name, 1 << 20).unwrap();
        bytes[8..12].copy_from_slice(&2u32.to_le_bytes());
        let crc = crate::store_tests::crc32c(&bytes[..252]);
        bytes[252..256].copy_from_slice(&crc.to_le_bytes());
        dir.write_atomic(name, &bytes).unwrap();
    }
    let copy = MemDir::new();
    Store::upgrade(&dir, &copy).unwrap();
    Store::upgrade_in_place(&dir).unwrap();
    for upgraded in [&copy, &dir] {
        let after = Store::inspect(upgraded).unwrap();
        assert_eq!(after.format_version, persist_version());
        assert_eq!(after.damage_policy, DamagePolicy::Continue);
        assert_eq!(after.chunk_bytes, 65_536);
        assert_eq!(after.store_id, info.store_id);
        assert_eq!(after.created_at, info.created_at);
        assert_eq!(after.graph_id, info.graph_id);
        assert_eq!(after.lineage, info.lineage);
        assert_eq!(after.commit_seq, info.commit_seq);
    }
}

#[test]
fn the_attic_entry_of_an_older_store_stays_restorable() {
    let temp = Temp::new("attic");
    let source = fixture_copy(&temp, "store", "store");
    let target = temp.join("upgraded");
    Store::upgrade(&source, &target).unwrap();
    let store = Store::open(&target, StoreOptions::new()).unwrap();
    let entry = store.attic_list().unwrap().remove(0);
    // Commits were made after the rollback (grace), so a restore is refused; a fork of the
    // entry gives the rolled-back state.
    assert!(store.attic_can_restore(&entry.id).is_err());
    let fork = temp.join("fork");
    store.attic_fork(&entry.id, &fork).unwrap();
    store.close().unwrap();
    let forked = Store::open(&fork, StoreOptions::new()).unwrap();
    let graph = forked.graph().read();
    assert!(names(&graph).contains(&"engine".to_string()));
    assert!(!names(&graph).contains(&"grace".to_string()));
    assert_eq!(age_of(&graph, "marko"), Some(ElementProperty::from(30i64)));
}

#[test]
fn a_backup_directory_upgrades_and_stays_a_backup_that_takes_increments() {
    let temp = Temp::new("backup");
    let source = fixture_copy(&temp, "store", "store");
    let backup = fixture_copy(&temp, "backup", "backup");
    let original = fs::read(backup.join("GRAPH")).unwrap();
    let marker = identity(&original).backup_commit;
    assert_eq!(marker, Some(7));

    // A copy of the backup is a backup too.
    let copy = temp.join("backup-copy");
    let report = Store::upgrade(&backup, &copy).unwrap();
    assert_eq!(report.backup.map(|m| m.commit_seq), Some(7));
    assert_eq!(
        Store::backup_marker_dir(&copy)
            .unwrap()
            .map(|m| m.commit_seq),
        Some(7)
    );
    let opened = Store::open_backup(&copy, StoreOptions::new()).unwrap();
    assert_eq!(opened.graph().read().last_commit_seq(), 7);
    drop(opened);

    // In place; then the upgraded store continues the upgraded backup with an increment (the
    // same store id).
    let report = Store::upgrade_in_place(&backup).unwrap();
    assert_eq!(report.backup.map(|m| m.commit_seq), Some(7));
    Store::upgrade_in_place(&source).unwrap();
    let store = Store::open(&source, StoreOptions::new()).unwrap();
    store
        .graph()
        .write()
        .traversal_mut()
        .add_v("person")
        .property("name", "katherine")
        .property("age", 101i64)
        .to_list()
        .unwrap();
    let increment = store.backup(&backup).unwrap();
    assert!(increment.incremental, "{increment:?}");
    assert_eq!(increment.commit_seq, 8);
    store.close().unwrap();
    // Restored, the backup is the live store.
    Store::restore_backup(&backup).unwrap();
    let live = Store::open(&backup, StoreOptions::new()).unwrap();
    assert!(names(&live.graph().read()).contains(&"katherine".to_string()));
    assert_eq!(live.store_id().unwrap(), identity(&original).store_id);
}

#[cfg(feature = "persist-zip")]
#[test]
fn a_zip_backup_of_an_older_store_is_upgraded_as_it_is_restored() {
    let temp = Temp::new("zip");
    let zip = fs::read(fixtures().join("backup.zip")).unwrap();
    let restored = temp.join("restored");
    Store::restore_zip(zip.as_slice(), &restored).unwrap();
    assert!(Store::backup_marker_dir(&restored).unwrap().is_none());
    let graph = fs::read(restored.join("GRAPH")).unwrap();
    assert_eq!(version_of(&graph), persist_version());
    let held = open_holds(&restored);
    assert_backup_fixture(&held, &fs::read(fixtures().join("backup/GRAPH")).unwrap());
    // Into memory too.
    let mem = MemDir::new();
    Store::restore_zip(zip.as_slice(), &mem).unwrap();
    assert_eq!(open_holds(&mem).data, held.data);
}

// ---------------------------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------------------------

fn store_cause(result: Result<UpgradeReport, PersistError>) -> StoreFailure {
    match result {
        Err(PersistError::Store { cause, .. }) => cause,
        other => panic!("expected a Store refusal, got {other:?}"),
    }
}

#[test]
fn an_upgrade_into_a_directory_that_holds_something_is_refused() {
    let source = fixture_mem("store");
    let target = MemDir::new();
    target.write_atomic("something", b"x").unwrap();
    assert!(matches!(
        Store::upgrade(&source, &target),
        Err(PersistError::DirectoryNotEmpty { .. })
    ));
    assert_eq!(target.read("something", 10).unwrap(), b"x");
    // An existing store of the current format too.
    let current = MemDir::new();
    Store::create(&current, StoreOptions::new())
        .unwrap()
        .close()
        .unwrap();
    assert!(matches!(
        Store::upgrade(&source, &current),
        Err(PersistError::DirectoryNotEmpty { .. })
    ));
}

#[test]
fn a_store_in_use_is_refused() {
    let source = fixture_mem("store");
    let before = store_files(&shared(&source));
    let lock = source.lock(LockKind::Writer).unwrap().unwrap();
    assert!(matches!(
        Store::upgrade(&source, MemDir::new()),
        Err(PersistError::Locked { .. })
    ));
    assert!(matches!(
        Store::upgrade_in_place(&source),
        Err(PersistError::Locked { .. })
    ));
    drop(lock);
    assert_eq!(store_files(&shared(&source)), before);
}

#[test]
fn a_current_newer_or_pre_release_store_is_refused() {
    // Current: a copy has nothing to convert; in place is a no-op.
    let current = MemDir::new();
    Store::create(&current, StoreOptions::new())
        .unwrap()
        .close()
        .unwrap();
    assert_eq!(
        store_cause(Store::upgrade(&current, MemDir::new())),
        StoreFailure::AlreadyCurrent
    );
    let err = Store::upgrade(&current, MemDir::new()).unwrap_err();
    assert!(graphersal::error::format_diagnostic(&err).contains("store convert"));
    assert!(Store::upgrade_in_place(&current).unwrap().already_current);

    let rewrite = |dir: &MemDir, version: u32| {
        for name in ["GRAPH", "GRAPH.copy"] {
            let mut bytes = dir.read(name, 1 << 20).unwrap();
            bytes[8..12].copy_from_slice(&version.to_le_bytes());
            let crc = crate::store_tests::crc32c(&bytes[..252]);
            bytes[252..256].copy_from_slice(&crc.to_le_bytes());
            dir.write_atomic(name, &bytes).unwrap();
        }
    };
    // Newer.
    let newer = fixture_mem("store");
    rewrite(&newer, persist_version() + 1);
    for result in [
        Store::upgrade(&newer, MemDir::new()),
        Store::upgrade_in_place(&newer),
    ] {
        assert!(
            matches!(result, Err(PersistError::UnsupportedVersion { .. })),
            "{result:?}"
        );
    }
    // Version 1 was never released: the migration through a packed snapshot.
    let pre_release = fixture_mem("store");
    rewrite(&pre_release, 1);
    for result in [
        Store::upgrade(&pre_release, MemDir::new()),
        Store::upgrade_in_place(&pre_release),
    ] {
        match result {
            Err(err @ PersistError::OlderFormat { found: 1, .. }) => {
                let text = graphersal::error::format_diagnostic(&err);
                assert!(text.contains("pre-release"), "{text}");
                assert!(text.contains("--from graph.gsnap"), "{text}");
            }
            other => panic!("expected OlderFormat 1, got {other:?}"),
        }
    }
    // Not a store.
    assert!(matches!(
        Store::upgrade(MemDir::new(), MemDir::new()),
        Err(PersistError::NotAStore { .. })
    ));
}

#[test]
fn a_damaged_older_store_is_refused_and_repaired_into_a_current_store() {
    let source = fixture_mem("store");
    // A flipped byte inside the vertex segment of the newest snapshot.
    let name = "snapshots/00000000000000000004/v-000000.seg";
    let mut bytes = source.read(name, 1 << 20).unwrap();
    let at = bytes.len() / 2;
    bytes[at] ^= 0x40;
    source.write_atomic(name, &bytes).unwrap();
    let before = store_files(&shared(&source));

    let target = MemDir::new();
    for result in [
        Store::upgrade(&source, &target),
        Store::upgrade_in_place(&source),
    ] {
        let err = result.unwrap_err();
        assert!(
            matches!(
                err,
                PersistError::Store {
                    cause: StoreFailure::DamagedSource,
                    ..
                }
            ),
            "{err}"
        );
        let text = graphersal::error::format_diagnostic(&err);
        assert!(text.contains("v-000000.seg"), "{text}");
        assert!(text.contains("graphersal store repair"), "{text}");
    }
    // Nothing written anywhere.
    assert_eq!(store_files(&shared(&source)), before);
    assert!(store_files(&shared(&target)).is_empty());

    // The damage report and the repair read the older store; the repair is a current store.
    let report = Store::damage_report_dir(&source).unwrap();
    assert!(!report.damage.is_empty());
    let repaired = MemDir::new();
    Store::repair_dir(&source, &repaired).unwrap();
    assert_eq!(
        version_of(&repaired.read("GRAPH", 1 << 20).unwrap()),
        persist_version()
    );
    let store = Store::open(&repaired, StoreOptions::new()).unwrap();
    assert!(names(&store.graph().read()).contains(&"grace".to_string()));
    assert_eq!(store_files(&shared(&source)), before);
}

// ---------------------------------------------------------------------------------------------
// Crashes
// ---------------------------------------------------------------------------------------------

/// Runs `upgrade` on a crashing copy of `base` after every number of writes in turn; returns
/// the crash images (the writes before the crash applied).
fn crash_images(base: &MemDir, upgrade: impl Fn(Arc<dyn StoreDir>) -> bool) -> Vec<MemDir> {
    let mut images = Vec::new();
    for budget in 0.. {
        let image = base.deep_copy();
        let crashing = Crashing {
            inner: image.clone(),
            budget: Arc::new(AtomicI64::new(budget)),
        };
        let finished = upgrade(Arc::new(crashing.clone()));
        if !crashing.crashed() {
            assert!(finished, "no crash, but the upgrade failed");
            break;
        }
        images.push(image);
        assert!(budget < 10_000, "the upgrade never finished");
    }
    images
}

#[test]
fn an_in_place_upgrade_survives_a_crash_after_every_write() {
    let base = fixture_mem("store");
    let original = base.read("GRAPH", 1 << 20).unwrap();
    let images = crash_images(&base, |dir| Store::upgrade_in_place(&dir).is_ok());
    assert!(!images.is_empty());
    for (step, image) in images.iter().enumerate() {
        // The image is either the older store, unchanged, or opens as the upgraded one.
        match Store::open(image, StoreOptions::new()) {
            Err(PersistError::OlderFormat { found: 2, .. }) => {}
            Ok(store) => {
                assert_eq!(store.graph().read().last_commit_seq(), 7, "step {step}");
                store.close().unwrap();
            }
            Err(other) => panic!("crash after {step} writes: {other}"),
        }
        // Repeating the upgrade completes it.
        let again = image.deep_copy();
        Store::upgrade_in_place(&again).unwrap();
        assert!(Store::verify_dir(&again).unwrap().is_ok(), "step {step}");
        assert_store_fixture(&open_holds(&again), &original);
    }
}

#[test]
fn a_copying_upgrade_survives_a_crash_after_every_write() {
    let source = fixture_mem("store");
    let original = source.read("GRAPH", 1 << 20).unwrap();
    let before = store_files(&shared(&source));
    let target = MemDir::new();
    let images = crash_images(&target, |dir| Store::upgrade(&source, &dir).is_ok());
    assert!(images.len() > 10, "{}", images.len());
    for (step, image) in images.iter().enumerate() {
        // Never a store that is not complete and verified: no store at all, or the whole one.
        let marked = image
            .entry_kind("upgrade-in-progress.tmp")
            .unwrap()
            .is_some();
        let complete = Store::is_store(shared(image)).unwrap();
        if complete {
            assert_store_fixture(&open_holds(&image.deep_copy()), &original);
        }
        let again = image.deep_copy();
        let result = Store::upgrade(&source, &again);
        if complete && !marked {
            // Finished but for the last sync: the upgrade is complete, the target taken.
            assert!(
                matches!(result, Err(PersistError::DirectoryNotEmpty { .. })),
                "step {step}: {result:?}"
            );
        } else {
            // Repeating the upgrade into it clears what the crash left and completes.
            result.unwrap_or_else(|e| panic!("step {step}: {e}"));
            assert_store_fixture(&open_holds(&again), &original);
        }
    }
    assert_eq!(store_files(&shared(&source)), before);
}

/// Stores of the RELEASED Graphersal 0.4.0 (store format version 3, embedding models as files
/// under `models/`; the fixtures in `tests/data/v0.4.0/`, see the README there): the upgrade
/// to store format version 4 migrates every model into a blob with a catalog definition (one
/// commit), lists the models the schema lacks and the vector properties without a rule, and
/// recomputes nothing.
mod v040 {
    use super::*;
    use graphersal::catalog::DefinitionKind;
    use graphersal::persist::{RecoveryTarget, VectorWithoutRule};
    use graphersal::storage::ElementKind;

    /// The 0.4.0 fixtures (never written: every test copies them first).
    fn fixtures() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/v0.4.0")
    }

    /// The fixture directory `name` copied into `temp`.
    fn copy(temp: &Temp, name: &str) -> PathBuf {
        let to = temp.join(name);
        for (relative, bytes) in files_of(&fixtures().join(name)) {
            let path = to.join(&relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }
        to
    }

    /// Every file below `dir` but the lock files (an operation creates `LOCK`).
    fn data_files(dir: &Path) -> Vec<(String, Vec<u8>)> {
        files_of(dir)
            .into_iter()
            .filter(|(name, _)| name != "LOCK" && name != "BACKUP")
            .collect()
    }

    /// The fixture directory `name` in memory.
    fn in_memory(name: &str) -> MemDir {
        let dir = MemDir::named(name);
        for (relative, bytes) in files_of(&fixtures().join(name)) {
            if let Some((parent, _)) = relative.rsplit_once('/') {
                dir.create_dir_all(parent).unwrap();
            }
            dir.write_atomic(&relative, &bytes).unwrap();
        }
        dir
    }

    fn holds_vector(properties: Option<&graphersal::prelude::PropertyMap>) -> bool {
        matches!(
            properties.and_then(|p| p.get("emb")),
            Some(ElementProperty::Vector(_))
        )
    }

    const TINY: &str = "model2vec/acme/tiny@70c61d0ed332";
    const OTHER_040: &str = "model2vec@b6b362e7a651";
    const OTHER: &str = "model2vec/local/b6b362e7a651@b6b362e7a651";

    fn vector(report: &UpgradeReport, label: &str) -> VectorWithoutRule {
        report
            .vectors_without_rule
            .iter()
            .find(|v| v.label() == label)
            .unwrap_or_else(|| panic!("{label}: {:?}", report.vectors_without_rule))
            .clone()
    }

    /// The report and store after the upgrade of the fixture `store/`: both models converted
    /// (the one without a repository as `local/<12 hex>`), one commit, `models/` gone, the
    /// vectors and the schema's other declarations as they were.
    fn assert_store_migrated(report: &UpgradeReport, dir: &Path) {
        assert_eq!(
            (report.from_version, report.to_version),
            (3, persist_version())
        );
        assert_eq!(report.to_version, 4);
        let converted: Vec<(&str, &str, &str, bool)> = report
            .models_converted
            .iter()
            .map(|m| (m.repo(), m.identity(), m.former_identity(), m.renamed()))
            .collect();
        assert_eq!(
            converted,
            [
                ("acme/tiny", TINY, TINY, false),
                ("local/b6b362e7a651", OTHER, OTHER_040, true),
            ]
        );
        assert!(report.models_converted.iter().all(|m| m.bytes() > 0));
        assert_eq!(report.migration_commit, Some(12));
        assert!(
            report.models_missing.is_empty(),
            "{:?}",
            report.models_missing
        );
        // The vector properties without a rule, counted exactly (the fixture's last commit is
        // in its newest snapshot).
        let doc = vector(report, "doc");
        assert_eq!(
            (
                doc.element_kind(),
                doc.property(),
                doc.model(),
                doc.elements(),
                doc.exact()
            ),
            (ElementKind::Vertex, "emb", "acme/tiny", 3, true)
        );
        assert_eq!(doc.embedder(), TINY);
        let person = vector(report, "person");
        assert_eq!((person.model(), person.elements()), ("hash-ngram/1", 2));
        let note = vector(report, "note");
        assert_eq!((note.model(), note.elements()), ("local/b6b362e7a651", 1));
        assert_eq!(report.vectors_without_rule.len(), 3);
        assert_eq!(
            doc.schema_patch(),
            "g.patch_schema(#{vertices: #{doc: #{schema: #{properties: #{emb: #{from: \"<source property>\"}}}}}})"
        );
        let text = doc.note("st");
        assert!(
            text.starts_with(
                "doc.emb has vectors of acme/tiny but no source property\n  vectors: 3 element(s)"
            ),
            "{text}"
        );
        assert!(
            text.contains("graphersal --graph st -e 'g.patch_schema("),
            "{text}"
        );
        assert!(text.contains("re-embeds every doc"), "{text}");

        let store = Store::open(dir, StoreOptions::new()).unwrap();
        let models = store.models().unwrap();
        let mut identities: Vec<&str> = models.iter().map(|m| m.identity.as_str()).collect();
        identities.sort();
        assert_eq!(identities, [TINY, OTHER]);
        assert!(
            models.iter().all(|m| !m.default),
            "no default model after a migration"
        );
        let tiny = models.iter().find(|m| m.identity == TINY).unwrap();
        assert_eq!((tiny.dimension, tiny.license.as_str()), (3, "mit"));
        {
            let graph = store.graph().read();
            assert!(
                graph
                    .definition(DefinitionKind::MODEL, TINY)
                    .unwrap()
                    .is_some()
            );
            // The schema: the renamed model's embedder follows it, nothing else changed (no
            // `from` anywhere: the upgrade never guesses a source).
            let json = graph.get_schema().unwrap().unwrap().to_json();
            assert!(
                json.contains(&format!("\"embedder\":\"{OTHER}\"")),
                "{json}"
            );
            assert!(json.contains(&format!("\"embedder\":\"{TINY}\"")), "{json}");
            assert!(!json.contains("\"from\""), "{json}");
            // The vectors are the data 0.4.0 wrote.
            let with = |label: &str| {
                graph
                    .vertices()
                    .unwrap()
                    .filter(|v| {
                        graph.vertex_label(*v).unwrap() == Some(label)
                            && holds_vector(graph.vertex_properties(*v).unwrap())
                    })
                    .count()
            };
            assert_eq!((with("doc"), with("person"), with("note")), (3, 2, 1));
        }
        store.close().unwrap();
        // The migration commit: principal `upgrade`, attribute `upgrade` = "3->4", in a new WAL
        // segment of its own.
        let mut segments: Vec<PathBuf> = fs::read_dir(dir.join("wal"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        segments.sort();
        assert_eq!(
            segments.last().unwrap().file_name().unwrap(),
            "00000000000000000012.wal"
        );
        let commits: Vec<_> =
            graphersal::persist::read_journal(fs::File::open(segments.last().unwrap()).unwrap())
                .unwrap()
                .filter_map(|r| match r.unwrap() {
                    graphersal::persist::JournalRecord::Commit(c) => Some(c),
                    _ => None,
                })
                .collect();
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].commit_seq(), 12);
        let metadata = commits[0].metadata();
        assert_eq!(metadata.principal(), Some("upgrade"));
        assert_eq!(
            metadata.attributes().get("upgrade"),
            Some(&ElementProperty::from("3->4"))
        );
        assert!(Store::verify_dir(dir).unwrap().is_ok());
    }

    #[test]
    fn a_040_store_with_a_model_upgrades_in_place_to_blobs_and_definitions() {
        let temp = Temp::new("v040-in-place");
        let dir = copy(&temp, "store");
        // Before: refused as older, the help names the upgrade and the models.
        match Store::open(&dir, StoreOptions::new()) {
            Err(err @ PersistError::OlderFormat { found: 3, .. }) => {
                let help = graphersal::error::format_diagnostic(&err);
                assert!(help.contains("store upgrade"), "{help}");
                assert!(help.contains("embedding models"), "{help}");
            }
            other => panic!("{other:?}"),
        }
        let report = Store::upgrade_in_place(&dir).unwrap();
        assert!(report.in_place && !report.already_current);
        assert!(!dir.join("models").exists());
        assert!(!dir.join("INTENT").exists());
        assert_eq!(version_of(&fs::read(dir.join("GRAPH")).unwrap()), 4);
        assert!(dir.join("blobs").is_dir());
        assert_store_migrated(&report, &dir);
        // Again: nothing to do.
        assert!(Store::upgrade_in_place(&dir).unwrap().already_current);
    }

    #[test]
    fn a_040_single_file_store_upgrades() {
        let temp = Temp::new("v040-single");
        let file = temp.join("single.gstore");
        fs::copy(fixtures().join("single.gstore"), &file).unwrap();
        let before = fs::read(&file).unwrap();
        // Into a copy (another single file), then in place.
        let copy = temp.join("copy.gstore");
        let report = Store::upgrade(&file, &copy).unwrap();
        assert_eq!(
            fs::read(&file).unwrap(),
            before,
            "the source is not changed"
        );
        assert_eq!(report.models_converted.len(), 1);
        assert_eq!(report.models_converted[0].identity(), TINY);
        // The vectors are all in the WAL after the newest snapshot: the count says so.
        let doc = vector(&report, "doc");
        assert!(!doc.exact());
        assert!(doc.commits_not_counted() > 0);
        assert!(
            doc.note("x").contains("later commit(s) not counted"),
            "{}",
            doc.note("x")
        );
        let report = Store::upgrade_in_place(&file).unwrap();
        assert_eq!(report.migration_commit, Some(11));
        for path in [&file, &copy] {
            let store = Store::open(path, StoreOptions::new()).unwrap();
            assert_eq!(store.models().unwrap().len(), 1);
            let docs = store
                .graph()
                .read()
                .vertices()
                .unwrap()
                .filter(|v| holds_vector(store.graph().read().vertex_properties(*v).unwrap()))
                .count();
            assert_eq!(docs, 5);
            store.close().unwrap();
            assert!(Store::verify_dir(path).unwrap().is_ok());
        }
    }

    #[test]
    fn an_upgrade_into_a_copy_converts_the_copy_only() {
        let temp = Temp::new("v040-copy");
        let dir = copy(&temp, "store");
        let before = data_files(&dir);
        let to = temp.join("upgraded");
        let report = Store::upgrade(&dir, &to).unwrap();
        assert!(!report.in_place);
        assert_eq!(data_files(&dir), before, "the source is not changed");
        assert!(dir.join("models").is_dir());
        assert!(!to.join("models").exists());
        assert!(!to.join("upgrade-in-progress.tmp").exists());
        assert_store_migrated(&report, &to);
        // The same store: its identity is carried.
        assert_eq!(
            identity(&fs::read(to.join("GRAPH")).unwrap()),
            identity(&fs::read(dir.join("GRAPH")).unwrap())
        );
    }

    #[test]
    fn an_interrupted_model_migration_completes_at_the_next_open() {
        let base = in_memory("store");
        let images = crash_images(&base, |dir| Store::upgrade_in_place(&dir).is_ok());
        assert!(images.len() > 5, "{}", images.len());
        let mut completed_at_open = 0;
        for (step, image) in images.iter().enumerate() {
            // Either the older store (refused as older: upgrade it again), or one of the
            // current version that the open completes.
            let opened = image.deep_copy();
            match Store::open(&opened, StoreOptions::new()) {
                Err(PersistError::OlderFormat { found: 3, .. }) => {}
                Ok(store) => {
                    if store.open_report().intent_completed.as_deref() == Some("upgrade_models") {
                        completed_at_open += 1;
                    }
                    let mut models: Vec<String> = store
                        .models()
                        .unwrap()
                        .into_iter()
                        .map(|m| m.identity)
                        .collect();
                    models.sort();
                    assert_eq!(models, [TINY, OTHER], "step {step}");
                    assert_eq!(store.graph().read().last_commit_seq(), 12, "step {step}");
                    store.close().unwrap();
                    assert!(
                        opened.entry_kind("models").unwrap().is_none(),
                        "step {step}"
                    );
                    assert!(
                        opened.entry_kind("INTENT").unwrap().is_none(),
                        "step {step}"
                    );
                }
                Err(other) => panic!("crash after {step} writes: {other}"),
            }
            // Repeating the upgrade completes it, with one migration commit.
            let again = image.deep_copy();
            let report =
                Store::upgrade_in_place(&again).unwrap_or_else(|e| panic!("step {step}: {e}"));
            // It lists the models still under models/ (a crash during their removal leaves
            // fewer); their definitions exist once, whatever the step.
            assert!(
                report.models_converted.len() <= 2,
                "step {step}: {report:?}"
            );
            assert!(
                report.migration_commit.is_none_or(|c| c == 12),
                "step {step}"
            );
            let store = Store::open(&again, StoreOptions::new()).unwrap();
            assert_eq!(store.graph().read().last_commit_seq(), 12, "step {step}");
            assert_eq!(store.models().unwrap().len(), 2, "step {step}");
            store.close().unwrap();
            let verified = Store::verify_dir(&again).unwrap();
            assert!(verified.is_ok(), "step {step}: {:?}", verified.problems);
        }
        assert!(completed_at_open > 0, "no image had a pending migration");
    }

    #[test]
    fn a_damaged_040_model_refuses_the_upgrade_before_writing() {
        let temp = Temp::new("v040-damaged");
        let dir = copy(&temp, "store");
        let weights = dir.join("models/model2vec-70c61d0ed332/model.safetensors");
        let mut bytes = fs::read(&weights).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0x55;
        fs::write(&weights, bytes).unwrap();
        let before = data_files(&dir);
        for result in [
            Store::upgrade_in_place(&dir),
            Store::upgrade(&dir, temp.join("copy")),
        ] {
            match result {
                Err(
                    err @ PersistError::Store {
                        cause: StoreFailure::UpgradeModelDamaged,
                        ..
                    },
                ) => {
                    let text = graphersal::error::format_diagnostic(&err);
                    assert!(text.contains("model2vec-70c61d0ed332"), "{text}");
                    assert!(text.contains("model.safetensors"), "{text}");
                    assert!(text.contains("embed-model import"), "{text}");
                }
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(data_files(&dir), before, "nothing was written");
        assert!(!temp.join("copy").join("GRAPH").exists());
    }

    #[test]
    fn a_store_without_models_only_changes_the_version_word() {
        // The 0.2.3 fixture: store format version 3, no models/.
        let temp = Temp::new("v040-no-models");
        let from = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/v0.2.3/store");
        let dir = temp.join("store");
        for (relative, bytes) in files_of(&from) {
            let path = dir.join(&relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }
        let before = data_files(&dir);
        let report = Store::upgrade_in_place(&dir).unwrap();
        assert_eq!((report.from_version, report.to_version), (3, 4));
        assert!(report.models_converted.is_empty());
        assert_eq!(report.migration_commit, None);
        assert!(report.vectors_without_rule.is_empty());
        let after = data_files(&dir);
        // GRAPH and GRAPH.copy rewritten (the version word and its checksum); every other file
        // byte for byte, nothing added (no blob, no journal record, no INTENT).
        let others = |files: &[(String, Vec<u8>)]| -> Vec<(String, Vec<u8>)> {
            files
                .iter()
                .filter(|(name, _)| !name.starts_with("GRAPH") && name != "LOCK")
                .cloned()
                .collect()
        };
        assert_eq!(others(&after), others(&before));
        for name in ["GRAPH", "GRAPH.copy"] {
            let old = &before.iter().find(|(n, _)| n == name).unwrap().1;
            let new = &after.iter().find(|(n, _)| n == name).unwrap().1;
            assert_eq!((version_of(old), version_of(new)), (3, 4));
            assert_eq!(old[12..252], new[12..252]);
            assert_eq!(old[256..], new[256..]);
        }
        let store = Store::open(&dir, StoreOptions::new()).unwrap();
        assert_eq!(store.graph().read().last_commit_seq(), 4);
        store.close().unwrap();
    }

    #[test]
    fn the_report_names_models_the_schema_needs_but_the_store_lacks() {
        let temp = Temp::new("v040-missing");
        let dir = copy(&temp, "missing");
        let report = Store::upgrade_in_place(&dir).unwrap();
        assert!(report.models_converted.is_empty());
        assert_eq!(report.models_missing.len(), 1);
        let missing = &report.models_missing[0];
        assert_eq!(missing.identity(), "model2vec/acme/gone@0123456789ab");
        assert_eq!(missing.repo(), Some("acme/gone"));
        assert_eq!(missing.needed_by(), ["doc.emb"]);
        assert_eq!(
            missing.commands("st"),
            [
                "graphersal embed-model download acme/gone",
                "graphersal embed-model import acme/gone --into st --default"
            ]
        );
        // The store does not open until the model is imported.
        match Store::open(&dir, StoreOptions::new()) {
            Err(PersistError::Store {
                cause: StoreFailure::ModelUnavailable,
                ..
            }) => {}
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_jump_before_the_migration_commit_has_no_models() {
        let temp = Temp::new("v040-jump");
        let dir = copy(&temp, "store");
        Store::upgrade_in_place(&dir).unwrap();
        let store = Store::open(&dir, StoreOptions::new()).unwrap();
        // At the migration commit the models are there.
        let at = store.read_only_at(RecoveryTarget::CommitSeq(12)).unwrap();
        assert_eq!(at.graph.definitions().unwrap().count(), 2);
        // Before it the catalog has none: the schema naming them refuses the view.
        match store.read_only_at(RecoveryTarget::CommitSeq(11)) {
            Err(
                err @ PersistError::Store {
                    cause: StoreFailure::ModelUnavailable,
                    ..
                },
            ) => {
                let text = err.to_string();
                assert!(
                    text.contains("not in the database at this position"),
                    "{text}"
                );
            }
            other => panic!("{:?}", other.map(|r| r.report.commit_seq)),
        }
        store.close().unwrap();
    }

    #[cfg(feature = "persist-zip")]
    #[test]
    fn the_040_zip_backup_restores_with_its_models() {
        let temp = Temp::new("v040-zip");
        let zip = fs::read(fixtures().join("backup.zip")).unwrap();
        let restored = temp.join("restored");
        Store::restore_zip(zip.as_slice(), &restored).unwrap();
        assert!(!restored.join("models").exists());
        assert!(Store::backup_marker_dir(&restored).unwrap().is_none());
        let store = Store::open(&restored, StoreOptions::new()).unwrap();
        let mut models: Vec<String> = store
            .models()
            .unwrap()
            .into_iter()
            .map(|m| m.identity)
            .collect();
        models.sort();
        assert_eq!(models, [TINY, OTHER]);
        store.close().unwrap();
        // Into memory too.
        let mem = MemDir::new();
        Store::restore_zip(zip.as_slice(), &mem).unwrap();
        assert!(mem.entry_kind("models").unwrap().is_none());
        assert_eq!(Store::models_dir(&mem).unwrap().len(), 2);
    }

    #[test]
    fn the_040_packed_snapshot_imports_its_models() {
        let temp = Temp::new("v040-gsnap");
        let packed = fs::read(fixtures().join("snapshot.gsnap")).unwrap();
        let dir = temp.join("store");
        let store =
            Store::create_from_packed(&dir, packed.as_slice(), StoreOptions::new()).unwrap();
        let mut models: Vec<String> = store
            .models()
            .unwrap()
            .into_iter()
            .map(|m| m.identity)
            .collect();
        models.sort();
        assert_eq!(models, [TINY, OTHER]);
        // The snapshot's commit 11, then the new store's first commit defining the models.
        assert_eq!(store.graph().read().last_commit_seq(), 12);
        store.close().unwrap();
        assert!(!dir.join("models").exists());
        assert!(Store::verify_dir(&dir).unwrap().is_ok());
        // Into an existing open store: the models come in as imports.
        let mem = MemDir::new();
        let store = Store::create(&mem, StoreOptions::new()).unwrap();
        store.import_models_from_packed(packed.as_slice()).unwrap();
        let mut models: Vec<String> = store
            .models()
            .unwrap()
            .into_iter()
            .map(|m| m.identity)
            .collect();
        models.sort();
        assert_eq!(models, [TINY, OTHER]);
        store.close().unwrap();
        assert!(mem.entry_kind("models").unwrap().is_none());
    }

    #[test]
    fn an_increment_after_an_upgrade_needs_a_full_backup() {
        let temp = Temp::new("v040-backup");
        let dir = copy(&temp, "store");
        let backup = copy(&temp, "backup");
        // The backup directory upgrades and stays a backup.
        let report = Store::upgrade_in_place(&backup).unwrap();
        assert!(report.backup.is_some());
        assert_eq!(report.models_converted.len(), 2);
        assert!(Store::backup_marker_dir(&backup).unwrap().is_some());
        Store::upgrade_in_place(&dir).unwrap();
        // Each has its own migration commit: the increment is refused, with the advice.
        match Store::backup_dir(&dir, &backup) {
            Err(err @ PersistError::BackupRefused { .. }) => {
                let text = graphersal::error::format_diagnostic(&err);
                assert!(text.contains("take a full backup"), "{text}");
            }
            other => panic!("{other:?}"),
        }
        // A full backup into a new directory works and carries the models.
        let full = temp.join("full");
        Store::backup_dir(&dir, &full).unwrap();
        assert_eq!(Store::models_dir(&full).unwrap().len(), 2);
    }

    #[test]
    fn a_migrated_store_is_newer_for_040() {
        // 0.4.0 refuses a store whose GRAPH has a version word above 3 as NEWER (its
        // `refuse_newer_version`), never as damage; the migration writes 4 before it writes
        // anything else but the INTENT, whose operation 0.4.0 does not know either.
        let temp = Temp::new("v040-newer");
        let dir = copy(&temp, "store");
        assert_eq!(version_of(&fs::read(dir.join("GRAPH")).unwrap()), 3);
        Store::upgrade_in_place(&dir).unwrap();
        for name in ["GRAPH", "GRAPH.copy"] {
            assert_eq!(version_of(&fs::read(dir.join(name)).unwrap()), 4, "{name}");
        }
        assert_eq!(persist_version(), 4);
    }
}

/// 0.4.0 stores whose catalog holds saved queries calling `g.embed_property`, removed in 0.5.0
/// (fixtures `tests/data/v0.4.0/saved_queries/` and `saved_queries.gsnap`, written by
/// `make_saved_queries.sh`): the upgrade report lists them with the vector rule replacing each
/// call, and nothing rewrites them.
#[cfg(feature = "script")]
mod v040_saved_queries {
    use super::*;
    use graphersal::catalog::{DefinitionKind, RemovedFunctionUse};

    fn fixtures() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/v0.4.0")
    }

    fn copy(temp: &Temp, name: &str) -> PathBuf {
        let to = temp.join(name);
        for (relative, bytes) in files_of(&fixtures().join(name)) {
            let path = to.join(&relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }
        to
    }

    const FILL: &str = r#"fn fill() { g.embed_property(#{label: "doc", from: "text", to: "emb", embedder: "hash-ngram/1"}) }"#;

    fn assert_listed(uses: &[RemovedFunctionUse]) {
        let names: Vec<&str> = uses.iter().map(|u| u.query.as_str()).collect();
        assert_eq!(names, ["fill", "fill_any"], "{uses:?}");
        assert!(
            uses.iter()
                .all(|u| u.function == "embed_property" && u.removed_in == "0.5.0")
        );
        assert_eq!(
            uses[0].schema_patch.as_deref(),
            Some(
                r#"g.patch_schema(#{vertices: #{doc: #{schema: #{type: "object", properties: #{emb: #{type: "array", format: "vector", embedder: "hash-ngram/1", from: "text"}}}}}})"#
            )
        );
        // The label is computed: no patch, the replacement in words.
        assert_eq!(uses[1].schema_patch, None);
        assert!(
            uses[1].note("x").contains("vector rule"),
            "{}",
            uses[1].note("x")
        );
    }

    fn stored_fill(store: &Store) -> String {
        store
            .graph()
            .read()
            .traversal()
            .get_definition(DefinitionKind::QUERY, "fill")
            .unwrap()
            .unwrap()
            .as_query()
            .unwrap()
            .source()
            .to_string()
    }

    #[test]
    fn the_upgrade_report_lists_saved_queries_calling_a_removed_function() {
        let temp = Temp::new("v040-sq-copy");
        let source = copy(&temp, "saved_queries");
        let target = temp.join("upgraded");
        let report = Store::upgrade(&source, &target).unwrap();
        assert_listed(&report.saved_queries_using_removed);

        let temp = Temp::new("v040-sq-in-place");
        let dir = copy(&temp, "saved_queries");
        let report = Store::upgrade_in_place(&dir).unwrap();
        assert_listed(&report.saved_queries_using_removed);

        // Never rewritten; a call fails with the help naming the vector rule.
        let store = Store::open(&dir, StoreOptions::new()).unwrap();
        assert_eq!(stored_fill(&store), FILL);
        let err = graphersal::script::eval_value(store.graph().clone(), r#"g.query("fill")"#)
            .expect_err("removed")
            .to_string();
        assert!(err.contains("embed_property was removed in 0.5.0"), "{err}");
        // The listed patch declares the rule, and the rule fills the docs.
        let patch = report.saved_queries_using_removed[0]
            .schema_patch
            .clone()
            .unwrap();
        let _ = graphersal::script::eval_value(store.graph().clone(), &patch).unwrap();
        let filled = store
            .graph()
            .read()
            .traversal()
            .v(None)
            .has_label("doc")
            .has_key("emb")
            .to_list()
            .unwrap()
            .len();
        assert_eq!(filled, 2);
        store.close().unwrap();
    }

    #[test]
    fn a_0_4_0_gsnap_loaded_into_a_store_keeps_them_and_the_scan_lists_them() {
        let temp = Temp::new("v040-sq-gsnap");
        let packed = fs::File::open(fixtures().join("saved_queries.gsnap")).unwrap();
        let store = Store::create_from_packed(
            temp.join("store"),
            std::io::BufReader::new(packed),
            StoreOptions::new(),
        )
        .unwrap();
        assert_eq!(stored_fill(&store), FILL);
        let definitions = store
            .graph()
            .read()
            .traversal()
            .list_definitions(Some(DefinitionKind::QUERY))
            .unwrap();
        assert_listed(&graphersal::script::saved_queries_using_removed(
            &definitions,
        ));
        store.close().unwrap();
    }
}
