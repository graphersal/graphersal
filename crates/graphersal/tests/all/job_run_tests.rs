//! Running the scheduled jobs of users (book page "Scheduled Jobs"): `Store::user_jobs` turns
//! the catalog's jobs into scheduler jobs; a query job calls its saved query as one unit carrying
//! `{job: <name>}`, under the effects stored with it; checkpoint and backup jobs work on the
//! store; disabled, dropped and changed jobs follow the catalog through `sync_user_jobs`.

use std::sync::{Arc, Mutex};

use graphersal::changes::{ChangeSet, CommitHook};
use graphersal::jobs::Scheduler;
use graphersal::persist::{MemDir, Store, StoreOptions};
use graphersal::prelude::*;
use graphersal::workload::{Channel, SeriesKind, WorkloadLevel};

use crate::script_host::eval_value;

const T0: i64 = 1_800_000_000_000_000;
const MINUTE: i64 = 60_000_000;

fn store(name: &str) -> Store {
    Store::create_with(
        MemDir::named(name),
        TraversalGraph::tinkerpop_modern(),
        StoreOptions::new().with_workload_level(WorkloadLevel::Memory),
    )
    .unwrap()
}

fn run(store: &Store, script: &str) {
    let _ =
        eval_value(store.graph().clone(), script).unwrap_or_else(|err| panic!("{script}: {err}"));
}

/// A scheduler with the store's jobs, planned at `T0`.
fn scheduler(store: &Store) -> Scheduler {
    let mut scheduler = Scheduler::new().with_stats(store.workload_stats());
    for job in store.system_jobs() {
        scheduler.add(job).unwrap();
    }
    scheduler.sync_user_jobs(store.user_jobs());
    scheduler.tick(T0);
    scheduler
}

fn count(store: &Store, label: &str) -> usize {
    store
        .graph()
        .read()
        .traversal()
        .v(None)
        .has_label(label)
        .to_list()
        .unwrap()
        .len()
}

/// Records the `job` attribute of every commit's metadata.
struct JobNames(Arc<Mutex<Vec<String>>>);

impl CommitHook for JobNames {
    fn name(&self) -> &str {
        "job-names"
    }

    fn after_commit(&mut self, changes: &ChangeSet) {
        if let Some(job) = changes.metadata().attributes().get("job") {
            self.0
                .lock()
                .unwrap()
                .push(job.as_str().unwrap_or_default().to_string());
        }
    }
}

#[test]
fn a_query_job_runs_its_saved_query_as_one_unit_named_after_the_job() {
    let store = store("memory:job-run-query");
    run(
        &store,
        r#"g.define_query(#{name: "purge", params: #{label: "string"},
            body: "fn purge(label) { g.v().has_label(label).drop() }"});
           g.define_job(#{name: "nightly_purge", schedule: "* * * * *", action: "query",
            query: "purge", args: #{label: "software"}})"#,
    );
    let names = Arc::new(Mutex::new(Vec::new()));
    store
        .graph()
        .write()
        .register_commit_hook(Box::new(JobNames(names.clone())))
        .unwrap();
    let mut scheduler = scheduler(&store);
    assert_eq!(count(&store, "software"), 2);
    assert!(
        scheduler
            .tick(T0 + MINUTE)
            .contains(&"nightly_purge".to_string())
    );
    assert_eq!(count(&store, "software"), 0);
    assert_eq!(count(&store, "person"), 4);
    assert_eq!(*names.lock().unwrap(), ["nightly_purge"]);
    // The run is in the job block, its queries under the `job` channel.
    let now = T0 + MINUTE;
    let report = store.workload_stats().report(now);
    let (_, last) = report
        .jobs
        .iter()
        .find(|(name, _)| name == "nightly_purge")
        .unwrap();
    assert_eq!(last.error, None);
    assert!(
        report.series.iter().any(
            |s| matches!(&s.key.kind, SeriesKind::Query(q) if q.as_str() == "saved:purge")
                && s.key.channel == Channel::Job
        ),
        "{report:?}"
    );
}

