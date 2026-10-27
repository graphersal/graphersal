//! The merge checkpoint's randomized differential test (CP2 matrix row 18): seeded random
//! histories (every mutation kind, live and closed checkpoints, prunes, rollbacks in move and
//! delete mode, attic restores of any entry in any order, attic removals, marks, random
//! chunk/segment/WAL sizes and codecs) with random damage of the merge's inputs. Every merge
//! without damage must equal its oracle (the newest snapshot loaded and the WAL replayed, taken
//! before it); a merge of damaged inputs must be refused with nothing written, or (damage it
//! does not read, a redundant manifest copy) equal the oracle. Never a panic.
//!
//! Invariants checked after every step that changes files (PF1): the store verifies with no
//! problem at all, so the donor rule holds (format spec 12.3: the newest snapshot shares no
//! segment file with the previous one) and every attic entry rebuilds; and every attic entry
//! holds exactly the history it took (its fork, and its restore when allowed, equal the state
//! recorded before its rollback).
//!
//! A fast budget runs in every `cargo test`. The long run is a release gate (CLAUDE.md):
//!
//! ```text
//! MERGE_FUZZ_CASES=60000 MERGE_FUZZ_SEED=1 MERGE_FUZZ_THREADS=8 \
//!   cargo test --release -p graphersal --features script,io,display,serde,persist,persist-zip \
//!   --test all merge_checkpoint_fuzz_tests::
//! ```
//!
//! A failing case prints its seed; `MERGE_FUZZ_REPLAY=<seed>` runs that case alone.
#![cfg(all(feature = "persist", feature = "script"))]

use std::collections::BTreeMap;
use std::sync::Arc;

use graphersal::persist::{
    Codec, MemDir, RecoveryTarget, RollbackMode, Store, StoreDir, StoreOptions,
};
use graphersal::prelude::GraphStorage;

