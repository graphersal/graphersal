//! Embedding models kept in a Store (feature `persist`; format spec 4.5, catalog kind 5, book
//! page "Embedding Models in a Store"), rewritten for 0.5.0 (models are ONE blob each plus a
//! catalog definition that follows the history): import, list, the licence template and base
//! model licences, refusals that leave nothing, removal as a commit, `verify` of every model
//! blob, every copy carrying them, the single-file and memory backends, hostile files never
//! panic, the store info naming what the schema needs. With the feature `model2vec`: a model
//! the schema needs is loaded at open and serves text queries. No network: the models are tiny
//! and synthetic (`model_fixture.rs`).
#![cfg(all(feature = "persist", not(target_arch = "wasm32")))]

use std::fs;
use std::io::Cursor;

use graphersal::persist::{
    ImportModelOptions, MemDir, ModelCopy, ModelRole, ModelSource, ModelState, NewModel,
    PersistError, RecoveryTarget, Store, StoreFailure,
};
use graphersal::prelude::*;

use crate::model_fixture::*;

/// The repositories of models.
fn repos(models: &[graphersal::persist::ModelInfo]) -> Vec<String> {
    models.iter().map(|m| m.repo.clone()).collect()
}

/// The files of the blobs directory of the store at `path` (no temporary files).
fn blob_files(path: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(path.join("blobs")) else {
        return out;
    };
    for entry in entries.flatten() {
        if entry.path().is_dir() {
            for file in fs::read_dir(entry.path()).unwrap().flatten() {
                out.push(file.file_name().to_string_lossy().to_string());
            }
        }
    }
    out
}

#[test]
fn an_added_model_is_listed_with_its_identity_licence_and_files() {
    let temp = Temp::new("listed");
    let path = temp.join("s");
    let store = Store::create(&path, options()).unwrap();
    add_person(&store, "ann");
    let before = store.graph().read().last_commit_seq();
    let report = import(&store, 1, "acme/tiny");
    assert!(!report.already_present);
    assert_eq!(report.commit_seq, Some(before + 1), "one commit");
    let model = &report.model;
    assert_eq!(model.repo, "acme/tiny");
    assert!(
        model.identity.starts_with("model2vec/acme/tiny@"),
        "{}",
        model.identity
    );
    assert_eq!(model.license, "mit");
    assert_eq!(model.license_text, "template");
    assert_eq!(model.dimension, DIMENSION as u64);
    assert_eq!(
        model.load_bytes,
        (VOCAB.len() * DIMENSION * 4) as u64 + tokenizer_json().len() as u64
    );
    assert!(model.bytes > safetensors(1).len() as u64);
    assert!(!model.default);
    assert_eq!(store.models().unwrap(), vec![model.clone()]);
    let info = store.info().unwrap();
    assert_eq!(repos(&info.models), ["acme/tiny"]);
    assert_eq!(info.default_model, None);
    assert_eq!(blob_files(&path).len(), 1);
    // The same model again: nothing committed, nothing written.
    let again = import(&store, 1, "acme/tiny");
    assert!(again.already_present);
    assert_eq!(again.commit_seq, None);
    assert_eq!(store.graph().read().last_commit_seq(), before + 1);
    assert_eq!(blob_files(&path).len(), 1);
    // The licence warning of a template; the model is in the catalog as kind 5.
    assert!(report.warnings.iter().any(|w| w.contains("template")));
    let graph = store.graph().read();
    let definition = graph
        .definition(graphersal::catalog::DefinitionKind::MODEL, &model.identity)
        .unwrap()
        .unwrap();
    assert_eq!(definition.as_model().unwrap().repo(), "acme/tiny");
}

