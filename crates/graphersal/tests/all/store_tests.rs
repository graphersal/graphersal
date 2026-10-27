//! The Store (feature `persist`, native only; format spec sections 4-6, 9-13): create,
//! reopen equality, the lock, WAL rotation, checkpoints (manual and automatic), point-in-time
//! loads, fork, prune with donor retention, verify, backup, the format guard, the change
//! stream, and crash simulations (cut WAL records at every offset, leftover temporary
//! directories, an interrupted prune, damaged `GRAPH` copies).
#![cfg(all(feature = "persist", not(target_arch = "wasm32")))]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use graphersal::persist::{self, Durability, PersistError, RecoveryTarget, Store, StoreOptions};
use graphersal::prelude::*;
use graphersal::schema::GraphSchema;
use graphersal::storage::PersistentStorage;

use crate::persist_tests::{dump, names, varied_graph};

/// A fresh directory under the system temp dir, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "graphersal-store-{}-{}-{name}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&path);
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

fn graph_dump(store: &Store) -> Vec<String> {
    dump(&store.graph().read())
}

fn graph_names(store: &Store) -> Vec<String> {
    names(&store.graph().read())
}

/// Copies a store directory as a crash would leave it (everything but the lock file).
fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target);
        } else if entry.file_name() != "LOCK" {
            fs::copy(entry.path(), &target).unwrap();
        }
    }
}

fn wal_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir.join("wal"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    files.sort();
    files
}

