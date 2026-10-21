# Cypher Aliases

`gql()` speaks ISO GQL. Common Cypher spellings are accepted as **aliases** where they are
unambiguous and mean exactly the same: an alias is translated to its GQL form while the query is
parsed, so both spellings give the same plan and the same rows. An alias never has a meaning of
its own. This table lists every accepted alias; Cypher functions without any GQL counterpart are
the [compatibility functions](#compatibility-functions) below, and Cypher's map projections and
comprehensions, `COUNT { }` and `COLLECT { }` the compatibility forms
([Map projections and comprehensions](#map-projections-and-comprehensions),
[COUNT and COLLECT](#count-and-collect)); any other Cypher
spelling is either not supported yet or [refused](refused.md).

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
| `CREATE <pattern>` | `INSERT <pattern>` | the same clause: once per row, nodes with label conjunctions (`:A:B` as `:A&B`), directed edges with one label ([Writing with GQL](writes.md)); `CREATE INDEX` / `CONSTRAINT` / `GRAPH` are [refused](refused.md) catalog commands |
| `CALL { WITH a, b ... }`, the importing `WITH` | `CALL (a, b) { ... }` | the first `WITH` of the body, when it lists plain variables only, is the variable scope clause ([CALL subqueries](subset.md#call-subqueries)) |
| `SET n:A:B`, `REMOVE n:A:B` | `SET n:A, n:B`, `REMOVE n:A, n:B` | the labels in order; `SET n IS A & B` likewise ([Writing with GQL](writes.md#set)) |
| `SET (n).k = v`, `REMOVE (n).k` | `SET n.k = v`, `REMOVE n.k` | a parenthesized variable as the target |

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
g.gql("MATCH (p:person) CALL { WITH p MATCH (p)-[:knows]->(f) RETURN count(f) AS n } RETURN p.name AS name, n ORDER BY name").to_list()   // josh 0, marko 2, peter 0, vadas 0
g.gql("MATCH (p:person) CALL (p) { MATCH (p)-[:knows]->(f) RETURN count(f) AS n } RETURN p.name AS name, n ORDER BY name").to_list()   // josh 0, marko 2, peter 0, vadas 0
g.gql("CREATE (c:city:capital {name: 'Rome'}) RETURN labels(c) AS l").to_list()   // #{"l": ["city", "capital"]}
g.gql("INSERT (c:city&capital {name: 'Rome'}) RETURN labels(c) AS l").to_list()   // #{"l": ["city", "capital"]}
g.gql("MATCH (p:person {name: 'marko'}) SET p:admin:staff RETURN labels(p) AS l").to_list()   // #{"l": ["person", "admin", "staff"]}
g.gql("MATCH (p:person {name: 'marko'}) SET p:admin, p:staff RETURN labels(p) AS l").to_list()   // #{"l": ["person", "admin", "staff"]}
```

The importing `WITH` passes the variables on unchanged; a first `WITH` that renames, computes or
filters (`WITH p AS q`, `WITH p WHERE ..`) is an ordinary `WITH` of a body that imports
nothing, so reading `p` there is an error (write `CALL (p) { .. }`). A `CALL { }`
without a scope clause and without that `WITH` imports nothing, as in Neo4j Cypher 25.

Mind the Cypher `*`: `-[*]->` starts at one edge, GQL's `-[]->*` at zero, so the GQL form
also returns marko himself:

```rhai,graph=modern
g.gql("MATCH (a {name: 'marko'})-[*]->(b) RETURN b.name AS b ORDER BY b").to_list()   // josh, lop, lop, ripple, vadas
g.gql("MATCH (a {name: 'marko'})-[]->*(b) RETURN b.name AS b ORDER BY b").to_list()   // josh, lop, lop, marko, ripple, vadas
```

## The Cypher clause `WITH`

`WITH` is Cypher; GQL writes the same with its linear statements (`LET`, `FILTER`, and
`RETURN ... NEXT`, [UNION and NEXT](subset.md#union-and-next)). `gql()` accepts `WITH` with Cypher's
meaning: it passes on only its items, may take `DISTINCT`, aggregates, `ORDER BY`, `SKIP` /
`OFFSET`, `LIMIT` and a `WHERE` (applied last), and a later clause continues from its columns
([The Supported Subset](subset.md#with)). A `WITH` that only passes values on is the same as
`LET` plus a narrower scope, and its `WHERE` the same as `FILTER`:

```rhai,graph=modern
g.gql("MATCH (p:person) WITH p, p.age / 10 AS d WHERE d = 3 RETURN p.name AS name ORDER BY name").to_list()   // #{"name": "josh"}, #{"name": "peter"}
g.gql("MATCH (p:person) LET d = p.age / 10 FILTER d = 3 RETURN p.name AS name ORDER BY name").to_list()   // #{"name": "josh"}, #{"name": "peter"}
g.gql("MATCH (p:person) RETURN p, p.age / 10 AS d NEXT FILTER d = 3 RETURN p.name AS name ORDER BY name").to_list()   // #{"name": "josh"}, #{"name": "peter"}
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
| `all(x IN l WHERE c)`, `any(..)`, `none(..)`, `single(..)` | list predicates | none; a list comprehension counted with `size()` (below) |
| `reduce(acc = init, x IN l \| expr)` | lists | none; `FOR x IN l` and an aggregate fold the rows of a query, not a list per row |

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

### List predicates and `reduce`

`all(x IN l WHERE c)`, `any(..)`, `none(..)` and `single(..)` test the items of a list, and
`reduce(acc = init, x IN l | expr)` folds them: `acc` starts at `init` and takes the value of
`expr` for each item in list order. `x` (and `acc`) exist only inside the call and must be new
names, like a [list comprehension's variable](#map-projections-and-comprehensions). The
`WHERE` is required; an aggregate may stand in the list and in `init`, not in the condition or
after `|`. Their `NULL` rules are the three-valued ones of the openCypher TCK, which Neo4j
Cypher 25 follows:

- a `NULL` list gives `NULL` (`reduce` too), a value that is not a list is error `22G03`;
- a decided answer wins and stops at once: `all` is FALSE at the first FALSE item, `any` TRUE
  and `none` FALSE at the first TRUE one, `single` FALSE at the second TRUE one (later items are
  not evaluated);
- otherwise an item whose condition is `NULL` makes the answer `NULL`;
- otherwise `all` and `none` are TRUE, `any` FALSE, `single` whether exactly one item was TRUE;
  so for an empty list `all` / `none` are TRUE, `any` / `single` FALSE, and `reduce` gives
  `init`.

A list comprehension counted with `size()` gives the same answer when no condition is `NULL`
(it drops a `NULL` condition instead): `all` is `size([x IN l WHERE c]) = size(l)`, `any` is
`> 0`, `none` is `= 0`, `single` is `= 1`. Over a pattern comprehension the predicates test the
matches of a pattern per row, the Gremlin `where(__..)` / `not(__..)`:

```rhai,graph=modern
g.gql("RETURN all(x IN [1, NULL] WHERE x > 0) AS a, any(x IN [1, NULL] WHERE x > 0) AS b, single(x IN [1, 2] WHERE x > 1) AS c, none(x IN [] WHERE x > 1) AS d").to_list()   // #{"a": (), "b": true, "c": true, "d": true}
g.gql("MATCH (p:person) WHERE any(a IN [(p)-[:knows]->(f) | f.age] WHERE a > 30) RETURN p.name AS name").to_list()   // #{"name": "marko"}
g.V().hasLabel("person").where(__.out("knows").has("age", P.gt(30))).values("name").to_list()   // "marko"
g.gql("MATCH (p:person) WHERE none(w IN [(p)-[:created]->(s) | s] WHERE w.name = 'lop') RETURN p.name AS name").to_list()   // #{"name": "vadas"}
g.V().hasLabel("person").not(__.out("created").has("name", "lop")).values("name").to_list()   // "vadas"
g.gql("MATCH (p:person) RETURN p.name AS name, reduce(total = 0, w IN [(p)-[e]->() | e.weight] | total + w) AS weight ORDER BY name").to_list()
// josh 1.4, marko 1.9, peter 0.2, vadas 0 (reduce starts at 0)
g.V().hasLabel("person").order().by("name").project("name", "weight").by("name").by(__.out_e().values("weight").fold().sum(Scope.local)).to_list()
// josh 1.4, marko 1.9, peter 0.2, vadas without "weight" (the sum of no values is no value)
g.gql("MATCH (p:person) RETURN reduce(s = '', n IN collect_list(p.name) | s || n) AS names").to_list()   // one string of the four names
```

## Map projections and comprehensions

Three Cypher expression forms have no GQL counterpart with the same meaning, so they are not
aliases either. `gql()` accepts them as **compatibility forms**, like the compatibility
functions: with their openCypher / Neo4j Cypher 25 meaning, because they are how Cypher users and
LLMs ask for nested, JSON-shaped answers ([The Supported Subset](subset.md#map-projections-and-comprehensions)).

| Cypher form | GQL nearest form | Difference |
|---|---|---|
| map projection `p {.name, .age, .*, f, k: expr}` | the record constructor `{name: p.name, age: p.age, f: f, k: expr}` (or `RECORD {..}`) | for a `NULL` `p` the projection is `NULL`, the record `{name: NULL, ...}`; `.*` has no record form (`properties(p)` is the map of all properties) |
| list comprehension `[x IN list WHERE cond \| expr]` | none; `FOR x IN list FILTER cond RETURN collect_list(expr)` gives one list per query, not per row | |
| pattern comprehension `[(p)-[:R]->(f) WHERE cond \| expr]` | none; `OPTIONAL MATCH (p)-[:R]->(f) WHERE cond` + `collect_list(expr)` groups the rows instead | the comprehension keeps the row and gives `[]` when nothing matches |

The projection and the record give the same map whenever the variable is not `NULL`:

```rhai,graph=modern
g.gql("MATCH (p:person) RETURN p {.name, .age} AS m ORDER BY p.name LIMIT 1").to_list()   // #{"m": #{"age": 32, "name": "josh"}}
g.gql("MATCH (p:person) RETURN {name: p.name, age: p.age} AS m ORDER BY p.name LIMIT 1").to_list()   // #{"m": #{"age": 32, "name": "josh"}}
g.gql("MATCH (p:person) RETURN p.name AS name, [(p)-[:created]->(s) | s.name] AS sw ORDER BY name").to_list()
// josh [ripple, lop] (in any order), marko [lop], peter [lop], vadas []
g.gql("MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s) RETURN p.name AS name, collect_list(s.name) AS sw ORDER BY name").to_list()
// josh [ripple, lop], marko [lop], peter [lop], vadas []
```

Cypher's list predicates `all()`, `any()`, `none()`, `single()` and `reduce()` bind a variable
the same way; they are [compatibility functions](#list-predicates-and-reduce).

## COUNT and COLLECT

Cypher's `COUNT { }` and `COLLECT { }` subqueries have no GQL counterpart either; `gql()`
accepts them as compatibility forms with their Neo4j Cypher 25 meaning
([COUNT and COLLECT](subset.md#count-and-collect)). They take the bodies of `EXISTS`: a pattern,
`MATCH` clauses, or a whole query ending in `RETURN` (one item for `COLLECT`).

| Cypher form | GQL nearest form | Difference |
|---|---|---|
| `COUNT { <pattern or query> }` | `CALL (p) { OPTIONAL MATCH .. RETURN count(x) AS n }`, or `OPTIONAL MATCH` + `count()` | the subquery keeps the row and stands in any expression; `COUNT { } > 0` is `EXISTS { }` |
| `COLLECT { <query> RETURN x }` | `CALL (p) { OPTIONAL MATCH .. RETURN collect_list(x) AS l }` | `COLLECT` keeps `NULL` items, `collect_list()` skips them |

```rhai,graph=modern
g.gql("MATCH (p:person) RETURN p.name AS name, COUNT { (p)-[:created]->() } AS n ORDER BY name").to_list()   // josh 2, marko 1, peter 1, vadas 0
g.gql("MATCH (p:person) CALL (p) { OPTIONAL MATCH (p)-[:created]->(s) RETURN count(s) AS n } RETURN p.name AS name, n ORDER BY name").to_list()   // josh 2, marko 1, peter 1, vadas 0
g.gql("MATCH (p:person) RETURN p.name AS name, COLLECT { MATCH (p)-[:created]->(s) RETURN s.name ORDER BY s.name } AS sw ORDER BY name").to_list()
// josh [lop, ripple], marko [lop], peter [lop], vadas []
g.gql("MATCH (p:person) CALL (p) { OPTIONAL MATCH (p)-[:created]->(s) WITH s ORDER BY s.name RETURN collect_list(s.name) AS sw } RETURN p.name AS name, sw ORDER BY name").to_list()
// josh [lop, ripple], marko [lop], peter [lop], vadas []
```

One difference from Neo4j: a `COUNT { }` body whose final `RETURN` only projects is counted
without evaluating its items ([Semantics](semantics.md#count-and-collect)).

## SET and REMOVE

GQL's `SET` items are a property (`SET n.k = v`), all properties from a braced map
(`SET n = {k: v}`) and a label (`SET n:A` / `SET n IS A`); `REMOVE` takes a property or a label.
Two Cypher forms have no GQL counterpart and are accepted as **compatibility forms** with
Cypher's meaning:

- **`SET n += map`** merges the map into the properties: the keys of the map are set (a `NULL`
  value removes its key), the other properties stay.
- **`SET n = expr`** with an expression that is not a braced map: a `$parameter` or any map
  value replaces every property, and a node or an edge stands for its properties, so `SET n = m`
  copies them (`SET n = n` changes nothing). Write `SET n = properties(m)` for the same as a
  plain map.

A `NULL` on the right of `=` or `+=` is an error (`22004`) rather than an empty map; write
`coalesce($map, {})` ([Semantics](semantics.md#set-and-remove)).

```rhai,graph=modern
g.gql("MATCH (p:person {name: 'marko'}) SET p += {city: 'Rome'} RETURN properties(p) AS props").to_list()   // #{"props": #{"age": 29, "city": "Rome", "name": "marko"}}
g.gql("MATCH (p:person {name: 'marko'}) SET p = $props RETURN properties(p) AS props", #{props: #{name: "marko", city: "Rome"}}).to_list()   // #{"props": #{"city": "Rome", "name": "marko"}}
g.gql("MATCH (p:person {name: 'marko'}), (q:person {name: 'josh'}) SET p = q RETURN properties(p) AS props").to_list()   // #{"props": #{"age": 32, "name": "josh"}}
g.gql("MATCH (p:person {name: 'marko'}) SET p += $more", #{more: ()})   // error: the right side of SET p += .. is NULL, not a map
```

## Cypher spellings that are not aliases

| Cypher | In `gql()` |
|---|---|
| `id(n)` | refused: an integer in Neo4j, Graphersal ids are strings; use `element_id(n)` |
| `nodes(p)`, `relationships(p)` | supported as [compatibility functions](#compatibility-functions), not aliases: GQL has no such functions |
| `x ^ y` | not supported yet; use GQL's `power(x, y)` |
| `rand()`, `randomUUID()` | refused: `gql()` functions are deterministic |
| map projections, list and pattern comprehensions | supported as [compatibility forms](#map-projections-and-comprehensions), not aliases |
| `any()` / `all()` / `none()` / `single()`, `reduce()` | supported as [compatibility functions](#list-predicates-and-reduce), not aliases: GQL has no such functions |
| `COUNT { }`, `COLLECT { }` | supported as [compatibility forms](#count-and-collect), not aliases |
| `CALL { } IN TRANSACTIONS` | [refused](refused.md): a write form; the `CALL` itself is supported |
| `exists(n.p)` (Cypher 4) | [refused](refused.md): `EXISTS` takes a pattern; test a property with `n.p IS NOT NULL` |
| `GROUP BY` (not Cypher) | ISO GQL, accepted after the `RETURN` items (optional: grouping is implicit); after `WITH` an error that explains implicit grouping |
| `MERGE`, `FOREACH` | not supported yet ([Writing with GQL](writes.md#not-yet)); `CREATE` is an alias of `INSERT`; `SET` and `REMOVE` are supported, `+=` as a [compatibility form](#set-and-remove) |
| `DELETE`, `DETACH DELETE` | the same in GQL, supported ([Writing with GQL](writes.md#delete)); GQL also writes the default explicitly as `NODETACH DELETE` |
| `SET n[key] = v`, `SET n:$(expr)` (Cypher 25 dynamic keys and labels) | not supported; name the key and the label in the query |
| `CALL db.*()`, `apoc.*`, `SHOW ...` | refused: vendor extensions |
| `a--1` (minus minus one) | a comment starts at `--` (GQL wins): `RETURN n.age--1 AS x` returns the column `n.age`; write `n.age - -1` |
