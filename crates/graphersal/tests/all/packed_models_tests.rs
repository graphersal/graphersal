//! A packed snapshot (`.gsnap`) loaded WITHOUT a Store serves the embedding models it carries
//! (user decisions on Q11.6 and of 2026-10-31, book page "Embedding Models in a Store", section
//! "A `.gsnap` without a store"): `persist::read_snapshot_file` and `GraphSource::from_file`
//! record where each blob lies and read it from the file when the model loads (no copy kept), the
//! 0.4.0 model rows are converted (their tar built again from the rows at the load; the file
//! stays as it is), a changed or removed file makes the model unavailable naming the file, the
//! report lists what an earlier release left; a graph without packed models has `hash-ngram/1`
//! only.

#![cfg(all(feature = "persist", feature = "script", not(target_arch = "wasm32")))]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use graphersal::persist::{
    ModelCopy, ModelState, ReadOptions, SnapshotLoadReport, Store, read_snapshot_file,
};
use graphersal::prelude::*;
use graphersal::storage::AnyGraph;

use crate::model_fixture::{DIMENSION, Temp, import, options, set_schema};

/// `script` on `graph` (the engine's host graph).
fn run(graph: &Arc<Graph>, script: &str) -> String {
    let mut engine = graphersal::script::engine();
    graphersal::script::set_engine_graph(&mut engine, graph.clone());
    let mut scope = graphersal::script::graph_scope(graph.clone());
    engine
        .eval_with_scope::<graphersal::script::rhai::Dynamic>(&mut scope, script)
        .unwrap_or_else(|err| panic!("{script}: {err}"))
        .to_string()
}

/// The names `g.embedders()` lists, the default first.
fn embedder_names(graph: &Arc<Graph>) -> String {
    run(
        graph,
        r#"let names = ""; for e in g.embedders() { if names != "" { names += " " } names += e.name } names"#,
    )
}

/// A store at `temp/s` with the tiny model `acme/tiny` (the default), a `doc` and a schema
/// naming the model, exported (after a checkpoint) to `temp/<file>` with `models`.
fn exported(temp: &Temp, file: &str, models: ModelCopy) -> (PathBuf, String) {
    let store = Store::create(temp.join("s"), options()).unwrap();
    let identity = import(&store, 1, "acme/tiny").model.identity;
    store.set_default_model(Some("acme/tiny")).unwrap();
    set_schema(&store, &identity);
    store
        .graph()
        .write()
        .traversal_mut()
        .add_v("doc")
        .property("text", "graph vector")
        .iterate()
        .unwrap();
    store.checkpoint(None).unwrap();
    let path = temp.join(file);
    store
        .export_packed_with(None, std::fs::File::create(&path).unwrap(), models)
        .unwrap();
    store.close().unwrap();
    (path, identity)
}

fn load(path: &Path) -> (Arc<Graph>, SnapshotLoadReport) {
    let (graph, report) = read_snapshot_file(path, &ReadOptions::new()).unwrap();
    (Arc::new(graph), report)
}

