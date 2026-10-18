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
