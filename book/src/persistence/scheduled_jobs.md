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
duration, error) is recorded in the workload statistics. Jobs defined by users (a saved query or
a backup on a schedule) are planned as catalog definitions.

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
