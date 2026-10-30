# Execution Options Reference

This page is the single, canonical list of every `g.with(key, value)` option Graphersal
recognises. Query authors: this is where to find every key that exists. Hosts embedding this
engine (a CLI, a web playground, a multi-tenant backend): this is where to find every key an
[`ExecutionPolicy`](#host-controlled-execution-policy) can lock down.

| Key | Type | Engine default | Host-lockable | What it does |
|---|---|---|---|---|
| `evaluationTimeout` | non-negative integer milliseconds | `0` (no timeout) | yes | Wall-clock budget of the whole execution, nested traversals included. See [Query Limits](query_limits.md). |
| `repeat.order` | `"bfs"` \| `"dfs"` (case-insensitive) | `"bfs"` | yes | The order a `repeat()` loop walks its frontier in. See [Recursive Traversals](recursive_traversals.md). |
| `repeat.max_loops` | integer, 1 to 4 294 967 295 | `10 000` | yes | Iteration limit of a `repeat()` without `times()`, so an unbounded loop on a cyclic graph fails instead of running forever. See [Query Limits](query_limits.md) and [Recursive Traversals](recursive_traversals.md). |
| `optimizer.disabled` | array of rule-name strings | `[]` (every rule on by default runs) | yes | Names of on-by-default optimizer rules to leave out of this execution's plan, or `["all"]` to disable every rule at once. See [Query Optimizer](../optimizer/index.md#toggle-rules-for-one-query). |
| `optimizer.enabled` | array of rule-name strings | `[]` | yes | Names of off-by-default optimizer rules to force into this execution's plan (a no-op today; no shipped rule defaults to off). See [Query Optimizer](../optimizer/index.md#toggle-rules-for-one-query). |
| `path.analysis` | boolean | `true` | yes | `true` records only the path positions a step actually reads; `false` records every position unconditionally. See [Path Requirement Analysis](../optimizer/path_analysis.md). |
| `bulk.one` | boolean | `false` | yes | `true` is `g.withBulk(false)` (TinkerPop `ONE_BULK`): a merge at an explicit `barrier()` keeps bulk 1, so duplicates are emitted once; a traverser with a sack and no merge operator never merges; the automatic merge points (`repeat()` frontier, `lazy_barrier()`) pass through. `withBulk(true)` sets it back to `false`. Any other value fails with `InvalidOption`. See [Sack and Operators](sack_and_operators.md#withbulkfalse). |
| `bulk.merge` | boolean | `true` | yes | `true` lets merge points (`barrier()`, the `repeat()` frontier, `lazy_barrier()`) merge equal traversers into one with a bulk; `false` turns them into pass-throughs. Never changes the result multiset. Merging is also off when the plan mutates. See [Bulk and Barriers](bulk_and_barriers.md). |
| `bulk.max_expansion` | integer, at least 1 | `10 000 000` | yes | The most values a step (`fold()`, `aggregate()`, `store()`, `group()` value lists, a terminal list) may produce when it expands merged traversers into single items; more fails with `BulkExpansionLimit`. A second line behind `traversal.max_materialized_bytes`. See [Bulk and Barriers](bulk_and_barriers.md). |
| `traversal.max_materialized_bytes` | integer, at least 1 | `1 073 741 824` (1 GiB; `268 435 456` on wasm32) | yes | The memory, in bytes, one execution holds at once (since 0.5.0; before, the size of one value or buffer): live bytes, so memory a step frees is given back. Tracked (tracking allocator): the live heap of the query's thread since it started, without what it adds to the graph; estimated: the engine's count of its step buffers, sized values (bulk expansions, `fold()` lists, GQL lists), the `repeat()` frontier and the path records. Checked at every step boundary, before an expansion allocates and per traverser of a new `repeat()` frontier; profiles report its peak (`TraversalMetrics::memory()`). A scheduled job's `memory_limit` caps it. Exceeding it fails with `ResourceLimitExceeded`. Administrative. See [Resource Limits](resource_limits.md#memory-materialized-bytes-and-expansions). |
| `traversal.max_string_bytes` | integer, at least 1 | `16 777 216` (16 MiB) | yes | The longest string, in bytes, a step builds: `replace()`, `concat()`, `conjoin()`, `format()`, `as_string()`, `to_upper()`/`to_lower()`, `substring()`, `reverse()`, the trims and the elements of `split()`. `replace()`, `concat()` and `conjoin()` check before they allocate. Exceeding it fails with `ResourceLimitExceeded`. Administrative. See [Resource Limits](resource_limits.md#strings-built-by-steps). |
| `memory.limit` | (not an option) | | | Since 0.5.0 the process memory ceiling, ONE setting of the whole process (`graphersal::exec::set_memory_limit`, CLI `--memory-limit`, Python `graphersal.set_memory_limit`), on by default. Setting it here, as an `ExecutionPolicy` default or a session preset fails with `InvalidOption`. Limit one query with `traversal.max_materialized_bytes`. See [Resource Limits](resource_limits.md#the-process-memory-ceiling). |
| `memory.headroom` | (not an option) | | | Part of the process memory ceiling (`graphersal::exec::set_memory_headroom`, CLI `--memory-headroom`); refused here like `memory.limit`. |
| `traversal.max_traversers` | integer, at least 1 | `10 000 000` | yes | The most traversers one step may leave alive (the stream between two steps, which is also what a terminal list materializes). Exceeding it fails with `ResourceLimitExceeded`. Pass `9223372036854775807` for no limit. See [Resource Limits](resource_limits.md). |
| `random.seed` | integer (int64) | none (seeded from the operating system's entropy) | yes | The seed of the execution's random number generator, which `coin()`, `sample()` and `order().by(Order.shuffle)` draw from (child traversals included, in plan order). The same seed gives the same draws on every run of the same query with the same Graphersal version, also in the browser. Graphersal's counterpart of TinkerPop's `SeedStrategy`, whose scenarios stay out of scope with `withStrategies()`; the draws are not those of TinkerPop's Java `Random`. A non-integer fails with `InvalidOption`. See [sample](../traversal/steps/sample.md#reproducible-samples). |
| `traversal.max_value_depth` | integer, at least 1 | `128` | yes | The deepest nesting of arrays, maps and paths one value may reach inside the engine: checked once per traverser per `repeat()` iteration (a body that wraps its value again, such as `repeat(__.fold())`) and on every property write (a `jpath(..)` path counts one level per segment below the property). Exceeding it fails with `ResourceLimitExceeded`. Deeply nested values are read recursively, so keep it in the hundreds. See [Resource Limits](resource_limits.md#value-depth). |
| `render.max_rows` | positive integer | none (unlimited) | no | Script DSL only: how many top-level results a *displayed* result shows. `graphersal` and the playground set it to `100` for the session. Never affects data terminals. See [Displaying Results](displaying_results.md). |
| `render.max_items` | positive integer | none (unlimited) | no | Script DSL only: how many items of each nested collection a displayed result shows. `graphersal` and the playground set it to `100`. See [Displaying Results](displaying_results.md). |
| `render.spelling` | `"snake"` \| `"camel"` (case-insensitive) | `"snake"` | yes | The spelling of step names in diagnostics: `.profile()` step names, error step locations and the `help()` rewrites of the failing query. `"snake"` renders `has_label("person")`, `group_count()`; `"camel"` renders the Gremlin spelling `hasLabel("person")`, `groupCount()`. Never affects results. `graphersal --spelling camel`, the REPL `/set spelling camel` and the playground setting set it for the session. Any other value fails with `InvalidOption`. See [Profiling and execute()](profiling_and_execute.md#step-names). |

Every key above changes a query's *behavior*, its *safety bound*, or its *plan* — never its
result, except `evaluationTimeout`/`repeat.max_loops` themselves failing the query when a bound is
hit. Setting a key again replaces its previous value; an unrecognised key is silently ignored.

An invalid *value* for a recognised key (wrong shape, out of range, an unknown rule name) fails
with `TraverserError::InvalidOption` when the traversal is **executed** — `with()` itself has no
error channel and always returns `Self`.

## Host-Controlled Execution Policy

`evaluationTimeout` and `repeat.max_loops` exist specifically to bound a runaway query's resource
use. But by themselves, both are just ordinary options: a query is free to set
`g.with("evaluationTimeout", 999999999).with("repeat.max_loops", 999999999)` and defeat them. A
host that runs untrusted queries against a shared graph — a public web playground, a multi-tenant
backend — needs a way to fix a ceiling on any option above, **outside the query's reach**.

`ExecutionPolicy` is that ceiling. It is built once, by the trusted host, and attached to a
`TraversalGraph` at construction time — before the graph is ever wrapped for sharing
(`Graph`/`Arc<TraversalGraph>`) and handed to code that might run an untrusted query against it:

```rust
use graphersal::prelude::*;
use graphersal::exec::ExecutionPolicy;

let policy = ExecutionPolicy::permissive()
    // A query can no longer touch evaluationTimeout at all; every execution gets 2000 ms.
    .with_default("evaluationTimeout", 2000)
    .with_locked("evaluationTimeout")
    // Same for repeat.max_loops: no query-supplied value is ever honored.
    .with_default("repeat.max_loops", 500)
    .with_locked("repeat.max_loops");

let graph = TraversalGraph::tinkerpop_modern().with_execution_policy(policy);
let g = Graph::new(graph);
```

There is no setter after this point — not `&mut self`, not one gated behind a write lock. Once a
`TraversalGraph` is constructed, its policy cannot change for the rest of its life; a host that
wants different limits for a different session constructs a different graph instance.

Two independent knobs, settable per key:

- **`with_default(key, value)`** — applied when the query's own `g.with()` doesn't set `key`.
  Takes precedence over the engine's own hardcoded default, but the query can still override it
  unless the key is also locked.
- **`with_locked(key)`** — a query's own `g.with(key, ..)` attempt fails immediately with
  `TraverserError::LockedOption`, regardless of the value it tried to set. The policy's default
  (or, absent one, the engine's hardcoded default) always applies instead.

`ExecutionPolicy::permissive()` — no key locked, no default overridden — is the implicit policy of
any `TraversalGraph` built without `with_execution_policy(..)`: every option takes its engine
default unless the query sets it.

Every key in the table above is checked against the policy the same, generic way, so a future
option automatically participates in locking and defaulting without special-cased code.
