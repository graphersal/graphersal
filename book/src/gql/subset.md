# The Supported Subset

This page lists what `gql()` accepts in this version, construct by construct, each with a
runnable example and, where it is natural, its Gremlin twin. Everything not listed here is an
error that says so ([Not supported yet](#not-supported-yet) at the end,
[Refused Constructs](refused.md) for what will never be supported).

## The shape of a query

```text
[ clauses, in any order and number:
  [OPTIONAL] MATCH <pattern> [, <pattern> ...] [WHERE <condition>]
  LET <name> = <expr> [, ...]
  FILTER [WHERE] <condition>
  FOR <name> IN <list> [WITH ORDINALITY <name> | WITH OFFSET <name>]
  UNWIND <list> AS <name>                     -- Cypher spelling of FOR
  WITH [DISTINCT] <item> [AS <name>], ...     -- or WITH *; Cypher
       [ORDER BY ...] [OFFSET <n>] [LIMIT <n>] [WHERE <condition>]
  [OPTIONAL] CALL [(<name>, ...) | (*)] { <query> }   -- a subquery run for each row
  INSERT <pattern> [, <pattern> ...]          -- Cypher spelling: CREATE; creates per row
  SET <item> [, ...]  /  REMOVE <item> [, ...] -- change properties and labels per row
  [DETACH | NODETACH] DELETE <expr> [, ...]   -- removes per row; NODETACH is the default
  MERGE [<name> =] <pattern>                  -- Cypher; finds or creates one pattern per row
       { ON CREATE SET <item> [, ...] | ON MATCH SET <item> [, ...] }   -- any number, any order ]
RETURN [DISTINCT] <item> [AS <name>], ...     -- or RETURN *; items may aggregate
                                              -- (mandatory unless the last clause writes;
                                              -- FINISH ends a query without a result)
[ORDER BY <expr> [ASC | DESC], ...]
[OFFSET <n>]                                  -- Cypher alias: SKIP
[LIMIT <n>]
[ UNION [ALL | DISTINCT] <clauses> RETURN ... [ORDER BY ...] [OFFSET ..] [LIMIT ..] ]   -- any number
[ NEXT <the same again: clauses, RETURN, UNION ...> ]                                   -- any number
[;]
```

A pattern is a path of node and edge patterns, optionally named (`p = (a)-->(b)`); an edge may
carry a quantifier (`-[:knows]->{1,3}`), and so may a parenthesized part of the path
(`((x)-[:knows]->(y)){1,3}`, a quantified path pattern). A query usually starts with `MATCH`, but it may start
with any clause, or with `RETURN` alone ([Queries without MATCH](#queries-without-match)).
Queries ending in `RETURN` combine with `UNION` and continue with `NEXT`
([UNION and NEXT](#union-and-next)); `NEXT` binds looser than `UNION`. A `CALL` runs a whole
query of that shape for each row ([CALL subqueries](#call-subqueries)). `INSERT` creates nodes
and edges once per row, `SET` / `REMOVE` change them, `DELETE` removes them and Cypher's `MERGE`
finds or creates them; a query whose last clause writes may end there and returns no rows
([Writing with GQL](writes.md)).

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

To repeat more than one edge, put the chain in parentheses and quantify it:
[a quantified path pattern](#quantified-path-patterns).

### Quantified path patterns

A parenthesized path pattern `( <chain> [WHERE <condition>] )` with a quantifier after its `)`
repeats the whole chain, its nodes, edges and conditions included:
`(a) ((x)-[:created]->(s)<-[:created]-(y)){1,2} (b)` walks from `a` to `b` over one or two
"created the same software" hops. It takes every quantifier of a quantified edge (`{m,n}`, `{n}`,
`{m,}`, `{,n}`, `*`, `+`) and also `?` (0 or 1 times); without a quantifier it matches exactly
once.

- **The outer nodes merge with the body.** The node written before the group is the first node of
  the first iteration, the node after it the last node of the last iteration, and consecutive
  iterations join end to start: `(a) ((x)-->(y)){1,3} (b)` binds `a` = the first `x` and `b` = the
  last `y`. Where the text has no node next to the group (at the start or end of the path, or
  between two groups), an anonymous one stands. After zero iterations (`{0,n}`, `*`, `?`) the two
  outer nodes are the same node.
- **Body variables are group variables.** With a quantifier other than `?`, every variable inside
  the body is, after the pattern, the list of its nodes or edges, one per iteration in walk
  order, like the variable of a quantified edge. Read the list (`size(x)`, `[n IN x | n.name]`,
  `RETURN x`), not a property of it: `x.name` outside the body is an error. With `?` or without a
  quantifier the body variables are single values; with `?` they are `NULL` when the part
  matched nothing ([Semantics](semantics.md#quantified-path-patterns)).
- **The body condition runs in every iteration.** The `WHERE` before the closing parenthesis, and
  the inline maps and `WHERE`s of the body's elements, test each iteration's elements, and read
  only the body's variables, parameters and values; test an outer variable in the `WHERE` of the
  `MATCH` clause.
- **Trail holds across iterations.** No edge is bound twice in one match, across the iterations,
  the body's edges and the other edges of the clause.

```rhai,graph=modern
g.gql("MATCH (a {name: 'marko'}) ((x)-[e:knows|created]->(y)){1,2} (b) RETURN b.name AS b, size(e) AS hops ORDER BY hops, b").to_list()
// josh 1, lop 1, vadas 1, lop 2, ripple 2
g.gql("MATCH (a {name: 'marko'}) ((x)-[:knows|created]->(y)){1,2} (b) RETURN [n IN x | n.name] AS xs, [n IN y | n.name] AS ys ORDER BY ys").to_list()
// #{"xs": ["marko"], "ys": ["josh"]}, #{"xs": ["marko", "josh"], "ys": ["josh", "lop"]}, #{"xs": ["marko", "josh"], "ys": ["josh", "ripple"]}, #{"xs": ["marko"], "ys": ["lop"]}, #{"xs": ["marko"], "ys": ["vadas"]}
g.gql("MATCH (a:person) ((x)-[:created]->(s)<-[:created]-(y)){1,2} (b) RETURN DISTINCT a.name AS a, b.name AS b ORDER BY a, b").to_list()
// josh/marko, josh/peter, marko/josh, marko/peter, peter/josh, peter/marko
g.gql("MATCH (a:person) ((x)-[:knows|created]-(y) WHERE y.age > x.age){1,3} (b) RETURN a.name AS a, b.name AS b, size(y) AS steps ORDER BY a, b").to_list()
// #{"a": "marko", "b": "josh", "steps": 1}, #{"a": "vadas", "b": "josh", "steps": 2}, #{"a": "vadas", "b": "marko", "steps": 1}
g.gql("MATCH (p:person {name: 'josh'}) ((x)-[:created]->(s) WHERE s.lang = 'java')? (q) RETURN s.name AS project, q.name AS last ORDER BY project").to_list()
// #{"last": "lop", "project": "lop"}, #{"last": "ripple", "project": "ripple"}, #{"last": "josh", "project": ()}
g.gql("MATCH ((x:person)-[:created]->(s) WHERE s.name = 'ripple') RETURN x.name AS who").to_list()   // #{"who": "josh"}
```

The Gremlin twin is a `repeat()` of the body's steps; the second-to-last query is a filter on the
body run in every iteration:

```rhai,graph=modern
g.v().has_label("person").as("a").repeat(__.out_e("created").in_v().in_e("created").out_v()).emit().times(2).simple_path().as("b").select("a", "b").by("name").dedup().to_list()
// the same six pairs (simple_path() stands in for trail here)
```

A quantified path pattern stands wherever a pattern does: in `MATCH` and `OPTIONAL MATCH`, in
`EXISTS { }`, `COUNT { }` and `COLLECT { }`, in a pattern comprehension and in a Cypher pattern
predicate (which names no new variable, so its body stays anonymous):

```rhai,graph=modern
g.gql("MATCH (p:person) RETURN p.name AS p, [(p) ((x)-[:created]->(s)<-[:created]-(y)){1,2} (q) | q.name] AS co ORDER BY p").to_list()
// josh [peter, marko], marko [peter, josh], peter [josh, marko], vadas []
g.gql("MATCH (p:person) WHERE EXISTS { (p) ((x)-[:knows]->(y)){1,3} (q:person {name: 'josh'}) } RETURN p.name AS p").to_list()   // #{"p": "marko"}
g.gql("MATCH (p:person) WHERE (p) (()-[:knows]->()-[:created]->()){1} RETURN p.name AS p").to_list()   // #{"p": "marko"}
```

Not yet supported inside the body (each refused with GQLSTATUS `0A000` and a help line naming
the rewrite): a nested parenthesized path pattern, a quantified edge, a path variable (`p =` goes
on the whole path), a path mode or search prefix, several comma-separated patterns or a union
(`|`), a body without an edge, a variable written twice in one body, a variable bound outside
the body written again inside it, a condition reading an outer variable, and a subquery. A
variable inside a quantified body must be a new name (`42002`).

```rhai,graph=modern
g.gql("MATCH (a) (((x)-->(y)){2})+ (b) RETURN b")   // error: a parenthesized path pattern nested in another one is not yet supported
g.gql("MATCH (a) ((x)-->(y)-->(z)){1,3} (b) RETURN b.name AS b ORDER BY b").to_list()   // #{"b": "lop"}, #{"b": "ripple"}
g.gql("MATCH (a) ((x)-->(y))+ (b) RETURN x.name")   // error: `x` is a group variable of the quantified path pattern
```

### Path variables

`p = <path pattern>` names the whole path. `RETURN p` returns it as a path value (the
alternating vertices and edges, the same value as Gremlin's `path()`;
[Result Shape](result_shape.md#paths)), and `path_length(p)` (Cypher `length(p)`) is its number
of edges. A quantified edge or path pattern contributes all its edges and the vertices in
between.

```rhai,graph=modern
g.gql("MATCH p = (a {name: 'marko'})-[:knows]->(b)-[:created]->(s) RETURN p, path_length(p) AS len ORDER BY s.name").to_list()
// #{"len": 2, "p": [v[1], e[2][1-knows->4], v[4], e[3][4-created->3], v[3]]}, #{"len": 2, "p": [v[1], e[2][1-knows->4], v[4], e[4][4-created->5], v[5]]}
g.v().has("name", "marko").out_e("knows").in_v().out_e("created").in_v().path().to_list()
// [v[1], e[2][1-knows->4], v[4], e[4][4-created->5], v[5]], [v[1], e[2][1-knows->4], v[4], e[3][4-created->3], v[3]]
g.gql("MATCH p = (a {name: 'marko'})->{1,2}(b) RETURN b.name AS b, length(p) AS len ORDER BY len, b").to_list()
// josh 1, lop 1, vadas 1, lop 2, ripple 2
```

A path is a value: it compares (`p = q`), sorts, groups and goes into `collect_list()`. The
Cypher functions `nodes(p)` and `relationships(p)` return its vertices and its edges as lists
([Lists, elements and paths](#lists-elements-and-paths)):

```rhai,graph=modern
g.gql("MATCH p = (a {name: 'marko'})-[:knows]->(b) RETURN nodes(p) AS n, relationships(p) AS r ORDER BY b.name").to_list()
// #{"n": [v[1], v[4]], "r": [e[2][1-knows->4]]}, #{"n": [v[1], v[2]], "r": [e[0][1-knows->2]]}
g.v().has("name", "marko").out_e("knows").in_v().path().to_list()
// [v[1], e[2][1-knows->4], v[4]], [v[1], e[0][1-knows->2], v[2]]
```

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

## EXISTS and pattern predicates

An `EXISTS` subquery tests whether a pattern matches: it is TRUE when its body has at least one
match for the row and FALSE otherwise, never `NULL`. Three forms mean the same:

```text
EXISTS { MATCH <pattern> [, <pattern> ...] [WHERE <condition>] [MATCH ...] }
EXISTS { <pattern> [, <pattern> ...] [WHERE <condition>] }
EXISTS ( ... )                                -- either body in parentheses
(a)-[:knows]->(b)                             -- Cypher pattern predicate: a pattern where an expression stands
```

The body reads the variables bound before it (they are **imported**: `p` inside is the row's
`p`), and the variables it binds itself stay inside it. A pattern predicate is
`EXISTS { <pattern> }` written without the keyword; since it stands where any expression may, it
must not name a new variable (write `EXISTS { MATCH .. }` to name one). The Gremlin twins are
`where(__..)` and `not(__..)`, and the "who has no ..." question runs exactly as its twin:

```rhai,graph=modern
g.gql("MATCH (p:person) WHERE NOT EXISTS { (p)-[:created]->() } RETURN p.name AS name ORDER BY name").to_list()   // #{"name": "vadas"}
g.gql("MATCH (p:person) WHERE NOT (p)-[:created]->() RETURN p.name AS name ORDER BY name").to_list()   // #{"name": "vadas"}
g.v().has_label("person").not(__.out("created")).values("name").to_list()   // "vadas"
g.gql("MATCH (p:person) WHERE EXISTS { MATCH (p)-[:created]->(s) WHERE s.name = 'ripple' } RETURN p.name AS name").to_list()   // #{"name": "josh"}
g.v().has_label("person").where(__.out("created").has("name", "ripple")).values("name").to_list()   // "josh"
g.gql("MATCH (a:person), (b:person) WHERE (a)-[:knows]->(b) RETURN a.name AS a, b.name AS b ORDER BY b").to_list()
// #{"a": "marko", "b": "josh"}, #{"a": "marko", "b": "vadas"}
g.gql("MATCH (p:person) WHERE EXISTS (MATCH (p)-[:knows]->(f) MATCH (f)-[:created]->(:software {name: 'ripple'})) RETURN p.name AS name").to_list()   // #{"name": "marko"}
```

`EXISTS` stands anywhere an expression does: in `WHERE` and `FILTER`, in the inline `WHERE` of a
node or edge pattern, under `OR`, `XOR` and `CASE`, in a `RETURN` or `WITH` column, an
`ORDER BY` key, a grouping key or an aggregate's argument, a `FOR` list. It composes with
three-valued logic like any boolean ([Semantics](semantics.md#exists-and-pattern-predicates)).

```rhai,graph=modern
g.gql("MATCH (p:person) WHERE p.age > 30 OR (p)<-[:knows]-() RETURN p.name AS name ORDER BY name").to_list()   // josh, peter, vadas
g.gql("MATCH (p:person) RETURN p.name AS name, EXISTS { (p)-[:knows]->() } AS knows ORDER BY knows DESC, name").to_list()
// #{"knows": true, "name": "marko"}, #{"knows": false, "name": "josh"}, ... peter, vadas
g.gql("MATCH (p:person) RETURN EXISTS { (p)-[:created]->() } AS creator, count(*) AS n ORDER BY creator").to_list()
// #{"creator": false, "n": 1}, #{"creator": true, "n": 3}
g.gql("MATCH (p:person WHERE NOT (p)-[:created]->())-[:knows]-(f) RETURN p.name AS p, f.name AS f").to_list()   // #{"f": "marko", "p": "vadas"}
```

The body holds `MATCH` clauses with their `WHERE` (and nested subqueries), or a **whole
query**: any clauses (`OPTIONAL MATCH`, `WITH`, `LET`, `FILTER`, `FOR`, `CALL`), `UNION` and
`NEXT`, ending in `RETURN`. `EXISTS` is then TRUE when that query returns at least one row, and
its `RETURN` items are still evaluated (a failing expression there fails the query). Whole-query
bodies nest at most 16 levels, `CALL` bodies included. A query holds at most 64 subqueries,
[pattern comprehensions](#map-projections-and-comprehensions), `COUNT { }` and `COLLECT { }`
included ([Errors](errors.md#subqueries)).

```rhai,graph=modern
g.gql("MATCH (p:person) WHERE EXISTS { MATCH (p)-[:created]->(s) WITH count(s) AS c WHERE c > 1 RETURN c } RETURN p.name AS name").to_list()   // #{"name": "josh"}
g.gql("MATCH (p:person) WHERE EXISTS { MATCH (p)-[:created]->(s) RETURN s UNION MATCH (p)-[:knows]->(s) RETURN s } RETURN p.name AS name ORDER BY name").to_list()   // josh, marko, peter
g.gql("MATCH (p:person) RETURN p.name AS name, (p)-[:knows]->(q) AS k")   // error: the pattern predicate introduces the new variable `q`
g.gql("MATCH (p:person) WHERE EXISTS { MATCH (p)-[:created]->(s) } RETURN s.name AS s")   // error: `s` is bound inside a subquery
```

### COUNT and COLLECT

`COUNT { }` counts and `COLLECT { }` collects what a subquery finds for each row. They are not
GQL: like the [list predicates](#list-predicates-and-reduce) they are openCypher compatibility
forms, accepted with their Neo4j Cypher 25 meaning ([Cypher Aliases](aliases.md#count-and-collect)).
Their bodies are those of `EXISTS`, and they stand anywhere an expression does:

```text
COUNT { <pattern> [WHERE <condition>] }       -- the number of matches
COUNT { MATCH ... }                           -- the same
COUNT { <query ending in RETURN> }            -- the number of rows the query returns
COLLECT { <query ending in RETURN <one item>> }   -- the list of that item, one entry per row
```

`COUNT { }` is an integer, 0 when nothing matches, never `NULL`; `COUNT { .. } > 0` keeps the
rows `EXISTS` keeps. `COLLECT { }` is a list in the order the body returns its rows (its own
`ORDER BY` decides it), `[]` when there are none, with `NULL` items kept. Each row runs the body
once, like the Gremlin twins `project(..).by(__.out("created").count())` and
`by(__.out("created").values("name").fold())`:

```rhai,graph=modern
g.gql("MATCH (p:person) RETURN p.name AS name, COUNT { (p)-[:created]->() } AS n ORDER BY name").to_list()   // josh 2, marko 1, peter 1, vadas 0
g.v().has_label("person").order().by("name").project("name", "n").by("name").by(__.out("created").count()).to_list()   // the same rows
g.gql("MATCH (p:person) RETURN p.name AS name, COLLECT { MATCH (p)-[:created]->(s) RETURN s.name ORDER BY s.name } AS sw ORDER BY name").to_list()
// josh [lop, ripple], marko [lop], peter [lop], vadas []
g.gql("MATCH (p:person {name: 'marko'}) RETURN COLLECT { MATCH (p)-[:knows]->(f) RETURN f.lang } AS l").to_list()   // #{"l": [(), ()]}: NULLs kept
g.gql("MATCH (p:person) WHERE COUNT { (p)-[:created]->() } > 1 RETURN p.name AS name").to_list()   // #{"name": "josh"}
g.gql("MATCH (p:person) RETURN p.name AS name, COUNT { MATCH (p)-[:created]->(s) RETURN s.lang AS l UNION MATCH (p)-[:knows]->(f) RETURN f.name AS l } AS n ORDER BY name").to_list()
// josh 1 (java once), marko 3, peter 1, vadas 0
```

`COLLECT { }` needs a body ending in a `RETURN` of exactly one item. When a `COUNT { }` body
is one query whose final `RETURN` only projects (no aggregate, `DISTINCT`, `ORDER BY` or
`UNION`), its rows are counted without evaluating the `RETURN` items, so an expression there
that would fail raises no error ([Semantics](semantics.md#count-and-collect)).

```rhai,graph=modern
g.gql("MATCH (p:person) RETURN COLLECT { MATCH (p)-[:knows]->(f) RETURN f.name, f.age } AS l")   // error: COLLECT { } needs a body that ends in a RETURN of exactly one column
g.gql("MATCH (p:person) RETURN p.name AS name, COUNT { MATCH (p)-[:created]->(s) RETURN 1 / 0 AS x } AS n ORDER BY name").to_list()   // josh 2, marko 1, peter 1, vadas 0: 1 / 0 never runs
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

## FOR and UNWIND

`FOR x IN list` gives one row per item of the list, with the item bound to `x`; Cypher's
`UNWIND list AS x` is the same clause ([Cypher Aliases](aliases.md)). The list is any expression
over the row before the `FOR`: a literal, a `$parameter`, a property, a `collect_list()` column
of a `WITH`, `nodes(p)`. Duplicates and `NULL` items give rows like any other item; a list
inside the list stays one item. `WITH ORDINALITY i` also binds the item's position from 1,
`WITH OFFSET i` from 0 (only after `FOR`). Gremlin's twin is `unfold()`.

```rhai,graph=modern
g.gql("FOR x IN [1, 2, 2] RETURN x ORDER BY x").to_list()   // #{"x": 1}, #{"x": 2}, #{"x": 2}
g.inject([1, 2, 2]).unfold().order().to_list()   // 1, 2, 2
g.gql("UNWIND [1, 2, 2] AS x RETURN x ORDER BY x").to_list()   // the same rows
g.gql("FOR x IN [10, 20] WITH ORDINALITY i RETURN x, i ORDER BY i").to_list()   // #{"i": 1, "x": 10}, #{"i": 2, "x": 20}
g.gql("FOR x IN [10, 20] WITH OFFSET i RETURN x, i ORDER BY i").to_list()   // #{"i": 0, "x": 10}, #{"i": 1, "x": 20}
g.gql("FOR x IN [1, 2] FOR y IN [x, x * 10] RETURN x, y ORDER BY x, y").to_list()   // 1 1, 1 10, 2 2, 2 20
```

The most frequent use is "for each value in a list, find its vertex": an item compared in a
property map or a `WHERE` equality. When an index or the element id serves that equality, each
item is a direct lookup instead of a scan ([Plans and Performance](plans.md#for-and-the-correlated-lookup)).
A name without a vertex gives no row:

```rhai,graph=modern
g.gql("FOR n IN $names WITH ORDINALITY i MATCH (p:person {name: n}) RETURN i, n, p.age AS age ORDER BY i", #{names: ["vadas", "marko", "nobody"]}).to_list()
// #{"age": 27, "i": 1, "n": "vadas"}, #{"age": 29, "i": 2, "n": "marko"}
g.gql("FOR id IN $ids MATCH (n) WHERE element_id(n) = id RETURN id, n.name AS name ORDER BY id", #{ids: ["1", "3", "42"]}).to_list()
// #{"id": "1", "name": "marko"}, #{"id": "3", "name": "lop"}
```

An item that is a vertex continues a pattern by its name, like a vertex column of a `WITH`
(an item of another kind gives no row there):

```rhai,graph=modern
g.gql("MATCH (p:person) WITH collect_list(p) AS ps FOR p IN ps MATCH (p)-[:created]->(s) RETURN p.name AS name, s.name AS sw ORDER BY name, sw").to_list()
// josh lop, josh ripple, marko lop, peter lop
g.gql("MATCH p = (a {name: 'marko'})-[:knows]->(b) FOR n IN nodes(p) MATCH (n)-[:created]->(s) RETURN DISTINCT n.name AS who, s.name AS sw ORDER BY who, sw").to_list()
// josh lop, josh ripple, marko lop
```

A `NULL` list gives no row and a value that is not a list gives one row of that value
([Semantics](semantics.md#for-null-and-values-that-are-not-lists)). The item and the position
are new names: a name already in scope, or the same name for both, is an error.

## Queries without MATCH

A query may start with `FOR`, `UNWIND`, `LET`, `WITH`, `FILTER` or `RETURN`. It then starts from
one empty row (Gremlin's `inject(0)`): `RETURN` alone returns one row, `FILTER` keeps or drops
it, `FOR` turns it into one row per item, and a later `MATCH` runs once for each row.

```rhai,graph=modern
g.gql("RETURN 1 + 2 AS three, upper('gql') AS up").to_list()   // #{"three": 3, "up": "GQL"}
g.gql("RETURN count(*) AS c").to_list()   // #{"c": 1}: the one empty row
g.gql("WITH 2 AS n RETURN n * n AS sq").to_list()   // #{"sq": 4}
g.gql("FILTER $on RETURN 1 AS x", #{on: false}).to_list()   // no row
g.gql("LET k = 30 MATCH (p:person) FILTER p.age > k RETURN p.name AS name ORDER BY name").to_list()   // #{"name": "josh"}, #{"name": "peter"}
```

## RETURN

`RETURN` is mandatory in a query that only reads (a query whose last clause writes may end
there, and `FINISH` ends any query without a result; [Writing with GQL](writes.md)). It builds
one map per row: `RETURN expr AS name, ...`, `RETURN *` (every
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
keys**: one row per distinct combination of their values (grouping is implicit, as in Cypher).
Without keys the whole input is one group. ISO GQL's `GROUP BY` may follow the `RETURN` items: it
must list exactly the items without an aggregate (by column name, or by repeating the item's
expression), changes nothing in the result, and `GROUP BY ()` is the single group of an
all-aggregate `RETURN`; without any aggregate it gives one row per group, like `DISTINCT`. After
`WITH` (Cypher) `GROUP BY` is an error that explains implicit grouping.

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
g.gql("MATCH (p:person)-[:created]->(s) RETURN p.name AS name, count(s) AS n GROUP BY name").to_list()   // the same rows as without GROUP BY
g.gql("MATCH (p:person) WITH p.name AS n, count(*) AS c GROUP BY n RETURN n, c")   // error: GROUP BY is not needed: grouping is implicit
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

## UNION and NEXT

`UNION` combines the rows of queries that each end in `RETURN`. Every operand returns the same
column names, in any order; the result has the column order of the first operand. `UNION` (also
written `UNION DISTINCT`) removes duplicate rows with GQL's equality (`1` and `1.0` are one
value), `UNION ALL` keeps them; one chain uses one kind. Each operand is a query of its own:
its own variables, and its own `ORDER BY`, `OFFSET` and `LIMIT`, which apply to that operand
only. Gremlin's twin is `union()`, followed by `dedup()` for `UNION`:

```rhai,graph=modern
g.gql("MATCH (p:person) WHERE p.age > 30 RETURN p.name AS name UNION MATCH (s:software) RETURN s.name AS name").to_list()   // josh, peter, lop, ripple (in any order)
g.union(__.V().has("person", "age", P.gt(30)), __.V().has_label("software")).values("name").to_list()   // "josh", "peter", "lop", "ripple"
g.gql("MATCH (p:person) RETURN p.name AS name UNION ALL MATCH (p:person)-[:created]->() RETURN p.name AS name").count().next()   // 8: duplicates kept
g.gql("RETURN 1 AS a, 2 AS b UNION RETURN 2 AS b, 1 AS a").to_list()   // #{"a": 1, "b": 2}: one row, the first operand's order
g.gql("MATCH (p:person) RETURN p.name AS name ORDER BY p.age DESC LIMIT 1 UNION ALL MATCH (s:software) RETURN s.name AS name ORDER BY name LIMIT 1").to_list()   // peter, lop: per operand
g.gql("RETURN 1 AS x UNION RETURN 2 AS y")   // error: the operands of UNION must return the same column names
g.gql("RETURN 1 AS x UNION RETURN 2 AS x UNION ALL RETURN 3 AS x")   // error: UNION ALL is mixed with UNION in one query
```

`NEXT` continues after a `RETURN`: the rows that `RETURN` builds are the input of the next
statement, which sees only its columns, by name (`RETURN *` passes every variable on). It may
start with any clause: `NEXT RETURN`, `NEXT FILTER`, `NEXT MATCH` continuing from an element
column. `NEXT` binds looser than `UNION`, so a `NEXT` after a union sorts, filters, pages or
aggregates the whole union, which no operand's `ORDER BY` can:

```rhai,graph=modern
g.gql("MATCH (p:person) RETURN p.name AS name UNION MATCH (s:software) RETURN s.name AS name NEXT RETURN name ORDER BY name LIMIT 3").to_list()   // josh, lop, marko
g.gql("MATCH (p:person) RETURN p.name AS name, p.age AS age NEXT FILTER age > 30 RETURN name ORDER BY name").to_list()   // #{"name": "josh"}, #{"name": "peter"}
g.gql("MATCH (p:person) RETURN p NEXT MATCH (p)-[:created]->(s) RETURN p.name AS who, s.name AS sw ORDER BY who, sw").to_list()   // josh lop, josh ripple, marko lop, peter lop
g.gql("MATCH (p:person) RETURN p.name AS name, p.age AS age NEXT RETURN count(*) AS n, avg(age) AS mean").to_list()   // #{"mean": 30.75, "n": 4}
g.gql("MATCH (p:person) RETURN p.name AS name NEXT RETURN p.age")   // error: the variable `p` is not in scope: the RETURN before NEXT does not return it
```

A `NEXT` after a plain query is the Cypher `WITH` of the same items: `q1 RETURN a, b NEXT q2`
gives the rows of `q1 WITH a, b q2` ([Cypher Aliases](aliases.md#the-cypher-clause-with)).
`NEXT YIELD` is not supported yet.

## CALL subqueries

`CALL { }` runs a whole query (any clauses, `UNION`, `NEXT`, ending in `RETURN`) **once for
each incoming row**, and joins the row with each row the body returns: the columns of the body's
`RETURN` become new variables of the query. Three things make it more than a `MATCH`:

- the body's aggregates, `DISTINCT`, `ORDER BY` and `LIMIT` apply to **each row's** records:
  a count per row, the top k per group;
- a row whose body returns no row is dropped, `OPTIONAL CALL` keeps it with the new variables
  `NULL` (an aggregating body returns one row anyway: `count` is 0 there);
- a `UNION` or `NEXT` inside the body works per row too.

```text
CALL (a, b) { <query> }       -- imports a and b (GQL and Cypher 25)
CALL (*) { <query> }          -- imports every variable in scope
CALL () { <query> }           -- imports nothing
CALL { <query> }              -- imports nothing, unless it starts with WITH a, b (Cypher's importing WITH)
OPTIONAL CALL ...             -- keeps a row without results, with NULL columns
```

```rhai,graph=modern
g.gql("MATCH (p:person) CALL (p) { MATCH (p)-[e:created]->(s) RETURN s.name AS top ORDER BY e.weight DESC LIMIT 1 } RETURN p.name AS who, top ORDER BY who").to_list()
// #{"top": "ripple", "who": "josh"}, #{"top": "lop", "who": "marko"}, #{"top": "lop", "who": "peter"}: vadas has no row
g.v().has_label("person").as("p").flat_map(__.out_e("created").order().by("weight", Order.desc).limit(1).in_v()).project("who", "top").by(__.select("p").values("name")).by("name").to_list()   // the same three rows
g.gql("MATCH (p:person) OPTIONAL CALL (p) { MATCH (p)-[:created]->(s) RETURN s.name AS sw ORDER BY sw LIMIT 1 } RETURN p.name AS who, sw ORDER BY who").to_list()
// josh lop, marko lop, peter lop, vadas ()
g.gql("MATCH (p:person) CALL (p) { MATCH (p)-[:knows]->(f) RETURN count(f) AS friends } RETURN p.name AS who, friends ORDER BY who").to_list()   // josh 0, marko 2, peter 0, vadas 0
g.gql("MATCH (p:person) CALL (p) { MATCH (p)-[:knows]->(f) RETURN f.name AS other UNION MATCH (p)-[:created]->(s) RETURN s.name AS other } RETURN p.name AS who, other ORDER BY who, other").to_list()
// josh lop, josh ripple, marko josh, marko lop, marko vadas, peter lop
g.gql("MATCH (p:person) CALL { WITH p MATCH (p)-[:created]->(s) RETURN s.name AS sw } RETURN p.name AS who, sw ORDER BY who, sw").to_list()   // Cypher's importing WITH: josh lop, josh ripple, marko lop, peter lop
```

The body reads only the variables it imports: in `CALL { MATCH (p)-->(s) .. }` without a scope
clause, `p` is a new variable of the body, not the outer one. A body that imports nothing still
runs once per row ([Semantics](semantics.md#call-subqueries)). The returned names must be new,
and a body without `RETURN` (a unit subquery) must end in a write
([Inside `CALL`](writes.md#once-per-row)): a body that only reads must return something;
`IN TRANSACTIONS` is [refused](refused.md). A later clause continues from a returned element as
from a `WITH` column:

```rhai,graph=modern
g.gql("CALL { MATCH (s:software) RETURN s } MATCH (s)<-[:created]-(p) RETURN s.name AS sw, count(p) AS n ORDER BY sw").to_list()   // lop 3, ripple 1
g.gql("MATCH (p:person) CALL (p) { MATCH (p)-[:knows]->(f) RETURN f AS p } RETURN p")   // error: the CALL body returns `p`, which is already defined outside it
g.gql("MATCH (p:person) CALL (p) { MATCH (p)-[:created]->(s) } RETURN p")   // error: a CALL { } body without RETURN only makes sense for writes
```

## INSERT

`INSERT <pattern>, ...` (Cypher `CREATE`) creates the pattern's new nodes and edges once for
each row that reaches it; a variable bound before connects an existing node. Edges need a
direction and one label, node labels are a conjunction (`:A&B`, Cypher `:A:B`). The full rules
are on [Writing with GQL](writes.md).

```rhai,graph=modern
g.gql("MATCH (p:person {name: 'marko'}) INSERT (p)-[:likes]->(f:fruit {name: 'apple'}) RETURN f.name AS fruit").to_list()   // #{"fruit": "apple"}
g.gql("FOR n IN ['ann', 'bob'] CREATE (:person {name: n})").to_list()   // no rows
```

## SET and REMOVE

`SET <item>, ...` changes the elements of each row that reaches it, items left to right: a
property (`SET n.k = expr`), all properties (`SET n = {..}`; Cypher's merge `SET n += {..}`) or
labels of a node (`SET n:A`, `SET n IS A`). `REMOVE <item>, ...` removes a property
(`REMOVE n.k`) or labels (`REMOVE n:A`). A `NULL` value removes the property, a `NULL` target
is skipped. The full rules are on [Writing with GQL](writes.md#set).

```rhai,graph=modern
g.gql("MATCH (p:person {name: 'marko'}) SET p.age = p.age + 1, p:admin RETURN p.age AS age, labels(p) AS labels").to_list()   // #{"age": 30, "labels": ["person", "admin"]}
g.gql("MATCH (p:person {name: 'marko'}) REMOVE p.age RETURN keys(p) AS keys").to_list()   // #{"keys": ["name"]}
```

## DELETE

`[DETACH | NODETACH] DELETE <expr>, ...` removes the nodes, edges, paths and lists of them its
expressions give, for each row. `DETACH DELETE` removes a node with its edges; a plain `DELETE`
(the default, also written `NODETACH DELETE`) fails the query (`G1001`) for a node that still
has edges the same clause does not delete. A `NULL` target is skipped; reading or returning a
deleted element later in the query is an error. The full rules are on [Writing with
GQL](writes.md#delete).

```rhai,graph=modern
g.gql("MATCH (p:person {name: 'vadas'}) WITH p, p.name AS name DETACH DELETE p RETURN name").to_list()   // #{"name": "vadas"}
g.gql("MATCH (a {name: 'marko'})-[e:knows]->() DELETE e RETURN count(e) AS deleted").to_list()   // #{"deleted": 2}
```

## MERGE

`MERGE [p =] <pattern> [ON CREATE SET <items>] [ON MATCH SET <items>]` (Cypher, a
[compatibility form](aliases.md#merge)) looks for its one pattern on each row: every match is a
row, and when nothing matches, the whole pattern is created, reusing the nodes bound before.
Each row sees what the rows before it created. `ON CREATE SET` and `ON MATCH SET` hold `SET`
items for the created or the found elements. A `NULL` property value in the pattern fails the
query (`22004`). The full rules are on [Writing with GQL](writes.md#merge).

```rhai,graph=modern
g.gql("FOR n IN ['ann', 'ann'] MERGE (p:person {name: n}) ON CREATE SET p.new = true RETURN count(DISTINCT p) AS people").to_list()   // #{"people": 1}
g.gql("MATCH (a:person {name: 'marko'}), (b:person {name: 'josh'}) MERGE (a)-[e:knows]->(b) RETURN e.weight AS weight").to_list()   // #{"weight": 1.0}: found
```

## Expressions

| Kind | Forms |
|---|---|
| literals | integers (`42`, `1_000`, `0x1F`, `0o17`, `0b101`), floats (`1.5`, `.5`, `1.5e3`), strings (`'...'` or `"..."`), `TRUE`, `FALSE`, `NULL`, lists `[1, 2]`, maps (records) `{k: v}`, also written `RECORD {k: v}` |
| parameters | `$name` ([Parameters](#parameters)) |
| property access | `n.name`, `m.key` on a map |
| comparison | `=`, `<>` (Cypher `!=`), `<`, `<=`, `>`, `>=`; not chainable (`a < b < c` is an error) |
| logic | `AND`, `OR`, `XOR`, `NOT` |
| predicates | `IS [NOT] NULL`, `IN [list]`, `STARTS WITH`, `ENDS WITH`, `CONTAINS`, `IS [NOT] LABELED`, `IS [NOT] SOURCE OF e`, `IS [NOT] DESTINATION OF e` |
| arithmetic | `+`, `-`, `*`, `/`, `%`, unary `-` and `+` |
| concatenation | `\|\|` on two strings or two lists (Cypher `+`) |
| conditional | simple `CASE x WHEN v THEN r ... ELSE d END` and searched `CASE WHEN cond THEN r ... END` |
| conversion | `CAST(x AS INTEGER \| FLOAT \| STRING \| BOOLEAN)` ([Conversions](#conversions)) |
| function calls | `name(args)` ([Functions](#functions)) |
| map projection | `p {.name, .*, f, key: expr}` (Cypher, [below](#map-projections-and-comprehensions)) |
| list comprehension | `[x IN list WHERE cond \| expr]`, either part optional (Cypher) |
| pattern comprehension | `[(p)-[:knows]->(f) WHERE cond \| expr]` (Cypher) |

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
g.gql("MATCH (n:person) RETURN RECORD {name: n.name, age: n.age} AS m ORDER BY n.name LIMIT 1").to_list()
// #{"m": #{"age": 32, "name": "josh"}}
```

`RECORD {k: v}` is the ISO record constructor with its keyword written out; it builds the same map
as `{k: v}`. The word is contextual, not reserved: `RECORD` (any case) followed by `{` and then
`}` or `name:` is the constructor; anywhere else `record` is an ordinary variable, also before a
map projection such as `record {.name}`. A variable named `record` whose projection is empty or
starts with a `key: expr` item is back-quoted: `` `record` {k: 1} ``.

Integer arithmetic stays integer (`/` truncates toward zero, `%` takes the sign of the
dividend); a float operand makes the result a float. An overflow or a division by zero is an
error, never a wrapped or infinite value ([Errors](errors.md#errors-while-the-query-runs)).

## Map projections and comprehensions

Three Cypher forms build maps and lists inside an expression. They are not GQL; `gql()` accepts
them with their Neo4j meaning because Cypher users and LLMs write them all the time
([Cypher Aliases](aliases.md#map-projections-and-comprehensions)). Together they give a
JSON-shaped answer: one row per entity, its related data nested inside.

```text
v {.key, .*, x, key: expr}                    -- map projection of a vertex, an edge or a map
[x IN list]  [x IN list WHERE cond]  [x IN list | expr]  [x IN list WHERE cond | expr]
[(a)-[:R]->(b) WHERE cond | expr]             -- pattern comprehension, also [p = (a)-->(b) | p]
```

**Map projection.** `v {...}` builds a map from the variable `v`: `.key` is its property (`NULL`
when missing, the key kept), `.*` every stored property (no id, no labels), `x` the entry `x: x`,
and `key: expr` any value, all in the written order. Its Gremlin twins are `project()` and
`value_map().by(unfold())`; the GQL form is the record constructor `{name: p.name}`, which
differs only for a `NULL` variable ([Semantics](semantics.md#map-projections-and-comprehensions)):

```rhai,graph=modern
g.gql("MATCH (p:person) RETURN p {.name, .age, decade: p.age / 10} AS m ORDER BY p.name LIMIT 2").to_list()
// #{"m": #{"age": 32, "decade": 3, "name": "josh"}}, #{"m": #{"age": 29, "decade": 2, "name": "marko"}}
g.gql("MATCH (p:person {name: 'marko'}) RETURN p {.*} AS m").to_list()   // #{"m": #{"age": 29, "name": "marko"}}
g.v().has("name", "marko").value_map().by(__.unfold()).to_list()   // #{"age": 29, "name": "marko"}
g.gql("MATCH (p:person {name: 'marko'})-[e:knows]->(f) RETURN e {.*, to: f.name} AS k ORDER BY k.to").to_list()
// #{"k": #{"to": "josh", "weight": 1.0}}, #{"k": #{"to": "vadas", "weight": 0.5}}
```

**List comprehension.** `[x IN list WHERE cond | expr]` walks the list in its order, keeps the
items for which `cond` is TRUE and maps each kept item to `expr`; leave out `WHERE` to keep every
item, `| expr` to keep the item itself. `x` is a new name that exists only inside the brackets.
The Gremlin twin is `local(__.unfold().is(..).fold())`:

```rhai,graph=modern
g.gql("RETURN [x IN range(1, 5) WHERE x % 2 = 1] AS odd, [x IN range(1, 3) | x * x] AS sq, [x IN [1, 2] | [y IN [10, 20] | x * y]] AS grid").to_list()
// #{"grid": [[10, 20], [20, 40]], "odd": [1, 3, 5], "sq": [1, 4, 9]}
g.inject([1, 2, 3, 4, 5]).local(__.unfold().is(P.gt(2)).fold()).to_list()   // [3, 4, 5]
g.gql("MATCH p = (a {name: 'marko'})-[:knows|created]->{2}(b) RETURN [x IN nodes(p) | x.name] AS names ORDER BY b.name").to_list()
// #{"names": ["marko", "josh", "lop"]}, #{"names": ["marko", "josh", "ripple"]}
g.gql("MATCH (p:person) RETURN p.name AS name, [x IN [p.age, p.age + 10] WHERE x > 30] AS l ORDER BY name LIMIT 2").to_list()
// #{"l": [32, 42], "name": "josh"}, #{"l": [39], "name": "marko"}
```

A `|` inside the list or the `WHERE` part ends that part, so a label alternative there needs
parentheses: `[x IN l WHERE x:(A|B) | x.name]`.

**Pattern comprehension.** `[(a)-[:R]->(b) WHERE cond | expr]` is the list of `expr` for every
match of the pattern on the row: `[]` when nothing matches, never `NULL`. The pattern has at
least one edge; it reads the variables bound before it (they are imported, like in
[`EXISTS`](#exists-and-pattern-predicates)), and the names it binds itself (`b` here, or a path
variable `p = ...`) exist only inside the brackets. It is a subquery: the Gremlin twin is
`project(..).by(__.out("knows").values("name").fold())`, and the items come in no defined order.

```rhai,graph=modern
g.gql("MATCH (p:person {name: 'marko'}) RETURN [(p)-[:knows]->(f) | f.name] AS friends").to_list()   // #{"friends": ["josh", "vadas"]}, in any order
g.v().has("name", "marko").project("friends").by(__.out("knows").values("name").fold()).to_list()   // #{"friends": ["josh", "vadas"]}
g.gql("MATCH (s:software {name: 'ripple'}) RETURN [(s)<-[:created]-(p) WHERE p.age > 30 | p.name] AS older").to_list()   // #{"older": ["josh"]}
g.gql("MATCH (p:person) WHERE size([(p)-[:created]->(s) | s]) >= 2 RETURN p.name AS name").to_list()   // #{"name": "josh"}
g.gql("MATCH (p:person {name: 'marko'}) RETURN [q = (p)-[:knows]->() | path_length(q)] AS l").to_list()   // #{"l": [1, 1]}
```

Together, one row per person with its friends and projects nested inside, the shape a JSON API
or an LLM wants ([Result Shape](result_shape.md#nested-maps-and-lists)):

```rhai,graph=modern
g.gql("MATCH (p:person) RETURN p {.name, friends: [(p)-[:knows]->(f) | f.name], projects: [(p)-[:created]->(s) | s {.name, .lang}]} AS person ORDER BY person.name").to_json()
// [{"person": {"name": "josh", "friends": [], "projects": [{"name": "ripple", "lang": "java"}, ...]}}, ...]
```

All three stand anywhere an expression does (`WHERE`, `RETURN`, `WITH`, `LET`, `FILTER`, `FOR`,
`ORDER BY`, an aggregate's argument) and nest in each other. Their rules:

- A comprehension variable must be a **new name**: one that is in scope, or that the query uses
  anywhere else, is an error ([Semantics](semantics.md#map-projections-and-comprehensions)).
- No aggregate inside the `WHERE` or `|` part (aggregate in a `WITH` first, or aggregate the list:
  `size([..])`).
- A list-comprehension variable cannot be read inside an `EXISTS` or a pattern comprehension in
  its `WHERE` or `|` part yet ([Not supported yet](#not-supported-yet)).
- A pattern comprehension counts as a subquery: a query holds at most 64 subqueries
  ([Errors](errors.md#subqueries)).
- The list predicates `all()`, `any()`, `none()`, `single()` and `reduce()` follow the same
  rules for their variables (below).

```rhai,graph=modern
g.gql("MATCH (p:person) RETURN [p IN [1, 2] | p * 2] AS l")   // error: the comprehension variable `p` shadows the variable `p` in scope
g.gql("MATCH (p:person) RETURN [(p)-[:knows]->(f) | count(f)] AS l")   // error: count() cannot be used inside the | part of a pattern comprehension
g.gql("MATCH (p:person) RETURN p.name AS n, [(p)-[:knows]->(f) | f.name] AS l ORDER BY f.name")   // error: `f` is bound inside a pattern comprehension
g.gql("RETURN size([x IN [1, 2, 3] WHERE x > 0]) = size([1, 2, 3]) AS all_positive").to_list()   // #{"all_positive": true}
```

### List predicates and `reduce`

Cypher's `all(x IN l WHERE c)`, `any(..)`, `none(..)`, `single(..)` and
`reduce(acc = init, x IN l | expr)` are openCypher compatibility forms
([Cypher Aliases](aliases.md#list-predicates-and-reduce) has their `NULL` rules). The predicates
are three-valued: a `NULL` list or, when no item decides the answer, a `NULL` condition gives
`NULL`. A list comprehension counted with `size()` gives the same answer when no condition is
`NULL`; over a pattern comprehension they are the Gremlin `where(__..)` / `not(__..)` of a
pattern. `reduce` folds a list per row, where `FOR` and an aggregate fold the rows of a query.

```rhai,graph=modern
g.gql("RETURN all(x IN [1, 2, 3] WHERE x > 0) AS a, any(x IN [1, NULL] WHERE x > 1) AS b, single(x IN [1, 2] WHERE x > 1) AS c").to_list()   // #{"a": true, "b": (), "c": true}
g.gql("RETURN size([x IN [1, 2, 3] WHERE x > 0]) = size([1, 2, 3]) AS a").to_list()   // #{"a": true}, the count form of all()
g.gql("MATCH (p:person) WHERE all(w IN [(p)-[:created]->(s) | s] WHERE w.lang = 'java') AND any(a IN [(p)-[:knows]->(f) | f.age] WHERE a > 30) RETURN p.name AS name").to_list()   // #{"name": "marko"}
g.V().hasLabel("person").not(__.out("created").has("lang", P.neq("java"))).where(__.out("knows").has("age", P.gt(30))).values("name").to_list()   // "marko"
g.gql("MATCH (p:person) RETURN p.name AS name, reduce(s = 0, a IN [(p)-[:knows]->(f) | f.age] | s + a) AS ages ORDER BY name").to_list()   // josh 0, marko 59, peter 0, vadas 0
g.gql("FOR x IN [1, 2, 3] RETURN sum(x) AS s").to_list()   // #{"s": 6}, one fold over the rows
g.gql("RETURN reduce(s = 0, x IN [1, 2, 3] | s + x) AS s").to_list()   // #{"s": 6}, one fold per row
g.gql("MATCH (p:person) RETURN all(p IN [1] WHERE p > 0) AS a")   // error: the variable `p` of all() shadows the variable `p` in scope
g.gql("MATCH (p:person) RETURN any(x IN [1] WHERE x > count(*)) AS a")   // error: count() cannot be used inside the WHERE part of all(), any(), none() or single()
```

## Functions

Function names are case-insensitive (`toInteger`, `TOINTEGER`). Every function returns `NULL`
for a `NULL` argument (except `coalesce`, `nullif` and the `...OrNull` conversions, by their
definition) and fails for an argument of the wrong kind (`22G03`), never with a silent `NULL`. A
position or a length counts **characters** (Unicode scalar values), never bytes. The aggregates
are listed under [Aggregation](#aggregation); [Semantics](semantics.md#functions) has the exact
rules and where they differ from Neo4j and from the Gremlin steps of the same name.

The functions come from two sources: the **GQL functions** (with their Cypher spellings as
[aliases](aliases.md)), and a fixed list of openCypher **compatibility functions** that GQL has no
counterpart for (`toInteger`, `substring`, `split`, `round`, `keys`, `nodes`, ...): they keep
their Neo4j meaning ([Cypher Aliases](aliases.md#compatibility-functions)).

| Function | Returns | Cypher alias |
|---|---|---|
| `element_id(x)` | the id of a vertex or edge, a string | `elementId(x)` |
| `labels(x)` | a vertex's labels in stored order (primary first); an edge's label as a one-item list | |
| `type(e)` | an edge's label; a vertex is an error (use `labels(v)`) | |
| `coalesce(a, b, ...)` | the first argument that is not `NULL` | |
| `nullif(a, b)` | `NULL` when `a = b` is TRUE, else `a` | same (`nullIf`) |
| `path_length(p)` | the number of edges of a path | `length(p)` |

```rhai,graph=modern
g.gql("MATCH (n) RETURN element_id(n) AS id, labels(n) AS labels ORDER BY id LIMIT 3").to_list()
// #{"id": "1", "labels": ["person"]}, #{"id": "2", "labels": ["person"]}, #{"id": "3", "labels": ["software"]}
g.gql("MATCH ()-[e]->() RETURN element_id(e) AS id, type(e) AS type, e.weight AS w ORDER BY id LIMIT 2").to_list()
// #{"id": "0", "type": "knows", "w": 0.5}, #{"id": "1", "type": "created", "w": 0.4}
g.gql("MATCH (n) RETURN n.name AS name, coalesce(n.age, n.lang) AS info ORDER BY name LIMIT 2").to_list()
// #{"info": 32, "name": "josh"}, #{"info": "java", "name": "lop"}
g.gql("MATCH (n:person) RETURN n.name AS name, nullif(n.age, 29) AS age ORDER BY name").to_list()
// #{"age": 32, "name": "josh"}, #{"age": (), "name": "marko"}, #{"age": 35, "name": "peter"}, #{"age": 27, "name": "vadas"}
```

### Conversions

GQL's `CAST` is **strict**: text that is not a value of the target type is an error (`22018`).
The Cypher conversions are **lenient**: `toInteger`, `toFloat`, `toBoolean` give `NULL` for such
text, and their `...OrNull` forms give `NULL` for a value of any unsupported kind too
([Semantics](semantics.md#conversions-strict-cast-lenient-tox)).

| Function | Converts | Unconvertible text | Wrong kind |
|---|---|---|---|
| `CAST(x AS INTEGER)` (`INT`, `INT64`, `BIGINT`, `SIGNED INTEGER`) | integer; float truncated toward zero; a decimal text (`'42'`, `' 1.9 '`, `'1e3'`) | error `22018` | error `22G03` (a boolean too) |
| `CAST(x AS FLOAT)` (`FLOAT64`, `DOUBLE`, `DOUBLE PRECISION`, `REAL`) | number; a decimal text, `'NaN'`, `'Infinity'`, `'-Infinity'` | error `22018` | error `22G03` |
| `CAST(x AS STRING)` (`VARCHAR`, `CHAR VARYING`) | any value except a vertex, edge or path | - | error `22G03` |
| `CAST(x AS BOOLEAN)` (`BOOL`) | boolean; `'true'` / `'false'` in any case, `'unknown'` is `NULL` | error `22018` | error `22G03` (numbers too) |
| `toInteger(x)` | as `CAST`, also `true` / `false` (1 / 0) and Cypher literals (`'0x1F'`, `'0o17'`, `'1_000'`) | `NULL` | error `22G03` |
| `toFloat(x)` | as `CAST`, also Cypher literals | `NULL` | error `22G03` |
| `toBoolean(x)` | as `CAST`, also integers (0 is FALSE, any other TRUE) | `NULL` | error `22G03` |
| `toString(x)` | as `CAST(x AS STRING)` | - | error `22G03` |
| `toIntegerOrNull(x)`, `toFloatOrNull(x)`, `toBooleanOrNull(x)`, `toStringOrNull(x)` | as their `toX` | `NULL` | `NULL` (a vertex, edge or path: still an error for `toStringOrNull`) |

A float's text always shows that it is a float (`toString(1.0)` is `'1.0'`, Neo4j's form), while
Gremlin's `as_string()` gives `"1"`; integers and booleans give the same text as `as_string()`.
A list or a map converts to Neo4j's text form (`'[29, marko, 1.0, null]'`, `'{a: 1, b: marko}'`).

```rhai,graph=modern
g.gql("MATCH (n:person) RETURN n.name AS name, toString(n.age) AS a, toString(n.age / 10.0) AS b ORDER BY name").to_list()
// #{"a": "32", "b": "3.2", "name": "josh"}, #{"a": "29", "b": "2.9", "name": "marko"}, #{"a": "35", "b": "3.5", "name": "peter"}, #{"a": "27", "b": "2.7", "name": "vadas"}
g.v().has_label("person").values("age").as_string().order().to_list()   // "27", "29", "32", "35"
g.gql("MATCH (n:person) RETURN toFloat(n.age) AS f ORDER BY f").to_list()   // #{"f": 27.0}, #{"f": 29.0}, #{"f": 32.0}, #{"f": 35.0}
g.v().has_label("person").values("age").as_number(GType.DOUBLE).order().to_list()   // 27.0, 29.0, 32.0, 35.0
g.gql("MATCH (n:person {name: 'marko'}) RETURN CAST('42' AS INTEGER) AS i, CAST(' 1.5 ' AS FLOAT) AS f, CAST('TRUE' AS BOOLEAN) AS b, CAST(1.0 AS STRING) AS s").to_list()
// #{"b": true, "f": 1.5, "i": 42, "s": "1.0"}
g.gql("MATCH (n:person {name: 'marko'}) RETURN toInteger(' 42 ') AS a, toInteger('1.9') AS b, toInteger('0x1F') AS c, toInteger('abc') AS d, toIntegerOrNull(true) AS e").to_list()
// #{"a": 42, "b": 1, "c": 31, "d": (), "e": 1}
g.gql("MATCH (n:person {name: 'marko'}) RETURN toBoolean(1) AS a, toBooleanOrNull(1.5) AS b, toFloat('1e3') AS c, toFloatOrNull([1]) AS d").to_list()
// #{"a": true, "b": (), "c": 1000.0, "d": ()}
g.gql("MATCH (n:person {name: 'marko'}) RETURN toString([n.age, n.name, 1.0, NULL]) AS l, toString({a: 1, b: n.name}) AS m").to_list()
// #{"l": "[29, marko, 1.0, null]", "m": "{a: 1, b: marko}"}
g.gql("MATCH (n:person) RETURN n.name || ' is ' || toString(n.age) AS s ORDER BY s LIMIT 2").to_list()
// #{"s": "josh is 32"}, #{"s": "marko is 29"}
g.gql("MATCH (n:person {name: 'marko'}) RETURN CAST('abc' AS INTEGER) AS x")   // error: invalid character value for cast (22018)
g.gql("MATCH (n:person {name: 'marko'}) RETURN toFloat(true) AS x")   // error: toFloat() takes a number or a string, not a boolean (22G03)
g.gql("MATCH (n:person {name: 'marko'}) RETURN toString(n) AS x")   // error: toString() takes no vertex, edge or path (22G03)
```

`CAST` to a GQL type Graphersal has no values of yet (`DATE`, `LIST<..>`, `DECIMAL`, ...) is
not supported yet; an unknown type name is a syntax error with a suggestion:

```rhai,graph=modern
g.gql("MATCH (n) RETURN CAST(n.age AS DATE) AS d")   // error: CAST to DATE is not yet supported
g.gql("MATCH (n) RETURN CAST(n.age AS INTEGR) AS d")   // error: unknown type INTEGR in CAST, did you mean INTEGER
```

### Strings

| Function | Returns | Cypher spelling |
|---|---|---|
| `upper(s)`, `lower(s)` | the string in upper / lower case | `toUpper(s)`, `toLower(s)` |
| `char_length(s)`, `character_length(s)` | the number of characters | `size(s)` |
| `byte_length(s)`, `octet_length(s)` | the number of UTF-8 bytes | |
| `left(s, n)`, `right(s, n)` | the first / last `n` characters (the whole string when `n` is larger) | same |
| `substring(s, start [, length])` | Cypher: `length` characters from the 0-based `start` (to the end without `length`); past the end `''` | compatibility function |
| `TRIM([[LEADING \| TRAILING \| BOTH] [c] FROM] s)`, `trim(s)` | `s` without the character `c` (default: the space) at both ends, the start or the end; `c` is exactly one character | `trim(s)` |
| `btrim(s [, chars])`, `ltrim(s [, chars])`, `rtrim(s [, chars])` | `s` without any character of the set `chars` (default: the space) at both ends / the start / the end | same |
| `replace(s, search, replacement [, limit])` | every occurrence of `search` replaced, left to right; at most `limit` of them | compatibility function |
| `split(s, delimiter)` | the list of the parts between the delimiters (a string or a list of strings); empty parts are kept | compatibility function |
| `reverse(x)` | a string with its characters reversed, or a list with its items reversed | compatibility function |
| `isEmpty(x)` | whether a string, list or map has no character / item / entry | compatibility function |

A negative position or length is an error (`22011`), and a `TRIM` character that is not exactly
one character is too (`22027`). The SQL form `SUBSTRING(s FROM a FOR b)` is not GQL; it fails
with a hint to `substring(s, start, length)`.

```rhai,graph=modern
g.gql("MATCH (n:person) RETURN left(n.name, 2) AS l, right(n.name, 2) AS r, substring(n.name, 1, 3) AS s, substring(n.name, 2) AS t ORDER BY l").to_list()
// #{"l": "jo", "r": "sh", "s": "osh", "t": "sh"}, #{"l": "ma", "r": "ko", "s": "ark", "t": "rko"}, ... peter, vadas
g.v().has_label("person").values("name").substring(1, 4).order().to_list()   // "ada", "ark", "ete", "osh"
g.gql("MATCH (n:person) RETURN n.name AS name, replace(n.name, 'a', '4') AS r, split(n.name, 'a') AS s ORDER BY name").to_list()
// #{"name": "josh", "r": "josh", "s": ["josh"]}, #{"name": "marko", "r": "m4rko", "s": ["m", "rko"]}, ... #{"name": "vadas", "r": "v4d4s", "s": ["v", "d", "s"]}
g.v().has_label("person").values("name").replace("a", "4").order().to_list()   // "josh", "m4rko", "peter", "v4d4s"
g.gql("MATCH (n:person {name: 'marko'}) RETURN reverse(n.name) AS r, reverse([1, 2, 3]) AS l, byte_length('héllo') AS b, char_length('héllo') AS c, isEmpty('') AS e").to_list()
// #{"b": 6, "c": 5, "e": true, "l": [3, 2, 1], "r": "okram"}
g.v().has("name", "marko").values("name").reverse().to_list()   // "okram"
g.gql("MATCH (n:person {name: 'marko'}) RETURN trim('  a  ') AS a, TRIM(LEADING 'x' FROM 'xxaxx') AS b, TRIM(TRAILING FROM '  a  ') AS c, btrim('xyaxy', 'xy') AS d, ltrim('  a  ') AS e, rtrim('..a..', '.') AS f").to_list()
// #{"a": "a", "b": "axx", "c": "  a", "d": "a", "e": "a  ", "f": "..a"}
g.gql("MATCH (n:person {name: 'marko'}) RETURN split('a,,b,', ',') AS a, split('a-b;c', ['-', ';']) AS b, split('abc', '') AS c, replace('abc', '', '.') AS d, replace('aaa', 'a', 'b', 2) AS e").to_list()
// #{"a": ["a", "", "b", ""], "b": ["a", "b", "c"], "c": ["a", "b", "c"], "d": ".a.b.c.", "e": "bba"}
g.gql("MATCH (n:person {name: 'marko'}) RETURN substring(n.name, -1) AS x")   // error: substring() takes a start of 0 or more (22011)
g.gql("MATCH (n:person {name: 'marko'}) RETURN TRIM(BOTH 'ab' FROM 'abxab') AS x")   // error: TRIM(.. FROM ..) removes exactly one character (22027)
g.gql("MATCH (n) RETURN SUBSTRING(n.name FROM 1 FOR 2) AS r")   // error: SUBSTRING(s FROM a FOR b) is SQL, not GQL
```

### Numbers

| Function | Returns | Cypher spelling |
|---|---|---|
| `abs(x)` | the absolute value, of the argument's type | same |
| `floor(x)`, `ceil(x)` / `ceiling(x)` | rounded down / up; an integer stays the integer | same |
| `round(x [, precision [, mode]])` | rounded, always a float; `mode` one of `'UP'`, `'DOWN'`, `'CEILING'`, `'FLOOR'`, `'HALF_UP'` (the default), `'HALF_DOWN'`, `'HALF_EVEN'` | compatibility function |
| `sign(x)` | -1, 0 or 1, an integer | compatibility function |
| `sqrt(x)`, `exp(x)` | the square root, e to the power `x`; a float | same |
| `ln(x)`, `log10(x)` | the natural / decimal logarithm; a float | `log(x)`, `log10(x)` |
| `log(base, x)` | the logarithm to `base`; a float | |
| `power(x, y)` | `x` to the power `y`; a float | (Cypher's `^` is not supported yet) |
| `mod(a, b)` | the remainder, exactly as `a % b` | `a % b` |

None of them makes `NaN` or an infinity from finite numbers: the square root of a negative
number and `power()` of 0 to a negative power are errors (`2201F`), the logarithm of a number
`<= 0` is an error (`2201E`), an overflow is `22003`. `round()` with one argument rounds a half
toward positive infinity (`round(-2.5)` is `-2.0`, Java's and Neo4j's rule); with a precision it
rounds the decimal text of the number (`round(2.675, 2)` is `2.68`).

```rhai,graph=modern
g.gql("MATCH (n:person) RETURN n.name AS name, sqrt(n.age) AS s ORDER BY name LIMIT 2").to_list()
// #{"name": "josh", "s": 5.656854249492381}, #{"name": "marko", "s": 5.385164807134504}
g.v().has_label("person").order().by("name").limit(2).values("age").math("sqrt(_)").to_list()   // 5.656854249492381, 5.385164807134504
g.gql("MATCH (n:person {name: 'marko'}) RETURN abs(-3) AS a, abs(-2.5) AS b, floor(2.7) AS c, ceil(2.1) AS d, ceiling(3) AS e, sign(-0.5) AS f, mod(7, 3) AS g").to_list()
// #{"a": 3, "b": 2.5, "c": 2.0, "d": 3.0, "e": 3, "f": -1, "g": 1}
g.gql("MATCH (n:person {name: 'marko'}) RETURN round(2.5) AS a, round(-2.5) AS b, round(2.675, 2) AS c, round(-1.55, 1) AS d, round(2.5, 0, 'HALF_EVEN') AS e, round(3) AS f").to_list()
// #{"a": 3.0, "b": -2.0, "c": 2.68, "d": -1.6, "e": 2.0, "f": 3.0}
g.gql("MATCH (n:person {name: 'marko'}) RETURN exp(1) AS a, ln(1) AS b, log(100) AS c, log10(1000) AS d, log(2, 8) AS e, power(2, 10) AS f").to_list()
// #{"a": 2.718281828459045, "b": 0.0, "c": 4.605170185988092, "d": 3.0, "e": 3.0, "f": 1024.0}
g.inject(100).math("log(_)").to_list()   // 4.605170185988092
g.gql("MATCH (n:person) RETURN round(avg(n.age), 1) AS mean").to_list()   // #{"mean": 30.8}
g.gql("MATCH (n:person {name: 'marko'}) RETURN ln(0) AS x")   // error: ln() of a number <= 0 (2201E)
g.gql("MATCH (n:person {name: 'marko'}) RETURN sqrt(-1) AS x")   // error: sqrt() of a negative number (2201F)
g.gql("MATCH (n:person {name: 'marko'}) RETURN round(1234.5, -2) AS x")   // error: round() needs a precision >= 0 (22023)
```

### Lists, elements and paths

| Function | Returns | Cypher spelling |
|---|---|---|
| `size(list)`, `cardinality(list)` | the number of items (`size(e)` of a group variable counts its edges) | `size(list)` |
| `head(list)`, `last(list)` | the first / last item; `NULL` for `[]` | compatibility function |
| `tail(list)` | all items but the first | compatibility function |
| `trim(list, n)` | the list without its last `n` items | |
| `range(start, end [, step])` | the integers from `start` to `end` inclusive, by `step` (default 1) | compatibility function |
| `keys(x)` | the property keys of a vertex or edge (a stored `null` is no key), or the keys of a map | compatibility function |
| `properties(x)` | the properties of a vertex or edge as a map; a map itself | compatibility function |
| `startNode(e)`, `endNode(e)` | the stored source / target vertex of the edge | compatibility function |
| `n IS [NOT] SOURCE OF e`, `n IS [NOT] DESTINATION OF e` | whether `n` is the edge's source / target (a predicate) | |
| `nodes(p)`, `relationships(p)` | the vertices / the edges of a path, in the path's order | compatibility function |

`startNode(e)` is the edge's **stored** source, whatever direction the pattern was written in:
in `(a)-[e]-(b)` the variable `a` may be either end. `range()` checks its size before it builds
the list, so `range(0, 9223372036854775807)` is a limit error, never an out-of-memory
([Errors](errors.md#scope-grouping-and-limits)).

```rhai,graph=modern
g.gql("MATCH (n:person {name: 'marko'}) RETURN head([1, 2, 3]) AS h, last([1, 2, 3]) AS l, tail([1, 2, 3]) AS t, trim([1, 2, 3], 1) AS tr, range(1, 5) AS r, range(10, 0, -5) AS d, head([]) AS e").to_list()
// #{"d": [10, 5, 0], "e": (), "h": 1, "l": 3, "r": [1, 2, 3, 4, 5], "t": [2, 3], "tr": [1, 2]}
g.gql("MATCH (p:person)-[:created]->(s) WITH p, s ORDER BY s.name RETURN p.name AS name, head(collect(s.name)) AS first ORDER BY name").to_list()
// #{"first": "lop", "name": "josh"}, #{"first": "lop", "name": "marko"}, #{"first": "lop", "name": "peter"}
g.gql("MATCH (n:person {name: 'marko'}) RETURN keys(n) AS k, properties(n) AS p").to_list()   // #{"k": ["name", "age"], "p": #{"age": 29, "name": "marko"}}
g.v().has("name", "marko").properties().key().fold().to_list()   // ["name", "age"]
g.v().has("name", "marko").value_map().by(__.unfold()).to_list()   // #{"age": 29, "name": "marko"}
g.gql("MATCH (a)-[e:knows]-(b) RETURN a.name AS a, b.name AS b, startNode(e).name AS s, endNode(e).name AS t ORDER BY a, b").to_list()
// josh marko: s marko, t josh; marko josh: marko, josh; marko vadas: marko, vadas; vadas marko: marko, vadas
g.e().has_label("knows").project("s", "t").by(__.out_v().values("name")).by(__.in_v().values("name")).to_list()
// #{"s": "marko", "t": "vadas"}, #{"s": "marko", "t": "josh"}
g.gql("MATCH (a)-[e:knows]-(b) RETURN a.name AS a, b.name AS b, a IS SOURCE OF e AS src ORDER BY a, b").to_list()
// josh marko false, marko josh true, marko vadas true, vadas marko false
g.gql("MATCH p = (a {name: 'marko'})-[:knows|created]->{1,2}(b) RETURN b.name AS b, size(nodes(p)) AS n, size(relationships(p)) AS r, last(nodes(p)) = b AS same ORDER BY n, b").to_list()
// josh 2 1 true, lop 2 1 true, vadas 2 1 true, lop 3 2 true, ripple 3 2 true
g.gql("MATCH (n:person {name: 'marko'}) RETURN range(0, 9223372036854775807) AS r")   // error: Resource limit 'traversal.max_materialized_bytes' exceeded
```

A [list comprehension](#map-projections-and-comprehensions) maps or filters the items of a
list (`[x IN nodes(p) | x.name]`); [`FOR`](#for-and-unwind) walks a list as rows and
`collect_list()` collects them again:

```rhai,graph=modern
g.gql("MATCH p = (a {name: 'marko'})-[:knows]->(b) RETURN b.name AS b, [x IN nodes(p) | x.name] AS names ORDER BY b").to_list()
// #{"b": "josh", "names": ["marko", "josh"]}, #{"b": "vadas", "names": ["marko", "vadas"]}
g.gql("MATCH p = (a {name: 'marko'})-[:knows]->(b) FOR x IN nodes(p) RETURN b.name AS b, collect_list(x.name) AS names ORDER BY b").to_list()
// #{"b": "josh", "names": ["marko", "josh"]}, #{"b": "vadas", "names": ["marko", "vadas"]}
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

These are part of GQL (or Cypher constructs that LLMs and Cypher users write often) and come in
a later version. Each fails before the run with GQLSTATUS `0A000` and a message that names the
construct and the alternative.

| Construct | Until then |
|---|---|
| Cypher's `FOREACH` | `FOR x IN list INSERT ..` (or `SET ..`, `DELETE ..`, `MERGE ..`) ([Writing with GQL](writes.md#not-yet)) |
| a write in an operand of `UNION`, a path variable in `INSERT` | run the write on its own; name the nodes and edges |
| a `SET` / `REMOVE` target deeper than a variable (`n.a.b = ..`, `head(l).k = ..`) | `WITH head(l) AS x SET x.k = ..` |
| `NEXT YIELD` | `RETURN` the columns to pass on before `NEXT` |
| `OPTIONAL { ... }` / `OPTIONAL ( ... )` blocks | one `OPTIONAL MATCH` per optional part |
| a quantifier nested in a quantified path pattern, `(((a)-->(b)){2})+` | write the inner path out, `((x)-->(y)-->(z))+`, or Gremlin `repeat()` |
| path search prefixes and match modes (`ANY SHORTEST`, `TRAIL`, `DIFFERENT EDGES` written out) | Gremlin `path()`, `repeat()` |
| `EXCEPT`, `INTERSECT`, `OTHERWISE` | `NOT EXISTS { }` / `EXISTS { }` on the rows of the first query, or `NEXT` |
| list indexing and slicing `x[0]`, `x[1..2]` | Gremlin after `gql()` |
| `NULLS FIRST` / `NULLS LAST` | the fixed order: `NULL` last ascending, first descending |
| `gql()` after other steps (`g.v().gql(..)`) | start with `g.gql(..)` and continue with Gremlin |
| a list-comprehension or list-predicate variable read inside a subquery of its `WHERE` / `\|` part | test the variable outside the subquery, or match the pattern in the query |
| the exponentiation operator `^` | `power(x, y)` |
| trigonometric functions (`sin`, `cos`, ..., `pi()`, `degrees()`) | Gremlin `math()` after `gql()` |
| date and time functions (`date()`, `datetime()`, `duration()`, ...) | ISO-8601 strings compare correctly as text; dates come in version 0.2.0 |
| list conversions `toIntegerList()`, `toStringList()`, ... | `FOR t IN list RETURN collect_list(toInteger(t))`, or Gremlin `unfold()` and `as_number()` after `gql()` |
| `normalize(s)` | store normalized text |
| GQL's `elements(p)` | `nodes(p)` and `relationships(p)` |
| statistical aggregates (`percentileCont()`, `stDev()`, ...) | `collect_list(x)`, then compute after `gql()` |

```rhai,graph=modern
g.gql("MATCH (((a)-[:knows]->(b)){1,2})+ RETURN b")   // error: a parenthesized path pattern nested in another one is not yet supported
g.gql("MATCH ((a)-[:knows]->(b)){1,3} RETURN [n IN b | n.name] AS b ORDER BY b").to_list()   // #{"b": ["josh"]}, #{"b": ["vadas"]}
g.gql("MATCH (p:person) RETURN [x IN [p] WHERE (x)-[:created]->() | x.name] AS l")   // error: reading the comprehension variable `x` inside a subquery is not yet supported
g.gql("MATCH (p:person) WHERE (p)-[:created]->() RETURN [x IN [p] | x.name] AS l ORDER BY l").to_list()   // #{"l": ["josh"]}, #{"l": ["marko"]}, #{"l": ["peter"]}
g.gql("MATCH (p:person) RETURN p.name AS n EXCEPT MATCH (p:person)-[:created]->() RETURN p.name AS n")   // error: EXCEPT is not yet supported
g.gql("MATCH (p:person) WHERE NOT EXISTS { (p)-[:created]->() } RETURN p.name AS n").to_list()   // #{"n": "vadas"}
g.gql("MATCH (n:person) RETURN sin(n.age) AS s")   // error: sin() (a trigonometric function) is not yet supported
g.gql("MATCH (n:person) RETURN n.name AS name, n.age AS a ORDER BY name").select("a").math("sin(_)").to_list()   // 0.5514266812416906, -0.6636338842129675, -0.428182669496151, 0.956375928404503
```
