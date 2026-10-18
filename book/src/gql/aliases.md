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
```

## Spellings that are GQL as well

These look like Cypher but are GQL too, so they are not aliases: `:` before a label expression
(GQL also has `IS`), `ASC` / `DESC` (also `ASCENDING` / `DESCENDING`), string literals in
`'...'` and `"..."`, `//` and `/* */` comments, `$name` parameters, back-quoted names.

## Cypher spellings that are not aliases

| Cypher | In `gql()` |
|---|---|
| `id(n)` | refused: an integer in Neo4j, Graphersal ids are strings; use `element_id(n)` |
| `collect(x)`, `count(*)` and the other aggregates | not supported yet (aggregation comes in a later version; `collect` will be the alias of `collect_list`) |
| `-[:T*1..3]->` | not supported yet (quantified edges; will be the alias of `-[:T]->{1,3}`) |
| `WITH`, `UNWIND`, `OPTIONAL MATCH` | not supported yet |
| `CREATE`, `MERGE`, `SET`, `DELETE`, `REMOVE` | refused: `gql()` is read-only |
| `CALL db.*()`, `apoc.*`, `SHOW ...` | refused: vendor extensions |
| `a--1` (minus minus one) | a comment starts at `--` (GQL wins): `RETURN n.age--1 AS x` returns the column `n.age`; write `n.age - -1` |
