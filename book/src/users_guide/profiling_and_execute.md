# Profiling and execute()

Two terminals tell you how a traversal ran:

- `profile()` runs the traversal and returns **only its metrics**, like TinkerPop's `profile()`.
- `execute()` runs it and returns an **execution object**: the results, the error (as data, never
  thrown) and, when you ask for it, the metrics of the same run.

Both report the *optimized* plan, the one that actually ran: fused steps appear under their real
names (`v(labels: ["person"]).count()`). What a step records into traverser paths is the separate
`path_recording` field (`"full"`, `"labels(a,b)"`); only the table renders it after the name, as
` [path: ...]`.
Profiling instruments every step, so a profiled run is slightly slower; its results are identical.

## profile()

```rhai,graph=modern
g.V().hasLabel("person").where(__.out("knows")).profile()
```

As the final value of a session (the CLI, the REPL, the playground) it renders as the familiar
table:

```text
Traversal Metrics
Step                                                                   Call      In     Out       Time    % Dur
===============================================================================================================
v(labels: ["person"])                                                     1       0       4    3.708µs     0.44
where(__.out("knows"))                                                    1       4       1  131.291µs    15.72
  \> out("knows")                                                         4       4       2  128.000µs    15.33
     [min: 41ns, avg: 32.000µs, max: 127.542µs]
                                                                TOTAL:             execute:  835.000µs    16.17
===============================================================================================================
Optimizer rules applied: source_filter_pushdown
```

The `Step` column is as wide as the longest step text, annotations such as `[index: ..]` and
`[path: ..]` included (at least 60 characters, 70 with nested steps); only a step text longer
than 160 characters is cut with `...`.

In a script it is data, a `TraversalMetrics` value:

```rhai
let p = g.V().hasLabel("person").out("knows").profile();
p.duration_ns                       // the whole run, integer nanoseconds
p.metrics[0].name                   // "v(labels: [\"person\"])"
p.metrics[1].counts.traverser_count // 2
p.optimizer_rules_applied           // ["source_filter_pushdown"]
p.to_map()                          // the whole tree as a map
p.to_json()                         // ... as compact JSON text
p.to_table()                        // the text table above
```

`profile(ProfileType.Memory)` (also `ProfileType::Memory`, and `profile_with(..)`) adds a `memory`
section to every step; see [Memory Inspection and Profiling](memory_and_profiling.md).

A traversal that fails inside `profile()` is a **script error** with the full diagnostic, exactly
like `to_list()`. Use `execute(#{profile: true})` to get the profile up to the failing step.

In Rust, `profile()` / `profile_with(flags)` return `TraverserResult<TraversalMetrics>`; its
`Display` impl renders the table.

## Step names

Every step has one rendering, used by `.profile()`, by the step location of an error
(`at #2: ...` with its caret) and by the `help()` text that rewrites the failing query. It reads
like the DSL call that builds the step:

- snake_case step names: `has_label("person")`, `group_count()`, `side_effect(__.out())`;
- Rhai literals for arguments: `"x"`, `1`, `2.5`, `()`, `[1, 2]`, `#{name: "marko"}`,
  `UUID.from_string("...")`, `jpath("$.a.b")`;
- predicates and tokens as you write them: `P.gt(30).and(P.lt(40))`, `TextP.starting_with("m")`,
  `Order.desc`, `T.label`, `Scope.local`, `Pop.first`, `Column.keys`, `GType.LONG`,
  `Operator.sum`, `Cardinality.list`;
- child traversals as `__.out().has("name", "x")`; a `where()` child shows its start and end
  labels where you wrote them: `where(__.as("a").out().as("b"))`;
- nothing for an argument you left out (`v()`, `out()`), and the default `Order.asc` of a sort
  key is left out too (`order().by("age")`).

A step the optimizer fused or inserted shows under the name of what it does:
`v(labels: ["person"]).count()`, `v(ids: ["1"], labels: ["person"])`,
`e().group_count().by(T.label)`, `add_v("person", properties: ["name"])`, `lazy_barrier()`.
An empty intersection of id filters (`g.V("1").hasId("2")`) is `v([])`, a scan of nothing.
Profile annotations stay after the name in brackets: `[path: full]`, `[lookup: label]`.

`g.with("render.spelling", "camel")` switches the whole rendering to the Gremlin camelCase
spelling for one query (`"snake"` is the default):