use crate::merge_checkpoint_tests::{
    Rng, exact, fake_data, history, inputs, listing, newest_alone, oracle, same,
};

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// One case: everything follows from `seed`.
fn case(seed: u64) {
    let mut rng = Rng(seed);
    let codec = [Codec::None, Codec::Lz4][rng.below(2)];
    let options = StoreOptions::new()
        .with_checkpoint_after_bytes(0)
        .with_codec(codec)
        .with_chunk_bytes(150 + rng.below(2000) as u32)
        .with_snapshot_segment_bytes(300 + rng.below(8_000) as u64)
        .with_wal_segment_bytes(400 + rng.below(4000) as u64)
        .with_checkpoint_merge_bytes([1 << 30, 1 << 30, 1 << 30, 1][rng.below(4)]);
    let dir = MemDir::new();
    let shared: Arc<dyn StoreDir> = Arc::new(dir.clone());
    let mut store = Some(Store::create(&dir, options.clone()).unwrap());
    fake_data(store.as_ref().unwrap().graph(), seed, 5 + rng.below(40));
    let mut round = 0;
    // Every attic entry with the state its rollback moved aside: `(commit, exact state)`.
    let mut attic: BTreeMap<String, (u64, Vec<String>)> = BTreeMap::new();
    for _ in 0..3 + rng.below(8) {
        let open = match store.take() {
            Some(open) => open,
            None => Store::open(&dir, options.clone()).unwrap(),
        };
        let rounds = 1 + rng.below(25);
        history(open.graph(), &mut rng, rounds, round);
        round += rounds;
        let action = rng.below(10);
        if std::env::var("MERGE_FUZZ_VERBOSE").is_ok() {
            eprintln!(
                "action {action} at commit {} snapshots {:?} lineage {:?}",
                open.graph().read().last_commit_seq(),
                open.snapshots()
                    .unwrap()
                    .iter()
                    .map(|s| s.commit_seq)
                    .collect::<Vec<_>>(),
                open.info().unwrap().lineage
            );
        }
        match action {
            0 | 1 => {
                // A live checkpoint against the live graph (now and then a few in a row, one
                // commit between each: the third references the first's twins).
                for extra in 0..rng.below(4) {
                    open.checkpoint(None).unwrap();
                    history(open.graph(), &mut rng, 1, round + 1000 + extra);
                }
                let snapshot = open.checkpoint(None).unwrap();
                let live = open.graph().read();
                assert_eq!(snapshot.commit_seq, live.last_commit_seq());
                // (Without the lineage id: an existing snapshot at a rollback target is of the
                // ancestor lineage; the closed branch checks it for written snapshots.)
                let alone = newest_alone(&shared).0;
                assert_eq!(exact(&alone)[1..], exact(&live)[1..], "live");
                assert_eq!(
                    alone.statistics().unwrap(),
                    live.statistics().unwrap(),
                    "live"
                );
                drop(live);
                assert_verified(&open);
                store = Some(open);
            }
            2 | 3 => {
                // A closed checkpoint against the oracle.
                open.close().unwrap();
                let expected = oracle(&shared);
                let report = Store::checkpoint_dir(&dir, options.clone(), None).unwrap();
                assert_eq!(report.snapshot.commit_seq, expected.last_commit_seq());
                if report.written {
                    same(&newest_alone(&shared).0, &expected, "closed");
                } else {
                    // The existing snapshot at the position: after a rollback to exactly its
                    // commit it is of the ancestor lineage, the same state otherwise.
                    assert_eq!(exact(&newest_alone(&shared).0)[1..], exact(&expected)[1..]);
                }
                let report = Store::verify_dir(&dir).unwrap();
                assert!(report.problems.is_empty(), "{:?}", report.problems);
                assert!(
                    report.without_donor.is_empty(),
                    "{:?}",
                    report.without_donor
                );
            }
            4 => {
                // Damage one byte of an input of the closed store's merge, in a copy.
                open.close().unwrap();
                let expected = exact(&oracle(&shared));
                let image = dir.deep_copy();
                let files = inputs(&image);
                let file = &files[rng.below(files.len())];
                let mut bytes = image.file(file).unwrap();
                if bytes.is_empty() {
                    continue;
                }
                let at = rng.below(bytes.len());
                bytes[at] ^= 1 << rng.below(8);
                image.put_file(file, bytes);
                let before = listing(&image);
                match Store::checkpoint_dir(&image, options.clone(), None) {
                    Err(_) => assert_eq!(listing(&image), before, "{file} byte {at}"),
                    // Nothing to merge: the existing snapshot is returned, nothing is read
                    // or written.
                    Ok(report) if !report.written => {
                        assert_eq!(listing(&image), before, "{file} byte {at}")
                    }
                    Ok(_) => {
                        let image: Arc<dyn StoreDir> = Arc::new(image);
                        assert_eq!(exact(&newest_alone(&image).0), expected, "{file} byte {at}");
                    }
                }
            }
            5 => {
                // A prune up to the newest snapshot; everything still loads.
                let last = open.snapshots().unwrap().pop().unwrap().commit_seq;
                open.prune(last).unwrap();
                assert_verified(&open);
                store = Some(open);
            }
            6 | 8 => {
                // A rollback to a random earlier commit (refused when no snapshot is left at
                // or before it; now and then deleting the history), sometimes restored from
                // the attic again.
                let current = open.graph().read().last_commit_seq();
                let before = (current, exact(&open.graph().read())[1..].to_vec());
                // Uniform, or at (or right after) a snapshot: rollbacks before an older entry's
                // base and before the files its snapshots reference.
                let snapshots: Vec<u64> = open
                    .snapshots()
                    .unwrap()
                    .iter()
                    .map(|s| s.commit_seq)
                    .collect();
                let target = if rng.below(2) == 0 {
                    rng.below(current as usize + 1) as u64
                } else {
                    let at = snapshots[rng.below(snapshots.len())];
                    (at + rng.below(2) as u64).saturating_sub(rng.below(2) as u64)
                };
                let mode =
                    [RollbackMode::Move, RollbackMode::Move, RollbackMode::Delete][rng.below(3)];
                if std::env::var("MERGE_FUZZ_VERBOSE").is_ok() {
                    eprintln!(
                        "rollback to {target}; wal {:?}",
                        dir.file_names()
                            .into_iter()
                            .filter(|n| n.starts_with("wal/") || n.starts_with("attic"))
                            .collect::<Vec<_>>()
                    );
                }
                if let Ok(report) = open.rollback_to(RecoveryTarget::CommitSeq(target), mode)
                    && let Some(id) = report.attic
                {
                    attic.insert(id.clone(), before);
                    if rng.below(2) == 0 {
                        restore(&open, &id, &mut attic);
                    }
                }
                assert_verified(&open);
                check_attic(&open, &attic);
                store = Some(open);
            }
            9 => {
                // An attic operation on a random entry: a restore (when allowed: tried in any
                // order, refused otherwise with nothing changed), or a removal.
                let entries = open.attic_list().unwrap();
                if !entries.is_empty() {
                    let id = entries[rng.below(entries.len())].id.clone();
                    if rng.below(3) == 0 {
                        open.attic_remove(&id).unwrap();
                        attic.remove(&id);
                    } else if open.attic_can_restore(&id).is_ok() {
                        restore(&open, &id, &mut attic);
                    } else {
                        let before = listing(&dir);
                        assert!(open.attic_restore(&id).is_err());
                        assert_eq!(listing(&dir), before, "a refused restore changed files");
                    }
                }
                assert_verified(&open);
                check_attic(&open, &attic);
                store = Some(open);
            }
            _ => {
                let _ = open.mark(&format!("m{round}"));
                store = Some(open);
            }
        }
    }
    // The end: the store opens to its oracle and verifies.
    drop(store);
    let expected = oracle(&shared);
    let reopened = Store::open(&dir, options).unwrap();
    same(&reopened.graph().read(), &expected, "reopened");
    assert_verified(&reopened);
    check_attic(&reopened, &attic);
    // Every entry that can be restored now, in turn, newest first (each restore may make an
    // older one restorable).
    loop {
        let restorable = reopened
            .attic_list()
            .unwrap()
            .into_iter()
            .rev()
            .find(|entry| reopened.attic_can_restore(&entry.id).is_ok());
        let Some(entry) = restorable else {
            break;
        };
        restore(&reopened, &entry.id, &mut attic);
        assert_verified(&reopened);
    }
    check_attic(&reopened, &attic);
}

