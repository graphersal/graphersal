//! Tests of the launch ([`crate::launch`], one per [`crate::ServedSource`] kind) and of the
//! open functions ([`crate::open_store`], [`crate::layer_base_of_store`],
//! [`crate::open_layer_store`]): real session threads on the sample graphs, stores in memory
//! (`MemDir`) and graph files in a temporary directory.

use std::path::PathBuf;
use std::sync::Arc;

use graphersal::persist::{DamagePolicy, MemDir, Store, StoreOptions};
use graphersal::prelude::{Graph, GraphSource, GraphStorage, TraversalGraph};
use graphersal::storage::LayerBase;
use graphersal_session::{RunOptions, Session, SessionPresets};
use serde_json::Value;

use crate::{
    Context, Exposure, HostCheck, Launched, NoteLevel, OpenErrorKind, OpenStore, ServeError,
    ServedSource, launch, layer_base_of_store, open_layer_store, open_store,
};

fn check() -> HostCheck {
    HostCheck::new(Exposure::Loopback, 8080, "http://127.0.0.1:8080/")
}

/// Launches `source` without extra setup.
fn launched(source: ServedSource) -> Result<Launched, ServeError> {
    launch(
        source,
        SessionPresets::new(),
        |_: &mut Session| Ok::<_, String>(()),
        None,
    )
    .map(|(launched, ())| launched)
}

/// The context of a launch, built from its pre-filled builder.
fn context_of(launched: &Launched) -> Arc<Context> {
    launched.context_builder(check()).build()
}

/// The graph info of the session behind `context`.
fn info(context: &Context) -> Value {
    context
        .call(|session| session.info().map_err(String::from))
        .expect("the session answers")
        .expect("the info")
}

/// Runs `script` in the session behind `context` and answers its JSON.
fn run(context: &Context, script: &'static str) -> Value {
    let text = context
        .call(move |session| session.execute(script, &RunOptions::default()))
        .expect("the session answers");
    let answer: Value = serde_json::from_str(&text).expect("a JSON answer");
    assert!(answer.get("error").is_none_or(Value::is_null), "{answer}");
    answer
}

/// A store in memory holding the modern sample (closed).
fn modern_store(name: &str) -> MemDir {
    let dir = MemDir::named(name);
    Store::create_with(
        &dir,
        TraversalGraph::tinkerpop_modern(),
        StoreOptions::new(),
    )
    .expect("the store is created")
    .close()
    .expect("the store closes");
    dir
}

/// A temporary directory of this test process.
fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "graphersal_serve_launch_{}_{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("the temporary directory");
    dir
}

