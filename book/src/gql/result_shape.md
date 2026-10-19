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
missing property gives a `NULL` column, the key is never left out), a list, a map (both may
[nest](#nested-maps-and-lists)), a vertex or edge, or a [path](#paths). An element column holds the live element, so Gremlin can continue from it:

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
| `count(*)` without `AS` | the same rule: `count(*)` |
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

## Paths

A path variable (`RETURN p`) and a group variable (the variable of a quantified edge) hold
elements, so Gremlin can continue from them too:

| Column | Value | JSON |
|---|---|---|
| a path variable `p` | a path: vertex, edge, vertex, ..., in the written order (Gremlin's `path()` value) | a list of the element objects |
| a group variable `e` of `-[e]->{1,3}` | a list of the edges, in the written order; `[]` for a zero-length match | a list of edge objects |
| either, from an `OPTIONAL MATCH` that did not match | `NULL` | `null` |

```rhai,graph=modern
g.gql("MATCH p = (a {name: 'marko'})-[:knows]->(b {name: 'vadas'}) RETURN p").to_list()   // #{"p": [v[1], e[0][1-knows->2], v[2]]}
g.gql("MATCH p = (a {name: 'marko'})-[:knows]->(b {name: 'vadas'}) RETURN p").to_json()
// [{"p": [{"id": "1", "label": "person", ...}, {"id": "0", "label": "knows", "inV": "2", "outV": "1", ...}, {"id": "2", ...}]}]
g.gql("MATCH p = (a {name: 'marko'})-[:knows]->(b {name: 'vadas'}) RETURN p").select("p").unfold().to_list()   // v[1], e[0][1-knows->2], v[2]
g.v().has("name", "marko").out_e("knows").in_v().has("name", "vadas").path().to_list()   // [v[1], e[0][1-knows->2], v[2]]
```

## Nested maps and lists

A column may hold maps and lists nested to any depth: a record constructor `{k: expr}`, a
[map projection](subset.md#map-projections-and-comprehensions) `p {.name, ...}` and a list or
pattern comprehension build them per row, so one row can carry an entity with its related data
inside. Keys keep their written order in the JSON output, at every level, like the columns of a
row ([Column order](#column-order)); Rhai maps sort them by key as anywhere else.

```rhai,graph=modern
g.gql("MATCH (s:software) RETURN s {.name, creators: [(s)<-[:created]-(p) | p {.name, .age}]} AS sw ORDER BY sw.name").to_json()
// [{"sw": {"name": "lop", "creators": [{"name": "peter", "age": 35}, {"name": "josh", "age": 32}, {"name": "marko", "age": 29}]}},
//  {"sw": {"name": "ripple", "creators": [{"name": "josh", "age": 32}]}}]
// (the creators of one program come in no defined order)
```

A vertex, edge or path inside a map or a list stays the live element, as in a column of its own:
Gremlin can continue from it, and JSON renders it as the element object `{id, label, properties}`
(an edge also its endpoints, `inV` and `outV`). Project the properties you need (`f {.name}`
instead of `f`) for a smaller answer.

```rhai,graph=modern
g.gql("MATCH (p:person {name: 'marko'})-[:knows]->(f {name: 'vadas'}) RETURN p {.name, friend: f} AS x").to_list()   // #{"x": #{"friend": v[2], "name": "marko"}}
g.gql("MATCH (p:person {name: 'marko'})-[:knows]->(f {name: 'vadas'}) RETURN p {.name, friend: f} AS x").to_json()
// [{"x": {"name": "marko", "friend": {"id": "2", "label": "person", "properties": {"name": "vadas", "age": 27}}}}]
g.gql("MATCH (p:person {name: 'marko'}) RETURN [(p)-[:knows]->(f) | f] AS friends").select("friends").unfold().values("name").order().to_list()   // "josh", "vadas"
```

| Value | Rhai / Rust | JSON |
|---|---|---|
| a map projection or record | a map (`ElementProperty::Map`), keys in written order in Rust | an object, keys in written order |
| a map projection of a `NULL` variable | `()` / `NULL` | `null` |
| a list or pattern comprehension | a list; `[]` when nothing is kept or matched | an array |
| an element inside either | the live vertex or edge | the element object |

## Row order and duplicates

- Without `ORDER BY` the row order is unspecified ([Ordering](semantics.md#ordering)).
- Rows are a multiset: the same row may appear several times (one per match). `RETURN DISTINCT`
  removes duplicates ([with GQL's equality](semantics.md#distinct-and-grouping-equality)).
- An aggregating `RETURN` gives one row per group; without grouping keys exactly one row, even
  when nothing matched ([Aggregation](semantics.md#aggregation)).
- `OFFSET` and `LIMIT` apply after `ORDER BY` and `DISTINCT`.
