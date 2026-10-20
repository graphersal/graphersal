# Traversal pipeline (`traversal/`)

Agent guidance. Not packaged. Root rules 1, 3-6, 13, 20, 21 apply to every step.

## Steps

Traversals are built through `GraphTraversalSource` (`traversal/source.rs`) as a `Vec<GraphStep>`; `GraphStep` (`traversal/step/mod.rs`) is an enum of boxed trait objects, one variant per category. Each category has a `<category>_steps.rs` plus a directory with one `step_<name>.rs` per step; a new step goes there.

| Category | Examples |
|---|---|
| `Init` | `v()`, `e()` |
| `Append` | direct extension steps |
| `Map` (1->1) | `values`, `id`, `label` |
| `FlatMap` (1->N) | `out`, `in`, `both`, `out_e`, `union`, `json_path` |
| `Filter` | `has`, `has_label`, `has_id`, `where_step`, `filter(traversal)` |
| `Barrier` | `count`, `sum`, `max`, `min` |
| `FlatBarrier` | `dedup`, `limit`, `repeat` |
| `SideEffect` | `profile` |
| `SideEffectMut` | `add_v`, `add_e`, `property`, `drop`, `remove_property(keys...)` (`property(k, null)` stores null), `add_label`/`drop_label` (vertices), `set_label` (edges) |
| `Modulator` | `by`, `as_` |
| `As` / `Count` | step label / fused count |

- **Loops:** `repeat()` (`flatbarrier_steps/step_repeat.rs`) runs its body once per iteration on the whole frontier (BFS), or per traverser with an explicit stack under `g.with("repeat.order", "dfs")`. `times()`/`until()`/`emit()` attach to the preceding `repeat()` via `as_repeat_mut()` (postfix) or push a placeholder a following `repeat()` fills (prefix). Counters live in `TraverserContext::loop_stack` (`LoopFrame`), read by `loops()`/`loops(name)`. Without `times()` a loop fails after `repeat.max_loops`.
- **Child traversals:** numbering is defined once by `GraphStep::child_traversals_mut` (own arguments first, then modulator children in attachment order; also drives `optimize_nested`), stored in `AnonymousTraversal::child_index` by `number_children`. A new child-holding step overrides `child_traversals_mut` (not `optimize_nested`), `analyze_nested_paths()`, and gets an id case in `execute_tests.rs`.

## Optimizer (`traversal/optimizer/`)

`TraversalOptimizer::default_pipeline()` rewrites in place, nested children first. Order:
1. `WhereUnnest`
2. `AddPropertyFold`: `add_v`/`add_e` + the run of directly following constant `property(k, v)` (modulators `as_()`/`from()`/`to()` do not break it; last repeated key wins) become ONE `add_vertex_with`/`add_edge_with` call, so required properties are satisfied and a failing property leaves no element; stops at a traversal-valued, `Cardinality`, jpath-keyed or other step (`AddElementStep::fold_property`, `constant_property`; profile `add_v("L", properties: [...])`).
3. `FilterReorder` (cheap `has_id`/`has_label` first) 4. `HasIdPushdown`
5. `SourceFilterPushdown`: folds `has_id`/`has_label` into `StepV`/`StepE` (never the zero-argument `has_label()`; `not(__.has_label())` selects unlabeled).
6. `PropertyIndex`: `v()`/`e()` with folded labels (each indexed on the key) reads an index for its folded `has(key, value)` and `has(key, P)` (eq, within, ranges) in the following run of pure filters; the `has` steps stay and re-check; skipped when no index is declared.
7. `BarrierPushdown` (`out_e().has_label(x)` -> `StepOutE(labels)`) 8. `GroupCountPushdown`
9. `CountPushdown`: `count()` directly after the leading `v()`/`e()` (`v().count().is(6)` fuses too) from storage or an index (`count_only`).
10. `LazyBarrier`: a merge point between adjacent adjacency steps after vertex-yielding steps; adaptive probe (4096 items, gives up below 1/16 merged).