fn snapshot_dirs(dir: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = fs::read_dir(dir.join("snapshots"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    dirs.sort();
    dirs
}

fn flip(path: &Path, at: u64) {
    let mut bytes = fs::read(path).unwrap();
    bytes[at as usize] ^= 0x10;
    fs::write(path, bytes).unwrap();
}

// ---------------------------------------------------------------------------------------------
// Create, reopen, lock
// ---------------------------------------------------------------------------------------------

#[test]
fn a_store_reopens_with_graph_schema_sequences_position_and_time() {
    let dir = TempDir::new("reopen");
    let store = Store::create_with(dir.path(), varied_graph(), StoreOptions::new()).unwrap();
    assert!(store.open_report().clean_close);
    let schema = GraphSchema::from_json(r#"{"mode": "open", "vertices": {"person": {"schema": {"type": "object", "properties": {"name": {"type": "string"}}}}}}"#).unwrap();
    store
        .graph()
        .write()
        .traversal_mut()
        .set_schema(schema.clone())
        .unwrap();
    add_person(&store, "ann");
    store
        .graph()
        .write()
        .traversal_mut()
        .v("a")
        .property("name", "changed")
        .to_list()
        .unwrap();
    store
        .graph()
        .write()
        .traversal_mut()
        .add_v("person")
        .property("name", "tmp")
        .drop()
        .to_list()
        .unwrap();
    let before = graph_dump(&store);
    let (seq, time, nodes, edges, id) = {
        let g = store.graph().read();
        (
            g.last_commit_seq(),
            g.last_committed_at(),
            g.node_sequence(),
            g.edge_sequence(),
            g.graph_id(),
        )
    };
    store.close().unwrap();

    let store = Store::open(dir.path(), StoreOptions::new()).unwrap();
    let report = store.open_report().clone();
    assert!(report.clean_close, "{report:?}");
    assert_eq!(report.commit_seq, seq);
    assert!(report.commits_replayed >= 3, "{report:?}");
    assert_eq!(graph_dump(&store), before);
    let g = store.graph().read();
    assert_eq!(g.last_commit_seq(), seq);
    assert_eq!(g.last_committed_at(), time);
    assert_eq!((g.node_sequence(), g.edge_sequence()), (nodes, edges));
    assert_eq!(g.graph_id(), id);
    assert_eq!(g.get_schema().unwrap().unwrap().to_json(), schema.to_json());
}

#[test]
fn one_writer_per_directory_and_closed_stores_refuse_commits() {
    let dir = TempDir::new("lock");
    let store = Store::create(dir.path(), StoreOptions::new()).unwrap();
    let second = Store::open(dir.path(), StoreOptions::new());
    assert!(
        matches!(second, Err(PersistError::Locked { .. })),
        "{second:?}"
    );
    let graph = store.graph().clone();
    store.close().unwrap();
    // The graph outlives the store but refuses commits: nothing would be persisted.
    let result = graph.write().traversal_mut().add_v("person").to_list();
    assert!(result.is_err());
    assert_eq!(graph.read().vertex_count(), 0);
    // Closed: the lock is free again, and nothing was committed.
    let store = Store::open(dir.path(), StoreOptions::new()).unwrap();
    assert_eq!(store.graph().read().vertex_count(), 0);
    // Not a store / not empty.
    let empty = TempDir::new("not-a-store");
    fs::create_dir_all(empty.path()).unwrap();
    assert!(matches!(
        Store::open(empty.path(), StoreOptions::new()),
        Err(PersistError::NotAStore { .. })
    ));
    // A new store needs a new or empty directory: its own error and help, not "not a store".
    let err = Store::create(dir.path(), StoreOptions::new()).unwrap_err();
    assert!(
        matches!(err, PersistError::DirectoryNotEmpty { .. }),
        "{err:?}"
    );
    let text = err.to_string();
    assert!(
        text.starts_with("Cannot create a store in ")
            && text.ends_with(": the directory is not empty"),
        "{text}"
    );
    let help = graphersal::error::DiagnosticError::help(&err).unwrap_or_default();
    assert!(
        help.contains("Choose an empty or new directory") && help.contains("graphersal store info"),
        "{help}"
    );
    assert!(
        !help.contains("Create a store with Store::create"),
        "{help}"
    );
}

#[test]
fn a_store_dropped_without_close_reopens_as_unclean() {
    let dir = TempDir::new("unclean");
    let store = Store::create(dir.path(), StoreOptions::new()).unwrap();
    add_person(&store, "ann");
    let crashed = TempDir::new("unclean-copy");
    copy_dir(dir.path(), crashed.path());
    drop(store);
    let store = Store::open(crashed.path(), StoreOptions::new()).unwrap();
    assert!(!store.open_report().clean_close);
    assert_eq!(graph_names(&store), ["ann"]);
}

// ---------------------------------------------------------------------------------------------
// WAL segments, checkpoints
// ---------------------------------------------------------------------------------------------

#[test]
fn wal_segments_rotate_and_all_of_them_replay() {
    let dir = TempDir::new("rotate");
    let options = StoreOptions::new()
        .with_wal_segment_bytes(300)
        .with_durability(Durability::Os);
    let store = Store::create(dir.path(), options.clone()).unwrap();
    for i in 0..30 {
        add_person(&store, &format!("p{i:02}"));
    }
    let segments = wal_files(dir.path());
    assert!(segments.len() > 5, "{segments:?}");
    for path in &segments {
        // A segment closes after the record that crossed the limit: never much larger.
        assert!(fs::metadata(path).unwrap().len() < 300 + 400, "{path:?}");
    }
    let before = graph_dump(&store);
    store.close().unwrap();
    let store = Store::open(dir.path(), options).unwrap();
    assert_eq!(graph_dump(&store), before);
    assert_eq!(store.open_report().commits_replayed, 30);
    assert!(store.verify().unwrap().is_ok());
}

#[test]
fn checkpoints_are_named_listed_and_shorten_the_replay() {
    let dir = TempDir::new("checkpoint");
    let store = Store::create(dir.path(), StoreOptions::new()).unwrap();
    add_person(&store, "ann");
    let first = store.checkpoint(Some("after ann")).unwrap();
    assert_eq!(first.commit_seq, 1);
    assert_eq!(first.name, "after ann");
    // The same position again: the existing snapshot, no new one; another name is refused.
    assert_eq!(store.checkpoint(None).unwrap().commit_seq, 1);
    assert!(store.checkpoint(Some("other")).is_err());
    add_person(&store, "bob");
    assert!(
        store.checkpoint(Some("after ann")).is_err(),
        "names are unique"
    );
    let listed = store.snapshots().unwrap();
    assert_eq!(
        listed.iter().map(|s| s.commit_seq).collect::<Vec<_>>(),
        [0, 1]
    );
    assert_eq!(listed[1].vertex_count, 1);
    // A new WAL segment starts with the first commit after a checkpoint.
    let segments = wal_files(dir.path());
    assert!(
        segments
            .last()
            .unwrap()
            .ends_with("00000000000000000002.wal"),
        "{segments:?}"
    );
    store.close().unwrap();
    let store = Store::open(dir.path(), StoreOptions::new()).unwrap();
    assert_eq!(store.open_report().snapshot_commit_seq, 1);
    assert_eq!(store.open_report().commits_replayed, 1);
    assert_eq!(graph_names(&store), ["ann", "bob"]);
    // The packed export of a stored snapshot loads as that state.
    let mut packed = Vec::new();
    store.export_packed(Some(1), &mut packed).unwrap();
    let loaded = TraversalGraph::read_snapshot(packed.as_slice()).unwrap();
    assert_eq!(names(&loaded), ["ann"]);
    // And a store can start from a packed file.
    let other = TempDir::new("from-packed");
    let imported =
        Store::create_from_packed(other.path(), packed.as_slice(), StoreOptions::new()).unwrap();
    assert_eq!(graph_names(&imported), ["ann"]);
    add_person(&imported, "cy");
    drop(imported);
    let imported = Store::open(other.path(), StoreOptions::new()).unwrap();
    assert_eq!(graph_names(&imported), ["ann", "cy"]);
}

#[test]
fn an_automatic_checkpoint_runs_when_the_wal_grows_past_the_limit() {
    let dir = TempDir::new("auto");
    let store = Store::create(
        dir.path(),
        StoreOptions::new()
            .with_checkpoint_after_bytes(2_000)
            .with_durability(Durability::Os),
    )
    .unwrap();
    for i in 0..40 {
        add_person(&store, &format!("p{i:02}"));
    }
    let deadline = Instant::now() + Duration::from_secs(20);
    while store.snapshots().unwrap().len() < 2 {
        assert!(Instant::now() < deadline, "no automatic checkpoint");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(store.last_auto_checkpoint_error().unwrap().is_none());
    let before = graph_dump(&store);
    store.close().unwrap();
    let store = Store::open(dir.path(), StoreOptions::new()).unwrap();
    assert!(store.open_report().snapshot_commit_seq > 0);
    assert_eq!(graph_dump(&store), before);
}

// ---------------------------------------------------------------------------------------------
// Marks, point-in-time, fork, changes
// ---------------------------------------------------------------------------------------------

#[test]
fn point_in_time_loads_reach_every_target_kind() {
    let dir = TempDir::new("pit");
    let store = Store::create(dir.path(), StoreOptions::new()).unwrap();
    add_person(&store, "ann");
    store.mark("one").unwrap();
    let after_ann = store.graph().read().last_committed_at();
    std::thread::sleep(Duration::from_millis(2));
    store.checkpoint(None).unwrap();
    add_person(&store, "bob");
    store.mark("two").unwrap();
    add_person(&store, "cy");
    // Marks are unique along the store's history, also across a reopen.
    assert!(store.mark("one").is_err());
    store.close().unwrap();

    let at = |target| Store::open_read_only(dir.path(), target).unwrap();
    assert_eq!(
        names(&at(RecoveryTarget::Mark("one".into())).graph),
        ["ann"]
    );
    assert_eq!(
        names(&at(RecoveryTarget::Mark("two".into())).graph),
        ["ann", "bob"]
    );
    assert_eq!(
        names(&at(RecoveryTarget::CommitSeq(0)).graph),
        Vec::<String>::new()
    );
    assert_eq!(
        names(&at(RecoveryTarget::CommitSeq(2)).graph),
        ["ann", "bob"]
    );
    assert_eq!(names(&at(RecoveryTarget::Time(after_ann)).graph), ["ann"]);
    assert_eq!(
        names(&at(RecoveryTarget::Latest).graph),
        ["ann", "bob", "cy"]
    );
    assert!(matches!(
        Store::open_read_only(dir.path(), RecoveryTarget::Mark("none".into())),
        Err(PersistError::TargetNotReached { .. })
    ));
    let store = Store::open(dir.path(), StoreOptions::new()).unwrap();
    assert!(store.mark("two").is_err());
    let marks: Vec<String> = store
        .marks()
        .unwrap()
        .iter()
        .map(|m| m.name().to_string())
        .collect();
    assert_eq!(marks, ["one", "two"]);
}

#[test]
fn a_fork_is_a_new_lineage_with_its_parent_recorded() {
    let dir = TempDir::new("fork-src");
    let store = Store::create(dir.path(), StoreOptions::new()).unwrap();
    add_person(&store, "ann");
    store.mark("m").unwrap();
    add_person(&store, "bob");
    let parent_id = store.graph().read().graph_id();
    let target = TempDir::new("fork-dst");
    let info = store
        .fork(RecoveryTarget::Mark("m".into()), target.path())
        .unwrap();
    // The fork carries no marks; the report names the ones of the forked history.
    let not_carried: Vec<_> = info
        .marks_not_carried
        .iter()
        .map(|m| (m.name().to_string(), m.commit_seq()))
        .collect();
    assert_eq!(not_carried, [("m".to_string(), 1)]);
    assert!(
        info.to_string().contains("mark not carried: m @ commit 1"),
        "{info}"
    );
    assert_ne!(info.snapshot.graph_id, parent_id);
    assert_eq!(info.snapshot.parent_graph_id, parent_id);
    assert_eq!(info.snapshot.parent_commit_seq, 1);
    // The source goes on; the fork starts at the mark and continues its own way.
    add_person(&store, "cy");
    let fork = Store::open(target.path(), StoreOptions::new()).unwrap();
    assert_eq!(graph_names(&fork), ["ann"]);
    add_person(&fork, "dan");
    let info = fork.info().unwrap();
    assert_eq!(info.lineage.len(), 2);
    assert_eq!(info.lineage[1].graph_id, parent_id);
    assert_eq!(info.lineage[0].branched_at, 1);
    assert_eq!(fork.graph().read().last_commit_seq(), 2);
    drop(fork);
    let fork = Store::open(target.path(), StoreOptions::new()).unwrap();
    assert_eq!(graph_names(&fork), ["ann", "dan"]);
    assert_eq!(graph_names(&store), ["ann", "bob", "cy"]);
}

#[test]
fn changes_since_streams_the_committed_change_sets() {
    let dir = TempDir::new("changes");
    let store = Store::create(dir.path(), StoreOptions::new().with_wal_segment_bytes(200)).unwrap();
    for name in ["a", "b", "c", "d"] {
        add_person(&store, name);
    }
    store.checkpoint(None).unwrap();
    add_person(&store, "e");
    let seqs: Vec<u64> = store
        .changes_since(1)
        .unwrap()
        .map(|c| c.unwrap().commit_seq())
        .collect();
    assert_eq!(seqs, [2, 3, 4, 5]);
    assert_eq!(store.changes_since(5).unwrap().count(), 0);
}

// ---------------------------------------------------------------------------------------------
// Prune, verify, backup
// ---------------------------------------------------------------------------------------------

#[test]
fn prune_removes_old_history_but_keeps_two_snapshots_and_the_wal_between() {
    let dir = TempDir::new("prune");
    let store = Store::create(dir.path(), StoreOptions::new()).unwrap();
    for round in 0..4 {
        add_person(&store, &format!("p{round}"));
        store.checkpoint(None).unwrap();
    }
    add_person(&store, "last");
    // Snapshots at 0..=4; pruning up to 4 keeps 3 and 4 (the last two) and the WAL from 3 on.
    let report = store.prune(4).unwrap();
    assert_eq!(report.kept_from, 3);
    assert_eq!(report.snapshots_removed, [0, 1, 2]);
    assert!(!report.segments_removed.is_empty());
    let kept: Vec<u64> = store
        .snapshots()
        .unwrap()
        .iter()
        .map(|s| s.commit_seq)
        .collect();
    assert_eq!(kept, [3, 4]);
    assert!(store.verify().unwrap().is_ok(), "{:?}", store.verify());
    // Targets from the kept snapshot on are reachable, earlier ones are gone.
    assert_eq!(
        names(
            &Store::open_read_only(dir.path(), RecoveryTarget::CommitSeq(3))
                .unwrap()
                .graph
        )
        .len(),
        3
    );
    assert!(Store::open_read_only(dir.path(), RecoveryTarget::CommitSeq(1)).is_err());
    assert!(store.changes_since(0).is_err());
    // Pruning below the oldest snapshot does nothing.
    assert!(store.prune(1).unwrap().snapshots_removed.is_empty());
    let before = graph_dump(&store);
    store.close().unwrap();
    let store = Store::open(dir.path(), StoreOptions::new()).unwrap();
    assert_eq!(graph_dump(&store), before);
}

#[test]
fn prune_reports_the_marks_it_makes_unreachable_and_listings_leave_them_out() {
    let dir = TempDir::new("prune-marks");
    let backup = TempDir::new("prune-marks-backup");
    let store = Store::create(dir.path(), StoreOptions::new()).unwrap();
    for round in 0..4 {
        add_person(&store, &format!("p{round}"));
        store.mark(&format!("m{}", round + 1)).unwrap();
        store.checkpoint(None).unwrap();
        if round == 0 {
            // A full backup holds the latest snapshot (at 1) and reaches every later mark.
            store.backup(backup.path()).unwrap();
        }
    }
    // Marks at 1..=4, snapshots at 0..=4; the backup is brought up to date (an increment).
    store.backup(backup.path()).unwrap();
    let mark_names = |marks: Vec<graphersal::changes::Mark>| -> Vec<String> {
        marks.iter().map(|m| m.name().to_string()).collect()
    };
    // The advice warns before anything is removed.
    let advice = store.compaction_advice().unwrap();
    assert_eq!(advice.prune.marks_unreachable, ["m1", "m2"]);
    // So does the estimate of a prune to any bound (what front ends ask about), and it
    // removes nothing.
    let estimate = Store::prune_estimate_dir(dir.path(), 4).unwrap();
    assert_eq!(estimate.marks_unreachable, ["m1", "m2"]);
    assert_eq!(estimate.kept_from, 3);
    assert_eq!(
        Store::prune_estimate_dir(dir.path(), 2)
            .unwrap()
            .marks_unreachable,
        ["m1"]
    );
    assert!(
        Store::prune_estimate_dir(dir.path(), 0)
            .unwrap()
            .marks_unreachable
            .is_empty()
    );
    assert_eq!(mark_names(store.marks().unwrap()), ["m1", "m2", "m3", "m4"]);

    let report = store.prune(4).unwrap();
    assert_eq!(report.kept_from, 3);
    assert_eq!(report.marks_unreachable, ["m1", "m2"]);
    assert_eq!(mark_names(store.marks().unwrap()), ["m3", "m4"]);
    assert_eq!(mark_names(store.info().unwrap().marks), ["m3", "m4"]);
    // A second prune loses nothing more.
    assert!(store.prune(4).unwrap().marks_unreachable.is_empty());

    // Opening an unreachable mark says why; its name stays taken.
    let err = Store::open_read_only(dir.path(), RecoveryTarget::Mark("m1".into()))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("a prune removed the history it needs"),
        "{err}"
    );
    let err = store.mark("m2").unwrap_err().to_string();
    assert!(err.contains("belongs to the mark at commit 2"), "{err}");
    assert!(Store::open_read_only(dir.path(), RecoveryTarget::Mark("m3".into())).is_ok());

    // An increment of the backup keeps the marks the backup still reaches.
    add_person(&store, "late");
    store.mark("m5").unwrap();
    store.backup(backup.path()).unwrap();
    assert_eq!(
        mark_names(Store::marks_dir(backup.path()).unwrap()),
        ["m1", "m2", "m3", "m4", "m5"]
    );
    let opened = Store::open_backup(backup.path(), StoreOptions::new()).unwrap();
    assert_eq!(
        names(
            &opened
                .read_only_at(RecoveryTarget::Mark("m1".into()))
                .unwrap()
                .graph
        )
        .len(),
        1
    );
    // A prune of the backup reports its own.
    drop(opened);
    let pruned = Store::prune_backup(backup.path(), 4).unwrap();
    assert!(
        pruned.marks_unreachable.contains(&"m1".to_string()),
        "{pruned:?}"
    );
    assert!(
        !Store::marks_dir(backup.path())
            .unwrap()
            .iter()
            .any(|m| m.name() == "m1")
    );

    // A reopen lists the same.
    store.close().unwrap();
    assert!(Store::verify_dir(dir.path()).unwrap().is_ok());
    let store = Store::open(dir.path(), StoreOptions::new()).unwrap();
    assert_eq!(mark_names(store.marks().unwrap()), ["m3", "m4", "m5"]);
}

#[test]
fn verify_finds_flipped_bits_in_snapshots_wal_and_graph_files() {
    let dir = TempDir::new("verify");
    let store = Store::create_with(dir.path(), varied_graph(), StoreOptions::new()).unwrap();
    add_person(&store, "ann");
    store.mark("m").unwrap();
    add_person(&store, "bob");
    store.close().unwrap();
    let report = Store::verify_dir(dir.path()).unwrap();
    assert!(report.is_ok(), "{report:?}");
    assert_eq!(report.last_commit, 2);

    let snapshot = snapshot_dirs(dir.path()).remove(0);
    let wal = wal_files(dir.path()).remove(0);
    let mut targets = vec![
        (dir.join("GRAPH"), 20),
        // The store id (format spec 5.1 offset 100).
        (dir.join("GRAPH"), 105),
        (snapshot.join("schema.json"), 3),
        (snapshot.join("manifest"), 100),
        (snapshot.join("manifest"), 4100),
        (
            snapshot.join("manifest"),
            fs::metadata(snapshot.join("manifest")).unwrap().len() - 100,
        ),
        (wal.clone(), 70),
        (wal.clone(), fs::metadata(&wal).unwrap().len() - 2),
    ];
    for entry in fs::read_dir(&snapshot).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "seg") {
            let len = fs::metadata(&path).unwrap().len();
            targets.push((path, len / 2));
        }
    }
    for (path, at) in targets {
        let copy = TempDir::new("verify-copy");
        copy_dir(dir.path(), copy.path());
        let damaged = copy.path().join(path.strip_prefix(dir.path()).unwrap());
        flip(&damaged, at);
        let report = Store::verify_dir(copy.path()).unwrap();
        assert!(
            !report.is_ok(),
            "flip in {damaged:?} at {at} went unnoticed"
        );
        // Damage is never silently opened over: maintenance mode (read-only); a damaged
        // GRAPH copy is repaired from the other one.
        let opened = Store::open(copy.path(), StoreOptions::new());
        let manifest_copy = damaged.ends_with("manifest")
            && (at < 4096 || at >= fs::metadata(&damaged).unwrap().len() - 4096);
        if damaged.ends_with("GRAPH") || manifest_copy {
            // Redundant copies: the other one is used (and the scrub reports the damage).
            let store = opened.unwrap();
            assert_eq!(
                store.open_report().graph_file_repaired.is_some(),
                damaged.ends_with("GRAPH")
            );
        } else {
            let store = opened.unwrap();
            assert!(
                store.maintenance().is_some(),
                "flip in {damaged:?} at {at} opened normally"
            );
        }
    }
}

#[test]
fn a_backup_restores_to_the_same_lineage_and_state() {
    let dir = TempDir::new("backup");
    let store = Store::create(dir.path(), StoreOptions::new().with_wal_segment_bytes(250)).unwrap();
    add_person(&store, "ann");
    store.checkpoint(None).unwrap();
    for name in ["bob", "cy", "dan"] {
        add_person(&store, name);
    }
    store.mark("m").unwrap();
    let target = TempDir::new("backup-dst");
    // While the store is open and without any graph lock held by the backup.
    let report = store.backup(target.path()).unwrap();
    assert_eq!(report.snapshot_commit_seq, 1);
    assert_eq!(report.commit_seq, 4);
    // Commits after the backup are not in it.
    add_person(&store, "eve");
    // A backup is marked and opens read-only until it is restored (same lineage).
    assert!(matches!(
        Store::open(target.path(), StoreOptions::new()),
        Err(PersistError::IsBackup { commit_seq: 4, .. })
    ));
    Store::restore_backup(target.path()).unwrap();
    let restored = Store::open(target.path(), StoreOptions::new()).unwrap();
    assert!(restored.open_report().clean_close);
    assert_eq!(graph_names(&restored), ["ann", "bob", "cy", "dan"]);
    assert_eq!(
        restored.graph().read().graph_id(),
        store.graph().read().graph_id()
    );
    assert_eq!(restored.marks().unwrap().len(), 1);
    assert!(restored.verify().unwrap().is_ok());
    // A backup by another process (no durable end there) of a closed store.
    store.close().unwrap();
    let offline = TempDir::new("backup-offline");
    Store::backup_dir(dir.path(), offline.path()).unwrap();
    Store::restore_backup(offline.path()).unwrap();
    let restored = Store::open(offline.path(), StoreOptions::new()).unwrap();
    assert_eq!(graph_names(&restored), ["ann", "bob", "cy", "dan", "eve"]);
    // A non-empty target that is not a backup is refused.
    assert!(Store::backup_dir(dir.path(), offline.path()).is_err());
}

#[test]
fn incremental_backups_on_disk_append_tails_and_copy_new_files() {
    let dir = TempDir::new("incr");
    let store = Store::create(dir.path(), StoreOptions::new().with_wal_segment_bytes(300)).unwrap();
    add_person(&store, "ann");
    let target = TempDir::new("incr-dst");
    store.backup(target.path()).unwrap();
    let state = |commit: u64| {
        dump(
            &Store::open_read_only(target.path(), RecoveryTarget::CommitSeq(commit))
                .unwrap()
                .graph,
        )
    };
    for (round, name) in ["bob", "cy", "dan", "eve"].into_iter().enumerate() {
        add_person(&store, name);
        if round == 2 {
            store.checkpoint(None).unwrap();
        }
        let report = store.backup(target.path()).unwrap();
        assert!(report.incremental);
        assert_eq!(report.commit_seq, round as u64 + 2);
        assert_eq!(state(report.commit_seq), graph_dump(&store));
    }
    assert!(Store::verify_dir(target.path()).unwrap().is_ok());
    assert_eq!(Store::snapshots_dir(target.path()).unwrap().len(), 2);
    // Every commit of the backup is a target, also the first one.
    let first = Store::open_read_only(target.path(), RecoveryTarget::CommitSeq(1)).unwrap();
    assert_eq!(names(&first.graph), ["ann"]);
    store.close().unwrap();
    // The backup becomes the live store after the original is lost.
    fs::remove_dir_all(dir.path()).unwrap();
    Store::restore_backup(target.path()).unwrap();
    let live = Store::open(target.path(), StoreOptions::new()).unwrap();
    assert_eq!(graph_names(&live), ["ann", "bob", "cy", "dan", "eve"]);
    add_person(&live, "fay");
    live.close().unwrap();
}

#[test]
fn a_running_backup_blocks_prune() {
    let dir = TempDir::new("pin");
    let store = Store::create(dir.path(), StoreOptions::new()).unwrap();
    add_person(&store, "ann");
    store.checkpoint(None).unwrap();
    add_person(&store, "bob");
    store.checkpoint(None).unwrap();
    // Another process's backup holds the shared pin on BACKUP.
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("BACKUP"))
        .unwrap();
    let lock = fd_lock::RwLock::new(file);
    let pin = lock.read().unwrap();
    let err = store.prune(2).unwrap_err().to_string();
    assert!(err.contains("backup"), "{err}");
    drop(pin);
    assert_eq!(store.prune(2).unwrap().snapshots_removed, [0]);
}

