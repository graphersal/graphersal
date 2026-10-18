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
| `gql:project("a", "b", ...)` | builds the row map of `RETURN` in written order, keeping `NULL` columns |
| `gql:filter("<condition>")` | a `WHERE` condition, or the part of it that cannot become `has()`, with GQL's three-valued logic |
| `gql:order("<keys>")` | `ORDER BY` with GQL's `NULL` placement and type order; stable |
| `gql:not_self_loop()` | keeps a self-loop from matching twice in an either-direction edge |
| `gql:error("<GQLSTATUS>")` | the marker of a query that failed to parse or bind; raises the error before the run |

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

Connect patterns through a shared variable where the question allows it; the planner prefers
connected patterns and starts the product at the smaller side.

## Limits, cancellation, permissions

A GQL query is a traversal, so everything that bounds a traversal bounds it too:
[resource limits](../users_guide/resource_limits.md) and [query limits](../users_guide/query_limits.md),
cancellation, the memory budget, and the [permission](../users_guide/permissions.md) check,
which sees the reads of the lowered plan: reading a denied label through `gql()` is refused as
the equivalent Gremlin is. The parser bounds nesting (128 levels, `54001`) so no query can
exhaust the stack.
