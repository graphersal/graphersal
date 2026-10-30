//! Randomized blob histories (format spec 4.6, 10.1, 12.4): blob-referencing catalog commits
//! (a host kind 200 payload with `BlobRef`s), removals, checkpoints, prunes, rollbacks (move and
//! delete), attic removals, backups and increments, reopenings. The oracle after every step:
//! every blob the current state, every retained snapshot's state and every backup's state
//! references is present and intact; after a prune to the latest position with an empty attic
//! no blob file is left that no retained history references. `BLOB_FUZZ_CASES` (default 12) and
//! `BLOB_FUZZ_SEED` set the budget; a failing case prints `BLOB_FUZZ_REPLAY=<seed>`.
#![cfg(feature = "persist")]

use graphersal::blob::BlobRef;
use graphersal::persist::{BackupMode, MemDir, RecoveryTarget, RollbackMode, Store, StoreOptions};
use graphersal::prelude::*;

use crate::blob_store_tests::{define, host_kind, undefine, write};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// Every blob the catalog of `graph` references.
fn referenced(graph: &TraversalGraph) -> Vec<BlobRef> {
    let mut out = Vec::new();
    for definition in graph.definitions().unwrap() {
        if definition.kind() != host_kind() {
            continue;
        }
        let payload =
            graphersal::catalog::decode_payload(definition.opaque_payload().unwrap()).unwrap();
        if let Some(ElementProperty::Array(items)) = payload.get("blobs") {
            for item in items {
                if let ElementProperty::Blob(blob) = item {
                    out.push((**blob).clone());
                }
            }
        }
    }
    out
}

fn names(graph: &TraversalGraph) -> Vec<String> {
    graph
        .definitions()
        .unwrap()
        .filter(|d| d.kind() == host_kind())
        .map(|d| d.name().to_string())
        .collect()
}

/// The oracle: every state the store keeps finds its blobs.
fn check(store: &Store, label: &str) {
    for blob in referenced(&store.graph().read()) {
        store
            .read_blob(&blob)
            .unwrap_or_else(|err| panic!("{label}: current state: {err}"));
    }
    // Every position a target can reach: from the oldest snapshot to the current commit.
    let oldest = store.snapshots().unwrap()[0].commit_seq;
    let current = store.graph().read().last_commit_seq();
    for seq in oldest..=current {
        let past = store.read_only_at(RecoveryTarget::CommitSeq(seq)).unwrap();
        for blob in referenced(&past.graph) {
            store
                .read_blob(&blob)
                .unwrap_or_else(|err| panic!("{label}: commit {seq}: {err}"));
        }
    }
    let report = store.verify().unwrap();
    assert!(report.is_ok(), "{label}: {:?}", report.problems);
}

fn run_case(seed: u64) {
    let mut rng = Rng(seed);
    let dir = MemDir::new();
    let mut store =
        Store::create(&dir, StoreOptions::new().with_checkpoint_after_bytes(0)).unwrap();
    let backup = MemDir::new();
    let mut backed_up = false;
    let contents: Vec<Vec<u8>> = (0..6u8).map(|i| vec![i; 10 + usize::from(i) * 7]).collect();
    for step in 0..40 {
        let label = format!("seed {seed} step {step}");
        match rng.below(12) {
            0..=3 => {
                let pending: Vec<_> = (0..1 + rng.below(3))
                    .map(|_| write(&store, &contents[rng.below(contents.len())]))
                    .collect();
                let blobs: Vec<&BlobRef> = pending.iter().map(|p| p.blob()).collect();
                define(&store, &format!("d{}", rng.below(5)), &blobs);
            }
            4 => {
                let existing = names(&store.graph().read());
                if !existing.is_empty() {
                    undefine(&store, &existing[rng.below(existing.len())]);
                }
            }
            5 => {
                // An orphan: written, never referenced.
                drop(write(&store, &[0xEE; 13]));
            }
            6 => {
                store.checkpoint(None).unwrap();
            }
            7 => {
                let up_to = store.graph().read().last_commit_seq();
                store.prune(up_to).unwrap();
            }
            8 => {
                let current = store.graph().read().last_commit_seq();
                let oldest = store.snapshots().unwrap()[0].commit_seq;
                if current > oldest {
                    let target = oldest + rng.below((current - oldest) as usize) as u64;
                    let mode = if rng.below(2) == 0 {
                        RollbackMode::Move
                    } else {
                        RollbackMode::Delete
                    };
                    store
                        .rollback_to(RecoveryTarget::CommitSeq(target), mode)
                        .unwrap();
                }
            }
            9 => {
                if let Some(entry) = store.attic_list().unwrap().first() {
                    store.attic_remove(&entry.id).unwrap();
                }
            }
            10 => {
                match store.backup_with(&backup, BackupMode::Auto) {
                    Ok(_) => backed_up = true,
                    // A rollback behind the backup or a pruned gap: a full backup is needed.
                    Err(graphersal::persist::PersistError::BackupRefused { .. }) => {}
                    Err(err) => panic!("{label}: backup: {err}"),
                }
            }
            _ => {
                store.close().unwrap();
                store =
                    Store::open(&dir, StoreOptions::new().with_checkpoint_after_bytes(0)).unwrap();
            }
        }
        check(&store, &label);
    }
    if backed_up {
        let opened = Store::open_backup(&backup, StoreOptions::new()).unwrap();
        for blob in referenced(&opened.graph().read()) {
            opened
                .read_blob(&blob)
                .unwrap_or_else(|err| panic!("seed {seed}: backup: {err}"));
        }
        assert!(opened.verify().unwrap().is_ok());
    }
    // Everything aside: nothing unreferenced is left.
    for entry in store.attic_list().unwrap() {
        store.attic_remove(&entry.id).unwrap();
    }
    store.checkpoint(None).unwrap();
    store.checkpoint(None).unwrap();
    let up_to = store.graph().read().last_commit_seq();
    store.prune(up_to).unwrap();
    check(&store, &format!("seed {seed} final"));
    let report = store.verify().unwrap();
    assert!(
        !report
            .notes
            .iter()
            .any(|n| n.contains("no retained history references")),
        "seed {seed}: {:?}",
        report.notes
    );
    assert_eq!(store.info().unwrap().blobs.unreferenced, 0, "seed {seed}");
}

#[test]
fn random_blob_histories_keep_every_referenced_blob() {
    if let Ok(seed) = std::env::var("BLOB_FUZZ_REPLAY") {
        run_case(seed.parse().unwrap());
        return;
    }
    let cases: u64 = std::env::var("BLOB_FUZZ_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(12);
    let base: u64 = std::env::var("BLOB_FUZZ_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(7);
    for case in 0..cases {
        let seed = base.wrapping_mul(1_000_003).wrapping_add(case);
        if let Err(panic) = std::panic::catch_unwind(|| run_case(seed)) {
            eprintln!("BLOB_FUZZ_REPLAY={seed}");
            std::panic::resume_unwind(panic);
        }
    }
}
