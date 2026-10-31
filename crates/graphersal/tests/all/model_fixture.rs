//! Shared helpers of the embedding-model tests (`store_models_tests`, `model_*_tests`): a TINY
//! synthetic Model2Vec model (a word-level tokenizer of four tokens, an f32 matrix of 4 x 3)
//! that loads with the feature `model2vec`, its files, and a schema naming a model. No network.
#![cfg(all(feature = "persist", not(target_arch = "wasm32")))]
#![allow(dead_code)]

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use graphersal::persist::{
    ImportModelOptions, ImportModelReport, ModelRole, ModelSource, NewModel, Store, StoreOptions,
};
use graphersal::prelude::*;

/// The vocabulary of the tiny model.
pub const VOCAB: [&str; 4] = ["[UNK]", "graph", "vector", "model"];
/// The tiny model's dimension.
pub const DIMENSION: usize = 3;

/// A fresh directory under the system temp dir, removed on drop.
pub struct Temp(pub PathBuf);

impl Temp {
    pub fn new(name: &str) -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "graphersal-model-tests-{}-{}-{name}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    pub fn join(&self, part: &str) -> PathBuf {
        self.0.join(part)
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Store options without automatic checkpoints.
pub fn options() -> StoreOptions {
    StoreOptions::new().with_checkpoint_after_bytes(0)
}

/// The tokenizer of the tiny model.
pub fn tokenizer_json() -> String {
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

/// The weights of the tiny model; `seed` makes another model (other values).
pub fn safetensors(seed: u8) -> Vec<u8> {
    let data: Vec<u8> = (0..VOCAB.len() * DIMENSION)
        .flat_map(|i| (1.0f32 + i as f32 + f32::from(seed) * 0.5).to_le_bytes())
        .collect();
    let mut header = format!(
        r#"{{"embeddings": {{"dtype": "F32", "shape": [{}, {DIMENSION}], "data_offsets": [0, {}]}}}}"#,
        VOCAB.len(),
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

/// The config of the tiny model.
pub const CONFIG: &str = r#"{"normalize": true}"#;

/// The tiny model `seed` of the repository `repo` as a [`NewModel`] (licence MIT, a card).
pub fn tiny_model(seed: u8, repo: &str) -> NewModel {
    NewModel::new()
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
            Cursor::new(safetensors(seed)),
        )
        .with_file(
            "README.md",
            ModelRole::Card,
            Cursor::new(b"---\nlicense: mit\n---\n# A tiny model\n".to_vec()),
        )
        .with_repo(repo)
        .with_license("mit")
}

/// The tiny model `seed` as a directory under `temp` (a download manifest naming `repo`).
pub fn tiny_model_dir(temp: &Temp, name: &str, seed: u8, repo: &str) -> PathBuf {
    let dir = temp.join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("config.json"), CONFIG).unwrap();
    fs::write(dir.join("tokenizer.json"), tokenizer_json()).unwrap();
    fs::write(dir.join("model.safetensors"), safetensors(seed)).unwrap();
    fs::write(dir.join("README.md"), "# A tiny model\n").unwrap();
    fs::write(
        dir.join("graphersal-embed-model.json"),
        format!(
            r#"{{"format": 2, "repo": "{repo}", "revision": "main", "license": "apache-2.0", "base_models": [{{"repo": "acme/base", "license": "mit"}}], "gated": false, "files": []}}"#
        ),
    )
    .unwrap();
    dir
}

/// Imports the tiny model `seed` of `repo` into `store`.
pub fn import(store: &Store, seed: u8, repo: &str) -> ImportModelReport {
    store
        .import_model(
            ModelSource::Files(tiny_model(seed, repo)),
            ImportModelOptions::new(),
        )
        .unwrap()
}

/// A schema whose vertex label `doc` has the vector property `emb` made by `identity`.
pub fn schema_naming(identity: &str) -> GraphSchema {
    GraphSchema::from_json(&format!(
        r#"{{"mode": "open", "vertices": {{"doc": {{"schema": {{"type": "object", "properties": {{"emb": {{"type": "array", "format": "vector", "dimension": {DIMENSION}, "embedder": "{identity}"}}}}}}}}}}}}"#
    ))
    .unwrap()
}

/// Sets [`schema_naming`] on the store's graph.
pub fn set_schema(store: &Store, identity: &str) {
    store
        .graph()
        .write()
        .traversal_mut()
        .set_schema(schema_naming(identity))
        .unwrap();
}

/// Adds a person.
pub fn add_person(store: &Store, name: &str) {
    store
        .graph()
        .write()
        .traversal_mut()
        .add_v("person")
        .property("name", name)
        .to_list()
        .unwrap();
}

/// The path of the blob of `identity`'s model in the directory store at `path`.
pub fn blob_path(path: &Path, store: &Store, identity: &str) -> PathBuf {
    let graph = store.graph().read();
    let definition = graph
        .definition(graphersal::catalog::DefinitionKind::MODEL, identity)
        .unwrap()
        .unwrap();
    let blob = definition.as_model().unwrap().blob().clone();
    path.join("blobs").join(blob.relative_path())
}

/// Overwrites one byte of a file in the middle.
pub fn flip_byte(path: &Path) {
    let mut bytes = fs::read(path).unwrap();
    let middle = bytes.len() / 2;
    bytes[middle] ^= 0x55;
    fs::write(path, bytes).unwrap();
}
