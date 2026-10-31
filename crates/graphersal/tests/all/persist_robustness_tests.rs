//! Hostile-input robustness of the persistence decoders (CLAUDE.md rule 1, format spec section 2):
//! seeded byte mutations of valid packed snapshots and journals, plus handwritten adversarial
//! headers (huge lengths and counts). Every case runs in [`guarded`] (own thread, panic catch,
//! watchdog): a decoder may refuse the input, never panic, hang or allocate from an unchecked
//! length. After a successful decode the graph is exercised (a traversal, a new snapshot).
//!
//! `ROBUSTNESS_INPUT_CASES=<n>` raises the mutation budget like `robustness_input_tests`.
#![cfg(all(feature = "persist", feature = "script"))]

use std::time::Duration;

use graphersal::catalog::{Definition, DefinitionKind, QueryDefinition};
use graphersal::persist::{self, Durability, Journal, RecoveryTarget, SharedBuffer, WriteOptions};
use graphersal::prelude::*;

use crate::robustness_fuzz_tests::{Rng, guarded};

const WATCHDOG: Duration = Duration::from_secs(30);

fn budget(default: u64) -> u64 {
    std::env::var("ROBUSTNESS_INPUT_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn mutate(rng: &mut Rng, base: &[u8]) -> Vec<u8> {
    let mut out = base.to_vec();
    for _ in 0..1 + rng.below(4) {
        if out.is_empty() {
            break;
        }
        let at = rng.below(out.len());
        match rng.below(6) {
            0 => out.truncate(at),
            1 => out[at] ^= 1 << rng.below(8),
            2 => out[at] = *rng.pick(&[0x00, 0xFF, 0x7F, 0x80, 0x01]),
            3 => {
                let end = (at + rng.below(64)).min(out.len());
                let slice = out[at..end].to_vec();
                out.splice(at..at, slice);
            }
            4 => {
                let end = (at + rng.below(32)).min(out.len());
                out.drain(at..end);
            }
            _ => {
                // A huge little-endian length or count.
                let end = (at + 8).min(out.len());
                for byte in &mut out[at..end] {
                    *byte = 0xFF;
                }
            }
        }
    }
    out
}

fn sample_graph() -> TraversalGraph {
    let mut graph = TraversalGraph::tinkerpop_modern();
    graph
        .traversal_mut()
        .v("1")
        .property(
            "nested",
            ElementProperty::Array(vec![
                ElementProperty::Uuid(42),
                ElementProperty::String("x".repeat(40).into()),
            ]),
        )
        .to_list()
        .unwrap();
    // The catalog of definitions: a saved query and a kind this version does not know.
    graph
        .set_definition(Definition::query(
            "people",
            QueryDefinition::new("fn people() { g.v() }").with_folder("a"),
        ))
        .unwrap();
    graph
        .set_definition(Definition::opaque(
            DefinitionKind::from_code(50),
            Default::default(),
            "later",
            vec![1, 2, 3],
        ))
        .unwrap();
    graph
}

fn snapshot(graph: &TraversalGraph, chunk: u32) -> Vec<u8> {
    let mut out = Vec::new();
    persist::write_snapshot(
        graph,
        &mut out,
        &WriteOptions::new()
            .with_chunk_bytes(chunk)
            .with_segment_bytes(512),
    )
    .unwrap();
    out
}

#[test]
fn mutated_packed_snapshots_never_crash_the_reader() {
    let graph = sample_graph();
    let bases = [snapshot(&graph, 64), snapshot(&graph, 1 << 20)];
    let mut rng = Rng::new(0x05EE_D5A9);
    let mut fatal = Vec::new();
    for case in 0..budget(300) {
        let base = &bases[case as usize % bases.len()];
        let input = mutate(&mut rng, base);
        let result = guarded(WATCHDOG, move || {
            if let Ok(graph) = TraversalGraph::read_snapshot(input.as_slice()) {
                let _ = graph
                    .traversal()
                    .v(None::<()>)
                    .out(None::<()>)
                    .count()
                    .next();
                let mut again = Vec::new();
                let _ = graph.write_snapshot(&mut again);
            }
        });
        if let Err(outcome) = result {
            fatal.push(format!("case {case}: {outcome:?}"));
        }
    }
    assert!(fatal.is_empty(), "{fatal:#?}");
}

#[test]
fn mutated_journals_never_crash_recovery() {
    let mut graph = sample_graph();
    let mut base_snapshot = Vec::new();
    graph.write_snapshot(&mut base_snapshot).unwrap();
    let wal = SharedBuffer::new();
    Journal::new(wal.clone(), Durability::Os)
        .attach(&mut graph)
        .unwrap();
    graph
        .traversal_mut()
        .add_v("person")
        .property("name", "ann")
        .to_list()
        .unwrap();
    graph.mark("m").unwrap();
    graph.traversal_mut().v("2").drop().to_list().unwrap();
    graph
        .set_definition(Definition::query(
            "more",
            QueryDefinition::new("fn more() { 1 }"),
        ))
        .unwrap();
    graph
        .remove_definition(DefinitionKind::QUERY, "people")
        .unwrap();
    graph
        .transaction(|g| -> Result<(), GraphError> {
            for i in 0..100i64 {
                g.add_vertex(None, vec!["bulk".into()], {
                    let mut p = graphersal::prelude::PropertyMap::default();
                    p.insert("i", i.into());
                    p
                })?;
            }
            Ok(())
        })
        .unwrap();
    let journal = wal.contents();
    let mut rng = Rng::new(0x0BAD_010E);
    let mut fatal = Vec::new();
    for case in 0..budget(300) {
        let input = mutate(&mut rng, &journal);
        let snapshot = base_snapshot.clone();
        let result = guarded(WATCHDOG, move || {
            for record in persist::read_journal(input.as_slice())
                .into_iter()
                .flatten()
            {
                let _ = record;
            }
            let _ = persist::recover(
                snapshot.as_slice(),
                [input.as_slice()],
                RecoveryTarget::Latest,
            );
        });
        if let Err(outcome) = result {
            fatal.push(format!("case {case}: {outcome:?}"));
        }
    }
    assert!(fatal.is_empty(), "{fatal:#?}");
}

#[test]
fn adversarial_headers_are_refused_without_allocating() {
    let mut cases: Vec<Vec<u8>> = Vec::new();
    // Packed: valid magic and version, absurd section count and a manifest of 2^62 bytes.
    let mut packed = b"GRSLPACK".to_vec();
    packed.extend_from_slice(&1u32.to_le_bytes());
    packed.extend_from_slice(&u32::MAX.to_le_bytes());
    packed.push(1);
    packed.extend_from_slice(&(1u64 << 62).to_le_bytes());
    cases.push(packed.clone());
    packed.extend_from_slice(&[0u8; 100]);
    cases.push(packed);
    // Journal: a valid header followed by a frame that declares 4 GiB.
    let mut graph = TraversalGraph::new();
    let wal = SharedBuffer::new();
    Journal::new(wal.clone(), Durability::Os)
        .attach(&mut graph)
        .unwrap();
    let mut journal = wal.contents();
    journal.extend_from_slice(&0x4752_5357u32.to_le_bytes());
    journal.extend_from_slice(&u32::MAX.to_le_bytes());
    journal.extend_from_slice(&[0u8; 12]);
    journal.extend_from_slice(&[1u8; 64]);
    cases.push(journal);
    for (i, input) in cases.into_iter().enumerate() {
        let result = guarded(WATCHDOG, move || {
            let _ = TraversalGraph::read_snapshot(input.as_slice());
            let _ = persist::read_snapshot_info(input.as_slice());
            let _ = persist::Recovery::new().journal(input.as_slice()).run();
        });
        assert!(result.is_ok(), "case {i}: {result:?}");
    }
}

/// Mutated system journal segments (format spec 4.7): reading them through `journal_dir`,
/// verifying the store and opening it never panic, hang or allocate from a hostile length; the
/// store itself opens (the journal is never damage).
#[test]
fn mutated_system_journal_segments_never_crash_the_store() {
    use graphersal::persist::{MemDir, Store, StoreOptions};
    use graphersal::system_journal::JournalQuery;
    let base = MemDir::new();
    let options = StoreOptions::new().with_checkpoint_after_bytes(0);
    let store = Store::create(&base, options.clone()).unwrap();
    for _ in 0..3 {
        store.checkpoint(None).unwrap();
    }
    store.close().unwrap();
    let name = base
        .file_names()
        .into_iter()
        .find(|name| name.starts_with("journal/"))
        .unwrap();
    let segment = base.file(&name).unwrap();
    let mut rng = Rng::new(0x0005_EC7A);
    let mut fatal = Vec::new();
    for case in 0..budget(200) {
        let input = if case % 20 == 19 {
            // A hostile length right after a valid header.
            let mut hostile = segment[..64].to_vec();
            hostile.extend_from_slice(&0x4A52_5347u32.to_le_bytes());
            hostile.extend_from_slice(&u32::MAX.to_le_bytes());
            hostile.extend_from_slice(&[0; 18]);
            hostile
        } else {
            mutate(&mut rng, &segment)
        };
        let image = base.deep_copy();
        image.put_file(&name, input);
        let options = options.clone();
        let result = guarded(WATCHDOG, move || {
            let _ = Store::journal_dir(&image, &JournalQuery::new());
            let report = Store::verify_dir(&image).unwrap();
            assert!(report.is_ok(), "{:?}", report.problems);
            let store = Store::open(&image, options).unwrap();
            assert!(!store.open_report().maintenance);
            let journal = store.journal().unwrap();
            let _ = journal.query(&JournalQuery::new().limit(10_000));
            store.close().unwrap();
        });
        if let Err(outcome) = result {
            fatal.push(format!("case {case}: {outcome:?}"));
        }
    }
    assert!(fatal.is_empty(), "{fatal:#?}");
}

/// [`mutated_journals_never_crash_recovery`] over commits in parts (format spec 9.3.1): parts of
/// 48 bytes, so nearly every commit is several records.
#[test]
fn mutated_journals_in_parts_never_crash_recovery() {
    let mut graph = sample_graph();
    let mut base_snapshot = Vec::new();
    graph.write_snapshot(&mut base_snapshot).unwrap();
    let wal = SharedBuffer::new();
    Journal::new(wal.clone(), Durability::Os)
        .with_commit_part_bytes(48)
        .attach(&mut graph)
        .unwrap();
    graph
        .traversal_mut()
        .add_v("person")
        .property("name", "ann, a person whose commit takes several parts")
        .to_list()
        .unwrap();
    graph.mark("m").unwrap();
    graph.traversal_mut().v("2").drop().to_list().unwrap();
    graph
        .transaction(|g| -> Result<(), GraphError> {
            for i in 0..40i64 {
                g.add_vertex(None, vec!["bulk".into()], {
                    let mut p = graphersal::prelude::PropertyMap::default();
                    p.insert("i", i.into());
                    p
                })?;
            }
            Ok(())
        })
        .unwrap();
    let journal = wal.contents();
    assert!(
        persist::recover(
            base_snapshot.as_slice(),
            [journal.as_slice()],
            RecoveryTarget::Latest
        )
        .is_ok()
    );
    let mut rng = Rng::new(0x0BAD_9A27);
    let mut fatal = Vec::new();
    for case in 0..budget(300) {
        let input = mutate(&mut rng, &journal);
        let snapshot = base_snapshot.clone();
        let result = guarded(WATCHDOG, move || {
            for record in persist::read_journal(input.as_slice())
                .into_iter()
                .flatten()
            {
                let _ = record;
            }
            let _ = persist::recover(
                snapshot.as_slice(),
                [input.as_slice()],
                RecoveryTarget::Latest,
            );
        });
        if let Err(outcome) = result {
            fatal.push(format!("case {case}: {outcome:?}"));
        }
    }
    assert!(fatal.is_empty(), "{fatal:#?}");
}

/// Mutated model blobs (the tar of a model, format spec 4.5) and mutated model definition
/// payloads (catalog kind 5) never crash an open, a verify or a listing: a blob whose bytes
/// still check but whose content is no model tar is a verify problem; a damaged payload is
/// kept opaque or refused, never a panic.
#[test]
#[cfg(not(target_arch = "wasm32"))]
fn mutated_model_blobs_and_definitions_never_crash_the_store() {
    use graphersal::catalog::ModelDefinition;
    use graphersal::persist::{MemDir, Store, StoreOptions};
    let base = MemDir::new();
    let options = StoreOptions::new().with_checkpoint_after_bytes(0);
    let store = Store::create(&base, options.clone()).unwrap();
    let report = crate::model_fixture::import(&store, 1, "acme/tiny");
    let definition = store
        .graph()
        .read()
        .definition(DefinitionKind::MODEL, &report.model.identity)
        .unwrap()
        .unwrap()
        .clone();
    store.close().unwrap();
    let blob_name = base
        .file_names()
        .into_iter()
        .find(|name| name.starts_with("blobs/") && !name.contains("tmp-"))
        .unwrap();
    let tar = base.file(&blob_name).unwrap();
    let payload =
        graphersal::catalog::encode_payload(&definition.as_model().unwrap().to_payload()).unwrap();
    let mut rng = Rng::new(0x0000_A0DE);
    let mut fatal = Vec::new();
    for case in 0..budget(200) {
        // A mutated payload: decoded (kept opaque when not a model) without a panic.
        let bytes = mutate(&mut rng, &payload);
        if let Ok(map) = graphersal::catalog::decode_payload(&bytes) {
            let _ = ModelDefinition::from_payload(&map).map(|m| m.to_payload());
        }
        // A mutated tar under the blob's name (its CRC no longer matches unless the mutation
        // was empty): verify reports, the open never panics.
        let image = base.deep_copy();
        image.put_file(&blob_name, mutate(&mut rng, &tar));
        let options = options.clone();
        let result = guarded(WATCHDOG, move || {
            let _ = Store::verify_dir(&image);
            let _ = Store::models_dir(&image);
            if let Ok(store) = Store::open(&image, options) {
                let _ = store.models();
                let _ = store.verify();
                let _ = store.close();
            }
        });
        if let Err(outcome) = result {
            fatal.push(format!("case {case}: {outcome:?}"));
        }
    }
    assert!(fatal.is_empty(), "{fatal:#?}");
}
