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
    SeriesKey::new(SeriesKind::Lock("query".into()), Channel::Mcp)
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
    // Every tier survives the restart (the minute tier too).
    assert_eq!(report.series[0].last_hour.count, 2);
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

// ---------------------------------------------------------------------------------------------
// Write-lock time (0b stage B)
// ---------------------------------------------------------------------------------------------

fn lock_totals(store: &Store, name: &str, channel: Channel) -> graphersal::workload::Counters {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_micros() as i64;
    store
        .workload_stats()
        .report(now)
        .series
        .into_iter()
        .find(|s| s.key == SeriesKey::new(SeriesKind::Lock(name.into()), channel))
        .map(|s| s.last_hour)
        .unwrap_or_default()
}

#[test]
fn writes_count_their_wait_and_hold_under_the_channel() {
    let dir = MemDir::named("memory:lock");
    let store = new_store(&dir, WorkloadLevel::Memory);
    store.workload_stats().clear(None);
    store
        .graph()
        .write()
        .traversal_mut()
        .add_v("x")
        .to_list()
        .unwrap();
    assert_eq!(lock_totals(&store, "write.wait", Channel::Host).count, 1);
    assert_eq!(lock_totals(&store, "write.hold", Channel::Host).count, 1);
    graphersal::workload::scope(Channel::Mcp, false, || {
        store
            .graph()
            .write()
            .traversal_mut()
            .add_v("y")
            .to_list()
            .unwrap();
    });
    assert_eq!(lock_totals(&store, "write.hold", Channel::Mcp).count, 1);
    // Technical work is not counted; an uncontended read neither.
    graphersal::workload::scope(Channel::Playground, true, || {
        store
            .graph()
            .write()
            .traversal_mut()
            .add_v("z")
            .to_list()
            .unwrap();
    });
    let _ = store.graph().read().vertex_count();
    let report_len = |store: &Store| {
        store
            .workload_stats()
            .report(T0)
            .series
            .iter()
            .filter(|s| s.key.channel == Channel::Playground)
            .count()
    };
    assert_eq!(report_len(&store), 0);
    assert_eq!(lock_totals(&store, "read.wait", Channel::Host).count, 0);
    // Off: nothing at all.
    store.workload_stats().set_level(WorkloadLevel::Off);
    store
        .graph()
        .write()
        .traversal_mut()
        .add_v("w")
        .to_list()
        .unwrap();
    assert_eq!(lock_totals(&store, "write.hold", Channel::Host).count, 1);
}

#[test]
fn contention_shows_as_wait_time() {
    use std::sync::Arc;
    use std::time::Duration;
    let dir = MemDir::named("memory:contention");
    let store = Arc::new(new_store(&dir, WorkloadLevel::Memory));
    store.workload_stats().clear(None);
    let holder = {
        let store = store.clone();
        let (held_tx, held_rx) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            let guard = store.graph().write();
            held_tx.send(()).unwrap();
            std::thread::sleep(Duration::from_millis(60));
            drop(guard);
        });
        held_rx.recv().unwrap();
        handle
    };
    let reader = {
        let store = store.clone();
        std::thread::spawn(move || store.graph().read().vertex_count())
    };
    let _ = store.graph().write();
    holder.join().unwrap();
    reader.join().unwrap();
    let wait = lock_totals(&store, "write.wait", Channel::Host);
    assert!(wait.max_micros >= 20_000, "{wait:?}");
    let hold = lock_totals(&store, "write.hold", Channel::Host);
    assert!(hold.max_micros >= 50_000, "{hold:?}");
    let read = lock_totals(&store, "read.wait", Channel::Host);
    assert_eq!(read.count, 1, "{read:?}");
}

#[test]
fn a_graph_without_statistics_counts_nothing() {
    let graph = graphersal::storage::Graph::new(TraversalGraph::tinkerpop_modern());
    assert!(graph.workload_stats().is_none());
    graph.write().traversal_mut().add_v("x").to_list().unwrap();
    let stats = std::sync::Arc::new(graphersal::workload::WorkloadStats::new(
        WorkloadLevel::Memory,
    ));
    assert!(graph.attach_workload_stats(stats.clone()));
    assert!(!graph.attach_workload_stats(stats.clone()), "attached once");
    graph.write().traversal_mut().add_v("y").to_list().unwrap();
    // write.wait, write.hold and the query itself (stage C).
    let kinds: Vec<SeriesKind> = stats
        .report(0)
        .series
        .into_iter()
        .map(|s| s.key.kind)
        .collect();
    assert_eq!(kinds.len(), 3, "{kinds:?}");
    assert!(kinds.iter().any(|k| matches!(k, SeriesKind::Query(_))));
}

