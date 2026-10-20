# Release 0.2.2

A patch release of the 0.2 line. It brings GQL read queries, marked **experimental**, and fixes
how value filters treat a string that names a step label. It changes no format: stores,
snapshots and journals of 0.2.1 are read and written exactly as before. Its API additions are
additive (a new feature, new methods, a new `#[non_exhaustive]` error variant).

## Added

- **GQL read queries (experimental)** ([GQL Queries](../gql/index.md)): `g.gql(query)` runs a
  query in GQL, the ISO graph query language (ISO/IEC 39075), on the same graph and engine as
  Gremlin. The query is compiled into an ordinary traversal plan, so the optimizer, property
  indexes, `profile()`, resource limits, cancellation, permissions and custom storages apply as
  they do to Gremlin; each result row is a map from column name to value
  ([Result Shape](../gql/result_shape.md)). **Experimental** means: `gql()` only reads (every write
  clause is refused), the supported subset grows from version to version, and the rows, errors
  and plans of a construct may still change in a later version. Gremlin stays the complete,
  stable interface.
  - **Supported** ([The Supported Subset](../gql/subset.md)): `MATCH` and `OPTIONAL MATCH`,
    `WHERE`, `LET`, `FILTER`, Cypher's `WITH`, `RETURN` with aggregation, `GROUP BY`, `DISTINCT`,
    `ORDER BY`, `OFFSET`/`LIMIT`; `FOR` (Cypher `UNWIND`) with `WITH ORDINALITY`/`OFFSET`;
    queries that start without `MATCH`; node and edge patterns, label expressions, quantified
    edges (`-[:knows]->{1,3}`) and quantified path patterns
    (`(a) ((x)-[:created]->(s)<-[:created]-(y)){1,2} (b)`), path variables (trail semantics: no edge bound twice in a match);
    `EXISTS` subqueries and Cypher pattern predicates; map projections, list and pattern
    comprehensions and `RECORD {..}` for JSON-shaped answers; Cypher's `COUNT { }`,
    `COLLECT { }`, `all()`/`any()`/`none()`/`single()` and `reduce()`; composite queries
    (`UNION`, `UNION ALL`, `NEXT`); `CALL { }` subqueries run per row (`OPTIONAL CALL`);
    parameters (`$name`); three-valued logic; the GQL scalar functions (strings, numbers,
    `CAST`) and the common openCypher ones (`toInteger`, `substring`, `split`, `round`, `keys`,
    `nodes(p)`, ...).
  - **ISO first, Cypher spellings accepted** ([Cypher Aliases](../gql/aliases.md)): where GQL and
    Cypher differ, GQL wins; unambiguous Cypher spellings with the same meaning are accepted as
    aliases (`SKIP`, `!=`, `toUpper()`, `-->`, ...). There is no Graphersal-only syntax.
  - **Errors** ([Errors](../gql/errors.md)): a query that does not parse or bind fails before
    anything runs, with a GQLSTATUS code, a caret under the offending text and a `Help:` line;
    constructs outside the subset are refused by name with an alternative
    ([Refused Constructs](../gql/refused.md)). Size limits (pattern edges, subqueries, nesting)
    are errors, never a cut result.
  - **Plans** ([Plans and Performance](../gql/plans.md)): a GQL query and its Gremlin twin get the
    same plan where they mean the same; `profile()` shows the internal `gql:*` steps; an equality
    on an indexed property and an id lookup per row are served directly.
  - **Everywhere:** Rust `GraphTraversalSource::gql` and `gql_with(query, params)` (feature `gql`,
    off by default, no dependencies, builds for wasm32), the Rhai DSL `g.gql(query)` /
    `g.gql_with(query, params)`, the CLI and REPL, Python (`graph.query("g.gql(...)")`), the
    playground (GQL examples), and the MCP server's tool `gql`. New error
    `TraverserError::Gql` with `GqlErrorKind`.
  - **Not yet:** writes (`INSERT`, `SET`, `DELETE`, ...), `EXCEPT`/`INTERSECT`, path modes and
    shortest paths, a quantifier nested in a quantified path pattern
    ([Conformance](../gql/conformance.md), [Current Limitations](../users_guide/current_limitations.md)).

## Changed

- **A value filter compares a string as a value, never as a step label**
  ([Predicates](../users_guide/predicates.md)): `has(key, P.eq("a"))`, `is("a")`, `has_value`,
  their `P` forms, `any`/`all`/`none` and a jpath `has` compared a string that named a step label
  (`as("a")`) with the labelled element instead of the string itself, so
  `g.v(1).as("vadas").out("knows").has("name", P.eq("vadas"))` returned nothing. They now compare
  the literal, as TinkerPop does; only `where(P)` and `where(P).by(..)` read step labels. A
  traversal that relied on the old behaviour should use `where(P.eq("a"))`. As a side effect, a
  property index now also serves a string `P.eq` inside child traversals and `union()` operands.
- **The profile table's `Step` column is as wide as the longest step text**
  ([Profiling](../users_guide/profiling_and_execute.md)): at least 60 characters (70 with nested
  steps), at most 160, then `...`. It was cut at 60/70, which hid annotations such as
  `[index: ..]`. The playground's Text view uses the same rule.
