//! `hash-ngram/1` gives the same bits in the browser as natively: the golden vectors of
//! `crates/graphersal/tests/data/hash_ngram_golden.rs` (recorded natively) under Node.js with
//! `cargo test -p graphersal-wasm --target wasm32-unknown-unknown`, and natively with
//! `cargo test -p graphersal-wasm`. The vectors come from the public `embed` step (the
//! embedder itself is crate-internal since 0.5.0).

use graphersal::prelude::*;

include!("../../graphersal/tests/data/hash_ngram_golden.rs");

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn the_browser_embeds_the_same_bits_as_native() {
    let texts: Vec<&str> = HASH_NGRAM_GOLDEN.iter().map(|(text, _)| *text).collect();
    let graph = GraphSource::empty();
    let g = graph.read();
    let vectors = g
        .traversal()
        .inject(texts.iter().map(|t| t.to_string()).collect::<Vec<_>>())
        .embed_value(Some("hash-ngram/1"))
        .to_list()
        .unwrap();
    let actual: Vec<(&str, u64)> = texts
        .iter()
        .zip(&vectors)
        .map(|(text, value)| {
            let vector = value.as_vector().expect("a vector");
            (*text, hash_ngram_bits_hash(&vector.to_f32_vec()))
        })
        .collect();
    assert_eq!(actual, HASH_NGRAM_GOLDEN.to_vec());
}
