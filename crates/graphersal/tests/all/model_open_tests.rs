//! The open of a store whose schema NEEDS an embedding model (format spec 4.5; book page
//! "Embedding Models in a Store"): a needed model whose blob is damaged or missing, or that is
//! not in the catalog, refuses the open in every mode with the commands that bring it back; a
//! model nothing needs that is damaged leaves the store working (journal record); a build
//! without a model loader opens read-only without text queries; with the feature `model2vec` a
//! needed model is loaded at open, counted in `model_bytes`, refused when it does not fit under
//! the memory ceiling (nothing allocated) and unloaded when a rollback leaves it behind.
#![cfg(all(feature = "persist", not(target_arch = "wasm32")))]

use graphersal::persist::{PersistError, RecoveryTarget, Store, StoreFailure};
#[allow(unused_imports)]
use graphersal::prelude::*;
use graphersal::system_journal::JournalQuery;

use crate::model_fixture::*;

fn cause_of<T>(result: Result<T, PersistError>) -> (StoreFailure, String) {
    match result {
        Err(err @ PersistError::Store { .. }) => {
            let help = graphersal::error::format_diagnostic(&err);
            let PersistError::Store { cause, .. } = err else {
                unreachable!()
            };
            (cause, help)
        }
        Err(other) => panic!("another error: {other}"),
        Ok(_) => panic!("not refused"),
    }
}

/// A store at `path` whose schema needs the tiny model, the model's identity.
fn store_needing_a_model(path: &std::path::Path) -> String {
    let store = Store::create(path, options()).unwrap();
    let model = import(&store, 1, "acme/tiny").model;
    set_schema(&store, &model.identity);
    add_person(&store, "ann");
    store.checkpoint(None).unwrap();
    store.close().unwrap();
    model.identity
}

/// The path of the only blob of the store at `path`.
fn only_blob(path: &std::path::Path) -> std::path::PathBuf {
    let mut found = Vec::new();
    for dir in std::fs::read_dir(path.join("blobs")).unwrap().flatten() {
        if dir.path().is_dir() {
            for file in std::fs::read_dir(dir.path()).unwrap().flatten() {
                found.push(file.path());
            }
        }
    }
    assert_eq!(found.len(), 1, "{found:?}");
    found.remove(0)
}

#[test]
fn a_damaged_needed_model_refuses_every_open_with_the_restore_commands() {
    for missing in [false, true] {
        let temp = Temp::new("damaged");
        let path = temp.join("s");
        store_needing_a_model(&path);
        {
            // A backup before the damage, for the backup mode below.
            let store = Store::open(&path, options()).unwrap();
            store.backup(temp.join("backup")).unwrap();
            store.close().unwrap();
        }
        let blob = only_blob(&path);
        if missing {
            std::fs::remove_file(&blob).unwrap();
        } else {
            flip_byte(&blob);
        }
        let (cause, help) = cause_of(Store::open(&path, options()));
        assert_eq!(cause, StoreFailure::ModelUnavailable, "{help}");
        for needle in [
            "acme/tiny",
            "doc.emb",
            "embed-model download acme/tiny",
            "--replace",
            "backup",
        ] {
            assert!(help.contains(needle), "{needle}: {help}");
        }
        let (cause, _) = cause_of(Store::open_read_only(&path, RecoveryTarget::Latest));
        assert_eq!(cause, StoreFailure::ModelUnavailable);
        // The backup's blob is the same file name: damaged there too.
        let backup_blob = only_blob(&temp.join("backup"));
        if missing {
            std::fs::remove_file(&backup_blob).unwrap();
        } else {
            flip_byte(&backup_blob);
        }
        let (cause, _) = cause_of(Store::open_backup(temp.join("backup"), options()));
        assert_eq!(cause, StoreFailure::ModelUnavailable);
        // The refused open is in the store's journal (best effort, written while closed).
        let page = Store::journal_dir(&path, &JournalQuery::new().kind("model.load_failed"))
            .unwrap();
        assert!(!page.records.is_empty(), "no model.load_failed record");
    }
}

#[test]
fn a_needed_model_that_is_not_in_the_catalog_refuses_the_open() {
    let temp = Temp::new("not-in-catalog");
    let path = temp.join("s");
    let store = Store::create(&path, options()).unwrap();
    set_schema(&store, "model2vec/acme/gone@0123456789ab");
    store.close().unwrap();
    let (cause, help) = cause_of(Store::open(&path, options()));
    assert_eq!(cause, StoreFailure::ModelUnavailable);
    assert!(help.contains("not in the database"), "{help}");
    // The import into the closed store brings it back (it opens without the check).
    let report = Store::import_model_dir(
        &path,
        graphersal::persist::ModelSource::Files(tiny_model(1, "acme/tiny")),
        graphersal::persist::ImportModelOptions::new(),
    )
    .unwrap();
    assert!(report.commit_seq.is_some());
}

