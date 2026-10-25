# Steps - add_label, drop_label, set_label

Graphersal extensions that change the labels of existing elements. TinkerPop labels are immutable
and TinkerPop has no such steps, so these add to Gremlin rather than deviate from it. Each step
changes the element it receives and passes the same element on, so the traversal can continue.

| Step | Element | What it does |
|---|---|---|
| `add_label("L")` | vertex | adds `L` to the vertex's label set (appended; the first label stays the primary one `label()` returns) |
| `drop_label("L")` | vertex | removes `L` from the label set; dropping the last one leaves the vertex unlabeled |
| `set_label("L")` | edge | replaces the edge's one label (an unlabeled edge gets `L`) |

```rhai
g.v(1).add_label("employee").labels().next()        // ["person", "employee"]
g.v(1).drop_label("person").labels().next()         // []
g.e(0).set_label("met").label().next()              // "met"
g.v().has_label("person").add_label("employee")     // every person
```

- **Idempotent.** Adding a label a vertex already carries, dropping one it does not carry, or setting
  the label an edge already has changes nothing and is no error; a traverser with bulk `n` changes
  its element once.
- **Wrong element kind.** `add_label`/`drop_label` on an edge, `set_label` on a vertex, or any of
  them on a value (`g.v().values("name").add_label("x")`) fails; the help names the step for the
  kind that arrived.
- **Schema.** With an `open` or `closed` schema the element must be valid under its new labels, or
  the step fails and the traversal rolls back: in `closed` mode the new label must be declared, a
  vertex may not lose its last label or keep a property no remaining label declares, and every
  connection (incident edges, an edge's endpoints) must stay declared; in both modes a newly
  applying label's `required` keys must be present and the stored values must have its declared
  types. Stored values are not coerced. See [Schema Enforcement](../../users_guide/schema_enforcement.md)
  and [Multi-Label Vertices](../../users_guide/multi_label_vertices.md).
- **Atomic.** Like every traversal, a failing one leaves no label change behind
  ([Transactions](../../users_guide/transactions.md)).
- **Permissions.** `add_label`/`drop_label` ask `Update` on vertex data, `set_label` on edge data
  ([Permissions](../../users_guide/permissions.md)).
- A label containing `"::"` is rejected for vertices (the multi-label encoding of GraphSON and
  GraphML).

## Selecting labeled and unlabeled elements

`has_label()` / `hasLabel()` *without arguments* keeps the elements that have a label at all: a
vertex with a non-empty label set, an edge with a label (a vertex property is labeled with its key,
so it always passes). Its complement `not(__.has_label())` selects the unlabeled ones. There is no
`has_not_label()`. TinkerPop has no zero-argument `hasLabel()`, so this is a Graphersal extension
([TinkerPop Deviations](../../for_developers/tinkerpop_deviations.md)); `has_label([])` (an empty
list) still keeps nothing. `label()` yields `null` for an unlabeled vertex, so
`not(__.label())` does not select them; use `not(__.has_label())`.

```rhai
g.v().has_label().count().next()             // labeled vertices
g.v().not(__.has_label()).count().next()     // unlabeled vertices
g.e().not(__.has_label()).to_list()          // unlabeled edges
```

The optimizer never folds the zero-argument form into the source step (it is not an empty label
list), and `.profile()` shows it as `has_label()`.

A typical use is labelling unlabeled elements before freezing a `closed` schema
([Freeze the structure you have](../../users_guide/schema_enforcement.md#freeze-the-structure-you-have)):

```rhai
g.v().not(__.has_label()).add_label("thing").to_list()
g.e().not(__.has_label()).set_label("link").to_list()
g.set_schema(g.infer_schema(), SchemaMode.closed)
```

Rust: `add_label(label)`, `drop_label(label)`, `set_label(label)` on `GraphTraversalSource` and
`__`; they call `GraphStorage::add_vertex_label`, `remove_vertex_label` and `set_edge_label`.