// ---------------------------------------------------------------------------------------------
// Crash simulations and the format guard
// ---------------------------------------------------------------------------------------------

#[test]
fn a_cut_last_wal_record_is_cut_at_every_offset_and_the_store_goes_on() {
    let dir = TempDir::new("cut");
    let store = Store::create(dir.path(), StoreOptions::new()).unwrap();
    add_person(&store, "ann");
    let wal = wal_files(dir.path()).pop().unwrap();
    let complete = fs::metadata(&wal).unwrap().len();
    add_person(&store, "bob");
    let full = fs::metadata(&wal).unwrap().len();
    let crashed = TempDir::new("cut-image");
    copy_dir(dir.path(), crashed.path());
    drop(store);
    let image_wal = crashed.path().join(wal.strip_prefix(dir.path()).unwrap());
    // Every cut is an independent directory: four threads share the offsets (each open
    // syncs the GRAPH file, which dominates the time).
    let check = |cut: u64| {
        let attempt = TempDir::new("cut-attempt");
        copy_dir(crashed.path(), attempt.path());
        let target = attempt
            .path()
            .join(image_wal.strip_prefix(crashed.path()).unwrap());
        fs::OpenOptions::new()
            .write(true)
            .open(&target)
            .unwrap()
            .set_len(cut)
            .unwrap();
        let store = Store::open(attempt.path(), StoreOptions::new())
            .unwrap_or_else(|err| panic!("cut at {cut}: {err}"));
        assert_eq!(graph_names(&store), ["ann"], "cut at {cut}");
        let tail = store.open_report().torn_tail;
        assert_eq!(tail.is_some(), cut > complete, "cut at {cut}");
        assert_eq!(
            fs::metadata(&target).unwrap().len(),
            complete,
            "cut at {cut}"
        );
        // The store continues right after the cut (checked at a few cuts: header, body).
        if ![complete, complete + 3, complete + 24, full - 1].contains(&cut) {
            return;
        }
        add_person(&store, "cy");
        drop(store);
        let store = Store::open(attempt.path(), StoreOptions::new()).unwrap();
        assert_eq!(graph_names(&store), ["ann", "cy"], "cut at {cut}");
        assert!(store.verify().unwrap().is_ok(), "cut at {cut}");
    };
    let cuts: Vec<u64> = (complete..full).collect();
    std::thread::scope(|scope| {
        for part in cuts.chunks(cuts.len().div_ceil(4)) {
            let check = &check;
            scope.spawn(move || part.iter().for_each(|&cut| check(cut)));
        }
    });
    // A damaged length of the last record is damage, not a cut write: maintenance mode.
    let damaged = TempDir::new("cut-damaged");
    copy_dir(crashed.path(), damaged.path());
    flip(
        &damaged
            .path()
            .join(image_wal.strip_prefix(crashed.path()).unwrap()),
        complete + 5,
    );
    let store = Store::open(damaged.path(), StoreOptions::new()).unwrap();
    let report = store.maintenance().expect("maintenance mode");
    assert!(!report.is_recoverable(), "{report}");
    assert_eq!(graph_names(&store), ["ann"]);
}

