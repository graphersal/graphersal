//! Every copy path carries the models (root rule 22 style; format spec 4.5, 4.6, 12.x): full
//! and incremental backups (restored), the ZIP backup (restored, feature `persist-zip`), fork,
//! attic fork, repair, conversion to a single file, a packed snapshot. Each copy lists the
//! model with an intact blob, verifies, and (feature `model2vec`) opens with the model its
//! schema needs loaded.
#![cfg(all(feature = "persist", not(target_arch = "wasm32")))]

use std::path::{Path, PathBuf};

use graphersal::persist::{ModelState, RecoveryTarget, RollbackMode, Store};
use graphersal::prelude::*;

use crate::model_fixture::*;

/// Checks the copy at `path`: the model listed with an intact blob, verify clean, and with a
/// model loader the store opens with the needed model loaded.
fn check_copy(path: &Path, what: &str) {
    let models = Store::models_dir(path).unwrap();
    assert_eq!(models.len(), 1, "{what}: {models:?}");
    assert_eq!(models[0].repo, "acme/tiny", "{what}");
    assert_eq!(models[0].state, ModelState::NotLoaded, "{what}");
    assert_eq!(models[0].needed_by, ["doc.emb"], "{what}");
    let verified = Store::verify_dir(path).unwrap();
    assert!(verified.is_ok(), "{what}: {:?}", verified.problems);
    assert_eq!(verified.models_checked, 1, "{what}");
    let store = Store::open(path, options()).unwrap();
    #[cfg(feature = "model2vec")]
    {
        assert_eq!(
            store.models().unwrap()[0].state,
            ModelState::Loaded,
            "{what}"
        );
        assert!(store.read_only_reason().unwrap().is_none(), "{what}");
    }
    #[cfg(not(feature = "model2vec"))]
    assert!(store.read_only_reason().unwrap().is_some(), "{what}");
    store.close().unwrap();
}

#[test]
fn every_copy_path_carries_the_model_and_the_copy_opens_with_it() {
    let temp = Temp::new("copy-paths");
    let path = temp.join("s");
    let store = Store::create(&path, options()).unwrap();
    add_person(&store, "ann");
    store.mark("start").unwrap();
    let model = import(&store, 1, "acme/tiny").model;
    set_schema(&store, &model.identity);
    store.checkpoint(None).unwrap();
    let mut copies: Vec<(PathBuf, &str)> = Vec::new();

    // A full backup, then an increment of it; restored into a store.
    store.backup(temp.join("backup")).unwrap();
    add_person(&store, "bob");
    let increment = store.backup(temp.join("backup")).unwrap();
    assert!(increment.incremental);
    assert_eq!(increment.models.len(), 1);
    copy_tree(&temp.join("backup"), &temp.join("restored"));
    Store::restore_backup(temp.join("restored")).unwrap();
    copies.push((temp.join("restored"), "restored backup"));

    // A fork.
    let fork = store
        .fork(RecoveryTarget::Latest, temp.join("fork"))
        .unwrap();
    assert_eq!(fork.models.len(), 1);
    copies.push((temp.join("fork"), "fork"));

    // A repair (of the healthy store: a fresh copy).
    let repair = store.repair_to(temp.join("repair")).unwrap();
    assert_eq!(repair.models.len(), 1);
    assert!(repair.models_not_carried.is_empty());
    copies.push((temp.join("repair"), "repair"));

    // A packed snapshot.
    store.checkpoint(None).unwrap();
    let mut packed = Vec::new();
    store.export_packed(None, &mut packed).unwrap();
    Store::create_from_packed(temp.join("packed"), packed.as_slice(), options())
        .unwrap()
        .close()
        .unwrap();
    copies.push((temp.join("packed"), "packed snapshot"));

    // A ZIP backup, restored.
    #[cfg(feature = "persist-zip")]
    {
        let mut archive = std::io::Cursor::new(Vec::new());
        let report = store.backup_zip(&mut archive).unwrap();
        assert_eq!(report.models.len(), 1);
        archive.set_position(0);
        Store::restore_zip(archive, temp.join("zip")).unwrap();
        copies.push((temp.join("zip"), "zip backup"));
    }

    // An attic fork: roll back past "start" (the model is in the moved history), fork it.
    store
        .rollback_to(RecoveryTarget::Mark("start".into()), RollbackMode::Move)
        .unwrap();
    let entry = store.attic_list().unwrap().pop().unwrap();
    let attic_fork = store.attic_fork(&entry.id, temp.join("attic-fork")).unwrap();
    assert_eq!(attic_fork.models.len(), 1);
    copies.push((temp.join("attic-fork"), "attic fork"));
    store.close().unwrap();

    // A conversion to a single file (of the fork: the source has rolled back).
    Store::convert(temp.join("fork"), temp.join("fork.gstore")).unwrap();
    copies.push((temp.join("fork.gstore"), "single file"));

    for (path, what) in &copies {
        check_copy(path, what);
    }
}

/// Copies a directory tree.
fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[test]
fn the_same_files_give_the_same_blob_in_every_store() {
    let temp = Temp::new("dedup");
    let a = Store::create(temp.join("a"), options()).unwrap();
    let b = Store::create(temp.join("b"), options()).unwrap();
    let one = import(&a, 1, "acme/tiny").model;
    // The same files in another order and from a directory: the same tar, the same blob.
    let dir = tiny_model_dir(&temp, "dir", 1, "acme/tiny");
    std::fs::write(dir.join("README.md"), "---\nlicense: mit\n---\n# A tiny model\n").unwrap();
    std::fs::write(
        dir.join("graphersal-embed-model.json"),
        r#"{"format": 2, "repo": "acme/tiny", "license": "mit", "files": []}"#,
    )
    .unwrap();
    let two = b
        .import_model(
            graphersal::persist::ModelSource::Dir(dir),
            graphersal::persist::ImportModelOptions::new(),
        )
        .unwrap()
        .model;
    assert_eq!(one.identity, two.identity);
    assert_eq!(one.bytes, two.bytes);
    let blob = |store: &Store| {
        let graph = store.graph().read();
        graph
            .definition(graphersal::catalog::DefinitionKind::MODEL, &one.identity)
            .unwrap()
            .unwrap()
            .as_model()
            .unwrap()
            .blob()
            .clone()
    };
    assert_eq!(blob(&a), blob(&b), "same files, same bytes, same blob");
}
