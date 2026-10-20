# Semantics

`gql()` follows ISO GQL. Where the standard leaves a choice to the implementation, or where its
text could not be checked yet, Graphersal follows Neo4j's Cypher 25, which implements GQL too;
this page says where.

## Trail: different edges

Within one `MATCH` clause an edge is bound at most once (GQL's `DIFFERENT EDGES` match mode,
also called trail semantics; the GQL default and Neo4j's). Vertices may repeat.

```rhai,graph=modern
g.gql("MATCH (a)-[:created]->(s)<-[:created]-(b) WHERE s.name = 'lop' RETURN a.name AS a, b.name AS b ORDER BY a, b").to_list()
// josh/marko, josh/peter, marko/josh, marko/peter, peter/josh, peter/marko: never a person with themselves
g.gql("MATCH (a {name: 'marko'})-[:knows]->(b)<-[:knows]-(c) RETURN c.name AS c").to_list()   // no rows: c could only be marko again over the same edge
```

The second query has no rows: every `knows` edge into `b` comes from marko, and binding it again
for `c` would use the same edge twice. Gremlin has no such rule, so its twin returns marko:

```rhai,graph=modern
g.v().has("name", "marko").out("knows").in("knows").values("name").to_list()   // "marko", "marko"
```

Two separate `MATCH` clauses are checked separately: an edge bound in the first may be bound
again in the second. An edge variable written twice in one `MATCH` is an error.

```rhai,graph=modern
g.gql("MATCH (a {name: 'marko'})-[:knows]->(b) MATCH (b)<-[:knows]-(c) RETURN c.name AS c").to_list()   // #{"c": "marko"}, #{"c": "marko"}
g.gql("MATCH (a)-[e]->(b), (b)<-[e]-(a) RETURN a")   // error: the edge variable `e` is used for more than one edge in the same MATCH
```

## Quantified edges

A quantified edge (`-[:knows]->{1,3}`, [The Supported Subset](subset.md#quantified-edges)) is
part of its `MATCH` clause like any edge: within one match **every edge is different**, across
the repetitions and from the other edges of the clause (trail). Vertices may repeat, so a
quantified edge may walk around a cycle, but never over the same edge twice.

- The zero-length match of a lower bound 0 (`{0,n}`, `*`) binds the far node to the near one;
  the group variable is then `[]`. `{0}` matches only that.
- An upper bound larger than any trail can be (beyond about four billion) means "no bound";
  such a quantifier behaves like `{m,}`.
- **Unbounded quantifiers** (`{m,}`, `*`, `+`, Cypher `[*]`) have no limit of their own: trail
  ends every walk once it has used every edge it can reach, which on a finite graph always
  happens. Two general limits still apply: [`repeat.max_loops`](../users_guide/resource_limits.md)
  (10 000 repetitions) and `traversal.max_traversers` (live rows between two steps). Reaching
  either is an **error, never a shortened result**; its help names the quantifier and how to bound
  it or raise the limit with `g.with(..).gql(..)`.

```rhai,graph=modern
g.with("repeat.max_loops", 1).gql("MATCH (a {name: 'marko'})-[:knows|created]->*(b) RETURN b.name AS b")   // error: repeat() exceeded the loop limit of 1 iterations
g.with("repeat.max_loops", 5).gql("MATCH (a {name: 'marko'})-[:knows|created]->*(b) RETURN b.name AS b ORDER BY b").to_list()
// josh, lop, lop, marko, ripple, vadas
g.gql("MATCH (a {name: 'marko'})-[e]->{0}(b) RETURN b.name AS b, e").to_list()   // #{"b": "marko", "e": []}
```

The help of the first error reads (shortened):

```text
Help: The loop of the quantified edge -[:knows|created]->{0,} of the gql() query ran 1 iterations, the limit of repeat.max_loops. ...
Give the quantifier an upper bound, as in -[:knows|created]->{0,5}, ... Or raise the limit for the query, g.with("repeat.max_loops", 2).gql(..), ...
```

Each match of a quantified edge is a row of its own (the edges it used keep the rows apart), so
the number of rows can grow fast on a densely connected graph: bound the quantifier where the
question allows it.

## Quantified path patterns

A quantified path pattern (`(a) ((x)-[e]->(y)){1,3} (b)`,
[The Supported Subset](subset.md#quantified-path-patterns)) repeats a whole chain. Graphersal
follows ISO GQL and, where a detail is open, Neo4j's Cypher 25, which implements the same
construct:

- **Merge rule.** The node before the group is the first node of the first iteration, the node
  after it the last node of the last iteration, and each iteration starts where the previous one
  ended: in `(a) ((x)-->(y)){1,3} (b)`, `a` is the first `x`, `b` the last `y`, and the `y` of
  one iteration is the `x` of the next. After zero iterations `a` and `b` are one node, so
  `(a) ((x)-->(y))* (a)` matches the closed walks from `a` (the zero-length one included).
- **Trail per body edge.** Within one match every edge is different: across the iterations,
  between the body's edges and from the other edges of the `MATCH` clause, so a quantified
  path pattern ends on every finite graph, like a quantified edge. A body edge that a `?` part
  did not bind (zero iterations) clashes with nothing.
- **Group lists.** With a quantifier other than `?`, each body variable is the list of its
  elements, one per iteration, in the order the text reads (also when the plan walks the
  pattern from its far end): `x` = `[x1, x2, ...]`. A zero-length match gives `[]`. A property
  of a group variable has no single value outside the body (`42002`); inside the body, in its
  conditions, the variable is the element of the current iteration.
- **`?` gives nullable singletons (ISO).** `((x)-[e]->(y))?` matches its body once or not at
  all; its variables are single elements, `NULL` for the match where it did not match (no
  iteration), not lists. `{0,1}` matches the same paths but keeps group lists (`[]` or one
  element).
- **No quantifier: singletons.** A parenthesized path pattern without a quantifier matches its
  body exactly once; its variables are single elements, as if the parentheses were not there,
  and its `WHERE` is a condition on that part of the path.
- **The body condition runs in every iteration** and reads only body variables, parameters and
  values: an iteration whose condition is not `TRUE` ends that walk.

```rhai,graph=modern
g.gql("MATCH (a {name: 'marko'})-[e]->(m) ((x)-[f]-(y))? (b) RETURN m.name AS m, b.name AS b ORDER BY m, b").to_list()
// josh/josh, josh/lop, josh/ripple, lop/josh, lop/lop, lop/peter, vadas/vadas: never back to marko over the edge e
g.gql("MATCH (p:person {name: 'vadas'}) ((x)-[:created]->(s))? (q) RETURN s AS s, q.name AS q").to_list()   // #{"q": "vadas", "s": ()}
g.gql("MATCH (p:person {name: 'vadas'}) ((x)-[:created]->(s)){0,1} (q) RETURN s AS s, q.name AS q").to_list()   // #{"q": "vadas", "s": []}
```

Limits that hold now (E17; each refused with `0A000` and a help line naming the rewrite): one
chain per body, no quantifier nested inside a body (neither a group nor a quantified edge), no
path variable inside a body (name the whole path instead), and a body variable is a new name:
an outer variable written inside a body, a variable written twice in one body, a body condition
reading an outer variable, and a subquery inside a body are not supported yet. Every body edge
counts once toward the 64 edge patterns of a `MATCH` clause, however often the body repeats.

The unbounded quantifiers (`{m,}`, `*`, `+`) are bounded as for a quantified edge:
`repeat.max_loops` counts the iterations of the whole body, `traversal.max_traversers` the live
rows, and reaching either is an error naming the pattern
([Errors](errors.md#quantified-path-patterns)), never a shortened result.

**Reading `((`.** Where an expression stands, `((` followed by a pattern and a closing `)` with
`{..}`, `?`, a node, an edge, or `*` / `+` **not followed by an operand** reads as a quantified
path pattern (a pattern predicate). So `((a)-->(b)) + (c)` is the pattern `((a)-->(b))+ (c)`,
not the boolean of the pattern predicate `((a)-->(b))` plus `c`; with an operand after it
(`((a)-->(b)) + [1]`, `+ x`, `+ 1`) it stays arithmetic. Arithmetic on a predicate's boolean is
a type error (`22G03`) in either reading, so no working query changes meaning.

## OPTIONAL MATCH and NULL

A row that reaches an `OPTIONAL MATCH` whose pattern does not match continues once, with every
variable the optional part introduces set to `NULL`: a property of it is `NULL`, a group
variable is `NULL` (not `[]`), a path variable is `NULL`. The variables bound before keep their
values. The trail rule is checked per clause, so an optional part may bind an edge the `MATCH`
before it bound.

```rhai,graph=modern
g.gql("MATCH (a {name: 'marko'})-[:knows]->(b) OPTIONAL MATCH (b)-[:created]->(s) RETURN b.name AS b, s.name AS s ORDER BY b, s").to_list()
// #{"b": "josh", "s": "lop"}, #{"b": "josh", "s": "ripple"}, #{"b": "vadas", "s": ()}
```

## NULL and three-valued logic

A property that is missing reads as `NULL`, exactly like a property stored as `null`. `WHERE`
uses three-valued logic: a comparison with `NULL` is `NULL` (unknown), `AND` / `OR` / `NOT`
follow the SQL / GQL truth tables, and a row passes only when the condition is TRUE.

```rhai,graph=modern
g.gql("MATCH (n) RETURN n.name AS name, n.age > 30 AS old, n.lang = 'java' AS java ORDER BY name").to_list()
// josh: old true, java (); lop: old (), java true; marko: old false, java (); ...
g.gql("MATCH (n) RETURN n.age = NULL AS a, NULL IS NULL AS b, 2 IN [1, NULL] AS c, NULL IN [] AS d LIMIT 1").to_list()
// #{"a": (), "b": true, "c": (), "d": false}
g.gql("MATCH (n) WHERE n.age IS NULL RETURN n.name AS name ORDER BY name").to_list()   // #{"name": "lop"}, #{"name": "ripple"}
```

(`()` is how Rhai shows `NULL`; JSON shows `null`.) Test for a missing value with `IS NULL`,
never with `= NULL`.

The consequence that surprises Gremlin users: `NOT` of an unknown condition is still unknown, so
the software (which has no `age`) passes neither `n.age < 30` nor `NOT n.age < 30`, while
Gremlin's `not(has(..))` keeps them:

```rhai,graph=modern
g.gql("MATCH (n) WHERE NOT n.age < 30 RETURN n.name AS name ORDER BY name").to_list()   // #{"name": "josh"}, #{"name": "peter"}
g.v().not(__.has("age", P.lt(30))).values("name").order().to_list()   // "josh", "lop", "peter", "ripple"
g.gql("MATCH (n) WHERE n.age <> 29 RETURN n.name AS name ORDER BY name").to_list()   // josh, peter, vadas (not lop, not ripple)
```

The same holds for `<>`: a vertex without `age` is not "different from 29", its comparison is
unknown.

## EXISTS and pattern predicates

**A row is kept once, however often the body matches.** `EXISTS` (and a pattern predicate,
[The Supported Subset](subset.md#exists-and-pattern-predicates)) is a semi-join: it asks whether
there is a match and adds no rows. The same pattern in `MATCH` gives one row per match, so josh,
who created two programs, comes twice:

```rhai,graph=modern
g.gql("MATCH (p:person)-[:created]->() RETURN p.name AS name ORDER BY name").to_list()   // josh, josh, marko, peter
g.gql("MATCH (p:person) WHERE (p)-[:created]->() RETURN p.name AS name ORDER BY name").to_list()   // josh, marko, peter
```

**TRUE or FALSE, never `NULL`.** A body that matches is TRUE, one that does not is FALSE, also
when it reads a variable that is `NULL` on the row (an import from an `OPTIONAL MATCH` that did
not match): a `NULL` element matches nothing, so `EXISTS` is FALSE and `NOT EXISTS` TRUE. Around
it, three-valued logic works as usual: `NULL OR EXISTS {..}` is TRUE when the body matches and
`NULL` otherwise. (This follows Neo4j; the ISO text is still to be checked.)

```rhai,graph=modern
g.gql("MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s WHERE s.name = 'ripple') RETURN p.name AS name, EXISTS { (s)<-[:created]-() } AS made, NOT EXISTS { (s)<-[:created]-() } AS not_made ORDER BY name").to_list()
// josh: made true, not_made false; marko, peter, vadas (s is NULL): made false, not_made true
g.gql("MATCH (p:person) RETURN p.name AS name, p.lang = 'x' OR (p)-[:knows]->() AS a ORDER BY name").to_list()
// josh (), marko true, peter (), vadas ()
```

**Trail per body clause.** Each `MATCH` clause inside a body is its own clause for the trail rule
([Trail](#trail-different-edges)): its edges are different from each other, but they may repeat
an edge of the query outside the body (an imported edge variable is that same edge) or of another
body clause. (Neo4j's rule; the ISO text is still to be checked.)

```rhai,graph=modern
g.gql("MATCH (a {name: 'marko'}) WHERE EXISTS { MATCH (a)-[:knows]->(x)<-[:knows]-(a) } RETURN a.name AS a").to_list()   // no rows: one clause, one edge twice
g.gql("MATCH (a {name: 'marko'}) WHERE EXISTS { MATCH (a)-[:knows]->(x) MATCH (x)<-[:knows]-(a) } RETURN a.name AS a").to_list()   // #{"a": "marko"}
g.gql("MATCH (a {name: 'marko'})-[e:knows]->(b) WHERE EXISTS { MATCH (b)<-[e]-(x) } RETURN b.name AS b ORDER BY b").to_list()   // josh, vadas: e imported
```

**Evaluated only when needed, at most once per row.** A body stops at its first match. Where
`EXISTS` stands inside a larger expression, a row runs the body only when the evaluation reaches
it (`a OR EXISTS {..}` runs nothing when `a` is TRUE), and once even when several columns or keys
of the row hold the same subquery. A body that imports nothing (`EXISTS { (:robot) }`) is still
run per row; its answer is not cached yet.

**Filter order.** The conditions of one element (or one `WHERE`) run in this order: those
pushed into `has()`, the other plain conditions, then those holding a subquery, which cost the
most (a subquery inside a larger expression before a plain `[NOT] EXISTS`). The result does not depend on the order, only the
work does. The global optimizer rule `filter_reorder` may still move a subquery's step before a
`gql:filter` by its own cost estimate, as it does for the same Gremlin steps
([Plans and Performance](plans.md#exists-and-pattern-predicates)).

## Map projections and comprehensions

**A `NULL` variable projects to `NULL`.** A map projection `v {..}` of a `NULL` variable (an
`OPTIONAL MATCH` that did not match) is `NULL`, and its entries are not evaluated at all, while
the GQL record constructor `{name: v.name}` builds a map whose values are `NULL`. A pattern
comprehension that imports a `NULL` variable matches nothing and gives `[]`, never `NULL`:

```rhai,graph=modern
g.gql("MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s {name: 'ripple'}) RETURN p.name AS name, s {.name} AS projection, {name: s.name} AS record, [(s)<-[:created]-(c) | c.name] AS creators ORDER BY name").to_list()
// josh: projection #{"name": "ripple"}, record #{"name": "ripple"}, creators ["josh"]
// marko, peter, vadas: projection (), record #{"name": ()}, creators []
g.gql("MATCH (p:person {name: 'vadas'}) OPTIONAL MATCH (p)-[:created]->(s) RETURN s {k: 1 / 0} AS m").to_list()   // #{"m": ()}: the entry never runs
```

**Keys.** The entries come in the written order. `.*` puts every stored property at its place,
in stored order, without the id and the labels; a property stored as `null` is left out, as in
`properties()`. A key written twice (also through `.*`) keeps its first place and takes the
later value. The variable of a projection must be a vertex, an edge or a map; any other value is
the data error `22G03`. An element inside the map (`friend: f`) stays the live element
([Result Shape](result_shape.md#nested-maps-and-lists)).

```rhai,graph=modern
g.gql("MATCH (p:person {name: 'marko'}) RETURN p {.name, age: 1, .age} AS m").to_json()   // [{"m": {"name": "marko", "age": 29}}]
g.gql("WITH {a: 1, b: 2} AS m RETURN m {.a, .c, .*, d: 3} AS x").to_json()   // [{"x": {"a": 1, "c": null, "b": 2, "d": 3}}]
g.gql("WITH 1 AS m RETURN m {.a} AS x")   // error: a map projection reads a node, an edge or a map, not a number (22G03)
```

**List comprehension.** A `NULL` list gives `NULL`, a value that is not a list the data error
`22G03`. `WHERE` keeps an item only when the condition is TRUE: FALSE and `NULL` drop it
([three-valued logic](#null-and-three-valued-logic)). The result keeps the list's order.

```rhai,graph=modern
g.gql("RETURN [x IN [1, NULL, 3] WHERE x > 1] AS kept, [x IN [1, NULL, 3] | x] AS every, [x IN NULL | x] AS none").to_list()
// #{"every": [1, (), 3], "kept": [3], "none": ()}
g.gql("RETURN [x IN 5 | x] AS m")   // error: a list comprehension iterates over a list, not a number (22G03)
```

**List predicates and `reduce`.** `all()`, `any()`, `none()` and `single()` are three-valued,
not counts: a `NULL` list gives `NULL`; an item that decides the answer wins (`all` FALSE at a
FALSE item, `any` TRUE and `none` FALSE at a TRUE one, `single` FALSE at a second TRUE one) and
the later items are not evaluated; otherwise a `NULL` condition gives `NULL`. This is the
openCypher TCK (`Quantifier1` to `Quantifier4`), which Neo4j Cypher 25 follows; Neo4j's manual
words the `NULL` rule of `any()` like that of `all()`, but its implementation and the TCK give
`any(x IN [NULL, 2] WHERE x = 2)` TRUE, as here. `reduce(acc = init, x IN l | expr)` gives
`NULL` for a `NULL` list, `init` for an empty one, and lets `NULL` items flow through `expr`.
Their variables are new names like a comprehension's.

```rhai,graph=modern
g.gql("RETURN any(x IN [NULL, 2] WHERE x = 2) AS a, all(x IN [NULL, 2] WHERE x = 2) AS b, single(x IN [2, NULL] WHERE x = 2) AS c, single(x IN [2, NULL, 2] WHERE x = 2) AS d").to_list()
// #{"a": true, "b": (), "c": (), "d": false}
g.gql("RETURN reduce(s = 0, x IN [1, NULL] | s + x) AS a, reduce(s = 0, x IN [] | s + x) AS b, reduce(s = 0, x IN NULL | s + x) AS c").to_list()
// #{"a": (), "b": 0, "c": ()}
```

**A comprehension variable is a new name.** It may not shadow a variable in scope, and `gql()`
also refuses a name the query uses for another variable anywhere else: one a `WITH` dropped or
renamed, one bound inside a subquery, one a later `LET` defines. This is stricter than Neo4j:
`MATCH (n) WITH n AS m RETURN [n IN [1] | n]` runs there and is an error here; pick another name
(`[x IN [1] | x]`). The variable exists only inside its brackets.

```rhai,graph=modern
g.gql("MATCH (n:person) WITH n AS m RETURN [n IN [1] | n] AS l")   // error: the comprehension variable `n` is a name this query already uses
g.gql("MATCH (n:person) WITH n AS m RETURN [x IN [1] | x] AS l ORDER BY l LIMIT 1").to_list()   // #{"l": [1]}
```

**Pattern comprehension: one item per match, in no defined order.** The list holds one item per
match of the pattern, so the same value may come several times (josh created two programs in
java). Like any subquery it runs on the row's imports, with the trail rule inside its own pattern
([EXISTS](#exists-and-pattern-predicates)); its items come in the order the matches are found,
which depends on the plan. When the order matters, match the pattern in the query, sort the
rows and collect them with `collect_list()`, or sort the list after `gql()`.

```rhai,graph=modern
g.gql("MATCH (p:person {name: 'josh'}) RETURN [(p)-[:created]->(s) | s.lang] AS langs").to_list()   // #{"langs": ["java", "java"]}
g.gql("MATCH (a {name: 'marko'}) RETURN [(a)-[:knows]->(x)<-[:knows]-(a) | x.name] AS one").to_list()   // #{"one": []}: one pattern, one edge twice
```

**Evaluated when reached, once per row.** A pattern comprehension runs its pattern only for the
rows whose evaluation reaches it, at most once per row, like `EXISTS`; one that imports nothing
still runs per row. Comprehensions nested from the same element multiply the work:
`[(m)-->(a) | [(m)-->(b) | ..]]` runs the inner pattern once for every match of the outer one,
like nested Gremlin `map(..fold())` children, and only the resource limits bound a deep chain of
them ([Resource limits](../users_guide/resource_limits.md)).

## COUNT and COLLECT

**An aggregating answer for every row.** `COUNT { }` and `COLLECT { }` run their body once per
row, like `EXISTS`, but read it to the end: `COUNT { }` is the number of rows the body returns
(0, never `NULL`, when there are none) and `COLLECT { }` the list of its one `RETURN` item
(`[]` when there are none). An imported `NULL` variable matches nothing, as in `EXISTS`, so both
give 0 and `[]`. `COLLECT { }` keeps `NULL` items, unlike `collect_list()`, and keeps the body's
row order, so an `ORDER BY` inside it orders the list; without one the order is unspecified, as
for a pattern comprehension.

```rhai,graph=modern
g.gql("MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s {name: 'ripple'}) RETURN p.name AS name, COUNT { (s)<-[:created]-() } AS n, COLLECT { MATCH (s)<-[:created]-(c) RETURN c.name } AS l ORDER BY name").to_list()
// josh 1 ["josh"]; marko, peter, vadas (s is NULL): 0 []
g.gql("MATCH (p:person {name: 'marko'}) RETURN COLLECT { MATCH (p)-[:knows]->(f) RETURN f.lang } AS l, [(p)-[:knows]->(f) | f.lang] AS c").to_list()   // #{"c": [(), ()], "l": [(), ()]}
```

**A deviation: the `RETURN` of a counted body is not evaluated.** When a `COUNT { }` body is one
query whose final `RETURN` only projects (no aggregate, `DISTINCT`, `ORDER BY` or `UNION`, which
need the values), `gql()` counts the body's rows without evaluating the `RETURN` items, nor the
values a projecting `WITH` names only for them: the count is the same, but an expression there
that would fail on a row (a division by zero, a value of the wrong kind) raises no error, while
the same body run as a query (or in `CALL`) raises it. `EXISTS` and `COLLECT { }` evaluate
theirs.

```rhai,graph=modern
g.gql("MATCH (p:person) RETURN p.name AS name, COUNT { MATCH (p)-[:created]->(s) RETURN 1 / 0 AS x } AS n ORDER BY name").to_list()   // josh 2, marko 1, peter 1, vadas 0
g.gql("MATCH (p:person) RETURN COUNT { MATCH (p)-[:created]->(s) RETURN DISTINCT 1 / 0 AS x } AS n")   // error: division by zero: DISTINCT needs the value
g.gql("MATCH (p:person) WHERE EXISTS { MATCH (p)-[:created]->(s) RETURN 1 / 0 AS x } RETURN p.name AS name")   // error: division by zero: EXISTS evaluates its RETURN
```

## UNION and NEXT

**Columns by name.** The operands of a `UNION` are matched by column name, not by position: an
operand may list the same names in another order, and the result takes the first operand's
order. (Whether the standard also allows another order is still to be checked; `gql()` takes
the permissive reading.)
`UNION` removes duplicate rows with GQL's equality, as `DISTINCT` does (`1` and `1.0` are one
value, [DISTINCT](#distinct-and-grouping-equality)); which of two equal rows is kept is
unspecified.

```rhai,graph=modern
g.gql("RETURN 1 AS x UNION RETURN 1.0 AS x").to_list()   // #{"x": 1}: one row
g.gql("RETURN 1 AS x UNION ALL RETURN 1.0 AS x").to_list()   // #{"x": 1}, #{"x": 1.0}
```

**Per operand.** Each operand is a query of its own: its variables are its own, and its
`ORDER BY`, `OFFSET` and `LIMIT` apply to it before the union. The rows of a union come in no
defined order: the operands' rows are not interleaved in any documented way, so an order within
an operand is not an order of the result. Sort or page the whole union after `NEXT`.

**`NEXT` passes on the columns, not the variables.** After `NEXT` the next statement sees the
`RETURN` columns of the statement before it as its variables and nothing else; an element column
is the live element, so a `MATCH` after `NEXT` continues from it. `NEXT` after one plain query is
that query with a `WITH` of the same items, so it gives the same rows and the same plan; after a
`UNION` the union's rows are the input. The order a `RETURN .. ORDER BY` makes before `NEXT` is
not kept by later clauses ([Ordering](#ordering)): sort in the last `RETURN`.

## CALL subqueries

**Per row, joined.** A `CALL` runs its body once for each incoming row, with the imported
variables set to the row's values, and each row the body returns extends the incoming row: an
incoming row with three results becomes three rows, one with none is dropped (`OPTIONAL CALL`
keeps it once, the returned names `NULL`). A body that aggregates without grouping keys always
returns one row (`count` 0, `collect_list` `[]`), so it never drops a row. The body's
`DISTINCT`, `ORDER BY`, `OFFSET` and `LIMIT` apply to one row's results: `ORDER BY .. LIMIT k`
in a body is the top k per row, which a `LIMIT` of the whole query cannot express.

```rhai,graph=modern
g.gql("MATCH (p:person) CALL (p) { MATCH (p)-[:knows]->(f) RETURN f.name AS friend ORDER BY f.age DESC LIMIT 1 } RETURN p.name AS who, friend").to_list()   // #{"friend": "josh", "who": "marko"}
g.gql("MATCH (p:person) OPTIONAL CALL (p) { MATCH (p)-[:knows]->(f) RETURN f.name AS friend ORDER BY f.age DESC LIMIT 1 } RETURN p.name AS who, friend ORDER BY who").to_list()
// josh (), marko josh, peter (), vadas ()
```

**Imports.** The body reads only what it imports: `CALL (a, b)` names them, `CALL (*)` takes
every variable in scope, `CALL { }` and `CALL () { }` take none, and Cypher's importing `WITH`
names them as the body's first clause ([Cypher Aliases](aliases.md#the-cypher-clause-with)). A
name the body binds that is also an outer name it did not import is a new variable of the body:
`MATCH (p) CALL { MATCH (p)-->(x) RETURN x }` matches every `p` in the body, for every outer row.
An imported `NULL` (from an `OPTIONAL MATCH`) matches nothing in the body's patterns, as in
`EXISTS`.

**An uncorrelated body still runs per row.** A body that imports nothing gives the same rows for
every incoming row, but `gql()` does not cache them yet: it runs once for each row, so its cost
multiplies with the rows before the `CALL`. Put such a `CALL` first, or compute the value once
with a `WITH` before the `MATCH` that multiplies the rows:

```rhai,graph=modern
g.gql("MATCH (p:person) CALL { MATCH (s:software) RETURN count(s) AS n } RETURN p.name AS who, n ORDER BY who").to_list()   // four rows with n 2: the count ran four times
g.gql("CALL { MATCH (s:software) RETURN count(s) AS n } MATCH (p:person) RETURN p.name AS who, n ORDER BY who").to_list()   // the same rows: the count ran once
```

**Not lazy inside one row.** A `CALL` builds all the results of its body for one incoming row
before the first of them moves on (Gremlin's `flat_map()`), so a `LIMIT` after the `CALL` stops
the incoming rows, not a body run that has started: `MATCH (p) CALL (p) { MATCH (p)-->{1,}(x)
RETURN x } RETURN x LIMIT 1` still finds every `x` of the first `p`. Bound the body itself with
its own `LIMIT` when only a few results per row are needed. The rows the body builds count
against the [resource limits](../users_guide/resource_limits.md) like any other traversers.

**Labels in the body stay in the body.** Variables the body binds and does not return are gone
after it; reading one is an error, as for a subquery. Returned names must be new names.

## Writes

**What a row sees.** Each clause sees every write of the clauses before it, for all rows; within
one clause the rows are processed in order and each row sees the writes of the rows before it.
This is Neo4j's behaviour for every case the openCypher tests check. It follows from the
executor, which runs a plan **step at a time**: a write step starts only after the read steps
before it consumed their whole input. So a query never matches what it is creating (the
"Halloween problem"): `MATCH (n) INSERT (:X)` creates exactly one node per node that existed
before, `MATCH (a)-[e]->(b) INSERT (a)-[:copy]->(b)` exactly one edge per existing edge, and
`FOR i IN range(1, 3) MATCH (n) INSERT (:Y)` three per node, because each row's `MATCH` ran
before any `INSERT`. No barrier step is added to the plan for this.

```rhai,graph=modern,script
g.gql("MATCH (a)-[e]->(b) INSERT (a)-[:copy]->(b)").iterate();
g.e().has_label("copy").count().next()   // 6
```

**One element per row.** A plan that writes never merges traversers into one with a higher bulk,
and a write step treats a traverser of bulk n as n rows: each row creates its own elements.

**Values are read before the write.** A value a row carries (`WITH n.age AS a`, `LET`, a
`RETURN` column, an aggregate) is computed when the row is built; elements stay live, so
`RETURN n.k` after a write reads the new state, as in Neo4j.

**`NULL` is never stored.** `INSERT ({k: NULL})` stores no `k`; reading it gives `NULL` again.
A `null` stored by Gremlin (`property("k", ())`) also reads as `NULL` in GQL.

**Permissions.** `INSERT` asks `Create` on the labels it creates, exactly like `add_v(labels)` /
`add_e(label)`, and nothing for the properties of the new element. Gremlin's
`add_v(..).property(..)` asks `Create` and `Update` (the check runs on the plan as written,
before the optimizer folds the property into the creation), so a policy that denies only
`Update` allows the `INSERT` and refuses its Gremlin twin
([Writing with GQL](writes.md#permissions)).

**Where writes differ from Neo4j:**

| Neo4j | `gql()` | Why |
|---|---|---|
| a property value must be a scalar or a list of scalars of one kind; a map, a list of mixed kinds, a nested list or a `NULL` inside a list is a type error | stored as given: maps, mixed and nested lists (a `NULL` inside a list stays in the list) | Graphersal's data model stores JSON-like values ([jpath keys](../users_guide/jpath_keys.md)); a [schema](../users_guide/schema_enforcement.md) restricts a property's type where wanted |
| `CREATE` requires `WITH` between a write and a following `MATCH` (Cypher 5) | `MATCH` may follow `INSERT` directly | GQL's linear statements allow it, as Neo4j's Cypher 25 does |

## Type errors surface at evaluation time

An operand of the wrong kind (`123 AND true`, a property read of a number, `'a' + 1`) is a
type error (`22G03`) when a row reaches the expression, never before the run, even when the
kind is already clear from the query text. `gql()` compiles into a Gremlin traversal and its
expressions are evaluated per row inside the traversal's steps, like every Gremlin step's
arguments; so a query whose rows never reach the expression raises nothing:

```rhai,graph=empty
g.gql("RETURN 123 AND true AS x").to_list()   // error: a condition must be a boolean, not a number (22G03)
g.gql("MATCH (n) RETURN n.k AND 123 AS x").to_list()   // [] on the empty graph: no row reaches the expression
```

**A deliberate deviation from Neo4j:** Neo4j (and the openCypher TCK) rejects such a query at
compile time with a `SyntaxError` or `TypeError` (`InvalidArgumentType`) when the operand's kind
is known statically. The TCK cases that expect this are excluded from the measured scope with
this reason ([TCK Compliance](tck_compliance.md#out-of-scope)). Errors that do not depend on a
row (syntax, unknown variables, misplaced aggregates) are raised before the run, as always
([Errors](errors.md#errors-before-the-run)).

## Comparisons across types

Numbers compare by value across integers and floats (`1 = 1.0` is TRUE). Strings, booleans,
UUIDs and dates compare within their own kind. Comparing values of different kinds (a number
with a string) is `NULL`, never an error and never a conversion:

```rhai,graph=modern
g.gql("MATCH (n) RETURN n.name AS n, n.age > 'a' AS cmp ORDER BY n LIMIT 2").to_list()   // #{"cmp": (), "n": "josh"}, #{"cmp": (), "n": "lop"}
```

`=` and `<>` also compare lists (item by item), maps (key by key) and elements (the same vertex
or edge). No value is ever turned into text implicitly: `'a' + 1` is an error, not `'a1'`
(Neo4j converts here; Graphersal does not). Convert explicitly: `'a' || toString(1)` or
`'a' || CAST(1 AS STRING)` ([Conversions](#conversions-strict-cast-lenient-tox)).

## Aggregation

The aggregates follow GQL and, where the standard text could not be checked yet, Neo4j:

- **`NULL` is skipped** by every aggregate except `count(*)`, which counts rows.
- **Typing.** `sum` of integers is an integer, and an overflow is an error (`22003`), never a
  wrap; one float makes it a float. `avg` is always a float. `sum` and `avg` of a non-number
  (a string, a list) are an error (`22G03`); `count` and `collect_list` take any value.
- **`min` / `max`** compare across kinds in the [order of `ORDER BY`](#ordering) (strings
  before numbers), never an error.
- **Empty input.** Without grouping keys, an aggregation of no rows still returns one row:
  `count` 0, `sum` 0, `collect_list` `[]`, `avg` / `min` / `max` `NULL` (Neo4j's values; ISO
  GQL, from SQL, may give `NULL` for `sum`: provisional). With grouping keys there is no group,
  so no row.
- **Group order** is unspecified (today: the order in which the groups first appear); write
  `ORDER BY`.
- **`GROUP BY`** after the `RETURN` items lists the grouping keys explicitly; grouping stays
  implicit, so it changes nothing in the result. A key names a column (`GROUP BY name`) or, beyond
  ISO GQL's references to variables, repeats an item's expression (`GROUP BY p.name` for
  `RETURN p.name AS name, ...`).

```rhai,graph=modern
g.gql("MATCH (n) RETURN count(n.age) AS ages, count(*) AS rows, collect_list(n.lang) AS langs").to_list()
// #{"ages": 4, "langs": ["java", "java"], "rows": 6}
g.gql("MATCH (n:person) RETURN sum(n.age) AS i, sum(n.age * 1.0) AS f, avg(n.age) AS a").to_list()   // #{"a": 30.75, "f": 123.0, "i": 123}
g.gql("MATCH (n) RETURN min(coalesce(n.age, n.lang)) AS lo, max(coalesce(n.age, n.lang)) AS hi").to_list()   // #{"hi": 35, "lo": "java"}
g.gql("MATCH (n:robot) RETURN count(*) AS n, sum(n.age) AS s, avg(n.age) AS a, collect_list(n) AS l").to_list()
// #{"a": (), "l": [], "n": 0, "s": 0}
g.gql("MATCH (n:robot) RETURN n.name AS name, count(*) AS c").to_list()   // no rows
g.gql("MATCH (n:person) RETURN sum(9223372036854775807) AS s")   // error: numeric value out of range (22003)
```

Gremlin differs here: `values("age").sum()` of nothing gives no result, `count()` counts what
reaches it (Gremlin has no `NULL` property), and `min()` of mixed kinds is an error.

## DISTINCT and grouping equality

`RETURN DISTINCT`, `WITH DISTINCT`, grouping keys and `DISTINCT` inside an aggregate use
**GQL's equality**: numbers are equal by value across integers and floats (`1` and `1.0` are one
value), and `NULL`s are equal to each other (they form one group). The value kept for a group is
the first that arrives. Gremlin's `dedup()` keeps `1` and `1.0` apart:

```rhai,graph=modern
g.gql("MATCH (p:person) RETURN DISTINCT CASE WHEN p.age > 30 THEN 1 ELSE 1.0 END AS one").to_list()   // one row
g.v().has_label("person").map(__.choose(__.has("age", P.gt(30)), __.constant(1), __.constant(1.0))).dedup().to_list()   // 1.0, 1: two values
g.gql("MATCH (n) RETURN n.lang AS lang, count(*) AS c ORDER BY lang").to_list()   // #{"c": 2, "lang": "java"}, #{"c": 4, "lang": ()}
```

## FOR: NULL and values that are not lists

`FOR x IN list` (and `UNWIND list AS x`, one meaning for both spellings) gives one row per item
in the list's order. For a list expression that is not a list, Graphersal follows Neo4j (the
ISO text is still to be checked; GQL may make it a type error):

- `NULL` (a missing property included) gives **no row**, like the empty list `[]`;
- any other value that is not a list (a number, a string, a map, a vertex) gives **one row** of
  that value.

```rhai,graph=modern
g.gql("FOR x IN null RETURN x").to_list()   // no row
g.gql("FOR x IN 5 RETURN x").to_list()   // #{"x": 5}
g.gql("MATCH (p:person) FOR t IN p.tags RETURN count(*) AS c").to_list()   // #{"c": 0}: no person has tags
g.gql("FOR x IN [1, null, [2, 3]] RETURN x").to_list()   // #{"x": 1}, #{"x": ()}, #{"x": [2, 3]}: NULL items and nested lists are items
```

`WITH ORDINALITY i` numbers the items from **1**, `WITH OFFSET i` from **0**; both count the
position in the list, not the row: the numbers start again for every row the `FOR` reads. A row
keeps its multiplicity: a list item of a row that occurs twice gives two rows.

```rhai,graph=modern
g.gql("MATCH (p:person) WHERE p.age < 30 FOR x IN ['a', 'b'] WITH OFFSET i RETURN p.name AS name, x, i ORDER BY name, i").to_list()
// marko a 0, marko b 1, vadas a 0, vadas b 1
```

## Lookups use GQL equality

An item compared with a property or an element id (`FOR n IN $names MATCH (p:person {name: n})`,
`WHERE element_id(n) = id`) may run as a direct lookup by id or index instead of a scan
([Plans and Performance](plans.md#for-and-the-correlated-lookup)). The lookup keeps GQL's `=`
exactly, whichever way it runs: `29.0` finds the age `29`, the string `'29'` does not, and a
`NULL` item finds nothing.

```rhai,graph=modern
{ g.create_index(#{label: "person", key: "age"}); g.gql("FOR a IN [29.0, '27', null] MATCH (p:person {age: a}) RETURN a, p.name AS name").to_list() }   // #{"a": 29.0, "name": "marko"}
g.gql("FOR a IN [29.0, '27', null] MATCH (p:person {age: a}) RETURN a, p.name AS name").to_list()   // the same row without the index
```

## WITH: the order of its sub-clauses

The sub-clauses of a `WITH` run in this order: the items (and their aggregation), `DISTINCT`,
`ORDER BY`, `OFFSET` / `SKIP`, `LIMIT`, and last the `WHERE`. So `WITH p ORDER BY p.age LIMIT 3
WHERE p.age > 28` takes the three youngest and then filters them (Cypher's order; to be checked
against the ISO text). A `WITH` that only passes values on (no `DISTINCT`, aggregate, `ORDER BY`,
`OFFSET` or `LIMIT`) changes nothing in how the query runs; it only limits the names in scope.

## Either direction and self-loops

`-[e]-` matches an edge pointing either way. A self-loop (an edge from a vertex to itself)
matches **once**, not once per direction, as in GQL and Neo4j; Gremlin's `both()` sees it twice:

```rhai,graph=empty,script
g.add_v("node").property("name", "a").as("a").add_e("link").to("a").iterate();
g.gql("MATCH (x)-[e:link]-(y) RETURN x.name AS x, y.name AS y").to_list()   // #{"x": "a", "y": "a"}: one row
```

```rhai,graph=empty,script
g.add_v("node").property("name", "a").as("a").add_e("link").to("a").iterate();
g.v().both("link").values("name").to_list()   // "a", "a": two
```

## Ordering

- Without `ORDER BY` the row order is **unspecified**: it may change with the data, the plan
  or the version. Write `ORDER BY` whenever the order matters (every example in this chapter
  does).
- `NULL` sorts **last ascending and first descending** (Neo4j's rule; GQL leaves it to the
  implementation). `NULLS FIRST` / `NULLS LAST` are not supported yet.
- Only the final `ORDER BY` orders the result. The order of a `WITH ... ORDER BY` decides what
  its `OFFSET` / `LIMIT` keep, but later clauses do not preserve it.
- Values of different types sort by type first, in this order (Neo4j's global order where
  Graphersal has the type): map, vertex, edge, list, date/time, UUID, string, boolean, number,
  vector, `NULL`. Within a type: numbers by value, strings by code point, elements by id, lists
  item by item and then by length.
- The sort is stable: rows with equal keys keep their relative order.

```rhai,graph=modern
g.gql("MATCH (n) RETURN n.name AS name, n.age AS age ORDER BY age, name").to_list()
// vadas 27, marko 29, josh 32, peter 35, lop (), ripple ()
g.gql("MATCH (n) RETURN n.name AS name, n.age AS age ORDER BY age DESC, name").to_list()
// lop (), ripple (), peter 35, josh 32, marko 29, vadas 27
g.gql("MATCH (n) RETURN coalesce(n.age, n.lang) AS v ORDER BY v").to_list()
// "java", "java", 27, 29, 32, 35: strings before numbers
```

Gremlin's `order().by("age")` drops the vertices without `age`; `ORDER BY` keeps them.

## Functions

The functions ([The Supported Subset](subset.md#functions)) are pure: the same arguments always
give the same result, so a `LET` or `WITH` name may stand for its expression anywhere. They
follow these rules:

- **Characters.** Every position and length counts characters (Unicode scalar values), never
  bytes: `char_length('héllo')` is 5, `byte_length('héllo')` is 6, and `left`, `right`,
  `substring`, `reverse` and `split(s, '')` never cut a character in two.
- **`NULL` in, `NULL` out.** A function returns `NULL` when any argument is `NULL`, before it
  checks the kinds, except `coalesce`, `nullif` and the `...OrNull` conversions. An unmatched
  `OPTIONAL MATCH` therefore gives `NULL` through every function.
- **Wrong kind is an error.** An argument of the wrong kind fails the query (`22G03`), never a
  silent `NULL` (only the `...OrNull` conversions turn it into `NULL`). An argument that must be
  an integer (`left`, `substring`, `range`, the precision of `round`, the `n` of the list
  `trim`) does not take a float.
- **No `NaN` or infinity from finite numbers.** `sqrt(-1)` (`2201F`), `ln(0)` (`2201E`) and an
  `exp()` or `power()` that overflows (`22003`) are errors, like the arithmetic operators. A `NaN`
  that is already stored passes through `abs`, `floor`, `ceil` and `round` unchanged, and
  `sign(NaN)` is 0. Conversions are not arithmetic: `toFloat('NaN')` and `toFloat('Infinity')`
  give the float values, as in Neo4j.
- **Result types.** The GQL functions keep the argument's type: `abs`, `floor` and `ceil` of an
  integer are integers. `round` (Cypher) is always a float, `sign` always an integer; `sqrt`,
  `exp`, `ln`, `log`, `log10` and `power` are floats; `mod(a, b)` is exactly `a % b`.
- **Bounded results.** A function whose result can be larger than its arguments (`range`,
  `split`, `replace`, `toString` of a list or map, and the operator `||`) computes the size of the
  result first and asks for it against `traversal.max_materialized_bytes` and the memory budget,
  like `collect_list()`. A result that is too large fails the query with the limit error; it is
  never cut short and never runs the process out of memory ([Errors](errors.md#scope-grouping-and-limits)).
- **No plan change.** A condition with a function is evaluated by the GQL filter step and never
  pushed into `has()` or a property index: `WHERE lower(n.name) = 'marko'` scans the label.

```rhai,graph=modern
g.gql("MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s:software {lang: 'none'}) RETURN p.name AS name, upper(s.name) AS u, toString(s) AS t, head(s.tags) AS h ORDER BY name LIMIT 1").to_list()
// #{"h": (), "name": "josh", "t": (), "u": ()}
g.gql("MATCH (n:person {name: 'marko'}) RETURN left(n.name, 2.0) AS x")   // error: left() takes an integer length (22G03)
```

### Conversions: strict `CAST`, lenient `toX`

GQL's `CAST` and Cypher's `toInteger` / `toFloat` / `toBoolean` / `toString` share one set of
conversion rules and differ in what an unconvertible value gives:

| Case | `CAST(x AS T)` | `toX(x)` | `toXOrNull(x)` |
|---|---|---|---|
| a text that is no value of the type (`'abc'` to INTEGER) | error `22018` | `NULL` | `NULL` |
| a value of a kind the type does not take (`true` to FLOAT) | error `22G03` | error `22G03` | `NULL` |
| a number out of range (`1.0e19` to INTEGER) | error `22003` | error `22003` | `NULL` |
| a vertex, edge or path to STRING | error `22G03` | error `22G03` | error `22G03` |

- **To INTEGER:** a float is truncated toward zero (`1.9` is 1, `-1.9` is -1); a text is a
  decimal number, also with a fraction or an exponent, truncated the same way (`'1.9'` is 1,
  `'1e3'` is 1000, computed exactly, without a float in between). `toInteger` also reads Cypher
  integer literals (`'0x1F'` is 31, `'0o11'` is 9, `'1_000'` is 1000) and `true` / `false` as 1 /
  0; `CAST` reads decimal text only, and a boolean is `22G03`.
- **To FLOAT:** a decimal number, `'NaN'`, `'Infinity'`, `'-Infinity'`; `toFloat` also reads Cypher
  literals. Java-only forms (`'1.5f'`, `'1d'`, hexadecimal floats) are not numbers.
- **To BOOLEAN:** `'true'` and `'false'` in any letter case; `CAST` reads `'unknown'` as `NULL`;
  `toBoolean` takes an integer too (0 is FALSE, every other integer TRUE), `CAST` does not.
- **To STRING:** an integer, UUID or date gives the same text as Gremlin's `as_string()`; a float
  always shows a decimal point or an exponent, in Neo4j's (Java's) form: `'1.0'`, `'3.2'`,
  `'1.0E7'`, `'1.0E-4'`, `'1.5E20'` (plain decimal from 0.001 up to 10 000 000); booleans give
  `'true'` / `'false'`; a list or map gives Neo4j's text (`'[1, a, true, null]'`,
  `'{a: 1, b: hi}'`, strings unquoted).
- **Surrounding blanks:** `CAST` skips spaces around the text; the Cypher forms skip every
  character up to U+0020 (spaces, tabs, line breaks), as Java's `String.trim()` does, and
  `toBoolean` any Java whitespace.

```rhai,graph=modern
g.gql("MATCH (n:person {name: 'marko'}) RETURN toString(1.0) AS a, toString(1.5e20) AS b, toString(0.0001) AS c, toString(10000000.0) AS d").to_list()
// #{"a": "1.0", "b": "1.5E20", "c": "1.0E-4", "d": "1.0E7"}
g.inject(1.0).as_string().to_list()   // "1": Gremlin's text of a float
g.gql("MATCH (n:person {name: 'marko'}) RETURN toInteger('1.9') AS a, toInteger('0x1F') AS b, toIntegerOrNull('0x1F') AS c").to_list()   // #{"a": 1, "b": 31, "c": 31}
g.inject("1.9").as_number(GType.LONG).to_list()   // error: Gremlin's as_number(GType.LONG) does not read a fraction in text
g.gql("MATCH (n:person {name: 'marko'}) RETURN CAST('0x1F' AS INTEGER) AS x")   // error: invalid character value for cast (22018)
g.gql("MATCH (n:person {name: 'marko'}) RETURN toInteger(1.0e19) AS x")   // error: out of range for an INTEGER (22003)
```

### Rounding

`round(x [, precision [, mode]])` follows Neo4j and always returns a float:

- One argument, or `precision` 0 without a mode: Java's `Math.round`, a half rounds toward
  positive infinity (`round(2.5)` is 3.0, `round(-2.5)` and `round(-2.5, 0)` are -2.0).
- With a positive `precision`, or with a `mode`: the shortest decimal text of `x` is rounded
  (Java's `BigDecimal.valueOf`), so `round(2.675, 2)` is 2.68 although the float 2.675 is a bit
  below it. The default mode is `'HALF_UP'` (a half away from zero: `round(-1.55, 1)` is -1.6).
- `mode` is one of `'UP'`, `'DOWN'`, `'CEILING'`, `'FLOOR'`, `'HALF_UP'`, `'HALF_DOWN'`,
  `'HALF_EVEN'`, written in **upper case** (as Neo4j, which uses Java's mode names); any other
  text, Java's `'UNNECESSARY'` included, is `22023`. A negative `precision` is `22023` too, and a
  float `precision` `22G03`.
- `NaN` and the infinities pass through. A number beyond the integer range is rounded as it is,
  never clamped: `round(1.0e20)` is `1.0e20`.

Gremlin's `math()` has no `round`; the logarithm of `math("log(_)")` is the natural one, the same
as `ln(x)` and the one-argument `log(x)`. `log(2, x)` and `log(10, x)` are exact for powers of
the base (`log(2, 1024)` is 10.0).

```rhai,graph=modern
g.gql("MATCH (n:person {name: 'marko'}) RETURN round(-1.5) AS a, round(-1.5, 0) AS b, round(-1.5, 0, 'HALF_UP') AS c, round(1.25, 1, 'HALF_EVEN') AS d").to_list()
// #{"a": -1.0, "b": -1.0, "c": -2.0, "d": 1.2}
g.gql("MATCH (n:person {name: 'marko'}) RETURN log(2, 1024) AS a, log(10, 0.001) AS b").to_list()   // #{"a": 10.0, "b": -3.0}
g.gql("MATCH (n:person {name: 'marko'}) RETURN round(1.5, 0, 'half_up') AS x")   // error: round() has no rounding mode "half_up" (22023)
```

### Strings: trim, split, replace

- **Trim.** Every GQL trim form removes the **space** (U+0020) when no character is given:
  `TRIM(s)`, `TRIM(LEADING FROM s)`, `trim(s)`, `btrim(s)`, `ltrim(s)`, `rtrim(s)`. A tab or a
  line break stays. `TRIM(c FROM s)` takes exactly one character `c` (`22027` otherwise);
  `btrim(s, chars)`, `ltrim(s, chars)` and `rtrim(s, chars)` remove any character of the set
  `chars` (`''` removes nothing). `trim(s, x)` with a string `s` is an error that names
  `btrim(s, chars)`; `trim(list, n)` is GQL's list trim.
- **Split.** Empty parts are kept: `split('a,,b,', ',')` is `['a', '', 'b', '']`, and
  `split('', ',')` is `['']`. An empty delimiter splits into characters (`split('', '')` is
  `['']`). With a list of delimiters the text is cut at the leftmost match, and when two
  delimiters match at the same place the one listed first wins (`split('a--b', ['-', '--'])` is
  `['a', '', 'b']`, `split('a--b', ['--', '-'])` is `['a', 'b']`); a list containing `''` splits
  into characters, a `NULL` in the list gives `NULL`, and `[]` gives `[s]`.
- **Replace.** Every non-overlapping occurrence, left to right. An empty `search` inserts the
  replacement before every character and at the end (`replace('abc', '', '-')` is `'-a-b-c-'`,
  Java's rule). The optional fourth argument `limit` (Neo4j 2025.06) replaces at most that many
  occurrences, from the left (`0` changes nothing, a negative limit is `22023`).

Gremlin's string steps differ in these places, so the same-named Gremlin step is a twin only
for plain inputs:

```rhai,graph=modern
g.gql("MATCH (n:person {name: 'marko'}) RETURN split('a,,b', ',') AS s").to_list()   // #{"s": ["a", "", "b"]}
g.inject("a,,b").split(",").to_list()   // ["a", "b"]: Gremlin drops the empty part
g.gql("MATCH (n:person {name: 'marko'}) RETURN replace('abc', '', '-') AS r").to_list()   // #{"r": "-a-b-c-"}
g.inject("abc").replace("", "-").to_list()   // "abc": Gremlin leaves the text unchanged
g.gql("MATCH (n:person {name: 'marko'}) RETURN trim(' \t a \t ') AS t").to_list()   // the tabs stay: "\t a \t"
g.inject(" \t a \t ").trim().to_list()   // "a": Gremlin's trim() removes all whitespace
```

### Elements and paths

- **`startNode(e)` / `endNode(e)`** are the edge's stored source and target, independent of the
  direction the pattern was written in: `MATCH (a)-[e]-(b)` binds `a` to either end, while
  `startNode(e)` is always the source. `n IS SOURCE OF e` and `n IS DESTINATION OF e` test the
  same (TRUE, FALSE, or `NULL` when an operand is `NULL`).
- **`keys(x)` / `properties(x)`** of a vertex or edge list its properties in stored order; a
  property stored as `null` is no key, as `x.key` reads it as `NULL` anyway. Labels never appear
  (`labels(x)` lists them). `keys()` of a map keeps a key whose value is `NULL`
  (`keys({a: 1, b: NULL})` is `['a', 'b']`, as in Neo4j); `properties()` of a map is the map.
- **`nodes(p)` / `relationships(p)`** follow the path in the written order of the pattern; an
  edge matched against its direction (`-[e]-`) keeps its stored direction. A zero-length path
  gives one vertex and `[]`; the `NULL` path of an unmatched `OPTIONAL MATCH` gives `NULL`.

```rhai,graph=modern
g.gql("MATCH (a)<-[e:knows]-(b) RETURN a.name AS a, startNode(e).name AS s, endNode(e).name AS t ORDER BY a").to_list()
// #{"a": "josh", "s": "marko", "t": "josh"}, #{"a": "vadas", "s": "marko", "t": "vadas"}
g.gql("MATCH p = (a {name: 'marko'})-[*0..1]->(b) RETURN b.name AS b, size(nodes(p)) AS n, relationships(p) AS r ORDER BY n, b LIMIT 2").to_list()
// #{"b": "marko", "n": 1, "r": []}, #{"b": "josh", "n": 2, "r": [e[2][1-knows->4]]}
```

### Where functions differ from Neo4j

The [compatibility functions](aliases.md#compatibility-functions) keep Neo4j's meaning, and the
GQL functions follow GQL. These are the places where a query gives a different answer than in
Neo4j, each deliberate:

| Neo4j | `gql()` | Why |
|---|---|---|
| `toInteger(1.0e19)` saturates to the largest integer; `toInteger(NaN)` is 0 | error `22003` | a silent wrong value is worse than an error; `toIntegerOrNull` gives `NULL` |
| `toFloat('1e400')` is `Infinity` | error `22003` (`toFloatOrNull`: `NULL`) | the same rule for both targets |
| `toString(n)` of a node, relationship or path gives vendor text (`'(:A:B)'`) | error `22G03` naming `element_id(x)`, `properties(x)` or `RETURN p` | Neo4j's element text is vendor-specific |
| `CAST` does not exist (`toInteger` and friends are the alternatives) | GQL's strict `CAST`; it reads decimal text only, no Cypher literals (`'0x1F'`) | GQL is normative; the Cypher literals are `toInteger`'s |
| `trim(s)`, `ltrim(s)`, `rtrim(s)` remove any whitespace | spaces only | GQL's `TRIM` removes spaces (to be checked against the ISO text) |
| `floor(3)`, `ceil(3)` are floats | integers (they compare equal) | GQL keeps the argument's type |
| `round(x, p)` takes a float `p`; `range()` takes floats | error `22G03` | an integer argument must be an integer |
| a rounding mode outside Java's names fails with `22000`; `'UNNECESSARY'` is a mode | error `22023`; `'UNNECESSARY'` refused | the code is ours; `'UNNECESSARY'` fails whenever rounding is needed |
| `round()` of a number beyond the integer range | the number itself, never saturated | |
| `left('a', null)` raises an error (per the manual) | `NULL` | GQL's `NULL` rule |
| `sqrt(-1)` is `NaN`, `ln(0)` is `-Infinity`, `exp(1000)` is `Infinity` | errors `2201F`, `2201E`, `22003` | no `NaN` or infinity from finite numbers (GQL requires the logarithm error too) |
| `split(s, '')` splits UTF-16 code units | splits characters | an emoji stays one item |
| range step 0, negative `round` precision: `22N38` / `22N03` | `22023` | the ISO / SQL-heritage code |

`log(2, x)` and `log(10, x)` are exact for powers of the base, and `keys(map)` keeps keys whose
value is `NULL`, both as in Neo4j.

## Ids are strings

`element_id(x)` is the element's id as a **string**, the same value as Gremlin's `id()`. Compare
it with a string: `element_id(n) = '1'`, never `= 1` (a number never equals a string). Vertex and
edge ids are separate spaces in Graphersal, so a vertex and an edge may have the same id; Cypher
`id(x)`, an integer in Neo4j, is refused ([Refused Constructs](refused.md)).

```rhai,graph=modern
g.gql("MATCH (n) WHERE element_id(n) = '1' RETURN n.name AS n").to_list()   // #{"n": "marko"}
g.gql("MATCH (n) WHERE element_id(n) = 1 RETURN n.name AS n").to_list()   // no rows
```

## Labels and properties

- `:A` on a vertex tests whether `A` is in its label set; `labels(v)` lists the set, primary
  label first. An edge has exactly one label, `type(e)`.
- `n.k` reads a property. A [jpath key](../users_guide/jpath_keys.md) or a map-valued property
  reads as a map, and `m.key` reads into it.
- `NULL` from a missing property and a stored `null` are the same value.
