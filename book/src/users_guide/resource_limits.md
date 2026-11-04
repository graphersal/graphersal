# Resource Limits

A script is untrusted input as soon as a front end faces a user. `loop {}`, a string that doubles
forever, a huge array or unbounded recursion would hang or exhaust the process. Graphersal bounds
all of them with two kinds of limit, both reported as one error,
`TraverserError::ResourceLimitExceeded { limit, max, observed }`:

| Kind | Limit | Default | Bounds |
|---|---|---|---|
| Script (`ScriptLimits`) | `script.max_operations` | 10 000 000 | Rhai operations of one script (`loop {}`) |
| | `script.max_string_size` | 16 MiB | the longest string a script builds |
| | `script.max_array_size` | 100 000 | the largest array a script builds |
| | `script.max_map_size` | 100 000 | the largest object map a script builds |
| | `script.max_call_levels` | 32 | nested script function calls (recursion) |
| | `script.max_value_depth` | 128 | nesting of arrays/maps in a value the script hands to the engine |
| Traversal (`g.with()`) | `traversal.max_traversers` | 10 000 000 | traversers one step may leave alive |
| | `traversal.max_value_depth` | 128 | nesting of arrays/maps/paths in a value the traversal builds or writes |
| | `traversal.max_materialized_bytes` | 1 GiB (256 MiB on wasm32) | the memory one execution holds at once (live bytes: its buffers and values; freed memory is given back) |
| | `traversal.max_string_bytes` | 16 MiB | the longest string a step builds (`replace()`, `concat()`, `format()`, ...) |
| | `evaluationTimeout` | none | wall-clock budget, see [Query Limits](query_limits.md) |
| | `repeat.max_loops` | 10 000 | iterations of a `repeat()` without `times()`, also of an unbounded [GQL quantifier](../gql/semantics.md#quantified-edges) (`{m,}`, `*`, `+`, Cypher `[*]`) on an edge or a path pattern |
| | `bulk.max_expansion` | 10 000 000 | values one bulk expansion may materialize |
| Process (`exec::set_memory_limit`) | `memory.limit` | on: cgroup limit minus headroom, else 80 % of RAM (3.5 GiB on wasm32) | memory in use by the whole process (live heap, or the estimate) during a traversal, an import into a live graph, an applied change set, at every outermost commit, and before every allocation proportional to data (loads, store opens, recovery, checkpoints, backup verifies; see [what the ceiling covers](#what-the-ceiling-covers)); NOT a `g.with()` option, see [the process memory ceiling](#the-process-memory-ceiling) |

A `0` for any of the four script counters means *unlimited*; the call depth and the value depth
are never unlimited because they protect the native stack of the thread (`0` reads as the
default for them, and `ScriptLimits::unlimited()` keeps both defaults).

A graph with a journal (a [Store](../persistence/store.md), or a journal attached by hand) writes
a large unit in parts and has no practical commit size limit
([the size of one commit](#the-size-of-one-commit-journal)); the process memory ceiling bounds
it.

## Script limits

`graphersal::script::engine()` and the `eval_*` functions use the SAFE defaults above. To change
them, build the engine yourself (it also takes the script's [authorizer](permissions.md)):

```rust
use std::sync::Arc;
use graphersal::auth::{AccessPolicy, AllowAll};
use graphersal::script::{engine_with_limits, ScriptLimits};

let limits = ScriptLimits::default().with_max_operations(50_000_000);
let engine = engine_with_limits(&limits, Arc::new(AccessPolicy::read_only()));
// or, for a trusted local front end only:
let engine = engine_with_limits(&ScriptLimits::unlimited(), Arc::new(AllowAll));
```

`eval_value_with_limits(graph, script, params, &limits, authorizer)` does the same for a
one-shot evaluation. Raising a traversal limit with the query's own `g.with(..)` is an `Update` on
`Resource::Option` ([Permissions](permissions.md#administrative-gwith-options)).
A host that drives its own engine turns a Rhai limit error into the structured error with
`resource_limit_from_rhai(&err, &limits)`; the `eval_*` functions do this themselves and return
the formatted diagnostic.

The defaults were sized so every example of this book, every doctest and the TinkerPop scenarios
run unchanged. The array and map caps are deliberately modest: Rhai re-measures an array on each
method call that touches it, so a loop pushing items is quadratic and a large cap is also a CPU
budget (measured: a loop filling a one-million-item cap ran about half an hour in a debug build).
Assigning to a map by index (`m[k] = v`) is not size-checked by Rhai at all; only the operation
limit bounds it.

## Traversal limits

`traversal.max_traversers` is an ordinary `g.with()` option, checked once per step boundary (a
counter comparison, no per-item work). A single step can overshoot the cap by one expansion, since
the check runs when the step has produced its output. A cross product (a `v()` in the middle of a
traversal, as in `g.v().v()` or the GQL `MATCH (a), (b)`) is checked *before* it is built: its
size is known up front, so a product over the cap fails at once instead of allocating it first.

A count that comes from the query (a bulk, a row count, the size of a product) never becomes an
unchecked up-front reservation. Where the engine reserves an exact size it asks the allocator
fallibly: when the system cannot provide that block (the limit raised to "unlimited" and a
product of trillions of rows), the run fails with `ResourceLimitExceeded` of `memory.allocation`
and is rolled back instead of the process aborting. `memory.allocation` is not an option: narrow
the query, or bound the run with the [process memory ceiling](#the-process-memory-ceiling).

```rhai,graph=modern
g.with("traversal.max_traversers", 50000000).v().out().out().count()
```

A host sets a default and locks it with an `ExecutionPolicy`, exactly like `evaluationTimeout`:

```rust,ignore
let policy = ExecutionPolicy::permissive()
    .with_default("traversal.max_traversers", 1_000_000)
    .with_locked("traversal.max_traversers");
```

Every traversal limit is an administrative option: a script's own `g.with(..)` of it asks the
host's authorizer for `Update` on `Option(key)`, while a host's preset (`graph_scope_with_options`)
or policy default asks nothing. The presets of `graph_scope_with_options` reach every traversal source of the
session: `g`, `_g` and `__` (also a traversal *started* from `__`, such as
`__.inject(1).repeat(..)`). Build the engine with `engine_with_options(&limits, authorizer,
&options)` and the same options, so the sources a script creates itself
(`GraphSource::empty()`, `GraphSource::tinkerpop_modern()`, `schema.to_graph()`, ...) get them
too; otherwise a script could escape a preset `evaluationTimeout` by building its own source.

A `gql()` query is bounded by the same options. An unbounded quantified edge (`-[:knows]->{1,}`,
`*`, `+`, Cypher `[*]`) or quantified path pattern (`((x)-[:knows]->(y)-[:created]->(z))+`) has
no limit of its own: trail matching ends it on a finite graph, and `repeat.max_loops` (counting
iterations of the whole body) and `traversal.max_traversers` turn a walk that grows too far into
an error, never into a shortened result. The help of that error names the quantified edge or
path pattern, suggests an upper bound and shows `g.with(..).gql(..)` with a raised limit
([GQL Semantics](../gql/semantics.md#quantified-edges)):

```rhai,graph=modern
g.with("repeat.max_loops", 1).gql("MATCH (a {name: 'marko'})-[:knows|created]->*(b) RETURN b.name AS b")   // error: repeat() exceeded the loop limit of 1 iterations
g.with("repeat.max_loops", 5).gql("MATCH (a {name: 'marko'})-[:knows|created]->*(b) RETURN count(*) AS n").to_list()   // #{"n": 6}
g.with("repeat.max_loops", 1).gql("MATCH (a {name: 'marko'}) ((x:person)-[:knows]->(m)-[:created]->(y))+ (b) RETURN b.name AS b")   // error: repeat() exceeded the loop limit of 1 iterations
```

The same holds for a quantified edge or path pattern inside a subquery (`EXISTS`, `COUNT { }`, `COLLECT { }`, a
pattern comprehension, a `CALL` body), whose help names it. The list a pattern comprehension (`[(p)-->(f) | f.name]`) collects expands
the bulk of its matches under `bulk.max_expansion` and is checked against
`traversal.max_materialized_bytes` and the memory budget before it is built, like
`collect_list()` ([GQL Errors](../gql/errors.md#map-projections-and-comprehensions)). Besides
the options, `gql()` bounds the size of the query itself with fixed limits (no option changes
them), each a `54001` error before the run ([GQL Errors](../gql/errors.md#gqlstatus-codes)):

| Limit | Bounds |
|---|---|
| 128 levels | nesting of parentheses, brackets and subqueries; an expression at most 512 high |
| 64 edge patterns | per `MATCH` clause, over all its comma-separated paths (also per clause inside a subquery); an edge in the body of a quantified path pattern counts once, however often the body repeats |
| 64 subqueries (`MAX_SUBQUERIES`) | `EXISTS`, pattern predicates, pattern comprehensions, `COUNT { }` and `COLLECT { }` written in one query, nested ones included (not `CALL` bodies) |
| 16 body levels (`MAX_BODY_DEPTH`) | `CALL` bodies and whole-query subquery bodies (`EXISTS` / `COUNT` / `COLLECT` with clauses other than `MATCH`) nested in each other; it keeps the deepest query within a 2 MiB thread stack (the wasm32 worker's 1 MiB) |
| 1024 subquery bodies (`MAX_LOWERED_SUBQUERIES`) | bodies the query runs once its `LET` / `WITH` variables are substituted: a variable holding a subquery counts once for every place that reads it |

## Memory: materialized bytes and expansions

Equal traversers are merged into one record with a multiplicity (bulk), so a query like
`g.v().repeat(__.both()).times(17)` stays cheap while it only counts. A step that needs every
value as its own item (`fold()`, `aggregate()`, `store()`, `group()` value lists, a terminal list)
expands the bulk, and that is where memory is spent. Two limits bound it:

- **`traversal.max_materialized_bytes`** (default 1 GiB natively, 256 MiB on `wasm32`): the
  memory ONE execution holds AT ONCE (changed in 0.5.0; before, it bounded the size of one value
  or buffer). It counts **live bytes**: memory a step frees is given back, so only what is held at
  the same time counts. A query that builds a list, drops it and builds the next
  (`fold().unfold().fold().unfold()`) holds one list at a time; a query that keeps two lists (two
  labelled folds, a `project()` of two folds) holds both. What is measured:
  - **tracked** (a tracking allocator is installed, as in the `graphersal` command line, the
    playground and the Python binding): the live heap of the query's thread minus what it held
    when the query started. What the query adds to the GRAPH (a write) does not count here: in a
    query that writes it is estimated at every step boundary (the elements added, at the graph's
    average element size) and subtracted; the process ceiling below bounds the graph's growth;
  - **estimated** (otherwise): the engine's count of what the query holds: the stream between
    two steps (by capacity, 80 bytes per traverser), the values it sizes anyway (bulk expansions
    and bucket copies, `fold()` lists, GQL lists and function results, `product()`, step-built
    strings), the `repeat()` frontier with its values and sacks, and the values the path records
    keep (`repeat(__.path())` doubles its value on every iteration and keeps each earlier one).
    A value is charged when a step builds it and released when a later step replaces the stream
    (a map, flat-map or barrier step; filters, labels and side effects pass it on) or when the
    child traversal that built it returns; side-effect buckets are held to the end of the query.
    The figures come from what the engine computes anyway, never from a walk over every element,
    so the estimate is coarse: a value built inside a child traversal (`project().by(__.fold())`)
    counts while the child runs, not afterwards, and most element properties read on the way are
    not counted.

  It is checked at every step boundary, before a bulk expansion, a bucket push of copies or a
  sized value is built, and for every traverser of a new `repeat()` frontier WHILE the frontier is
  built, so a loop that multiplies its traversers stops within the iteration that crosses the
  limit. The error's `observed` figure (`reached N`) is what the query held plus what the failing
  check was about to allocate. [Profiles](profiling_and_execute.md) report the PEAK of the
  measure (`TraversalMetrics::memory()`); the peak is sampled at these checks, not exact, so a
  short spike between two checks can be missed.
- **`bulk.max_expansion`** (default 10 000 000 values): the number of values one expansion may
  produce, a second line behind the byte budget (`BulkExpansionLimit`).

```rhai,graph=modern
g.with("traversal.max_materialized_bytes", 2147483648).v().repeat(__.both()).times(8).fold()
```

Prefer a step that works on the multiplicity directly (`count()`, `groupCount()`, `dedup()`,
`limit(n)`) over raising either limit.

## The process memory ceiling

There are exactly **two memory limits**, never mixed:

- the **process memory ceiling** `memory.limit`: what the whole PROCESS may hold. One setting of
  the process, set by the host, on by default;
- the **per-query limit** `traversal.max_materialized_bytes` (above): the memory one query holds
  at once (live bytes, the query's own working memory without what it adds to the graph), an
  ordinary `g.with()` option. Profiles report its peak against this limit, never against the
  process ceiling.

The ceiling exists so that a query (or an import, or a write) which would exhaust the memory of
the process fails with `ResourceLimitExceeded` and is rolled back, like every failing traversal,
instead of the operating system killing the process (a Kubernetes or cgroup OOM kill).

**It is on by default.** Unless the host sets it, the ceiling is resolved once, in this order,
and the result carries its source (`graphersal::exec::memory_limit()`, `MemoryLimitSource`):

| Source | Ceiling |
|---|---|
| `configured` | the bytes the host set |
| `none` | the host turned it off (`MemoryLimit::Unlimited`, CLI `--memory-limit none`) |
| `cgroup` | the cgroup memory limit of the process minus the headroom: Graphersal reads `/sys/fs/cgroup/memory.max` (cgroup v2) or, failing that, `/sys/fs/cgroup/memory/memory.limit_in_bytes` (cgroup v1), once per process; `max` or the v1 "unlimited" value is no cgroup limit. The **headroom** (`memory.headroom`) stays free for the stack, the code, allocator overhead and what the engine does not count: by default a tenth of the limit, at least 64 MiB and at most half of it (a 512 MiB container gets 448 MiB) |
| `physical` | 80 % of the machine's physical memory (Linux `/proc/meminfo`, macOS `hw.memsize`, Windows `GlobalMemoryStatusEx`), read once per process |
| `wasm` | 3.5 GiB on `wasm32` (a 32-bit linear memory ends at 4 GiB; a browser may grant less) |
| `unknown` | a platform where none of the above can be read: no ceiling |

A host sets it once, for the whole process (every graph, every session of a server):

```rust
use graphersal::exec::{self, MemoryLimit, MemoryLimitSource};

exec::set_memory_limit(MemoryLimit::Bytes(4 << 30)).unwrap(); // 4 GiB
let limit = exec::memory_limit().unwrap();
assert_eq!(limit.bytes(), Some(4 << 30));
assert_eq!(limit.source(), MemoryLimitSource::Configured);

exec::set_memory_limit("auto".parse().unwrap()).unwrap();    // back to the default
exec::set_memory_headroom(Some(256 << 20)).unwrap();          // for a cgroup limit
```

`MemoryLimit` reads and prints the text forms the front ends accept: a positive number of bytes,
`auto` or `none`. It is **not an execution option**: a query's `g.with("memory.limit", ..)` (or
`memory.headroom`), an `ExecutionPolicy` default or a session preset of these keys fails with
`InvalidOption`, whose `help()` names the process setting (changed in 0.5.0; before, the budget
was an opt-in option per execution). A scheduled job's `memory_limit` does not touch the
ceiling: it is the per-query limit `traversal.max_materialized_bytes` of every traversal the
job's run executes, with the same measure (live bytes held at once; the smaller of the two
values; see
[Scheduled jobs](../persistence/scheduled_jobs.md#limits-per-run)).

What is compared with the ceiling (the error's `help()` says which one applied):

- **tracked**: with a tracking allocator (the `alloc-tracking` feature and
  `graphersal::alloc_tracking::TrackingAllocator` installed as the binary's global allocator, as
  the `graphersal` command line, the playground and the Python binding do; in Python it counts
  the extension's Rust heap only, not Python objects), the process's exact live heap bytes, plus
  what a check is about to allocate;
- **estimated**: otherwise (a library host with the system allocator), the engine's estimate: the
  graph's footprint (`g.memory_usage()`) when the traversal starts, plus what the traversal holds
  by the same count as the per-query limit (its buffers, the values it sizes, the `repeat()`
  frontier, the path records), the elements the traversal added (at the graph's average element
  size) and what a check is about to allocate. Property values written to existing elements are
  not counted, and the count of what a query holds is coarse, so the real process can hold
  more. Because the ceiling is on by default, the start of a run never walks the graph while the
  graph is far from the ceiling: it takes the footprint of the last walk, adjusted by the
  elements added or removed since. The graph is walked (time proportional to its size, cached by
  its commit position) only on the first run, once its element count has doubled or halved since
  the last walk, and on every run after a committed change while the figure is at least half of
  the ceiling. All of these happen only between units: **inside a script, a saved-query call or
  a transaction that has already changed data, the graph is never walked**; the figure is the
  last walk extrapolated by the elements added or removed in it, so its traversals do not walk
  the graph one after the other. This is an edge case of the estimate: after large changes in
  that unit (many rewritten property values, elements much larger or smaller than the graph's
  average) the figure can be off until the unit ends. With a tracking allocator no traversal
  walks the graph and the figures are exact. `g.memory_usage()` walks the graph too and keeps
  its figure for the next runs: call it after rewriting many property values to refresh the
  estimate. For large graphs that change often near the ceiling, prefer the tracking allocator.

The ceiling is cooperative (an allocator cannot fail softly): it is checked at every step
boundary and at the places that check `traversal.max_materialized_bytes`, before a bulk expansion
allocates, before a bucket push of copies and on every `repeat()` iteration. A single step can
overshoot it by what it builds before the next check. Writes are checked the same way: a
traversal that grows the graph past the ceiling fails and leaves nothing behind. When the graph
*alone* is over the ceiling, every traversal fails at its first step; a trusted host raises the
ceiling for a moment, drops what it does not need (`g.v().has_label("tmp").drop()`) and lowers it
again.

Units that are not traversals are checked against the same ceiling:

- a **GraphML or GraphSON import into a live graph** (`import_graphml*`, `import_graphson*`,
  `GraphMLImport::import_reader`, `GraphSONImport::import_reader`) and
  **`apply_change_set`** every 4096 elements (imported vertices and edges, applied mutations) and
  at their end;
- **every outermost unit at its commit**: a `transaction(|g| ..)` closure, an applied change
  set, an import, a traversal (whose steps already checked their own).

Over the ceiling the whole unit is rolled back and fails with `GraphError::ResourceLimitExceeded`
(wrapped in `IOError::Graph` for an import), whose message names the operation: `Resource limit
'memory.limit' exceeded by the GraphML import (limit N, reached M)`; the `help()` gives the same
host advice as a traversal's. The measure is the one above: the live heap with a tracking
allocator, else the footprint when the unit started plus the elements it added. Only a unit that
**grew** that measure is stopped, so a unit that only removes or rewrites data commits even on a
graph that is already over the ceiling. With the ceiling turned off (`none`) nothing is paid: no
footprint, no check.

### What the ceiling covers

Every allocation whose size is proportional to data checks the ceiling, BEFORE it allocates when
the size is known up front, otherwise every 4096 elements and at the end. Nothing is half-written:
a refused open changes nothing on disk, a refused load drops the half-built graph, a refused
checkpoint leaves the store as it was.

| Operation (the error's `operation`) | Check |
|---|---|
| traversals | step boundaries, bulk expansions, materializing steps (above) |
| `product()` | its estimated size against `traversal.max_materialized_bytes` and the ceiling before the pairs are built; the `evaluationTimeout` is checked while they are |
| imports, `apply_change_set`, commits | every 4096 elements and at the end, rolled back (above) |
| `snapshot load` (`read_snapshot*`, Python `Graph.load`) | the snapshot header's estimate of the graph (plus the live heap when tracked) BEFORE anything is read: `PersistError::MemoryBudget`; while the graph is built every 4096 elements, each segment and its decoded chunks before they are read |
| `store open` (`Store::open`, CLI `--graph <store>`, dev server, Python `Store.open`) | the same, plus the replay of the WAL after the snapshot: a graph that does not fit refuses the open, naming the minimum to raise to. Never maintenance mode, never read-only instead |
| `recovery` (`Recovery`, past states, read-only views) | every WAL record before it is read, the replay every 4096 mutations |
| `commit encoding` | the encoded body of a commit while the journal builds it (each time it doubles, from 1 MiB on): a unit that does not fit is rolled back |
| `reading a commit in parts` | the assembled body of a commit written in parts before each growth (recovery, checkpoints, backups, verify, salvage) |
| `store read` | a whole store file or range read into memory (an `io::Error` of kind `OutOfMemory` inside `PersistError::Io`) |
| `checkpoint`, `snapshot write` | the encoder's id index (24 bytes per element) and one chunk before they are allocated; a merged checkpoint's chunks and its delta graph |
| `maintenance copy` | the copy of a salvaged graph (twice the graph) |
| `backup verify` | every metadata entry of a ZIP backup read back whole |
| `GraphSON load`, `GraphML load` (`GraphSource::from_graphson*`, `from_graphml*`, `GraphSONImport::load`) | every 4096 elements and at the end; without a tracking allocator the size is estimated by the bytes of text read so far |

Two errors tell when a persisted graph was refused: `PersistError::MemoryBudget` (the estimate
did not fit: refused before reading; its `setting` is `memory.limit`, or
`ReadOptions::with_memory_limit` when that stricter per-read bound was lower) and
`PersistError::Graph(GraphError::ResourceLimitExceeded { .. })` (the estimate passed, but the
data grew past the ceiling while it was read). Both name the operation and the setting to raise;
`ResourceLimitExceeded` carries `needed` (the bytes a check before an allocation asked for) and
`observed` (what was in use plus that: the smallest ceiling that would have passed). The
manifest's estimate is the graph's footprint when it was written, so a graph just at the ceiling
can be refused (or pass and then stop while loading).

A host that allocates data-proportional memory itself (a model it loads, a buffer it fills) asks
first:

```rust
use graphersal::exec;

exec::check_memory_for("model load", 64 << 20).unwrap(); // 64 MiB more: fits?
let room = exec::memory_room().unwrap();                  // ceiling, source, live heap, room left
println!("{:?} bytes left", room.available());
```

`check_memory_for` compares the live heap plus the bytes with a tracking allocator, else only
the bytes (the rest of the process is not seen).

**Embedding models of a store** (book page [Embedding Models in a
Store](../persistence/store_models.md)) load under the ceiling too: before a load the blob's
length plus the loaded size its definition records (`load_bytes`) are checked against what is in
use (the live heap with a tracking allocator, else the models already loaded), so nothing is read
when it does not fit (operation `"model load"`). A model the schema NEEDS is loaded when the
store opens: one that does not fit refuses the open with `StoreFailure::ModelDoesNotFit`, whose
message names the smallest ceiling that fits. Other models are loaded at the first query that
embeds with them; a refusal there fails that query. Loaded models count in the estimated memory
of every run (without a tracking allocator) and in `g.memory_usage()` as `model_bytes`.

### Seeing how full the process is

Every front end shows the process against its ceiling with the same figures (computed once, in
the session layer):

- **Playground**: the process memory bar in the Profile panel's Statistics tab and in
  Optimize -> Memory ([The Web Playground](web_playground.md)): used of the ceiling, its
  source, the percentage, and the segments structure, properties, compressed, indexes, models,
  live_graph, store, engine and other (the heap no segment explains). The graph's segments use the
  figures of `g.memory_usage()`: structure is `structure_bytes`, indexes is `index_bytes` (the id
  and label indexes) plus `property_index_bytes`. The others are the process's components of the
  [memory report](memory_and_profiling.md#where-the-process-memory-goes), read without its walk:
  models are the `embedding_model` components (every model loaded in the process, once),
  live_graph is `store_graph` (a store's live graph beside a view of a past state), store is
  `store` + `store_files`, engine is the graph's `workload_bytes` plus every other component
  (`script_engine`, `session`, `mcp`, `ai_chat`, a host's own); each is left out at 0.
- **REPL**: `/memory` prints the bar as text before the memory report:

  ```text
  Memory   1.6 GiB of 3.4 GiB (cgroup)                                        47 %
           [################################....................................]
           structure 150.0 MiB  properties 420.0 MiB  compressed 95.0 MiB
           indexes 90.0 MiB  models 506.0 MiB  other 340.0 MiB
           compression saves 285.0 MiB (plain 380.0 MiB -> 95.0 MiB, -75 %)
  ```

- **Dev server**: `GET /api/statistics` (and the MCP `statistics` tool) carries
  `process.memory.usedBytes`, `usedTracked`, `limitBytes`, `limitSource`, `usedPercent`,
  `segments` (each `{name, bytes}`, the component segments with `parts: [{name, detail?, bytes}]`)
  and `compression`; the start banner's memory row ends with
  `process 47 % of 3.4 GiB (cgroup)`.

Without a tracking allocator (a host that did not install it) the used figure is the sum of the
known segments, marked "process total not tracked"; with the ceiling turned off
(`memory.limit none`) there is no percentage. This is the process ceiling only: a query's own
peak against the per-query limit `traversal.max_materialized_bytes` is the PROFILE's
`query_memory` ("Peak memory" in the playground's Profile panel).

## Strings built by steps

`script.max_string_size` bounds the strings a *script* builds; **`traversal.max_string_bytes`**
(default 16 MiB, the SAFE `script.max_string_size`) bounds the strings the *engine* builds:
`replace()`, `concat()`, `conjoin()`, `format()`, `as_string()`, `to_upper()`/`to_lower()`,
`substring()`, `reverse()`, the trims and the elements of `split()`. The steps that can multiply
the length of their input (`replace()` at every match, `concat()`/`conjoin()` of many parts)
compute the length first and fail before the string is allocated; the others check their result.

```rhai,graph=modern
g.with("traversal.max_string_bytes", 67108864).v().values("name").replace("a", "aa")
```

## Time inside a step

`evaluationTimeout` is checked between steps, every 1024 traversers a step consumes or produces,
and every 1024 values a step materializes: the copies of a bulk expansion (`fold()`, a terminal
list, `aggregate()`, `group()` value lists), the members `group()` and `tree()` collect, the sort
keys `order()` computes (and once after the sort) and every `repeat()` iteration. The timeout is
reported at the step that was running (`Step #2 'fold()'`), not at the next step boundary.

## Value depth

Almost everything that reads a value walks it recursively: the conversion to a script value,
hashing in `dedup()`, schema inference, rendering. A value nested thousands of levels deep would
overflow the native stack and abort the whole process, so Graphersal bounds the depth where a
value *enters* the engine; no deeper value can exist afterwards, and every reader stays bounded.
A scalar has depth 0 and every array, object, map or path around it adds one level: `[1]` is 1,
`[[1]]` and `{"a": [1]}` are 2.

- **`script.max_value_depth`** (`ScriptLimits::with_max_value_depth`, default 128, the recursion
  limit `serde_json` applies to JSON input): a value a script hands to the engine (a step argument
  such as `inject(x)`, `property("k", x)`, `has("k", x)`, a token argument), a host parameter
  (`eval_*_with_params`), and the result of a whole-script unit (`eval_value_atomic`,
  `eval_value_dry_run`, `materialize_traversals`). The conversion stops at the limit; nothing
  recurses deeper.
- **`traversal.max_value_depth`** (a `g.with()` option, default 128, administrative, lockable with
  an `ExecutionPolicy` like every option): what the engine builds itself. A `repeat()` whose body
  wraps its value again on every iteration (`repeat(__.fold())`, `repeat(__.path())`,
  `repeat(__.group())`, a sack folded into itself) is checked once per traverser per iteration;
  a property write is checked for the depth it leaves behind, so `property(jpath("a.b.c"), v)`
  counts two levels for the path plus the depth of `v`.
- **jpath**: a path has at most 256 segments (a parse error beyond), so even a direct
  `GraphStorage::set_vertex_property_path` call cannot build an unbounded value.
- **Imports**: GraphML values are scalars (containers cannot be imported), and schema and
  change-set JSON is parsed by `serde_json`, whose own recursion limit (128) applies. A
  [GraphSON](graphson.md) import skips a property value nested deeper than 128 levels (reported)
  and bounds the JSON nesting of a line, so no file can exhaust the stack.
- **GraphSON input size**: one line (line form) or the whole document (wrapped form) is read only
  up to `GraphSONImport::max_text_bytes` (512 MiB natively, 64 MiB on wasm32), checked before the
  text is held in memory (`IOError::InputTooLarge`, whose `help()` names
  `with_max_text_bytes(n)`).

```rhai,graph=modern
g.with("traversal.max_value_depth", 256).inject(1).repeat(__.fold()).times(200).count()
```

Raise the limits only moderately: a few hundred levels are fine on every thread stack, thousands
are not. The display of a script value the script never handed to the engine (a deeply nested
Rhai array returned as the result) is cut with `[…]` after 128 levels; the data itself is not
changed.

## Front ends

- **`graphersal` (CLI)**: a trusted local tool, so no script limits by default. `--safe-limits`
  applies the SAFE defaults; `--max-operations`, `--max-string-size`, `--max-array-size`,
  `--max-map-size` and `--max-traversers` set one limit (`0` is unlimited; `--max-string-size`
  also sets `traversal.max_string_bytes`, which follows the script's string limit and is
  unlimited by default). `--max-value-depth <n>` sets `script.max_value_depth` and
  `traversal.max_value_depth` (`0` is the default, 128). `--max-materialized-bytes <n>` sets
  the default of the per-query memory limit `traversal.max_materialized_bytes` (`0` is
  unlimited; without it the library default, 1 GiB, applies); a query's own `g.with(..)`
  overrides it. `--memory-limit <bytes|auto|none>` (and
  `--memory-headroom <bytes>`) sets the [process memory ceiling](#the-process-memory-ceiling),
  measured on the live heap (the CLI installs the tracking allocator); no flag keeps the default
  (`auto`), `none` or `0` turns it off. With `--server` it covers every session of the server.
  In the REPL, `/set max-operations <n>`
  (and the other script limits, `max-value-depth` included) and `/set safe-limits`. The
  presets reach every source a script creates. The session runs on a thread with a 64 MiB stack.
- **Web playground**: no script limits and no default timeout, like the CLI (it runs in your own
  browser); the process memory ceiling is the `wasm32` default, 3.5 GiB of tracked heap. Its Stop
  button restarts the engine to end a runaway query.
- **Python**: SAFE defaults; `graph.set_limits(max_operations=..., max_value_depth=...,
  max_materialized_bytes=..., unlimited=True, ...)`; `max_materialized_bytes` is the default of
  the per-query memory limit `traversal.max_materialized_bytes` for every traversal of that
  graph's scripts (a query's own `g.with(..)` overrides it). The process memory ceiling is not
  a keyword of a graph: it is set with
  `graphersal.set_memory_limit(bytes | "auto" | "none", headroom=...)` and read with
  `graphersal.memory_limit()`, one value for the whole process; `Graph.load` also refuses a
  snapshot whose size estimate exceeds it. It is measured on the live Rust heap of the
  extension (the binding installs the tracking allocator); Python objects are not counted, so
  leave room for the Python side of the process. Scripts run on a worker
  thread with a 64 MiB stack (one per calling Python thread).

The large stack is a mitigation for the scripting engine itself: Rhai copies a nested map value
recursively (`x = #{a: x}` in a loop), and a few thousand levels overflowed the 8 MiB stack of a
debug build before any Graphersal code ran, which no depth limit of Graphersal can catch. A host
that embeds the engine on a thread of its own should give that thread a large stack too
(`std::thread::Builder::stack_size`; not available on `wasm32`).

## wasm32

The script limits are counters Rhai checks itself, so they work on `wasm32-unknown-unknown`.
`evaluationTimeout` works there too: the engine reads the clock through `web_time::Instant`
(`performance.now()` in the browser). The operation counter stays the portable, deterministic bound;
the playground additionally has a Stop button that restarts the engine.

## Mutating traversals

A limit abort in the middle of a *mutating* traversal rolls back every write the traversal made
before the abort: an aborted write traversal is all-or-nothing (see [Transactions](transactions.md)).
A Rhai limit that stops a *script* (operations, call depth, string or collection size) stops it
in the script code around the traversals; the traversals that already finished stay applied (the
unit is one traversal, not the script), unless the host runs the script as one unit with
`eval_value_atomic` ([Transactions](transactions.md#a-whole-script-as-one-unit-rhai)), where a limit
abort rolls back the whole script.

## The size of one commit (journal)

A journal (the write-ahead log of a [Store](../persistence/store_commits.md), or a
`persist::Journal`) writes every committed unit as one commit. Its body holds every change of the
unit with its before and after values, so dropping an element writes its full properties too:
dropping 300 000 vertices with long texts and creating them again in the same unit writes both.
A body of more than 64 MiB before compression is written in parts of 64 MiB (format spec 9.3.1),
so since 0.5.0 a commit has no practical size limit (up to 2^20 parts, 64 TiB; before 0.5.0 one
commit was at most 1 GiB). What bounds it is memory: the unit's changes and its encoded body
are held while it commits (operation `commit encoding` of the
[process memory ceiling](#the-process-memory-ceiling)), and a recovery holds one commit's body
while it replays it. Plan for several times the commit's size: while a unit commits, the data it
added to the graph, its undo log (every before image) and the whole encoded body are in memory at
once. A single commit of a little over 1 GiB peaked between 4.4 and 5.7 GB of process memory in
our measurements; more, smaller units (below) keep the peak at the size of one of them.

A commit beyond 2^20 parts, or one whose encoding does not fit under the ceiling, is refused by
the journal's commit hook: the whole unit is rolled back, nothing is written, and the journal
keeps working. The error is `CommitTooLarge` (or `ResourceLimitExceeded`) inside

```text
The commit was rejected by the commit hook 'journal': ...
```

and its help says how to split the work. The limit applies per unit, so the fix is more, smaller
units:

- **Web playground and dev server**: a whole script is one unit. Drop the old data in a run of its
  own (`g.v().drop().iterate()`) and create the new data over several runs (a part of the input
  per run).
- **CLI, Python, Rust**: each traversal is already its own unit; only a single huge traversal
  (one `drop()` of everything, one load of millions of elements) can reach the limit. Split it
  into batches. In Rust, `transaction(..)` blocks and whole-script units
  (`script::eval_value_atomic`) commit as one, so keep each below the limit.

A graph without a journal (in memory only) has no such limit; the
[process memory ceiling](#the-process-memory-ceiling) bounds it instead.
