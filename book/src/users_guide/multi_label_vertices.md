# Multi-Label Vertices

Gremlin/TinkerPop vertices carry exactly one label. Cypher and GQL vertices (nodes) carry a
**set** of labels: `MATCH (n:A:B)` matches a node tagged with both `A` and `B`, and `SET n:Admin`
adds a label to an existing node without touching the others. Graphersal's storage carries a
vertex's labels as an ordered, deduplicated set — zero, one, or many — so that the
[GQL front end](../gql/index.md) sits on top of the same storage: `MATCH (n:A&B)` (Cypher
`(n:A:B)`) tests a vertex's label set.

**This page documents storage and Gremlin-facing behavior.** [`g.gql()`](../gql/index.md) reads
label sets with GQL label expressions; it is read-only, so labels are changed with the Gremlin
steps below (there is no `SET n:Admin`).

## Creating a multi-label vertex

`add_v()` accepts a single label, a list of labels, or none:

```rhai,graph=modern
g.addV("Person")                    // one label
g.addV(["Person", "Admin"])         // two labels, in the given order
g.addV()                            // no label
```

```rust,ignore
graph.traversal_mut().add_v(vec!["Person", "Admin"]).next()?;
```

Labels are a **set**, not a multiset: adding the same label twice is a no-op, not an error, and
the first label ever assigned is the vertex's **primary** label.

## `label()` vs `labels()`

- **`label()`** returns the vertex's single primary label — the first one it was given. This
  matches TinkerPop's single-valued `label()` contract, and is what `elementMap()`'s `"label"`
  key keeps returning too (`valueMap()` returns properties only, with no `id` or `label` key). Unaffected by this feature for single-label vertices.
- **`labels()`** returns the full ordered label set as a list. For a single-label vertex this is a
  one-element list; for an unlabeled vertex, an empty list. On an edge (which always carries
  exactly one label), `labels()` returns a single-element list for parity with the vertex side.

```rhai,graph=modern
g.addV(["Person", "Admin"]).label()    // "Person"
g.addV(["Person", "Admin"]).labels()   // ["Person", "Admin"]
```

## `has_label()`'s OR-over-set semantics

`has_label(a, b, ...)` matches a vertex whose label set intersects the candidate set — `true` iff
*any* candidate label is one of the vertex's labels. For a single-label vertex this is exactly the
old "does my one label equal any candidate" check, so single-label queries behave identically to
before multi-label support existed. Under multi-label, it generalizes correctly:

```rhai,graph=modern
g.addV(["Person", "Admin"]).hasLabel("Admin")              // matches
g.addV(["Person", "Admin"]).hasLabel("Person", "Admin")    // matches (either is enough)
g.addV(["Person", "Admin"]).hasLabel("Person").hasLabel("Admin")  // matches (both filters pass independently)
```

## Changing the labels of a vertex

The steps `add_label("L")`/`addLabel` and `drop_label("L")`/`dropLabel` add or remove a single
label on an existing vertex (Graphersal extensions, TinkerPop has no such steps; see
[add_label, drop_label, set_label](../traversal/steps/label_steps.md)):

```rhai
g.V("1").add_label("Admin").labels().next()    // ["person", "Admin"]
g.V("1").drop_label("person").label().next()   // "Admin"
```

They call `GraphStorage::add_vertex_label`/`remove_vertex_label`, which a future Cypher
`SET n:Admin`/`REMOVE n:Admin` will use as well.

Both methods go through the schema and commit only on success (`Ok(true)`/`Ok(false)` as before):
the vertex must be valid under the new label set. Adding a label fails when it is undeclared in a
`Closed` schema, when a field it newly requires is missing, or when an existing property
contradicts its declared type; removing a label fails in `Closed` mode when it would leave the
vertex without a label or with properties no remaining label declares. An undeclared label is
allowed in `Open` mode. In `Closed` mode a label change is also rejected when it would make an
incident edge violate its declared connections (see [Schemas](schema_enforcement.md)).

## Schema

The schema's `vertices` stay keyed by a single label string — there is no composite
multi-label schema constraint (a rule requiring two labels to co-occur, for example). Instead,
schema inference and validation treat each of a multi-label vertex's labels independently:

- **Inference** contributes the vertex's properties to *every* label bucket it carries.
- **Required keys** are enforced in `open` and `closed` mode alike, per label: each label's own
  `required` list must be present (on add, on set and on removal); whether `null` is allowed is
  the declared type's business, independent of `required`.
- **Undeclared properties**: a top-level key that no label declares is allowed when ANY of the
  vertex's labels admits additional properties (the mode's default, or the label schema's own
  `additionalProperties`), the same "any label" rule as edge topology. `closed` also requires every
  label the vertex carries to be declared.
- **Coercion on write** (`Open` and `Closed` mode, on add and on `property()`) follows the
  declaration of the *first* label, in the vertex's label order, that declares the key: a
  canonical UUID string becomes a `uuid`, an `int64` becomes a `float64`. The coerced value is
  then type-checked against *every* label that declares the key, so conflicting declarations
  (`A.k` a `uuid`, `B.k` a `string`) are reported as a type mismatch.

## GraphML representation

GraphML has no native list-valued attribute type. Graphersal represents multiple vertex labels by
joining them into the *same* `labelV` attribute value with a `"::"` delimiter:

```xml
<data key="labelV">Person::Admin</data>
```

A single-label vertex round-trips to an unqualified value with no delimiter, byte-for-byte
identical to a graph with no multi-label vertices. Because `"::"` is the delimiter, a label
containing the literal substring `"::"` is rejected at `add_vertex`/`insert_vertex`/`add_vertex_label` time with a
clear error, rather than silently corrupting a later export.

This delimiter convention is vertex-only — edges keep exactly one label in both Gremlin and
Cypher, and `edge_label()`/the edge schema/edge GraphML export are untouched by multi-label support.

## GraphSON representation

[GraphSON](graphson.md) uses the same convention, as Amazon Neptune does: export writes
`"label":"Person::Admin"`, and import splits a `"::"` label into the label set (empty parts are
dropped). TinkerGraph has no label sets and reads `"Person::Admin"` as one opaque label.