#[test]
fn the_licence_text_comes_from_a_file_a_template_or_nowhere() {
    let store = Store::create(MemDir::new(), options()).unwrap();
    let with_file = tiny_model(1, "acme/a").with_file(
        "LICENSE",
        ModelRole::License,
        Cursor::new(b"own terms".to_vec()),
    );
    let model = store
        .import_model(ModelSource::Files(with_file), ImportModelOptions::new())
        .unwrap()
        .model;
    assert_eq!(model.license_text, "file");
    let apache = tiny_model(2, "acme/b").with_license("apache-2.0");
    let model = store
        .import_model(ModelSource::Files(apache), ImportModelOptions::new())
        .unwrap()
        .model;
    assert_eq!(
        (model.license.as_str(), model.license_text.as_str()),
        ("apache-2.0", "template")
    );
    let other = tiny_model(3, "acme/c").with_license("other");
    let report = store
        .import_model(ModelSource::Files(other), ImportModelOptions::new())
        .unwrap();
    assert_eq!(report.model.license_text, "none");
    let unknown = NewModel::new()
        .with_file(
            "config.json",
            ModelRole::Config,
            Cursor::new(CONFIG.as_bytes().to_vec()),
        )
        .with_file(
            "tokenizer.json",
            ModelRole::Tokenizer,
            Cursor::new(tokenizer_json().into_bytes()),
        )
        .with_file(
            "model.safetensors",
            ModelRole::Weights,
            Cursor::new(safetensors(4)),
        );
    let report = store
        .import_model(
            ModelSource::Files(unknown),
            ImportModelOptions::new().with_repo("acme/d"),
        )
        .unwrap();
    assert_eq!(report.model.license, "unknown");
    assert!(report.warnings.iter().any(|w| w.contains("unknown")));
}

#[test]
fn the_licences_of_the_base_models_travel_with_the_model() {
    let temp = Temp::new("bases");
    let dir = tiny_model_dir(&temp, "model", 1, "acme/tiny");
    let store = Store::create(temp.join("s"), options()).unwrap();
    let model = store
        .import_model(ModelSource::Dir(dir), ImportModelOptions::new())
        .unwrap()
        .model;
    assert_eq!(model.license, "apache-2.0");
    assert_eq!(model.revision.as_deref(), Some("main"));
    assert_eq!(model.base_models.len(), 1);
    assert_eq!(model.base_models[0].repo, "acme/base");
    assert_eq!(model.base_models[0].license.as_deref(), Some("mit"));
}

#[test]
fn an_invalid_model_is_refused_and_leaves_nothing() {
    let temp = Temp::new("invalid");
    let path = temp.join("s");
    let store = Store::create(&path, options()).unwrap();
    let seq = store.graph().read().last_commit_seq();
    let refused = |model: NewModel, options: ImportModelOptions| match store
        .import_model(ModelSource::Files(model), options)
    {
        Err(PersistError::Store { cause, .. }) => assert_eq!(cause, StoreFailure::InvalidModel),
        other => panic!("not refused: {other:?}"),
    };
    // No weights.
    refused(
        NewModel::new()
            .with_file(
                "config.json",
                ModelRole::Config,
                Cursor::new(b"{}".to_vec()),
            )
            .with_file(
                "tokenizer.json",
                ModelRole::Tokenizer,
                Cursor::new(b"{}".to_vec()),
            ),
        ImportModelOptions::new().with_repo("acme/x"),
    );
    // No repository.
    refused(
        NewModel::new()
            .with_file(
                "config.json",
                ModelRole::Config,
                Cursor::new(b"{}".to_vec()),
            )
            .with_file(
                "tokenizer.json",
                ModelRole::Tokenizer,
                Cursor::new(b"{}".to_vec()),
            )
            .with_file(
                "model.safetensors",
                ModelRole::Weights,
                Cursor::new(safetensors(1)),
            ),
        ImportModelOptions::new(),
    );
    // A name that is not allowed, weights that are no safetensors file, a repository that
    // contradicts the source.
    refused(
        tiny_model(1, "acme/x").with_file("../x", ModelRole::Card, Cursor::new(b"x".to_vec())),
        ImportModelOptions::new(),
    );
    refused(
        NewModel::new()
            .with_file(
                "config.json",
                ModelRole::Config,
                Cursor::new(b"{}".to_vec()),
            )
            .with_file(
                "tokenizer.json",
                ModelRole::Tokenizer,
                Cursor::new(b"{}".to_vec()),
            )
            .with_file(
                "model.safetensors",
                ModelRole::Weights,
                Cursor::new(vec![9u8; 64]),
            ),
        ImportModelOptions::new().with_repo("acme/x"),
    );
    refused(
        tiny_model(1, "acme/x"),
        ImportModelOptions::new().with_repo("acme/y"),
    );
    assert_eq!(store.graph().read().last_commit_seq(), seq);
    assert!(store.models().unwrap().is_empty());
    assert!(blob_files(&path).is_empty(), "no blob was written");
}

