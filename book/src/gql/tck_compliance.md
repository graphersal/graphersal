# TCK Compliance

Graphersal's GQL is measured against the openCypher TCK (Technology Compatibility Kit): Gherkin
scenarios that each set up a graph, run one query and state the exact expected result, side
effects or error. They exercise the Cypher compatibility forms that `gql()` accepts (`CREATE`,
`MATCH ... RETURN`, `WITH`, `UNWIND`, ...) with Neo4j's semantics, so the kit works as a
ready-made differential test. It serves three purposes:

1. **Compliance measure:** what passes, what does not, and why.
2. **Regression gate:** a test case that passed must keep passing; `cargo test` fails otherwise.
3. **Backlog generator:** the report ranks the missing constructs and lists every wrong
   behaviour.

Graphersal's GQL is not a Cypher implementation; the TCK measures how far its compatibility
forms agree with the behaviour the kit specifies.

## The kit

- **Pinned version:** openCypher TCK release **2024.3** (commit
  `677cbafabb8c3c5eed458fd3b1ec0daec8d67d23`), vendored unmodified under
  `crates/graphersal/tests/opencypher/` (`features/`, the named graphs in `graphs/`, its licence
  and notice). The directory's `README.md` records the upgrade procedure. A *test case* is a
  scenario, or one `Examples` row of a scenario outline.
- **Runner:** the integration test target `crates/graphersal/tests/opencypher_tck.rs` (feature
  `gql`), with its harness in `crates/graphersal/tests/opencypher/harness/`. Every case runs on a
  fresh graph through the Rust GQL API: the setup queries (`having executed`, the named graphs)
  run through `gql()`'s own writes, the query runs with its parameters, then its rows, its side
  effects (a state diff) and its error are compared. Every query runs under resource limits and
  a wall-clock guard.
- **Gate:** `crates/graphersal/tests/opencypher/baseline.toml` lists every in-scope case that
  does not pass yet, with its class and a one-line reason.

## Headline numbers