New rules go in `optimizer/rules/` and are registered in `TraversalOptimizer::registry()` (stable `name()`, constructor, default flag: `default_on: true` unless genuinely experimental). `.profile()` ends with "Optimizer rules applied: ..." (rules that changed the plan); `g.with("optimizer.disabled"|"optimizer.enabled", [names])` toggles per execution (bad names: `TraverserError::InvalidOption` at execution). `KNOWN_RULES` and defaults: `book/src/optimizer/index.md`. `Init` and `FlatMap` steps expose filter parameters (labels, ids) so rules can fold into them.

**Path analysis** (`optimizer/path_analysis.rs`) runs on the final plan: each level backwards, every step gets a `PathMode` (`None`/`Labels{set}`/`Full`); nested traversals report through their holder. Paths live in a per-execution arena (`value/path_arena.rs`), `Traverser.path` is a 4-byte `PathRef`. An `as()` retracts its labels upstream (readers see only the latest occurrence), except "history labels" a step reads in every occurrence (`history_labels()`, only the internal GQL steps: trail checks and group variables of quantified edges; the method and its collection exist only with feature `gql`, so a build without it pays nothing). `g.with("path.analysis", false)` forces `Full`.

## Bulk, sack, Pop

- **Bulk** (`traversal/bulk.rs`: `merge_traversers`, `total_bulk`, `expand`): `Traverser.bulk: u64`; equal traversers merge at `barrier()`/`barrier(n)`, the `repeat()` BFS frontier, `lazy_barrier()`. Invisible: same multiset (only duplicate order may change). Skipped for `Full` paths, containers, DFS repeat, mutating plans. `barrier(n)` validates but does not chunk. Options `bulk.merge` (`false` = differential oracle), `bulk.max_expansion` (10 000 000; `BulkExpansionLimit`), `bulk.one` (= `withBulk(false)`). Docs `bulk_and_barriers.md`.
- **Sack** (`value/sack.rs`): `Traverser.sack: SackRef` (4 bytes in former padding, `NONE` = null) into the per-execution `SackArena`; scalars interned, containers fresh handles; `size_of::<Traverser>() == 80` (`traverser_stays_80_bytes`). `Operator` drives `sack(op)`, `withSack(init, op)`'s merge, `fold(seed, op)`, reducer side effects (`SideEffectValue::Reduced`, `finish_batch`); `Barrier.normSack`. Without a merge operator only equal handles merge; with one (or `bulk.one`) the sack leaves the merge key, combined in encounter order, `context.auto_merge` false (only explicit `barrier()` merges). Merge pass monomorphised `merge_inner::<S, OPERATOR, ONE>`. **Child-input rule:** a child receiving a real traverser keeps the parent's sack (`split`/`with_parts`); an object-valued `by()` projection (`path().by`, `select().by`, `tree().by`, `where(P).by`, `math` `by`) gets `Traverser::start(context, v)` with the initial sack. Docs `sack_and_operators.md`.
- **Pop:** `select(Pop, labels...)` via `PathArena::labelled`: `first`, `last`, `all` (always a list), `mixed`; a map key or side effect wins over the path and ignores the pop. Vanilla Gremlin 3.7.2 `ImmutablePath` (a list-valued occurrence is not unwrapped; `mixed` replicates `Path.get(label)` on a copy). `select(Pop, traversal)`/`select(traversal)` = `StepSelectTraversal` (key = first child result; string keys like `select("key")`, non-string only as keys of the incoming map; not found filters; last `by()` applies). Every pop except `last` and every traversal key needs `Full` paths. The plan-time undeclared-label check (`required_labels()`) is switched off by any `MAP_PRODUCING_KINDS` step. Tests `pop_tests.rs`, `pop_notation_tests.rs` (extend for a new form). Docs `select_pop.md`.

## Profiling, errors, rendering, execute

