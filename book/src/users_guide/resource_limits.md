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
| | `traversal.max_materialized_bytes` | 1 GiB (256 MiB on wasm32) | estimated size of one value or buffer a step materializes |
| | `traversal.max_string_bytes` | 16 MiB | the longest string a step builds (`replace()`, `concat()`, `format()`, ...) |
| | `evaluationTimeout` | none | wall-clock budget, see [Query Limits](query_limits.md) |
| | `repeat.max_loops` | 10 000 | iterations of a `repeat()` without `times()`, also of an unbounded [GQL quantifier](../gql/semantics.md#quantified-edges) (`{m,}`, `*`, `+`, Cypher `[*]`) on an edge or a path pattern |
| | `bulk.max_expansion` | 10 000 000 | values one bulk expansion may materialize |
| | `memory.limit` | none | memory in use (live heap, or the estimate) during a traversal, an import into a live graph, an applied change set and at every outermost commit; `"auto"` = cgroup limit minus `memory.headroom` |

A `0` for any of the four script counters means *unlimited*; the call depth and the value depth
are never unlimited because they protect the native stack of the thread (`0` reads as the
default for them, and `ScriptLimits::unlimited()` keeps both defaults).

A graph with a journal (a [Store](../persistence/store.md), or a journal attached by hand) has one
more, fixed limit that is not an option: [the size of one commit](#the-size-of-one-commit-journal),
1 GiB of uncompressed journal record per unit.

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
the query, or bound the run with the [memory budget](#memory-budget).

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
expands the bulk, and that is where memory is spent. Two limits bound it, both checked *before*
anything is allocated:

- **`traversal.max_materialized_bytes`** (default 1 GiB natively, 256 MiB on `wasm32`): the
  estimated size of one expansion, `size_of::<Traverser>()` plus the value's own heap estimate per
  copy (the estimator of the `Mem` column of `.profile_with(ProfileType::Memory)` without
  `alloc-tracking`). The same budget bounds a value a `repeat()` loop builds up again on every
  iteration, checked once per traverser per iteration together with the depth: the value, its
  sack, and the values the execution's path records keep (`repeat(__.path())` doubles its value
  on every iteration and keeps each earlier one in the path records). Such a loop stops at most
  one doubling past the budget. The size is an estimate, not a measurement.
- **`bulk.max_expansion`** (default 10 000 000 values): the number of values one expansion may
  produce, a second line behind the byte budget (`BulkExpansionLimit`).

```rhai,graph=modern
g.with("traversal.max_materialized_bytes", 2147483648).v().repeat(__.both()).times(8).fold()
```

Prefer a step that works on the multiplicity directly (`count()`, `groupCount()`, `dedup()`,
`limit(n)`) over raising either limit.

## Memory budget

The limits above bound *one* value or buffer. **`memory.limit`** bounds the whole: the memory in
use while a traversal runs. It exists so that a query which would exhaust the memory of the
process fails with `ResourceLimitExceeded` (and is rolled back, like every failing traversal)
instead of the operating system killing the process (a Kubernetes or cgroup OOM kill).

There is **no budget by default**. A host opts in, either with an explicit number of bytes or with
`"auto"`:

- **bytes**: `g.with("memory.limit", 4294967296)`, or better a host setting (below).
- **`"auto"`**: the cgroup memory limit of the process minus a headroom. Graphersal reads
  `/sys/fs/cgroup/memory.max` (cgroup v2) or, failing that,
  `/sys/fs/cgroup/memory/memory.limit_in_bytes` (cgroup v1), once per process. `max`, the v1
  "unlimited" value or no file at all means no limit, so `"auto"` is no budget outside a
  memory-limited container, and always on `wasm32`. **`memory.headroom`** (bytes) is what stays
  free below the cgroup limit for the stack, the code, allocator overhead and whatever the engine
  does not count; the default is a tenth of the limit, at least 64 MiB and at most half of it (a
  512 MiB container gets a budget of 448 MiB).

What is compared with the budget:

- **with a tracking allocator** (the `alloc-tracking` feature and
  `graphersal::alloc_tracking::TrackingAllocator` installed as the binary's global allocator, as
  the `graphersal` command line, the playground and the Python binding do; in Python it counts
  the extension's Rust heap only, not Python objects): the process's exact live heap bytes, plus
  what a check is about to allocate;
- **otherwise** the engine's estimate: the graph's footprint (`g.memory_usage()`), taken once when
  the traversal starts, plus the traversers alive between two steps, the elements the traversal
  added (at the graph's average element size) and what a check is about to allocate. Property
  values written to existing elements are not counted. The footprint is a walk of the graph (time
  proportional to its size), cached by the graph's commit position: a traversal on a graph that
  has not changed since the last walk reuses the figure, and only the first traversal after a
  committed change walks again (a rolled-back traversal changes nothing, so the figure stays;
  inside an explicit unit with uncommitted changes every traversal walks). For large graphs that
  change often, prefer the tracking allocator.

The budget is cooperative (an allocator cannot fail softly): it is checked at every step boundary
and at the places that check `traversal.max_materialized_bytes`, before a bulk expansion
allocates, before a bucket push of copies and on every `repeat()` iteration. A single step can
overshoot it by what it builds before the next check. Writes are checked the same way: a
traversal that grows the graph past the budget fails and leaves nothing behind. When the graph
*alone* is over the budget, every traversal fails at its first step; a trusted host frees memory
with a raised budget for one query, for example
`g.with("memory.limit", 8589934592).v().has_label("tmp").drop()`.

Units that are not traversals are checked too, against the budget of the graph's
`ExecutionPolicy` default (and, for a script's `g.import_graphml(..)`/`g.import_graphson(..)`,
the session's preset options; a query's own `g.with(..)` does not reach them):

- a **GraphML or GraphSON import into a live graph** (`import_graphml*`, `import_graphson*`,
  `GraphMLImport::import_reader`, `GraphSONImport::import_reader`) and
  **`apply_change_set`** every 4096 elements (imported vertices and edges, applied mutations) and
  at their end;
- **every outermost unit at its commit**: a `transaction(|g| ..)` closure, an applied change
  set, an import, a traversal (whose steps already checked their own budget).

Over the budget the whole unit is rolled back and fails with `GraphError::ResourceLimitExceeded`
(wrapped in `IOError::Graph` for an import), whose message names the operation: `Resource limit
'memory.limit' exceeded by the GraphML import (limit N, reached M)`; the `help()` gives the same
host advice as a traversal's. The measure is the one above: the live heap with a tracking
allocator, else the footprint when the unit started plus the elements it added. Only a unit that
**grew** that measure is stopped, so a unit that only removes or rewrites data commits even on a
graph that is already over its budget. Without a budget nothing is paid: no walk, no check.
Loading a graph nobody sees yet (`GraphSource::from_graphml`, `from_graphson`,
`GraphSONImport::load`, a snapshot or store load) is not checked; the next traversal is.

The budget is a host setting, like the other ceilings: an `ExecutionPolicy` default (locked or
not) on the graph, or a session preset of `graph_scope_with_options`/`engine_with_options`:

```rust,ignore
let policy = ExecutionPolicy::permissive()
    .with_default("memory.limit", "auto")
    .with_locked("memory.limit");
let graph = TraversalGraph::new().with_execution_policy(policy);
```

A query's own `g.with("memory.limit", ..)` is an administrative option (`Update` on its
`Option`), refused outright when the host locked the key.

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
  `traversal.max_value_depth` (`0` is the default, 128). `--memory-limit <bytes|auto>` (and
  `--memory-headroom <bytes>`) sets the [memory budget](#memory-budget) of the session, measured
  on the live heap (the CLI installs the tracking allocator); `0` or no flag is no budget. In the REPL, `/set max-operations <n>`
  (and the other script limits, `max-value-depth` included) and `/set safe-limits`. The
  presets reach every source a script creates. The session runs on a thread with a 64 MiB stack.
- **Web playground**: no script limits, no default timeout and no memory budget, like the CLI
  (it runs in your own browser); its Stop button restarts the engine to end a runaway query.
- **Python**: SAFE defaults; `graph.set_limits(max_operations=..., max_value_depth=...,
  unlimited=True, ...)`. The memory budget is a graph setting:
  `Graph(memory_limit=..., memory_headroom=...)` (also on `Graph.tinkerpop_modern`,
  `Graph.from_graphml` and `Graph.from_graphson`), measured on the live Rust heap of the extension (the binding installs
  the tracking allocator). Python objects are not counted, so with `memory_limit="auto"` the
  `memory_headroom` must also cover the Python side of the process. Scripts run on a worker
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
`persist::Journal`) writes every committed unit as ONE record, and the body of a record is at most
**1 GiB before compression** (format spec 9.3). The body holds every change of the unit with its
before and after values, so dropping an element writes its full properties too: dropping 300 000
vertices with long texts and creating them again in the same unit writes both. The limit is
fixed (it is not an option, and the record is not split over several records).

A larger commit is refused by the journal's commit hook: the whole unit is rolled back, nothing
is written, and the journal keeps working. The error is

```text
The commit was rejected by the commit hook 'journal': Commit 2 is too large for the journal:
its uncompressed record body is 1203145990 bytes, more than the limit of 1073741824 bytes per commit
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
[memory budget](#memory-budget) bounds it instead.