The numbers below are the kit's result on the current code. All totals and percentages count
in-scope cases only (see [Out of scope](#out-of-scope)). The text between the
`opencypher:begin` / `opencypher:end` markers on this page is **generated** by the run and
checked by every test run (see [Running and regenerating](#running-and-regenerating)); never
edit it by hand.

<!-- opencypher:begin scope -->
3712 in-scope test cases; 185 excluded (procedures 52, upstream-ignore 1, type-error-at-evaluation 131, w21-property-kinds 1).
<!-- opencypher:end scope -->

<!-- opencypher:begin headline -->
| | Test cases | Share |
|---|---:|---:|
| **Passed** | **2192** / 3712 | **59.1 %** |
| Unsupported (not yet or refused) | 1369 / 3712 | 36.9 % |
| **Wrong behaviour of supported constructs** | **151** / 3712 | **4.1 %** |
| Passed without `expressions/temporal` | 2192 / 2708 | 80.9 % |

Unsupported = missing 1279, setup 90. Wrong behaviour = wrong_result 30, wrong_columns 5, wrong_side_effects 0, wrong_error 39, unexpected_error 77, timeout 0, panic 0.
<!-- opencypher:end headline -->

The date and time functions (`date()`, `datetime()`, `duration()`, `datetime.truncate()`, ...)
are not yet supported in GQL, and `expressions/temporal` is about a quarter of the kit, so the
headline also shows the share without it. The other large blocks are `MERGE` and `DELETE` (the
next write stages; `SET` and `REMOVE` pass) and list indexing; `rand()` is refused by design
([Refused Constructs](refused.md)). The ranking with counts is in the report.

Per sub-directory of the kit:

<!-- opencypher:begin directories -->
| Directory | Pass % | Passed | Total | Unsupported | Wrong |
|---|---:|---:|---:|---:|---:|
| `clauses/create` | 92.3 % | 72 | 78 | 3 | 3 |
| `clauses/delete` | 0.0 % | 0 | 41 | 41 | · |
| `clauses/match` | 96.9 % | 369 | 381 | 4 | 8 |
| `clauses/match-where` | 97.1 % | 33 | 34 | · | 1 |
| `clauses/merge` | 0.0 % | 0 | 75 | 75 | · |
| `clauses/remove` | 100.0 % | 33 | 33 | · | · |
| `clauses/return` | 87.3 % | 55 | 63 | 6 | 2 |
| `clauses/return-orderby` | 82.9 % | 29 | 35 | 1 | 5 |
| `clauses/return-skip-limit` | 74.2 % | 23 | 31 | 1 | 7 |
| `clauses/set` | 100.0 % | 52 | 52 | · | · |
| `clauses/union` | 100.0 % | 12 | 12 | · | · |
| `clauses/unwind` | 71.4 % | 10 | 14 | 2 | 2 |
| `clauses/with` | 89.7 % | 26 | 29 | 1 | 2 |
| `clauses/with-orderBy` | 73.3 % | 214 | 292 | 70 | 8 |
| `clauses/with-skip-limit` | 66.7 % | 6 | 9 | 2 | 1 |
| `clauses/with-where` | 73.7 % | 14 | 19 | · | 5 |
| `expressions/aggregation` | 62.9 % | 22 | 35 | 13 | · |
| `expressions/boolean` | 85.1 % | 40 | 47 | · | 7 |
| `expressions/comparison` | 56.9 % | 41 | 72 | 7 | 24 |
| `expressions/conditional` | 100.0 % | 13 | 13 | · | · |
| `expressions/existentialSubqueries` | 100.0 % | 10 | 10 | · | · |
| `expressions/graph` | 82.0 % | 41 | 50 | 8 | 1 |
| `expressions/list` | 60.5 % | 112 | 185 | 42 | 31 |
| `expressions/literals` | 99.2 % | 130 | 131 | 1 | · |
| `expressions/map` | 60.5 % | 23 | 38 | 15 | · |
| `expressions/mathematical` | 100.0 % | 6 | 6 | · | · |
| `expressions/null` | 100.0 % | 44 | 44 | · | · |
| `expressions/path` | 42.9 % | 3 | 7 | · | 4 |
| `expressions/pattern` | 90.0 % | 45 | 50 | 1 | 4 |
| `expressions/precedence` | 86.0 % | 104 | 121 | 9 | 8 |
| `expressions/quantifier` | 88.0 % | 521 | 592 | 63 | 8 |
| `expressions/string` | 100.0 % | 32 | 32 | · | · |
| `expressions/temporal` | 0.0 % | 0 | 1004 | 1004 | · |
| `expressions/typeConversion` | 95.7 % | 45 | 47 | · | 2 |
| `useCases/countingSubgraphMatches` | 100.0 % | 11 | 11 | · | · |
| `useCases/triadicSelection` | 5.3 % | 1 | 19 | · | 18 |
| **all** | **59.1 %** | **2192** | **3712** | 1369 | 151 |
<!-- opencypher:end directories -->

## Out of scope

Some cases are excluded from the scope: they are not run, get the class `excluded`, have no
baseline entry and are in no total or percentage. They are listed by case id, grouped by reason,
in `crates/graphersal/tests/opencypher/harness/scope.rs` (ids, never tags or content, so a case
that a kit upgrade adds is never excluded silently). Merely unimplemented constructs are never
excluded: they stay `missing`, the backlog.

<!-- opencypher:begin excluded -->
| Code | Why | Docs | Test cases |
|---|---|---|---:|
| `procedures` | vendor procedures: the scenarios register test procedures and CALL them; procedure calls are vendor extensions, not GQL, and refused for good | [gql/refused.md](../gql/refused.md) | 52 |
| `upstream-ignore` | marked @ignore by the TCK itself ("does not work in Cypher") | [gql/tck_compliance.md#out-of-scope](../gql/tck_compliance.md#out-of-scope) | 1 |
| `type-error-at-evaluation` | the TCK expects a type error at compile time; GQL runs inside a Gremlin step: type errors surface at evaluation time (22G03 when a row reaches the expression) (deliberate deviation) | [gql/semantics.md#type-errors-surface-at-evaluation-time](../gql/semantics.md#type-errors-surface-at-evaluation-time) | 131 |
| `w21-property-kinds` | a list of maps is a valid property value (maps and lists of any kinds are stored as given); Neo4j raises a TypeError (deliberate deviation) | [gql/semantics.md#writes](../gql/semantics.md#writes) | 1 |
| **all** | | | **185** |
<!-- opencypher:end excluded -->

- **`procedures`:** the `clauses/call` scenarios register test procedures and call them.
  Procedure calls are vendor extensions, not part of GQL, and refused for good.
- **`upstream-ignore`:** the one scenario that the kit itself marks `@ignore`.
- **`type-error-at-evaluation`** (a deliberate deviation, user decision): the kit expects a
  `SyntaxError` or `TypeError` *at compile time* (detail `InvalidArgumentType`) when an operand's
  kind is already known from the query text, as in `RETURN 123 AND true` or
  `WITH 123 AS x RETURN x.num`. Graphersal's GQL runs inside a Gremlin step, so type errors
  surface at evaluation time: the query is accepted and raises `22G03` when a row reaches the
  expression ([Semantics](semantics.md#type-errors-surface-at-evaluation-time)). The ids were
  generated once from a run and reviewed; a guard test checks that each of these cases still
  expects that error and still gets `22G03` at runtime.
- **`w21-property-kinds`** (a deliberate deviation, user decision): a list of maps is a valid
  property value in Graphersal, where Neo4j raises a `TypeError`
  ([Semantics](semantics.md#writes)).

The deliberate deviations run in a guard test (`deviation_cases_still_deviate`): it fails when
one of them passes, so it moves back into scope. `id()` (an integer in Neo4j, a string id here)
does not occur in the pinned kit.

## Classes

| Class | Meaning |
|---|---|
| `pass` | every expectation holds: rows (and their order where stated), column names, side effects, the error type and phase |
| `missing` | the query uses a construct that `gql()` refuses or does not support yet (GQLSTATUS `0A000`); the report ranks the constructs |
| `setup` | a setup step (`having executed`, a named graph, a parameter value) does not run, usually for the same reason |
| `excluded` | out of the scope (above): not run, in no total |
| `wrong_result` | the rows differ |
| `wrong_columns` | the column names differ |
| `wrong_side_effects` | the side effects differ, or a failed query changed the graph |
| `wrong_error` | an error was expected and another one (type or phase), or none, came |
| `unexpected_error` | an error where a result was expected |
| `timeout` | a time or resource budget of the harness fired |
| `panic` | a panic was caught (a rule-1 violation, top priority) |

The headline groups them: **Unsupported** = `missing` + `setup`; **Wrong behaviour of
supported constructs** = the seven classes from `wrong_result` to `panic`. An unimplemented
construct is fine; a supported construct must behave as the kit specifies, unless a user
decision says otherwise (then the case is excluded with its reason and documented as a
difference from Neo4j). The wrong-behaviour count is driven to 0.

Errors match by type and phase only, never by the detail text. The phase comes from where the
error arose: *compile time* is an error of parsing, binding or planning, before the traversal
runs; *runtime* is an error while it runs.

## Running and regenerating

```bash
# Run the kit (also part of the plain `cargo test --package graphersal --features script,io,display,serde,persist,gql`)
cargo test --package graphersal --features script,io,display,serde,persist,gql --test opencypher_tck

# Rewrite the baseline AND the generated blocks of this page after intended changes; review the diff like code
OPENCYPHER_UPDATE_BASELINE=1 cargo test --package graphersal --features script,io,display,serde,persist,gql --test opencypher_tck
```

Every run prints a summary to stderr and writes the full report to
`target/opencypher-report.md`: the headline, the per-directory table, the exclusions with their
ids, the full wrong-behaviour list by class, the missing constructs ranked by the number of
cases they block, the setup failures by reason and the baseline class changes.
`OPENCYPHER_DETAILS=1` prints one line per in-scope case that does not pass;
`OPENCYPHER_LANES=<n>` sets the number of parallel lanes (default: one per core).

The test fails when:

- a case that is not in the baseline does not pass (a regression);
- a case in the baseline passes now, no longer exists or is excluded (the entry is stale; remove
  it in the same change);
- an id of the scope table does not exist, a procedure or `@ignore` case is not in it, or a
  deliberate deviation passes now (`scope_table_is_consistent`, `deviation_cases_still_deviate`);
- a generated block of this page (`scope`, `headline`, `directories`, `excluded`) differs from
  what the run computes. Only trailing whitespace is ignored.

A case still failing under another class than recorded is reported, not failed.
`GRAPHERSAL_TEST_STORAGE=forwarding` runs the whole kit on a storage that is not
`TraversalGraph` (the `Forwarding` wrapper of `graphersal-storage-tests`): the same gate against
the same baseline and page, it additionally fails when a case changes class, never rewrites
either and writes `target/opencypher-report-forwarding.md`.
