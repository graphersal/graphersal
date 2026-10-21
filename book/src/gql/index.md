# GQL Queries

> **Experimental.** GQL support is new in Graphersal 0.2.2 and marked experimental: its
> [subset](subset.md) grows from version to version, and the answers of a construct
> (its rows, errors and plans) may still change in a later version. Gremlin stays the complete,
> stable interface: a `gql()` query runs as ordinary Gremlin steps, and this chapter shows the
> Gremlin twin of each construct.

`g.gql(query)` runs a query written in **GQL**, the ISO standard graph query language
(ISO/IEC 39075), on the same graph and the same engine as Gremlin. A GQL query describes the
*pattern* you are looking for; Graphersal finds every match and returns one row per match:

```rhai,graph=modern
g.gql("MATCH (p:person)-[:created]->(s:software) WHERE p.age > 30 RETURN p.name AS who, s.name AS what ORDER BY who, what").to_list()
// #{"what": "lop", "who": "josh"}, #{"what": "ripple", "who": "josh"}, #{"what": "lop", "who": "peter"}
```

The Gremlin equivalent walks the graph step by step:

```rhai,graph=modern
g.v().has_label("person").has("age", P.gt(30)).as("p").out("created").has_label("software").as("s").select("p", "s").by("name").to_list()
// #{"p": "josh", "s": "ripple"}, #{"p": "josh", "s": "lop"}, #{"p": "peter", "s": "lop"}
```

Every example in this chapter was run on the sample graph `modern`; the comment after `//` is
its real output (a list is shown as its items, comma-separated, or one per line).

## What `gql()` is

- **ISO GQL is the norm.** Queries are written in GQL; where GQL and Cypher differ, GQL wins.
  Familiar **Cypher spellings are accepted as aliases** where they are unambiguous and mean the
  same (`SKIP` for `OFFSET`, `!=` for `<>`, `toUpper()` for `upper()`, `-->` for `->`, ...);
  [Cypher Aliases](aliases.md) lists every one. There is no Graphersal-only syntax: an accepted
  query means the same in a GQL or Cypher 25 database (modulo the listed aliases).
- **Reads and writes.** `INSERT` (Cypher `CREATE`) creates nodes and edges, `SET` and `REMOVE`
  change their properties and labels, and `DELETE` / `DETACH DELETE` remove them, each once per
  row ([Writing with GQL](writes.md)); `MERGE` is not supported yet: use the [Gremlin mutation
  steps](../dsl/changing_the_graph.md) for it. A query is one unit: a query that fails changes
  nothing.
- **A start step.** `gql()` starts a traversal: `g.gql(..)`, never `g.v().gql(..)` or
  `__.gql(..)`.
- **Rows are maps.** Each result row is a map from column name to value
  ([Result Shape](result_shape.md)). `RETURN` is mandatory unless the query ends in a write.
- **One engine.** The query is compiled into an ordinary traversal plan: the same optimizer,
  property indexes, `profile()`, limits, cancellation, permissions and storages as Gremlin
  ([Plans and Performance](plans.md)). A query that does not parse fails before anything runs,
  with a caret under the offending text ([Errors](errors.md)).

This version supports `INSERT` and GQL read queries: `MATCH` and `OPTIONAL MATCH`, `WHERE`, `LET`, `FILTER`,
Cypher's `WITH`, `RETURN` with aggregation (`count`, `sum`, `avg`, `min`, `max`, `collect_list`),
`ORDER BY`, `OFFSET`, `LIMIT`, `FOR` (Cypher `UNWIND`) with a direct lookup per item where an
id or index serves it, queries that start without `MATCH` (`RETURN 1 + 2 AS x`), `EXISTS`
subqueries and Cypher pattern predicates (`WHERE NOT (p)-[:created]->()`), Cypher map projections
and list and pattern comprehensions for nested, JSON-shaped answers
(`p {.name, friends: [(p)-[:knows]->(f) | f.name]}`), Cypher's `COUNT { }` / `COLLECT { }` and
list predicates (`all()`, `any()`, `reduce()`, ...), composite queries (`UNION`, `UNION ALL`,
`NEXT`), `CALL { }` subqueries run per row (top k per group), node and edge
patterns, quantified edges (`-[:knows]->{1,3}`), quantified path patterns that repeat a whole
chain (`(a) ((x)-[:created]->(s)<-[:created]-(y)){1,2} (b)`), path variables, label expressions,
expressions, parameters, the GQL scalar functions and the common openCypher ones (`toInteger`,
`substring`, `split`, `round`, `keys`, `nodes(p)`, ...) ([The Supported Subset](subset.md)).
`EXCEPT` / `INTERSECT`, shortest paths and a quantifier nested inside a quantified path pattern
come later; until then the error names the form or the Gremlin step that does the same.

```rhai,graph=modern
g.gql("MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s) RETURN p.name AS name, count(s) AS made ORDER BY made DESC, name").to_list()
// josh 2, marko 1, peter 1, vadas 0
g.gql("MATCH (a {name: 'marko'})-[:knows]->{1,2}(b)-[:created]->(s) RETURN DISTINCT s.name AS s ORDER BY s").to_list()   // #{"s": "lop"}, #{"s": "ripple"}
g.gql("MATCH (p:person) CALL (p) { MATCH (p)-[e:created]->(s) RETURN s.name AS top ORDER BY e.weight DESC LIMIT 1 } RETURN p.name AS who, top ORDER BY who").to_list()
// josh ripple, marko lop, peter lop: each person's heaviest software
```

## Mixing GQL and Gremlin