#[test]
fn leftovers_of_an_interrupted_checkpoint_and_prune_are_cleaned_on_open() {
    let dir = TempDir::new("leftovers");
    let store = Store::create(dir.path(), StoreOptions::new()).unwrap();
    for name in ["a", "b", "c"] {
        add_person(&store, name);
        store.checkpoint(None).unwrap();
    }
    let crashed = TempDir::new("leftovers-image");
    copy_dir(dir.path(), crashed.path());
    drop(store);
    // A checkpoint killed before its rename: a half-written tmp directory, a GRAPH.tmp.
    let tmp = crashed.join("snapshots/tmp-0123456789abcdef0123456789abcdef");
    fs::create_dir_all(&tmp).unwrap();
    fs::write(tmp.join("manifest"), b"half").unwrap();
    fs::write(crashed.join("GRAPH.tmp"), b"partial").unwrap();
    // A prune killed after its INTENT and the first deletion.
    let first = snapshot_dirs(crashed.path()).remove(0);
    let rel = first
        .strip_prefix(crashed.path())
        .unwrap()
        .to_string_lossy()
        // A logical store name ('/'-separated, format spec), also on Windows.
        .replace('\\', "/");
    fs::write(
        crashed.join("INTENT"),
        format!(
            r#"{{"op":"prune","files":["{rel}","wal/00000000000000000099.wal"],"started_at":1}}"#
        ),
    )
    .unwrap();
    // A WAL segment killed while its header was written.
    // A NEW segment (never recorded in GRAPH) killed while its header was written; an
    // existing segment cut like this would be damage (format spec 9.8).
    let partial = crashed.join("wal/00000000000000000005.wal");
    fs::write(&partial, b"GRSLWALS\x01\x00").unwrap();
    let store = Store::open(crashed.path(), StoreOptions::new()).unwrap();
    let report = store.open_report().clone();
    assert_eq!(
        report.torn_tail.map(|t| (t.segment, t.bytes)),
        Some((5, 10))
    );
    assert!(!partial.exists());
    assert_eq!(report.tmp_removed, 2, "{report:?}");
    assert_eq!(report.intent_completed.as_deref(), Some("prune"));
    assert!(!crashed.join("INTENT").exists());
    assert!(!first.exists());
    assert!(!tmp.exists());
    assert_eq!(graph_names(&store), ["a", "b", "c"]);
}

#[test]
fn graph_file_copies_and_the_marks_file_repair_themselves() {
    let dir = TempDir::new("repair");
    let store = Store::create(dir.path(), StoreOptions::new()).unwrap();
    add_person(&store, "ann");
    store.mark("m1").unwrap();
    store.close().unwrap();
    // GRAPH lost: GRAPH.copy is used and GRAPH rewritten; the marks file lost: rebuilt.
    fs::remove_file(dir.join("GRAPH")).unwrap();
    fs::remove_file(dir.join("marks")).unwrap();
    let store = Store::open(dir.path(), StoreOptions::new()).unwrap();
    let report = store.open_report().clone();
    assert!(report.graph_file_repaired.is_some(), "{report:?}");
    assert!(report.marks_rebuilt);
    assert!(dir.join("GRAPH").exists());
    assert_eq!(store.marks().unwrap()[0].name(), "m1");
    assert!(store.mark("m1").is_err());
}

#[test]
fn an_older_or_newer_format_version_is_guarded() {
    let dir = TempDir::new("version");
    let store = Store::create(dir.path(), StoreOptions::new()).unwrap();
    add_person(&store, "ann");
    store.close().unwrap();
    let rewrite = |version: u32| {
        for name in ["GRAPH", "GRAPH.copy"] {
            let path = dir.join(name);
            let mut bytes = fs::read(&path).unwrap();
            bytes[8..12].copy_from_slice(&version.to_le_bytes());
            let crc = crc32c(&bytes[..252]);
            bytes[252..256].copy_from_slice(&crc.to_le_bytes());
            fs::write(&path, bytes).unwrap();
        }
    };
    // An older version is classified by its version word, whatever else the file holds
    // (here even a store id): never damage, never "not a store", also not read-only.
    rewrite(1);
    for err in [
        Store::open(dir.path(), StoreOptions::new())
            .map(|_| ())
            .unwrap_err(),
        Store::open_read_only(dir.path(), RecoveryTarget::Latest)
            .map(|_| ())
            .unwrap_err(),
    ] {
        assert!(
            matches!(
                err,
                PersistError::OlderFormat {
                    found: 1,
                    supported: 3,
                    ..
                }
            ),
            "{err}"
        );
        let help = graphersal::error::format_diagnostic(&err);
        assert!(
            help.contains("store create <new_dir> --from graph.gsnap"),
            "{help}"
        );
    }
    rewrite(4);
    let err = Store::open(dir.path(), StoreOptions::new()).unwrap_err();
    assert!(
        matches!(err, PersistError::UnsupportedVersion { found: 4, .. }),
        "{err}"
    );
}

