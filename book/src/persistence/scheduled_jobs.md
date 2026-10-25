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

The CLI (a script or the REPL on a store) and the Python binding do not tick: a store opened
there runs no jobs (its workload statistics are still written when it closes). Open the store
with the dev server, or tick a `Scheduler` in your own program, to run them.

**Daylight saving time.** A schedule is read in the local wall clock. A time that does not exist
on the day the clocks go forward (02:30 in most of Europe) runs right after the gap (03:00); a
time that happens twice when they go back runs once, at its first occurrence.

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

System jobs cannot be removed; their schedule and switch can be changed (the playground's and
the dev server's job list, `POST /api/jobs/schedule` and `/api/jobs/enable`, or
`WorkloadStats::set_job_override` in your own program). The change is a setting of the
instance, not of the graph: it is kept in the store's workload sidecar (at the level
`persisted`, the default), so it survives a restart but is not carried by a backup or a fork.
Every run (start, duration, error) is recorded in the workload statistics.

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

**In the playground**, **Catalog ▸ Scheduled jobs** lists every job (the system jobs first) with
its schedule, the next and the last run (with its error), the runs of the last two days and a
switch; a job of yours has **Run now**, **Edit** and **Drop**, a system job **Run now** and
**Schedule** (empty: its built-in schedule again). **New job** opens the form: the name, what it
does (a saved query with its arguments as JSON, a checkpoint, a backup directory), the schedule
(5-field cron, with presets), on or off, a timeout and a description; a refused job shows the
engine's message next to the field.

**History.** Every run (start, duration, error) is counted in the
[workload statistics](workload_statistics.md) as series `job` per minute, hour and day: the job
list shows the runs of the last two days, **Optimize ▸ Workload ▸ Jobs** draws them over time,
`GET /api/jobs/history?name=..&tier=..` answers the buckets.

**Front ends.** The dev server and the playground share one job API (`graphersal-session`):

| Dev server | What it does |
|---|---|
| `GET /api/jobs` | every job: the system jobs, then the jobs of users: schedule, on or off, next time, last run, the run counts of the last hour, two days and 90 days, whether it runs (and why not), a user job's action, arguments and effects |
| `POST /api/jobs/define` | defines or replaces a job from the keys of `g.define_job`; a refused job answers the wrong fields |
| `POST /api/jobs/drop` | `{name}`: removes a job of a user |
| `POST /api/jobs/enable` | `{name, enabled}`: switches a job on or off (a user job's definition keeps its effects) |
| `POST /api/jobs/schedule` | `{name, schedule}`: a system job's schedule (`null`: the built-in one); a user job is defined again instead |
| `POST /api/jobs/run` | `{name}`: runs a job now, whatever its schedule |
| `GET /api/jobs/history?name=..&tier=minutes\|hours\|days` | the job's runs per minute, hour or day: count, failures, total and longest time |

The dev server's user owns the machine's files, so a backup job defined there may name any
directory; the browser playground has no files and refuses backup jobs.

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
