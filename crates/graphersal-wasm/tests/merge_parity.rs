//! The merge checkpoint gives the same snapshot in the browser as natively (CP2 matrix row 8):
//! one deterministic history on a store in memory, checkpointed through
//! `Store::run_due_checkpoint` on wasm32 (no threads: the host runs the due checkpoint) and
//! through `Store::checkpoint` natively; the merged snapshot's `schema.json` and segment files
//! (everything but the manifest, which records its creation time) hash to the same value on
//! both. Runs natively with `cargo test -p graphersal-wasm` and under Node.js with
//! `cargo test -p graphersal-wasm --target wasm32-unknown-unknown`.

use graphersal::persist::{Codec, MemDir, Store, StoreOptions};
use graphersal::prelude::*;
use graphersal::storage::PersistentStorage;

/// FNV-1a over the packed snapshot's sections after the manifest.
fn data_hash(packed: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    let mut at = 16;
    while at + 9 <= packed.len() - 4 {
        let kind = packed[at];
        let len = u64::from_le_bytes(packed[at + 1..at + 9].try_into().unwrap()) as usize;
        if kind != 1 {
            for byte in &packed[at + 9..at + 9 + len] {
                hash = (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3);
            }
        }
        at += 9 + len;
    }
    hash
}

fn history(store: &Store) {
    for i in 0..300i64 {
        store
            .graph()
            .write()
            .transaction(|g| {
                let mut properties = PropertyMap::default();
                properties.insert("name", format!("name {i}").into());
                properties.insert("n", ElementProperty::Number(i.into()));
                let label = if i % 3 == 0 { "company" } else { "person" };
                g.add_vertex(
                    Some(format!("v{i:04}").into()),
                    vec![label.into()],
                    properties,
                )
                .map(|_| ())
            })
            .unwrap();
    }
    // The base of the merge: every vertex, in several segments.
    store.checkpoint(None).unwrap();
    for i in (0..20i64).step_by(7) {
        store
            .graph()
            .write()
            .transaction(|g| {
                let v = g.get_vertex_by_id(&format!("v{i:04}")).unwrap().unwrap();
                g.set_vertex_property(v, "touched".into(), ElementProperty::Boolean(true))
            })
            .unwrap();
    }
}

/// The merged snapshot's data hash for the same history.
fn merged_hash() -> u64 {
    let options = StoreOptions::new()
        .with_codec(Codec::Lz4)
        .with_chunk_bytes(2048)
        .with_snapshot_segment_bytes(8192)
        .with_checkpoint_after_bytes(if cfg!(target_arch = "wasm32") { 1 } else { 0 });
    let dir = MemDir::new();
    let mut graph = TraversalGraph::new();
    graph.set_graph_id(0x5EED).unwrap();
    let store = Store::create_with(&dir, graph, options).unwrap();
    history(&store);
    #[cfg(target_arch = "wasm32")]
    {
        assert!(store.run_due_checkpoint().unwrap().is_some());
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        store.checkpoint(None).unwrap();
    }
    let mut packed = Vec::new();
    let info = store.export_packed(None, &mut packed).unwrap();
    assert_eq!(info.commit_seq, store.graph().read().last_commit_seq());
    data_hash(&packed)
}

/// Recorded from the native run; the browser must produce the same bytes (file format version 3:
/// the version word in every segment header).
const EXPECTED: u64 = 1_991_369_822_573_691_254;

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn the_browser_merges_the_same_snapshot_as_native() {
    assert_eq!(merged_hash(), EXPECTED);
}
