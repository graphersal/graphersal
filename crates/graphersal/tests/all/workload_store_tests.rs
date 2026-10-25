//! The workload statistics of a Store (book page "Workload Statistics") and its system jobs
//! (book page "Scheduled Jobs"): persisted in the sidecar `WORKLOAD` (written by the job
//! `stats_flush` and at close, read back at open), never written at level `memory`/`off` or by
//! a store that writes nothing (a backup), never copied into a backup, a damaged sidecar starts
//! anew without harming the store, verify and prune ignore it, `compact` releases spare slots.
#![cfg(feature = "persist")]

use graphersal::jobs::Scheduler;
use graphersal::persist::{MemDir, Store, StoreDir, StoreOptions};
use graphersal::prelude::*;
use graphersal::workload::{Channel, Sample, SeriesKey, SeriesKind, WorkloadLevel};

const T0: i64 = 1_800_000_000_000_000;
const MINUTE: i64 = 60_000_000;

fn key() -> SeriesKey {
    SeriesKey::new(SeriesKind::WriteLock("query".into()), Channel::Mcp)
}

fn new_store(dir: &MemDir, level: WorkloadLevel) -> Store {
    Store::create_with(
        dir,
        TraversalGraph::tinkerpop_modern(),
        StoreOptions::new().with_workload_level(level),
    )
    .unwrap()
}

fn open(dir: &MemDir, level: WorkloadLevel) -> Store {
    Store::open(dir, StoreOptions::new().with_workload_level(level)).unwrap()
}

fn has_sidecar(dir: &MemDir) -> bool {
    dir.file_names().iter().any(|name| name == "WORKLOAD")
}

#[test]
fn statistics_survive_a_restart_through_the_sidecar() {
    let dir = MemDir::named("memory:workload");
    let store = new_store(&dir, WorkloadLevel::Persisted);
    let stats = store.workload_stats();
    assert_eq!(stats.level(), WorkloadLevel::Persisted);
    stats.record(key(), T0, Sample::new(3, 120));
    assert!(store.flush_workload_stats().unwrap());
    assert!(has_sidecar(&dir));
    assert!(
        !store.flush_workload_stats().unwrap(),
        "nothing new: nothing written"
    );
    stats.record(key(), T0 + MINUTE, Sample::new(1, 80));
    store.close().unwrap();

    let again = open(&dir, WorkloadLevel::Persisted);
    let report = again.workload_stats().report(T0 + MINUTE);
    assert_eq!(report.series.len(), 1);
    let days = report.series[0].last_ninety_days;
    assert_eq!((days.count, days.items, days.total_micros), (2, 4, 200));
    // The minute tier is not persisted.
    assert_eq!(report.series[0].last_hour.count, 0);
}

#[test]
fn memory_and_off_write_no_sidecar() {
    for level in [WorkloadLevel::Memory, WorkloadLevel::Off] {
        let dir = MemDir::named("memory:levels");
        let store = new_store(&dir, level);
        store.workload_stats().record(key(), T0, Sample::new(1, 1));
        assert!(!store.flush_workload_stats().unwrap());
        store.close().unwrap();
        assert!(!has_sidecar(&dir), "{level:?}");
    }
    // A persisted sidecar is not read at level memory (the statistics start empty).
    let dir = MemDir::named("memory:levels2");
    let store = new_store(&dir, WorkloadLevel::Persisted);
    store.workload_stats().record(key(), T0, Sample::new(1, 1));
    store.close().unwrap();
    let memory = open(&dir, WorkloadLevel::Memory);
    assert!(memory.workload_stats().report(T0).series.is_empty());
}

#[test]
fn a_damaged_sidecar_starts_anew_and_the_store_opens() {
    let dir = MemDir::named("memory:damaged");
    let store = new_store(&dir, WorkloadLevel::Persisted);
    store.workload_stats().record(key(), T0, Sample::new(1, 1));
    store.close().unwrap();
    dir.write_atomic("WORKLOAD", b"not statistics").unwrap();
    let again = open(&dir, WorkloadLevel::Persisted);
    assert!(again.workload_stats().report(T0).series.is_empty());
    assert!(again.verify().unwrap().problems.is_empty());
    // The next flush replaces it.
    again.workload_stats().record(key(), T0, Sample::new(1, 1));
    assert!(again.flush_workload_stats().unwrap());
}

