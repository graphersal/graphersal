# Step - add_v

`addV(label)` adds a vertex, `addV([labels])` a vertex with several labels, `addV()` an unlabeled
one. As a start step (`g.addV(..)`) it adds one vertex; in the middle of a traversal one per
incoming traverser.

## `addV(traversal)`

The label is the first result of a child traversal, run on the incoming traverser (on a `null` seed
when the step starts the traversal), TinkerPop's `AddVertexStep` with a label traversal.

```rhai,graph=modern
g.addV(__.V("1").label()).label()                                   // "person"
g.addV(__.constant("prefix_").concat(__.V("1").label())).label()   // "prefix_person"
```

The result must be a string; no result or another value is a cast error, and the traversal is
rolled back. Directly following `property(key, constant)` calls fold into the step as for the
other forms.

The id of the new vertex comes from a following `property(T.id, v)`, where `v` may be a traversal
run on the incoming traverser, so each vertex gets its own id
(see [property](property.md#propertytid--and-propertytlabel-)):

```rhai,graph=empty
g.inject("p0", "p1").addV("person").property(T.id, __.identity()).id().toList()   // ["p0", "p1"]
```

Rust: `add_v(label)`, `add_v_by(traversal)`.