#[test]
fn a_sample_launches_with_its_title_and_a_filled_builder() {
    let presets = SessionPresets::new().with_script_timeout_ms(Some(5_000));
    let expected_presets = presets.to_value();
    let (launched, answer) = launch(
        ServedSource::Sample("modern".to_string()),
        presets,
        |session: &mut Session| Ok::<_, String>(session.graph().vertex_count().unwrap_or(0)),
        None,
    )
    .expect("the launch");
    // The extra setup ran on the loaded graph.
    assert_eq!(answer, 6);
    assert!(!launched.layered);
    assert!(launched.store.is_none() && launched.layer_store.is_none());
    assert!(launched.warnings.is_empty());
    assert_eq!(launched.presets, expected_presets);
    assert!(launched.statistics.is_object(), "{}", launched.statistics);
    let context = context_of(&launched);
    let info = info(&context);
    assert_eq!(info["name"], "Modern (TinkerPop)");
    assert_eq!(info["kind"], "modern");
    assert_eq!(info["vertexCount"], 6);
    // A write moves the change feed from the launch's commit position.
    let before = launched.commit_seq;
    run(&context, r#"g.add_v("person").iterate()"#);
    let after = context
        .call(|session| session.graph().last_commit_seq().unwrap_or(0))
        .expect("the session answers");
    assert!(after > before, "{before} -> {after}");
}

#[test]
fn an_unknown_sample_and_a_failing_extra_setup_are_setup_errors() {
    let Err(err) = launched(ServedSource::Sample("nope".to_string())) else {
        panic!("refused");
    };
    assert!(matches!(err, ServeError::Setup(_)), "{err:?}");
    assert_eq!(err.to_string(), "Unknown sample graph: nope");
    let Err(err) = launch(
        ServedSource::Sample("empty".to_string()),
        SessionPresets::new(),
        |_: &mut Session| Err::<(), _>("the host's setup failed".to_string()),
        None,
    ) else {
        panic!("refused");
    };
    assert!(matches!(err, ServeError::Setup(_)), "{err:?}");
    assert_eq!(err.to_string(), "the host's setup failed");
}

#[test]
fn graph_files_load_by_their_extension_with_the_import_warnings() {
    let dir = temp_dir("files");
    let mut session = Session::new();
    session.load_sample("modern").expect("the sample");
    // A packed snapshot.
    let gsnap = dir.join("modern.gsnap");
    std::fs::write(&gsnap, session.save_snapshot().expect("a snapshot")).expect("written");
    let launched_gsnap = launched(ServedSource::File(gsnap)).expect("the launch");
    assert!(
        launched_gsnap.warnings.is_empty(),
        "{:?}",
        launched_gsnap.warnings
    );
    let info_gsnap = info(&context_of(&launched_gsnap));
    assert_eq!(info_gsnap["name"], "modern.gsnap");
    assert_eq!(info_gsnap["kind"], "file");
    assert_eq!(info_gsnap["vertexCount"], 6);
    // GraphSON with its first vertex line twice: the second one is skipped and reported (a
    // dangling edge fails the load since 0.10.0, so it no longer serves as the warning).
    let text = session.save_graphson().expect("GraphSON")["text"]
        .as_str()
        .expect("the text")
        .to_string();
    let mut lines: Vec<&str> = text.lines().collect();
    lines.push(lines[0]);
    let graphson = dir.join("modern.json");
    std::fs::write(&graphson, lines.join("\n")).expect("written");
    let launched_json = launched(ServedSource::File(graphson)).expect("the launch");
    assert!(
        !launched_json.warnings.is_empty(),
        "the skipped duplicate is reported"
    );
    let info_json = info(&context_of(&launched_json));
    assert_eq!(info_json["name"], "modern.json");
    assert_eq!(info_json["vertexCount"], 6);
    // A file that cannot be read.
    let Err(err) = launched(ServedSource::File(dir.join("missing.json"))) else {
        panic!("refused");
    };
    assert!(matches!(err, ServeError::Setup(_)), "{err:?}");
    assert!(err.to_string().starts_with("Error: cannot read '"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_store_is_attached_as_the_sessions_database() {
    let dir = modern_store("launch-store");
    let opened = open_store(&dir, OpenStore::new()).expect("the store opens");
    assert!(opened.notes.is_empty(), "{:?}", opened.notes);
    assert!(opened.models.is_empty());
    let store = opened.store;
    let commit = store.open_report().commit_seq;
    let launched = launched(ServedSource::Store(store.clone())).expect("the launch");
    assert!(Arc::ptr_eq(launched.store.as_ref().expect("kept"), &store));
    assert_eq!(launched.commit_seq, commit);
    let context = context_of(&launched);
    // The builder carries the Store (the Store panel's endpoints serve it).
    assert!(Arc::ptr_eq(
        context.store().expect("the context's store"),
        &store
    ));
    let info = info(&context);
    assert_eq!(info["kind"], "store");
    assert_eq!(info["name"], store.location());
    assert_eq!(info["vertexCount"], 6);
    let has_store = context
        .call(|session| session.has_store())
        .expect("the session answers");
    assert!(has_store);
}

#[test]
fn a_host_graph_is_served_with_its_title_and_kind() {
    let graph: Arc<Graph> = Arc::new(GraphSource::tinkerpop_modern());
    let launched = launched(ServedSource::graph(graph, "mine.json", "file")).expect("the launch");
    assert!(!launched.layered);
    let info = info(&context_of(&launched));
    assert_eq!(info["name"], "mine.json");
    assert_eq!(info["kind"], "file");
    assert_eq!(info["vertexCount"], 6);
}

#[test]
fn a_base_served_as_an_empty_layer_is_a_layer_and_never_written() {
    let base = LayerBase::from_graph(GraphSource::tinkerpop_modern()).expect("a base");
    let layer = Arc::new(base.new_layer().expect("a layer"));
    let launched = launched(ServedSource::graph(layer, "modern", "modern")).expect("the launch");
    assert!(launched.layered);
    let context = context_of(&launched);
    run(&context, r#"g.add_v("person").iterate()"#);
    assert_eq!(info(&context)["vertexCount"], 7);
    assert_eq!(
        base.graph().vertex_count().ok(),
        Some(6),
        "the base never changes"
    );
}

#[test]
fn a_layer_store_over_a_stores_base_is_served_and_kept_for_its_close() {
    let base_dir = modern_store("launch-base");
    let base = layer_base_of_store(&base_dir).expect("the base");
    assert_eq!(base.base.name(), "launch-base");
    assert!(base.models.is_empty() && base.notes.is_empty());
    let layer_dir = MemDir::named("launch-layer");
    let opened = open_layer_store(&layer_dir, &base.base, OpenStore::new().create(true))
        .expect("the layer store is created");
    let created = &opened.notes[0];
    assert_eq!(created.level, NoteLevel::Note);
    assert_eq!(created.headline, "created the layer store launch-layer");
    let names: Vec<&str> = created.details.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(names, ["base", "base identity", "damage policy"]);
    let store = opened.store;
    let launched = launched(ServedSource::layer_store(
        store.clone(),
        "launch-base",
        "store",
    ))
    .expect("the launch");
    assert!(launched.layered);
    assert!(
        launched.store.is_none(),
        "a layer store is not the session's Store"
    );
    assert!(Arc::ptr_eq(
        launched.layer_store.as_ref().expect("kept"),
        &store
    ));
    let context = context_of(&launched);
    assert!(context.store().is_none());
    let info = info(&context);
    assert_eq!(info["name"], "launch-base");
    assert_eq!(info["kind"], "store");
    run(&context, r#"g.add_v("person").iterate()"#);
    // The commit is the layer store's; the base stays as it was.
    assert_eq!(store.graph().read().vertex_count().unwrap_or(0), 7);
    assert_eq!(base.base.graph().vertex_count().ok(), Some(6));
}

#[test]
fn a_layer_store_opens_over_a_given_base_and_refuses_another() {
    let base = LayerBase::from_graph(GraphSource::tinkerpop_modern()).expect("a base");
    let dir = MemDir::named("given-base-layer");
    // Not a store yet, and not asked to create one.
    let Err(err) = open_layer_store(&dir, &base, OpenStore::new()) else {
        panic!("refused");
    };
    let ServeError::Open(open) = &err else {
        panic!("an open error: {err:?}");
    };
    assert_eq!(open.kind(), OpenErrorKind::NotAStore);
    let opened = open_layer_store(&dir, &base, OpenStore::new().create(true)).expect("created");
    let Ok(store) = Arc::try_unwrap(opened.store) else {
        panic!("the only handle");
    };
    store.close().expect("closed");
    // Over the same base again: a clean open, nothing to say.
    let opened = open_layer_store(&dir, &base, OpenStore::new()).expect("reopened");
    assert!(opened.notes.is_empty(), "{:?}", opened.notes);
    drop(opened);
    // Over another base: refused with the engine's error.
    let other = LayerBase::from_graph(GraphSource::empty()).expect("another base");
    let Err(err) = open_layer_store(&dir, &other, OpenStore::new()) else {
        panic!("refused");
    };
    let ServeError::Open(open) = &err else {
        panic!("an open error: {err:?}");
    };
    assert_eq!(open.kind(), OpenErrorKind::Failed);
    assert!(open.persist_error().is_some());
    assert!(std::error::Error::source(&err).is_some());
}

#[test]
fn open_store_answers_its_notes_as_data() {
    let dir = MemDir::named("open-notes");
    // Not a store, not asked to create one.
    let Err(err) = open_store(&dir, OpenStore::new()) else {
        panic!("refused");
    };
    let ServeError::Open(open) = &err else {
        panic!("an open error: {err:?}");
    };
    assert_eq!(open.kind(), OpenErrorKind::NotAStore);
    assert!(
        err.to_string().contains("is not a Graphersal store"),
        "{err}"
    );
    // Created with a damage policy: one note naming it.
    let options = OpenStore::new()
        .create(true)
        .on_damage(DamagePolicy::Continue);
    assert!(options.creates());
    let opened = open_store(&dir, options).expect("created");
    assert_eq!(opened.notes.len(), 1, "{:?}", opened.notes);
    assert_eq!(opened.notes[0].headline, "created the store open-notes");
    assert_eq!(
        opened.notes[0].details,
        [("damage policy".to_string(), "continue".to_string())]
    );
    // A backup of it opens read-only, with a note and the commands that help.
    let backup = MemDir::named("open-notes-backup");
    opened.store.backup(&backup).expect("a backup");
    let Ok(store) = Arc::try_unwrap(opened.store) else {
        panic!("the only handle");
    };
    store.close().expect("closed");
    let restored = open_store(&backup, OpenStore::new()).expect("the backup opens");
    assert!(restored.store.backup_marker().is_some());
    let note = &restored.notes[0];
    assert_eq!(
        note.headline,
        "open-notes-backup is a BACKUP: opened READ-ONLY, writes are refused"
    );
    assert_eq!(note.commands.len(), 2);
    assert_eq!(
        note.commands[0].1,
        "graphersal store restore open-notes-backup"
    );
    // The damage policy is fixed at creation: another one is refused, none asked opens.
    let Err(err) = open_store(&dir, OpenStore::new().on_damage(DamagePolicy::Maintenance)) else {
        panic!("refused");
    };
    let ServeError::Open(open) = &err else {
        panic!("an open error: {err:?}");
    };
    assert_eq!(open.kind(), OpenErrorKind::DamagePolicyMismatch);
    assert!(
        open.message().contains("\"continue\""),
        "{}",
        open.message()
    );
    let opened = open_store(&dir, OpenStore::new().on_damage(DamagePolicy::Continue))
        .expect("the same policy opens");
    assert!(opened.notes.is_empty(), "{:?}", opened.notes);
    // The HTTP answer of an open failure is a 500 with its text.
    let http = crate::HttpError::from(err);
    assert_eq!(http.status(), 500);
}

#[test]
fn a_base_needs_a_store() {
    let dir = MemDir::named("no-base");
    let Err(err) = layer_base_of_store(&dir) else {
        panic!("refused");
    };
    let ServeError::Open(open) = &err else {
        panic!("an open error: {err:?}");
    };
    assert_eq!(open.kind(), OpenErrorKind::NotAStore);
}
