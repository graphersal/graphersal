//! Embedding models as catalog definitions (kind 5, format spec 4.5 and 6.3; book page
//! "Embedding Models in a Store"): an import is ONE commit defining the model; the same identity
//! again commits nothing; another revision of the repository is refused without replace and
//! switches in one commit with it; at most one default, which follows a replace; every position
//! (mark, commit, rollback, view) has the models and the default of its moment; prune removes
//! the blob no retained history references; a definition of a newer version (unknown family,
//! unknown keys) is kept and written back unchanged.
#![cfg(all(feature = "persist", not(target_arch = "wasm32")))]

use graphersal::catalog::{Definition, DefinitionKind, ModelDefinition};
use graphersal::persist::{
    ImportModelOptions, ModelSource, ModelState, PersistError, RecoveryTarget, RollbackMode, Store,
    StoreFailure,
};
use graphersal::prelude::*;
use graphersal::system_journal::JournalQuery;

use crate::model_fixture::*;

#[test]
fn the_schema_accepts_the_repository_and_stores_the_identity() {
    let store = Store::create(graphersal::persist::MemDir::new(), options()).unwrap();
    let model = import(&store, 1, "acme/tiny").model;
    set_schema(&store, "acme/tiny");
    let schema = store.graph().read().get_schema().unwrap().unwrap();
    let embedder = schema.vertices["doc"].schema.properties["emb"]
        .vector
        .as_ref()
        .unwrap()
        .embedder
        .clone();
    assert_eq!(embedder.as_deref(), Some(model.identity.as_str()));
    assert_eq!(store.models().unwrap()[0].needed_by, ["doc.emb"]);
}

fn cause_of<T: std::fmt::Debug>(result: Result<T, PersistError>) -> StoreFailure {
    match result {
        Err(PersistError::Store { cause, .. }) => cause,
        other => panic!("not refused: {other:?}"),
    }
}

/// The model definitions of the store's graph, by name.
fn model_names(store: &Store) -> Vec<String> {
    let graph = store.graph().read();
    let mut names: Vec<String> = graph
        .definitions()
        .unwrap()
        .filter(|d| d.kind() == DefinitionKind::MODEL)
        .map(|d| d.name().to_string())
        .collect();
    names.sort();
    names
}

fn kinds(store: &Store, kind: &str) -> usize {
    store
        .journal()
        .unwrap()
        .query(&JournalQuery::new().kind(kind))
        .unwrap()
        .records
        .len()
}

#[test]
fn an_import_is_one_commit_defining_the_model() {
    let store = Store::create(graphersal::persist::MemDir::new(), options()).unwrap();
    let seq = store.graph().read().last_commit_seq();
    let report = import(&store, 1, "acme/tiny");
    let changes: Vec<_> = store
        .changes_since(seq)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(changes.len(), 1, "one commit");
    let mutations = changes[0].mutations();
    assert_eq!(mutations.len(), 1);
    match &mutations[0] {
        graphersal::changes::Mutation::SetDefinition {
            kind, name, after, ..
        } => {
            assert_eq!(*kind, DefinitionKind::MODEL);
            assert_eq!(name, &report.model.identity);
            let model = after.as_ref().unwrap().as_model().unwrap().clone();
            assert_eq!(model.repo(), "acme/tiny");
            assert_eq!(model.blob().length(), report.model.bytes);
        }
        other => panic!("{other:?}"),
    }
    // The same identity again: no commit.
    assert!(import(&store, 1, "acme/tiny").already_present);
    assert_eq!(store.graph().read().last_commit_seq(), seq + 1);
    assert_eq!(kinds(&store, "model.imported"), 1);
}