- `.profile()` returns `TraversalMetrics` (`traversal/metrics.rs`, getters, `Display` = table, `to_property()`/`to_json()`), recorded by `ProfileContext` (`step/profile.rs`) only inside `is_profile` gates. It shows the OPTIMIZED plan, fused steps under real names (`v(labels: ["person"]).count()`), accurate timings and counts. **A new `ProfileType` only adds an optional section to `Metrics`**; existing fields never change (`execute_tests.rs`).
- Memory: a bare `.profile()` = `.profile_with(ProfileType::none())`, no memory work, no `memory` section. `.profile_with(ProfileType::Memory)` adds `MemoryMetrics` (`bytes_materialized`/`bytes_retained`/`alloc_count`/`source`): `MemorySource::Estimated` (size_of/capacity, only for `materializes() == true` steps: `group`/`fold`/`sum`/`mean`/`median`/`path`/`element_map`) or `Tracked` (feature `alloc-tracking`, `TrackingAllocator` installed by the binary).
- Step ids (`Metrics::id`, `"3.1.0"`) are static plan positions passed as `LevelAt`/`StepAt`. Errors carry the same ids (frames skip the display-only `limit(n) [display]`, `plan_frame`): `TraverserError::StepFailed`, `step_location()` -> `StepLocation`. Pre-run errors (validation pass, `plan_check()`, `UndeclaredLabel`, a denied step) are located too (`locate_pre_run_error`); only step-less errors stay bare.
- Rendering: one canonical snake_case text per step (`GraphStep::get_name`), values as Rhai literals (`value/literal.rs`, never `Debug`), cut at 64 chars / 16 items. `g.with("render.spelling", "camel")` (`traversal/spelling.rs`; `--spelling`, `/set spelling`) respells profile and error ONCE in `run()` (`TraverserError::respell`, `context::requested_spelling`, `TraverserError::Spelled`). `respell` renames identifiers followed by `(` outside strings, so `help()` prose is canonical snake_case DSL (`as("a")`, `where(..)`, `out()`, `GType.LONG`, `__.out()`), never Rust API names (`as_`, `where_t`, `out(None)`, `GType::LONG`).
- `execute()`/`execute_with(ExecOptions)` (`traversal/execution.rs`) -> `Execution` (`results`, `error`, `profile`, `mutated()`, `changes()`, `into_result()`), never `Err`; a failed run is rolled back, results empty, partial profile kept. Engine `execute_items`. Rhai: `profile()` returns metrics (failure = script error), `execute(#{profile, profile_types})` a `ScriptExecution` that never throws (`script/execution.rs`). Docs `profiling_and_execute.md`.

## Limits, cancellation, memory budget

- Administrative options (`Update` on `Resource::Option`, lockable via `ExecutionPolicy`, `context.rs`): `traversal.max_traversers` (per step boundary in `execute_steps`), `traversal.max_materialized_bytes` (1 GiB native, 256 MiB wasm32; every bulk expansion, bucket push, `repeat()` iteration), `traversal.max_string_bytes` (16 MiB), `traversal.max_value_depth` (128). The deadline is checked inside materializing loops too. Script-side limits: `script/CLAUDE.md`.
- Cancellation (`traversal/cancel.rs`): `CancelToken::scope(|| ..)` makes every execution on that thread check the token where the deadline is checked -> `TraverserError::Cancelled` (rolled back).
- Memory budget (`traversal/memory_budget.rs`, off by default): `memory.limit` (bytes or `"auto"` = cgroup v2/v1 limit minus `memory.headroom`, read once, never on wasm32; `cgroup_limit_at(root)`) -> `ExecutionOptions::memory`, started once per `run()` (tracked = global live heap, else `memory_footprint_bytes` + counts, cached in `FootprintCache` by seq + counts, not during an open unit with changes), checked at step boundaries and `max_materialized_bytes` checkpoints; never a walk per check. Non-traversal units use `UnitBudget` (imports, `apply_change_set` every 4096 elements, `commit_undo_unit` every outermost unit); only a unit that grew fails with `GraphError::ResourceLimitExceeded`, rolled back. Hosts: CLI `--memory-limit`/`--memory-headroom`, Python `Graph(memory_limit=..)`.
- **A new limit** goes into `book/src/users_guide/resource_limits.md` and gets a `help()` arm (`resource_limit_help` in `error.rs`).
