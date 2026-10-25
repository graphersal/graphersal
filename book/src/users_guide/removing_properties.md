# Removing Properties

`remove_property(keys...)` (Gremlin spelling `removeProperty`) removes properties from the incoming
vertices or edges and lets the element continue. A `jpath(..)` key removes a value *below* a stored
property instead, see [Path Keys (jpath)](jpath_keys.md#removing-remove_propertyjpath).

```groovy
g.V().hasLabel("person").remove_property("age")             // one key
g.V().remove_property(["age", "name"])                       // several keys, as an array
g.E().remove_property()                                     // every property of each edge
g.V("1").remove_property("age").values("name")              // the vertex flows on
```

- A key the element does not carry is not an error; the step is idempotent (a bulk-n traverser
  removes once).
- A value, map, path or property handle as input is a cast error (`vertex` or `edge` expected).
- An empty array is an argument error, because it would silently mean "everything".
- The step is mutating, like `property()`; it is never fused or reordered, and `count()` after it
  does not skip it.

## Relation to `properties(k).drop()`

`properties("age").drop()` also removes the property, but it consumes the traverser (nothing is
emitted) and needs a handle per key. `remove_property("age")` takes the element, names keys directly
and emits the element, so it behaves like `sideEffect(properties("age").drop())` without the handles.

## Why a separate step, and why this name

`property(k, null)` stores a real `null` (see [Upserts](upserts.md)), so removal must be explicit.
`remove` is the verb the storage layer uses for partial removals and the one Cypher/GQL use
(`REMOVE n.prop`); the singular mirrors the writer `property(k, v)`. `drop_properties` was rejected
because `drop()` ends the stream, and a sentinel value in `property(k, <sentinel>)` cannot remove
several keys or all keys and cannot come from a child traversal.

## Deviations

- `remove_property()` is a Graphersal extension. TinkerPop removes with `properties(k).drop()` or,
  in the `@DisallowNullPropertyValues` flavour, with `property(k, null)`.
- `property(k, null)` keeps storing `null`; the three `@DisallowNullPropertyValues` scenarios stay
  failing and are excluded from the compatibility numbers. `remove_property()` is the supported way.
- Removal is checked against a declared schema (`open` and `closed` mode): removing a property
  in its label's `required` list is rejected, the element stays unchanged, and the error names the
  property and the label. This holds for `remove_property()`, `properties(k).drop()` and a
  one-segment `jpath` removal alike. Removing a property that is not required (whether or not its
  type admits `null`) or an undeclared one is allowed, and removing an absent property is a no-op. A property required by any of a
  multi-label vertex's labels cannot be removed. With schema mode `None` nothing is checked.
- Several keys are passed as an array; there is no variadic form (start-up cost of the overload
  table).
