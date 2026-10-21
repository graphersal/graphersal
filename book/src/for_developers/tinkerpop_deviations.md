# TinkerPop Deviations

This page lists every place where Graphersal knowingly differs from Apache TinkerPop 3.8.2, and every
known gap that is deliberately not fixed (yet). Pass/fail numbers are on
[TinkerPop Compliance](tinkerpop_compliance.md). Each row links to the page that explains the
behaviour for users.

The scenarios of the rows `select()` of an undeclared label, `order()` of property elements,
multi-properties and meta-properties, `property(k, null)`, and `asString()` of a map, and the families
[excluded for good](#excluded-for-good) (`match()`, the extra number types, `io()`, the graph algorithms), are
excluded from the compatibility numbers: see
[Deliberate incompatibilities](tinkerpop_compliance.md#deliberate-incompatibilities).

## Excluded for good

These TinkerPop features are not planned. Their scenarios are listed by id in
`INCOMPATIBILITIES` (`crates/graphersal/tests/tinkerpop/harness/scope.rs`) and are outside the compatibility scope.

| Code | Feature | Why |
|---|---|---|
| `match-step` | `match()` | declarative pattern matching is the start step [`gql()`](../gql/index.md) instead: ISO GQL queries on the same engine: reads (`MATCH ... WHERE ... RETURN ...`) and `INSERT` / `CREATE`, `SET`, `REMOVE`, `DELETE` and `MERGE` writes; `match` is also a reserved Rhai word |
| `extra-number-types` | `GType.BYTE`/`SHORT`/`BIGINT`/`BIGDECIMAL`/`CHAR`/`BINARY`, `BigInteger` literals | the number model is `int64`/`float64`; see [Number widths](#number-width-literals-in-the-test-surface) |
| `io-step` | `io()`, `read()`, `write()`, the `IO` tokens | GraphSON and GraphML import and export is Graphersal's own API (`GraphSource::from_graphson`, `from_graphml`, `import_graphson`, `import_graphml`, the CLI's `--graph`), not a traversal step ([GraphSON](#graphson)) |
| `graph-algorithms` | `pageRank()`, `shortestPath()`, `connectedComponent()`, `peerPressure()` and their token classes | GraphComputer vertex programs, not planned for the traversal language (the scenarios are `@GraphComputerOnly` as well) |

Dates (`asDate`, `dateAdd`, `dateDiff`, `DT`, `GType.DATETIME`) are implemented as TinkerPop specifies them,
with no deviation; Graphersal's additions are listed in
[Dates and Times: TinkerPop Compatibility](../dates/tinkerpop.md).

## Deliberate differences

| Area | TinkerPop | Graphersal | Details |
|---|---|---|---|
| `negate()` / `P.not(p)` | `Compare.negate()` flips the operator | plain complement of the result | [Predicates](../users_guide/predicates.md#deviations) |
| Element ids | numeric ids order numerically | ids are strings, `"10"` sorts before `"9"`; `hasId(P.gt(3))` (an order predicate on a number) is an error whose help shows `hasId(P.gt("3"))`; equality predicates (`eq`, `neq`, `within`, `without`) take numbers and compare their text | [Predicates](../users_guide/predicates.md#deviations) |
| `hasId(id, ids...)` | a later list stays one id | the same; a list flattens in first position only, `null` is an id that matches nothing | [Id Arguments](../users_guide/id_arguments.md) |
| `length()` | counts UTF-16 code units | counts Unicode scalar values; an emoji is 1, TinkerPop gives 2 | [Type Conversion](../users_guide/type_conversion.md) |
| `asNumber(GType.INT\|LONG)` of a float | `NaN`, infinity or out of range wraps or narrows to the target width | truncates toward zero; `NaN`, infinity and out of `int64` range are errors (one `int64` width) | [Type Conversion](../users_guide/type_conversion.md#asbool-and-asnumber-are-explicit-conversions) |
| `asBool()` of a string | `true`/`false` in any letter case | the same; no other text converts (`"1"`, `"yes"` are errors) | [Type Conversion](../users_guide/type_conversion.md#asbool-and-asnumber-are-explicit-conversions) |
| `trim()`/`lTrim()`/`rTrim()` | Java `String.trim()` strips characters up to U+0020 | strips Unicode White_Space (U+00A0, U+3000 too) | [Type Conversion](../users_guide/type_conversion.md#trim-ltrim-and-rtrim) |
| `choose()`/`branch()` with `Pick.any` | runs for a produced value | also runs when the selector yields nothing | [Option Keys](../users_guide/option_semantics.md) |
| Branch children of `union`/`choose`/`branch` | one traverser at a time | stateful children run once over the whole stream; output is branch-major | [Branch Children](../users_guide/branch_batches.md) |
| `where(traversal)` labels | start/end label rules of `configureStartAndEndSteps` | the same rules; an unresolved label filters out | [where() Labels](../users_guide/where_scope.md) |
| `filter(traversal)` | `TraversalFilterStep`: a plain existence test, `as()` in the child is an ordinary label | the same (no deviation); it is *not* an alias of `where(traversal)`, which reads a leading/trailing `as()` as a scope label and accepts a predicate | [filter() and where()](../users_guide/where_scope.md#filter-versus-where) |
| Second `by()` | accepted by some steps | `InvalidModulator` for `aggregate`, `store`, `groupCount`, `valueMap`, `dedup` | [The by() Modulator](../users_guide/by_modulator_single.md) |
| `select()` of an undeclared label | `select("a")` and `select(Pop.x, "a")` where no `as("a")` or side-effect key exists silently filter the traverser | an error before execution that names the label and shows how to declare it (a label declared but only sometimes set still filters); a silent filter hides typos (a map-producing step upstream, such as `valueMap()` or `inject()`, disables the check, so `valueMap().select(Pop.first, "a")` filters as in TinkerPop) | [Select with Pop](../users_guide/select_pop.md#deviations-from-tinkerpop), [Deliberate incompatibilities](tinkerpop_compliance.md#deliberate-incompatibilities), `help()` of the error |
| `order()` of `properties()` elements | `Property` elements have their own total order (key, then value) and property ids | ordered by value only; no property ids | [Property Elements](../users_guide/property_elements.md#deviations) |
| `by(T.id)` / `by(T.key)` / `by(T.value)` over `properties()` elements | `T.id` is the opaque property id; `T.key`/`T.value` are `Property` tokens | `by(T.label)` orders by key (as in TinkerPop); `by(T.id)` is an error (no property ids); `by(T.key)`/`by(T.value)` read the key and the value as in TinkerPop (both also read a map entry, a Graphersal extension; any other value is a cast error, as in TinkerPop) | [Property Elements](../users_guide/property_elements.md#by-tokens-over-property-elements) |
| `Paths::g_V_shortestpath` | the expected 24 rows depend on `PathRetractionStrategy` dropping the repeat-loop `v` labels in the second iteration, which filters every distance-3 candidate | the full shortest-path set, 30 rows: the six extra rows are the true distance-3 shortest paths (vadas-peter, vadas-ripple, ripple-peter, both directions). Verified on TinkerPop: the scenario's query returns 24, the same query with a no-op `filter(__.path())` at the start of the repeat body (which disables the strategy) returns 30. An upstream TinkerPop issue is being prepared | [Deliberate incompatibilities](tinkerpop_compliance.md#deliberate-incompatibilities) |
| Property `null` | `@AllowNullPropertyValues` / `@DisallowNullPropertyValues` flavours | `null` is a real stored value; writing it never removes a property; use `remove_property()` to remove | [Upserts](../users_guide/upserts.md), [Removing Properties](../users_guide/removing_properties.md) |
| Multi-properties, meta-properties | `Cardinality.list`/`set`, properties on properties | one value per key; `list`/`set` writes fail with `UnsupportedCardinality` | [Upserts](../users_guide/upserts.md), [Property Elements](../users_guide/property_elements.md) |
| `has(key, ..)` / `hasNot(key)` on a property element | tests the property's meta-properties (an edge property has none) | tests the key inside the property's object value (`properties("meta").has("a", P.eq(1))` reads `meta.a`, never the owning element's `a`); a non-object value is a cast error | [Property Elements](../users_guide/property_elements.md#has-on-property-elements) |
| `asString()` of a map | Java `Map.toString` (`{name=[marko]}`) | the JSON-like physical text | [Property Elements](../users_guide/property_elements.md) |
| Set side effects | a real `Set` value type | no set value type: `cap()` returns an array in first-seen order, members compare by value | [Set Side Effects](../users_guide/set_side_effects.md) |
| `tree("a")` | progressive tree, bulk counts | the tree grows with the stream, bulk is ignored (see the gaps below for `local()`) | [tree() Side Effect](../users_guide/tree_side_effect.md) |
| `repeat.order = dfs` | does not exist | a Graphersal addition; a `limit()` in the body then counts per traverser | [Recursive Traversals](../users_guide/recursive_traversals.md) |
| Map keys `0.0` / `-0.0` | different keys (`Double.equals`) | the same key (`-0.0` is canonicalized when a number is built) | [Maps With Non-String Keys](../users_guide/value_keyed_maps.md#deviations-from-tinkerpop) |
| Map with a non-string key as a property value | TinkerGraph stores any value | never storable (`InvalidPropertyValue`); a string-keyed map is storable | [Maps With Non-String Keys](../users_guide/value_keyed_maps.md#the-rule) |
| `group()`/`groupCount()`/`tree()` vertex or edge key | the live element, compared by id | a snapshot (element map) materialized once per result; equal within one result | [Maps With Non-String Keys](../users_guide/value_keyed_maps.md#vertex-and-edge-keys-are-snapshots) |
| `sideEffect(traversal)` and bulk | the child runs once per traverser | the child runs once per unit of a bulk-n traverser (the engine-wide bulk rule); the stream passes through unchanged | [Bulk and Barriers](../users_guide/bulk_and_barriers.md) |
| `profile()` metrics | `TraversalMetrics` with camelCase keys (`traverserCount`, `elementCount`, `percentDur`, `dur`) and internal step ids | snake_case keys (`traverser_count`, `element_count`, `percent_duration`, `duration_ns`) plus our fields (`calls`, `count_in`, optional `timing`/`loops`/`memory`), ids are static plan positions `<step>.<child>.<step>` shared with error locations; step names are the canonical DSL rendering of the step (`has_label("person")`, `v(labels: ["person"]).count()`; `g.with("render.spelling", "camel")` gives `hasLabel("person")`), not the step class names (`HasStep([~label.eq(person)])`); a failing `profile()` is an error, `execute()` (ours) returns results, error and metrics of one run | [Profiling and execute()](../users_guide/profiling_and_execute.md) |
| `limit()`/`count()` in a `repeat()` body | counts across the whole loop | counts per iteration (BFS) or per traverser (DFS); only `dedup()` has loop-wide state | [Recursive Traversals](../users_guide/recursive_traversals.md) |
| `cap()` of `subgraph()` | a graph value | a graph value (a `TraversalGraph` snapshot, empty when nothing was collected): selected edges, both endpoint vertices, ids, vertex label sets and all properties at full depth; the stored schema is copied. In the Rust API `next()`/`to_list()` list it as `{"vertices", "edges"}` (use `to_graph()` for the graph) | [Set Side Effects](../users_guide/set_side_effects.md#subgraphs) |
| `to_graph()` | not in TinkerPop | a Graphersal terminal: `subgraph("sg").cap("sg")` without a label; a stream of anything but edges is an error | [Set Side Effects](../users_guide/set_side_effects.md#subgraphs) |
| `hasLabel()` without arguments | not in TinkerPop (`hasLabel` takes at least one label or a predicate) | a Graphersal extension: keeps the elements that have a label (a vertex with a non-empty label set, an edge with a label); `not(__.has_label())` selects the unlabeled ones; `hasLabel(null)` and an empty list still match nothing | [Label Steps](../traversal/steps/label_steps.md#selecting-labeled-and-unlabeled-elements) |
| `subgraph()` over non-edges, `subgraph()` without a label | not specified here (vanilla fails with a cast error on a vertex; unverified, no network) | an explicit error with a help naming `outE()/inE()/bothE()`, and for the missing label `subgraph("sg")`/`to_graph()` | [Set Side Effects](../users_guide/set_side_effects.md#subgraphs) |
| `withSideEffect("sg", graph)` as the `subgraph()` target | any graph | a graph value of this engine, a `TraversalGraph` (a host graph of another storage fails with `Unsupported`; not the traversed graph: it fails instead of deadlocking); an existing vertex id is reused, an existing edge id skipped, the target's own schema kept | [Set Side Effects](../users_guide/set_side_effects.md#writing-into-your-own-graph) |
| Script expression depth | the JVM stack decides | a Rhai expression-depth limit (128 expressions, 64 in a function); a chain of about 124 steps is rejected cleanly | [Query Limits](../users_guide/query_limits.md) |
| Number widths | byte, short, int, long, BigInteger, BigDecimal | `int64`, `float64` only | below |
| `conjoin()` of a list holding a list or a map | writes Java's `toString` (`[a, b]`, `{k=v}`) for the element | a cast error: Graphersal writes no Java formats as data text (the rule of `asString()` of a map); `conjoin()` or unfold the inner list first; no suite scenario covers it | [conjoin](../traversal/steps/conjoin.md#deviation-from-tinkerpop) |
| `format()` of a list or map value | writes Java's `toString` for the value | a cast error, as for `conjoin()`; no suite scenario covers it | [format](../traversal/steps/format.md#value-text) |
| `TextP.regex()`/`notRegex()` dialect | `java.util.regex` | the Rust `regex` crate's own dialect ([syntax](https://docs.rs/regex/latest/regex/#syntax)); the individual differences are not listed | [Predicates](../users_guide/predicates.md#regular-expressions) |
| Random draws (`coin()`, `sample()`, `Order.shuffle`) | `SeedStrategy` seeds Java's `Random` | `g.with("random.seed", n)` seeds the execution's own generator (Rust `rand`): reproducible per Graphersal version, but not the same draws as TinkerPop for the same seed; the `SeedStrategy` scenarios stay out of scope with `withStrategies()` | [sample](../traversal/steps/sample.md#reproducible-samples), [Execution Options](../users_guide/execution_options_reference.md) |
| `sample(n).by(weight)` | weights any number; with only zero weights left the sampling loop does not end | the weight must be a number of at least 0 (`ValueError::SampleWeight` otherwise); a zero-weight traverser is never drawn, so fewer than `n` can come back; each draw is exactly proportional to the remaining weight | [sample](../traversal/steps/sample.md#weights) |
| `P.typeOf(name)` | any type name registered in TinkerPop's type cache (custom types included) | a fixed list of names: `String`, `Integer`, `Long`, `Double`, `Float`, `Boolean`, `UUID`, `OffsetDateTime`, `Duration`, `Number`, `List`, `Map`, `Vertex`, `Edge`, `Path`, `Graph` (every Java simple class name TinkerPop registers for a `GType` Graphersal has), plus `LocalDate`, `LocalDateTime`, `LocalTime` for Graphersal's own date and time types; `Integer` is `int64` and `Float` is `float64` (one width each) | [Predicates](../users_guide/predicates.md#type-predicates) |
| `min()`/`max()` over incomparable values | a `ClassCastException` for mixed kinds; vertices and edges order by id | a mix of kinds (number and string, boolean and number) fails with a cast-style error naming both kinds; elements, paths and collections are not ordered by `min`/`max` and fail the same way | [Local Scope](../users_guide/local_scope.md#numeric-rules) |
| int + float in `sum`/`min`/`max` | `NumberHelper` widens to double | the same: the result is a float even when the extremum came from an integer; integers beyond 2^53 lose precision when widened; integer-only streams stay exact and overflow raises | [Local Scope](../users_guide/local_scope.md#numeric-rules) |
| `path()`/`simplePath()`/`cyclicPath()` `from()`/`to()` | `Path.subPath(from, to)`: last carrier of each label | the same; an unknown label or a `to()` before the `from()` is an error (TinkerPop raises too, text differs); a label list is an `InvalidModulator` | [Path Windows](../users_guide/path_windows.md#deviations) |
| `by()` of `path()`/`simplePath()`/`cyclicPath()` yielding nothing | drops the traverser (without `ProductiveByStrategy`) | the same, for all three steps | [Path Windows](../users_guide/path_windows.md#deviations) |
| `where(P.gt("a"))` with `by()`; ring order of a composite predicate | label operands take ring entries in the order TinkerPop's connective handling visits them (not verified) | textual order of the predicate, independent of short-circuiting; an unproductive `by()` drops the traverser, also under `not()` | [where() Start and End Labels](../users_guide/where_scope.md#deviations) |
| `group("a")`/`groupCount("a")` on a key that holds another side effect | the map is merged into whatever `a` holds (`withSideEffect("a", [:])` seeds it) | an error naming the key: only group steps that reduce a key the same way may share a side-effect key; a reducing value traversal is re-run over all members of a grown key at the end of each step execution, so readers in the same traversal see reduced values | [group](../traversal/steps/group.md#the-side-effect-form-groupkey) |
| `dedup("a", "b")` with a label that is not on the path | `getSafeScopeValue` raises an error | the traverser is filtered out, like an unproductive `by()` | [dedup](../traversal/steps/dedup.md) |
| `addE()` endpoint that is no path label and no side-effect key | the `select(key)` lookup fails | the name is looked up as a vertex id (`to("2")`) | [addE() Endpoints](../users_guide/add_edge_endpoints.md#deviations) |
| `addE().to(["2", "3"])` | one endpoint per `from()`/`to()` | one edge per name | [addE() Endpoints](../users_guide/add_edge_endpoints.md#deviations) |
| `addE()` endpoint ids | vertex id of the graph's id type | compared as strings (`2` is `"2"`) | [addE() Endpoints](../users_guide/add_edge_endpoints.md#deviations) |
| `id()`/`label()` of an element removed earlier in the same query (`as("a")`, `aggregate()`, a path, a lazy `values()`/`id()` read after the drop) | TinkerGraph still returns the id and label it keeps on the removed element object; properties and edges are gone | properties and edges are gone too, but `id()`, `label()`, materialization and writes fail with `GraphError::ElementRemoved`: a generational handle no longer has any data (the freed slot may hold another element), and it never resolves to that other element | [Dropping Elements](../users_guide/dropping_elements.md#deviation-from-tinkerpop) |
| A traversal that fails after it changed the graph (a failing step, a schema violation, a resource limit, `evaluationTimeout`) | TinkerGraph without transactions keeps the changes made before the error | every traversal is one unit (implicit auto-commit): the failure rolls back all its changes, so nothing of it is applied; in a script the unit is one traversal (a host may make the whole script one unit, and the Rust API has explicit transactions and dry runs) | [Transactions](../users_guide/transactions.md#deviations-from-tinkerpop) |
| `outV()`/`inV()`/`bothV()`/`otherV()` of an edge removed earlier in the same query | TinkerGraph's removed edge object still references its endpoint vertices | empty, like every other adjacency of a removed element (the generational handle has no data left) | [Dropping Elements](../users_guide/dropping_elements.md#an-element-dropped-earlier-in-the-same-query) |
| Explicit transactions | `g.tx()` with `commit()`/`rollback()` on a transactional graph (TinkerGraph itself is not transactional) | no `tx()` in the DSL; the Rust API has `Transactional::transaction(\|g\| ..)` for every storage (a closure: `Ok` commits, `Err` rolls back, every traversal inside is a savepoint), `transaction_with` (commit metadata) and `dry_run` (always rolled back, returns the `ChangeSet`); a host can make a whole Rhai script one unit (`eval_value_atomic`); automatic ids of rolled-back elements are never handed out again | [Transactions](../users_guide/transactions.md#explicit-transactions-rust) |
| Mutation events | `EventStrategy` with `MutationListener`s, called per mutation after it happened | per committed unit: `CommitHook::before_commit` (may veto, the unit is rolled back), `after_commit`, `after_rollback`, each with the whole `ChangeSet` (before and after values); no per-mutation events | [Transactions](../users_guide/transactions.md#commit-hooks) |
| `id()` of a property element (`properties().id()`) | an opaque property id | an error, Graphersal has no property ids (`key()` and `element().id()` work); `Orderability::g_V_properties_order_id` stays failing | [Property Elements](../users_guide/property_elements.md#deviations) |
| `label()`/`hasLabel()` of a property | a vertex property's label is its key; an edge `Property` is no element | the same for `properties()` elements of vertices; edge properties and `values()` handles are cast errors | [Property Elements](../users_guide/property_elements.md#deviations) |
| A vertex or an edge in untyped JSON | GraphSON: vertex `"properties"` map each key to a list of vertex-property objects (`[{"id": .., "value": ..}]`) | `"properties"` holds each value directly (one value per key, no property ids); a multi-label vertex adds `"labels"` | [Vertices and Edges as Values](../users_guide/vertex_and_edge_values.md#in-the-results-tables-and-json) |
| `select(T.id)`, `select(T.label)`, `select(Direction.OUT\|IN)` | not TinkerPop steps (`select()` takes no token) | Graphersal extension: reads that token entry of an `elementMap()`/`valueMap(true)` map; the keys `"id"`/`"label"` name properties | [Properties Named id or label](../users_guide/id_and_label_keys.md#element_map-and-value_maptrue) |
| `remove_property(keys...)` | not a TinkerPop step (removal is `properties(k).drop()`) | Graphersal extension: removes keys (none = all) of vertices/edges and emits the same element; rejected for a property in its label's `required` list in `open`/`closed` schemas | [Removing Properties](../users_guide/removing_properties.md) |
| `add_label(..)`, `drop_label(..)`, `set_label(..)` | not TinkerPop steps (labels are immutable) | Graphersal extensions: change the label set of a vertex or the label of an edge and emit the same element; checked by an `open`/`closed` schema | [add_label, drop_label, set_label](../traversal/steps/label_steps.md) |
| `valueMap(null)` | ambiguous single-null varargs | a null key is dropped, so a lone null means all keys | [Property Elements](../users_guide/property_elements.md#deviations) |
| `Operator` integer overflow | `sum`/`minus`/`mult` promote int to long and long to BigInteger | `int64` only: overflow is an `OperatorFailed` error | [Sack and Operators](../users_guide/sack_and_operators.md#operators) |
| `Operator.sumLong` overflow | wraps around (two's complement) | an `OperatorFailed` error | [Sack and Operators](../users_guide/sack_and_operators.md#operators) |
| `Operator.addAll` of a list and a scalar | 3.7.2 throws, 3.8.2 appends | appends the scalar (the 3.8.2 feature files decide) | [Sack and Operators](../users_guide/sack_and_operators.md#operators) |
| `Operator` null operand | `NumberHelper`: `sum(null, x)` is null, `sum(x, null)` is x | the same (kept deliberately); `min`/`max`/`and`/`or` take the other operand | [Sack and Operators](../users_guide/sack_and_operators.md#operators) |
| Sack value is a vertex or an edge | any object is a valid sack | a cast error (help names `by(T.id)`); use an id or a property | [Sack and Operators](../users_guide/sack_and_operators.md#sack) |
| Sack split and `Supplier` initial values | `withSack(Supplier)` clones per traverser, a `UnaryOperator` splits | not supported; a container sack is shared by handle and never modified in place | [Sack and Operators](../users_guide/sack_and_operators.md#sack) |
| Merging traversers that carry a sack | a sacked traverser never merges without a merge operator | equal-sack traversers merge (invisible after bulk expansion); different sacks never do | [Sack and Operators](../users_guide/sack_and_operators.md#sack) |
| `sack(Operator)` arithmetic widening | a sack of `127b` plus `1b` becomes a `long`, `Long.MAX_VALUE + 1` a `BigInteger` | `int64`/`float64` only: the result is an `int64`, and an overflow is an `OperatorFailed` error naming `sack` | [Sack and Operators](../users_guide/sack_and_operators.md#updating-a-sack) |
| `sack(Operator)` with a vertex or edge operand | any object | `OperatorFailed` or a cast error (a sack never holds an element); `by(T.id)` gives the id | [Sack and Operators](../users_guide/sack_and_operators.md#updating-a-sack) |
| `sack(BiFunction)`, `barrier(Consumer)` lambdas | accepted | not supported: `sack(Operator.sum)` and `barrier(Barrier.normSack)` only | [Sack and Operators](../users_guide/sack_and_operators.md#updating-a-sack) |
| `by()` of `sack(Operator)` | the first result of the child (`TraversalUtil.produce`) | the same; one `by()` only (a second is `InvalidModulator`) | [Sack and Operators](../users_guide/sack_and_operators.md#updating-a-sack) |
| `normSack` numerator | `sack * bulk / total` | with a merge operator the same; without one `sack / total` with `total = sum(sack * bulk)`: equal sacks merge invisibly here, TinkerPop's bulk is 1 without a merge operator, so the answers agree | [Sack and Operators](../users_guide/sack_and_operators.md#normsack) |
| Merge points with a sack merge operator or `withBulk(false)` | `LazyBarrierStrategy` inserts barriers that merge sacks, so the result depends on where they land | only an explicit `barrier()` merges; the `repeat()` frontier and `lazy_barrier()` pass through (`context.auto_merge`); write `repeat(__.out().barrier())` | [Sack and Operators](../users_guide/sack_and_operators.md#automatic-merging-and-sacks) |
| `bulk.merge=false` with a merge operator | not an option | disables every merge including the explicit barrier, so the sack result changes (diagnostic switch) | [Sack and Operators](../users_guide/sack_and_operators.md#automatic-merging-and-sacks) |
| `withBulk(false)` | requirement `ONE_BULK` | the option `bulk.one`; same effect at an explicit barrier; additionally turns the automatic merge points off | [Sack and Operators](../users_guide/sack_and_operators.md#withbulkfalse) |
| Containers and `Full` paths under a merge operator | arrays and maps with equal content merge; path equality is by content | never merge (containers have no key, paths compare by handle), so the operator does not combine there | [Sack and Operators](../users_guide/sack_and_operators.md#automatic-merging-and-sacks) |
| `withSack(init, UnaryOperator)`, three-argument `withSack` | split operators | not supported (error naming the merge `Operator`) | [Sack and Operators](../users_guide/sack_and_operators.md#merge-operator) |
| `aggregate()`/`store()` into `withSideEffect(key, init, Operator)` | 3.7.2 gives the reducer one `BulkSet` per step execution (`sum(1, BulkSet)` throws); 3.8.2 behaviour is only visible in the feature files | one uniform rule per step execution: `assign`/`addAll` receive the collected values as one list, every other operator folds them one by one; inferred from the 24 `sideEffect/Aggregate` Operator scenarios (all pass) | [Sack and Operators](../users_guide/sack_and_operators.md#reducer-operators) |
| `fold(seed, Operator)`, `withSideEffect(key, init, Operator)` lambdas | `fold(seed, BiFunction)`, `withSideEffect(key, init, BinaryOperator)` accepted | only an `Operator` token (an error names the form); a bulk-`n` traverser applies the operator `n` times, no shortcut | [Sack and Operators](../users_guide/sack_and_operators.md#foldseed-operator) |
| `select(Pop.x, ..)` path recording | the path keeps every labelled position | every pop except `Pop.last`, and every `select(.., traversal)` (the key is only known at run time), records full paths and so skips bulk merging upstream (a cost difference only; results are the same) | [Select with Pop](../users_guide/select_pop.md#how-the-occurrences-are-found) |
| `select(Pop.mixed, "a")`, oldest occurrence is a list | `Path.get(label)` appends the later occurrences to the stored list in place | the same result on a copy; the stored value never changes | [Select with Pop](../users_guide/select_pop.md#deviations-from-tinkerpop) |
| `math()` of a trigonometric function | Java `Math.sin` | the platform libm; the last digit of a result can differ (`sin(4.0)` is `-0.7568024953079283` here, `...282` in Java: `map/Math::g_withSackX1X_injectX1X_repeatXsackXsumX_byXconstantX1XXX_timesX5X_emit_mathXsin__X_byXsackX` passes in the harness because float results compare within 1 ulp, see [1 ulp float tolerance](#1-ulp-float-tolerance-in-the-test-surface)) | [Math](../users_guide/math.md) |

### GraphSON

GraphSON 3.0 import and export is Graphersal's own API, not the `io()` step (which stays excluded).
Where it differs from TinkerPop's `GraphSONReader`/`GraphSONWriter` (details and the full type
table on [GraphSON](../users_guide/graphson.md#deviations-from-tinkerpop)):

| Area | TinkerPop | Graphersal |
|---|---|---|
| Element ids | any type (`g:Int32 1`) | strings: an imported `g:Int32 1` is `"1"`; export writes string ids, so TinkerGraph answers `g.V("1")`; with its `LONG` id managers also `g.V(1)` ([recipe](../users_guide/graphson.md#loading-an-export-into-tinkergraph)) |
| Multi- and meta-properties | list cardinality, properties on properties | several entries become one array, meta-properties the reserved `_meta` object property; export restores them from `_meta` only |
| Multi-label vertices | no label sets | written as `"A::B"`, one opaque label in TinkerGraph |
| Unlabeled vertex/edge | always labeled | exported with the default labels `vertex`/`edge`, read back with them |
| Date and time values | read into `java.time` values | read into the temporal types (a `gx:ZonedDateTime` keeps its offset, its zone id is dropped and reported; digits below a microsecond are cut and reported); `gx:Year`, `gx:YearMonth`, `gx:MonthDay`, `gx:OffsetTime`, `gx:ZoneOffset` are skipped and reported; a duration with both months or days and a time part is exported as `graphersal:Interval`, which TinkerPop cannot read |
| `BigDecimal`, `Class`, enum tokens, vendor types | read into Java values | skipped and listed in the import report, never converted |
| Edge to a vertex that is not in the file | `readGraph` fails | skipped and reported (or connected to the target graph's vertex of that id) |
| Duplicate vertex id | an error | the first vertex wins, the second is skipped and reported |
| Vertex-property ids | kept | not kept; export writes a file-wide `g:Int64` sequence |
| `g:Int32`, `g:Float` | kept as such | `int64`/`float64`; export writes `g:Int64`/`g:Double` |

### `jpath(..)` keys wherever a property name is taken

Extension, not a deviation of an existing TinkerPop behaviour. `values`, `properties`, `has` (every
form with a key), `hasNot`, `valueMap`, `elementMap`, every property `by()`, `property` and
`remove_property` also accept `jpath("a.b[1]")`, a singular RFC 9535 path into a nested property
value ([Path Keys](../users_guide/jpath_keys.md)). A plain string is always a literal property name,
exactly as in TinkerPop, and is never parsed as a path. A missing path is an absent property, not an
error. In maps (`valueMap`, `elementMap`) the key of a path entry is the canonical path text
(`$.a.b[1]`). `json_path("a.b[1]")` stays the standalone string-argument step with the same grammar.
A side effect of the key conversion: `has(null, v)` reads the null key as the name `""` and matches
nothing, which is what the TinkerPop scenario `g_V_hasXnull_testnullkeyX` expects.

### `properties(jpath(..))` yields a path property element

Extension, not a deviation of an existing TinkerPop behaviour: `properties("name")` is exactly as in
TinkerPop. `properties(jpath("a.b[1]"))` (a Graphersal path key, see
[Path Keys](../users_guide/jpath_keys.md)) yields `TraverserValue::PathProperty`, a materialized property element: `key()` is the
canonical path text (`$.a.b[1]`), `value()` the leaf, `label()`/`hasKey()` see the path text, `id()` is
the same deliberate error as for any property element, and a terminal materializes the leaf. Reason: a
path has no stored property a lazy handle could point at. It is only produced by a path key, never by a
plain string.

### `property(jpath(..), value)` writes below a stored property

TinkerPop has no nested property write. Graphersal's `property(jpath("a.b[1]"), v)`
([Path Keys](../users_guide/jpath_keys.md)) sets a value inside the stored property `a`: missing object keys are
created, `[n]` replaces, `[len]` appends, anything else out of range or of the wrong type is an
error (`GraphError::InvalidPropertyPath`) and never an overwrite. `property(Cardinality, jpath(..), v)`
is an explicit error in v1. With a schema, the written leaf is coerced at its declared nested type
and the resulting property is validated, `Closed` rejecting undeclared nested keys
([Schemas and nested paths](../users_guide/jpath_keys.md#schemas-and-nested-paths)). A plain string key behaves exactly as in TinkerPop. A traversal as the
key (`property(select("a"), v)`) is still not implemented.

### `remove_property(jpath(..))` removes below a stored property

TinkerPop removes a property with `properties(k).drop()`; Graphersal's `remove_property(..)` is an
extension that keeps the element in the stream, and it now also takes a `jpath(..)` key (user's
guide page "Path keys (jpath)"): an object key is removed, an array element is removed with shift,
and a missing path or a type mismatch on the way is a no-op, never an error (more lenient than the
write side, where a mismatch is an error). `properties(jpath(..)).drop()` is an error, because a path
property element holds no reference to its owner; use `remove_property(jpath(..))`.

## Known gaps, not fixed yet

Counts are in-scope scenarios (`missing` unless noted; the current numbers are in `target/tinkerpop-report.md` after a suite run); a scenario can wait on more than one gap.

| Gap | Effect | Scenarios |
|---|---|---|
| `repeat.order = dfs` body `limit()` | counts per traverser, not per iteration (a Graphersal addition, see above) | none |
| `local(tree("a"))` needs a streaming executor | a step after the `local()` child sees the finished tree, not the progressive one | none known to fail |
| literal types the harness cannot express (`set`) | `translate`; a `{..}` set literal is not mapped to `Set.of(..)` because Graphersal has no set *value* (only a set side-effect seed), so `GType.SET` would have nothing to match | a few, two of them `Orderability` scenarios that also hold dates (byte/short/bigint/bigdecimal are [excluded for good](#excluded-for-good)) |
| `GType.SET`, `GType.TREE`, `GType.VPROPERTY` | `missing`; no set, tree or vertex-property value type | 7 |
| `call()` service step | `translate` (`call` is a Rhai built-in function, so a future step needs a DSL name of its own); no service model (registry, `tinkerpop.dc`, `list`, `search`) yet; may come with the server | 20 |
| `@DisallowNullPropertyValues`, multi-/meta-properties, `valueMap().asString()` | deliberate, outside the compatibility scope | see [Deliberate incompatibilities](tinkerpop_compliance.md#deliberate-incompatibilities) |
| `subgraph()` cap | none: `cap()` is a graph value, the harness compares its `{"vertices", "edges"}` listing with the scenario's edge and vertex tables, and `GType.GRAPH` exists. The 4 `Subgraph` and 3 `TypeOfGraph` scenarios pass | none |

Why `@DisallowNullPropertyValues` stays unfixed: the 3 scenarios expect a write of `null` to remove the
property, the 4 `@AllowNullPropertyValues` scenarios that pass expect it to be stored. Both cannot hold
in one mode; the answer would be a graph-level null mode (not scheduled).

## Number-width literals in the test surface

The harness maps `1b`/`1s`/`1n` to an integer and `1m` to a float, so these scenarios pass by value
equality only. They do **not** show byte, short, BigInteger or BigDecimal support:

- `map/AsNumber.feature`: `g_injectX5bX_asNumber`, `g_injectX5sX_asNumber`, `g_injectX5nX_asNumber`.
- `map/Sum.feature` (12): `g_V_injectX127b_1bX_sumXX`, `g_V_injectX_128b__1bX_sumXX`,
  `g_V_injectX32767s_1sX_sumXX`, `g_V_injectX_32768s__1sX_sumXX`, `g_V_age_injectX1000nX_sum`,
  `g_injectX1b_2b_3bX_sum`, `g_injectX1b_2b_3sX_sum`, `g_injectX1b_26b_3iX_sum`,
  `g_V_age_injectX1000nX_fold_sumXlocalX`, `g_injectX1b_2b_3bX_fold_sumXlocalX`,
  `g_injectX1b_2b_3sX_fold_sumXlocalX`, `g_injectX1b_26b_3iX_fold_sumXlocalX`.
- `sideEffect/Inject.feature::g_injectXbigintBoundaryValuesX`.

That is 16 scenarios. The 8 `semantics/Equality.feature` `Primitives_Number_*` scenarios pass the same
way (byte, short, bigint and bigdecimal included); `Sum` of byte values that overflow a byte is the
case where TinkerPop and Graphersal would differ without the collapse.

## 1 ulp float tolerance in the test surface

The harness compares two `float64` result values as equal when they are equal or at most 1 ulp apart
(`NaN` equals `NaN`, infinities exact, `+0` equals `-0`, other signs must agree). The cause is that
Rust's libm and Java's `Math` differ in the last digit of some results (`sin(4.0)`); Graphersal's
arithmetic does not deviate from TinkerPop. The tolerance applies only to float vs. float result values,
never to integers, ids, orderings or error cases. It lets the one `map/Math` `sin` scenario pass, which is
a pass by tolerance, not proof of bit-identical trigonometry.

## Which scenarios could expose `negate()`

`Compare.negate()` and a plain complement differ only for incomparable operands (`NaN`, a string
against a number). The `P.not`/`negate()` scenarios of the suite (`filter/TypeOf`, `filter/Where`,
`filter/HasLabel`, `integrated/Recommendation`) negate `typeOf`, `within` or comparisons of numbers,
where both agree. The `NaN` scenarios of `semantics/Comparability.feature` (`not(is(P.gt(NaN)))`)
use the `not()` traversal step, which negates the filter result, not the predicate. No vendored
scenario exposes the difference.
