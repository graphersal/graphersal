# Result Shape

## One map per row

Every result row of `gql()` is a **map** from column name to value, whatever the plan behind it
(the planner may start a pattern at any of its nodes; the row looks the same). `RETURN` is
mandatory, as in GQL and Cypher: a query without it is a syntax error whose help suggests
`RETURN *` or `RETURN a, b`.

```rhai,graph=modern
g.gql("MATCH (p:person) RETURN p.name AS name, p.age AS age ORDER BY age").to_list()
// #{"age": 27, "name": "vadas"}, #{"age": 29, "name": "marko"}, #{"age": 32, "name": "josh"}, #{"age": 35, "name": "peter"}
g.gql("MATCH (n:person)")   // error: missing RETURN
```

A value is a property value (number, string, boolean, date, UUID, vector, ...), `NULL` (a
missing property gives a `NULL` column, the key is never left out), a list, a map, or a vertex
or edge. An element column holds the live element, so Gremlin can continue from it:

```rhai,graph=modern
g.gql("MATCH (a)-[e]->(b) RETURN * ORDER BY element_id(e) LIMIT 2").to_list()
// #{"a": v[1], "b": v[2], "e": e[0][1-knows->2]}, #{"a": v[1], "b": v[3], "e": e[1][1-created->3]}
g.gql("MATCH (s:software) RETURN s ORDER BY s.name").select("s").in("created").values("name").order().to_list()   // "josh", "josh", "marko", "peter"
```

## Column names

| Item | Column name |
|---|---|
| `expr AS name` | `name` |
| `n.name` without `AS` | the expression text as written: `n.name` |
| `RETURN *` | every named variable, in the order of its first appearance in the query |
| `RETURN *, expr AS x` | the variables first, then `x` |

A name used twice is an error. Give every computed column an `AS` name: it is easier to read
and to reach from Gremlin (`select("name")`), and the text form of an unaliased column follows
Neo4j's rule today (the expression as written), which may still be aligned with the ISO naming
rule.

```rhai,graph=modern
g.gql("MATCH (p:person) RETURN p.name ORDER BY p.name LIMIT 2").to_list()   // #{"p.name": "josh"}, #{"p.name": "marko"}
g.gql("MATCH (s:software) RETURN s.name AS `the name` ORDER BY `the name`").to_list()   // #{"the name": "lop"}, #{"the name": "ripple"}
g.gql("MATCH (n:person) RETURN n.age AS a, n.name AS a")   // error: the column name `a` is used twice in RETURN
```

A keyword (`by`, `order`, `match`, ...) is fine as a label, a property key or an `AS` name, but
where it is read back as a variable (in `ORDER BY`, or as a pattern variable) it must be
back-quoted: ``RETURN p.name AS by ORDER BY `by` ``.

## Column order

The engine keeps the columns in the **written order** (`RETURN p.name AS name, p.age AS age`
gives `name`, then `age`). Whether you see that order depends on where the rows go:

| Output | Column order |
|---|---|
| JSON: `to_json()`, `--format json`, the dev server's JSON API, the MCP tools `gql` and `query` | written order |
| Rust: the row is an `ElementProperty::Map` | written order |
| Rhai maps (`to_list()`, `next()`), the REPL / CLI table view | sorted by key |
| Python dicts | sorted by key |

Rhai object maps and Python dicts built from them are sorted by key, the same as for Gremlin's
`project()` today; the table view shows their columns in that order.

```rhai,graph=modern
g.gql("MATCH (p:person) RETURN p.name AS name, p.age AS age ORDER BY age LIMIT 2").to_json()
// [{"name": "vadas", "age": 27}, {"name": "marko", "age": 29}]
g.gql("MATCH (p:person) RETURN p.name AS name, p.age AS age ORDER BY age").next().keys()   // "age", "name"
```

When the written order matters, read the rows as JSON or name the columns so that their
alphabetical order is the order you want.

## Row order and duplicates

- Without `ORDER BY` the row order is unspecified ([Ordering](semantics.md#ordering)).
- Rows are a multiset: the same row may appear several times (one per match). `RETURN DISTINCT`
  removes duplicates.
- `OFFSET` and `LIMIT` apply after `ORDER BY` and `DISTINCT`.