#[test]
fn another_revision_is_refused_without_replace_and_switches_with_it() {
    let temp = Temp::new("replace");
    let path = temp.join("s");
    let store = Store::create(&path, options()).unwrap();
    let old = import(&store, 1, "acme/tiny").model;
    store.set_default_model(Some("acme/tiny")).unwrap();
    let refused = store.import_model(
        ModelSource::Files(tiny_model(2, "acme/tiny")),
        ImportModelOptions::new(),
    );
    assert_eq!(cause_of(refused), StoreFailure::ModelRevisionExists);
    assert_eq!(model_names(&store), [old.identity.clone()]);
    let seq = store.graph().read().last_commit_seq();
    let report = store
        .import_model(
            ModelSource::Files(tiny_model(2, "acme/tiny")),
            ImportModelOptions::new().with_replace(true),
        )
        .unwrap();
    assert_eq!(report.commit_seq, Some(seq + 1), "one commit");
    assert_eq!(report.replaced.as_ref().unwrap().identity, old.identity);
    // The default follows the replace; the old definition is gone, its blob kept.
    assert!(report.model.default);
    assert_eq!(model_names(&store), [report.model.identity.clone()]);
    assert_eq!(
        store.info().unwrap().default_model.as_deref(),
        Some("acme/tiny")
    );
    assert_eq!(store.info().unwrap().blobs.count, 2);
    assert_eq!(kinds(&store, "model.replaced"), 1);
    // Prunes of the history before the replace drop the old blob (the snapshot before the
    // newest is kept as the donor of the newest, so it takes two checkpoints).
    let mut removed = 0;
    for round in 0..3 {
        add_person(&store, &format!("p{round}"));
        store.checkpoint(None).unwrap();
        let seq = store.graph().read().last_commit_seq();
        removed += store.prune(seq).unwrap().blobs_removed;
    }
    assert_eq!(removed, 1);
    assert_eq!(store.info().unwrap().blobs.count, 1);
}

#[test]
fn a_replace_while_vectors_use_the_old_revision_needs_vector_rules() {
    let store = Store::create(graphersal::persist::MemDir::new(), options()).unwrap();
    let old = import(&store, 1, "acme/tiny").model;
    set_schema(&store, &old.identity);
    let refused = store.import_model(
        ModelSource::Files(tiny_model(2, "acme/tiny")),
        ImportModelOptions::new().with_replace(true),
    );
    match refused {
        Err(PersistError::Graph(err)) => {
            assert!(err.to_string().contains("vector rules"), "{err}")
        }
        other => panic!("{other:?}"),
    }
    // Without replace the message names the property the old revision made.
    match store.import_model(
        ModelSource::Files(tiny_model(2, "acme/tiny")),
        ImportModelOptions::new(),
    ) {
        Err(err) => assert!(err.to_string().contains("doc.emb"), "{err}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn at_most_one_model_is_the_default() {
    let store = Store::create(graphersal::persist::MemDir::new(), options()).unwrap();
    import(&store, 1, "acme/a");
    store
        .import_model(
            ModelSource::Files(tiny_model(2, "acme/b")),
            ImportModelOptions::new().with_default(true),
        )
        .unwrap();
    store.set_default_model(Some("acme/a")).unwrap();
    let defaults: Vec<String> = store
        .models()
        .unwrap()
        .into_iter()
        .filter(|m| m.default)
        .map(|m| m.repo)
        .collect();
    assert_eq!(defaults, ["acme/a"]);
    // Setting the same default again commits nothing; clearing it does.
    assert_eq!(
        store.set_default_model(Some("acme/a")).unwrap().commit_seq,
        None
    );
    assert!(store.set_default_model(None).unwrap().commit_seq.is_some());
    assert!(store.models().unwrap().iter().all(|m| !m.default));
    // The catalog itself refuses a second default and a second revision of a repository.
    store.set_default_model(Some("acme/a")).unwrap();
    let b = store
        .graph()
        .read()
        .definitions()
        .unwrap()
        .find(|d| d.as_model().is_some_and(|m| m.repo() == "acme/b"))
        .unwrap()
        .clone();
    let second_default =
        Definition::model(b.name(), b.as_model().unwrap().clone().with_default(true));
    assert!(
        store
            .graph()
            .write()
            .set_definition(second_default)
            .is_err()
    );
    let other_revision = Definition::model(
        "model2vec/acme/b@000000000000",
        ModelDefinition::new(b.as_model().unwrap().blob().clone(), "acme/b").with_dimension(3),
    );
    assert!(
        store
            .graph()
            .write()
            .set_definition(other_revision)
            .is_err()
    );
    assert_eq!(
        store.models().unwrap().iter().filter(|m| m.default).count(),
        1
    );
}

#[test]
fn every_position_has_the_models_and_the_default_of_its_moment() {
    let temp = Temp::new("history");
    let path = temp.join("s");
    let store = Store::create(&path, options()).unwrap();
    add_person(&store, "ann");
    store.mark("before").unwrap();
    import(&store, 1, "acme/a");
    store.mark("one").unwrap();
    import(&store, 2, "acme/b");
    store.set_default_model(Some("acme/b")).unwrap();
    // Views of the past.
    let at = |target: RecoveryTarget| {
        let recovered = store.read_only_at(target).unwrap();
        let mut names: Vec<(String, bool)> = recovered
            .graph
            .definitions()
            .unwrap()
            .filter_map(|d| d.as_model().map(|m| (m.repo().to_string(), m.is_default())))
            .collect();
        names.sort();
        names
    };
    assert!(at(RecoveryTarget::Mark("before".into())).is_empty());
    assert_eq!(
        at(RecoveryTarget::Mark("one".into())),
        [("acme/a".to_string(), false)]
    );
    assert_eq!(
        at(RecoveryTarget::Latest),
        [("acme/a".to_string(), false), ("acme/b".to_string(), true)]
    );
    // A rollback to the mark: that moment's models, no default.
    store
        .rollback_to(RecoveryTarget::Mark("one".into()), RollbackMode::Move)
        .unwrap();
    let models = store.models().unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].repo, "acme/a");
    assert_eq!(store.info().unwrap().default_model, None);
    assert_eq!(
        store.embedders().unwrap().default_embedder().info().name,
        "hash-ngram/1"
    );
    // The registry follows the rollback: acme/b is no embedder any more.
    assert!(store.embedders().unwrap().get("acme/b").is_none());
    assert!(store.embedders().unwrap().get("acme/a").is_some());
    store.close().unwrap();
    let store = Store::open(&path, options()).unwrap();
    assert_eq!(store.models().unwrap().len(), 1);
}

