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

## Comparisons across types

Numbers compare by value across integers and floats (`1 = 1.0` is TRUE). Strings, booleans,
UUIDs and dates compare within their own kind. Comparing values of different kinds (a number
with a string) is `NULL`, never an error and never a conversion:

```rhai,graph=modern
g.gql("MATCH (n) RETURN n.name AS n, n.age > 'a' AS cmp ORDER BY n LIMIT 2").to_list()   // #{"cmp": (), "n": "josh"}, #{"cmp": (), "n": "lop"}
```

`=` and `<>` also compare lists (item by item), maps (key by key) and elements (the same vertex
or edge). No value is ever turned into text implicitly: `'a' + 1` is an error, not `'a1'`
(Neo4j converts here; Graphersal does not).

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
