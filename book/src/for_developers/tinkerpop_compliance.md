# TinkerPop Compliance

Graphersal measures its Gremlin compliance against the reference semantics that Apache TinkerPop
ships as executable Gherkin scenarios. Each scenario names a graph, a traversal and the exact
expected result, so the suite works as a ready-made differential test against real Gremlin. It
serves three purposes:

1. **Compliance measure:** what passes, what does not, and why, grouped by root cause.
2. **Regression gate:** a scenario that passed must keep passing; `cargo test` fails otherwise.
3. **Backlog generator:** the failure report ranks the fix tasks.

## The suite

- **Pinned version:** Apache TinkerPop **3.8.2** (commit
  `952d8d8e4c3cd37ed8e192ecb97348e6e82594a9`), vendored under
  `crates/graphersal/tests/tinkerpop/features/`. Only the scenarios in the compatibility scope count
  (see [Out of scope](#out-of-scope) and [Deliberate incompatibilities](#deliberate-incompatibilities));
  their number is in the generated [headline numbers](#headline-numbers). The vendored
  directory's `README.md` records the upgrade procedure.
- **Runner:** the integration test target `crates/graphersal/tests/tinkerpop_features.rs`, with its
  harness in `crates/graphersal/tests/tinkerpop/harness/`. Every scenario is translated to the Rhai
  DSL and runs through the script engine, the same path users take, so the suite also tests DSL
  parity.
- **Gate:** `crates/graphersal/tests/tinkerpop/baseline.toml` lists every scenario that does not
  pass yet, with its class and a one-line reason.

## Headline numbers

The numbers below are the suite's result on the current code, with the strategy scenarios and the deliberate
incompatibilities excluded from the compatibility scope. All totals and percentages count in-scope scenarios only. The scope, the tables and the
counts between the `tinkerpop:begin`/`tinkerpop:end` markers on this page are **generated** by the suite and
checked by every test run (see [Running and regenerating](#running-and-regenerating)); never edit them by hand.

Scope of this run:

<!-- tinkerpop:begin scope -->
1675 in-scope scenarios; 215 excluded (strategies), 182 excluded (deliberate incompatibilities).
<!-- tinkerpop:end scope -->

<!-- tinkerpop:begin headline -->
| | Scenarios | Share |
|---|---:|---:|
| **Passed** | **1631** / 1675 | **97.4 %** |
| Skipped | 5 / 1675 | 0.3 % |
| Unsupported (not implemented) | 39 / 1675 | 2.3 % |
| **Wrong behaviour of supported steps** | **0** / 1675 | **0.0 %** |
| Passed of executed (non-skipped) | 1631 / 1670 | 97.7 % |

Unsupported = translate 32, missing 7. Wrong behaviour = runtime 0, wrong_result 0, wrong_error 0, timeout 0, panic 0.
<!-- tinkerpop:end headline -->

Every remaining non-pass is a missing step, overload or token, or a literal type the translator cannot
express (see [Classes](#classes)); the scenarios that ran but differed on purpose are the
[deliberate incompatibilities](#deliberate-incompatibilities). The per-class counts are in the line below the table.

<!-- tinkerpop:begin categories -->
| Category | Pass % | Passed | Total | Skipped | Unsupported | Wrong |
|---|---:|---:|---:|---:|---:|---:|
| `branch` | 100.0 % | 142 | 142 | · | · | · |
| `data` | 88.0 % | 73 | 83 | · | 10 | · |
| `filter` | 98.9 % | 358 | 362 | · | 4 | · |
| `integrated` | 100.0 % | 4 | 4 | · | · | · |
| `map` | 97.1 % | 759 | 782 | 3 | 20 | · |
| `semantics` | 97.2 % | 103 | 106 | · | 3 | · |
| `sideEffect` | 98.0 % | 192 | 196 | 2 | 2 | · |
| **all** | **97.4 %** | **1631** | **1675** | 5 | 39 | · |
<!-- tinkerpop:end categories -->

<!-- tinkerpop:begin inventory -->
`API inventory (TinkerPop 3.8.2): 19 of 151 methods, 5 of 21 token classes, 37 of 131 members of registered token classes not registered`
<!-- tinkerpop:end inventory -->

The largest remaining blockers are the `call()` step (its name is a Rhai built-in function), the
set literals and `GType.SET`/`TREE`/`VPROPERTY` (no such value types), ranked with their scenario
counts in the report and in the API inventory section of it. The date and time steps and literals
(`asDate`, `datetime`, `dateAdd`, `dateDiff`) pass since 0.2.0.
The full ranking is in the report. The open gaps are listed in
[TinkerPop Deviations](tinkerpop_deviations.md#known-gaps-not-fixed-yet).

## Out of scope

Graphersal will provide subgraph views, partitions, read-only mode and row-level security through
its own, more flexible access-control mechanism, and execution settings through its own unified,
hierarchical execution-metadata mechanism (replacing `with()`/`withStrategies()`/parameters) — not
through TinkerPop `TraversalStrategy`s. GraphComputer/OLAP strategies do not apply to an in-memory
OLTP engine. Therefore every scenario whose traversal, graph initializer or graph-count traversal
calls `withStrategies(...)` or `withoutStrategies(...)` is **excluded from the compatibility
scope**: it is not run, gets the class `excluded`, has no baseline entry, and is left out of every
total and percentage. Scenarios that only use `withSideEffect`, `withSack` or `with(key, value)`
stay in scope. The rule matches the strategy call wherever the scenario lives, not the feature file
name; the table is `crates/graphersal/tests/tinkerpop/harness/scope.rs`.

| Reason | Strategies |
|---|---|
| Views, partitions, read-only, row-level security: Graphersal's own access-control mechanism | `SubgraphStrategy`, `PartitionStrategy`, `ReadOnlyStrategy` |
| GraphComputer/OLAP, not applicable | `VertexProgramStrategy`, `HaltedTraverserStrategy`, `VertexProgramRestrictionStrategy`, `ComputerVerificationStrategy`, `ComputerFinalizationStrategy`, `MessagePassingReductionStrategy`, `GraphFilterStrategy` |
| Strategy configuration not supported: Graphersal's own execution-metadata mechanism | every other strategy (catch-all), e.g. `ProductiveByStrategy`, `RepeatUnrollStrategy`, `SeedStrategy`, `CountStrategy`, `EarlyLimitStrategy`, `OptionsStrategy`, the verification strategies |

The counts per reason and strategy (generated):

<!-- tinkerpop:begin excluded -->
Excluded from compatibility scope: 215 scenarios [views/partitions/read-only/row-level security are provided by Graphersal's own access-control mechanism, not by TinkerPop strategies: 93 (SubgraphStrategy 62, PartitionStrategy 24, ReadOnlyStrategy 7); GraphComputer/OLAP — not applicable to Graphersal: 16 (VertexProgramStrategy 3, VertexProgramRestrictionStrategy 2, ComputerVerificationStrategy 2, ComputerFinalizationStrategy 2, HaltedTraverserStrategy 3, MessagePassingReductionStrategy 2, GraphFilterStrategy 2); TinkerPop strategy configuration is not supported — Graphersal will provide its own unified, hierarchical execution-metadata mechanism (replacing with()/withStrategies()/parameters): 106 (AdjacentToIncidentStrategy 4, ByModulatorOptimizationStrategy 2, ConnectiveStrategy 2, CountStrategy 2, EarlyLimitStrategy 3, EdgeLabelVerificationStrategy 3, ElementIdStrategy 2, FilterRankingStrategy 2, IdentityRemovalStrategy 2, IncidentToAdjacentStrategy 2, InlineFilterStrategy 2, LambdaRestrictionStrategy 2, LazyBarrierStrategy 2, MatchAlgorithmStrategy 3, MatchPredicateStrategy 2, OptionsStrategy 3, OrderLimitStrategy 2, PathProcessorStrategy 2, PathRetractionStrategy 2, ProductiveByStrategy 29, ProfileStrategy 2, ReferenceElementStrategy 2, RepeatUnrollStrategy 18, ReservedKeysVerificationStrategy 3, SeedStrategy 6, StandardVerificationStrategy 2)]
<!-- tinkerpop:end excluded -->

Most of them are in `integrated/` (all its strategy feature files); the rest live in a few other
files (`map/Max`, `Mean`, `Min`, `Sum`, `filter/Sample`, `sideEffect/Aggregate` and single scenarios elsewhere).
A feature file whose scenarios are all excluded is not listed in the report's per-file table. The
report's "Excluded from compatibility scope" section lists the counts per reason and strategy and every
excluded id.

## Deliberate incompatibilities

Some scenarios cannot pass because Graphersal chose a different behaviour, or does not plan the feature at all.
Each reason is a design decision, is documented on the page linked in the table and as a row of
[TinkerPop Deviations](tinkerpop_deviations.md), and is listed explicitly by scenario id in
`INCOMPATIBILITIES` in `crates/graphersal/tests/tinkerpop/harness/scope.rs` (ids, never tags, so a scenario a
suite upgrade adds is never excluded silently). These scenarios are not run, have no baseline entry and are
in no total or percentage; a guard test fails when one of them starts to pass, and another when an id does
not exist. To move one back into scope, remove its id from the table and run the suite.

<!-- tinkerpop:begin incompat -->
| Code | Where (feature files) | Why | Docs | Scenarios |
|---|---|---|---|---:|
| `select-undeclared-label` | Select | select("a"), select("a", "b") and select(Pop.x, ..) on a step label that no as() or side-effect key of the query declares raise Graphersal's own detailed diagnostic, which names the label and shows how to declare it, before execution; TinkerPop silently filters the traverser out. A silent filter hides typos. | [for_developers/tinkerpop_deviations.md#deliberate-differences](../for_developers/tinkerpop_deviations.md#deliberate-differences) | 9 |
| `property-element-order` | Orderability | order() over properties() elements: TinkerPop orders Property elements by their own total order (key, then value, with cross-type rules) and by property id; properties() here yields value-bearing handles that are ordered by value only, and Graphersal has no property ids. Property elements are handles, not values. | [users_guide/property_elements.md#deviations](../users_guide/property_elements.md#deviations) | 5 |
| `multi-meta-properties` | AddVertex, Combine, Conjoin, Dedup, Difference, Disjunct, Drop, Element, Has, HasLabel, Intersect, Local, Merge, MergeVertex, Product, Repeat, Select, ValueMap | The storage model is one value per property key, by design, with no properties on properties: Cardinality.list and Cardinality.set fail with UnsupportedCardinality and meta-property arguments are not stored. Every non-passing scenario tagged @MultiProperties or @MetaProperties that runs here (the executable part of the reason) is listed. The scenarios that need the crew dataset (the multi-/meta-property dataset, which the GraphSON import loads with one value per key: several entries fold into an array and meta-properties into _meta) are listed too. | [users_guide/upserts.md#not-supported-yet](../users_guide/upserts.md#not-supported-yet) | 41 |
| `null-property-removal` | AddEdge, AddVertex, MergeEdge | Scenarios tagged @DisallowNullPropertyValues: TinkerPop removes the property on property(k, null); Graphersal stores a real null, and removal is the explicit remove_property() step. By design: no hidden deletes. | [users_guide/removing_properties.md](../users_guide/removing_properties.md) | 3 |
| `map-to-string` | AsString | valueMap().asString() yields Java Map.toString text ({name=[marko]}) in TinkerPop; Graphersal never produces Java formats as data text (the physical codec is the only text form), so a map is a cast error. | [users_guide/property_elements.md#deviations](../users_guide/property_elements.md#deviations) | 2 |
| `path-retraction-artefact` | Paths | expected result depends on TinkerPop's PathRetractionStrategy dropping repeat-loop labels; Graphersal returns the full shortest-path set (verified on TinkerPop: 24 with the strategy, 30 without) | [for_developers/tinkerpop_deviations.md#deliberate-differences](../for_developers/tinkerpop_deviations.md#deliberate-differences) | 1 |
| `match-step` | Local, Match | The declarative match() step is not planned: declarative pattern matching will come through a Cypher/GQL front end on top of the same engine instead. match is also a reserved Rhai word. | [for_developers/tinkerpop_deviations.md#excluded-for-good](../for_developers/tinkerpop_deviations.md#excluded-for-good) | 36 |
| `extra-number-types` | AsNumber, BigDecimal, BigInt, Byte, Sack, Short, TypeOf | The number model is int64 and float64 by design: GType.BYTE, SHORT, BIGINT, BIGDECIMAL, CHAR and BINARY and BigInteger literals have no Graphersal value type and are not planned; the width literals 1b/1s/1n/1m collapse to int64/float64 in the harness. | [for_developers/tinkerpop_deviations.md#excluded-for-good](../for_developers/tinkerpop_deviations.md#excluded-for-good) | 42 |
| `io-step` | Read, Write | The io() source step with read()/write() and the IO tokens is not planned: GraphML import and export is Graphersal's own API (GraphSource::from_graphml, import_graphml, the CLI --graph), not a traversal step. | [for_developers/tinkerpop_deviations.md#excluded-for-good](../for_developers/tinkerpop_deviations.md#excluded-for-good) | 12 |
| `graph-algorithms` | ConnectedComponent, PageRank, PeerPressure, ShortestPath | The GraphComputer vertex-program steps pageRank(), shortestPath(), connectedComponent() and peerPressure() (and their token classes) are not planned for the traversal language; these scenarios are also @GraphComputerOnly. | [for_developers/tinkerpop_deviations.md#excluded-for-good](../for_developers/tinkerpop_deviations.md#excluded-for-good) | 31 |
| **all** | | | | **182** |
<!-- tinkerpop:end incompat -->

The report's "Deliberate incompatibilities" section has the same table, with the full id list per code.

## Running and regenerating

```bash
# Run the suite (also part of the plain `cargo test --package graphersal --features script,io,display,serde,persist,gql`)
cargo test --package graphersal --features script,io,display,serde,persist,gql --test tinkerpop_features

# Rewrite the baseline AND the generated blocks of this page after intended changes; review the diff like code
TINKERPOP_UPDATE_BASELINE=1 cargo test --package graphersal --features script,io,display,serde,persist,gql --test tinkerpop_features
```

Every run prints the summary to stderr and writes the full report to `target/tinkerpop-report.md`:
headline, per-category and per-file tables, failures grouped by class (within `missing` ranked by
the missing name, within `runtime`/`wrong_result` by the exercised step), baseline class changes,
order-relaxed passes and the API inventory. `TINKERPOP_DETAILS=1` prints one line per non-passing
scenario; `TINKERPOP_LANES=<n>` sets the number of parallel worker lanes (default: one per core).

The test fails when:

- a scenario that is not in the baseline fails (a regression);
- a scenario in the baseline passes now, no longer exists or is excluded from the scope (the entry
  is stale; remove it in the same change);
- an id of the deliberate-incompatibility table does not exist, is also excluded by a strategy, has no
  existing doc page, or names a scenario that passes now (`deliberate_table_is_consistent`,
  `deliberate_scenarios_do_not_pass_silently`).

- a generated block of this page (between `<!-- tinkerpop:begin NAME -->` and `<!-- tinkerpop:end NAME -->`:
  `scope`, `headline`, `categories`, `inventory`, `excluded`, `incompat`) differs from what the run
  computes. The message shows the differing lines and the command above. Only trailing whitespace is
  ignored. A missing, duplicated or unclosed marker fails with a clear error too.

A normal run never writes a file (apart from the report under `target/`). With `TINKERPOP_UPDATE_BASELINE=1` the
run rewrites `baseline.toml` and the text between the markers of this page, nothing else: the prose and the
step tables are hand-written. The blocks render the same Markdown as
`target/tinkerpop-report.md`, so the page and the report cannot disagree. The page path resolves through
`CARGO_MANIFEST_DIR`, so the suite works from the repository root and from the crate directory. Commit the
regenerated page together with the change that moved the numbers.

A baseline scenario that keeps failing under a different class is reported, not failed.

`GRAPHERSAL_TEST_STORAGE=forwarding` runs the whole suite on a storage that is not `TraversalGraph` (the
`Forwarding` wrapper of `graphersal-storage-tests`, with its own element handle types) to prove the front ends
are storage-independent: it is gated against the same baseline and generated blocks, additionally fails when a
scenario changes class, never rewrites either (`TINKERPOP_UPDATE_BASELINE` is refused with it) and writes its
report to `target/tinkerpop-report-forwarding.md`.

## Classes

| Class | Meaning |
|---|---|
| `skipped` | `@GraphComputerOnly` (OLAP) or an upstream "unsupported test"; counted in the scope, not executed (the `crew` scenarios are not skipped: they are the [deliberate incompatibility](#deliberate-incompatibilities) `multi-meta-properties`, since Graphersal stores one value per key and its [GraphSON import](../users_guide/graphson.md) maps multi- and meta-properties to arrays and `_meta`) |
| `translate` | the translator cannot express it, e.g. a literal type Graphersal lacks (BigDecimal, BigInteger, byte, short, set, a map with a non-string key) |
| `missing` | Rhai "Function/Variable not found": a missing step, overload or token; the name is extracted |
| `runtime` | an error while executing: the step exists but rejects this input |
| `wrong_result` | executed, but the result differs from the expected one |
| `wrong_error` | expected an error but got a result (an expected error matches by type: any non-`missing` runtime error, whatever its text; a deliberate deviation of the harness, Graphersal's error texts differ from TinkerPop's) |
| `timeout` | exceeded the per-scenario budget |
| `panic` | a panic was caught (a rule-1 violation, top priority) |
| `excluded` | out of the compatibility scope (a `withStrategies`/`withoutStrategies` scenario, or a [deliberate incompatibility](#deliberate-incompatibilities)): not run, in no total |

The headline splits the non-passing, non-skipped scenarios into two groups, because they mean different
things. Graphersal does not plan to support 100 % of Gremlin's steps, but every step it does support must
behave as the specification says:

- **Unsupported** (classes `missing` and `translate`): the step, overload, token or literal type does not
  exist in Graphersal. An open feature gap, not a defect.
- **Wrong behaviour of supported steps** (classes `runtime`, `wrong_result`, `wrong_error`, `timeout`,
  `panic`): the step exists and runs, but differs from the specification or fails. This is the number to
  keep at 0, and any entry in it is a bug.

The report, the console summary and the per-category and per-file tables use the same two groups; the
per-class counts stay in a line below the headline table and in the "Failures by class" section.

## Documented normalizations

The runner applies a small, fixed set of normalizations. None of them hides a semantic gap.

- **Tolerant id comparison.** Graphersal's element ids are strings, TinkerPop's stock datasets use
  small integers with the same values. An expected `d[N].i`/`d[N].l` that stands for an id is
  stringified and compared with the string id. This applies to ids only.
- **Number-class comparison.** Graphersal has `int64` and `float64` only, so numbers compare by class
  (integer vs. floating point), ignoring the declared width: `d[1].i` and `d[1].l` both match an
  `int64`, `d[1].f` and `d[1].d` both match a `float64`.
- **1 ulp float tolerance.** Two `float64` result values compare equal when they are equal or differ
  by at most 1 ulp (`NaN` equals `NaN`, infinities exact, `+0` equals `-0`, other signs must agree).
  This absorbs last-digit differences between Rust's libm and Java (`sin(4.0)`), not an arithmetic
  deviation of Graphersal (see [Deviations](tinkerpop_deviations.md#1-ulp-float-tolerance-in-the-test-surface)).
  It applies to float vs. float result values only, never to integers, ids, orderings or error cases.
- **Order relaxation.** When a scenario expects an `ordered` result, the sequence differs but the
  multiset matches, and the translated traversal has no explicit ordering step (`order()`/`by()`),
  the scenario passes as an "order-relaxed pass", listed separately in the report. Graphersal does
  not guarantee TinkerGraph's insertion order across `GraphStorage` implementations. With an
  explicit ordering step the exact sequence is required.
- **Tree assertions.** `the result should be a tree with a structure of` compares the single
  result as a map of key to nested map (a leaf is an empty map). Keys are compared by value
  (vertices by id, numbers by class, strings) and the siblings of a node match as a set, because
  TinkerPop's `Tree` is an unordered map.
- **Subgraph assertions.** `the result should be a subgraph with the following` compares the
  graph value's `{"vertices": [..], "edges": [..]}` listing with the edge and the vertex table, each
  as a multiset (vertices and edges by id).
- **Lexical-only translation.** The translator from canonical Gremlin to the Rhai DSL rewrites only
  lexical differences between Groovy and Rhai: numeric suffixes (`1L`, `1d`), list and map
  literals, string quoting, parameter and side-effect bindings, and Groovy's static imports (a bare
  `out()` becomes `__.out()`, a bare `desc` becomes `Order.desc`). Token map keys (`T.label:`,
  `(T.id):`, `Direction.OUT:`, and the `t[..]`/`D[..]` keys of parameter maps) become the DSL's
  reserved string keys `"T.label"`, `"T.id"`, `"Direction.OUT"`, `"Direction.IN"`. A missing
  step, overload or token is never emulated: it reaches the engine and fails there, counted
  against that step.
