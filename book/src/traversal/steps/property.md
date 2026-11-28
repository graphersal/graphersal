# Step - property

`property(key, value)` sets a property on the incoming vertex or edge; the value can be a constant
or a child traversal (its first result; no result leaves the property untouched). The key can be
a `jpath(..)` ([Path Keys](../../users_guide/jpath_keys.md)), and `property(Cardinality, key, value)` and
`property(map)` exist as well.

## A key named `id` or `label`

`property("id", ..)` and `property("label", ..)` store ordinary properties; they never change the
element's id or label (TinkerPop accepts these keys too). `values("label")`, `by("label")` and
`select("label")` on an `element_map()` read such a property, `label()`, `by(T.label)` and
`select(T.label)` the element's label. Where a map leaves the engine with both (an `element_map()`
result in a table or as JSON), the property wins under `"label"`. See
[Properties Named id or label](../../users_guide/id_and_label_keys.md).

```rhai,graph=modern
g.V("1").property("label", "CUSTOM").project("p", "t").by("label").by(T.label).next()   // {"p": "CUSTOM", "t": "person"}
```

## `property(traversal, value)`

The key is the first result of a child traversal run on the incoming traverser (TinkerPop's
`AddPropertyStep` with a key traversal):

```rhai,graph=modern
g.withSideEffect("a", "name").addV().property(__.select("a"), "marko").values("name")   // "marko"
```

The key must be a string; no result or another value is a cast error. The value may be a
traversal too (`property(__.select("k"), __.values("age"))`).

Rust: `property_key_by(key_traversal, value)`, `property_key_by_traversal(key_traversal,
value_traversal)`.

## `property(T.id, ..)` and `property(T.label, ..)`

Right after `addV()`/`addE()` (other `property()` calls may come in between), `property(T.id, v)`
sets the id of the new element and `property(T.label, v)` the label of the new vertex. `v` is a
constant or a child traversal: TinkerPop evaluates the parameters of `AddVertexStep`/`AddEdgeStep`
per traverser, so the traversal runs on the traverser reaching the add step (not on the new
element) and its first result is used. An id must be a string or an integer (an integer becomes its
decimal string); no result or another value fails the traversal, which is rolled back.

```rhai,graph=empty
g.inject(#{id: "p0", name: "a"}, #{id: "p1", name: "b"}).as("m").addV("person").property(T.id, __.select("m").select("id")).property("name", __.select("m").select("name")).id().toList()   // ["p0", "p1"]
```

A traversal label turns the step into `addV(traversal)`; after `addE()`, whose label comes from
`addE(label)`, `property(T.label, ..)` is refused.

Rust: `property_id(v)`, `property_id_by(traversal)`, `property_label(v)`,
`property_label_by(traversal)`.
