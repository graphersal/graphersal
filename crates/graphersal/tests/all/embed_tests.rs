//! The `embed` step and the function `embed(text)` with the built-in `hash-ngram/1` (book page
//! "Vectors", section "Embeddings"): the key form, the profile, a schema-declared target, an
//! unknown embedder and a value that is not text. The frozen `hash-ngram/1` bits and the cases
//! that need a mock embedder (batching, bulk, rollback, wrong answers, cancellation, replay) are
//! crate unit tests (`value/vector/embed/hash_ngram.rs`, `value/vector/embed/tests.rs`): 0.5.0
//! made the embedder API crate-internal. Database models: `store_models_tests.rs`,
//! `model_open_tests.rs`, `packed_models_tests.rs`.

#![cfg(feature = "script")]

use std::sync::Arc;

use graphersal::prelude::*;
use graphersal::script::rhai::{Dynamic, EvalAltResult};

// ---------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------

/// Runs `script` on `graph`, the engine's host graph.
fn run(graph: &Arc<Graph>, script: &str) -> Result<Dynamic, Box<EvalAltResult>> {
    let mut engine = graphersal::script::engine();
    graphersal::script::set_engine_graph(&mut engine, graph.clone());
    let mut scope = graphersal::script::graph_scope(graph.clone());
    engine.eval_with_scope::<Dynamic>(&mut scope, script)
}

fn run_ok(graph: &Arc<Graph>, script: &str) -> Dynamic {
    run(graph, script).unwrap_or_else(|err| panic!("{script}: {err}"))
}

fn run_err(graph: &Arc<Graph>, script: &str) -> String {
    match run(graph, script) {
        Ok(value) => panic!("{script} should fail, gave {value:?}"),
        Err(err) => err.to_string(),
    }
}

fn modern() -> Arc<Graph> {
    Arc::new(GraphSource::tinkerpop_modern())
}

// ---------------------------------------------------------------------------------------------
// The step and the function
// ---------------------------------------------------------------------------------------------

#[test]
fn the_key_form_reads_the_property_and_drops_elements_without_it() {
    let graph = modern();
    // `lang` exists on the two software vertices only.
    let count = run_ok(&graph, r#"g.v().embed("lang").count().next()"#);
    assert_eq!(count.as_int().unwrap(), 2);
    // The key form equals values(key).embed() and the function embed(text).
    let same = run_ok(
        &graph,
        r#"let a = g.v().has("name", "josh").embed("name").next();
           let b = g.v().has("name", "josh").values("name").embed().next();
           a == b && a == embed("josh")"#,
    );
    assert!(same.as_bool().unwrap());
    // A jpath key.
    let nested = run_ok(
        &graph,
        r#"g.inject(#{doc: #{title: "Graph"}}).embed(jpath("doc.title")).next() == embed("graph")"#,
    );
    assert!(nested.as_bool().unwrap());
    // The Rust API.
    let g = graph.read();
    let vectors = g
        .traversal()
        .v(None::<()>)
        .embed("name", Some("hash-ngram/1"))
        .to_list()
        .unwrap();
    assert_eq!(vectors.len(), 6);
    assert!(vectors.iter().all(|v| v.as_vector().is_some()));
}

#[test]
fn the_profile_names_the_resolved_embedder() {
    let profile = run_ok(
        &modern(),
        r#"g.v().has_label("person").embed("name").profile().to_string()"#,
    );
    let text = profile.into_string().unwrap();
    assert!(
        text.contains(r#"embed("name", embedder: "hash-ngram/1")"#),
        "{text}"
    );
}

#[test]
fn a_schema_declared_target_checks_the_embedding() {
    let graph = modern();
    let _ = run_ok(
        &graph,
        r#"g.set_schema(#{mode: "open", vertices: #{person: #{schema: #{type: "object",
             properties: #{emb: #{type: "array", format: "vector", element: "f32",
                                  dimension: 3}}}}}})"#,
    );
    let err = run_err(
        &graph,
        r#"g.v().has_label("person").property("emb", __.embed("name")).iterate()"#,
    );
    assert!(err.contains("dimension"), "{err}");
    assert!(
        run_ok(
            &graph,
            r#"g.v().has_label("person").has("emb").count().next()"#
        )
        .as_int()
        .unwrap()
            == 0
    );
}

#[test]
fn an_unknown_embedder_fails_before_the_run() {
    let err = run_err(
        &modern(),
        r#"g.v().embed("name", #{embedder: "nope"}).to_list()"#,
    );
    assert!(err.contains("there is no embedder \"nope\""), "{err}");
    let err = run_err(&modern(), r#"embed("a", #{embedder: "nope"})"#);
    assert!(err.contains("there is no embedder \"nope\""), "{err}");
    let err = run_err(&modern(), r#"g.inject("a").embed(#{model: 1})"#);
    assert!(err.contains("the key \"model\""), "{err}");
}

#[test]
fn not_text_is_refused_without_conversion() {
    let err = run_err(&modern(), r#"g.v().values("age").embed().to_list()"#);
    assert!(err.contains("embed() takes a string, not int64"), "{err}");
    let fixed = run_ok(
        &modern(),
        r#"g.v().values("age").as_string().embed().count().next()"#,
    );
    assert_eq!(fixed.as_int().unwrap(), 4);
}
