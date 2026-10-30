# Bulk and Barriers

Every traverser carries a **bulk**: how many identical traversers it stands for. When two
traversers are equal (the same vertex, reached by different routes), the engine can keep one of
them with bulk 2 instead of processing both. A step that expands the survivor does the work once,
and a `count()` adds up the bulks. This is the same idea as TinkerPop's traverser bulk.

Without it, `repeat(out()).times(8).count()` on the grateful-dead graph would have to walk about
2.5 * 10^15 paths. With it, the loop never holds more than one traverser per distinct vertex:

```rhai
g.V().repeat(__.out()).times(8).count().next()    // grateful graph: 2505037961767380 in milliseconds
```

## Bulk is invisible

A query returns the **same multiset of results with and without merging**. Merging is an
optimization, never a semantic change. Only the *order of duplicates* can change: equal traversers
are emitted together, at the position of their first occurrence.

```rhai,graph=modern
g.V().both().both().values("name").groupCount().next()                   // josh 7, lop 7, marko 7, ...
g.V().both().both().barrier().values("name").groupCount().next()         // the same counts
```

Steps that need every value as its own item (`fold()`, `aggregate()`, `store()`, a terminal
`toList()`, `group()` value lists) expand a merged traverser back into one value per unit of bulk.
Steps that can work on the multiplicity directly (`count()`, `sum()`, `mean()`, `groupCount()`,
`limit()`, ...) never expand it.

## Where traversers are merged

| Merge point | What it is |
|---|---|
| `barrier()` / `barrier(n)` | An explicit merge point you write yourself. |
| the `repeat()` frontier | (off with a sack merge operator or `withBulk(false)`) In breadth-first order, the starting traversers and the output of every iteration are merged before `until()`/`emit()` are checked. Depth-first `repeat()` never merges. |
| `lazy_barrier()` | (off with a sack merge operator or `withBulk(false)`) Inserted by the [`lazy_barrier` optimizer rule](../optimizer/lazy_barrier.md) between adjacent steps such as `out().out()`. |

`.profile()` shows a `Bulk` column next to `Out` when some step's bulk differs from its traverser
count, and names the inserted steps:

```rhai,graph=modern
g.V().both().both().barrier().profile()
```

`dedup()` is not a merge: every survivor gets bulk 1.

## What prevents merging

Two traversers merge only if nothing later in the query can tell them apart.

- **Full paths.** `path()`, `simplePath()` or an `as()` label that is read later
  make every traverser carry its own history, so none are equal. The merge pass is skipped. See
  [Path Requirement Analysis](../optimizer/path_analysis.md).
  `g.V().out().out().path()` does not merge.
- **Containers.** Arrays, maps, records, entries, paths and schemas are never merged (hashing them
  costs per item and they are not where explosions happen). Vertices, edges, properties and scalar
  values merge.
- **Mutating traversals.** If the plan contains `addV()`, `addE()`, `property()`, `drop()`,
  `mergeV()` or another mutation, nothing merges, so every logical traverser runs its mutation.

## barrier() and barrier(n)

```rhai,graph=modern
g.V().out().barrier().out().values("name").toList()
```

`barrier()` merges the whole stream at that point. `barrier(n)` is accepted for TinkerPop
compatibility; `n` must be an integer of at least 1, and it is shown in `.profile()`, but it does
**not** cut the stream into chunks of `n`: the whole stream is merged at once. Because our engine
already materializes each step's stream, the size only affects TinkerPop's unspecified order of
grouped duplicates, so it is not implemented. `barrier(0)` fails with
`barrier(0): the size must be an integer from 1 to 4294967295`.

## Options