#[test]
fn a_model_definition_of_a_newer_version_is_kept_and_written_back() {
    let temp = Temp::new("forward");
    let path = temp.join("s");
    let store = Store::create(&path, options()).unwrap();
    let report = import(&store, 1, "acme/tiny");
    let definition = store
        .graph()
        .read()
        .definition(DefinitionKind::MODEL, &report.model.identity)
        .unwrap()
        .unwrap()
        .clone();
    let mut payload = definition.as_model().unwrap().to_payload();
    payload.insert("future_key", ElementProperty::from(42i64));
    let known = ModelDefinition::from_payload(&payload).unwrap();
    payload.insert("family", ElementProperty::from("future-family"));
    payload.insert("repo", ElementProperty::from("acme/future"));
    let foreign = ModelDefinition::from_payload(&payload).unwrap();
    assert!(!foreign.served());
    {
        let mut graph = store.graph().write();
        graph
            .set_definition(Definition::model(&report.model.identity, known.clone()))
            .unwrap();
        graph
            .set_definition(Definition::model("future/model@1", foreign.clone()))
            .unwrap();
    }
    store.close().unwrap();
    let store = Store::open(&path, options()).unwrap();
    let graph = store.graph().read();
    let read_known = graph
        .definition(DefinitionKind::MODEL, &report.model.identity)
        .unwrap()
        .unwrap()
        .as_model()
        .unwrap()
        .clone();
    assert_eq!(read_known.to_payload(), known.to_payload());
    assert_eq!(
        read_known.extra().get("future_key"),
        Some(&ElementProperty::from(42i64))
    );
    let read_foreign = graph
        .definition(DefinitionKind::MODEL, "future/model@1")
        .unwrap()
        .unwrap()
        .as_model()
        .unwrap()
        .clone();
    assert_eq!(read_foreign.to_payload(), payload);
    drop(graph);
    let models = store.models().unwrap();
    let foreign_info = models
        .iter()
        .find(|m| m.identity == "future/model@1")
        .unwrap();
    assert!(matches!(foreign_info.state, ModelState::Unsupported { .. }));
    // Not served: no embedder of it.
    assert!(
        store
            .embedders()
            .unwrap()
            .by_model("future/model@1")
            .is_none()
    );
}