/// Restores attic entry `id`: the store then holds exactly the state its rollback moved aside.
fn restore(store: &Store, id: &str, attic: &mut BTreeMap<String, (u64, Vec<String>)>) {
    let position = store.attic_restore(id).unwrap();
    let (commit, state) = attic.remove(id).expect("a known entry");
    assert_eq!(position, commit, "restore of {id}");
    assert_eq!(
        exact(&store.graph().read())[1..],
        state[..],
        "restore of {id}"
    );
}

/// Every attic entry holds the history its rollback moved aside: a fork of it opens to that
/// state.
fn check_attic(store: &Store, attic: &BTreeMap<String, (u64, Vec<String>)>) {
    let entries = store.attic_list().unwrap();
    assert_eq!(
        entries.iter().map(|e| e.id.clone()).collect::<Vec<_>>(),
        attic.keys().cloned().collect::<Vec<_>>()
    );
    for (id, (commit, state)) in attic {
        let fork = MemDir::new();
        store
            .attic_fork(id, &fork)
            .unwrap_or_else(|err| panic!("fork of {id}: {err}"));
        let forked = Store::open(&fork, StoreOptions::new()).unwrap();
        let graph = forked.graph().read();
        assert_eq!(graph.last_commit_seq(), *commit, "fork of {id}");
        assert_eq!(exact(&graph)[1..], state[..], "fork of {id}");
    }
}

/// The store verifies: no damage, the donor rule holds (format spec 12.3), every attic entry
/// rebuilds.
fn assert_verified(store: &Store) {
    let report = store.verify().unwrap();
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    assert!(
        report.without_donor.is_empty(),
        "{:?}",
        report.without_donor
    );
}

fn run_case(seed: u64) {
    if std::panic::catch_unwind(|| case(seed)).is_err() {
        panic!("merge fuzz case {seed} failed; replay it with MERGE_FUZZ_REPLAY={seed}");
    }
}

/// The campaign: `MERGE_FUZZ_CASES` cases from `MERGE_FUZZ_SEED` on, on `MERGE_FUZZ_THREADS`
/// threads (a small budget by default).
#[test]
fn random_histories_with_random_damage() {
    if let Ok(seed) = std::env::var("MERGE_FUZZ_REPLAY") {
        run_case(seed.parse().expect("MERGE_FUZZ_REPLAY=<seed>"));
        return;
    }
    let cases = env_u64("MERGE_FUZZ_CASES", 100);
    let first = env_u64("MERGE_FUZZ_SEED", 1);
    let threads = env_u64("MERGE_FUZZ_THREADS", 4).max(1);
    std::thread::scope(|scope| {
        for lane in 0..threads {
            scope.spawn(move || {
                let mut seed = first + lane;
                while seed < first + cases {
                    run_case(seed);
                    seed += threads;
                }
            });
        }
    });
}
