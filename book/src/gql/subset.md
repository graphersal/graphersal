# The Supported Subset

This page lists what `gql()` accepts in this version, construct by construct, each with a
runnable example and, where it is natural, its Gremlin twin. Everything not listed here is an
error that says so ([Not supported yet](#not-supported-yet) at the end,
[Refused Constructs](refused.md) for what will never be supported).

## The shape of a query

```text
[OPTIONAL] MATCH <pattern> [, <pattern> ...] [WHERE <condition>]
[ more clauses, in any order and number:
  [OPTIONAL] MATCH ...
  LET <name> = <expr> [, ...]
  FILTER [WHERE] <condition>
  WITH [DISTINCT] <item> [AS <name>], ...     -- or WITH *; Cypher
       [ORDER BY ...] [OFFSET <n>] [LIMIT <n>] [WHERE <condition>] ]
RETURN [DISTINCT] <item> [AS <name>], ...     -- or RETURN *, mandatory; items may aggregate
[ORDER BY <expr> [ASC | DESC], ...]
[OFFSET <n>]                                  -- Cypher alias: SKIP
[LIMIT <n>]
[;]
```

A pattern is a path of node and edge patterns, optionally named (`p = (a)-->(b)`); an edge may
carry a quantifier (`-[:knows]->{1,3}`).

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

### Quantified edges

A quantifier after an edge pattern repeats the edge: `(a)-[:knows]->{1,3}(b)` matches a path of
one to three `knows` edges from `a` to `b`, every node in between unnamed. It applies to full and
abbreviated edges (`->{2}`) in every direction.

| Quantifier | Repeats the edge | Cypher spelling ([alias](aliases.md)) |
|---|---|---|
| `{m,n}` | `m` to `n` times | `[*m..n]` |
| `{n}` | exactly `n` times | `[*n]` |
| `{m,}` | at least `m` times | `[*m..]` |
| `{,n}` | 0 to `n` times | |
| `*` | 0 or more times, `{0,}` | |
| `+` | 1 or more times, `{1,}` | `[*]` (Cypher's lower bound is 1) |
| | 1 to `n` times, `{1,n}` | `[*..n]` |

A lower bound of 0 includes the zero-length match, where `b` is `a` itself. The edges of one match
are all different (trail), so an unbounded quantifier ends on every finite graph
([Semantics](semantics.md#quantified-edges)). The Gremlin twin is a `repeat()` with `emit()` and
`times()`:

```rhai,graph=modern
g.gql("MATCH (a {name: 'marko'})-[:knows|created]->{1,2}(b) RETURN b.name AS b ORDER BY b").to_list()
// josh, lop, lop, ripple, vadas
g.v().has("name", "marko").repeat(__.out("knows", "created")).emit().times(2).values("name").order().to_list()
// "josh", "lop", "lop", "ripple", "vadas"
g.gql("MATCH (a {name: 'marko'})->{2}(b) RETURN b.name AS b ORDER BY b").to_list()   // #{"b": "lop"}, #{"b": "ripple"}
g.v().has("name", "marko").repeat(__.out()).times(2).values("name").order().to_list()   // "lop", "ripple"
g.gql("MATCH (a {name: 'marko'})->{0,1}(b) RETURN b.name AS b ORDER BY b").to_list()   // josh, lop, marko, vadas
g.v().has("name", "marko").emit().repeat(__.out()).times(1).values("name").order().to_list()   // "josh", "lop", "marko", "vadas"
g.gql("MATCH (a {name: 'marko'})-[:knows]->{1,2}(b)-[:created]->(s) RETURN b.name AS b, s.name AS s ORDER BY b, s").to_list()
// #{"b": "josh", "s": "lop"}, #{"b": "josh", "s": "ripple"}
```

The variable of a quantified edge is a **group variable**: after the pattern it is the list of
the edges of the match, in the written order (`[]` for a zero-length match). Inside its own
brackets (`-[e WHERE e.weight > 0.5]->{1,2}`) it is the single edge of each repetition, so the
condition tests every edge:

```rhai,graph=modern
g.gql("MATCH (a {name: 'marko'})-[e]->*(b) RETURN b.name AS b, size(e) AS hops ORDER BY hops, b").to_list()
// marko 0, josh 1, lop 1, vadas 1, lop 2, ripple 2
g.gql("MATCH (a {name: 'marko'})-[e:knows|created WHERE e.weight > 0.5]->{1,2}(b) RETURN b.name AS b, e AS edges ORDER BY b").to_list()
// #{"b": "josh", "edges": [e[2][1-knows->4]]}, #{"b": "ripple", "edges": [e[2][1-knows->4], e[4][4-created->5]]}
g.v().has("name", "marko").repeat(__.out_e("knows", "created").has("weight", P.gt(0.5)).in_v()).emit().times(2).values("name").order().to_list()
// "josh", "ripple"
```

Only single edges take a quantifier; a quantified parenthesized path `((a)-->(b)){1,3}` is not
supported yet.

### Path variables

`p = <path pattern>` names the whole path. `RETURN p` returns it as a path value (the
alternating vertices and edges, the same value as Gremlin's `path()`;
[Result Shape](result_shape.md#paths)), and `path_length(p)` (Cypher `length(p)`) is its number
of edges. A quantified edge contributes all its edges and the vertices in between.

```rhai,graph=modern
g.gql("MATCH p = (a {name: 'marko'})-[:knows]->(b)-[:created]->(s) RETURN p, path_length(p) AS len ORDER BY s.name").to_list()
// #{"len": 2, "p": [v[1], e[2][1-knows->4], v[4], e[3][4-created->3], v[3]]}, #{"len": 2, "p": [v[1], e[2][1-knows->4], v[4], e[4][4-created->5], v[5]]}
g.v().has("name", "marko").out_e("knows").in_v().out_e("created").in_v().path().to_list()
// [v[1], e[2][1-knows->4], v[4], e[4][4-created->5], v[5]], [v[1], e[2][1-knows->4], v[4], e[3][4-created->3], v[3]]
g.gql("MATCH p = (a {name: 'marko'})->{1,2}(b) RETURN b.name AS b, length(p) AS len ORDER BY len, b").to_list()
// josh 1, lop 1, vadas 1, lop 2, ripple 2
```

A path is a value: it compares (`p = q`), sorts, groups and goes into `collect_list()`. Cypher's
`nodes(p)` and `relationships(p)` are not supported yet: return `p`, or name the elements and
return them.

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

## OPTIONAL MATCH

`OPTIONAL MATCH` keeps every row that reaches it: where its pattern (and its own `WHERE`)
matches, the row is extended once per match; where it does not, the row continues once with the
new variables set to `NULL`. Gremlin's twin is `optional()`.

```rhai,graph=modern
g.gql("MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s) RETURN p.name AS name, s.name AS sw ORDER BY name, sw").to_list()
// #{"name": "josh", "sw": "lop"}, #{"name": "josh", "sw": "ripple"}, #{"name": "marko", "sw": "lop"}, #{"name": "peter", "sw": "lop"}, #{"name": "vadas", "sw": ()}
g.v().has_label("person").as("p").optional(__.out("created").as("s")).project("name", "sw").by(__.select("p").values("name")).by(__.coalesce(__.select("s").values("name"), __.constant("-"))).order().by("name").to_list()
// the same five rows, vadas with "-"
```

The `WHERE` of an `OPTIONAL MATCH` belongs to it: a row whose optional part fails the condition
keeps its `NULL`s instead of being dropped. A leading `OPTIONAL MATCH` starts from one empty row,
so a pattern without a match gives one row of `NULL`s:

```rhai,graph=modern
g.gql("MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s) WHERE s.lang = 'none' RETURN p.name AS name, s AS s ORDER BY name").to_list()
// josh, marko, peter, vadas, each with s ()
g.gql("OPTIONAL MATCH (n:robot) RETURN n").to_list()   // #{"n": ()}
```

`NULL` columns then work as everywhere: `count(s)` skips them, so this counts each person's
software, 0 for vadas ([Aggregation](#aggregation)).

## LET and FILTER

`LET name = expr` names a value computed from the variables in scope; `FILTER condition`
(`FILTER WHERE condition` too) keeps the rows for which it is TRUE, like a `WHERE` that may stand
anywhere between the clauses. Neither changes the rows otherwise: `MATCH ... LET ... FILTER ...`
runs exactly like the same query with the value written out in `WHERE` and `RETURN`, including
the conditions that become `has()` ([Plans and Performance](plans.md#let-filter-and-with)).

```rhai,graph=modern
g.gql("MATCH (p:person) LET decade = p.age / 10 * 10 FILTER decade = 30 RETURN p.name AS name, decade ORDER BY name").to_list()
// #{"decade": 30, "name": "josh"}, #{"decade": 30, "name": "peter"}
g.v().has_label("person").has("age", P.between(30, 40)).project("name", "decade").by("name").by(__.constant(30)).order().by("name").to_list()
// the same rows
```

A `LET` name may not be bound again, and a `LET` item may not read a name defined in the same
`LET` (write a second `LET`).

## WITH

`WITH` (Cypher; GQL's linear statements express the same) ends one part of the query and passes
only its items on: the next clauses see only the names `WITH` lists (`WITH *` keeps all). An item
that is not a plain variable needs `AS`. `WITH` may take `DISTINCT`, an aggregate,
`ORDER BY`, `OFFSET` / `SKIP`, `LIMIT` and a `WHERE`, in this order; the `WHERE` filters **after**
`LIMIT` (Cypher's order of the sub-clauses).

```rhai,graph=modern
g.gql("MATCH (p:person) WITH p.name AS name, p.age AS age WHERE age < 30 RETURN name ORDER BY name").to_list()   // #{"name": "marko"}, #{"name": "vadas"}
g.gql("MATCH (p:person) WITH p ORDER BY p.age DESC LIMIT 2 MATCH (p)-[:created]->(s) RETURN p.name AS name, s.name AS sw ORDER BY name, sw").to_list()
// #{"name": "josh", "sw": "lop"}, #{"name": "josh", "sw": "ripple"}, #{"name": "peter", "sw": "lop"}
g.gql("MATCH (p:person) WITH p ORDER BY p.age LIMIT 3 WHERE p.age > 28 RETURN p.name AS name ORDER BY name").to_list()
// #{"name": "josh"}, #{"name": "marko"}: the three youngest, then the filter
g.gql("MATCH (p:person) WITH p.name AS name RETURN p.age")   // error: the variable `p` is not in scope: the WITH before does not carry it
```

A `WITH` that only renames and computes values is as transparent as `LET`. One with `DISTINCT`,
an aggregate, `ORDER BY`, `OFFSET` or `LIMIT` builds its rows first; a later `MATCH` continues
from the elements in its columns. The order a `WITH ... ORDER BY` makes is not kept by later
clauses: sort again in the final `RETURN` ([Ordering](semantics.md#ordering)).

## RETURN

`RETURN` is mandatory and builds one map per row: `RETURN expr AS name, ...`, `RETURN *` (every
named variable) and `RETURN DISTINCT` (duplicate rows removed, with GQL's equality:
`1` and `1.0` are one value, [Semantics](semantics.md#distinct-and-grouping-equality)).
[Result Shape](result_shape.md) covers names and column order.

```rhai,graph=modern
g.gql("MATCH (a)-[:created]->(s) RETURN s.name AS s ORDER BY s").to_list()   // lop, lop, lop, ripple
g.gql("MATCH (a)-[:created]->(s) RETURN DISTINCT s.name AS s ORDER BY s").to_list()   // #{"s": "lop"}, #{"s": "ripple"}
g.v().out("created").values("name").dedup().order().to_list()   // "lop", "ripple"
```

## Aggregation

`RETURN` and `WITH` items may aggregate. The items without an aggregate are the **grouping
keys**: one row per distinct combination of their values (no `GROUP BY` is written; grouping is
implicit, as in Cypher). Without keys the whole input is one group.

| Aggregate | Returns | On no input |
|---|---|---|
| `count(*)` | the number of rows | `0` |
| `count(x)` | the number of rows where `x` is not `NULL` | `0` |
| `sum(x)` | the sum of the numbers; integers stay integers | `0` |
| `avg(x)` | the mean, always a float | `NULL` |
| `min(x)`, `max(x)` | the smallest / largest value in the [order of `ORDER BY`](semantics.md#ordering) | `NULL` |
| `collect_list(x)` (Cypher `collect(x)`) | the values as a list | `[]` |

`NULL` values are skipped by every aggregate except `count(*)`. `DISTINCT` inside the call
(`count(DISTINCT x)`) aggregates each distinct value once. An aggregate may be part of an
expression (`count(*) + 1`); outside its aggregates such an item reads only grouping keys,
constants and parameters. The details are on [Semantics](semantics.md#aggregation).

```rhai,graph=modern
g.gql("MATCH (p:person) RETURN count(*) AS n, sum(p.age) AS total, avg(p.age) AS mean, min(p.age) AS lo, max(p.age) AS hi").to_list()
// #{"hi": 35, "lo": 27, "mean": 30.75, "n": 4, "total": 123}
g.v().has_label("person").values("age").fold().project("n", "total", "mean", "lo", "hi").by(__.count(Scope.local)).by(__.sum(Scope.local)).by(__.mean(Scope.local)).by(__.min(Scope.local)).by(__.max(Scope.local)).to_list()
// the same values
g.gql("MATCH (s:software)<-[:created]-(p) RETURN s.name AS sw, count(p) AS authors ORDER BY sw").to_list()
// #{"authors": 3, "sw": "lop"}, #{"authors": 1, "sw": "ripple"}
g.v().has_label("software").group().by("name").by(__.in("created").count()).to_list()   // #{"lop": 3, "ripple": 1}
g.gql("MATCH (p:person)-[:created]->(s) RETURN p.name AS name, collect_list(s.name) AS sw ORDER BY name").to_list()
// #{"name": "josh", "sw": ["ripple", "lop"]}, #{"name": "marko", "sw": ["lop"]}, #{"name": "peter", "sw": ["lop"]}
g.gql("MATCH (n) RETURN count(n.age) AS ages, count(*) AS all, count(DISTINCT n.lang) AS langs").to_list()
// #{"ages": 4, "all": 6, "langs": 1}
```

With `OPTIONAL MATCH`, `count()` of an optional variable gives 0 where nothing matched; Gremlin
counts per vertex with `group()` or `project()`:

```rhai,graph=modern
g.gql("MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s) RETURN p.name AS name, count(s) AS n ORDER BY name").to_list()
// josh 2, marko 1, peter 1, vadas 0
g.v().has_label("person").project("name", "n").by("name").by(__.out("created").count()).order().by("name").to_list()
// the same rows
```

An aggregating `WITH` passes the groups on, and its `WHERE` filters them (SQL's `HAVING`):

```rhai,graph=modern
g.gql("MATCH (p:person)-[:created]->(s) WITH p, count(s) AS n WHERE n > 1 RETURN p.name AS name, n").to_list()   // #{"n": 2, "name": "josh"}
g.gql("MATCH (p:person) RETURN p.name, count(*) GROUP BY p.name")   // error: GROUP BY is not needed: grouping is implicit
g.gql("MATCH (p:person) WHERE count(p) > 1 RETURN p")   // error: an aggregate cannot be used in WHERE
g.gql("MATCH (p:person) RETURN sum(p.name) AS s")   // error: sum() takes numbers, not a string (22G03)
```

After an aggregation `ORDER BY` reads the returned columns only. The groups come out in an
unspecified order: write `ORDER BY`.

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
| `path_length(p)` | the number of edges of a path | `length(p)` |

Function names are case-insensitive. Every function returns `NULL` for a `NULL` argument and
fails for an argument of the wrong kind. `size(e)` of a group variable counts its edges. The
aggregates are listed under [Aggregation](#aggregation).

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
| `GROUP BY` | not needed: grouping is implicit ([Aggregation](#aggregation)); refused with a syntax error (`42001`) that says so |
| `NEXT` | `WITH`, or Gremlin steps after `gql()` |
| `OPTIONAL { ... }` / `OPTIONAL ( ... )` blocks | one `OPTIONAL MATCH` per optional part |
| quantified path patterns `((a)-->(b)){1,3}` | quantify a single edge, `(a)-->{1,3}(b)`, or Gremlin `repeat()` |
| `nodes(p)`, `relationships(p)` | `RETURN p`, or name the elements: `MATCH p = (a)-[e]->(b) RETURN a, e, b` |
| path search prefixes and match modes (`ANY SHORTEST`, `TRAIL`, `DIFFERENT EDGES` written out) | Gremlin `path()`, `repeat()` |
| `UNION`, `EXCEPT`, `INTERSECT`, `EXISTS { }`, `CALL { }` subqueries | Gremlin `union()`, `where()` |
| `FOR` (Cypher `UNWIND`) | Gremlin `unfold()` |
| list indexing and slicing `x[0]`, `x[1..2]` | Gremlin after `gql()` |
| `NULLS FIRST` / `NULLS LAST` | the fixed order: `NULL` last ascending, first descending |
| a query without `MATCH` (`RETURN 1`) | Gremlin `inject()` |
| `gql()` after other steps (`g.v().gql(..)`) | start with `g.gql(..)` and continue with Gremlin |

```rhai,graph=modern
g.gql("MATCH ((a)-[:knows]->(b)){1,3} RETURN b")   // error: quantified path patterns are not yet supported
g.gql("MATCH (a)-[:knows]->{1,3}(b) RETURN b.name AS b ORDER BY b").to_list()   // #{"b": "josh"}, #{"b": "vadas"}
g.gql("MATCH p = (a)-[:knows]->(b) RETURN nodes(p)")   // error: nodes() over a path is not yet supported
g.gql("MATCH p = (a)-[:knows]->(b) RETURN p ORDER BY b.name").select("p").unfold().to_list()   // v[1], e[2][1-knows->4], v[4], v[1], e[0][1-knows->2], v[2]
```
