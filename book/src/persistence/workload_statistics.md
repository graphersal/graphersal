# Workload Statistics

A graph instance keeps statistics of what was done with it: which scheduled jobs ran and how
long they took, how long writers held the graph's lock, and, as later releases add them, which
properties queries read and how. They describe the **instance**, not the data: they are not
versioned with the graph, never written to the journal, and never part of a commit, so they
do not move with a rollback and are not replicated.

## Levels

| Level | What is kept |
|---|---|
| `off` | nothing |
| `memory` | in memory, gone with the process |
| `persisted` (default) | in memory, and in the Store's sidecar file `WORKLOAD`, read back when the store opens |

The level is set when a store is opened (`StoreOptions::with_workload_level`); it is not stored.
Switching to `off` keeps what was collected; only clearing deletes it.

## Time buckets

Every series keeps three tiers of buckets, like a round-robin database:

- one bucket per minute for the last 60 minutes,
- one per hour for the last 48 hours,
- one per day for the last 90 days.

Each bucket counts the occurrences, the failed ones, the items they touched, their total and
their longest duration. The minute and hour tiers live in memory; the day tier is what the
sidecar keeps, so after a restart the long history is back and the fine-grained one starts
anew. Old buckets fall out on their own.

## The sidecar

A writable store writes `WORKLOAD` once a minute (the system job `stats_flush`, see
[Scheduled Jobs](scheduled_jobs.md)) and when it closes. After an orderly stop nothing is lost;
after a crash at most the last minute. The file is best effort: a missing or damaged file means
the statistics start anew, never damage, and the store opens as usual. It is never copied into a
backup, a fork or a repaired store: a restored store starts without statistics. Its byte layout
is section 4.4 of the [format specification](format.md).

A backup opened read-only, a read-only or maintenance-mode store keeps statistics in memory only.

## Reading them

```rust
use graphersal::persist::{MemDir, Store, StoreOptions};
use graphersal::workload::{Channel, Sample, SeriesKey, SeriesKind};

let dir = MemDir::named("memory:stats");
let store = Store::create(&dir, StoreOptions::new()).unwrap();
let stats = store.workload_stats();
let now = 1_800_000_000_000_000; // microseconds since the Unix epoch
stats.record(
    SeriesKey::new(SeriesKind::WriteLock("query".into()), Channel::Cli),
    now,
    Sample::new(10, 250),
);
let report = stats.report(now);
assert_eq!(report.series[0].last_hour.count, 1);
store.close().unwrap(); // writes WORKLOAD
```