#[test]
fn a_damaged_model_nothing_needs_leaves_the_store_working() {
    let temp = Temp::new("not-needed");
    let path = temp.join("s");
    let store = Store::create(&path, options()).unwrap();
    import(&store, 1, "acme/tiny");
    store.close().unwrap();
    flip_byte(&only_blob(&path));
    let store = Store::open(&path, options()).unwrap();
    assert!(store.read_only_reason().unwrap().is_none());
    add_person(&store, "ann");
    let models = store.models().unwrap();
    assert!(
        matches!(models[0].state, graphersal::persist::ModelState::Damaged { .. }),
        "{models:?}"
    );
    let page = store
        .journal()
        .unwrap()
        .query(&JournalQuery::new().kind("model.unavailable"))
        .unwrap();
    assert_eq!(page.records.len(), 1);
}

/// A build without a model loader: the store opens read-only, text queries name the reason.
#[cfg(not(feature = "model2vec"))]
#[test]
fn a_build_without_a_model_loader_opens_read_only_without_text_queries() {
    let temp = Temp::new("no-loader");
    let path = temp.join("s");
    store_needing_a_model(&path);
    let store = Store::open(&path, options()).unwrap();
    let reason = store.read_only_reason().unwrap().unwrap();
    assert!(reason.contains("cannot run the model acme/tiny"), "{reason}");
    let err = store
        .graph()
        .read()
        .traversal()
        .inject(vec!["graph"])
        .embed_value(Some("acme/tiny"))
        .to_list()
        .unwrap_err();
    assert!(err.to_string().contains("cannot run the model"), "{err}");
    assert!(matches!(
        Store::open(&path, options()).map(|_| ()),
        Err(PersistError::Locked { .. })
    ));
}

/// Loads, memory and the ceiling (feature `model2vec`).
#[cfg(feature = "model2vec")]
mod loading {
    use super::*;
    use graphersal::persist::{ModelState, RollbackMode};

    #[test]
    fn a_needed_model_is_loaded_at_open_and_counted() {
        let temp = Temp::new("loaded");
        let path = temp.join("s");
        let identity = store_needing_a_model(&path);
        let store = Store::open(&path, options()).unwrap();
        let models = store.models().unwrap();
        assert_eq!(models[0].identity, identity);
        assert_eq!(models[0].state, ModelState::Loaded);
        let memory = graphersal::storage::AnyGraph::memory_usage(&**store.graph()).unwrap().unwrap();
        assert_eq!(memory.model_bytes, models[0].load_bytes);
        assert!(memory.model_bytes > 0);
        // A text query embeds with it.
        let found = store
            .graph()
            .read()
            .traversal()
            .v(None::<()>)
            .has_label("doc")
            .count()
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(found.as_i64(), Some(0));
        let page = store
            .journal()
            .unwrap()
            .query(&JournalQuery::new().kind("model.loaded"))
            .unwrap();
        assert!(!page.records.is_empty());
    }

    #[test]
    fn a_needed_model_over_the_ceiling_refuses_the_open_with_the_minimum() {
        let temp = Temp::new("ceiling");
        let path = temp.join("s");
        store_needing_a_model(&path);
        let refused = graphersal::exec::memory_limit_scope(64, || Store::open(&path, options()));
        let (cause, help) = match refused {
            Err(err @ PersistError::Store { .. }) => {
                let help = graphersal::error::format_diagnostic(&err);
                let PersistError::Store { cause, .. } = err else {
                    unreachable!()
                };
                (cause, help)
            }
            // The snapshot load itself may be refused first under so small a ceiling.
            Err(PersistError::MemoryBudget { .. }) | Err(PersistError::Graph(_)) => return,
            other => panic!("{:?}", other.map(|_| ())),
        };
        assert_eq!(cause, StoreFailure::ModelDoesNotFit, "{help}");
        assert!(help.contains("at least"), "{help}");
        assert!(help.contains("--memory-limit"), "{help}");
        // Nothing stays loaded: the store opens normally afterwards.
        let store = Store::open(&path, options()).unwrap();
        assert_eq!(store.models().unwrap()[0].state, ModelState::Loaded);
    }

    #[test]
    fn a_rollback_past_the_import_unloads_the_model() {
        let temp = Temp::new("unload");
        let path = temp.join("s");
        let store = Store::create(&path, options()).unwrap();
        add_person(&store, "ann");
        store.mark("before").unwrap();
        import(&store, 3, "acme/tiny");
        store.set_default_model(Some("acme/tiny")).unwrap();
        // A query loads it lazily.
        store
            .graph()
            .read()
            .traversal()
            .inject(vec!["graph"])
            .embed_value(None)
            .to_list()
            .unwrap();
        assert!(graphersal::storage::AnyGraph::memory_usage(&**store.graph()).unwrap().unwrap().model_bytes > 0);
        store
            .rollback_to(RecoveryTarget::Mark("before".into()), RollbackMode::Move)
            .unwrap();
        assert_eq!(graphersal::storage::AnyGraph::memory_usage(&**store.graph()).unwrap().unwrap().model_bytes, 0);
        assert!(store.models().unwrap().is_empty());
        let page = store
            .journal()
            .unwrap()
            .query(&JournalQuery::new().kind("model.unloaded"))
            .unwrap();
        assert!(!page.records.is_empty());
    }
}