#[test]
fn verify_prune_and_backups_leave_the_sidecar_alone() {
    let dir = MemDir::named("memory:maintenance");
    let store = new_store(&dir, WorkloadLevel::Persisted);
    store.workload_stats().record(key(), T0, Sample::new(1, 1));
    store.flush_workload_stats().unwrap();
    store.checkpoint(None).unwrap();
    assert!(store.verify().unwrap().problems.is_empty());
    let up_to = store.graph().read().last_commit_seq();
    store.prune(up_to).unwrap();
    assert!(has_sidecar(&dir), "prune keeps the sidecar");
    let backup = MemDir::named("memory:backup");
    store.backup(&backup).unwrap();
    assert!(!has_sidecar(&backup), "a backup holds no statistics");
    // A backup opened read-only keeps statistics in memory and writes nothing.
    let opened = Store::open_backup(&backup, StoreOptions::new()).unwrap();
    assert_eq!(opened.workload_stats().level(), WorkloadLevel::Memory);
    opened.workload_stats().record(key(), T0, Sample::new(1, 1));
    assert!(!opened.flush_workload_stats().unwrap());
    opened.close().unwrap();
    assert!(!has_sidecar(&backup));
}

#[test]
fn the_system_jobs_flush_and_compact() {
    let dir = MemDir::named("memory:jobs");
    let store = new_store(&dir, WorkloadLevel::Persisted);
    let mut scheduler = Scheduler::new().with_stats(store.workload_stats());
    for job in store.system_jobs() {
        scheduler.add(job).unwrap();
    }
    let jobs = scheduler.jobs();
    let names: Vec<(&str, bool)> = jobs
        .iter()
        .map(|job| (job.name.as_str(), job.enabled))
        .collect();
    assert_eq!(names, [("stats_flush", true), ("compact", false)]);
    store.workload_stats().record(key(), T0, Sample::new(1, 1));
    scheduler.tick(T0);
    assert_eq!(scheduler.tick(T0 + MINUTE), ["stats_flush"]);
    assert!(has_sidecar(&dir));
    // The run itself is in the statistics' job block.
    let report = store.workload_stats().report(T0 + MINUTE);
    assert_eq!(report.jobs[0].0, "stats_flush");
    assert_eq!(report.jobs[0].1.error, None);

    // compact (switched on, run by hand) releases spare slot capacity.
    for i in 0..50 {
        store
            .graph()
            .write()
            .traversal_mut()
            .add_v("filler")
            .property("i", i as i64)
            .to_list()
            .unwrap();
    }
    let before = store.graph().read().memory_structure().unwrap();
    scheduler.set_enabled("compact", true).unwrap();
    scheduler.run_now("compact", T0 + 2 * MINUTE).unwrap();
    let after = store.graph().read().memory_structure().unwrap();
    assert!(before.vertex_slots.spare > 0, "{before:?}");
    assert_eq!(after.vertex_slots.spare, 0);
}

/// The single-file store (`.gstore`, the knowledge graph's form) keeps the sidecar inside its
/// container.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_single_file_store_keeps_the_statistics_inside() {
    let dir = std::env::temp_dir().join(format!("graphersal-workload-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("stats.gstore");
    let store = Store::create(&path, StoreOptions::new()).unwrap();
    store.workload_stats().record(key(), T0, Sample::new(2, 9));
    store.close().unwrap();
    let again = Store::open(&path, StoreOptions::new()).unwrap();
    let report = again.workload_stats().report(T0);
    assert_eq!(report.series[0].last_ninety_days.items, 2);
    again.close().unwrap();
    // One file, nothing beside it.
    let entries: Vec<_> = std::fs::read_dir(&dir).unwrap().collect();
    assert_eq!(entries.len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}
