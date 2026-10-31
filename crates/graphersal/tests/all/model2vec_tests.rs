//! The Model2Vec embedder (feature `model2vec`, book page "Embedding Models in a Store") through
//! the DSL: a TINY synthetic model imported into a store serves the `embed` step, the function
//! `embed(text)`, `nearest` and `g.embedders()`. The loader itself (layouts, identity, load
//! failures, odd input) is tested by the crate unit tests of `value/vector/embed/model2vec.rs`
//! since 0.5.0 made it crate-internal.

#![cfg(all(feature = "model2vec", feature = "script", feature = "persist"))]

use std::sync::Arc;

use graphersal::persist::{MemDir, Store};
use graphersal::prelude::*;
use graphersal::script::rhai::Dynamic;

use crate::model_fixture::{DIMENSION, import, options};

fn run(graph: &Arc<Graph>, script: &str) -> Dynamic {
    let mut engine = graphersal::script::engine();
    graphersal::script::set_engine_graph(&mut engine, graph.clone());
    let mut scope = graphersal::script::graph_scope(graph.clone());
    engine
        .eval_with_scope::<Dynamic>(&mut scope, script)
        .unwrap_or_else(|err| panic!("{script}: {err}"))
}

#[test]
fn the_embed_step_runs_the_model() {
    let store = Store::create(MemDir::new(), options()).unwrap();
    let report = import(&store, 1, "acme/tiny");
    let identity = report.model.identity.clone();
    store.set_default_model(Some("acme/tiny")).unwrap();
    let graph = store.graph().clone();
    let _ = run(
        &graph,
        r#"g.add_v("doc").property("name", "graph").iterate();
           g.add_v("doc").property("name", "vector").iterate();
           g.add_v("doc").property("name", "model").iterate()"#,
    );

    // The default model embeds without a name.
    let dimension = run(&graph, r#"g.v().embed("name").next().dimension"#);
    assert_eq!(dimension.as_int().unwrap(), DIMENSION as i64);

    // The nearest name to "vector" by its embedding, with the model named by its repository.
    let nearest = run(
        &graph,
        r#"g.v().property("emb", __.embed("name", #{embedder: "acme/tiny"})).iterate();
           g.v().nearest("emb", embed("vector", #{embedder: "acme/tiny"}), 1).values("name").to_list()"#,
    );
    assert_eq!(nearest.into_array().unwrap()[0].to_string(), "vector");

    // The function embed(text) on the default model equals the step.
    let same = run(
        &graph,
        r#"g.v().has("name", "graph").embed("name").next() == embed("graph")"#,
    );
    assert!(same.as_bool().unwrap());

    // g.embedders() reports the model identity, the default first.
    let listed = run(&graph, r#"g.embedders()[0].model"#).to_string();
    assert_eq!(listed, identity);
    store.close().unwrap();
}
