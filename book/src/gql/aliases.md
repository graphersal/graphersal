# Cypher Aliases

`gql()` speaks ISO GQL. Common Cypher spellings are accepted as **aliases** where they are
unambiguous and mean exactly the same: an alias is translated to its GQL form while the query is
parsed, so both spellings give the same plan and the same rows. An alias never has a meaning of
its own. This table lists every accepted alias; a Cypher spelling that is not in it is either
not supported yet or [refused](refused.md).

| Cypher spelling | GQL form | Notes |
|---|---|---|
| `SKIP n` | `OFFSET n` | |
| `a != b` | `a <> b` | |
| `s1 + s2` on two strings | `s1 \|\| s2` | also on two lists; `+` of a string and a number is an error (no implicit conversion) |
| `(n:A:B)` | `(n:A&B)` | plain names only; mixing `:A:B` with `\|`, `&` or `!` is an error (write it with `&`) |
| `n:A` in an expression | `n IS LABELED A` | |
| `-->` | `->` | abbreviated edge pointing right |
| `<--` | `<-` | abbreviated edge pointing left |
| `--` | `-` | abbreviated edge, either direction ([the `--` rule](rhai_strings.md#comments)) |
| `<-->` | `<->` | either direction |
| `elementId(x)` | `element_id(x)` | the id as a string |
| `toUpper(s)` | `upper(s)` | |
| `toLower(s)` | `lower(s)` | |
| `size(s)` on a string | `char_length(s)` | `size(list)` is the item count, as `cardinality(list)` |
| `collect(x)` | `collect_list(x)` | also `collect(DISTINCT x)` |
| `length(p)` | `path_length(p)` | the number of edges of a path |
| `-[e:T*]->` | `-[e:T]->{1,}` | Cypher's lower bound is 1: **not** GQL's postfix `*`, which is `{0,}` |
| `-[e:T*n]->` | `-[e:T]->{n}` | |
| `-[e:T*m..n]->` | `-[e:T]->{m,n}` | |
| `-[e:T*..n]->` | `-[e:T]->{1,n}` | lower bound 1 again |
| `-[e:T*m..]->` | `-[e:T]->{m,}` | |

Each pair below returns the same rows:

```rhai,graph=modern
g.gql("MATCH (n:person) RETURN n.name AS name ORDER BY name SKIP 2").to_list()   // #{"name": "peter"}, #{"name": "vadas"}
g.gql("MATCH (n:person) RETURN n.name AS name ORDER BY name OFFSET 2").to_list()   // #{"name": "peter"}, #{"name": "vadas"}
g.gql("MATCH (n:person) WHERE n.age != 29 RETURN n.name AS name ORDER BY name").to_list()   // josh, peter, vadas
g.gql("MATCH (n:person) WHERE n.age <> 29 RETURN n.name AS name ORDER BY name").to_list()   // josh, peter, vadas
g.gql("MATCH (p {name: 'marko'})-->(x) RETURN x.name AS x ORDER BY x").to_list()   // josh, lop, vadas
g.gql("MATCH (p {name: 'marko'})->(x) RETURN x.name AS x ORDER BY x").to_list()   // josh, lop, vadas
g.gql("MATCH (n) WHERE n:software RETURN n.name AS name ORDER BY name").to_list()   // lop, ripple
g.gql("MATCH (n) WHERE n IS LABELED software RETURN n.name AS name ORDER BY name").to_list()   // lop, ripple
g.gql("MATCH (n:person) RETURN elementId(n) AS id, toUpper(n.name) AS up, size(n.name) AS len, n.name + '?' AS q ORDER BY id LIMIT 1").to_list()
// #{"id": "1", "len": 5, "q": "marko?", "up": "MARKO"}
g.gql("MATCH (n:person) RETURN element_id(n) AS id, upper(n.name) AS up, char_length(n.name) AS len, n.name || '?' AS q ORDER BY id LIMIT 1").to_list()
// #{"id": "1", "len": 5, "q": "marko?", "up": "MARKO"}
g.gql("MATCH (p:person)-[:created]->(s) RETURN p.name AS name, collect(s.name) AS sw ORDER BY name").to_list()
// #{"name": "josh", "sw": ["ripple", "lop"]}, #{"name": "marko", "sw": ["lop"]}, #{"name": "peter", "sw": ["lop"]}
g.gql("MATCH (p:person)-[:created]->(s) RETURN p.name AS name, collect_list(s.name) AS sw ORDER BY name").to_list()
// #{"name": "josh", "sw": ["ripple", "lop"]}, #{"name": "marko", "sw": ["lop"]}, #{"name": "peter", "sw": ["lop"]}
g.gql("MATCH p = (a {name: 'marko'})-[*1..2]->(b) RETURN b.name AS b, length(p) AS len ORDER BY len, b").to_list()
// josh 1, lop 1, vadas 1, lop 2, ripple 2
g.gql("MATCH p = (a {name: 'marko'})-[]->{1,2}(b) RETURN b.name AS b, path_length(p) AS len ORDER BY len, b").to_list()
// josh 1, lop 1, vadas 1, lop 2, ripple 2
```

Mind the Cypher `*`: `-[*]->` starts at one edge, GQL's `-[]->*` at zero, so the GQL form
also returns marko himself:

```rhai,graph=modern
g.gql("MATCH (a {name: 'marko'})-[*]->(b) RETURN b.name AS b ORDER BY b").to_list()   // josh, lop, lop, ripple, vadas
g.gql("MATCH (a {name: 'marko'})-[]->*(b) RETURN b.name AS b ORDER BY b").to_list()   // josh, lop, lop, marko, ripple, vadas
```

## The Cypher clause `WITH`

`WITH` is Cypher; GQL writes the same with its linear statements (`LET`, `FILTER`, and
`RETURN ... NEXT`, which `gql()` does not support yet). `gql()` accepts `WITH` with Cypher's
meaning: it passes on only its items, may take `DISTINCT`, aggregates, `ORDER BY`, `SKIP` /
`OFFSET`, `LIMIT` and a `WHERE` (applied last), and a later clause continues from its columns
([The Supported Subset](subset.md#with)). A `WITH` that only passes values on is the same as
`LET` plus a narrower scope, and its `WHERE` the same as `FILTER`:

```rhai,graph=modern
g.gql("MATCH (p:person) WITH p, p.age / 10 AS d WHERE d = 3 RETURN p.name AS name ORDER BY name").to_list()   // #{"name": "josh"}, #{"name": "peter"}
g.gql("MATCH (p:person) LET d = p.age / 10 FILTER d = 3 RETURN p.name AS name ORDER BY name").to_list()   // #{"name": "josh"}, #{"name": "peter"}
```

## Spellings that are GQL as well

These look like Cypher but are GQL too, so they are not aliases: `:` before a label expression
(GQL also has `IS`), `ASC` / `DESC` (also `ASCENDING` / `DESCENDING`), string literals in
`'...'` and `"..."`, `//` and `/* */` comments, `$name` parameters, back-quoted names.

## Cypher spellings that are not aliases

| Cypher | In `gql()` |
|---|---|
| `id(n)` | refused: an integer in Neo4j, Graphersal ids are strings; use `element_id(n)` |
| `nodes(p)`, `relationships(p)` | not supported yet: GQL has no such functions; return `p`, or name the elements |
| `UNWIND` | not supported yet (GQL's `FOR`) |
| `GROUP BY` (not Cypher, but often written) | an error that explains it: grouping is implicit |
| `CREATE`, `MERGE`, `SET`, `DELETE`, `REMOVE` | refused: `gql()` is read-only |
| `CALL db.*()`, `apoc.*`, `SHOW ...` | refused: vendor extensions |
| `a--1` (minus minus one) | a comment starts at `--` (GQL wins): `RETURN n.age--1 AS x` returns the column `n.age`; write `n.age - -1` |