```text
$ graphersal -e 'g.with("render.spelling", "camel").V().hasLabel("person").outE().groupCount().profile()'
Traversal Metrics
Step                                                         Call      In     Out       Time    % Dur
=====================================================================================================
v(labels: ["person"])                                           1       0       4    3.958µs     5.86
outE()                                                          1       4       6    2.542µs     3.76
groupCount()                                                    1       6       1    4.667µs     6.91
                                                      TOTAL:             execute:   67.541µs    16.54
=====================================================================================================
Optimizer rules applied: source_filter_pushdown
```

Front ends set it for a session: `graphersal --spelling camel` (REPL: `/set spelling camel`), the
playground's Settings dialog, and in Rust `RenderOptions::with_spelling(Spelling::Camel)` passed on
through `graphersal::script::render_scope_options`. A query's own `g.with(..)` wins. The camelCase
names are the ones the DSL registers for each step (`hasLabel` for `has_label`), so a step that
was not fused can be pasted back into a query in either spelling. Error messages and `help()` follow the same
spelling, also for an error raised before the run (a denied step, an invalid option, a plan-time
check); results never depend on it. In the Rust API, `TraverserError::spelling()` tells which
spelling an error was rendered in; an error without a step frame comes wrapped in
`TraverserError::Spelled` under `camel` (`root_cause()` unwraps it).

Long literal arguments are cut in every rendered step: a string after 64 characters
(`replace("aaaa…" (8192 chars), "b")`) and a list, a map or the arguments of a variadic step
after 16 items (a 10 000-item list shows its first 16 items, then `, …] (10000 items)`), so a
location stays readable.

## The metrics

```text
TraversalMetrics { duration_ns, metrics: [Metrics], optimizer_rules_applied: [string] }
Metrics {
  id, name, duration_ns, percent_duration,
  path_recording?: "full" | "labels(a,b)",                // what the step records into paths
  counts: { traverser_count, element_count },
  calls, count_in,
  timing?: { min_duration_ns, max_duration_ns },          // once the step completed a call
  loops?:  { total_loops, max_depth, avg_loops_per_traverser },   // repeat() only
  memory?: { bytes_materialized, bytes_retained, alloc_count, source },  // ProfileType.Memory
  nested: [Metrics],                                     // steps of the child traversals
}
```

Keys are snake_case everywhere: Rust getters (`metrics.counts().traverser_count()`), Rhai map keys
and JSON keys. Durations are `Duration` in Rust and integer nanoseconds (`duration_ns`) in Rhai
and JSON. A key marked `?` is absent unless it applies.

| TinkerPop `Metrics` | Graphersal | Notes |
|---|---|---|
| `dur` | `duration` (`duration_ns`) | total over all calls of the step |
| `traverserCount` | `counts.traverser_count` | traverser objects the step emitted |
| `elementCount` | `counts.element_count` | logical traversers they stand for (sum of bulks) |
| `percentDur` | `percent_duration` | share of the run's `duration` |
| nested metrics | `nested` | flat list of every child-traversal step, ordered by id |
| — | `calls`, `count_in` | ours: completed calls, traversers handed in |
| — | `timing`, `loops`, `memory` | ours: optional sections |

`traverser_count` and `element_count` differ only when equal traversers were merged
([bulk](bulk_and_barriers.md)); the table then shows a `Bulk` column.

### The extensibility rule

The metrics are data that you may store, compare and parse, so they only ever grow one way:
**a new `ProfileType` adds a new optional section to `Metrics`** (as `memory` did), present only
when that type was requested. The meaning and type of an existing field never change. Rust structs
are `#[non_exhaustive]` and read through getters; consumers of the map or JSON form must ignore
keys they do not know.

## Step ids

Every step of the optimized plan has an id, its **static position** in the plan:

- a top-level step is its index: `"0"`, `"1"`, ...;
- a step inside a child traversal alternates step index and child-traversal index:
  `<step>.<child>.<step>...`. `"3.1.0"` is top-level step 3, its child traversal 1, step 0 in it.

The children of a step are numbered in a fixed order: the step's **own traversal arguments
first, in argument order** (the `union()`/`coalesce()`/`and()`/`or()` branches, the body of
`where()`/`not()`/`filter()`/`local()`/`optional()`/`sideEffect()`, the `repeat()` body, the
`choose()`/`branch()` selector, the key of `select(traversal)`), **then the modulator children in
attachment order** (`by(__...)`, every `option()`'s key traversal and branch, the `until()` and
`emit()` conditions of `repeat()`). A modulator without a traversal (`by("name")`, `times(2)`)
takes no number.

