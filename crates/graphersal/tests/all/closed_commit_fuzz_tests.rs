//! Closed-store catalog commits (format spec 12.12) in seeded random histories: data commits,
//! live and closed checkpoints, prunes, rollbacks, marks and full or incremental backups mixed
//! with closed model imports, default changes and removals. After every step the store must
//! open to its oracle (the newest snapshot loaded and the WAL replayed: the closed commits are
//! ordinary commits), with the catalog the closed path reported, verify without a problem, and
//! a backup taken after a closed commit (an increment) must open to the same state.
//!
//! A fast budget runs in every `cargo test`; `CLOSED_FUZZ_CASES`, `CLOSED_FUZZ_SEED` and
//! `CLOSED_FUZZ_THREADS` widen it; a failing case prints `CLOSED_FUZZ_REPLAY=<seed>`.
#![cfg(all(feature = "persist", feature = "script", not(target_arch = "wasm32")))]

use std::sync::Arc;

use graphersal::catalog::DefinitionKind;
use graphersal::persist::{
    ClosedCommitOptions, Codec, ImportModelOptions, MemDir, ModelSource, PersistError,
    RecoveryTarget, RollbackMode, Store, StoreDir, StoreFailure, StoreOptions,
};
use graphersal::prelude::GraphStorage;

use crate::merge_checkpoint_tests::{Rng, exact, fake_data, history, oracle, same};
use crate::model_fixture::tiny_model;

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// The models of a graph's catalog as `(identity, default)`, sorted.
fn models_of<G: GraphStorage + ?Sized>(graph: &G) -> Vec<(String, bool)> {
    let mut out: Vec<(String, bool)> = graph
        .definitions()
        .unwrap()
        .filter(|d| d.kind() == DefinitionKind::MODEL)
        .map(|d| {
            (
                d.name().to_string(),
                d.as_model().is_some_and(|m| m.is_default()),
            )
        })
        .collect();
    out.sort();
    out
}

/// A closed catalog change; `Ok(())` also for the refusals a random request meets.
fn closed_change(dir: &MemDir, rng: &mut Rng) {
    let repo = ["acme/a", "acme/b"][rng.below(2)];
    let options = ClosedCommitOptions::new()
        .with_codec([Codec::None, Codec::Lz4][rng.below(2)])
        .with_wal_segment_bytes([400, 1 << 20][rng.below(2)]);
    let result: Result<(), PersistError> = match rng.below(4) {
        0 | 1 => Store::import_model_dir(
            dir,
            ModelSource::Files(tiny_model(1 + rng.below(2) as u8, repo)),
            ImportModelOptions::new()
                .with_replace(rng.below(2) == 0)
                .with_default(rng.below(2) == 0),
        )
        .map(|_| ()),
        2 => Store::set_default_model_dir(dir, (rng.below(3) != 0).then_some(repo), options)
            .map(|_| ()),
        _ => Store::remove_model_dir(dir, repo, options).map(|_| ()),
    };
    match result {
        Ok(()) => {}
        Err(PersistError::Store {
            cause: StoreFailure::UnknownModel | StoreFailure::ModelRevisionExists,
            ..
        }) => {}
        Err(err) => panic!("closed change: {err}"),
    }
}

/// The store in `dir` opens to its oracle, with the catalog the files hold, and verifies.
fn check(dir: &MemDir, options: &StoreOptions, label: &str) {
    let shared: Arc<dyn StoreDir> = Arc::new(dir.clone());
    let expected = oracle(&shared);
    let in_files: Vec<(String, bool)> = {
        let mut m: Vec<_> = Store::models_dir(dir)
            .unwrap()
            .into_iter()
            .map(|m| (m.identity, m.default))
            .collect();
        m.sort();
        m
    };
    let store = Store::open(dir, options.clone()).unwrap();
    same(&store.graph().read(), &expected, label);
    assert_eq!(
        models_of(&*store.graph().read()),
        models_of(&expected),
        "{label}"
    );
    assert_eq!(in_files, models_of(&expected), "{label}");
    let report = store.verify().unwrap();
    assert!(report.problems.is_empty(), "{label}: {:?}", report.problems);
    store.close().unwrap();
}

