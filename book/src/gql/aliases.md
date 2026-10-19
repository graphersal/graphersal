# Cypher Aliases

`gql()` speaks ISO GQL. Common Cypher spellings are accepted as **aliases** where they are
unambiguous and mean exactly the same: an alias is translated to its GQL form while the query is
parsed, so both spellings give the same plan and the same rows. An alias never has a meaning of
its own. This table lists every accepted alias; Cypher functions without any GQL counterpart are
the [compatibility functions](#compatibility-functions) below; any other Cypher spelling is either
not supported yet or [refused](refused.md).

| Cypher spelling | GQL form | Notes |
|---|---|---|
| `SKIP n` | `OFFSET n` | |
| `a != b` | `a <> b` | |
| `s1 + s2` on two strings | `s1 \|\| s2` | also on two lists; `+` of a string and a number is an error (no implicit conversion: write `s + toString(n)` or `s \|\| CAST(n AS STRING)`) |
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
| `log(x)` (one argument) | `ln(x)` | the natural logarithm; the two-argument `log(base, x)` is GQL's general logarithm |
| `-[e:T*]->` | `-[e:T]->{1,}` | Cypher's lower bound is 1: **not** GQL's postfix `*`, which is `{0,}` |
| `-[e:T*n]->` | `-[e:T]->{n}` | |
| `-[e:T*m..n]->` | `-[e:T]->{m,n}` | |
| `-[e:T*..n]->` | `-[e:T]->{1,n}` | lower bound 1 again |
| `-[e:T*m..]->` | `-[e:T]->{m,}` | |
| `WHERE (a)-[:T]->(b)`, a pattern predicate | `WHERE EXISTS { (a)-[:T]->(b) }` | anywhere an expression stands; it may name only variables bound before it ([EXISTS](subset.md#exists-and-pattern-predicates)) |
| `UNWIND list AS x` | `FOR x IN list` | one row per item; `WITH ORDINALITY` / `WITH OFFSET` only after `FOR` |

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
g.gql("MATCH (n:person {name: 'marko'}) RETURN log(100) AS l").to_list()   // #{"l": 4.605170185988092}
g.gql("MATCH (n:person {name: 'marko'}) RETURN ln(100) AS l").to_list()   // #{"l": 4.605170185988092}
g.gql("MATCH (p:person) WHERE (p)-[:created]->() RETURN p.name AS name ORDER BY name").to_list()   // josh, marko, peter
g.gql("MATCH (p:person) WHERE EXISTS { (p)-[:created]->() } RETURN p.name AS name ORDER BY name").to_list()   // josh, marko, peter
g.gql("MATCH (p:person {name: 'marko'}) UNWIND [p.age, p.age + 1] AS a RETURN a ORDER BY a").to_list()   // #{"a": 29}, #{"a": 30}
g.gql("MATCH (p:person {name: 'marko'}) FOR a IN [p.age, p.age + 1] RETURN a ORDER BY a").to_list()   // #{"a": 29}, #{"a": 30}
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

The functions `abs`, `floor`, `ceil`, `sqrt`, `exp`, `log10`, `left`, `right`, `ltrim`, `rtrim`,
`btrim`, `trim(s)`, `nullif` (Cypher `nullIf`; names are case-insensitive) and `coalesce` have the
same name in both languages and are GQL functions. Where Neo4j's version differs, GQL's meaning
wins: `trim(s)`, `ltrim(s)` and `rtrim(s)` remove **spaces only** (Cypher removes any
whitespace), and `floor(3)` / `ceil(3)` of an integer stay integers (Neo4j returns floats that
compare equal). `ceiling(x)` is GQL's second spelling of `ceil(x)`.

## Compatibility functions

Some Cypher functions have no GQL counterpart at all, so they cannot be aliases. `gql()` accepts
a fixed list of them, because Cypher users and LLMs write them all the time, with their
openCypher / Neo4j Cypher 25 meaning: a query using them means the same as in Neo4j, except
where [Semantics](semantics.md#where-functions-differ-from-neo4j) says otherwise. They never get
a meaning of their own, and no vendor function joins them (`apoc.*`, `text.*` are
[refused](refused.md)).

| Function | Family | GQL nearest form |
|---|---|---|
| `toInteger(x)`, `toFloat(x)`, `toBoolean(x)`, `toString(x)` | conversions | `CAST(x AS INTEGER \| FLOAT \| BOOLEAN \| STRING)`, but **stricter** (below) |
| `toIntegerOrNull(x)`, `toFloatOrNull(x)`, `toBooleanOrNull(x)`, `toStringOrNull(x)` | conversions | none |
| `substring(s, start [, length])` | strings | `left(s, n)`, `right(s, n)` |
| `replace(s, search, replacement [, limit])`, `split(s, delimiter)`, `reverse(x)`, `isEmpty(x)` | strings, lists | none |
| `round(x [, precision [, mode]])`, `sign(x)` | numbers | none |
| `head(list)`, `last(list)`, `tail(list)`, `range(start, end [, step])` | lists | `trim(list, n)` drops items from the end |
| `keys(x)`, `properties(x)` | elements | `x.key` reads one property |
| `startNode(e)`, `endNode(e)` | elements | the predicates `n IS SOURCE OF e`, `n IS DESTINATION OF e` |
| `nodes(p)`, `relationships(p)` | paths | GQL's `elements(p)` (not supported yet) |

**`CAST` is not an alias of `toInteger()`.** Both convert with the same rules, but they treat a
text that does not convert differently: GQL's `CAST` fails the query with `22018`, Cypher's
`toInteger` / `toFloat` / `toBoolean` give `NULL`, and the `...OrNull` forms also give `NULL`
for a value of the wrong kind. `toInteger` and `toFloat` also read Cypher literals (`'0x1F'`,
`'1_000'`), `CAST` only decimal text. Use `CAST` to catch dirty data, `toInteger` to skip it
([Semantics](semantics.md#conversions-strict-cast-lenient-tox)).

```rhai,graph=modern
g.gql("MATCH (n:person {name: 'marko'}) RETURN toInteger('42') AS a, CAST('42' AS INTEGER) AS b").to_list()   // #{"a": 42, "b": 42}
g.gql("MATCH (n:person {name: 'marko'}) RETURN toInteger('forty') AS a").to_list()   // #{"a": ()}
g.gql("MATCH (n:person {name: 'marko'}) RETURN CAST('forty' AS INTEGER) AS b")   // error: invalid character value for cast (22018)
g.gql("MATCH (a)-[e:knows]-(b) WHERE startNode(e) = a RETURN a.name AS a, b.name AS b ORDER BY b").to_list()   // #{"a": "marko", "b": "josh"}, #{"a": "marko", "b": "vadas"}
g.gql("MATCH (a)-[e:knows]-(b) WHERE a IS SOURCE OF e RETURN a.name AS a, b.name AS b ORDER BY b").to_list()   // #{"a": "marko", "b": "josh"}, #{"a": "marko", "b": "vadas"}
```

## Cypher spellings that are not aliases

| Cypher | In `gql()` |
|---|---|
| `id(n)` | refused: an integer in Neo4j, Graphersal ids are strings; use `element_id(n)` |
| `nodes(p)`, `relationships(p)` | supported as [compatibility functions](#compatibility-functions), not aliases: GQL has no such functions |
| `x ^ y` | not supported yet; use GQL's `power(x, y)` |
| `rand()`, `randomUUID()` | refused: `gql()` functions are deterministic |
| list comprehensions, `any()` / `all()` / `none()` / `single()`, `reduce()` | not supported yet; `FOR x IN list FILTER ...` walks a list item by item |
| `COUNT { pattern }` | not supported yet; `OPTIONAL MATCH` and `count()` count the matches, `EXISTS` tests for one |
| `exists(n.p)` (Cypher 4) | [refused](refused.md): `EXISTS` takes a pattern; test a property with `n.p IS NOT NULL` |
| `GROUP BY` (not Cypher) | ISO GQL, accepted after the `RETURN` items (optional: grouping is implicit); after `WITH` an error that explains implicit grouping |
| `CREATE`, `MERGE`, `SET`, `DELETE`, `REMOVE` | refused: `gql()` is read-only |
| `CALL db.*()`, `apoc.*`, `SHOW ...` | refused: vendor extensions |
| `a--1` (minus minus one) | a comment starts at `--` (GQL wins): `RETURN n.age--1 AS x` returns the column `n.age`; write `n.age - -1` |