Gremlin steps may follow `gql()`. They see the row maps and reach a column by its name, so
whatever GQL cannot do yet, a Gremlin step after it can:

```rhai,graph=modern
g.gql("MATCH (p:person) RETURN p ORDER BY p.name").select("p").out("created").values("name").order().to_list()   // "lop", "lop", "lop", "ripple"
g.gql("MATCH (a)-[:knows]->(b) RETURN *").select("a", "b").by("name").to_list()   // #{"a": "marko", "b": "josh"}, #{"a": "marko", "b": "vadas"}
g.gql("MATCH (p:person) RETURN p.name AS name, p.age AS age").select("age").sum().next()   // 123
g.gql("MATCH (p:person)-[:created]->(s) RETURN p.name AS name").select("name").dedup().order().to_list()   // "josh", "marko", "peter"
```

An element column (`RETURN p`) holds the live vertex or edge, so `select("p").out(..)` continues
the walk from it.

## Where you can use it

| Entry point | How |
|---|---|
| Rust | `GraphTraversalSource::gql(query)` and `gql_with(query, params)`, feature `gql` |
| Gremlin DSL (Rhai) | `g.gql(query)` and `g.gql(query, #{name: value})`; see [Writing GQL in Rhai Strings](rhai_strings.md) |
| Command line | `graphersal -e 'g.gql("MATCH (p:person) RETURN p.name AS name")'`, or in the REPL |
| Python | through a script: `graph.query('g.gql("...", #{...})', params={...})` |
| Playground | a **GQL query** tab (the **+** menu, or the **Rhai / GQL** switch): plain GQL with completion and a Parameters field; or `g.gql("...")` in a Rhai tab; the examples list has a GQL group ([Web Playground](../users_guide/web_playground.md#what-it-does)) |
| Saved queries | `g.define_query(#{name: "older", language: "gql", params: #{min: "integer"}, body: "MATCH (p:person) WHERE p.age > $min RETURN p.name AS name"})`, then `g.query("older", #{min: 30})` from any front end ([Saved queries in GQL](../users_guide/saved_queries.md#saved-queries-in-gql)) |
| MCP (dev server) | the tool `gql` {`query`, `params`, `profile`, `max_rows`}: the query as plain text, no Rhai string around it; it writes like the `query` tool, and a server started with `--mcp-read-only` refuses its writes |

In Rust:

```rust,ignore
use graphersal::prelude::*;

let graph = TraversalGraph::tinkerpop_modern();
let ages = graph
    .traversal()
    .gql_with(
        "MATCH (p:person {name: $name}) RETURN p.age AS age",
        [("name", ElementProperty::from("marko"))],
    )
    .select("age")
    .to_list()
    .unwrap();
assert_eq!(ages, [ElementProperty::from(29)]);
```

A writing query runs on the writing source, `traversal_mut()` ([In Rust](writes.md#in-rust)).

In the playground, a GQL tab takes the query as it is, without a Rhai string around it, and its
parameters as a JSON map in the **Parameters** field under the editor:

```text
MATCH (p:person) WHERE p.age > $min
RETURN p.name AS name ORDER BY name

Parameters: {"min": 30}
```

The tab runs it as `g.gql("MATCH (p:person) WHERE p.age > $min\nRETURN p.name AS name ORDER BY name", #{min: 30})`:
a JSON integer stays an integer, `30.0` is a float, `null` is `NULL`, lists and maps stay lists and
maps. An error is underlined where the engine reports it, with its GQLSTATUS in the message. To
continue a GQL result with Gremlin steps (`.select("p").out()`), switch the tab to Rhai: the query
and its parameters become that `g.gql(..)` call.

The feature `gql` is off by default in the `graphersal` crate ([Library Features](../users_guide/installation.md));
the command line, the playground, the dev server and the Python package have it on.

From Python, values travel as script variables and from there into the GQL parameter map:

```python
graph.query('g.gql("MATCH (p:person) WHERE p.age > $min RETURN p.name AS name, p.age AS age ORDER BY age", #{min: min_age})',
            params={"min_age": 30})
# [{'age': 32, 'name': 'josh'}, {'age': 35, 'name': 'peter'}]
```

## In this chapter

- [The Supported Subset](subset.md): clauses, patterns, label expressions, expressions,
  functions, parameters, and what is not supported yet.
- [Semantics](semantics.md): trail matching, quantified edges and path patterns and their
  limits, `OPTIONAL MATCH`,
  `NULL` and three-valued logic, subqueries (`EXISTS`, `COUNT`, `COLLECT`, `CALL`), `UNION` and
  `NEXT`, aggregation, `DISTINCT` equality, comparisons, ordering, the
  rules of the functions (strict `CAST`, lenient `toInteger`, rounding, where they differ from
  Neo4j), ids.
- [Result Shape](result_shape.md): row maps, column names and their order, path values.
- [Writing with GQL](writes.md): `INSERT` (Cypher `CREATE`), what a row sees, units, schema,
  permissions, and the write clauses that come later.
- [Writing GQL in Rhai Strings](rhai_strings.md): quotes, multi-line queries, comments,
  parameters, and when the MCP tool is easier.
- [Cypher Aliases](aliases.md): every accepted Cypher spelling and its GQL form.
- [Refused Constructs](refused.md): what `gql()` will not do, and why.
- [Errors](errors.md): GQLSTATUS codes and diagnostics.
- [Plans and Performance](plans.md): what a query becomes in `profile()`.
- [GQL Conformance](conformance.md): the GQL features supported, partially supported and not
  supported.
- The generated step page [gql](../traversal/steps/gql.md).
