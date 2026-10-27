# Optimize

The storage advisor turns what the engine already measures into concrete advice: the
[memory report](../users_guide/memory_and_profiling.md) (what the graph holds), the
[workload statistics](workload_statistics.md) (how it is used) and the catalog. Every
recommendation names the ordinary catalog command that applies it, a savings figure measured on
the data where one can be measured, its cost, its impact and how sure the advisor is. Nothing is applied
automatically, and every action of this version can be undone (drop the index or the rule
again).

## The recommendations

| Kind | When | Effect |
|---|---|---|
| create an index | a `(label, key)` filtered by a full scan at least 10 times in two days, on a label of at least 1,000 elements, with no index serving it and values an index holds | the scans become index lookups; the index takes memory (estimated) |
| drop an index | no query used the index in the time observed, after at least 7 days of statistics | frees the index's memory (measured) |
| compress or deduplicate | a top-level string property of at least 256 KiB without a rule, not indexed, read fewer than 1,000 times in two days, whose best codec (`lz4` or `dedup`) saves at least 64 KiB and 20 % | frees memory, measured by [`estimate_compression`](../users_guide/compressed_properties.md) with the rule's own code |
| stop compressing | a compressed key read at least 1,000 times in two days | reads stop decompressing; memory grows back (measured) |

Every recommendation has an **impact**, what applying it is worth, and a **confidence**, how much
data its figures rest on: the two are separate, so a saving measured exactly on all the data
(confidence `high`) can still be worth little (impact `low`).

| Kind | Impact `high` | Impact `medium` | Impact `low` |
|---|---|---|---|
| compress or deduplicate, drop an index | frees 10 % of the graph's memory or more | from 2 % | below 2 % |
| create an index | its scans read 100 million elements or more in two days | from 10 million | below |
| stop compressing | read 100,000 times or more in two days | from 10,000 | below |

The graph's memory is its estimated total footprint (the memory report's totals). A saving of
6 MiB in a graph of 700 MiB (under 1 %) is advised with impact `low`.

The list is ordered by impact, the highest first; within one impact new indexes come first (by
the elements their scans read), then compression by bytes saved, then unused indexes and hot
compressed keys. The thresholds can be lowered: *Every
candidate* in the playground, `everything: true` in the API, or each threshold on its own. Without
workload statistics (a graph outside a store) only the data is looked at, so there is no index
advice.

## Dismissing, and what an applied one did

A recommendation can be **snoozed** for 7 days or **dismissed** for good; it is then listed under
*Dismissed* (with **Restore**) until its time. Applying one records the memory before and what it
promised: for 30 days the advice lists it under *Applied* with the memory freed (or taken) since,
measured on the graph's property and property index memory (other changes of the data in the
meantime count too), and for a new index the indexed lookups of its key. Dismissals and these
records are settings of the instance, not data: they are kept in the store's
[workload statistics](workload_statistics.md) sidecar and need a store.

## In the playground

The top bar's **Optimize** opens one panel with three tabs:

- **Recommendations**: each with its impact, its evidence, cost, memory effect (with its share
  of the graph's memory), confidence and the exact command; **Apply** runs it (one commit, recorded in the change feed like any catalog change),
  **Copy** copies the command, **Snooze 7 days** and **Dismiss** hide it; below the list,
  *Applied* (the outcomes) and *Dismissed* (with **Restore**).
- **Workload**: the workload statistics as tables (queries, properties, locks, jobs) for a window
  (the last hour, 2 days or 90 days) and a channel, with a sparkline of the runs per row; a row
  opens its chart: runs per minute, hour or day with the failed part, and the mean and the
  longest time.
- **Memory**: the memory report: the totals, then the properties and the labels, the most bytes
  first, with what is compressed, indexed or a vector.

## Other front ends

| Front end | How |
|---|---|
| dev server | `POST /api/advise` with the thresholds (`{}`: the defaults); `POST /api/advise/apply` `{id, spec}` applies one by its id; `POST /api/advise/dismiss` `{id, days?}` and `POST /api/advise/restore` `{id}` |
| MCP | tool `advise` {everything} (reading, also in read-only mode); the agent applies a recommendation with `create_index`, `drop_index`, `define_compression` or `drop_compression` when the user asks |
| Rust | `Graph::advise(&AdviceRequest)`, module `graphersal::advisor` |

```rust
use std::sync::Arc;
use graphersal::advisor::AdviceRequest;
use graphersal::prelude::*;
use graphersal::workload::{WorkloadLevel, WorkloadStats};

let graph = GraphSource::tinkerpop_modern();
graph.attach_workload_stats(Arc::new(WorkloadStats::new(WorkloadLevel::Memory)));
for _ in 0..3 {
    let found = graph.read().traversal().v(None).has_label("person").has("name", "josh").to_list();
    assert_eq!(found.unwrap().len(), 1);
}
// The modern graph is tiny: every candidate.
let advice = graph.advise(&AdviceRequest::everything()).unwrap();
let index = advice
    .recommendations
    .iter()
    .find(|r| r.id == "create_index:vertex:person:name")
    .unwrap();
assert_eq!(index.action.command(), r#"g.create_index(#{label: "person", key: "name"})"#);
```

The JSON form (`Advice::to_json`, the answer of every front end) has `workload_level`,
`observed_since`, `commit_seq`, `property_bytes`, `graph_bytes` (the memory impact is measured
against), `estimates` and `recommendations`, each with
`id` (stable: the kind and the target), `kind`, `element`, `label`, `key`, `title`, `evidence`,
`cost`, `memory_bytes` (freed; negative when it takes memory), `measured`, `impact` and
`confidence` (each `low`, `medium` or `high`), `reversible`, `command` and `action` (the catalog change as fields); then
`dismissed` (`id`, `title`, `until`: `null` for good) and `applied` (`id`, `kind`, `title`, `at`,
`promised_bytes`, `measured_bytes`, `index_lookups`), and `memory_bytes`, the figure the outcomes
are measured on.