#[test]
fn a_gsnap_without_a_store_serves_its_models() {
    let temp = Temp::new("packed-serves");
    let (path, identity) = exported(&temp, "with.gsnap", ModelCopy::Include);
    let (graph, report) = load(&path);
    assert_eq!(report.models.len(), 1);
    assert_eq!(report.models[0].identity, identity);
    assert_eq!(report.models[0].state, ModelState::NotLoaded);
    assert_eq!(report.default_model.as_deref(), Some("acme/tiny"));
    assert!(report.models_converted.is_empty() && report.models_missing.is_empty());
    // The only note: `emb` names the model without a source property (no vector yet).
    assert_eq!(report.vectors_without_rule.len(), 1);
    assert_eq!(report.vectors_without_rule[0].elements(), 0);
    assert_eq!(
        report.notes("with.gsnap").len(),
        1,
        "{:?}",
        report.notes("x")
    );
    // The registry serves the model, the default first; the built-in stays.
    assert_eq!(embedder_names(&graph), "acme/tiny hash-ngram/1");
    assert_eq!(
        run(&graph, r#"g.embedders()[0].model"#),
        identity,
        "the identity"
    );
    // The default model embeds (with the loader), or refuses naming the build (without).
    let mut engine = graphersal::script::engine();
    graphersal::script::set_engine_graph(&mut engine, graph.clone());
    let mut scope = graphersal::script::graph_scope(graph.clone());
    let embedded = engine.eval_with_scope::<graphersal::script::rhai::Dynamic>(
        &mut scope,
        r#"[embed("graph").dimension, g.v().embed("text").next().dimension]"#,
    );
    if cfg!(feature = "model2vec") {
        let text = embedded.unwrap().to_string();
        assert_eq!(text, format!("[{DIMENSION}, {DIMENSION}]"));
    } else {
        let err = embedded.unwrap_err().to_string();
        assert!(err.contains("no Model2Vec loader"), "{err}");
    }
    // GraphSource::from_file serves it the same way.
    let graph = Arc::new(GraphSource::from_file(&path).unwrap());
    assert_eq!(embedder_names(&graph), "acme/tiny hash-ngram/1");
}

#[test]
fn a_gsnap_without_models_has_the_builtin_only() {
    let temp = Temp::new("packed-none");
    let path = temp.join("modern.gsnap");
    GraphSource::tinkerpop_modern()
        .write_snapshot(&mut std::fs::File::create(&path).unwrap())
        .unwrap();
    let (graph, report) = load(&path);
    assert_eq!(report, SnapshotLoadReport::default());
    assert_eq!(embedder_names(&graph), "hash-ngram/1");
    assert_eq!(graph.read().vertex_count(), 6);
}

#[test]
fn an_export_without_models_lists_them_as_missing() {
    let temp = Temp::new("packed-without");
    let (path, identity) = exported(&temp, "without.gsnap", ModelCopy::Exclude);
    let (graph, report) = load(&path);
    assert_eq!(report.models.len(), 1);
    assert_eq!(report.models[0].state, ModelState::Missing);
    let notes = report.notes("without.gsnap").join("\n");
    assert!(
        notes.contains(&format!(
            "names the embedding model {identity} but does not carry it"
        )),
        "{notes}"
    );
    assert!(
        notes.contains("graphersal embed-model import acme/tiny --into <dir>"),
        "{notes}"
    );
    // The definition is listed, but its model does not run.
    let mut engine = graphersal::script::engine();
    graphersal::script::set_engine_graph(&mut engine, graph.clone());
    let mut scope = graphersal::script::graph_scope(graph.clone());
    let err = engine
        .eval_with_scope::<graphersal::script::rhai::Dynamic>(&mut scope, r#"embed("graph")"#)
        .unwrap_err()
        .to_string();
    assert!(err.contains("unavailable"), "{err}");
}

#[test]
fn a_store_with_models_but_no_default_gives_the_note() {
    let temp = Temp::new("packed-no-default");
    let store = Store::create(temp.join("s"), options()).unwrap();
    import(&store, 1, "acme/tiny");
    store.checkpoint(None).unwrap();
    let path = temp.join("x.gsnap");
    store
        .export_packed(None, std::fs::File::create(&path).unwrap())
        .unwrap();
    store.close().unwrap();
    let (graph, report) = load(&path);
    assert_eq!(report.default_model, None);
    let notes = report.notes("x.gsnap").join("\n");
    assert!(notes.contains("but none is the default"), "{notes}");
    assert_eq!(embedder_names(&graph), "hash-ngram/1 acme/tiny");
}

/// The 0.4.0 fixtures (`tests/data/v0.4.0`, written by the released 0.4.0 binary).
fn v040(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data/v0.4.0")
        .join(name)
}

#[test]
fn the_model_rows_of_a_040_gsnap_are_converted_in_memory() {
    const TINY: &str = "model2vec/acme/tiny@70c61d0ed332";
    const OTHER_040: &str = "model2vec@b6b362e7a651";
    const OTHER: &str = "model2vec/local/b6b362e7a651@b6b362e7a651";
    let path = v040("snapshot.gsnap");
    let before = std::fs::read(&path).unwrap();
    let (graph, report) = load(&path);
    let mut converted: Vec<(String, String, bool)> = report
        .models_converted
        .iter()
        .map(|c| {
            (
                c.identity().to_string(),
                c.former_identity().to_string(),
                c.renamed(),
            )
        })
        .collect();
    converted.sort();
    assert_eq!(
        converted,
        [
            (TINY.to_string(), TINY.to_string(), false),
            (OTHER.to_string(), OTHER_040.to_string(), true),
        ]
    );
    let mut identities: Vec<&str> = report.models.iter().map(|m| m.identity.as_str()).collect();
    identities.sort();
    assert_eq!(identities, [TINY, OTHER]);
    // The schema names the renamed identity, as the store migration does.
    let schema = graph.read().get_schema().unwrap().unwrap();
    let text = schema.to_json();
    assert!(
        text.contains(OTHER) && !text.contains(&format!("\"{OTHER_040}\"")),
        "{text}"
    );
    // Both are served; the vectors 0.4.0 wrote stay as they are.
    let names = embedder_names(&graph);
    assert!(
        names.contains("acme/tiny") && names.contains("local/b6b362e7a651"),
        "{names}"
    );
    // The notes: the renamed model, the vectors without a rule.
    let notes = report.notes("snapshot.gsnap").join("\n");
    assert!(notes.contains(&format!("is served as {OTHER}")), "{notes}");
    assert!(!report.vectors_without_rule.is_empty());
    let doc = report
        .vectors_without_rule
        .iter()
        .find(|v| v.label() == "doc")
        .unwrap();
    assert_eq!(doc.elements(), 3);
    assert!(doc.exact());
    // The file is never written.
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn a_040_gsnap_names_the_saved_queries_calling_a_removed_function() {
    let (_, report) = load(&v040("saved_queries.gsnap"));
    let mut names: Vec<&str> = report
        .saved_queries_using_removed
        .iter()
        .map(|u| u.query.as_str())
        .collect();
    names.sort();
    assert_eq!(names, ["fill", "fill_any"]);
    let notes = report.notes("saved_queries.gsnap").join("\n");
    assert!(notes.contains("saved query \"fill\" calls"), "{notes}");
}

/// `embed(text)` with `embedder` on `graph`: the dimension, or the error text.
#[cfg(feature = "model2vec")]
fn embed_on(graph: &Arc<Graph>, embedder: &str) -> Result<i64, String> {
    let mut engine = graphersal::script::engine();
    graphersal::script::set_engine_graph(&mut engine, graph.clone());
    let mut scope = graphersal::script::graph_scope(graph.clone());
    engine
        .eval_with_scope::<graphersal::script::rhai::INT>(
            &mut scope,
            &format!(r#"embed("graph", #{{embedder: "{embedder}"}}).dimension"#),
        )
        .map_err(|err| err.to_string())
}

/// The blob stays in the file: a model loads from the file at its first use, so a file removed,
/// truncated or replaced after the load (before the model's first use) makes the model
/// unavailable, naming the file; nothing was kept in memory to load it from.
#[cfg(feature = "model2vec")]
#[test]
fn a_model_loads_from_the_file_and_a_changed_file_is_refused() {
    let temp = Temp::new("packed-file-reads");
    // Different seeds per case: the process-wide cache of loaded models never serves one of them
    // from an earlier load (a cached model is still checked against the file, see below).
    type Damage<'a> = &'a dyn Fn(&Path);
    let cases: [(&str, Damage); 3] = [
        ("removed", &|path| std::fs::remove_file(path).unwrap()),
        ("truncated", &|path| {
            let bytes = std::fs::read(path).unwrap();
            std::fs::write(path, &bytes[..bytes.len() / 2]).unwrap();
        }),
        ("replaced", &|path| {
            GraphSource::tinkerpop_modern()
                .write_snapshot(&mut std::fs::File::create(path).unwrap())
                .unwrap();
        }),
    ];
    for (seed, (case, change)) in (11u8..).zip(cases) {
        let dir = Temp::new(case);
        let store = Store::create(dir.join("s"), options()).unwrap();
        import(&store, seed, "acme/tiny");
        store.checkpoint(None).unwrap();
        let path = dir.join("m.gsnap");
        store
            .export_packed(None, std::fs::File::create(&path).unwrap())
            .unwrap();
        store.close().unwrap();
        let (graph, _) = load(&path);
        change(&path);
        let err = embed_on(&graph, "acme/tiny").unwrap_err();
        assert!(err.contains("unavailable"), "{case}: {err}");
        assert!(
            err.contains(&path.display().to_string()) || case == "removed",
            "{case}: {err}"
        );
        assert!(
            err.contains("changed after it was loaded") || err.contains("cannot be read"),
            "{case}: {err}"
        );
        // The graph itself is unaffected.
        assert_eq!(graph.read().vertex_count(), 0);
    }
    // An intact file serves the model; a model loaded once stays loaded.
    let (path, _) = exported(&temp, "ok.gsnap", ModelCopy::Include);
    let (graph, _) = load(&path);
    assert_eq!(embed_on(&graph, "acme/tiny"), Ok(DIMENSION as i64));
    std::fs::remove_file(&path).unwrap();
    assert_eq!(embed_on(&graph, "acme/tiny"), Ok(DIMENSION as i64));
    // A second graph of the same (now removed) file content: the cached model is checked
    // against its own file first, which is gone.
    let again = Temp::new("packed-file-cache");
    let (path, _) = exported(&again, "ok.gsnap", ModelCopy::Include);
    let (other, _) = load(&path);
    std::fs::remove_file(&path).unwrap();
    let err = embed_on(&other, "acme/tiny").unwrap_err();
    assert!(err.contains("unavailable"), "{err}");
}

/// A converted 0.4.0 model loads from the file's model rows (its tar built again, checked by
/// its hash) and stays unavailable once the file is gone.
#[cfg(feature = "model2vec")]
#[test]
fn a_converted_040_model_loads_from_the_rows() {
    let temp = Temp::new("packed-040-rows");
    let path = temp.join("snapshot.gsnap");
    std::fs::copy(v040("snapshot.gsnap"), &path).unwrap();
    let (graph, report) = load(&path);
    assert_eq!(report.models_converted.len(), 2);
    assert_eq!(embed_on(&graph, "acme/tiny"), Ok(DIMENSION as i64));
    let (other, _) = load(&path);
    std::fs::remove_file(&path).unwrap();
    let err = embed_on(&other, "local/b6b362e7a651").unwrap_err();
    assert!(err.contains("unavailable"), "{err}");
}

/// `Store::create_from_packed_dir` (CLI `store create <dir> --from file.gsnap`) makes the store
/// WITHOUT opening it: a file exported without the model its schema needs still becomes a
/// store, the report names the model (with the commands), and the store opens once the model
/// is imported into it (as with a 0.4.0 file whose model 0.4.0 served through `--embedder`).
#[test]
fn a_store_made_from_a_gsnap_lacking_its_model_names_it_and_opens_after_the_import() {
    let temp = Temp::new("packed-create-without");
    let (path, identity) = exported(&temp, "without.gsnap", ModelCopy::Exclude);
    let dir = temp.join("made");
    let report =
        Store::create_from_packed_dir(&dir, std::fs::File::open(&path).unwrap(), options())
            .unwrap();
    assert_eq!(
        report.from_version,
        graphersal::persist::STORE_FORMAT_VERSION
    );
    assert!(report.models_converted.is_empty() && report.migration_commit.is_none());
    assert_eq!(
        report.models_missing.len(),
        1,
        "{:?}",
        report.models_missing
    );
    let missing = &report.models_missing[0];
    assert_eq!(missing.identity(), identity);
    assert_eq!(missing.needed_by(), ["doc.emb"]);
    assert_eq!(
        missing.commands("made"),
        [
            "graphersal embed-model download acme/tiny",
            "graphersal embed-model import acme/tiny --into made",
        ]
    );
    // `doc.emb` names the model without a source property; no element holds a vector.
    assert_eq!(report.vectors_without_rule.len(), 1);
    let note = report.vectors_without_rule[0].note("made");
    assert!(
        note.starts_with(
            "doc.emb is declared with vectors of acme/tiny but has no source property\n  \
             vectors: no element holds one"
        ),
        "{note}"
    );
    // The store is there, closed cleanly, and refuses to open while the model is missing.
    assert!(Store::inspect(&dir).unwrap().clean);
    let refused = Store::open(&dir, options()).expect_err("the model is missing");
    assert!(refused.to_string().contains("acme/tiny"), "{refused}");
    // The import brings it back: the store opens and serves it.
    Store::import_model_dir(
        &dir,
        graphersal::persist::ModelSource::Files(crate::model_fixture::tiny_model(1, "acme/tiny")),
        graphersal::persist::ImportModelOptions::new(),
    )
    .unwrap();
    let store = Store::open(&dir, options()).unwrap();
    assert_eq!(store.graph().read().vertex_count(), 1);
    store.close().unwrap();
}

/// An open store that defines a model without its blob (made from an export without models)
/// takes the blob back by the import of the same model (it was a silent no-op before).
#[test]
fn an_open_store_takes_back_the_blob_of_a_model_it_defines() {
    let temp = Temp::new("packed-restore-open");
    let store = Store::create(temp.join("s"), options()).unwrap();
    import(&store, 1, "acme/tiny");
    store.checkpoint(None).unwrap();
    let path = temp.join("bare.gsnap");
    store
        .export_packed_with(
            None,
            std::fs::File::create(&path).unwrap(),
            ModelCopy::Exclude,
        )
        .unwrap();
    store.close().unwrap();
    let made = Store::create_from_packed(
        temp.join("made"),
        std::fs::File::open(&path).unwrap(),
        options(),
    )
    .unwrap();
    assert_eq!(made.models().unwrap()[0].state, ModelState::Missing);
    let report = import(&made, 1, "acme/tiny");
    assert!(!report.already_present);
    let state = made.models().unwrap()[0].state.clone();
    assert!(
        !matches!(state, ModelState::Missing | ModelState::Damaged { .. }),
        "{state:?}"
    );
    made.close().unwrap();
}

/// A store made from a 0.4.0 `.gsnap` without opening it reports what the store upgrade
/// reports: the converted models and their commit, the vectors without a rule (counted).
#[test]
fn a_store_made_from_a_040_gsnap_reports_the_migration() {
    let temp = Temp::new("packed-create-040");
    let dir = temp.join("made");
    let report = Store::create_from_packed_dir(
        &dir,
        std::fs::File::open(v040("snapshot.gsnap")).unwrap(),
        options(),
    )
    .unwrap();
    assert_eq!(report.from_version, 3);
    assert_eq!(report.models_converted.len(), 2);
    assert!(report.migration_commit.is_some());
    assert!(
        report.models_missing.is_empty(),
        "{:?}",
        report.models_missing
    );
    let doc = report
        .vectors_without_rule
        .iter()
        .find(|v| v.label() == "doc")
        .unwrap();
    assert_eq!(doc.elements(), 3);
    assert!(doc.exact());
    let store = Store::open(&dir, options()).unwrap();
    assert_eq!(store.models().unwrap().len(), 2);
    store.close().unwrap();
}

/// The notes of a `.gsnap` loaded without a store run their schema patch in a store: a schema
/// change of the loaded file stays in memory.
#[test]
fn the_gsnap_notes_put_the_graph_into_a_store_first() {
    let (_, report) = load(&v040("snapshot.gsnap"));
    let notes = report.notes("snapshot.gsnap").join("\n");
    assert!(
        notes.contains(
            "graphersal store create <dir> --from snapshot.gsnap\n  # add the source property \
             (replace <source property> first)\n  graphersal --graph <dir> -e \
             'g.patch_schema(#{vertices: #{doc:"
        ),
        "{notes}"
    );
    assert!(
        !notes.contains("graphersal --graph snapshot.gsnap"),
        "{notes}"
    );
    let (_, report) = load(&v040("saved_queries.gsnap"));
    let notes = report.notes("saved_queries.gsnap").join("\n");
    assert!(
        notes.contains("graphersal store create <dir> --from saved_queries.gsnap"),
        "{notes}"
    );
    assert!(
        !notes.contains("graphersal --graph saved_queries.gsnap"),
        "{notes}"
    );
}
