# Release 0.2.3

A patch release of the 0.2 line. GQL (still **experimental**) can now write, call saved queries and
work with dates and times, and the playground gains GQL query tabs. It changes no format: stores,
snapshots and journals of 0.2.2 are read and written exactly as before. Its API additions are
additive (new methods, constants and `#[non_exhaustive]` error variants); three Gremlin
behaviours are fixed (see "Fixed").

## Added

- **GQL writes (experimental)** ([Writing with GQL](../gql/writes.md)): `g.gql(query)` is no
  longer read-only. A query is one unit: it commits as a whole or leaves nothing, and the commit
  hooks, the journal, schema defaults and coercion, permissions, resource limits and cancellation
  apply as they do to Gremlin writes.
  - `INSERT` (Cypher spelling `CREATE`) creates nodes and edges once per row; `SET` and `REMOVE`
    change properties and labels (`n.k = e`, `n = map`, `n += map`, `n:A:B`, `REMOVE n.k`);
    `[DETACH | NODETACH] DELETE` removes nodes, edges and paths; `MERGE [p =] <pattern>` with
    `ON CREATE SET` / `ON MATCH SET` finds or creates one pattern per row.
  - A query whose last clause writes may end there and returns no rows; `FINISH` ends any query
    without a result.
  - Writes run through internal steps (`gql:insert_node`, `gql:set`, `gql:delete`, `gql:merge`, ...)
    that ask the same permissions as their Gremlin twins; a saved query that writes is recorded
    with its effects.
  - **Not yet:** `FOREACH` (use `FOR x IN list INSERT ...`), a write inside a `UNION` operand, a
    path variable in `INSERT`, a nested `SET` / `REMOVE` target such as `n.a.b`
    ([Not yet](../gql/writes.md#not-yet)).
- **Saved queries written in GQL** ([Saved Queries](../users_guide/saved_queries.md)):
  `g.define_query(#{name, language: "gql", body, params})` stores a GQL query, read with `$name`
  parameters and parsed, bound and planned when it is saved. A query that reads is recorded in the
  dialect `graphersal-gql/1`, one that writes in `graphersal-gql/2` with its effects
  (`catalog::GQL_QUERY_DIALECT`, `catalog::GQL_WRITING_QUERY_DIALECT`, `QueryDefinition::language()`);
  `queries()` and `get_query()` report `language` and `dialect`. New errors for an undeclared or
  unused `$parameter`. A version that does not know GQL saved queries (0.2.2 and older) lists them
  as not runnable and keeps them unchanged.
- **`CALL` of a saved GQL query** ([Calling saved queries](../gql/subset.md#calling-saved-queries)):
  `CALL folder.name(args) [YIELD ..]`, standalone or once per row inside a query
  (`OPTIONAL CALL`, `YIELD .. WHERE`, a `VOID` call may end a query). A saved query in Rhai and a
  call inside `EXISTS` / `COUNT` / `COLLECT` are not supported yet.
- **GQL dates and times** ([Dates in GQL](../dates/gql.md), [The Supported Subset](../gql/subset.md#dates-and-times)):
  `date()`, `local_time()`, `local_datetime()`, `datetime()` and `duration()` (Cypher spellings
  `localtime()`, `localdatetime()` accepted) from ISO 8601 text, a map of fields or another
  temporal value; components (`d.year`, ...), arithmetic with durations, comparison, the clocks
  (`.statement`, `.transaction`; one instant per execution, inside a script the script's `now()`),
  `X.truncate(unit, value)`, `datetime.fromepoch(millis)` and `duration.between` / `inMonths` /
  `inDays` / `inSeconds`. Values hold microseconds (finer input is refused, never cut), a zone
  name is turned into its offset, and zoned `time()` is not supported yet; every difference from
  Neo4j is listed on [Semantics](../gql/semantics.md#where-dates-and-times-differ-from-neo4j).
- **More GQL expressions and clauses**: list indexing and slicing (`x[i]`, `x[a..b]`, `m[k]`),
  `+` of a list and one value, the power operator `x ^ y`, `percentile_cont` / `percentile_disc`,
  chained comparisons, a constant `LIMIT` / `SKIP`, non-reserved words as names, `ORDER BY` over
  expressions of returned aggregates, a `LET` / `WITH` value as a pattern node or edge, `RETURN *`
  in name order. Equality across kinds is `FALSE`, lists are ordered, float division follows
  IEEE 754 (`NaN`, `Infinity`). A construct that is meaningless for its operand kinds is refused
  with a static error. The deliberate differences from Neo4j and ISO are collected in
  [Semantics](../gql/semantics.md#differences-from-neo4j) and
  [Conformance](../gql/conformance.md).
- **openCypher TCK harness** ([TCK Compliance](../gql/tck_compliance.md)): the openCypher TCK 2024.3
  is vendored unmodified (`crates/graphersal/tests/opencypher/`, not part of the published crate)
  and run by `cargo test --test opencypher_tck` against the compatibility forms GQL accepts, with
  a ratchet baseline and a generated compliance page. At this release 3216 of 3818 in-scope cases
  pass (84.2 %, 95.7 % without the temporal group) and the **wrong behaviour of supported
  constructs is 0**; the rest is missing constructs. The kit measures the Cypher compatibility forms
  that GQL accepts.
- **Playground** ([Web Playground](../users_guide/web_playground.md)): query tabs can be GQL
  (highlighting, completion, parameters, error spans, "Save as query..." from a `g.gql(..)` tab),
  the saved-query editor has a language switch and completes the declared `$parameters`, the new
  Examples panel has search, collapsible groups and GQL subgroups (including write examples),
  element details collapse long values, and the panel header wraps on narrow screens. The book's
  GQL run buttons open GQL tabs. The MCP tool `gql` and the AI Chat can write (read-only under
  `--mcp-read-only`; the AI Chat only when asked), and the session reports a change summary.

## Changed

- **`g.gql()` is no longer read-only**: a query with a write clause that used to be refused now
  runs. A host that relied on the refusal restricts it with an `Authorizer` / `AccessPolicy`
  (writes ask `Create`, `Update` and `Delete` on `Data` like their Gremlin twins) or `--mcp-read-only`.
- **GQL answers** that were wrong or too strict now follow the TCK (cross-kind equality, ordering
  of lists, `NaN`, `WITH ... WHERE` seeing the dropped names, and more); a query that relied on
  such an answer may return a different one.
- **Result JSON writes `NaN`, `Infinity` and `-Infinity` as GraphSON typed doubles**
  (`{"@type": "g:Double", "@value": "NaN"}`) in the playground, the dev server, MCP, `to_json()`
  and `--format json`; they used to fail with "non-finite float cannot be represented in JSON".
  The strict writers (GraphML, the plain serde of a value) stay strict.

## Fixed

- **`where(P.eq("x"))` compares by logical value** ([Predicates](../users_guide/predicates.md)):
  `where(P.eq(..))`, `where(P.neq(..))`, the end label of `where(__.as("x"))`,
  `where(P.eq(..)).by(..)` and `where(P.within(..))` compared the current object with the
  labelled one structurally, so a computed value (`inject`, `constant`, `math`) never equalled a
  lazily read property, two properties of different elements never matched, and `1` never
  equalled `1.0`. Scalars now compare by the rules `P.eq` applies to a literal, elements by
  identity, containers by value.
- **`merge_v()` and `merge_e()` ask `Read` on what they search** ([Permissions](../users_guide/permissions.md)):
  they asked only `Create` and `Update`, so a policy that denied reading could still probe for
  existence through them. A policy that allows writes but not reads now denies them.
- **A cross product is checked against the traverser limit before it is built**: `MATCH (a), (b)`
  (and a mid-traversal `v()`) on a large graph reserved the whole product up front and could
  abort the process. It now fails with `memory.allocation` and a `Help:` line when it exceeds
  `traversal.max_traversers` or the memory budget, and capacities taken from query data are
  capped.
