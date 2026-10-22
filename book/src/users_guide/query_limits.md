# Query Limits

See the [Execution Options Reference](execution_options_reference.md) for the full table of every
`g.with()` key, including how a host embedding this engine can lock `evaluationTimeout` and
`repeat.max_loops` so a query cannot override them.

Nothing bounds the running time of a traversal by default. A Cartesian fan-out such as
`v().out().out().out()` on a large graph can hold the graph lock and a CPU for as long as it
takes. When queries come from users, as in the web playground, set a budget. Only `repeat()`
loops have a default bound, `repeat.max_loops`.

## `evaluationTimeout`

`evaluationTimeout` is the TinkerPop per-request option of the same name
(`Tokens.ARGS_EVAL_TIMEOUT`). It sets the wall-clock budget of one traversal execution, in
milliseconds:

```rust,ignore
let count = graph
    .traversal()
    .with("evaluationTimeout", 5000)
    .v(None)
    .out(None)
    .out(None)
    .count()
    .next()?;
```

In the script DSL:

```rhai,graph=modern
g.with("evaluationTimeout", 5000).V().out().out().count().next()
```

- The value is a non-negative integer number of milliseconds. `0` means no timeout, which is also
  the default when the key is absent.
- The budget covers the whole execution, nested traversals included: a `where()`, `not()`,
  `union()` or `by()` child traversal spends the same budget as its parent.
- `.profile()` executes the traversal, so it honours the budget too. A profile that times out
  returns the error; no partial profile is produced.
- An invalid value (negative, fractional, or not a number) is reported as
  `InvalidOption` when the traversal is **executed**, because `with()` itself cannot fail.
- Setting the key again replaces the previous value.

When the budget runs out, the traversal stops with a `Timeout` error, for example:

```text
Error: Step #3 'out()' execution failed
  at #3: v().out().out().out().count()
                   ^^^^^
  plan:  v().out().lazy_barrier().out().lazy_barrier().out().count()
                                  ^^^^^
Caused by: Traversal exceeded evaluationTimeout of 50 ms (ran 50 ms)
Help: The traversal ran longer than its evaluationTimeout budget of 50 ms. ...
```

The diagnostic names the step that was running, in the query as written and, when the optimizer
changed it, in the executed plan (`plan:`). Its help rewrites the failing query:
narrow the start set with `has_label()`/`has_id()`, cap the stream with `limit()`, or raise
the budget.

## `repeat.max_loops`

A `repeat()` loop on a graph with cycles, such as `g.V().repeat(__.both())`, never runs out of
traversers. Graphersal stops every `repeat()` that is not bounded by `times()` after a number of
iterations, instead of letting it run until the timeout or forever:

```rhai,graph=modern
g.with("repeat.max_loops", 100).V("1").repeat(__.out()).until(__.has("name", "ripple"))
```

- The default is 10 000 iterations. The value must be an integer from 1 to 4 294 967 295; anything
  else is reported as `InvalidOption` when the traversal is executed.
- A loop with `times(n)` is never limited: it already ends after `n` iterations, so
  `repeat(__.out()).times(50000)` runs all of them.
- The limit applies to each `repeat()` separately, nested loops included.
- When a loop exceeds it, the traversal fails with `RepeatLimitExceeded`. The help suggests
  `times()`, `until()`, `simple_path()` in the body, and a raised limit, written on the failing
  loop.

The limit is deterministic: the same query on the same graph always fails after the same
iteration. `evaluationTimeout` bounds wall-clock time instead, including the time a bounded loop
spends. The two complement each other. See [Recursive Traversals](recursive_traversals.md).

## Granularity

The timeout is cooperative. The engine checks the clock:

- between steps,
- every 1024 traversers a step consumes or produces, and
- every 1024 values a step materializes (the copies of a bulk expansion in `fold()`, a terminal
  list, `aggregate()`, `group()` value lists; the members `group()` and `tree()` collect; the sort
  keys of `order()`, and once after its sort), so the timeout is reported at the step that spent
  the time.

A single storage call, such as one full index scan, is not interrupted, so a query can overrun
its budget by the time one such call takes; so can one sort, or one step that copies a single
large value (a `repeat(__.path())` iteration copies the whole growing path once).

## Mutating traversals

A mutating step (`property()`, `add_v()`, `add_e()`, `drop()`, ...) is never interrupted: the
budget is checked only at its step boundaries.

A traversal that times out is rolled back as a whole, also the mutations of the steps before the
timed-out one: a failing traversal leaves nothing behind (see [Transactions](transactions.md)).

## Script expression depth

A script is rejected with `Expression exceeds maximum complexity` when its expressions nest deeper
than **128** levels (64 inside a script-defined function). Every call of a method chain counts as
one level, so a chain of about 120 steps or 30 nested `union(..)` levels is accepted; a 150-step
chain is not.

The limit is the same in debug and release builds. Rhai's own default is 64 in release but only 32
in debug builds, which made the same query run in a release `graphersal` and fail in `cargo test`.
It is not higher because the parser recurses: on a 2 MiB thread stack (the default of spawned
threads) a debug build overflowed the stack at 160/80. Split a very long query into several
statements with a variable (`let people = g.V().hasLabel("person")`).

## Script hosts

- `graphersal --timeout <ms>` sets a default `evaluationTimeout` for every traversal of the session.
  Without the flag there is no timeout. A query overrides the default with its own
  `g.with("evaluationTimeout", ms)`, where `0` disables it.
- The CLI also stops a script whose own Rhai code (a `loop {}`) runs past the same budget,
  because such code is not a traversal and would otherwise bypass the timeout.
- The web playground sets no default timeout: it runs in your own browser, and its **Stop** button
  ends any query or script loop by restarting the engine (the graph comes back as last loaded or
  saved).
- An embedder builds the same defaults with
  `graphersal::script::graph_scope_with_options(graph, &[("evaluationTimeout", 5000.into())], authorizer)`.
  A preset option is the host's; a script that changes `evaluationTimeout` itself asks the
  host's authorizer for `Update` on `Option("evaluationTimeout")` ([Permissions](permissions.md)).