```rhai,graph=modern
g.V().union(__.out("knows"), __.in("created").values("name"))
// 0: v()   1: union(..)   1.0.0: out("knows")   1.1.0: in("created")   1.1.1: values("name")
```

Ids come from the plan, not from execution: a branch that never runs leaves no gap, and a step
called once per traverser keeps one id (its `calls` counts the calls). The same numbering locates
errors (below), so a partial profile and an error join by id.

## execute()

```rhai
let r = g.V().hasLabel("person").out("knows").execute();
r.results     // what to_list() returns
r.error       // () on success, else a map (below)
r.profile     // () unless requested
r.is_ok       // also isOk

let r = g.V().hasLabel("person").execute(#{profile: true});
let r = g.V().hasLabel("person").execute(#{profile_types: [ProfileType.Memory]});  // implies profile
r.to_map()    // #{results, error, profile}
r.to_json()
```

`execute()` **never throws**. Every error is in `r.error`: a runtime error (with the profile up
to the failing step when profiling was on), an error that prevents the run from starting (an
invalid `with()` option, a plan check, a locked option), and a malformed options map
(`execute(#{colour: 1})`). The options are `profile` (bool) and `profile_types` (a list of
`ProfileType` values or their names, `"memory"`).

The error map:

```rhai
#{
  message: "Step #1.1.1 'sum()' execution failed: Cast exception: ...",
  help: "...",                 // how to fix the query, when there is advice
  step_id: "1.1.1",            // the failing step, numbered like Metrics.id
  step_path: [#{id: "1", name: "union(...)"}, #{id: "1.1.1", name: "sum()"}],
  diagnostic: "Error: ...",    // the full text the CLI prints
}
```

`step_id`/`step_path` are absent for an error that has no step (an invalid option). An error
found before the run (a rejected `by()`/`from()`/`to()`, an invalid constant argument such as
`merge_v(1)` or `math("_ +")`, an undeclared step label, a denied step) carries the location of
the step it is about, like a run-time failure. The names in `step_path` are the same plain step
names as `Metrics.name`.

As the final value of a session an execution renders as its results (in the traversal's
visualizer format, within the display limits) followed by the profile table; a failed execution
renders as its diagnostic, followed by the partial profile, and counts as a failure (the CLI exits
with code `1`).

### In Rust

```rust
use graphersal::prelude::*;

let graph = GraphSource::tinkerpop_modern();
let g = graph.read();
let exec = g
    .traversal()
    .v(None::<()>)
    .has_label("person")
    .out(None::<()>)
    .execute_with(ExecOptions::profile(ProfileType::none()));
assert!(exec.error().is_none());
assert_eq!(exec.results().len(), 6);
let profile = exec.profile().unwrap();
assert_eq!(profile.metrics()[0].id(), "0");
let results = exec.into_result().unwrap(); // the Result shape, for `?`
```

When the run fails, none of its changes are applied: the traversal is rolled back as a whole
(see [Transactions](transactions.md)); `results` is empty and `error` and the partial `profile` are
kept. `exec.error()` is `Option<&TraverserError>`; `TraverserError::step_location()` returns its
`StepLocation { step_id, step_path }` (also usable on any error of `to_list()` and the other
terminals). `exec.mutated()` (Rhai `r.mutated`) says whether the run changed the graph and kept the
change: committed on its own, or part of the enclosing unit inside an explicit transaction or a
whole-script unit (see [Transactions](transactions.md)). `exec.changes()` (Rhai `r.changes`, a map
`#{data, schema}`) says what it changed: `UnitChanges { data, schema }`, with `schema` true when the
run set or patched the schema ([Schemas](schema_enforcement.md#the-schema-methods)). `Execution` is `#[non_exhaustive]`: later
per-run data is added to it as new getters.

## TinkerPop differences

- `profile()` returns the metrics in the same shape, with snake_case names and our extra fields
  (table above). The TinkerPop key names are not mirrored.
- Step ids are static plan positions with the child-traversal index in them (`3.1.0`); TinkerPop
  numbers its metrics by internal step ids.
- `execute()` is ours; TinkerPop has no terminal returning results and metrics of one run.
- Step names are the canonical DSL rendering described in [Step names](#step-names) (snake_case,
  or camelCase with `render.spelling`), not TinkerPop's step class names such as
  `HasStep([~label.eq(person)])`.
