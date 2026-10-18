# The Supported Subset

This page lists what `gql()` accepts in this version, construct by construct, each with a
runnable example and, where it is natural, its Gremlin twin. Everything not listed here is an
error that says so ([Not supported yet](#not-supported-yet) at the end,
[Refused Constructs](refused.md) for what will never be supported).

## The shape of a query

```text
MATCH <pattern> [, <pattern> ...] [WHERE <condition>]
[MATCH ... ]                                  -- more MATCH clauses
RETURN [DISTINCT] <item> [AS <name>], ...     -- or RETURN *, mandatory
[ORDER BY <expr> [ASC | DESC], ...]
[OFFSET <n>]                                  -- Cypher alias: SKIP
[LIMIT <n>]
[;]
```

Keywords are case-insensitive (`match`, `Match`, `MATCH`); variables, labels and property keys
are case-sensitive. Line breaks are whitespace, so a query may span lines. Comments are `// ...`,
`/* ... */` and the GQL `-- ...` ([the `--` rule](rhai_strings.md#comments)).

```rhai,graph=modern
g.gql("match (n:person) where n.age > 30 return n.name as name order by name desc").to_list()   // #{"name": "peter"}, #{"name": "josh"}
```

## MATCH

`MATCH` takes one or more patterns separated by commas. A variable written twice is the same
element: that is how patterns join. Several `MATCH` clauses in a row combine the same way.

```rhai,graph=modern
g.gql("MATCH (a)-[:knows]->(b), (b)-[:created]->(s) RETURN a.name AS a, s.name AS s ORDER BY s").to_list()   // #{"a": "marko", "s": "lop"}, #{"a": "marko", "s": "ripple"}
g.gql("MATCH (a)-[:knows]->(b) MATCH (b)-[:created]->(s) RETURN b.name AS b, s.name AS s ORDER BY b, s").to_list()   // #{"b": "josh", "s": "lop"}, #{"b": "josh", "s": "ripple"}
```

Patterns that share no variable form a Cartesian product: every combination of their matches.

```rhai,graph=modern
g.gql("MATCH (a:person), (s:software) WHERE a.age > 32 RETURN a.name AS a, s.name AS s ORDER BY a, s").to_list()   // #{"a": "peter", "s": "lop"}, #{"a": "peter", "s": "ripple"}
```

### Node patterns

| Form | Matches |
|---|---|
| `()` | any vertex, unnamed |
| `(n)` | any vertex, bound to `n` |
| `(n:person)` or `(n IS person)` | a vertex with the label `person` |
| `(n:person {name: 'marko'})` | ... whose `name` equals `'marko'` |
| `(n:person WHERE n.age > 30)` | ... for which the condition is TRUE |

```rhai,graph=modern
g.gql("MATCH (p:person {name: 'marko'})-->(x) RETURN x.name AS x ORDER BY x").to_list()   // #{"x": "josh"}, #{"x": "lop"}, #{"x": "vadas"}
g.gql("MATCH (p:person WHERE p.age < 30) RETURN p.name AS name ORDER BY name").to_list()   // #{"name": "marko"}, #{"name": "vadas"}
g.v().has_label("person").has("age", P.lt(30)).values("name").order().to_list()   // "marko", "vadas"
```

An inline property map `{k: v}` is the same as `WHERE n.k = v`: a property that is missing or
`NULL` never matches.

### Edge patterns

| Form | Abbreviated | Matches |
|---|---|---|
| `-[e:T]->` | `->` (Cypher `-->`) | an edge pointing right |
| `<-[e:T]-` | `<-` (Cypher `<--`) | an edge pointing left |
| `-[e:T]-` | `-` (Cypher `--`) | an edge in either direction |
| `<-[e:T]->` | `<->` (Cypher `<-->`) | an edge in either direction (same as `-[e]-`) |

The brackets take the same parts as a node: a variable, a label expression after `:` or `IS`,
an inline property map and a `WHERE` condition. Graphersal edges are always directed, so
"either direction" means "pointing left or right"; a self-loop matches once
([Semantics](semantics.md#either-direction-and-self-loops)).

```rhai,graph=modern
g.gql("MATCH (a)-[e:created]->(s:software) WHERE e.weight >= 0.4 RETURN a.name AS who, s.name AS what, e.weight AS w ORDER BY w DESC, who").to_list()
// #{"w": 1.0, "what": "ripple", "who": "josh"}, #{"w": 0.4, "what": "lop", "who": "josh"}, #{"w": 0.4, "what": "lop", "who": "marko"}
g.gql("MATCH (s:software)<-[:created]-(p) RETURN s.name AS sw, p.name AS author ORDER BY sw, author").to_list()
// #{"author": "josh", "sw": "lop"}, #{"author": "marko", "sw": "lop"}, #{"author": "peter", "sw": "lop"}, #{"author": "josh", "sw": "ripple"}
g.gql("MATCH (a)-[:knows]-(b) RETURN a.name AS a, b.name AS b ORDER BY a, b").to_list()
// #{"a": "josh", "b": "marko"}, #{"a": "marko", "b": "josh"}, #{"a": "marko", "b": "vadas"}, #{"a": "vadas", "b": "marko"}
g.gql("MATCH (a {name: 'marko'})-(b) RETURN b.name AS b ORDER BY b").to_list()   // #{"b": "josh"}, #{"b": "lop"}, #{"b": "vadas"}
g.v().has("name", "marko").both().values("name").order().to_list()   // "josh", "lop", "vadas"
```

Patterns chain: `(a)-[..]->(b)-[..]->(c)` is a path of two hops, and the same variable may close a
cycle.

```rhai,graph=modern
g.gql("MATCH (a:person)-[:knows]->(b)-[:created]->(s) RETURN a.name AS a, b.name AS b, s.name AS s ORDER BY b, s").to_list()
// #{"a": "marko", "b": "josh", "s": "lop"}, #{"a": "marko", "b": "josh", "s": "ripple"}
g.v().has_label("person").as("a").out("knows").as("b").out("created").as("s").select("a", "b", "s").by("name").to_list()
// #{"a": "marko", "b": "josh", "s": "ripple"}, #{"a": "marko", "b": "josh", "s": "lop"}
g.gql("MATCH (a)-[:created]->(s)<-[:created]-(b) RETURN a.name AS a, b.name AS b, s.name AS s ORDER BY s, a, b").to_list()
// josh/marko, josh/peter, marko/josh, marko/peter, peter/josh, peter/marko, all on "lop"
```

The last query never pairs a person with themselves: both edges would be the same edge, and a
`MATCH` binds every edge at most once ([trail semantics](semantics.md#trail-different-edges)).

## Label expressions

After `:` or `IS`, on nodes and edges:

| Expression | Means |
|---|---|
| `A` | has the label `A` |
| `A&B` | has both (Cypher `:A:B`) |
| `A\|B` | has at least one |
| `!A` | does not have `A` |
| `%` | has any label (the label set is not empty) |
| `(...)` | grouping, as in `A&(B\|C)` |

A vertex tests its [label set](../users_guide/multi_label_vertices.md); an edge has exactly one
label.

```rhai,graph=modern
g.gql("MATCH (n:person|software) RETURN n.name AS name ORDER BY name LIMIT 3").to_list()   // #{"name": "josh"}, #{"name": "lop"}, #{"name": "marko"}
g.gql("MATCH (n:!person) RETURN n.name AS name ORDER BY name").to_list()   // #{"name": "lop"}, #{"name": "ripple"}
g.v().not(__.has_label("person")).values("name").order().to_list()   // "lop", "ripple"
g.gql("MATCH (n IS person&!software) RETURN n.name AS name ORDER BY name").to_list()   // josh, marko, peter, vadas
```

In expressions the same test is `v IS [NOT] LABELED <label expression>` (Cypher: `v:A`):

```rhai,graph=modern
g.gql("MATCH (n) WHERE n IS LABELED software RETURN n.name AS name ORDER BY name").to_list()   // #{"name": "lop"}, #{"name": "ripple"}
g.gql("MATCH (n) WHERE n:software RETURN n.name AS name ORDER BY name").to_list()   // #{"name": "lop"}, #{"name": "ripple"}
```

## WHERE

`WHERE` after a `MATCH` clause (or inside a node or edge pattern) keeps the rows for which the
condition is TRUE; FALSE and `NULL` drop the row ([three-valued logic](semantics.md#null-and-three-valued-logic)).
It may read every variable of its `MATCH` clause and of the clauses before it.

```rhai,graph=modern
g.gql("MATCH (a:person), (b:person) WHERE a.age < b.age AND a.name STARTS WITH 'm' RETURN a.name AS a, b.name AS b ORDER BY b").to_list()
// #{"a": "marko", "b": "josh"}, #{"a": "marko", "b": "peter"}
```

## RETURN

`RETURN` is mandatory and builds one map per row: `RETURN expr AS name, ...`, `RETURN *` (every
named variable) and `RETURN DISTINCT` (duplicate rows removed). [Result Shape](result_shape.md)
covers names and column order.

```rhai,graph=modern
g.gql("MATCH (a)-[:created]->(s) RETURN s.name AS s ORDER BY s").to_list()   // lop, lop, lop, ripple
g.gql("MATCH (a)-[:created]->(s) RETURN DISTINCT s.name AS s ORDER BY s").to_list()   // #{"s": "lop"}, #{"s": "ripple"}
g.v().out("created").values("name").dedup().order().to_list()   // "lop", "ripple"
```

## ORDER BY, OFFSET, LIMIT

`ORDER BY` takes expressions (a column name, a property, a function call) with `ASC` /
`ASCENDING` (the default) or `DESC` / `DESCENDING`; it may read variables that are not returned
(except after `RETURN DISTINCT`). `OFFSET n` (Cypher `SKIP n`) skips rows, `LIMIT n` keeps at
most `n`. They come in this order, each at most once; `n` is a non-negative integer literal or a
parameter.

```rhai,graph=modern
g.gql("MATCH (n:person) RETURN n.name AS name ORDER BY n.age DESC").to_list()   // peter, josh, marko, vadas
g.gql("MATCH (n:person) RETURN n.name AS name ORDER BY name OFFSET 1 LIMIT 2").to_list()   // #{"name": "marko"}, #{"name": "peter"}
g.v().has_label("person").values("name").order().range(1, 3).to_list()   // "marko", "peter"
```

Without `ORDER BY` the row order is unspecified; [Semantics](semantics.md#ordering) gives the
order of `NULL` and of values of different types.

## Expressions

| Kind | Forms |
|---|---|
| literals | integers (`42`, `1_000`, `0x1F`, `0o17`, `0b101`), floats (`1.5`, `.5`, `1.5e3`), strings (`'...'` or `"..."`), `TRUE`, `FALSE`, `NULL`, lists `[1, 2]`, maps `{k: v}` |
| parameters | `$name` ([Parameters](#parameters)) |
| property access | `n.name`, `m.key` on a map |
| comparison | `=`, `<>` (Cypher `!=`), `<`, `<=`, `>`, `>=`; not chainable (`a < b < c` is an error) |
| logic | `AND`, `OR`, `XOR`, `NOT` |
| predicates | `IS [NOT] NULL`, `IN [list]`, `STARTS WITH`, `ENDS WITH`, `CONTAINS`, `IS [NOT] LABELED` |
| arithmetic | `+`, `-`, `*`, `/`, `%`, unary `-` and `+` |
| concatenation | `\|\|` on two strings or two lists (Cypher `+`) |
| conditional | simple `CASE x WHEN v THEN r ... ELSE d END` and searched `CASE WHEN cond THEN r ... END` |

Precedence, loosest first: `OR`, `XOR`, `AND`, `NOT`, comparison, the predicates (`IS NULL`,
`IN`, `STARTS WITH`, ...), `+ - ||`, `* / %`, unary minus, property access.

```rhai,graph=modern
g.gql("MATCH (n:person) WHERE n.age IN [27, 35] RETURN n.name AS name ORDER BY name").to_list()   // #{"name": "peter"}, #{"name": "vadas"}
g.gql("MATCH (n:person) WHERE n.name STARTS WITH 'm' OR n.name ENDS WITH 's' OR n.name CONTAINS 'et' RETURN n.name AS name ORDER BY name").to_list()   // marko, peter, vadas
g.gql("MATCH (n:person) WHERE n.age > 30 XOR n.name = 'josh' RETURN n.name AS name ORDER BY name").to_list()   // #{"name": "peter"}
g.gql("MATCH (n:person) RETURN n.name || '!' AS a, n.age * 2 + 1 AS b, n.age / 10 AS c, n.age % 10 AS d, n.age / 10.0 AS e ORDER BY a LIMIT 1").to_list()
// #{"a": "josh!", "b": 65, "c": 3, "d": 2, "e": 3.2}
g.gql("MATCH (n:person) RETURN n.name AS name, CASE WHEN n.age < 30 THEN 'young' WHEN n.age < 34 THEN 'middle' ELSE 'senior' END AS band ORDER BY name").to_list()
// josh middle, marko young, peter senior, vadas young
g.gql("MATCH (n) RETURN n.name AS name, CASE n.lang WHEN 'java' THEN 'jvm' ELSE 'other' END AS k ORDER BY name LIMIT 2").to_list()
// #{"k": "other", "name": "josh"}, #{"k": "jvm", "name": "lop"}
g.gql("MATCH (n:person) RETURN {name: n.name, tags: [n.age, NULL, true]} AS m ORDER BY n.name LIMIT 1").to_list()
// #{"m": #{"name": "josh", "tags": [32, (), true]}}
```

Integer arithmetic stays integer (`/` truncates toward zero, `%` takes the sign of the
dividend); a float operand makes the result a float. An overflow or a division by zero is an
error, never a wrapped or infinite value ([Errors](errors.md#errors-while-the-query-runs)).

## Functions

| Function | Returns | Cypher alias |
|---|---|---|
| `element_id(x)` | the id of a vertex or edge, a string | `elementId(x)` |
| `labels(x)` | a vertex's labels in stored order (primary first); an edge's label as a one-item list | |
| `type(e)` | an edge's label; a vertex is an error (use `labels(v)`) | |
| `coalesce(a, b, ...)` | the first argument that is not `NULL` | |
| `upper(s)`, `lower(s)` | the string in upper / lower case | `toUpper(s)`, `toLower(s)` |
| `char_length(s)`, `character_length(s)` | the number of characters | `size(s)` |
| `size(list)`, `cardinality(list)` | the number of items | |

Function names are case-insensitive. Every function returns `NULL` for a `NULL` argument and
fails for an argument of the wrong kind.

```rhai,graph=modern
g.gql("MATCH (n) RETURN element_id(n) AS id, labels(n) AS labels ORDER BY id LIMIT 3").to_list()
// #{"id": "1", "labels": ["person"]}, #{"id": "2", "labels": ["person"]}, #{"id": "3", "labels": ["software"]}
g.gql("MATCH ()-[e]->() RETURN element_id(e) AS id, type(e) AS type, e.weight AS w ORDER BY id LIMIT 2").to_list()
// #{"id": "0", "type": "knows", "w": 0.5}, #{"id": "1", "type": "created", "w": 0.4}
g.gql("MATCH (n) RETURN n.name AS name, coalesce(n.age, n.lang) AS info ORDER BY name LIMIT 2").to_list()
// #{"info": 32, "name": "josh"}, #{"info": "java", "name": "lop"}
g.gql("MATCH (n:person) RETURN upper(n.name) AS up, char_length(n.name) AS len, size([1, 2, 3]) AS sz ORDER BY up LIMIT 1").to_list()
// #{"len": 4, "sz": 3, "up": "JOSH"}
```

## Parameters

`$name` reads the value of `name` from the parameter map, the second argument of `gql()`. A
parameter is a value, never query text: it cannot change the query, whatever it holds. A
parameter the query reads but the map lacks is an error before the run; extra entries are
ignored.

```rhai,graph=modern
g.gql("MATCH (n:person) WHERE n.age > $min RETURN n.name AS name ORDER BY name", #{min: 30}).to_list()   // #{"name": "josh"}, #{"name": "peter"}
g.gql("MATCH (n:person) WHERE n.name IN $names RETURN n.name AS name, n.age AS age ORDER BY name", #{names: ["josh", "vadas"]}).to_list()
// #{"age": 32, "name": "josh"}, #{"age": 27, "name": "vadas"}
g.gql("MATCH (n:person {name: $who})-[:created]->(s) RETURN s.name AS s", #{who: "peter"}).to_list()   // #{"s": "lop"}
g.gql("MATCH (n:person) RETURN n.name AS name ORDER BY name SKIP $s LIMIT $l", #{s: 2, l: 5}).to_list()   // #{"name": "peter"}, #{"name": "vadas"}
```

A parameter may also stand where an index can serve it, so `MATCH (p:person {name: $n})` uses a
property index on `person.name` when one exists ([Plans and Performance](plans.md#index-use)).

## Not supported yet

These are part of GQL and come in a later version. Each fails before the run with GQLSTATUS
`0A000` and a message that names the construct and the Gremlin alternative.

| Construct | Until then |
|---|---|
| aggregation: `count()`, `sum()`, `avg()`, `min()`, `max()`, `collect_list()` (Cypher `collect()`), `GROUP BY` | Gremlin after `gql()`: `.select("age").sum()`, `count()`, `fold()`, `group()` |
| `OPTIONAL MATCH` | Gremlin `optional()` |
| `WITH`, `LET`, `FILTER`, `NEXT` | conditions in `WHERE`, values in `RETURN`, or Gremlin steps after `gql()` |
| quantified edges `-[:T]->{1,3}`, `*`, `+`, Cypher `-[:T*1..3]->` | Gremlin `repeat()` |
| path variables `p = (a)-->(b)`, path search prefixes and match modes (`ANY SHORTEST`, `TRAIL`, `DIFFERENT EDGES` written out) | Gremlin `path()`, `repeat()` |
| `UNION`, `EXCEPT`, `INTERSECT`, `EXISTS { }`, `CALL { }` subqueries | Gremlin `union()`, `where()` |
| `FOR` (Cypher `UNWIND`) | Gremlin `unfold()` |
| list indexing and slicing `x[0]`, `x[1..2]` | Gremlin after `gql()` |
| `NULLS FIRST` / `NULLS LAST` | the fixed order: `NULL` last ascending, first descending |
| a query without `MATCH` (`RETURN 1`) | Gremlin `inject()` |
| `gql()` after other steps (`g.v().gql(..)`) | start with `g.gql(..)` and continue with Gremlin |

```rhai,graph=modern
g.gql("MATCH (n:person) RETURN count(n) AS c")   // error: the aggregate function count() is not yet supported
g.gql("MATCH (n:person) RETURN n.name AS name").count().next()   // 4
```