#[test]
fn a_model_leaves_only_by_an_explicit_remove() {
    let temp = Temp::new("remove");
    let path = temp.join("s");
    let store = Store::create(&path, options()).unwrap();
    let model = import(&store, 1, "acme/tiny").model;
    add_person(&store, "ann");
    store.checkpoint(None).unwrap();
    store.compact().unwrap();
    assert_eq!(repos(&store.models().unwrap()), ["acme/tiny"]);
    let removed = store.remove_model("acme/tiny").unwrap();
    assert!(removed.commit_seq.is_some());
    assert_eq!(removed.model.unwrap().identity, model.identity);
    assert!(store.models().unwrap().is_empty());
    // The earlier position still has it: the blob stays until a prune drops that history.
    assert_eq!(blob_files(&path).len(), 1);
    match store.remove_model("acme/tiny") {
        Err(PersistError::Store { cause, .. }) => assert_eq!(cause, StoreFailure::UnknownModel),
        other => panic!("{other:?}"),
    }
    store.close().unwrap();
    assert!(Store::models_dir(&path).unwrap().is_empty());
}

#[test]
fn verify_finds_a_damaged_model_and_the_store_stays_writable() {
    let temp = Temp::new("verify");
    let path = temp.join("s");
    let store = Store::create(&path, options()).unwrap();
    let model = import(&store, 1, "acme/tiny").model;
    let report = store.verify().unwrap();
    assert!(report.is_ok(), "{:?}", report.problems);
    assert_eq!(report.models_checked, 1);
    assert_eq!(report.models.len(), 1);
    flip_byte(&blob_path(&path, &store, &model.identity));
    let report = store.verify().unwrap();
    assert!(
        report.problems.iter().any(|p| p.path.starts_with("blobs/")),
        "{:?}",
        report.problems
    );
    assert!(matches!(report.models[0].state, ModelState::Damaged { .. }));
    // Not the store's damage: it keeps writing.
    add_person(&store, "bob");
    assert!(store.read_only_reason().unwrap().is_none());
    assert!(matches!(
        store.models().unwrap()[0].state,
        ModelState::NotLoaded | ModelState::Damaged { .. }
    ));
}

