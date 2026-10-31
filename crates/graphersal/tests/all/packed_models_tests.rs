//! A packed snapshot (`.gsnap`) loaded WITHOUT a Store serves the embedding models it carries
//! (user decision on Q11.6, book page "Embedding Models in a Store", section "A `.gsnap`
//! without a store"): `persist::read_snapshot_file` and `GraphSource::from_file` keep the
//! packed blobs in memory, the 0.4.0 model rows are converted in memory (the file stays as it
//! is), the report lists what an earlier release left; a graph without packed models has
//! `hash-ngram/1` only.

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