#[test]
fn a_query_that_now_writes_more_than_the_job_was_allowed_fails_and_changes_nothing() {
    let store = store("memory:job-run-effects");
    run(
        &store,
        r#"g.define_query(#{name: "cleanup", body: "fn cleanup() { g.e().has_label(\"created\").drop() }"});
           g.define_job(#{name: "cleanup_job", schedule: "* * * * *", action: "query", query: "cleanup"})"#,
    );
    // The job may delete `created` edges. The query is changed to delete every vertex; the job
    // keeps the effects it was defined with.
    run(
        &store,
        r#"g.define_query(#{name: "cleanup", body: "fn cleanup() { g.v().drop() }"})"#,
    );
    let mut scheduler = scheduler(&store);
    assert!(
        scheduler
            .tick(T0 + MINUTE)
            .contains(&"cleanup_job".to_string())
    );
    assert_eq!(
        store.graph().read().vertex_count(),
        6,
        "nothing was dropped"
    );
    let report = store.workload_stats().report(T0 + MINUTE);
    let (_, last) = report
        .jobs
        .iter()
        .find(|(name, _)| name == "cleanup_job")
        .unwrap();
    let error = last.error.clone().unwrap();
    assert!(error.contains("define the job again"), "{error}");
}

#[test]
fn checkpoint_and_backup_jobs_work_on_the_store() {
    let store = store("memory:job-run-store");
    let backup_dir =
        std::env::temp_dir().join(format!("graphersal-job-backup-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&backup_dir);
    run(
        &store,
        &format!(
            r#"g.define_job(#{{name: "cp", schedule: "* * * * *", action: "checkpoint"}});
               g.define_job(#{{name: "bk", schedule: "* * * * *", action: "backup", path: {:?}}})"#,
            backup_dir.to_string_lossy()
        ),
    );
    let snapshots = store.snapshots().unwrap().len();
    run(
        &store,
        r#"g.add_v("person").property("name", "ann").iterate()"#,
    );
    let mut scheduler = scheduler(&store);
    let mut ran = scheduler.tick(T0 + MINUTE);
    ran.sort();
    assert_eq!(ran, ["bk", "cp", "stats_flush"]);
    assert_eq!(store.snapshots().unwrap().len(), snapshots + 1);
    let backup = Store::open_backup(backup_dir.as_path(), StoreOptions::new()).unwrap();
    assert_eq!(backup.graph().read().vertex_count(), 7);
    backup.close().unwrap();
    // The next run is an increment.
    run(
        &store,
        r#"g.add_v("person").property("name", "bob").iterate()"#,
    );
    scheduler.tick(T0 + 2 * MINUTE);
    let backup = Store::open_backup(backup_dir.as_path(), StoreOptions::new()).unwrap();
    assert_eq!(backup.graph().read().vertex_count(), 8);
    backup.close().unwrap();
    let _ = std::fs::remove_dir_all(&backup_dir);
}

#[test]
fn the_scheduler_follows_the_catalog() {
    let store = store("memory:job-run-sync");
    run(
        &store,
        r#"g.define_job(#{name: "a", schedule: "* * * * *", action: "checkpoint"});
           g.define_job(#{name: "b", schedule: "* * * * *", action: "checkpoint", enabled: false})"#,
    );
    let mut scheduler = scheduler(&store);
    let names = |s: &Scheduler| {
        let mut names: Vec<(String, bool)> =
            s.jobs().into_iter().map(|j| (j.name, j.enabled)).collect();
        names.sort();
        names
    };
    assert_eq!(
        names(&scheduler),
        [
            ("a".to_string(), true),
            ("b".to_string(), false),
            ("compact".to_string(), false),
            ("stats_flush".to_string(), true)
        ]
    );
    let ran = scheduler.tick(T0 + MINUTE);
    assert!(
        ran.contains(&"a".to_string()) && !ran.contains(&"b".to_string()),
        "{ran:?}"
    );
    // Dropped in the catalog: gone after the next sync; the system jobs stay.
    run(&store, r#"g.drop_job("a")"#);
    scheduler.sync_user_jobs(store.user_jobs());
    assert_eq!(names(&scheduler).len(), 3);
    assert!(!names(&scheduler).iter().any(|(n, _)| n == "a"));
}

#[test]
fn a_system_jobs_settings_survive_a_reopen() {
    use graphersal::workload::JobOverride;
    let dir = MemDir::named("memory:job-run-settings");
    let store = Store::create_with(
        &dir,
        TraversalGraph::tinkerpop_modern(),
        StoreOptions::new(),
    )
    .unwrap();
    store.workload_stats().set_job_override(
        "compact",
        JobOverride::new()
            .with_schedule("30 1 * * 0")
            .with_enabled(true),
    );
    // A schedule that does not parse is ignored: the built-in one stays.
    store
        .workload_stats()
        .set_job_override("stats_flush", JobOverride::new().with_schedule("never"));
    store.close().unwrap();
    let store = Store::open(&dir, StoreOptions::new()).unwrap();
    let jobs = store.system_jobs();
    let compact = jobs.iter().find(|j| j.name() == "compact").unwrap();
    assert!(compact.enabled());
    assert_eq!(compact.schedule().to_string(), "30 1 * * 0");
    let flush = jobs.iter().find(|j| j.name() == "stats_flush").unwrap();
    assert_eq!(flush.schedule().to_string(), "* * * * *");
    store.close().unwrap();
}