#[test]
fn every_copy_carries_the_models() {
    let temp = Temp::new("copies");
    let path = temp.join("s");
    let store = Store::create(&path, options()).unwrap();
    add_person(&store, "ann");
    import(&store, 1, "acme/tiny");
    store.checkpoint(None).unwrap();
    // Backup (full, then an increment), fork, packed snapshot, single-file conversion.
    let backup = store.backup(temp.join("backup")).unwrap();
    assert_eq!(repos(&backup.models), ["acme/tiny"]);
    import(&store, 2, "acme/other");
    let increment = store.backup(temp.join("backup")).unwrap();
    assert_eq!(repos(&increment.models), ["acme/other", "acme/tiny"]);
    let fork = store
        .fork(RecoveryTarget::Latest, temp.join("fork"))
        .unwrap();
    assert_eq!(repos(&fork.models), ["acme/other", "acme/tiny"]);
    assert!(fork.models.iter().all(|m| m.state == ModelState::NotLoaded));
    store.checkpoint(None).unwrap();
    let mut packed = Vec::new();
    store.export_packed(None, &mut packed).unwrap();
    store.close().unwrap();
    let from_packed =
        Store::create_from_packed(temp.join("packed"), packed.as_slice(), options()).unwrap();
    assert_eq!(
        repos(&from_packed.models().unwrap()),
        ["acme/other", "acme/tiny"]
    );
    assert!(from_packed.verify().unwrap().is_ok());
    from_packed.close().unwrap();
    // Without the models: the definitions stay, the blobs are missing.
    let store = Store::open(&path, options()).unwrap();
    let mut without = Vec::new();
    store
        .export_packed_with(None, &mut without, ModelCopy::Exclude)
        .unwrap();
    assert!(without.len() < packed.len());
    store.close().unwrap();
    Store::convert(&path, temp.join("s.gstore")).unwrap();
    assert_eq!(
        repos(&Store::models_dir(temp.join("s.gstore")).unwrap()),
        ["acme/other", "acme/tiny"]
    );
    for copy in ["backup", "fork", "packed"] {
        let models = Store::models_dir(temp.join(copy)).unwrap();
        assert!(!models.is_empty(), "{copy}");
        assert!(
            models.iter().all(|m| m.state == ModelState::NotLoaded),
            "{copy}: {models:?}"
        );
    }
}

#[test]
fn a_single_file_and_a_memory_store_keep_models_the_same_way() {
    let temp = Temp::new("backends");
    let file = temp.join("s.gstore");
    let memory = MemDir::new();
    for store in [
        Store::create(&file, options()).unwrap(),
        Store::create(memory.clone(), options()).unwrap(),
    ] {
        let model = import(&store, 1, "acme/tiny").model;
        assert_eq!(store.models().unwrap(), vec![model]);
        assert!(store.verify().unwrap().is_ok());
        store.close().unwrap();
    }
    assert_eq!(repos(&Store::models_dir(&file).unwrap()), ["acme/tiny"]);
    assert_eq!(repos(&Store::models_dir(memory).unwrap()), ["acme/tiny"]);
}

#[test]
fn hostile_model_files_never_panic() {
    let store = Store::create(MemDir::new(), options()).unwrap();
    let headers: [Vec<u8>; 5] = [
        Vec::new(),
        vec![0xff; 8],
        u64::MAX.to_le_bytes().to_vec(),
        {
            let mut v = 16u64.to_le_bytes().to_vec();
            v.extend_from_slice(b"{\"embeddings\":1}");
            v
        },
        {
            let header = r#"{"embeddings": {"dtype": "F32", "shape": [-1, 99999999999]}}"#;
            let mut v = (header.len() as u64).to_le_bytes().to_vec();
            v.extend_from_slice(header.as_bytes());
            v
        },
    ];
    for weights in headers {
        let model = NewModel::new()
            .with_file(
                "config.json",
                ModelRole::Config,
                Cursor::new(b"{}".to_vec()),
            )
            .with_file(
                "tokenizer.json",
                ModelRole::Tokenizer,
                Cursor::new(b"{}".to_vec()),
            )
            .with_file(
                "model.safetensors",
                ModelRole::Weights,
                Cursor::new(weights),
            );
        let _ = store.import_model(
            ModelSource::Files(model),
            ImportModelOptions::new().with_repo("acme/x"),
        );
    }
    for name in [
        "",
        ".x",
        "graphersal-model.json",
        &"a".repeat(41),
        "a/b",
        "x\u{0}",
    ] {
        let model = tiny_model(1, "acme/x").with_file(name, ModelRole::Notice, Cursor::new(vec![]));
        assert!(
            store
                .import_model(ModelSource::Files(model), ImportModelOptions::new())
                .is_err(),
            "{name:?}"
        );
    }
    let mut many = tiny_model(1, "acme/x");
    for i in 0..64 {
        many = many.with_file(
            format!("NOTICE{i}"),
            ModelRole::Notice,
            Cursor::new(vec![1]),
        );
    }
    assert!(
        store
            .import_model(ModelSource::Files(many), ImportModelOptions::new())
            .is_err()
    );
}