| Option | Default | Meaning |
|---|---|---|
| `bulk.merge` | `true` | `false` turns every merge point into a pass-through. The result is the same, only slower on dense graphs. Use it to diagnose or to compare. |
| `bulk.one` | `false` | `true` (`g.withBulk(false)`) makes a merge keep bulk 1 (duplicates collapse and are emitted once), stops sacked traversers from merging, and switches the automatic merge points off. A strict boolean. See [Sack and Operators](sack_and_operators.md#withbulkfalse). |
| `bulk.max_expansion` | `10 000 000` | The most values a step may produce when it expands a merged traverser into single items. Larger expansions fail with `BulkExpansionLimit`. |
| `traversal.max_materialized_bytes` | `1 GiB` (256 MiB on wasm32) | The memory the query holds at once (live bytes), an expansion included: one that would take it over the limit fails with `ResourceLimitExceeded` before anything is allocated. See [Resource Limits](resource_limits.md#memory-materialized-bytes-and-expansions). |

```rhai,graph=modern
g.with("bulk.merge", false).V().both().both().count().next()                 // same 30, no merging
g.with("bulk.max_expansion", 5).V().both().both().barrier().toList()         // fails: 30 values > 5
```

The second query fails with:
`Step 'to_list' would have to materialize 30 values, more than the limit of 5 (bulk.max_expansion)`.
The limit exists because a merged traverser is cheap but its expansion is not: one record with
bulk 10^12 would otherwise try to build 10^12 values and exhaust memory. The error message
suggests `count()`, `groupCount()`, `dedup()` or `limit(n)` before the step, or raising the limit
with `g.with("bulk.max_expansion", n)` when you really need every value and have the memory.

Both options are listed in the [Execution Options Reference](execution_options_reference.md).

## Rust callbacks: `side_effect(SideEffectFn)`

The Rust API's `side_effect(func)` (no DSL form: a Rhai script uses `sideEffect(traversal)`) calls
`func(&value, bulk)` once per traverser, as TinkerPop's `sideEffect(Consumer)` does: after a merge
point a traverser with bulk 3 is one call with `bulk == 3`. A callback that counts adds `bulk`, and
its total is then the same with and without merging. A failure later in the traversal rolls back
the writes of the unit; what the callback itself did outside the graph stays.

## Overflow

Bulks are `u64` and all arithmetic is checked. A bulk or total that does not fit fails with
`BulkOverflow` instead of wrapping. A `count()` above `i64::MAX` fails the same way.

## Differences from TinkerPop

- **Mutations never merge.** In TinkerPop, `addV()` after a barrier creates one vertex per merged
  traverser. Graphersal creates one vertex per logical traverser.
- **Side-effect children run once per logical traverser.** A child traversal that writes a side
  effect (for example `local(__.aggregate("x"))`) runs `bulk` times for a traverser with bulk
  `bulk`, so the side effect has the same content with and without merging. This holds for the
  child of `sideEffect(traversal)` too: `sideEffect(__.aggregate("x"))` after a bulk-3 traverser
  aggregates three times (TinkerPop runs it once per merged traverser object); the incoming
  traverser itself continues unchanged, with its bulk.
- **`barrier(n)` does not chunk** (see above).
- **`withBulk(false)` is the option `bulk.one`**: merging still happens at an explicit
  `barrier()`, the merged traverser keeps bulk 1, and the automatic merge points are off. A sack
  merge operator (`withSack(initial, Operator)`) also switches the automatic points off and makes
  merging part of the result; a loop body then writes `repeat(__.out().barrier())`. See
  [Sack and Operators](sack_and_operators.md#automatic-merging-and-sacks).
- **Merge equality is by identity.** A vertex merges with the same vertex only. Paths are
  compared by handle, not by content, so traversers with equal but separately recorded paths do
  not merge.

## A heuristic and its limit

Merging a stream in which nothing is equal is wasted work. The `repeat()` frontier and
`lazy_barrier()` therefore probe: after the first 4096 traversers, if fewer than one in sixteen
merged, the pass gives up and passes the stream through unchanged (and a `repeat()` stops merging
for its remaining iterations). Before that, `lazy_barrier()` also skips a merge whose duplicates
would save the following step less than one output per two traversers (their fan-out on that step,
estimated on the same first 4096; see [Lazy Barrier Rule](../optimizer/lazy_barrier.md)). The
probes only change speed, never results. Its limit is that it
judges the *start* of a stream. A stream whose first 4096 traversers are all distinct but whose
tail contains many duplicates is not merged, and a loop whose early iterations are sparse but
later ones dense stops merging early. Use an explicit `barrier()` (never adaptive) to force a
full merge at a chosen point.

## Operators, sacks and bulk

`fold(seed, Operator)` and a side effect declared with `withSideEffect(key, init, Operator)` apply their operator
once per unit of bulk, so merging never changes their result. A sack merge operator and `withBulk(false)` are the
two places where merging is visible on purpose: see [Sack and Operators](sack_and_operators.md#reducer-operators)
and [Merge operator](sack_and_operators.md#merge-operator).