// ---------------------------------------------------------------------------------------------
// Query statistics (0b stage C)
// ---------------------------------------------------------------------------------------------

fn queries(store: &Store) -> Vec<(String, Channel, graphersal::workload::Counters)> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_micros() as i64;
    store
        .workload_stats()
        .report(now)
        .series
        .into_iter()
        .filter_map(|s| match &s.key.kind {
            SeriesKind::Query(text) => Some((text.to_string(), s.key.channel, s.last_hour)),
            _ => None,
        })
        .collect()
}

#[test]
fn runs_count_under_their_normalized_plan() {
    let dir = MemDir::named("memory:queries");
    let store = new_store(&dir, WorkloadLevel::Memory);
    for name in ["marko", "vadas", "nobody"] {
        let graph = store.graph().read();
        graph
            .traversal()
            .v(None)
            .has("name", name)
            .values("age")
            .to_list()
            .unwrap();
    }
    let found = queries(&store);
    assert_eq!(found.len(), 1, "{found:?}");
    let (text, channel, counters) = &found[0];
    assert_eq!(text, r#"v().has(?, ?).values(?)"#);
    assert_eq!(*channel, Channel::Host);
    assert_eq!(counters.count, 3);
    assert_eq!(
        counters.items, 2,
        "29 and 27: two results over the three runs"
    );

    // A failing run counts as failed; technical work not at all; another channel apart.
    graphersal::workload::scope(Channel::Mcp, false, || {
        let graph = store.graph().read();
        let _ = graph.traversal().v(None).values("name").as_bool().to_list();
    });
    graphersal::workload::scope(Channel::Playground, true, || {
        let graph = store.graph().read();
        graph.traversal().v(None).count().next().unwrap();
    });
    let found = queries(&store);
    assert_eq!(found.len(), 2, "{found:?}");
    let mcp = found.iter().find(|(_, c, _)| *c == Channel::Mcp).unwrap();
    assert_eq!((mcp.2.count, mcp.2.failed), (1, 1));
}

#[test]
fn a_saved_query_call_counts_under_its_name() {
    use std::sync::Arc;
    let dir = MemDir::named("memory:saved");
    let store = new_store(&dir, WorkloadLevel::Memory);
    let graph: Arc<graphersal::storage::Graph> = store.graph().clone();
    let _ = crate::script_host::eval_value(
        graph.clone(),
        r#"g.define_query(#{name: "people", body: "fn people() { g.v().has_label(\"person\") }"})"#,
    )
    .unwrap();
    store.workload_stats().clear(None);
    let _ = crate::script_host::eval_value(graph, r#"g.query("people").values("name").to_list()"#)
        .unwrap();
    let found = queries(&store);
    let names: Vec<&str> = found.iter().map(|(t, _, _)| t.as_str()).collect();
    assert!(names.contains(&"saved:people"), "{names:?}");
    assert!(
        names
            .iter()
            .any(|t| t.starts_with("v(") && t.contains("values(?)")),
        "the plan it returned runs and counts too: {names:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// Property statistics (0b stage D)
// ---------------------------------------------------------------------------------------------

/// `(element, label, key, access, executions, items)` of every property series, sorted.
fn property_uses(store: &Store) -> Vec<(String, String, String, String, u64, u64)> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_micros() as i64;
    let mut out: Vec<_> = store
        .workload_stats()
        .report(now)
        .series
        .into_iter()
        .filter_map(|s| match &s.key.kind {
            SeriesKind::Property(p) => Some((
                p.element.map_or("*", |e| e.as_str()).to_string(),
                p.label.as_deref().unwrap_or("*").to_string(),
                p.key.to_string(),
                p.access.name().to_string(),
                s.last_hour.count,
                s.last_hour.items,
            )),
            _ => None,
        })
        .collect();
    out.sort();
    out
}

fn run_script(store: &Store, script: &str) {
    let _ = crate::script_host::eval_value(store.graph().clone(), script)
        .unwrap_or_else(|err| panic!("{script}: {err}"));
}

fn uses_of(store: &Store, script: &str) -> Vec<(String, String, String, String, u64, u64)> {
    store.workload_stats().clear(None);
    run_script(store, script);
    property_uses(store)
}

fn row(
    e: &str,
    l: &str,
    k: &str,
    a: &str,
    n: u64,
    items: u64,
) -> (String, String, String, String, u64, u64) {
    (e.into(), l.into(), k.into(), a.into(), n, items)
}

#[test]
fn filters_and_reads_count_per_label_and_key() {
    let dir = MemDir::named("memory:props");
    let store = new_store(&dir, WorkloadLevel::Memory);
    let uses = uses_of(
        &store,
        r#"g.v().has_label("person").has("age", P.gt(30)).values("name").to_list()"#,
    );
    assert_eq!(
        uses,
        [
            // 4 persons reach the filter, 2 (josh 32, peter 35) reach values().
            row("vertex", "person", "age", "filter_scan", 1, 4),
            row("vertex", "person", "name", "read", 1, 2),
        ]
    );
    // Order, project, element_map, a child traversal.
    let uses = uses_of(
        &store,
        r#"g.v().has_label("person").order().by("age").project("n").by("name").to_list()"#,
    );
    assert!(
        uses.contains(&row("vertex", "person", "age", "order_group", 1, 4)),
        "{uses:?}"
    );
    assert!(
        uses.contains(&row("vertex", "person", "name", "read", 1, 4)),
        "{uses:?}"
    );
    let uses = uses_of(
        &store,
        r#"g.e().has_label("knows").element_map().to_list()"#,
    );
    assert_eq!(uses, [row("edge", "knows", "*", "read", 1, 2)]);
    // A filter the optimizer unnests is a top-level step; one inside a child has no flow.
    let uses = uses_of(&store, r#"g.v().where(__.has("lang")).to_list()"#);
    assert_eq!(uses, [row("vertex", "*", "lang", "filter_scan", 1, 6)]);
    let uses = uses_of(&store, r#"g.v().where(__.out().has("lang")).to_list()"#);
    assert_eq!(uses, [row("*", "*", "lang", "filter_scan", 1, 0)]);
}

#[test]
fn an_index_served_filter_and_writes_count_as_such() {
    let dir = MemDir::named("memory:props-index");
    let store = new_store(&dir, WorkloadLevel::Memory);
    run_script(&store, r#"g.create_index(#{label: "person", key: "name"})"#);
    let uses = uses_of(
        &store,
        r#"g.v().has_label("person").has("name", "marko").values("age").to_list()"#,
    );
    assert!(
        uses.contains(&row("vertex", "person", "name", "filter_index", 1, 0)),
        "{uses:?}"
    );
    assert!(!uses.iter().any(|u| u.3 == "filter_scan"), "{uses:?}");
    let uses = uses_of(
        &store,
        r#"g.add_v("person").property("name", "ann").property("age", 1).iterate()"#,
    );
    assert!(
        uses.iter().any(|u| u.2 == "name" && u.3 == "write"),
        "{uses:?}"
    );
    assert!(
        uses.iter().any(|u| u.2 == "age" && u.3 == "write"),
        "{uses:?}"
    );
    let uses = uses_of(
        &store,
        r#"g.v().has_label("person").remove_property("age").iterate()"#,
    );
    assert!(
        uses.contains(&row("vertex", "person", "age", "remove", 1, 5)),
        "{uses:?}"
    );
}

/// Every step that takes a property key declares how it uses it (a new key-taking step needs a
/// `property_access` and a line here).
#[test]
fn every_key_taking_step_declares_its_property_access() {
    let dir = MemDir::named("memory:props-coverage");
    let store = new_store(&dir, WorkloadLevel::Memory);
    run_script(
        &store,
        r#"g.add_v("doc").property("emb", vector([1.0, 0.0])).property("meta", #{a: 1}).iterate()"#,
    );
    let cases: &[(&str, &str, &str)] = &[
        (r#"g.v().has("age", 29).to_list()"#, "age", "filter_scan"),
        (
            r#"g.v().has("age", P.gt(1)).to_list()"#,
            "age",
            "filter_scan",
        ),
        (r#"g.v().has("age").to_list()"#, "age", "filter_scan"),
        (r#"g.v().has_not("age").to_list()"#, "age", "filter_scan"),
        (
            r#"g.v().has(jpath("meta.a"), 1).to_list()"#,
            "meta",
            "filter_scan",
        ),
        (
            r#"g.v().has("name", __.to_lower().is("marko")).to_list()"#,
            "name",
            "filter_scan",
        ),
        (r#"g.v().values("name").to_list()"#, "name", "read"),
        (r#"g.v().values(jpath("meta.a")).to_list()"#, "meta", "read"),
        (r#"g.v().properties("name").to_list()"#, "name", "read"),
        (r#"g.v().value_map("name").to_list()"#, "name", "read"),
        (r#"g.v().value_map().to_list()"#, "*", "read"),
        (r#"g.v().element_map("age").to_list()"#, "age", "read"),
        (
            r#"g.v().has_label("doc").similarity("emb", vector([1.0, 0.0])).to_list()"#,
            "emb",
            "similarity",
        ),
        (
            r#"g.v().has_label("doc").nearest("emb", vector([1.0, 0.0]), 1).to_list()"#,
            "emb",
            "similarity",
        ),
        (r#"g.v().group().by("age").to_list()"#, "age", "order_group"),
        (
            r#"g.v().dedup().by("lang").to_list()"#,
            "lang",
            "order_group",
        ),
        (
            r#"g.v().order().by("name").to_list()"#,
            "name",
            "order_group",
        ),
        (
            r#"g.v().fold().order(Scope.local).by("name").to_list()"#,
            "name",
            "order_group",
        ),
        (r#"g.v().project("a").by("age").to_list()"#, "age", "read"),
        (
            r#"g.v().as_("a").select("a").by("name").to_list()"#,
            "name",
            "read",
        ),
        (r#"g.v().out().path().by("name").to_list()"#, "name", "read"),
        (r#"g.v().out().tree().by("name").to_list()"#, "name", "read"),
        (
            r#"g.v().has_label("doc").property("x", 1).iterate()"#,
            "x",
            "write",
        ),
        (
            r#"g.v().has_label("doc").property(#{y: 1}).iterate()"#,
            "y",
            "write",
        ),
        (
            r#"g.v().has_label("doc").remove_property("x").iterate()"#,
            "x",
            "remove",
        ),
    ];
    for (script, key, access) in cases {
        let uses = uses_of(&store, script);
        assert!(
            uses.iter().any(|u| u.2 == *key && u.3 == *access),
            "{script}: expected {key} {access} in {uses:?}"
        );
    }
}

/// A job's queries count under the `job` channel, whatever the host's channel is.
#[test]
fn a_jobs_queries_count_under_the_job_channel() {
    use graphersal::jobs::{Job, JobKind};
    let dir = MemDir::named("memory:job-channel");
    let store = new_store(&dir, WorkloadLevel::Memory);
    let graph = store.graph().clone();
    let mut scheduler = Scheduler::new().with_stats(store.workload_stats());
    scheduler
        .add(Job::new(
            "count_people",
            JobKind::User,
            "* * * * *".parse().unwrap(),
            move |_| {
                graph
                    .read()
                    .traversal()
                    .v(None)
                    .has_label("person")
                    .count()
                    .to_list()
                    .map(|_| ())
                    .map_err(|err| err.to_string())
            },
        ))
        .unwrap();
    graphersal::workload::scope(Channel::Mcp, false, || {
        scheduler.tick(T0);
        assert_eq!(scheduler.tick(T0 + MINUTE), ["count_people"]);
    });
    let runs = queries(&store);
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert_eq!(runs[0].1, Channel::Job);
}

/// A report with history gives every series' buckets of one tier within its window, oldest
/// first, each starting on its minute, hour or day; a plain report has none.
#[test]
fn a_report_with_history_gives_the_buckets_of_the_tier() {
    use graphersal::workload::HistoryTier;
    let dir = MemDir::named("memory:history");
    let store = new_store(&dir, WorkloadLevel::Memory);
    let stats = store.workload_stats();
    const HOUR: i64 = 60 * MINUTE;
    // In time order: two hours ago, ten minutes ago (twice), now.
    stats.record(key(), T0 - 2 * HOUR, Sample::new(4, 700));
    stats.record(key(), T0 - 10 * MINUTE, Sample::new(2, 300));
    stats.record(key(), T0 - 10 * MINUTE + 1, Sample::new(3, 500));
    stats.record(key(), T0, Sample::new(1, 100));
    let minutes = stats.report_with_history(T0, HistoryTier::Minutes);
    assert_eq!(minutes.history, Some(HistoryTier::Minutes));
    let history = &minutes.series[0].history;
    // The bucket two hours ago is outside the 60-minute window.
    assert_eq!(history.len(), 2, "{history:?}");
    assert!(history[0].start < history[1].start);
    assert_eq!(history[0].start % MINUTE, 0);
    assert_eq!(history[0].counters.count, 2);
    assert_eq!(history[0].counters.max_micros, 500);
    assert_eq!(history[1].counters.count, 1);
    let hours = stats.report_with_history(T0, HistoryTier::Hours);
    let total: u64 = hours.series[0]
        .history
        .iter()
        .map(|b| b.counters.count)
        .sum();
    assert_eq!(total, 4);
    assert!(hours.series[0].history.iter().all(|b| b.start % HOUR == 0));
    assert!(stats.report(T0).series[0].history.is_empty());
    assert_eq!(stats.report(T0).history, None);
}