/// CRC-32C (Castagnoli), bitwise: the test computes a header checksum itself.
pub(crate) fn crc32c(bytes: &[u8]) -> u32 {
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

#[test]
fn persist_recover_reads_store_wal_segments_too() {
    // The WAL segments are journals of the stream API: recovery over streams reads them.
    let dir = TempDir::new("streams");
    let store = Store::create(dir.path(), StoreOptions::new()).unwrap();
    add_person(&store, "ann");
    let mut packed = Vec::new();
    store.export_packed(None, &mut packed).unwrap();
    drop(store);
    let wal = fs::read(wal_files(dir.path()).pop().unwrap()).unwrap();
    let recovered =
        persist::recover(packed.as_slice(), [wal.as_slice()], RecoveryTarget::Latest).unwrap();
    assert_eq!(names(&recovered.graph), ["ann"]);
}

// ---------------------------------------------------------------------------------------------
// In-place rollback and the attic (format spec 12.5)
// ---------------------------------------------------------------------------------------------

use graphersal::persist::RollbackMode;

/// ann (1), mark "m1", bob (2), checkpoint, cy (3), dan (4).
fn history(dir: &Path, options: StoreOptions) -> Store {
    let store = Store::create(dir, options).unwrap();
    add_person(&store, "ann");
    store.mark("m1").unwrap();
    add_person(&store, "bob");
    store.checkpoint(Some("after bob")).unwrap();
    add_person(&store, "cy");
    add_person(&store, "dan");
    store
}

#[test]
fn a_rollback_moves_the_history_to_the_attic_and_a_restore_brings_it_back() {
    let dir = TempDir::new("rollback");
    let store = history(dir.path(), StoreOptions::new());
    let old_id = store.graph().read().graph_id();
    let all = graph_dump(&store);

    let report = store
        .rollback_to(RecoveryTarget::Mark("m1".into()), RollbackMode::Move)
        .unwrap();
    assert_eq!((report.target, report.previous_commit_seq), (1, 4));
    assert_eq!(report.snapshots_moved, [2]);
    assert_ne!(report.graph_id, old_id);
    let id = report.attic.clone().expect("an attic entry");
    assert_eq!(graph_names(&store), ["ann"]);
    {
        let g = store.graph().read();
        assert_eq!(g.graph_id(), report.graph_id);
        assert_eq!(g.last_commit_seq(), 1);
    }
    let info = store.info().unwrap();
    assert_eq!(info.lineage.len(), 2);
    assert_eq!(info.lineage[0].branched_at, 1);
    assert_eq!(info.lineage[1].graph_id, old_id);
    assert_eq!(info.attic.len(), 1);
    assert_eq!(
        store
            .snapshots()
            .unwrap()
            .iter()
            .map(|s| s.commit_seq)
            .collect::<Vec<_>>(),
        [0]
    );
    let entry = &store.attic_list().unwrap()[0];
    assert_eq!((entry.target, entry.last_commit_seq), (1, 4));
    assert_eq!(entry.snapshots, [2]);
    assert_eq!(entry.graph_id, old_id);
    assert!(entry.bytes > 0, "{entry:?}");
    // The check a UI uses before offering "Restore".
    store.attic_can_restore(&id).unwrap();
    assert!(store.attic_can_restore("no-such-entry").is_err());
    let moved: Vec<u64> = store
        .attic_changes(&id)
        .unwrap()
        .map(|c| c.unwrap().commit_seq())
        .collect();
    assert_eq!(moved, [2, 3, 4]);
    assert!(store.verify().unwrap().is_ok(), "{:?}", store.verify());

    // Nothing committed since: the rollback can be undone.
    assert_eq!(store.attic_restore(&id).unwrap(), 4);
    assert_eq!(graph_dump(&store), all);
    assert_eq!(store.graph().read().graph_id(), old_id);
    assert!(store.attic_list().unwrap().is_empty());
    assert_eq!(store.info().unwrap().lineage.len(), 1);
    assert!(store.verify().unwrap().is_ok(), "{:?}", store.verify());
    add_person(&store, "eve");
    drop(store);
    let store = Store::open(dir.path(), StoreOptions::new()).unwrap();
    assert_eq!(graph_names(&store), ["ann", "bob", "cy", "dan", "eve"]);
    assert_eq!(store.marks().unwrap().len(), 1);
}

#[test]
fn forks_and_repairs_list_the_marks_they_do_not_carry() {
    let dir = TempDir::new("marks-not-carried");
    let store = history(dir.path(), StoreOptions::new());
    store.mark("m4").unwrap();
    add_person(&store, "eve");
    // A fork at commit 3: m1 is in its history, m4 (at 4) is not.
    let forked = TempDir::new("marks-not-carried-fork");
    let report = store
        .fork(RecoveryTarget::CommitSeq(3), forked.path())
        .unwrap();
    let names = |marks: &[graphersal::changes::Mark]| -> Vec<(String, u64)> {
        marks
            .iter()
            .map(|m| (m.name().to_string(), m.commit_seq()))
            .collect()
    };
    assert_eq!(names(&report.marks_not_carried), [("m1".to_string(), 1)]);
    let fork = Store::open(forked.path(), StoreOptions::new()).unwrap();
    assert!(fork.marks().unwrap().is_empty());
    drop(fork);
    // A repair carries none either: both marks are listed (text too).
    let repaired_dir = TempDir::new("marks-not-carried-repair");
    let repaired = store.repair_to(repaired_dir.path()).unwrap();
    assert_eq!(
        names(&repaired.marks_not_carried),
        [("m1".to_string(), 1), ("m4".to_string(), 4)]
    );
    assert!(
        repaired
            .to_string()
            .contains("mark not carried: m4 @ commit 4"),
        "{repaired}"
    );
    assert_eq!(repaired.damage.lineage_approximate_before, None);
    // An attic fork: the entry's own WAL holds m4, the store's history before it m1.
    let report = store
        .rollback_to(RecoveryTarget::CommitSeq(2), RollbackMode::Move)
        .unwrap();
    let id = report.attic.unwrap();
    let attic_forked = TempDir::new("marks-not-carried-attic");
    let report = store.attic_fork(&id, attic_forked.path()).unwrap();
    assert_eq!(
        names(&report.marks_not_carried),
        [("m1".to_string(), 1), ("m4".to_string(), 4)]
    );
}

#[test]
fn after_new_commits_an_attic_entry_is_forked_not_restored() {
    let dir = TempDir::new("rollback-fork");
    let store = history(dir.path(), StoreOptions::new());
    let old_id = store.graph().read().graph_id();
    let all = graph_names(&store);
    let report = store
        .rollback_to(RecoveryTarget::CommitSeq(2), RollbackMode::Move)
        .unwrap();
    let id = report.attic.unwrap();
    // The snapshot at the target itself stays.
    assert!(report.snapshots_moved.is_empty());
    store.attic_can_restore(&id).unwrap();
    add_person(&store, "eve");
    let refused = store.attic_can_restore(&id).unwrap_err().to_string();
    assert!(refused.contains("new commits"), "{refused}");
    store.mark("after-eve").unwrap();
    let err = store.attic_restore(&id).unwrap_err().to_string();
    assert!(err.contains("fork"), "{err}");
    // m1 (before the target) is still taken.
    assert!(store.mark("m1").is_err());
    drop(store);

    let store = Store::open(dir.path(), StoreOptions::new()).unwrap();
    assert_eq!(graph_names(&store), ["ann", "bob", "eve"]);
    assert_eq!(store.graph().read().last_commit_seq(), 3);
    assert!(store.verify().unwrap().is_ok(), "{:?}", store.verify());
    // Point-in-time loads cross the lineages.
    let at_one = Store::open_read_only(dir.path(), RecoveryTarget::Mark("m1".into())).unwrap();
    assert_eq!(names(&at_one.graph), ["ann"]);

    let forked = TempDir::new("rollback-fork-dst");
    let info = store.attic_fork(&id, forked.path()).unwrap();
    assert_eq!(info.snapshot.parent_graph_id, old_id);
    assert_eq!(info.snapshot.parent_commit_seq, 4);
    // Not carried: the entry's history has m1; "after-eve" belongs to the new lineage.
    let not_carried: Vec<_> = info.marks_not_carried.iter().map(|m| m.name()).collect();
    assert_eq!(not_carried, ["m1"]);
    let fork = Store::open(forked.path(), StoreOptions::new()).unwrap();
    assert_eq!(graph_names(&fork), all);

    // Prune keeps the base the attic entry needs; removing the entry frees it.
    add_person(&store, "fay");
    store.checkpoint(None).unwrap();
    add_person(&store, "gus");
    store.checkpoint(None).unwrap();
    let pruned = store.prune(5).unwrap();
    assert_eq!(pruned.kept_from, 2, "{pruned:?}");
    assert_eq!(
        pruned.kept_for_attic,
        std::slice::from_ref(&id),
        "{pruned:?}"
    );
    assert!(store.verify().unwrap().is_ok(), "{:?}", store.verify());
    store.attic_remove(&id).unwrap();
    assert!(store.attic_list().unwrap().is_empty());
    let pruned = store.prune(5).unwrap();
    assert_eq!(pruned.kept_from, 4, "{pruned:?}");
    assert!(store.verify().unwrap().is_ok(), "{:?}", store.verify());
}

#[test]
fn a_rollback_in_delete_mode_leaves_no_attic_and_splits_any_segment_layout() {
    // Tiny WAL segments: a segment per record, marks open segments named after the target.
    for (segment_bytes, mode) in [
        (1u64, RollbackMode::Delete),
        (1, RollbackMode::Move),
        (1 << 20, RollbackMode::Move),
    ] {
        let dir = TempDir::new("rollback-layout");
        let options = StoreOptions::new().with_wal_segment_bytes(segment_bytes);
        let store = Store::create(dir.path(), options.clone()).unwrap();
        add_person(&store, "ann");
        add_person(&store, "bob");
        store.mark("at-two").unwrap();
        add_person(&store, "cy");
        store.mark("at-three").unwrap();
        add_person(&store, "dan");
        let report = store
            .rollback_to(RecoveryTarget::CommitSeq(2), mode)
            .unwrap();
        assert_eq!(report.attic.is_some(), mode == RollbackMode::Move);
        assert_eq!(graph_names(&store), ["ann", "bob"]);
        let marks: Vec<String> = store
            .marks()
            .unwrap()
            .iter()
            .map(|m| m.name().to_string())
            .collect();
        assert_eq!(marks, ["at-two"], "{segment_bytes}");
        add_person(&store, "eve");
        store.mark("at-three").unwrap();
        drop(store);
        let store = Store::open(dir.path(), options).unwrap();
        assert_eq!(
            graph_names(&store),
            ["ann", "bob", "eve"],
            "{segment_bytes}"
        );
        assert!(store.verify().unwrap().is_ok(), "{:?}", store.verify());
        let at_two =
            Store::open_read_only(dir.path(), RecoveryTarget::Mark("at-two".into())).unwrap();
        assert_eq!(names(&at_two.graph), ["ann", "bob"]);
        if mode == RollbackMode::Move {
            let id = store.attic_list().unwrap()[0].id.clone();
            let changes: Vec<u64> = store
                .attic_changes(&id)
                .unwrap()
                .map(|c| c.unwrap().commit_seq())
                .collect();
            assert_eq!(changes, [3, 4], "{segment_bytes}");
        }
        // Nothing to roll back after the last commit.
        assert!(
            store
                .rollback_to(RecoveryTarget::CommitSeq(3), RollbackMode::Move)
                .is_err()
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Damage: maintenance mode, the damage report, repair (format spec 13)
// ---------------------------------------------------------------------------------------------

use graphersal::persist::{DamageKind, Donor};

/// A store with two snapshots (A at commit 3, B at commit 6) of a graph spread over many small
/// chunks, then commits 7 (adds "late"), 8 (renames it "later"), 9 (adds "last"); closed.
/// Every checkpoint starts a new WAL segment, so commits 4 and 7 open segments of their own.
fn damaged_candidate(dir: &Path) -> Vec<String> {
    let options = StoreOptions::new().with_chunk_bytes(160);
    let store = Store::create(dir, options).unwrap();
    let unit = |body: &dyn Fn(&mut TraversalGraph) -> Result<(), GraphError>| {
        store.graph().write().transaction(|g| body(g)).unwrap();
    };
    let vertex = |g: &TraversalGraph, i: usize| g.get_vertex_by_id(&format!("v{i:02}")).unwrap();
    unit(&|g| {
        for i in 0..30 {
            let mut element = graphersal::prelude::PropertyMap::default();
            element.insert("name", ElementProperty::from(format!("p{i:02}").as_str()));
            element.insert("n", ElementProperty::from(i as i64));
            g.add_vertex(
                Some(format!("v{i:02}").into()),
                vec!["person".into()],
                element,
            )?;
        }
        Ok(())
    });
    unit(&|g| {
        for i in 0..29 {
            let mut element = graphersal::prelude::PropertyMap::default();
            element.insert("w", ElementProperty::from(1i64));
            let (from, to) = (vertex(g, i), vertex(g, i + 1));
            g.add_edge(
                Some(format!("e{i:02}").into()),
                Some("next".into()),
                from,
                to,
                element,
            )?;
        }
        Ok(())
    });
    unit(&|g| {
        for i in 0..5 {
            let v = vertex(g, i);
            g.set_vertex_property(v, "tag".into(), ElementProperty::from("early"))?;
        }
        Ok(())
    });
    store.checkpoint(Some("A")).unwrap();
    // Commits 4..=6 change elements all over the id space.
    unit(&|g| {
        for i in 0..30 {
            let v = vertex(g, i);
            g.set_vertex_property(v, "n2".into(), ElementProperty::from(2i64))?;
        }
        Ok(())
    });
    unit(&|g| {
        for i in 0..29 {
            let e = g.get_edge_by_id(&format!("e{i:02}")).unwrap();
            g.set_edge_property(e, "w".into(), ElementProperty::from(2i64))?;
        }
        Ok(())
    });
    add_person(&store, "zed");
    store.checkpoint(Some("B")).unwrap();
    add_person(&store, "late");
    unit(&|g| {
        let late = g
            .vertices()
            .find(|v| {
                g.vertex_properties(*v).and_then(|p| p.get("name").cloned())
                    == Some(ElementProperty::from("late"))
            })
            .unwrap();
        g.set_vertex_property(late, "name".into(), ElementProperty::from("later"))
    });
    add_person(&store, "last");
    assert_eq!(store.graph().read().last_commit_seq(), 9);
    let dump = graph_dump(&store);
    store.close().unwrap();
    dump
}

/// The offset of the first commit record in a WAL segment a checkpoint opened: after the
/// 64-byte header and the Checkpoint record (25-byte frame + 16-byte payload).
const FIRST_COMMIT: u64 = 64 + 41;

fn snapshot_of(dir: &Path, commit: u64) -> PathBuf {
    dir.join("snapshots").join(format!("{commit:020}"))
}

fn segment_files(snapshot: &Path, prefix: &str) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(snapshot)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.file_name().unwrap().to_string_lossy().starts_with(prefix))
        .collect();
    files.sort();
    files
}

fn kinds(report: &graphersal::persist::DamageReport) -> Vec<DamageKind> {
    report.damage.iter().map(|d| d.kind).collect()
}

#[test]
fn a_damaged_chunk_opens_in_maintenance_mode_and_repairs_from_the_donor_snapshot() {
    let image = TempDir::new("damage-chunk");
    let original = damaged_candidate(image.path());
    for prefix in ["v-", "e-"] {
        let dir = TempDir::new("damage-chunk-copy");
        copy_dir(image.path(), dir.path());
        let segment = segment_files(&snapshot_of(dir.path(), 6), prefix).remove(0);
        let len = fs::metadata(&segment).unwrap().len();
        flip(&segment, len / 2);
        let damaged_bytes = fs::read(&segment).unwrap();

        let store = Store::open(dir.path(), StoreOptions::new()).unwrap();
        assert!(store.open_report().maintenance);
        let report = store.maintenance().expect("maintenance mode").clone();
        let chunk = report
            .damage
            .iter()
            .find(|d| d.kind == DamageKind::Chunk)
            .unwrap_or_else(|| panic!("{report}"));
        assert_eq!(chunk.snapshot, Some(6));
        assert!(chunk.ids.is_some());
        assert!(
            matches!(chunk.donor, Some(Donor::OlderSnapshot { snapshot: 3, .. })),
            "{report}"
        );
        assert!(report.is_recoverable(), "{report}");
        // The readable state is complete: the donor filled the chunk in.
        assert_eq!(graph_dump(&store), original, "{prefix}");
        // Read-only: commits, marks, checkpoints are refused with a clear error.
        let err = store
            .graph()
            .write()
            .traversal_mut()
            .add_v("x")
            .to_list()
            .unwrap_err();
        let err = graphersal::error::format_diagnostic(&err);
        assert!(err.contains("maintenance mode"), "{err}");
        // Its own help (the damage report, a repaired copy), not the generic commit-hook help.
        assert!(err.contains("store repair"), "{err}");
        assert!(!err.contains("write-ahead"), "{err}");
        assert!(matches!(
            store.mark("m"),
            Err(PersistError::Maintenance { .. })
        ));
        assert!(matches!(
            store.checkpoint(None),
            Err(PersistError::Maintenance { .. })
        ));
        let help = graphersal::error::format_diagnostic(&store.checkpoint(None).unwrap_err());
        assert!(help.contains("repair"), "{help}");

        let target = TempDir::new("damage-chunk-repaired");
        let repaired = store.repair_to(target.path()).unwrap();
        assert!(repaired.is_lossless(), "{repaired}");
        assert_eq!(repaired.commit_seq, 9);
        assert_eq!(repaired.repaired.len(), 1, "{repaired}");
        drop(store);
        // The damaged file was not touched; the repaired store is complete and healthy.
        assert_eq!(fs::read(&segment).unwrap(), damaged_bytes);
        let fresh = Store::open(target.path(), StoreOptions::new()).unwrap();
        assert!(fresh.maintenance().is_none());
        assert_eq!(graph_dump(&fresh), original, "{prefix}");
        assert!(fresh.verify().unwrap().is_ok());
        let lineage = fresh.info().unwrap().lineage;
        assert!(lineage.len() >= 2, "{lineage:?}");
        add_person(&fresh, "after repair");
    }
}

#[test]
fn a_lost_wal_record_is_reported_and_later_writes_to_its_elements_diverge() {
    let image = TempDir::new("damage-wal");
    let original = damaged_candidate(image.path());
    let dir = TempDir::new("damage-wal-copy");
    copy_dir(image.path(), dir.path());
    // Commit 7 follows the Checkpoint record (41 bytes) that opens the segment of checkpoint B.
    let segment = dir.path().join("wal").join(format!("{:020}.wal", 7));
    flip(&segment, FIRST_COMMIT + 25 + 10);
    let store = Store::open(dir.path(), StoreOptions::new()).unwrap();
    let report = store.maintenance().expect("maintenance").clone();
    let record = report
        .damage
        .iter()
        .find(|d| d.kind == DamageKind::WalRecord)
        .unwrap_or_else(|| panic!("{report}"));
    assert_eq!(record.commits, Some((7, 7)), "{report}");
    assert!(record.donor.is_none());
    assert!(!report.is_recoverable());
    // Commits 8 and 9 were replayed after the gap: "late" never came, so 8 diverged.
    let names = graph_names(&store);
    assert!(names.contains(&"last".to_string()), "{names:?}");
    assert!(!names.contains(&"later".to_string()), "{names:?}");
    let target = TempDir::new("damage-wal-repaired");
    let repaired = store.repair_to(target.path()).unwrap();
    assert!(!repaired.is_lossless());
    assert_eq!(repaired.lost_commits, [(7, 7)]);
    assert_eq!(repaired.diverged.len(), 1, "{repaired}");
    assert_eq!(repaired.diverged[0].commit_seq, 8);
    let text = repaired.to_string();
    assert!(text.contains("lost      commit 7"), "{text}");
    assert!(text.contains("diverged"), "{text}");
    let fresh = Store::open(target.path(), StoreOptions::new()).unwrap();
    assert_eq!(graph_names(&fresh), names);
    assert_ne!(graph_dump(&fresh), original);
}

#[test]
fn a_damaged_wal_record_that_a_later_snapshot_covers_loses_nothing() {
    let image = TempDir::new("damage-covered");
    let original = damaged_candidate(image.path());
    let dir = TempDir::new("damage-covered-copy");
    copy_dir(image.path(), dir.path());
    // Commit 4 opens the segment after checkpoint A; snapshot B (commit 6) covers it.
    let segment = dir.path().join("wal").join(format!("{:020}.wal", 4));
    flip(&segment, FIRST_COMMIT + 25 + 10);
    // The latest snapshot and the WAL after it are intact: a normal open, verify finds it.
    let store = Store::open(dir.path(), StoreOptions::new()).unwrap();
    assert!(store.maintenance().is_none());
    assert!(!store.verify().unwrap().is_ok());
    drop(store);
    let report = Store::damage_report_dir(dir.path()).unwrap();
    let record = report
        .damage
        .iter()
        .find(|d| d.kind == DamageKind::WalRecord)
        .unwrap_or_else(|| panic!("{report}"));
    assert!(
        matches!(record.donor, Some(Donor::LaterSnapshot { snapshot: 6, .. })),
        "{report}"
    );
    let target = TempDir::new("damage-covered-repaired");
    let repaired = Store::repair_dir(dir.path(), target.path()).unwrap();
    assert!(repaired.is_lossless(), "{repaired}");
    let fresh = Store::open(target.path(), StoreOptions::new()).unwrap();
    assert_eq!(graph_dump(&fresh), original);
}

#[test]
fn bit_flips_in_every_region_give_the_right_damage_report() {
    let image = TempDir::new("damage-regions");
    let original = damaged_candidate(image.path());
    let b = snapshot_of(image.path(), 6);
    let manifest_len = fs::metadata(b.join("manifest")).unwrap().len();
    let vertex_segment = segment_files(&b, "v-").remove(0);
    let vertex_len = fs::metadata(&vertex_segment).unwrap().len();
    let wal7 = image.path().join("wal").join(format!("{:020}.wal", 7));
    let rel = |p: &Path| p.strip_prefix(image.path()).unwrap().to_path_buf();
    // (file, offset, expected kind, opens in maintenance mode, a repair loses nothing)
    let cases: Vec<(PathBuf, u64, DamageKind, bool, bool)> = vec![
        // The manifest's variable part: the snapshot is unusable; rebuilt from A + WAL.
        (
            rel(&b.join("manifest")),
            4096 + 20,
            DamageKind::Manifest,
            true,
            true,
        ),
        // One copy of the fixed part: redundant, a normal open (warning), verify reports it.
        (
            rel(&b.join("manifest")),
            100,
            DamageKind::ManifestCopy,
            false,
            true,
        ),
        (
            rel(&b.join("manifest")),
            manifest_len - 100,
            DamageKind::ManifestCopy,
            false,
            true,
        ),
        (
            rel(&b.join("schema.json")),
            3,
            DamageKind::Schema,
            true,
            true,
        ),
        // A segment header or footer: every chunk is intact (framed by the chunk index).
        (rel(&vertex_segment), 20, DamageKind::Segment, true, true),
        (
            rel(&vertex_segment),
            vertex_len - 2,
            DamageKind::Segment,
            true,
            true,
        ),
        (
            rel(&vertex_segment),
            vertex_len / 2,
            DamageKind::Chunk,
            true,
            true,
        ),
        // A WAL segment header: its records are read without it.
        (rel(&wal7), 20, DamageKind::WalHeader, true, true),
        // The Checkpoint record that opens it: no commit lost.
        (rel(&wal7), 64 + 2, DamageKind::WalRecord, true, true),
        // The header of commit 7's record: lost (no later snapshot).
        (
            rel(&wal7),
            FIRST_COMMIT + 2,
            DamageKind::WalRecord,
            true,
            false,
        ),
    ];
    for (file, at, kind, maintenance, recoverable) in cases {
        let dir = TempDir::new("damage-region");
        copy_dir(image.path(), dir.path());
        flip(&dir.path().join(&file), at);
        let context = format!("{} at {at}", file.display());
        let report = Store::damage_report_dir(dir.path()).unwrap();
        assert!(kinds(&report).contains(&kind), "{context}: {report}");
        assert_eq!(report.is_recoverable(), recoverable, "{context}: {report}");
        let store = Store::open(dir.path(), StoreOptions::new())
            .unwrap_or_else(|err| panic!("{context}: {err}"));
        assert_eq!(store.maintenance().is_some(), maintenance, "{context}");
        if !maintenance {
            assert!(!store.open_report().warnings.is_empty(), "{context}");
        }
        assert!(!store.verify().unwrap().is_ok(), "{context}");
        drop(store);
        if recoverable {
            let target = TempDir::new("damage-region-repaired");
            let repaired = Store::repair_dir(dir.path(), target.path())
                .unwrap_or_else(|err| panic!("{context}: {err}"));
            assert!(repaired.is_lossless(), "{context}: {repaired}");
            let fresh = Store::open(target.path(), StoreOptions::new()).unwrap();
            assert_eq!(graph_dump(&fresh), original, "{context}");
        }
    }
    // Both GRAPH copies: the identity comes from the snapshots and WAL headers.
    let dir = TempDir::new("damage-graph");
    copy_dir(image.path(), dir.path());
    flip(&dir.join("GRAPH"), 20);
    flip(&dir.join("GRAPH.copy"), 20);
    let store = Store::open(dir.path(), StoreOptions::new()).unwrap();
    let report = store.maintenance().expect("maintenance").clone();
    assert!(kinds(&report).contains(&DamageKind::GraphFile), "{report}");
    // The rebuilt lineage is approximate before the base snapshot, and the report says so.
    let base = report.base_snapshot.expect("a base snapshot");
    assert_eq!(report.lineage_approximate_before, Some(base), "{report}");
    let note = format!("lineage before commit {base} is approximate (both GRAPH copies damaged)");
    assert!(report.to_string().contains(&note), "{report}");
    assert_eq!(graph_dump(&store), original);
    let target = TempDir::new("damage-graph-repaired");
    let repaired = store.repair_to(target.path()).unwrap();
    assert!(repaired.is_lossless(), "{repaired}");
    assert_eq!(repaired.damage.lineage_approximate_before, Some(base));
    let fresh = Store::open(target.path(), StoreOptions::new()).unwrap();
    assert_eq!(graph_dump(&fresh), original);
}

/// Every file of a store, relative to it (the lock files left out).
fn store_files(dir: &Path) -> Vec<PathBuf> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(root, &path, out);
            } else if !["LOCK", "BACKUP"].contains(&path.file_name().unwrap().to_str().unwrap()) {
                out.push(path.strip_prefix(root).unwrap().to_path_buf());
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

#[test]
fn random_bit_flips_anywhere_never_panic_and_never_open_silently_wrong() {
    let image = TempDir::new("damage-fuzz");
    let original = damaged_candidate(image.path());
    let files = store_files(image.path());
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for round in 0..24 {
        let file = &files[(next() % files.len() as u64) as usize];
        let dir = TempDir::new("damage-fuzz-copy");
        copy_dir(image.path(), dir.path());
        let path = dir.path().join(file);
        let len = fs::metadata(&path).unwrap().len();
        if len == 0 {
            continue;
        }
        let at = next() % len;
        let mut bytes = fs::read(&path).unwrap();
        bytes[at as usize] ^= 1 << (next() % 8);
        fs::write(&path, bytes).unwrap();
        let context = format!("round {round}: {} at {at}", file.display());
        match Store::open(dir.path(), StoreOptions::new()) {
            Ok(store) => {
                if store.maintenance().is_none() {
                    // A normal open is either exact or a redundant copy / old history was hit.
                    let dump = graph_dump(&store);
                    assert_eq!(dump, original, "{context}");
                }
                drop(store);
                let target = TempDir::new("damage-fuzz-repaired");
                if let Ok(report) = Store::repair_dir(dir.path(), target.path())
                    && report.is_lossless()
                {
                    let fresh = Store::open(target.path(), StoreOptions::new()).unwrap();
                    assert_eq!(graph_dump(&fresh), original, "{context}: {report}");
                }
            }
            Err(err) => {
                // Only refusals that are not damage (none expected here) may fail the open.
                panic!("{context}: the open failed: {err}");
            }
        }
    }
}

/// A store whose `wal/`, `snapshots/` and `attic/` are symbolic links to directories elsewhere
/// (parts on other disks) works like one directory: commits, checkpoints, rollback into the
/// attic and restore (moves between the parts), prune, reopen, verify.
#[cfg(unix)]
#[test]
fn a_store_split_over_linked_directories_works_like_one() {
    let dir = TempDir::new("split");
    let wal = TempDir::new("split-wal");
    let snapshots = TempDir::new("split-snapshots");
    let attic = TempDir::new("split-attic");
    let store = Store::create(dir.path(), StoreOptions::new()).unwrap();
    add_person(&store, "ann");
    store.close().unwrap();
    for (name, place) in [("wal", &wal), ("snapshots", &snapshots)] {
        fs::rename(dir.join(name), place.path()).unwrap();
        std::os::unix::fs::symlink(place.path(), dir.join(name)).unwrap();
    }
    fs::create_dir_all(attic.path()).unwrap();
    std::os::unix::fs::symlink(attic.path(), dir.join("attic")).unwrap();

    let store = Store::open(dir.path(), StoreOptions::new()).unwrap();
    assert_eq!(graph_names(&store), ["ann"]);
    store.checkpoint(None).unwrap();
    add_person(&store, "bob");
    store.checkpoint(None).unwrap();
    add_person(&store, "cy");
    let report = store
        .rollback_to(RecoveryTarget::CommitSeq(1), persist::RollbackMode::Move)
        .unwrap();
    assert_eq!(graph_names(&store), ["ann"]);
    let id = report.attic.unwrap();
    // The moved history lives in the linked attic directory.
    assert!(attic.path().join(&id).is_dir());
    assert_eq!(store.attic_restore(&id).unwrap(), 3);
    assert_eq!(graph_names(&store), ["ann", "bob", "cy"]);
    assert!(store.verify().unwrap().is_ok());
    add_person(&store, "dan");
    store.checkpoint(None).unwrap();
    let pruned = store.prune(u64::MAX).unwrap();
    assert!(!pruned.snapshots_removed.is_empty(), "{pruned:?}");
    store.close().unwrap();
    // The links stay links; the files are in the linked directories.
    for name in ["wal", "snapshots", "attic"] {
        assert!(
            fs::symlink_metadata(dir.join(name))
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }
    assert!(fs::read_dir(wal.path()).unwrap().count() > 0);
    let store = Store::open(dir.path(), StoreOptions::new()).unwrap();
    assert_eq!(graph_names(&store), ["ann", "bob", "cy", "dan"]);
    assert!(store.verify().unwrap().is_ok());
}

/// A store holds no paths: the whole directory moved elsewhere opens as the same graph.
#[test]
fn a_moved_store_directory_opens_as_the_same_graph() {
    let dir = TempDir::new("move-from");
    let store = Store::create(dir.path(), StoreOptions::new()).unwrap();
    add_person(&store, "ann");
    store.mark("m1").unwrap();
    store.checkpoint(None).unwrap();
    add_person(&store, "bob");
    store
        .rollback_to(
            RecoveryTarget::Mark("m1".into()),
            persist::RollbackMode::Move,
        )
        .unwrap();
    add_person(&store, "cy");
    let expected = graph_dump(&store);
    store.close().unwrap();
    let moved = TempDir::new("move-to");
    fs::rename(dir.path(), moved.path()).unwrap();
    let store = Store::open(moved.path(), StoreOptions::new()).unwrap();
    assert_eq!(graph_dump(&store), expected);
    assert!(store.verify().unwrap().is_ok());
    assert_eq!(store.attic_list().unwrap().len(), 1);
    assert!(Store::open_read_only(moved.path(), RecoveryTarget::Mark("m1".into())).is_ok());
}

/// A rollback to a commit before the current lineage's branch point splits a WAL segment of
/// an ANCESTOR lineage: the attic tail keeps that lineage (format spec 11.1), so the entry's
/// changes, its restore and the reopened store all hold their histories (found by the merge
/// checkpoint's randomized test: the tail used to get the current lineage's header, and the
/// restore then failed with "beyond the branch point").
#[test]
fn a_rollback_before_the_lineages_branch_point_restores_from_the_attic() {
    let dir = persist::MemDir::new();
    let options = StoreOptions::new()
        .with_checkpoint_after_bytes(0)
        .with_wal_segment_bytes(300);
    let store = Store::create(&dir, options.clone()).unwrap();
    for i in 0..30 {
        add_person(&store, &format!("a{i}"));
    }
    store.checkpoint(None).unwrap();
    store
        .rollback_to(RecoveryTarget::CommitSeq(25), persist::RollbackMode::Move)
        .unwrap();
    for i in 0..5 {
        add_person(&store, &format!("b{i}"));
    }
    let before = graph_dump(&store);
    let rolled = store
        .rollback_to(RecoveryTarget::CommitSeq(8), persist::RollbackMode::Move)
        .unwrap();
    assert_eq!(store.graph().read().last_commit_seq(), 8);
    let attic = rolled.attic.unwrap();
    assert_eq!(
        store
            .attic_changes(&attic)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .len(),
        22,
        "commits 9..=25 of the first lineage and 26..=30 of the second"
    );
    store.attic_restore(&attic).unwrap();
    assert_eq!(graph_dump(&store), before);
    assert!(store.verify().unwrap().is_ok());
    drop(store);
    let reopened = Store::open(&dir, options).unwrap();
    assert_eq!(graph_dump(&reopened), before);
}

/// Two rollbacks to the same commit within one second get two attic entries (the name's time
/// part has a resolution of one second, format spec 11.2): each one keeps its own history, and
/// restoring the newer brings back exactly what it moved (found by the merge checkpoint's
/// randomized test: both used to share one directory, and the restore mixed the histories).
#[test]
fn two_rollbacks_to_the_same_commit_get_their_own_attic_entries() {
    let dir = persist::MemDir::new();
    let options = StoreOptions::new()
        .with_checkpoint_after_bytes(0)
        .with_wal_segment_bytes(300);
    let store = Store::create(&dir, options.clone()).unwrap();
    for i in 0..20 {
        add_person(&store, &format!("a{i}"));
    }
    let first = store
        .rollback_to(RecoveryTarget::CommitSeq(4), persist::RollbackMode::Move)
        .unwrap()
        .attic
        .unwrap();
    add_person(&store, "b0");
    add_person(&store, "b1");
    let before = graph_dump(&store);
    let second = store
        .rollback_to(RecoveryTarget::CommitSeq(4), persist::RollbackMode::Move)
        .unwrap()
        .attic
        .unwrap();
    assert_ne!(first, second);
    assert_eq!(store.attic_list().unwrap().len(), 2);
    assert_eq!(store.attic_changes(&first).unwrap().count(), 16);
    assert_eq!(store.attic_changes(&second).unwrap().count(), 2);
    assert_eq!(store.attic_restore(&second).unwrap(), 6);
    assert_eq!(graph_dump(&store), before);
    assert!(store.verify().unwrap().is_ok());
    drop(store);
    assert_eq!(graph_dump(&Store::open(&dir, options).unwrap()), before);
}

/// A store whose every checkpoint is encoded from the graph (`checkpoint_merge_bytes(0)`, what
/// the automatic checkpoint falls back to when the WAL is too large to merge), with one
/// committed vertex `kept`.
fn store_with_kept_vertex(dir: &TempDir) -> (Store, StoreOptions) {
    let options = StoreOptions::new()
        .with_checkpoint_after_bytes(0)
        .with_checkpoint_merge_bytes(0);
    let store = Store::create(dir.path(), options.clone()).unwrap();
    GraphTraversalSource::new(&mut *store.graph().write())
        .add_v("person")
        .property("name", "kept")
        .next()
        .unwrap();
    (store, options)
}

/// A run that keeps ONE unit over several locks, as the playground session does
/// (`Graph::begin_unit`, one lock per traversal, then `Graph::rollback_unit` on the same
/// thread), and a checkpoint of another thread (the Store's checkpoint thread) in that window.
/// The unit wrote, so the checkpoint waits until it closes; the run fails and rolls back, and
/// nothing it wrote survives a reopen.
#[test]
fn a_checkpoint_while_a_unit_is_open_keeps_no_uncommitted_data() {
    let dir = TempDir::new("open-unit-checkpoint");
    let (store, options) = store_with_kept_vertex(&dir);
    let graph = store.graph().clone();

    // The run: its unit stays open while it takes one lock per traversal.
    let mark = graph.begin_unit().unwrap();
    GraphTraversalSource::new(&mut *graph.write())
        .add_v("person")
        .property("name", "uncommitted")
        .next()
        .unwrap();

    let checkpointed = std::thread::scope(|s| {
        let checkpoint = s.spawn(|| store.checkpoint(None));
        // A check, not a synchronization: the checkpoint locks the graph, which the unit holds
        // exclusively since its write, so it cannot have finished.
        std::thread::sleep(Duration::from_millis(200));
        assert!(
            !checkpoint.is_finished(),
            "the checkpoint waits for the unit"
        );
        // The run fails: its unit is rolled back.
        graph
            .rollback_unit(
                mark,
                &graphersal::changes::RollbackReason::failed(&"the script failed"),
            )
            .unwrap();
        checkpoint.join().unwrap()
    });
    assert!(checkpointed.is_ok(), "{checkpointed:?}");
    assert_eq!(
        names(&graph.read()),
        ["kept"],
        "live graph after the rollback"
    );
    drop(graph);
    store.close().unwrap();

    let reopened = Store::open(dir.path(), options).unwrap();
    assert_eq!(
        names(&reopened.graph().read()),
        ["kept"],
        "reopened store (the checkpoint written after the unit: {checkpointed:?})"
    );
}

/// The mirror case: the run COMMITS after a checkpoint of another thread was asked for. The
/// checkpoint waits for the unit and then writes the committed state; the reopened store holds
/// exactly the committed data and opens without failure.
#[test]
fn a_checkpoint_while_a_unit_is_open_keeps_the_committed_data() {
    let dir = TempDir::new("open-unit-checkpoint-commit");
    let (store, options) = store_with_kept_vertex(&dir);
    let graph = store.graph().clone();

    let mark = graph.begin_unit().unwrap();
    GraphTraversalSource::new(&mut *graph.write())
        .add_v("person")
        .property("name", "committed")
        .next()
        .unwrap();
    let checkpointed = std::thread::scope(|s| {
        let checkpoint = s.spawn(|| store.checkpoint(None));
        std::thread::sleep(Duration::from_millis(200));
        assert!(
            !checkpoint.is_finished(),
            "the checkpoint waits for the unit"
        );
        graph.commit_unit(mark).unwrap();
        checkpoint.join().unwrap()
    });
    assert!(checkpointed.is_ok(), "{checkpointed:?}");
    let mut live = names(&graph.read());
    live.sort();
    assert_eq!(live, ["committed", "kept"], "live graph after the commit");
    drop(graph);
    store.close().unwrap();

    let reopened = Store::open(dir.path(), options).unwrap();
    let mut stored = names(&reopened.graph().read());
    stored.sort();
    assert_eq!(
        stored,
        ["committed", "kept"],
        "reopened store: {checkpointed:?}"
    );
    assert!(reopened.verify().unwrap().is_ok());
}
