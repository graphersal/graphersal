# Gremlin in Ten Minutes

Gremlin describes a query as a **traversal**: a chain of **steps** that **traversers** flow
through. Each traverser sits on a vertex, an edge or a value; a step moves it, filters it out,
turns it into something else or combines many of them. All examples run on the "modern" graph
([The Property Graph Model](property_graph.md)): **Run in the playground** under a block opens
it in the [playground](../getting_started/playground_tour.md), or pass it to `graphersal -e`.

## Start, filter, walk

`g` is the graph. `v()` starts one traverser on every vertex, `has_label` and `has` keep the ones
that match, `values` turns each into a property value:

```rhai,graph=modern
g.v().has_label("person").values("name")          // marko, vadas, josh, peter
```

```rhai,graph=modern
g.v().has_label("person").has("age", P.gt(30)).values("name")   // josh, peter
```

`out(label)` walks along outgoing edges, `in(label)` along incoming ones, `both(label)` along
either:

```rhai,graph=modern
g.v().has("name", "marko").out("knows").values("name")                  // josh, vadas
```

```rhai,graph=modern
g.v().has("name", "marko").out("knows").out("created").values("name")   // ripple, lop
```

Edges are elements too: `out_e` steps onto the edges, `in_v` to their far end, and an edge can be
filtered by its properties on the way:

```rhai,graph=modern
g.v().has_label("person").out_e("created").has("weight", P.gte(0.5)).in_v().values("name")   // ripple
```

## Remember and come back

`as("x")` names a position of the walk; `select`, `where` and `path` read it later:

```rhai,graph=modern
// who created software together with marko?
g.v().has("name", "marko").as("me")
  .out("created").in("created")
  .where(P.neq("me"))
  .values("name")                                  // peter, josh
```

```rhai,graph=modern
g.v().has("name", "marko").out("created").in("created").path().by("name")
// [marko, lop, peter], [marko, lop, josh], [marko, lop, marko]
```

## Group, count, order

Some steps need the whole stream before they answer: `count`, `group`, `order`, `dedup`.
`by(...)` modulates the step before it, and `__.` starts an anonymous traversal that runs for
each element:

```rhai,graph=modern
g.v().count()                                                           // 6
```

```rhai,graph=modern
g.v().has_label("person").order().by("age", Order.desc).values("name")  // peter, josh, marko, vadas
```

```rhai,graph=modern
g.v().has_label("person").group().by("name").by(__.out("created").count())
// {josh: 2, marko: 1, peter: 1, vadas: 0}
```

## Loops

`repeat` runs a traversal again and again, `times(n)` or `until(condition)` stops it, and
`emit()` also yields the steps in between:

```rhai,graph=modern
// a path from peter to vadas, in either direction along the edges
g.v().has("name", "peter")
  .repeat(__.both().simple_path()).until(__.has("name", "vadas"))
  .path().by("name").limit(1)                      // [peter, lop, marko, vadas]
```

## Results

A traversal without a final step is displayed (a table in the command line and the playground).
In code, a **terminal** step returns the data: `to_list()` all results, `next()` the first one,
`iterate()` runs it for its side effects only.

```rhai,graph=modern,script
let names = g.v().has_label("person").values("name").to_list();   // a list
let marko = g.v().has("name", "marko").next();                     // one vertex
names.len()                                                        // 4
```

## Changing the graph

`add_v`, `add_e`, `property` and `drop` write. Every traversal is one unit: when any step fails,
nothing it wrote stays.

```rhai,graph=modern,script
g.add_v("person").property("name", "ann").property("age", 41).as("a")
  .v().has("name", "marko").add_e("knows").to("a")
  .iterate();
g.v().has("name", "marko").out("knows").values("name")   // ann, josh, vadas
```

## Two spellings

Every TinkerPop step has a snake_case name and its Gremlin camelCase twin, so a query copied
from TinkerPop documentation runs as it is (with double quotes for strings); Graphersal's own
extensions are snake_case only:

```rhai,graph=modern
g.V().hasLabel("person").outE("created").has("weight", P.gte(0.5)).inV().values("name")
```

## Next

- [Graphersal and TinkerPop](graphersal_and_tinkerpop.md): what is the same and what is not;
- [Writing Queries](../dsl/writing_queries.md): the DSL in detail, by topic;
- [Step Reference](../traversal/step_reference.md): the steps one by one.
