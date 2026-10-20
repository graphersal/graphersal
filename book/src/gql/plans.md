# Plans and Performance

`gql()` has no executor of its own. A query is parsed, its variables bound, a **planner** picks
where each pattern starts and in which order it expands, and the result is **lowered** into an
ordinary traversal plan: Gremlin steps (`v()`, `has_label()`, `has()`, `out()`, `as()`,
`where()`, `skip()`, `limit()`, ...) plus a few internal steps. That plan then goes through the
same [optimizer](../optimizer/index.md), path analysis, property indexes, limits, cancellation,
permissions and storages as a hand-written traversal. `.profile()` shows it.

```rhai,graph=modern
g.gql("MATCH (p:person) WHERE p.age > 30 RETURN p.name AS name").profile()
```

```text
Step                                                         Call      In     Out
==================================================================================
v(labels: ["person"])                                           1       0       4
has("age", P.gt(30))                                            1       4       2
gql:project("name")                                             1       2       2
Optimizer rules applied: source_filter_pushdown
```

(The profiles on this page leave out the time columns.) The query became
`v().has_label("person").has("age", P.gt(30))` plus a projection, and the optimizer folded the
label into the scan, exactly as it does for the Gremlin form.

## The internal steps

| Step | Does |
|---|---|
| `gql:project("a", "b", ...)` | builds the row map of `RETURN` (or of a `WITH` that builds rows) in written order, keeping `NULL` columns |
| `gql:aggregate([keys], [aggregates], [columns])` | groups the rows by the key values and builds one row map per group (a barrier) |
| `gql:distinct()` | `DISTINCT` on the row maps, with GQL's equality (`1` = `1.0`) |
| `gql:bind("p")` | continues the pattern from the element in a `WITH` column |
| `gql:trail("#1", ...)` | the trail check of a quantified edge: the edge is none of the edges already bound under these labels |
| `gql:filter("<condition>")` | a `WHERE` condition, or the part of it that cannot become `has()`, with GQL's three-valued logic |
| `gql:order("<keys>")` | `ORDER BY` with GQL's `NULL` placement and type order; stable |
| `gql:not_self_loop()` | keeps a self-loop from matching twice in an either-direction edge |
| `gql:for("x IN <list>")` | `FOR` / `UNWIND`: one row per item of the list (`unfold()` with GQL's `NULL` rule); with an ordinal the row is the record of item and position |
| `gql:lookup("(n:label)", "<equality>")` | starts a pattern at the vertices an equality with a value of the row finds, by id (`[id]`) or through a property index (`[index: vertex label.key]`) |
| `gql:value("<expression>")` | the last step of a pattern comprehension's child: the value of its `\|` part for one match, which the host step collects into the list |
| `gql:error("<GQLSTATUS>")` | the marker of a query that failed to parse or bind; raises the error before the run |

`gql:filter`, `gql:project`, `gql:value`, `gql:order`, `gql:aggregate` and `gql:for` may hold
the bodies of the `EXISTS`, `COUNT { }` and `COLLECT { }` subqueries and pattern comprehensions
their expressions contain as child traversals, shown as `\>` lines under the step
([EXISTS and pattern predicates](#exists-and-pattern-predicates),
[comprehensions](#map-projections-and-comprehensions), [COUNT and COLLECT](#count-and-collect)).
`UNION` and `CALL` need no step of their own: they are Gremlin's `union()`, `flat_map()` and
`optional()` ([UNION, NEXT and CALL](#union-next-and-call)).
They exist only in plans of `gql()`; they are not DSL steps. Everything else in a GQL plan is an
ordinary step.

## What becomes `has()`

A `WHERE` condition is split at its top-level `AND`s. A part of the form `var.key <op> value`,
where the value is a literal or a `$parameter`, is pushed into `has()` right at the scan or the
edge it reads, where the optimizer and the property indexes see it; the rest runs in
`gql:filter`. Pushed: `=` (as `P.eq`), `<`, `<=`, `>`, `>=`, `IN` (with `NULL` items removed),
`STARTS WITH`, `ENDS WITH`, `CONTAINS` with a string, `IS NOT NULL`. Never pushed: `<>`,
`IS NULL`, `OR`, `NOT`, conditions over two variables, `NULL` or list operands: there the
semantics of `has()` and GQL's three-valued logic differ.

```rhai,graph=modern
g.gql("MATCH (n:person) WHERE n.age > 30 AND n.name STARTS WITH 'j' RETURN n.name AS n").profile()
g.gql("MATCH (n) WHERE n.age <> 29 RETURN n.name AS n").profile()
```

```text
v(labels: ["person"])                                           1       0       4
has("name", TextP.starting_with("j"))                           1       4       1
has("age", P.gt(30))                                            1       1       1
gql:project("n")                                                1       1       1

v()                                                             1       0       6
gql:filter("n.age <> 29")                                       1       6       3
gql:project("n")                                                1       3       3
```

## Index use

When a [property index](../users_guide/property_indexes.md) exists, the planner starts at the
node with an indexed equality (or `IN`), then one with a label and a property condition, then
one with a label, then the first node; and the property index rule of the optimizer serves the
scan. A parameter works as well as a literal:

```rhai,graph=modern
{ g.create_index(#{label: "person", key: "name"}); g.gql("MATCH (p:person {name: $n})-[:created]->(s) RETURN s.name AS s ORDER BY s", #{n: "josh"}).to_list() }   // #{"s": "lop"}, #{"s": "ripple"}
```

```text
v(labels: ["person"]) [index: vertex person.name]               1       0       1
has("name", P.eq("josh"))                                       1       1       1
out("created")                                                  1       1       2
gql:project("s")                                                1       2       2
Optimizer rules applied: source_filter_pushdown, property_index
```

The planner reads only what every storage offers (`statistics()` and the index catalog), so a
host's [own storage](../for_developers/custom_storage.md) gets the same plans.

## Edges: `out()` when nothing reads the edge

An edge that is not named, filtered or read becomes a plain `out("T")` / `in("T")`, the
cheapest form. A named, filtered or trail-checked edge becomes `out_e("T")` ... `in_v()`, with its
filters on the edge in between. An either-direction edge is a `union()` of both directions with
the self-loop guard:

```rhai,graph=modern
g.gql("MATCH (a:person)-[e:created]->(s) WHERE e.weight > 0.5 RETURN a.name AS a, s.name AS s ORDER BY a").profile()
g.gql("MATCH (a:person)-[:knows]-(b) RETURN b.name AS b").profile()
```

```text
v(labels: ["person"])                                           1       0       4
as("a") [path: labels(a)]                                       1       4       4
out_e("created") [path: labels(a)]                              1       4       4
has("weight", P.gt(0.5)) [path: labels(a)]                      1       4       1
in_v() [path: labels(a)]                                        1       1       1
gql:project("a", "s")                                           1       1       1
gql:order("a")                                                  1       1       1

v(labels: ["person"])                                                     1       0       4
union(__.out("knows"), __.in_e("knows").gql:not_self_loop().out_v())      1       4       4
gql:project("b")                                                          1       4       4
```

## `as()` only where needed

A variable gets a path label (`as("a")`) only when something reads it while the traversal has
moved past it: a `RETURN` column of an earlier node, a join, a trail check, a condition over two
variables. `MATCH (p:person) RETURN p.name` records no path at all. The `[path: ...]` note in
the profile shows what is recorded ([Path Requirement Analysis](../optimizer/path_analysis.md)).
Return only what you need: each returned earlier variable is one more label per traverser.

## Merge points

Between two hops the lowering places a `lazy_barrier()`, a merge point: traversers that reached
the same vertex with the same recorded labels merge into one with a higher bulk, so the next hop
runs once for all of them ([Bulk and Barriers](../users_guide/bulk_and_barriers.md)). The rows
stay the same; the work shrinks. Here four authors (josh twice) become three traversers:

```rhai,graph=modern
g.gql("MATCH (s:software)<-[:created]-(p)<-[:knows]-(m) RETURN m.name AS m").profile()
```

```text
Step                                                         Call      In     Out Bulk
=======================================================================================
v(labels: ["software"])                                         1       0       2    2
in("created")                                                   1       2       4    4
lazy_barrier()                                                  1       4       3    4
in("knows")                                                     1       3       1    2
gql:project("m")                                                1       1       1    2
```

Merging needs equal labels, so it pays off where the middle node is not returned. Returning `p`
labels it, and traversers at the same vertex still merge only with the same `p`.

**Trail checks and merging.** The trail rule (an edge is bound once per `MATCH`) is checked with
`where(P.neq("#1"))` against the earlier edge, which must therefore be labelled. When two edges
of one `MATCH` can have the same label, the first is labelled and the traversers stay apart
until the check has run. The planner skips the check when the label sets cannot overlap
(`-[:knows]->` then `-[:created]->`), so name edge labels in multi-hop patterns where you can:

```rhai,graph=modern
g.gql("MATCH (a)-[:created]->(s)<-[:created]-(b) RETURN a.name AS a, b.name AS b").profile()
```

```text
v()                                                             1       0       6
as("a") [path: labels(a)]                                       1       6       6
out_e("created") [path: labels(a)]                              1       6       4
as("#1") [path: labels(#1,a)]                                   1       4       4
in_v() [path: labels(#1,a)]                                     1       4       4
lazy_barrier() [path: labels(#1,a)]                             1       4       4
in_e("created") [path: labels(#1,a)]                            1       4      10
where(P.neq("#1")) [path: labels(a)]                            1      10       6
out_v() [path: labels(a)]                                       1       6       6
gql:project("a", "b")                                           1       6       6
```

## Disconnected patterns are not optimized yet

Patterns that share no variable (`MATCH (a:person), (s:software)`) are a Cartesian product,
built with a second `v()` in the middle of the traversal. The optimizer does not fold labels or
use indexes for a `v()` there yet, so the second scan runs in full once per row of the first:

```rhai,graph=modern
g.gql("MATCH (a:person), (s:software) RETURN a.name AS a, s.name AS s").profile()
```

```text
v(labels: ["software"])                                         1       0       2
as("s") [path: labels(s)]                                       1       2       2
v() [path: labels(s)]                                           1       2      12
has_label("person") [path: labels(s)]                           1      12       8
gql:project("a", "s")                                           1       8       8
```

A pattern after the first with an equality an id or index serves is the exception: it starts
with a [`gql:lookup`](#for-and-the-correlated-lookup) instead of the second `v()`. Otherwise,
connect patterns through a shared variable where the question allows it; the planner prefers
connected patterns and starts the product at the smaller side. The same holds for a pattern
after a `WITH` or inside an `OPTIONAL MATCH` that shares no variable with what came before, and
for a leading `OPTIONAL MATCH` (its scan runs inside `optional()`).

## LET, FILTER and WITH

`LET`, `FILTER` and a `WITH` that only passes values on add no steps: the planner treats their
names as the expressions they stand for and their conditions like `WHERE`, so a condition on a
`LET` value can still become `has()` where it reads a property directly, and the plan is that of
the same query written without them:

```rhai,graph=modern
g.gql("MATCH (p:person) LET d = p.age / 10 FILTER d = 3 AND p.name STARTS WITH 'j' RETURN p.name AS name").profile()
```

```text
v(labels: ["person"])                                           1       0       4
has("name", TextP.starting_with("j"))                           1       4       1
gql:filter("d = 3")                                             1       1       1
gql:project("name")                                             1       1       1
```

A `WITH` with `DISTINCT`, an aggregate, `ORDER BY`, `OFFSET` or `LIMIT` builds its rows with the
same steps as a `RETURN`. When a later clause reads them, the row is labelled once (`#w1`) and
`gql:bind` moves to the element of a column to continue the pattern; the barrier starts fresh
paths, so nothing recorded before it is carried on:

```rhai,graph=modern
g.gql("MATCH (p:person) WITH p ORDER BY p.age DESC LIMIT 2 MATCH (p)-[:created]->(s) RETURN p.name AS name, s.name AS sw").profile()
```

```text
v(labels: ["person"])                                           1       0       4
as("p") [path: labels(p)]                                       1       4       4
gql:project("p") [path: labels(p)]                              1       4       4
gql:order("p.age DESC")                                         1       4       4
limit(2)                                                        1       4       2
as("#w1") [path: labels(#w1)]                                   1       2       2
gql:bind("p") [path: labels(#w1)]                               1       2       2
out("created") [path: labels(#w1)]                              1       2       3
gql:project("name", "sw")                                       1       3       3
```

## FOR and the correlated lookup

`FOR` (and `UNWIND`) becomes one `gql:for` step; the item is the current object of each row, and
`as("x")` labels it only when a later step reads it after the row moved on. A query that does not
start with `MATCH` starts from one empty row, `inject(0)`:

```rhai,graph=modern
g.gql("FOR x IN [10, 20] WITH ORDINALITY i RETURN x, i").profile()
```

```text
inject(0)                                                       1       0       1
gql:for("x IN [10, 20] WITH ORDINALITY i")                      1       1       2
gql:project("x", "i")                                           1       2       2
```

A `MATCH` after `FOR` runs once per row. Without help that is a scan per item, so a list of 1000
names reads the whole label 1000 times:

```rhai,graph=modern
g.gql("FOR n IN $names MATCH (p:person {name: n}) RETURN n, p.age AS age", #{names: ["vadas", "marko", "nobody"]}).profile()
```

```text
inject(0)                                                       1       0       1
gql:for("n IN $names")                                          1       1       3
as("n") [path: labels(n)]                                       1       3       3
v() [path: labels(n)]                                           1       3      18
has_label("person") [path: labels(n)]                           1      18      12
gql:filter("name: n") [path: labels(n)]                         1      12       2
gql:project("n", "age")                                         1       2       2
```

When an equality between the pattern's vertex and a value of the row can be answered directly,
the planner starts the pattern with `gql:lookup` instead: by **element id** always
(`element_id(n) = id`, either operand order), by a **property** (`{name: n}`, `n.name = x`) when
a [property index](../users_guide/property_indexes.md) on that key exists for every label of the
node. Each row then reads only the vertices the value finds:

```rhai,graph=modern
g.gql("FOR id IN $ids MATCH (n) WHERE element_id(n) = id RETURN id, n.name AS name", #{ids: ["1", "3", "42"]}).profile()
```

```text
inject(0)                                                          1       0       1
gql:for("id IN $ids")                                              1       1       3
as("id") [path: labels(id)]                                        1       3       3
gql:lookup("(n)", "element_id(n) = id") [id] [path: labels(id)]    1       3       2
gql:project("id", "name")                                          1       2       2
```

```rhai,graph=modern
{ g.create_index(#{label: "person", key: "name"}); g.gql("FOR n IN $names MATCH (p:person {name: n}) RETURN n, p.age AS age", #{names: ["vadas", "marko", "nobody"]}).profile() }
```

```text
inject(0)                                                                            1       0       1
gql:for("n IN $names")                                                               1       1       3
as("n") [path: labels(n)]                                                            1       3       3
gql:lookup("(p:person)", "name: n") [index: vertex person.name] [path: labels(n)]    1       3       2
has_label("person") [path: labels(n)]                                                1       2       2
gql:project("n", "age")                                                              1       2       2
```

The lookup checks every vertex it finds with GQL's `=`, so it
returns exactly the rows of the scan ([Semantics](semantics.md#lookups-use-gql-equality)); a
`NULL` value finds nothing, and a value an index cannot serve (a list, `NaN`, a non-string id,
a storage that reports no index) falls back to scanning the label. The same lookup starts any
pattern that is not the first one: a second, disconnected pattern with an indexed equality
(`MATCH (a:person {name: 'marko'}), (b:person) WHERE b.age = a.age + 3` with an index on
`person.age`), or a `MATCH` after a `WITH`. The first pattern of a query is never a
lookup: it is a scan the optimizer serves with the index as usual. Edges are not looked up
(an equality on `element_id(e)` stays a filter).

## Aggregation

An aggregating `RETURN` (or `WITH`) becomes one `gql:aggregate` in place of `gql:project`. It
does not use Gremlin's `group()` / `count()` / `fold()`, whose rules for missing values and empty
input differ from GQL's ([Semantics](semantics.md#aggregation)). It counts a merged traverser by
its bulk, so merge points before it save work without changing a count:

```rhai,graph=modern
g.gql("MATCH (s:software)<-[:created]-(p) RETURN s.name AS sw, count(p) AS authors ORDER BY sw").profile()
g.gql("MATCH (p:person) RETURN DISTINCT p.age > 30 AS old").profile()
```

```text
v(labels: ["software"])                                         1       0       2
as("s") [path: labels(s)]                                       1       2       2
in("created") [path: labels(s)]                                 1       2       4
gql:aggregate(["s.name"], ["count(p)"], ["sw", "authors"])      1       4       2
gql:order("sw")                                                 1       2       2

v(labels: ["person"])                                           1       0       4
gql:project("old")                                              1       4       4
gql:distinct()                                                  1       4       2
```

## OPTIONAL MATCH

An `OPTIONAL MATCH` is Gremlin's `optional()` around the lowered part, its `WHERE` inside. Where
the part does not match, the row continues unchanged and the labels set inside are absent, which
reads as `NULL`. A leading `OPTIONAL MATCH` starts from one row, `inject(0)`:

```rhai,graph=modern
g.gql("MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s) RETURN p.name AS name, count(s) AS n").profile()
g.gql("OPTIONAL MATCH (n:robot) RETURN n").profile()
```

```text
v(labels: ["person"]) [path: labels(s)]                                   1       0       4
as("p") [path: labels(p,s)]                                               1       4       4
optional(__.out("created").as("s")) [path: labels(p,s)]                   1       4       5
  \> out("created") [path: labels(p)]                                     4       4       4
  \> as("s") [path: labels(p,s)]                                          4       4       4
gql:aggregate(["p.name"], ["count(s)"], ["name", "n"])                    1       5       4

inject(0) [path: labels(n)]                                               1       0       1
optional(__.v(labels: ["robot"]).as("n")) [path: labels(n)]               1       1       1
  \> v(labels: ["robot"])                                                 1       1       0
  \> as("n") [path: labels(n)]                                            1       0       0
gql:project("n")                                                          1       1       1
```

After an optional part every later read goes through a label, since the current element differs
between the rows that matched and those that did not.

## Quantified edges

A quantified edge is a `repeat()` of the single edge, with `emit()` and `times()` for its bounds:
`{1,2}` is `repeat(..).emit().times(2)`, `{0,n}` `emit().repeat(..).times(n)`, `{n}`
`repeat(..).times(n)`, `{m,n}` with `m > 1` `emit(__.loops().is(P.gte(m)))`, an unbounded one
has no `times()`. Inside the loop each edge is labelled and checked with `gql:trail` against the
edges of the match so far (trail):

```rhai,graph=modern
g.gql("MATCH (a {name: 'marko'})-[:knows|created]->{1,2}(b) RETURN b.name AS b").profile()
```

```text
v() [path: labels(#1)]                                                                  1       0       6
has("name", P.eq("marko")) [path: labels(#1)]                                           1       6       1
repeat(__.out_e("knows", "created").gql:trail("#1").as("#1").in_v()).times(2).emit()    1       1       5
[loops: 2, max_depth: 2, avg_loops: 1.4]
  \> out_e("knows", "created") [path: labels(#1)]                                       2       4       5
  \> gql:trail("#1") [path: labels(#1)]                                                 2       5       5
  \> as("#1") [path: labels(#1)]                                                        2       5       5
  \> in_v() [path: labels(#1)]                                                          2       5       5
gql:project("b")                                                                        1       5       5
```

The labels keep the traversers of different matches apart, so there is no merge point after a
quantified edge: a quantified pattern costs more than the plain `repeat()` that ignores trail.
The plan never records full paths (`[path: labels(..)]`, never `full`).

## Path variables

A path variable is assembled from labels, not from Gremlin's `path()` (which would record the
full path at every step): every element of the named pattern is labelled, and for a quantified
edge also the vertices inside the loop (`#gv..`):

```rhai,graph=modern
g.gql("MATCH p = (a {name: 'marko'})-[:knows]->{1,2}(b) RETURN p").profile()
```

```text
v() [path: labels(#2,#gv2)]                                                                                       1       0       6
has("name", P.eq("marko")) [path: labels(#2,#gv2)]                                                                1       6       1
as("a") [path: labels(#2,#gv2,a)]                                                                                 1       1       1
repeat(__.out_e("knows").gql:trail("#2").as("#2").in_v().as("#gv2")).times(2).emit() [path: labels(#2,#gv2,a)]    1       1       2
[loops: 2, max_depth: 2, avg_loops: 1.0]
  \> out_e("knows") [path: labels(#2,#gv2,a)]                                                                     2       3       2
  \> gql:trail("#2") [path: labels(#2,#gv2,a)]                                                                    2       2       2
  \> as("#2") [path: labels(#2,#gv2,a)]                                                                           2       2       2
  \> in_v() [path: labels(#2,#gv2,a)]                                                                             2       2       2
  \> as("#gv2") [path: labels(#2,#gv2,a)]                                                                         2       2       2
as("b") [path: labels(#2,#gv2,a,b)]                                                                               1       2       2
gql:project("p")                                                                                                  1       2       2
```

A path variable that nothing reads labels nothing.

## EXISTS and pattern predicates

A subquery is lowered in one of two ways.

**The fast path.** A `[NOT] EXISTS` (or pattern predicate) that is a condition of its own in a
`WHERE` or `FILTER` (alone, or joined to the others by `AND`) becomes Gremlin's
`filter(__.<body>)` or `not(__.<body>)`, the step Gremlin's `where(__..)` / `not(__..)` would
run. The body starts where the row stands: a body that starts at the element the row is on
needs no step and no label, so the "who has no ..." question is exactly its Gremlin twin:

```rhai,graph=modern
g.gql("MATCH (p:person) WHERE NOT (p)-[:created]->() RETURN p.name AS name").profile()
g.gql("MATCH (a:person), (b:person) WHERE (a)-[:knows]->(b) RETURN a.name AS a, b.name AS b").profile()
g.gql("MATCH (s:software) WHERE EXISTS { (p:person) WHERE p.age > 33 } RETURN s.name AS s").profile()
```

```text
v(labels: ["person"])                                                     1       0       4
not(__.out("created"))                                                    1       4       1
  \> out("created")                                                       4       4       4
gql:project("name")                                                       1       1       1

v(labels: ["person"])                                                     1       0       4
as("a") [path: labels(a)]                                                 1       4       4
v() [path: labels(a)]                                                     1       4      24
has_label("person") [path: labels(a)]                                     1      24      16
filter(__.in("knows").where(P.eq("a"))) [path: labels(a)]                 1      16       2
  \> in("knows") [path: labels(a)]                                       16      16       8
  \> where(P.eq("a"))                                                     8       8       2
gql:project("a", "b")                                                     1       2       2

v(labels: ["software"])                                                   1       0       2
filter(__.v(labels: ["person"]).has("age", P.gt(33)))                     1       2       2
  \> v(labels: ["person"])                                                2       2       8
  \> has("age", P.gt(33))                                                 2       8       2
gql:project("s")                                                          1       2       2
```

A body that reads a second imported variable joins it with `where(P.eq("a"))` (or starts from
it with `select()`, or with `gql:bind` from a `WITH` column); a body that imports nothing starts
with its own scan, which the optimizer folds and pushes down as anywhere else. The optimizer
treats the child like any Gremlin child: between two hops it may add a `lazy_barrier()`, as it
does for `where(__.out("knows").out("created"))`.

**The expression hook.** Anywhere else (under `OR`, `XOR`, `CASE`, in a `RETURN` or `WITH`
column, an `ORDER BY` key, an aggregate, a `FOR` list) the `gql:*` step that evaluates the
expression holds the body as a child and runs it when the evaluation of a row needs the answer:
the `Call` and `In` columns of the child show how many rows ran it. Here only the two rows that
`p.age > 30` did not already decide ran `in("knows")`:

```rhai,graph=modern
g.gql("MATCH (p:person) WHERE p.age > 30 OR (p)<-[:knows]-() RETURN p.name AS name").profile()
g.gql("MATCH (p:person) RETURN p.name AS name, EXISTS { (p)-[:knows]->() } AS knows").profile()
```

```text
v(labels: ["person"])                                                     1       0       4
gql:filter("p.age > 30 OR (p)<-[:knows]-()")                              1       4       3
  \> in("knows")                                                          2       2       1
gql:project("name")                                                       1       3       3

v(labels: ["person"])                                                     1       0       4
gql:project("name", "knows")                                              1       4       4
  \> out("knows")                                                         4       4       2
```

Both forms run the same body, so they give the same rows. On an element, the subquery steps
come last ([Semantics](semantics.md#exists-and-pattern-predicates)): after the pushed `has()`
and the plain `gql:filter`, and after the element's `as()`, so a body that reads the element
through its label (a second body clause starting at it) finds it:

```rhai,graph=modern
g.gql("MATCH (a:person) WHERE CASE WHEN EXISTS { MATCH (a)<-[:knows]-() MATCH (a)--() } THEN true ELSE false END RETURN a.name AS name ORDER BY name").profile()
```

```text
v(labels: ["person"])                                                                         1       0       4
as("a") [path: labels(a)]                                                                     1       4       4
gql:filter("CASE WHEN EXISTS { MATCH (a)<-[:knows]-() MATCH (a)--() } THEN t…" (82 chars))    1       4       2
  \> in("knows") [path: labels(a)]                                                            4       4       2
  \> select("a")                                                                              2       2       2
  \> union(__.out(), __.in_e().gql:not_self_loop().out_v())                                   2       2       4
...
gql:project("name")                                                                           1       2       2
gql:order("name")                                                                             1       2       2
```

The global optimizer rule `filter_reorder` may still move a fast-path child before a
`gql:filter` by its cost estimate (`WHERE NOT (p)-->() AND p.name <> 'x'` runs `not(__.out())`
before `gql:filter("p.name <> 'x'")`), as it would in a Gremlin plan; the rows are the same.

## Map projections and comprehensions

A map projection and a list comprehension add no step: the `gql:*` step that evaluates the
expression builds the map or the list from the row's values, with no allocation per item beyond
the result.

```rhai,graph=modern
g.gql("MATCH (p:person) RETURN p {.name, .age} AS m, [x IN [p.age, 30] WHERE x > 28] AS l").profile()
```

```text
v(labels: ["person"])                                           1       0       4
gql:project("m", "l")                                           1       4       4
```

A pattern comprehension is a subquery that collects: like an `EXISTS` in an expression, its body
is a child of the step evaluating it, and the child ends in `gql:value(..)`, the value of the
`|` part per match. The host runs it once per row that reaches it and builds the list from the
child's results (here 4 rows ran `out("knows")`, which found 2 friends), the plan of its
Gremlin twin `project(..).by(__.out("knows").values("name").fold())`:

```rhai,graph=modern
g.gql("MATCH (p:person) RETURN p.name AS name, [(p)-[:knows]->(f) | f.name] AS friends").profile()
g.gql("MATCH (p:person) RETURN p {.name, projects: [(p)-[:created]->(s) | s {.name, n: size([(s)<-[:created]-(c) | c])}]} AS m").profile()
```

```text
v(labels: ["person"])                                                     1       0       4
gql:project("name", "friends")                                            1       4       4
  \> out("knows")                                                         4       4       2
  \> gql:value("f.name")                                                  4       2       2

v(labels: ["person"])                                                     1       0       4
gql:project("m")                                                          1       4       4
  \> out("created")                                                       4       4       4
  \> gql:value("s {.name, n: size([(s)<-[:created]-(c) | c])}")           4       4       4
        \> in("created")                                                  4       4      10
        \> gql:value("c")                                                 4      10      10
```

A comprehension nested in the `|` part is a child of the inner `gql:value`, so it runs once per
match of the outer pattern: nested comprehensions from the same element multiply the work, as
nested Gremlin `fold()` children do ([Semantics](semantics.md#map-projections-and-comprehensions)).
The collected list is checked against the limits before it is built
([Limits](#limits-cancellation-permissions)).

## COUNT and COLLECT

`COUNT { }` and `COLLECT { }` are hosted like an `EXISTS` in an expression: the step evaluating
the expression holds the body as its child and runs it once per row that reaches it. A `COUNT`
child ends in `count()`, which counts the body's traversers with their bulk, and a `COLLECT`
child in `gql:value(..)`, the `RETURN` item, which the host collects in arrival order (after the
body's own `gql:order`). A `COUNT` body whose `RETURN` only projects keeps its `gql:project`,
which `count()` never reads, so its items are not evaluated
([Semantics](semantics.md#count-and-collect)):

```rhai,graph=modern
g.gql("MATCH (p:person) RETURN p.name AS who, COUNT { (p)-[:created]->() } AS n").profile()
g.gql("MATCH (p:person) RETURN p.name AS who, COLLECT { MATCH (p)-[:created]->(s) RETURN s.name ORDER BY s.name } AS sw").profile()
g.gql("MATCH (p:person) WHERE COUNT { (p)-[:created]->() } > 1 RETURN p.name AS who").profile()
```

```text
v(labels: ["person"])                                                     1       0       4
gql:project("who", "n")                                                   1       4       4
  \> out("created")                                                       4       4       4
  \> count()                                                              4       4       4

v(labels: ["person"])                                                     1       0       4
gql:project("who", "sw")                                                  1       4       4
  \> out("created")                                                       4       4       4
  \> gql:project("s.name")                                                4       4       4
  \> gql:order("s.name")                                                  4       4       4
  \> gql:value("s.name")                                                  4       4       4

v(labels: ["person"])                                                     1       0       4
gql:filter("COUNT { (p)-[:created]->() } > 1")                            1       4       1
  \> out("created")                                                       4       4       4
  \> count()                                                              4       4       4
gql:project("who")                                                        1       1       1
```

A whole-query body (`WITH`, an aggregate, `RETURN`) is lowered like a top-level query onto the
child, so an `EXISTS` with one keeps the fast path when it stands alone in a `WHERE`; the child
ends in the body's own steps, and `filter()` asks only whether one traverser comes out:

```rhai,graph=modern
g.gql("MATCH (p:person) WHERE EXISTS { MATCH (p)-[:created]->(s) WITH count(s) AS c WHERE c > 1 RETURN c } RETURN p.name AS who").profile()
```

```text
v(labels: ["person"])                                                                                    1       0       4
filter(__.out("created").gql:aggregate([], ["count(s)"], ["c"]).gql:filter("c > 1").gql:project("c"))    1       4       1
  \> out("created")                                                                                      4       4       4
  \> gql:aggregate([], ["count(s)"], ["c"])                                                              4       4       4
  \> gql:filter("c > 1")                                                                                 4       4       1
  \> gql:project("c")                                                                                    4       1       1
gql:project("who")                                                                                       1       1       1
```

A body that starts from an import with an equality the id or a property index serves starts with
`gql:lookup`, as a later pattern of the query does
([FOR and the correlated lookup](#for-and-the-correlated-lookup)); this holds for `EXISTS`,
`COUNT`, `COLLECT`, pattern comprehensions and `CALL` bodies alike.

## UNION, NEXT and CALL

**`UNION`** is Gremlin's `union()` as the start step: each operand is lowered onto its own child
exactly as a query of its own, so its scan keeps the label, the pushed `has()` and an index, and
`UNION` (not `ALL`) adds `gql:distinct()` after it. Each operand's `ORDER BY` and `LIMIT` stay
inside its child:

```rhai,graph=modern
g.gql("MATCH (p:person) WHERE p.age > 30 RETURN p.name AS name UNION MATCH (s:software) RETURN s.name AS name").profile()
```

```text
union(__.v(labels: ["person"]).has("age", P.gt(30)).gql:project("name"), __.v(labels: ["software"]).gql:project("name"))    1       1       4
  \> v(labels: ["person"])                                                                                                  1       1       4
  \> has("age", P.gt(30))                                                                                                   1       4       2
  \> gql:project("name")                                                                                                    1       2       2
  \> v(labels: ["software"])                                                                                                1       1       2
  \> gql:project("name")                                                                                                    1       2       2
gql:distinct()                                                                                                              1       4       4
```

**`NEXT`** adds no step. After a plain query it is the `WITH` of the same items, so it gives the
same plan (here the `FILTER` after `NEXT` became the `has()` of the scan); after a `UNION` the
next statement continues from the union's row maps:

```rhai,graph=modern
g.gql("MATCH (p:person) RETURN p.name AS name, p.age AS age NEXT FILTER age > 30 RETURN name").profile()
g.gql("MATCH (p:person) RETURN p.name AS name UNION ALL MATCH (s:software) RETURN s.name AS name NEXT RETURN name ORDER BY name LIMIT 3").profile()
```

```text
v(labels: ["person"])                                           1       0       4
has("age", P.gt(30))                                            1       4       2
gql:project("name")                                             1       2       2

union(__.v(labels: ["person"]).gql:project("name"), __.v(labels: ["software"]).gql:project("name"))    1       1       6
  \> v(labels: ["person"])                                                                             1       1       4
  \> gql:project("name")                                                                               1       4       4
  \> v(labels: ["software"])                                                                           1       1       2
  \> gql:project("name")                                                                               1       2       2
gql:project("name")                                                                                    1       6       6
gql:order("name")                                                                                      1       6       6
limit(3)                                                                                               1       6       3
```

**`CALL`** is `flat_map(__.<body>)`: the body is lowered onto the child from the row's current
element, so a body that continues from an import needs no step to reach it, and its
`gql:order` and `limit()` run per call (`Call` 4: once per person). `OPTIONAL CALL` wraps it in
`optional(..)` and labels the body's row (`as("#c<k>")`) so a row without results reads `NULL`
columns. After a `CALL`, the outer variables are read through their labels (`as("p")`):

```rhai,graph=modern
g.gql("MATCH (p:person) CALL (p) { MATCH (p)-[e:created]->(s) RETURN s.name AS top ORDER BY e.weight DESC LIMIT 1 } RETURN p.name AS who, top").profile()
g.gql("MATCH (p:person) OPTIONAL CALL (p) { MATCH (p)-[:created]->(s) RETURN s.name AS sw } RETURN p.name AS who, sw").profile()
```

```text
v(labels: ["person"])                                                                                                    1       0       4
as("p") [path: labels(p)]                                                                                                1       4       4
flat_map(__.out_e("created").as("e").in_v().gql:project("top").gql:order("e.weight DESC").limit(1)) [path: labels(p)]    1       4       3
  \> out_e("created")                                                                                                    4       4       4
  \> as("e") [path: labels(e)]                                                                                           4       4       4
  \> in_v() [path: labels(e)]                                                                                            4       4       4
  \> gql:project("top") [path: labels(e)]                                                                                4       4       4
  \> gql:order("e.weight DESC")                                                                                          4       4       4
  \> limit(1)                                                                                                            4       4       3
gql:project("who", "top")                                                                                                1       3       3

v(labels: ["person"]) [path: labels(#c2)]                                                     1       0       4
as("p") [path: labels(#c2,p)]                                                                 1       4       4
optional(__.flat_map(__.out("created").gql:project("sw")).as("#c2")) [path: labels(#c2,p)]    1       4       5
  \> flat_map(__.out("created").gql:project("sw")) [path: labels(p)]                          4       4       4
        \> out("created")                                                                     4       4       4
        \> gql:project("sw")                                                                  4       4       4
  \> as("#c2") [path: labels(#c2,p)]                                                          4       4       4
gql:project("who", "sw")                                                                      1       5       5
```

`flat_map()` builds the results of one call before the first moves on, so an outer `LIMIT`
does not shorten a call that has started ([Semantics](semantics.md#call-subqueries)); a body
that imports nothing still runs once per row. A `UNION` or `NEXT` inside a body is a `union()`
or the next steps inside the `flat_map` child, run per row.

## Limits, cancellation, permissions

A GQL query is a traversal, so everything that bounds a traversal bounds it too:
[resource limits](../users_guide/resource_limits.md) and [query limits](../users_guide/query_limits.md),
cancellation, the memory budget, and the [permission](../users_guide/permissions.md) check,
which sees the reads of the lowered plan: reading a denied label through `gql()` is refused as
the equivalent Gremlin is (also a label reached only through a quantified edge or an optional
part, or only inside an `EXISTS`, `COUNT` or `COLLECT` body, a pattern comprehension, a `CALL`
body or a later `UNION` operand, whichever way it is lowered: a subquery's child is
checked like a Gremlin child, even when no row reaches it). An unbounded quantifier is bounded
by `repeat.max_loops` and `traversal.max_traversers` ([Semantics](semantics.md#quantified-edges)),
also inside a subquery. The list of a pattern comprehension expands the bulk of its child's
results under `bulk.max_expansion` and is checked against `traversal.max_materialized_bytes` and
the memory budget before it is built, like `collect_list()`. The parser bounds nesting (128
levels, `54001`) so no query can exhaust the stack, `CALL` bodies and whole-query subquery
bodies nest at most 16 levels, and a query holds at most 64 subqueries (pattern comprehensions,
`COUNT { }` and `COLLECT { }` included; `CALL` bodies are not counted) and runs at most 1024
subquery bodies ([Errors](errors.md#subqueries)).
