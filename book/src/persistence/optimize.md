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
| compress | a top-level string property of at least 256 KiB without a rule, not indexed, read fewer than 1,000 times in two days, whose best `lz4` candidate saves at least 64 KiB and 20 % | frees memory, measured by [`estimate_compression`](../users_guide/compressed_properties.md) with the rule's own code; every read decompresses |
| deduplicate | a top-level property of any value type without a rule (indexed or not, read often or not; the 200 with the most values are measured, `max_dedup_estimates`) whose values repeat: at most one in 8 distinct (`dedup_distinct_ratio`), saving at least 1 MiB (`min_dedup_saving_bytes`) and more than `lz4` would; the evidence names the values, the distinct ones and the saving in MiB | frees memory (each value keeps a 4-byte code instead of its 32-byte slot, each distinct value is stored once), measured like the above; writes look the value up: overwriting the key costs about 5-9 % more, adding a key that is new to an element of the label or a jpath write into a deduplicated value about 30 % more (measured at query level); reads stay plain and are mostly faster |
| stop compressing | a compressed key read at least 1,000 times in two days | reads stop decompressing; memory grows back (measured) |

Every recommendation has an **impact**, what applying it is worth, and a **confidence**, how much
data its figures rest on: the two are separate, so a saving measured exactly on all the data
(confidence `high`) can still be worth little (impact `low`).

| Kind | Impact `high` | Impact `medium` | Impact `low` |
|---|---|---|---|
| compress, deduplicate, drop an index | frees 10 % of the graph's memory or more | from 2 % | below 2 % |
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

## Applying several at once

The advice also **groups** its recommendations by kind (and for compression by codec), each
group with its ids and its total: `dedup: 74 keys, total -400.1 MiB`. A group (or any pick of
ids) is applied with ONE call: the advice is computed again, then every catalog change the
picked recommendations name runs in **one unit**, so **one commit** (one WAL record, one entry in
the change feed), and all the new compression rules re-encode the data in **one pass** over the
elements of their labels instead of one pass per rule (see
[Several rules at once](../users_guide/compressed_properties.md#several-rules-at-once)). It is
all or nothing: an id no longer advised (the data or the workload changed since) or a refused
change (its error names the rule or index) applies nothing. A new index or rule whose suggested
name is taken by another definition is refused, never replaced. Nothing is picked for you:
grouping only makes picking easier, a human still decides.

Each applied recommendation is recorded as if applied alone: the memory before it counts the
promises of the others of the same call as kept, so its outcome under *Applied* is its own share
while the promises hold (a deviation of the whole call from its promises shows on every outcome
of the call alike).

## After applying compression rules: write a snapshot

**Write a snapshot after adding or dropping compression rules on a store** (a bulk drop offers it
in its message)**:** `graphersal store checkpoint <DIR>` on a closed store, **Write a snapshot now**
in the playground, `Store::checkpoint` in Rust. It is an optimization, not a requirement: the data
is safe either way, and the rules work from the first commit.

Rules cost almost nothing in the WAL (74 rules: about 2 KiB), so the
[automatic checkpoint](store_checkpoints.md), which goes by WAL size, does not run because of them.
Until a snapshot holds them, every open replays the change: it loads the rows as they are in the
snapshot, then re-encodes the labels of the rules, in one walk for all the rules in a row, even
over many commits. A snapshot that holds the rules codes each row as it enters the graph, so the
plain rows never fill memory. Measured on a real graph (2.8 million elements, 74 `dedup` rules,
`--release`):

| The store | Open | Peak memory of the process | Graph after the open |
|---|---|---|---|
| rules only in the WAL | 8.5-10.5 s | 2.2-2.9 GiB | 1172 MiB |
| rules in a snapshot (the checkpoint took 1 s) | 8.4-11.4 s | 2.0 GiB | 1172 MiB |

The start takes about the same here: the walk is paid for by the coding it spares. The cost of the
walk grows with the elements of the labels the rules name, so the peak is what a snapshot saves on
a graph that barely fits. Graphersal 0.5.2 and older walk once per rule and again per rule at the
end of the open: the same store took 25.1 s, and 90 rules on a graph of about 3 GB in memory took
an open from 14 s to 111 s. There a snapshot is what brings the start back.

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
  *Applied* (the outcomes) and *Dismissed* (with **Restore**). With more than one
  recommendation each has a checkbox, and **Select all** names the groups (`dedup: 74 keys,
  total -400.1 MiB`); nothing is ticked when the panel opens (a human picks). **Apply selected**
  shows what the selection frees and the titles, asks for confirmation and applies them in one
  commit, all or nothing. After a change of compression rules on a store, a note with **Write a
  snapshot now** stays until a snapshot holds that commit (see
  [above](#after-applying-compression-rules-write-a-snapshot)).
- **Workload**: the workload statistics as tables (queries, properties, locks, jobs) for a window
  (the last hour, 2 days or 90 days) and a channel, with a sparkline of the runs per row; a row
  opens its chart: runs per minute, hour or day with the failed part, and the mean and the
  longest time.
- **Memory**: the memory report: the totals, then the properties and the labels, the most bytes
  first, with what is compressed, indexed or a vector; at most 500 rows per list, then a line
  saying how many more properties or labels are not shown and their bytes.

## Other front ends

| Front end | How |
|---|---|
| dev server | `POST /api/advise` with the thresholds (`{}`: the defaults); `POST /api/advise/apply` `{id, spec}` applies one by its id, `{ids: [..], spec}` several in one commit (`{ok, applied: [{id, kind, title, memory_bytes}], memory_bytes}`); `POST /api/advise/dismiss` `{id, days?}` and `POST /api/advise/restore` `{id}` |
| MCP | tool `advise` {everything} (reading, also in read-only mode); `apply_recommendations` {ids, everything} (writing, not offered in read-only mode) applies the ids the user picked in one commit; one recommendation can also be applied with `create_index`, `drop_index`, `define_compression` or `drop_compression` when the user asks |
| Python | `graph.advise(everything=False)`, `graph.apply_recommendations(ids, everything=False)` (both take `policy=`) |
| Rust | `Graph::advise(&AdviceRequest)`, `Graph::apply_recommendations(&ids, &AdviceRequest, authorizer)`, module `graphersal::advisor` (`AdviceGroup`) |

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
// Apply a pick (here one group) in one commit; `None`: no authorizer to ask.
let group = advice.groups.iter().find(|g| g.kind == "create_index").unwrap();
let applied = graph.apply_recommendations(&group.ids, &AdviceRequest::everything(), None);
assert_eq!(applied.unwrap().len(), group.ids.len());
```

The JSON form (`Advice::to_json`, the answer of every front end) has `workload_level`,
`observed_since`, `commit_seq`, `property_bytes`, `graph_bytes` (the memory impact is measured
against), `estimates` and `recommendations`, each with
`id` (stable: the kind and the target), `kind`, `element`, `label`, `key`, `title`, `evidence`,
`cost`, `memory_bytes` (freed; negative when it takes memory), `measured`, `impact` and
`confidence` (each `low`, `medium` or `high`), `reversible`, `command` and `action` (the catalog change as fields); then
`groups` (`key`, `kind`, `codec`, `title`, `ids`, `memory_bytes`), `dismissed` (`id`, `title`,
`until`: `null` for good) and `applied` (`id`, `kind`, `title`, `at`,
`promised_bytes`, `measured_bytes`, `index_lookups`), and `memory_bytes`, the figure the outcomes
are measured on.
