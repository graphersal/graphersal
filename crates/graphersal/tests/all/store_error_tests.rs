//! `PersistError::Store` carries a structured `cause` (`StoreFailure`) and a `help()` per cause:
//! every cause a caller can reach through the public API is triggered here (on a `MemDir`) and
//! its cause and help checked. The causes that need a fault inside the process (a poisoned
//! lock, a failing journal sync, a file changed by someone else during the operation, a copy
//! that does not read back) have their help texts checked in `persist/error.rs`'s tests.
#![cfg(feature = "persist")]

use graphersal::error::DiagnosticError;
use graphersal::persist::{
    LockKind, MemDir, PersistError, RecoveryTarget, RollbackMode, Store, StoreDir, StoreFailure,
    StoreOptions,
};

use crate::persist_tests::varied_graph;

/// Asserts `result` failed with `PersistError::Store` of `cause`, whose help mentions `help`.
#[track_caller]
fn assert_cause<T: std::fmt::Debug>(
    result: Result<T, PersistError>,
    cause: StoreFailure,
    help: &str,
) {
    let err = result.expect_err("the operation must be refused");
    match &err {
        PersistError::Store { cause: found, .. } => assert_eq!(*found, cause, "{err}"),
        other => panic!("expected PersistError::Store, got {other:?}"),
    }
    let text = err.help().expect("a store failure has help");
    assert!(text.contains(help), "help of {cause:?}: {text}");
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

fn store_with_commits(dir: &MemDir, commits: usize) -> Store {
    let store = Store::create_with(dir, varied_graph(), StoreOptions::new()).unwrap();
    for i in 0..commits {
        add_person(&store, &format!("p{i}"));
    }
    store
}

fn commit_seq(store: &Store) -> u64 {
    store.graph().read().last_commit_seq()
}

#[test]
fn a_closed_store_refuses_operations() {
    let dir = MemDir::new();
    let store = store_with_commits(&dir, 1);
    store.shutdown().unwrap();
    assert_cause(
        store.checkpoint(None),
        StoreFailure::Closed,
        "Open it again",
    );
    assert_cause(store.mark("m"), StoreFailure::Closed, "Store::open");
}

#[test]
fn snapshot_and_mark_names_must_be_unique() {
    let dir = MemDir::new();
    let store = store_with_commits(&dir, 1);
    store.checkpoint(Some("weekly")).unwrap();
    add_person(&store, "x");
    assert_cause(
        store.checkpoint(Some("weekly")),
        StoreFailure::NameTaken,
        "Choose another name",
    );
    // A mark whose history a prune removed keeps its name.
    store.mark("old").unwrap();
    add_person(&store, "y");
    store.checkpoint(None).unwrap();
    add_person(&store, "z");
    store.checkpoint(None).unwrap();
    store.prune(commit_seq(&store)).unwrap();
    assert_cause(
        store.mark("old"),
        StoreFailure::NameTaken,
        "graphersal store marks",
    );
}

#[test]
fn a_snapshot_name_has_at_most_255_bytes() {
    let dir = MemDir::new();
    let store = store_with_commits(&dir, 1);
    let long = "x".repeat(256);
    assert_cause(
        store.checkpoint(Some(&long)),
        StoreFailure::NameTooLong,
        "at most 255 bytes",
    );
}

#[test]
fn a_second_name_for_the_same_commit_is_refused() {
    let dir = MemDir::new();
    let store = store_with_commits(&dir, 1);
    store.checkpoint(Some("a")).unwrap();
    assert_cause(
        store.checkpoint(Some("b")),
        StoreFailure::SnapshotExists,
        "Commit something first",
    );
    // Without a name (or with the same one) the existing snapshot comes back.
    assert_eq!(store.checkpoint(None).unwrap().name, "a");
}

#[test]
fn an_export_of_a_commit_without_snapshot_is_refused() {
    let dir = MemDir::new();
    let store = store_with_commits(&dir, 2);
    assert_cause(
        store.export_packed(Some(1), &mut Vec::new()),
        StoreFailure::NoSnapshot,
        "graphersal store list",
    );
}

#[test]
fn a_prune_is_refused_while_a_backup_runs() {
    let dir = MemDir::new();
    let store = store_with_commits(&dir, 2);
    {
        let _backup = dir.lock(LockKind::BackupShared).unwrap().unwrap();
        assert_cause(
            store.prune(commit_seq(&store)),
            StoreFailure::BackupRunning,
            "Prune again when the backup has finished",
        );
    }
    // (The other direction, `PruneRunning`, needs a `StoreDir` whose shared pin refuses instead
    // of waiting, as the built-in backends do: its help is checked in `persist/error.rs`.)
    store.prune(commit_seq(&store)).unwrap();
}

#[test]
fn a_rollback_needs_a_target_before_the_current_commit() {
    let dir = MemDir::new();
    let store = store_with_commits(&dir, 2);
    let at = commit_seq(&store);
    assert_cause(
        store.rollback_to(RecoveryTarget::CommitSeq(at), RollbackMode::Move),
        StoreFailure::NothingAfterTarget,
        "choose a target before the current commit",
    );
}

#[test]
fn an_unknown_attic_entry_is_named() {
    let dir = MemDir::new();
    let store = store_with_commits(&dir, 2);
    assert_cause(
        store.attic_restore("nope"),
        StoreFailure::UnknownAtticEntry,
        "graphersal store attic <dir>",
    );
    assert_cause(
        store.attic_remove("../x"),
        StoreFailure::UnknownAtticEntry,
        "Store::attic_list",
    );
}

#[test]
fn attic_entries_restore_newest_first() {
    let dir = MemDir::new();
    let store = store_with_commits(&dir, 3);
    store
        .rollback_to(RecoveryTarget::CommitSeq(2), RollbackMode::Move)
        .unwrap();
    store
        .rollback_to(RecoveryTarget::CommitSeq(1), RollbackMode::Move)
        .unwrap();
    let entries = store.attic_list().unwrap();
    assert_eq!(entries.len(), 2);
    let older = entries
        .iter()
        .find(|e| e.target == 2)
        .expect("the first rollback's entry");
    assert_cause(
        store.attic_restore(&older.id),
        StoreFailure::AtticLineageChanged,
        "restore the newer entry first",
    );
}

#[test]
fn an_attic_entry_is_not_restored_over_new_commits() {
    let dir = MemDir::new();
    let store = store_with_commits(&dir, 2);
    store
        .rollback_to(RecoveryTarget::CommitSeq(1), RollbackMode::Move)
        .unwrap();
    add_person(&store, "after");
    let id = store.attic_list().unwrap()[0].id.clone();
    assert_cause(
        store.attic_restore(&id),
        StoreFailure::ChangedSinceRollback,
        "Fork the entry into a new store",
    );
}

#[test]
fn an_attic_entry_whose_base_snapshot_is_gone_cannot_be_read() {
    let dir = MemDir::new();
    let store = store_with_commits(&dir, 2);
    store
        .rollback_to(RecoveryTarget::CommitSeq(1), RollbackMode::Move)
        .unwrap();
    let id = store.attic_list().unwrap()[0].id.clone();
    // Someone removes the base snapshot's manifest by hand.
    let manifest = dir
        .file_names()
        .into_iter()
        .find(|n| n.starts_with("snapshots/") && n.ends_with("/manifest"))
        .expect("the base snapshot");
    dir.remove_file(&manifest).unwrap();
    assert_cause(
        store.attic_fork(&id, MemDir::new()),
        StoreFailure::AtticBaseMissing,
        "graphersal store verify",
    );
}

#[test]
fn a_store_is_not_pruned_as_a_backup() {
    let dir = MemDir::new();
    store_with_commits(&dir, 1).close().unwrap();
    assert_cause(
        Store::prune_backup(&dir, 0),
        StoreFailure::NotABackup,
        "a live store",
    );
}

#[test]
fn a_prune_keeps_everything_when_a_kept_snapshot_is_damaged() {
    let dir = MemDir::new();
    let store = store_with_commits(&dir, 1);
    add_person(&store, "x");
    let snapshot = store.checkpoint(None).unwrap();
    let prefix = format!("snapshots/{:020}/", snapshot.commit_seq);
    let name = dir
        .file_names()
        .into_iter()
        .filter(|n| n.starts_with(&prefix) && !n.ends_with("/manifest"))
        .max_by_key(|n| dir.file(n).map_or(0, |b| b.len()))
        .expect("a segment of the newest snapshot");
    let mut bytes = dir.file(&name).unwrap();
    let middle = bytes.len() / 2;
    bytes[middle] ^= 0x40;
    dir.put_file(&name, bytes);
    let before = dir.file_names();
    assert_cause(
        store.prune(snapshot.commit_seq),
        StoreFailure::KeptSnapshotDamaged,
        "graphersal store repair",
    );
    assert_eq!(dir.file_names(), before, "nothing was removed");
}

#[test]
fn every_cause_has_a_stable_name() {
    assert_eq!(StoreFailure::NameTaken.name(), "name_taken");
    assert_eq!(StoreFailure::BackupRunning.to_string(), "backup_running");
}

/// A store made older: `GRAPH` and its copy rewritten with store format version 2 (the
/// version of Graphersal 0.1.0; its layout is the current one).
#[cfg(not(target_arch = "wasm32"))]
fn make_version_2(dir: &MemDir) {
    for name in ["GRAPH", "GRAPH.copy"] {
        let mut bytes = dir.file(name).unwrap();
        bytes[8..12].copy_from_slice(&2u32.to_le_bytes());
        let crc = crate::store_tests::crc32c(&bytes[..252]);
        bytes[252..256].copy_from_slice(&crc.to_le_bytes());
        dir.put_file(name, bytes);
    }
}

#[test]
fn a_copying_upgrade_of_a_current_store_is_refused() {
    let dir = MemDir::new();
    store_with_commits(&dir, 1).close().unwrap();
    assert_cause(
        Store::upgrade(&dir, MemDir::new()),
        StoreFailure::AlreadyCurrent,
        "graphersal store convert",
    );
    assert_eq!(StoreFailure::AlreadyCurrent.name(), "already_current");
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_damaged_older_store_is_not_upgraded() {
    let dir = MemDir::new();
    let store = store_with_commits(&dir, 1);
    let snapshot = store.checkpoint(None).unwrap();
    store.close().unwrap();
    let prefix = format!("snapshots/{:020}/", snapshot.commit_seq);
    let name = dir
        .file_names()
        .into_iter()
        .filter(|n| n.starts_with(&prefix) && !n.ends_with("/manifest"))
        .max_by_key(|n| dir.file(n).map_or(0, |b| b.len()))
        .expect("a segment of the newest snapshot");
    let mut bytes = dir.file(&name).unwrap();
    let middle = bytes.len() / 2;
    bytes[middle] ^= 0x40;
    dir.put_file(&name, bytes);
    make_version_2(&dir);
    let before = dir.file_names();
    assert_cause(
        Store::upgrade_in_place(&dir),
        StoreFailure::DamagedSource,
        "graphersal store repair",
    );
    assert_cause(
        Store::upgrade(&dir, MemDir::new()),
        StoreFailure::DamagedSource,
        "graphersal store repair",
    );
    assert_eq!(dir.file_names(), before, "nothing was written");
    assert_eq!(StoreFailure::DamagedSource.name(), "damaged_source");
}

/// The causes of the embedding models a store keeps (format spec 4.5).
#[test]
fn an_unknown_or_invalid_model_is_refused_with_its_cause() {
    use graphersal::persist::{ImportModelOptions, ModelRole, ModelSource, NewModel};
    use std::io::Cursor;
    let store = Store::create(MemDir::new(), StoreOptions::new()).unwrap();
    assert_cause(
        store.remove_model("acme/none"),
        StoreFailure::UnknownModel,
        "graphersal store models",
    );
    assert_cause(
        store.set_default_model(Some("acme/none")),
        StoreFailure::UnknownModel,
        "embed-model download",
    );
    assert_cause(
        store.import_model(
            ModelSource::Files(NewModel::new().with_file(
                "config.json",
                ModelRole::Config,
                Cursor::new(b"{}".to_vec()),
            )),
            ImportModelOptions::new().with_repo("acme/tiny"),
        ),
        StoreFailure::InvalidModel,
        "embed-model import",
    );
    assert_eq!(StoreFailure::UnknownModel.name(), "unknown_model");
    assert_eq!(StoreFailure::InvalidModel.name(), "invalid_model");
}

/// The causes of the embedding models as catalog definitions (format spec 4.5, kind 5).
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn the_model_definition_causes_are_refused_with_their_help() {
    use crate::model_fixture::{import, set_schema, tiny_model};
    use graphersal::persist::{ImportModelOptions, ModelSource};
    let dir = MemDir::new();
    let store = Store::create(dir.clone(), StoreOptions::new()).unwrap();
    let model = import(&store, 1, "acme/tiny").model;
    assert_cause(
        store.import_model(
            ModelSource::Files(tiny_model(2, "acme/tiny")),
            ImportModelOptions::new(),
        ),
        StoreFailure::ModelRevisionExists,
        "with_replace",
    );
    set_schema(&store, &model.identity);
    assert_cause(
        store.remove_model("acme/tiny"),
        StoreFailure::ModelInUse,
        "re-embed",
    );
    // A schema that needs a model the catalog does not hold: the store does not open.
    set_schema(&store, "model2vec/acme/gone@0123456789ab");
    store.close().unwrap();
    assert_cause(
        Store::open(dir.clone(), StoreOptions::new()),
        StoreFailure::ModelUnavailable,
        "embed-model download",
    );
    assert_eq!(StoreFailure::ModelUnavailable.name(), "model_unavailable");
    assert_eq!(StoreFailure::ModelDoesNotFit.name(), "model_does_not_fit");
    assert_eq!(StoreFailure::ModelInUse.name(), "model_in_use");
    assert_eq!(
        StoreFailure::ModelRevisionExists.name(),
        "model_revision_exists"
    );
    // The memory refusal needs a model loader (feature `model2vec`): `model_open_tests.rs`.
}

/// A damaged embedding model of a Graphersal 0.4.0 store (its `models/`) refuses the upgrade
/// before anything is written (format spec 15.1).
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_damaged_040_model_refuses_the_upgrade_with_its_cause() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/v0.4.0/store");
    fn walk(root: &std::path::Path, at: &std::path::Path, dir: &MemDir) {
        for entry in std::fs::read_dir(at).unwrap() {
            let path = entry.unwrap().path();
            let name = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if path.is_dir() {
                dir.create_dir_all(&name).unwrap();
                walk(root, &path, dir);
            } else {
                dir.write_atomic(&name, &std::fs::read(&path).unwrap())
                    .unwrap();
            }
        }
    }
    for damage in ["flip", "remove"] {
        let dir = MemDir::new();
        walk(&root, &root, &dir);
        let file = "models/model2vec-70c61d0ed332/tokenizer.json";
        match damage {
            "flip" => {
                let mut bytes = dir.read(file, u64::MAX).unwrap();
                bytes[0] ^= 0x55;
                dir.write_atomic(file, &bytes).unwrap();
            }
            _ => dir.remove_file(file).unwrap(),
        }
        let before = dir.read("GRAPH", u64::MAX).unwrap();
        assert_cause(
            Store::upgrade_in_place(&dir),
            StoreFailure::UpgradeModelDamaged,
            "embed-model import",
        );
        assert_eq!(dir.read("GRAPH", u64::MAX).unwrap(), before, "{damage}");
        assert!(dir.entry_kind("INTENT").unwrap().is_none(), "{damage}");
    }
    assert_eq!(
        StoreFailure::UpgradeModelDamaged.name(),
        "upgrade_model_damaged"
    );
}

#[test]
fn a_missing_blob_names_both_recovery_commands_and_a_closed_change_names_the_open_store() {
    let dir = MemDir::new();
    let store = Store::create(dir.clone(), StoreOptions::new()).unwrap();
    let blob = store
        .write_blob(&mut &b"some bytes"[..])
        .unwrap()
        .blob()
        .clone();
    dir.remove_file(&format!("blobs/{}", blob.relative_path()))
        .unwrap();
    for needle in ["store restore-blobs", "--drop-damaged-blobs"] {
        assert_cause(
            store.read_blob(&blob),
            StoreFailure::BlobUnavailable,
            needle,
        );
    }
    // A closed-store catalog change while the store is open: refused with the lock, whose help
    // names the open store's methods.
    let err = Store::remove_model_dir(
        dir.clone(),
        "acme/tiny",
        graphersal::persist::ClosedCommitOptions::new(),
    )
    .unwrap_err();
    assert!(matches!(err, PersistError::Locked { .. }), "{err}");
    assert!(err.help().unwrap().contains("Store::remove_model"));
    store.close().unwrap();
}
