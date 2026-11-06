//! A store whose schema needs an embedding model opens READ-ONLY in the browser (no Model2Vec
//! loader on wasm32; format spec 4.5, book page "Embedding Models in a Store"), and a text query
//! with the model is refused naming the reason. Runs natively with `cargo test -p
//! graphersal-wasm` (a build without the `model2vec` feature behaves the same) and under Node.js
//! with `cargo test -p graphersal-wasm --target wasm32-unknown-unknown`. Natively, Cargo's
//! feature unification turns the loader ON when this crate is tested in one command with
//! `graphersal-cli` (which enables `graphersal/model2vec`): the fake model is a real one (it
//! loads), and with a loader the test checks the other side, the model loaded and serving.

use std::io::Cursor;

use graphersal::persist::{
    ImportModelOptions, MemDir, ModelRole, ModelSource, ModelState, NewModel, Store, StoreOptions,
};
use graphersal::prelude::*;

/// The vocabulary of the tiny model, one row of the matrix each.
const VOCAB: [&str; 4] = ["[UNK]", "graph", "vector", "model"];

/// The tokenizer of the tiny model (a word-level vocabulary: it loads, so an import that checks
/// the model's load accepts it).
fn tokenizer_json() -> String {
    let entries: Vec<String> = VOCAB
        .iter()
        .enumerate()
        .map(|(id, token)| format!("{token:?}: {id}"))
        .collect();
    format!(
        r#"{{"version": "1.0", "truncation": null, "padding": null, "added_tokens": [],
            "normalizer": {{"type": "Lowercase"}}, "pre_tokenizer": {{"type": "Whitespace"}},
            "post_processor": null, "decoder": null,
            "model": {{"type": "WordLevel", "vocab": {{{}}}, "unk_token": "[UNK]"}}}}"#,
        entries.join(", ")
    )
}

/// The weights of a tiny model (a 4 x 3 f32 embedding matrix).
fn safetensors() -> Vec<u8> {
    let data: Vec<u8> = (0..12)
        .flat_map(|i| (1.0f32 + i as f32).to_le_bytes())
        .collect();
    let mut header = format!(
        r#"{{"embeddings": {{"dtype": "F32", "shape": [4, 3], "data_offsets": [0, {}]}}}}"#,
        data.len()
    );
    while header.len() % 8 != 0 {
        header.push(' ');
    }
    let mut out = (header.len() as u64).to_le_bytes().to_vec();
    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(&data);
    out
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn a_store_needing_a_model_opens_read_only_without_text_queries() {
    let dir = MemDir::new();
    let store = Store::create(dir.clone(), StoreOptions::new()).unwrap();
    let model = NewModel::new()
        .with_file(
            "config.json",
            ModelRole::Config,
            Cursor::new(b"{}".to_vec()),
        )
        .with_file(
            "tokenizer.json",
            ModelRole::Tokenizer,
            Cursor::new(tokenizer_json().into_bytes()),
        )
        .with_file(
            "model.safetensors",
            ModelRole::Weights,
            Cursor::new(safetensors()),
        )
        .with_repo("acme/tiny")
        .with_license("mit");
    let report = store
        .import_model(ModelSource::Files(model), ImportModelOptions::new())
        .unwrap();
    let identity = report.model.identity.clone();
    let schema = GraphSchema::from_json(&format!(
        r#"{{"mode": "open", "vertices": {{"doc": {{"schema": {{"type": "object", "properties": {{"emb": {{"type": "array", "format": "vector", "dimension": 3, "embedder": "{identity}"}}}}}}}}}}}}"#
    ))
    .unwrap();
    store
        .graph()
        .write()
        .traversal_mut()
        .set_schema(schema)
        .unwrap();
    store.close().unwrap();

    let store = Store::open(dir, StoreOptions::new()).unwrap();
    let models = store.models().unwrap();
    assert_eq!(models[0].needed_by, ["doc.emb"]);
    let embedded = store
        .graph()
        .read()
        .traversal()
        .inject(vec!["graph"])
        .embed_value(Some("acme/tiny"))
        .to_list();
    let Some(reason) = store.read_only_reason().unwrap() else {
        // A build with the Model2Vec loader (natively only, `graphersal/model2vec` unified on):
        // the model the schema needs is loaded at the open and serves the text query.
        assert!(!cfg!(target_arch = "wasm32"), "wasm32 has no loader");
        assert_eq!(models[0].state, ModelState::Loaded);
        assert_eq!(embedded.unwrap().len(), 1);
        return;
    };
    assert!(
        reason.contains("cannot run the model acme/tiny"),
        "{reason}"
    );
    assert_eq!(models[0].state, ModelState::NotLoaded);
    let err = embedded.unwrap_err();
    let text = graphersal::error::format_diagnostic(&*err);
    assert!(text.contains("cannot run the model"), "{text}");
}
