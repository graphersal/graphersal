# Scheduled Jobs

The engine runs background work on a schedule: **jobs** with a standard 5-field cron schedule
(`minute hour day-of-month month day-of-week`, e.g. `*/5 * * * *` for every five minutes,
`0 3 * * *` for 03:00 every day; ranges `1-5`, steps `*/15`, lists `1,15`, names `jan`/`mon`).
Times are wall-clock times in the instance's local time zone.

## Who drives the clock

The engine owns no thread: the **host** calls the scheduler's `tick` periodically with the
current time, and the scheduler runs the jobs that are due.

| Host | Ticks |
|---|---|
| Dev server (`graphersal --server`) | a thread, every 5 seconds, in the machine's time zone (`TZ` or `/etc/localtime`) |
| Browser playground | a timer in the engine's worker, every 5 seconds, in the browser's time zone |
| Your own program | call `Scheduler::tick(now)` yourself |

A job never overlaps itself: due jobs run one after another. A time missed while nothing ticked
(the process was stopped) is skipped: the job runs at its next scheduled time. A failing job
(an error, a panic) is recorded and runs again at its next time.

## Limits per run

A job may limit each of its runs: a **timeout** and a **memory limit** (`JobLimits`, both
optional). The engine cannot interrupt arbitrary code, so the limits reach the checks every
traversal already makes: while the job runs, its remaining time caps each traversal's
`evaluationTimeout` and its memory limit caps `memory.limit` (a query's own stricter value
stays). A breach fails the traversal as any timeout or memory limit does, its work is rolled
back, and the run is recorded as failed with the reason. An action doing long work of its own
checks `limits.timeout` itself; if it finishes past its timeout anyway, the run is recorded as
over time.

## System jobs

A Store brings its system jobs (`Store::system_jobs()`):

| Job | Schedule | Default | What it does |
|---|---|---|---|
| `stats_flush` | every minute | on | writes the [workload statistics](workload_statistics.md) to the sidecar |
| `compact` | 03:00 daily | off | releases spare capacity of the in-memory structure |

System jobs cannot be removed; their schedule and switch can be changed. Every run (start,
duration, error) is recorded in the workload statistics.

## Jobs of users

A user's job is a catalog definition (kind `job`, beside saved queries, compression rules and
property indexes): it is versioned with the graph, written to the journal, carried by
checkpoints, backups and the catalog file. Its action is one of

| Action | What a run does |
|---|---|
| `query` | calls a [saved query](../users_guide/saved_queries.md) with fixed arguments |
| `checkpoint` | writes a checkpoint of the store |
| `backup` | backs the store up into a directory (the first run a full backup, the next ones increments) |

```rhai,graph=modern,script
g.define_query(#{name: "purge", params: #{label: "string"},
    body: "fn purge(label) { g.v().has_label(label).drop() }"});
g.define_job(#{name: "nightly_purge", schedule: "0 3 * * *", action: "query",
    query: "purge", args: #{label: "software"}, timeout_ms: 30000});
g.jobs().map(|j| [j.name, j.schedule, j.action])
```

`g.define_job(#{name, schedule, action, query?, args?, path?, enabled?, timeout_ms?,
memory_limit?, run_on?, description?})` defines or replaces a job, `g.jobs()` lists them,
`g.drop_job(name)` removes one. The arguments are checked against the query's parameters when
the job is defined.

**Rights.** A job runs with the rights of whoever defined it. When it is defined, the definer's
authorizer is asked for everything the job will do: `Execute` on the saved query and every data
write the query may make, `Create` on the backup directory; the allowed requests are stored with
the job as its **effects**, and every run may do exactly them (and read). If the query is
changed later to write more, its runs are refused until the job is defined again. Revoking the
definer's rights later does not stop a job until it is redefined (the engine has no identities;
an embedding server that has them re-checks on every run). A browser playground cannot define a
backup job: its pages have no files.

A job of a newer version (an action this version does not know) is kept as it was written and
never run; `g.jobs()` lists it with `served: false` and the reason.

**Running.** The jobs of a store's catalog run on the store's scheduler beside its system jobs:
the dev server and the playground pick up a defined, changed or dropped job at their next tick.
A `query` job's run is one transaction whose commit metadata carries `job: <name>` (the change
feed and the history name the job), its queries count under the channel `job` of the
[workload statistics](workload_statistics.md), and its limits per run apply. A run that fails
(a write outside its effects, a timeout, an error of the query) changes nothing and is recorded
with its error; the job runs again at its next time. A job on a graph outside a store is kept but
not run (there is no scheduler). In your own program, give the scheduler the store's user jobs
and sync them whenever the catalog's jobs changed:

```rust,ignore
scheduler.sync_user_jobs(store.user_jobs());
```

The dev server answers `GET /api/jobs` with the jobs of the active store: name, kind, schedule,
whether it is on, the next time and the last run.

## In your own program

```rust
use graphersal::jobs::{Job, JobKind, JobLimits, Scheduler};
use graphersal::persist::{MemDir, Store, StoreOptions};

let dir = MemDir::named("memory:jobs");
let store = Store::create(&dir, StoreOptions::new()).unwrap();
let mut scheduler = Scheduler::new()
    .with_time_zone("Europe/Berlin")
    .unwrap()
    .with_stats(store.workload_stats());
for job in store.system_jobs() {
    scheduler.add(job).unwrap();
}
scheduler
    .add(
        Job::new("hello", JobKind::User, "0 * * * *".parse().unwrap(), |run| {
            let _started = run.started_at; // your work; traversals here get the limits below
            Ok(())
        })
        .with_limits(JobLimits::new().with_timeout(std::time::Duration::from_secs(30))),
    )
    .unwrap();
let now = 1_800_000_000_000_000; // microseconds since the Unix epoch
scheduler.tick(now); // plans the first runs; call it again every few seconds
assert_eq!(scheduler.jobs().len(), 3);
```
