# Workload Statistics

A graph instance keeps statistics of what was done with it: which scheduled jobs ran and how
long they took, how long writers held the graph's lock, which queries ran, and which
properties they used and how. They describe the **instance**, not the data: they are not
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
their longest duration. The sidecar keeps all three tiers, so a restart loses none of them;
buckets that aged past their window while the process was stopped fall out at the next
sample.

## Channels

Every sample is counted under the **channel** of the work that produced it, so the report says
who runs what (which queries the AI agents run, which ones the page):

| Channel | Work |
|---|---|
| `playground` | queries run from the browser playground (in the page, or over the dev server) |
| `mcp` | the MCP endpoint's tools and the AI Chat's agent |
| `cli` | the command line: one-shot scripts and the REPL |
| `python` | the Python binding |
| `job` | the actions of [scheduled jobs](scheduled_jobs.md) |
| `host` | anything else (an application embedding the library) |

Only a user's queries count. The work a front end makes up itself (drawing the Graph panel,
the inspector, schema inference, the panels' lists) is **technical** and not counted. A host
declares its channel around the work it does for a request with
`graphersal::workload::scope(channel, technical, || ..)`, or once for a thread that serves one
front end with `set_thread_default(channel, technical)`; a front end that marks its own work
technical runs a user's query in `scope(current_channel(), false, ..)`. Scopes nest, and a
job's run is always `job`.

## The graph's lock

The graph has one writer lock: a writer excludes readers and other writers. To see whether it
is a problem, three series measure it under the channel of the work that took the lock:

| Series | What it measures |
|---|---|
| `write.wait` | a writer asking for the lock until it holds it (contention) |
| `write.hold` | a writer holding the lock (what readers and other writers wait for) |
| `read.wait` | a reader waiting because a writer holds the lock (an uncontended read is not counted) |

Each bucket has the count, the total and the longest time, so the mean and the worst wait are
at hand. A Store attaches its statistics to its graph; a graph of your own counts after
`Graph::attach_workload_stats(stats)`. Work a front end marks as technical (its own drawing and
panel queries) is not counted.

## Queries

Every run of a traversal on a graph with statistics (GQL included: it runs the same pipeline)
counts in the series of its **fingerprint**: the optimized plan, as `.profile()` names its
steps, with every literal replaced by `?`. `g.v().has("name", "ann").values("age")` and the same
query for `"bob"` are one series, `v().has(?, ?).values(?)`; a list of literals is one `?`. Each
run adds its result count (with bulk), its duration and whether it failed. A saved query call
counts once more as `saved:<name>` (the call itself; the traversal it returns counts under its
plan when it runs). At most 500 query series are kept: a new one beyond that replaces the one
run least in the last 90 days. Like `pg_stat_statements` in PostgreSQL, this answers "which
queries run, how often, how long", by channel (which ones the AI agents run).

## Properties

The same run also counts, per property, how the plan used it: series `(element, label, key,
access)`, each counted once per run with the number of traversers that reached the step using
it. The access is one of

| Access | Steps |
|---|---|
| `filter_scan` | `has(key, ..)`, `has(key)`, `has_not(key)`, `has(key, __.to_lower()...)`, a `has` folded into `v()`/`e()` |
| `filter_index` | the same filters when a [property index](../users_guide/property_indexes.md) serves them |
| `read` | `values`, `properties`, `value_map`, `element_map`, a `by(key)` of `project`/`select`/`path`/`tree` |
| `order_group` | a `by(key)` of `order`, `group`, `group_count`, `dedup` |
| `similarity` | `similarity`, `nearest`, `embed` |
| `write` / `remove` | `property`, `add_v`/`add_e` properties / `remove_property` |

The element kind and label are what the plan states before the step (`v(labels)`,
`e(labels)`, `has_label`, `out_e(labels)`, `add_v(label)`); a step that reads every property
(`value_map()`, `element_map()`) names the key `*`, and a use inside a child traversal
(`where(__.out().has("lang"))`) has no element, no label and no traverser count. A jpath key
counts under its top-level property. These counts answer "which properties are filtered
without an index, which are only written, which are read at all": the input of the planned
storage advisor.

## Memory

The statistics stay bounded (at most 500 query series, at most 60 + 48 + 90 buckets per series,
only buckets in which something was counted). Their memory is shown as `workload_bytes` in
`g.memory_usage()`, in the dev server's and the playground's statistics (Profile ▸ Statistics,
**Workload statistics**), never added to the graph's `total_bytes`.

## The sidecar

A writable store writes `WORKLOAD` once a minute (the system job `stats_flush`, see
[Scheduled Jobs](scheduled_jobs.md)) and when it closes. After an orderly stop nothing is lost;
after a crash at most the last minute. The file is best effort: a missing or damaged file means
the statistics start anew, never damage, and the store opens as usual. It is never copied into a
backup, a fork or a repaired store: a restored store starts without statistics. Its byte layout
is section 4.4 of the [format specification](format.md).

A backup opened read-only, a read-only or maintenance-mode store keeps statistics in memory only.

## Reading them

| Front end | How |
|---|---|
| REPL (`--graph <store>`) | `/workload`: one line per series, last 90 days |
| dev server | `GET /api/workload` (the JSON below) |
| MCP | tool `workload_statistics` {max_rows} (at most 50 rows per list by default) |
| Python | `store.workload_report()` (a dict) |
| Rust | `store.workload_stats().report(now)`, `WorkloadReport::to_property()` / `to_json()` / `Display` |

The JSON form (`WorkloadReport::to_json`) has the keys `level`, `queries` (`query`),
`properties` (`element`, `label`, `key`, `access`), `locks` (`lock`) and `jobs` (`name`,
`last_run`); every row has its `channel` and the windows `last_hour`, `last_two_days` and
`last_ninety_days`, each with `count`, `failed`, `items`, `total_micros` and `max_micros`. Rows
are sorted by total time over 90 days. A graph outside a store answers level `off` with empty
lists.

**History.** Asked with a tier (`?history=minutes|hours|days` on the dev server, the MCP tool's
`history`, Python's `workload_report(history="hours")`, Rust's
`report_with_history(now, HistoryTier::Hours)`), the report adds `history_tier` and gives every
row its `history`: the buckets of that tier within its window (the last 60 minutes, 48 hours or
90 days), oldest first, only those in which something was counted, each `{start, count, failed,
items, total_micros, max_micros}` with `start` in microseconds since the Unix epoch. That is the
data of a chart: how often a query ran and how long it took, minute by minute or day by day.


```rust
use graphersal::persist::{MemDir, Store, StoreOptions};
use graphersal::workload::{Channel, Sample, SeriesKey, SeriesKind};

let dir = MemDir::named("memory:stats");
let store = Store::create(&dir, StoreOptions::new()).unwrap();
let stats = store.workload_stats();
let now = 1_800_000_000_000_000; // microseconds since the Unix epoch
stats.record(
    SeriesKey::new(SeriesKind::Lock("write.hold".into()), Channel::Cli),
    now,
    Sample::new(10, 250),
);
let report = stats.report(now);
assert_eq!(report.series[0].last_hour.count, 1);
store.close().unwrap(); // writes WORKLOAD
```