#[test]
fn the_store_info_names_the_models_of_the_schema() {
    let temp = Temp::new("info");
    let path = temp.join("s");
    let store = Store::create(&path, options()).unwrap();
    let model = import(&store, 1, "acme/tiny").model;
    import(&store, 2, "acme/other");
    set_schema(&store, &model.identity);
    let info = store.info().unwrap();
    assert_eq!(info.schema_models, vec![model.identity.clone()]);
    let tiny = info.models.iter().find(|m| m.repo == "acme/tiny").unwrap();
    assert_eq!(tiny.needed_by, ["doc.emb"]);
    let other = info.models.iter().find(|m| m.repo == "acme/other").unwrap();
    assert!(other.needed_by.is_empty());
    // A model the schema needs cannot be removed.
    match store.remove_model("acme/tiny") {
        Err(err @ PersistError::Store { .. }) => {
            assert!(err.to_string().contains("doc.emb"), "{err}");
            assert!(matches!(
                err,
                PersistError::Store {
                    cause: StoreFailure::ModelInUse,
                    ..
                }
            ));
        }
        other => panic!("{other:?}"),
    }
    store.close().unwrap();
    let info = Store::inspect(&path).unwrap();
    assert_eq!(info.schema_models, vec![model.identity.clone()]);
    assert_eq!(repos(&info.models), ["acme/other", "acme/tiny"]);
}

/// The models of a schema are loaded at open (feature `model2vec`).
#[cfg(feature = "model2vec")]
mod auto_load {
    use super::*;

    #[test]
    fn a_stored_model_the_schema_names_is_loaded_at_open() {
        for single_file in [false, true] {
            let temp = Temp::new("autoload");
            let path = if single_file {
                temp.join("s.gstore")
            } else {
                temp.join("s")
            };
            let store = Store::create(&path, options()).unwrap();
            let model = import(&store, 1, "acme/tiny").model;
            set_schema(&store, &model.identity);
            store.close().unwrap();

            let store = Store::open(&path, options()).unwrap();
            let models = store.models().unwrap();
            assert_eq!(models[0].state, ModelState::Loaded);
            let memory = graphersal::storage::AnyGraph::memory_usage(&**store.graph())
                .unwrap()
                .unwrap();
            assert_eq!(memory.model_bytes, model.load_bytes);
            // The model serves `embed` by its repository name without any host embedder.
            let vector = store
                .graph()
                .write()
                .traversal_mut()
                .inject(vec!["graph vector"])
                .embed_value(Some("acme/tiny"))
                .next()
                .unwrap()
                .unwrap();
            assert_eq!(vector.as_vector().map(|v| v.dimension()), Some(DIMENSION));
            store.close().unwrap();
        }
    }

    #[test]
    fn every_stored_model_is_registered_at_open() {
        let temp = Temp::new("registered");
        let path = temp.join("s");
        let store = Store::create(&path, options()).unwrap();
        import(&store, 1, "acme/tiny");
        store.set_default_model(Some("acme/tiny")).unwrap();
        store.close().unwrap();
        let store = Store::open(&path, options()).unwrap();
        // Not needed by the schema: not loaded until a query uses it.
        assert_eq!(store.models().unwrap()[0].state, ModelState::NotLoaded);
        let vector = store
            .graph()
            .read()
            .traversal()
            .inject(vec!["graph"])
            .embed_value(None)
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(vector.as_vector().map(|v| v.dimension()), Some(DIMENSION));
        assert_eq!(store.models().unwrap()[0].state, ModelState::Loaded);
    }
}