fn case(seed: u64) {
    let mut rng = Rng(seed);
    let options = StoreOptions::new()
        .with_checkpoint_after_bytes(0)
        .with_chunk_bytes(200 + rng.below(1500) as u32)
        .with_wal_segment_bytes(400 + rng.below(4000) as u64);
    let dir = MemDir::new();
    let store = Store::create(&dir, options.clone()).unwrap();
    fake_data(store.graph(), seed, 5 + rng.below(20));
    store.close().unwrap();
    let mut backup = MemDir::new();
    let mut round = 0;
    for step in 0..4 + rng.below(8) {
        let label = format!("seed {seed} step {step}");
        for _ in 0..1 + rng.below(3) {
            closed_change(&dir, &mut rng);
        }
        check(&dir, &options, &label);
        match rng.below(7) {
            0 => {
                Store::checkpoint_dir(&dir, options.clone(), None).unwrap();
            }
            1 => {
                // A backup (full, then increments) opens to the store's state.
                match Store::backup_dir(&dir, &backup) {
                    Ok(_) => {}
                    // A rollback behind the backup: a new full backup.
                    Err(PersistError::BackupRefused { .. }) => {
                        backup = MemDir::new();
                        Store::backup_dir(&dir, &backup).unwrap();
                    }
                    Err(err) => panic!("{label} backup: {err}"),
                }
                let restored = backup.deep_copy();
                Store::restore_backup(&restored).unwrap();
                let shared: Arc<dyn StoreDir> = Arc::new(dir.clone());
                let opened = Store::open(&restored, options.clone()).unwrap();
                assert_eq!(
                    exact(&opened.graph().read()),
                    exact(&oracle(&shared)),
                    "{label} backup"
                );
                assert_eq!(
                    models_of(&*opened.graph().read()),
                    models_of(&oracle(&shared)),
                    "{label} backup"
                );
                opened.close().unwrap();
            }
            _ => {
                let open = Store::open(&dir, options.clone()).unwrap();
                let rounds = 1 + rng.below(10);
                history(open.graph(), &mut rng, rounds, round);
                round += rounds;
                match rng.below(5) {
                    0 => {
                        open.checkpoint(None).unwrap();
                    }
                    1 => {
                        let last = open.snapshots().unwrap().pop().unwrap().commit_seq;
                        open.prune(last).unwrap();
                    }
                    2 => {
                        let current = open.graph().read().last_commit_seq();
                        let target = rng.below(current as usize + 1) as u64;
                        let mode = [RollbackMode::Move, RollbackMode::Delete][rng.below(2)];
                        let _ = open.rollback_to(RecoveryTarget::CommitSeq(target), mode);
                    }
                    3 => {
                        let _ = open.mark(&format!("m{round}"));
                    }
                    _ => {}
                }
                open.close().unwrap();
            }
        }
        check(&dir, &options, &label);
    }
}

fn run_case(seed: u64) {
    if std::panic::catch_unwind(|| case(seed)).is_err() {
        panic!("closed commit fuzz case {seed} failed; replay it with CLOSED_FUZZ_REPLAY={seed}");
    }
}

#[test]
fn random_histories_with_closed_catalog_commits() {
    if let Ok(seed) = std::env::var("CLOSED_FUZZ_REPLAY") {
        run_case(seed.parse().expect("CLOSED_FUZZ_REPLAY=<seed>"));
        return;
    }
    let cases = env_u64("CLOSED_FUZZ_CASES", 12);
    let first = env_u64("CLOSED_FUZZ_SEED", 1);
    let threads = env_u64("CLOSED_FUZZ_THREADS", 4).max(1);
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
