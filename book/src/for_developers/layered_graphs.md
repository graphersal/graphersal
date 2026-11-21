# Layered Graphs

A **layered graph** (`graphersal::storage::LayeredGraph`) is a frozen base graph that any number
of layers share, plus a layer of its own: the elements the layer adds, and the base elements it
changes or removes. From outside a layer is an ordinary storage (`GraphStorage`). The typed
traversal API, the Rhai DSL, GQL, saved queries and the session all run on it the same way they
run on a `TraversalGraph`. The base never changes: each layer stores only its own difference.

> **EXPERIMENTAL in 0.6.x**, like the `GraphStorage` trait. Persistence and a few catalog
> kinds are not supported yet (see [Limitations](#limitations)).

What a layer is for and how queries see it (`layer()`, `has_layer()`, `g.layer_diff()`, the
CLI's `--layer`, Python) is on the user page [Layers](../users_guide/layered_graphs.md). This
page describes the storage.

## When to use it

- **Many writers over one large graph.** Give every user, session or agent its own layer over
  one shared product graph. Each layer costs about what it changes, not a copy of the graph.
- **What-if edits.** Try changes on a layer and throw the layer away, or keep it beside the
  untouched base.
- **Tests and experiments.** Start every test case from the same loaded graph without loading
  it again or copying it.

If one graph has a single writer, a plain `TraversalGraph` is simpler and a little faster.

## Example

```rust
use std::sync::Arc;
use graphersal::prelude::*;
use graphersal::storage::{Graph, LayeredGraph};

// The base: built once, then frozen behind an `Arc` that every layer shares.
let base = Arc::new(TraversalGraph::tinkerpop_modern());

// A layer reads exactly like its base until it is written to.
let layer = LayeredGraph::over(Arc::clone(&base)).unwrap();
let graph = Graph::new(layer); // shared form: a lock around the layer, as for any storage

GraphTraversalSource::new(&mut *graph.write())
    .add_v("person")
    .property("name", "ada")
    .iterate()
    .unwrap();
GraphTraversalSource::new(&mut *graph.write())
    .v(None)
    .has("name", "marko")
    .property("age", 30)
    .iterate()
    .unwrap();

let read = graph.read();
let g = GraphTraversalSource::new(&*read);
assert_eq!(g.v(None).count().next().unwrap(), Some(7_i64.into()));
// The base is unchanged; a second layer over it sees none of these writes.
assert_eq!(TraversalGraph::vertex_count(&base), 6);
let other = LayeredGraph::over(Arc::clone(&base)).unwrap();
assert_eq!(GraphTraversalSource::new(&other).v(None).count().next().unwrap(), Some(6_i64.into()));

// Memory: the total is the layer's own; the shared base is reported beside it.
let stats = read.memory_usage().unwrap().unwrap();
assert_eq!(stats.shared_bytes, base.memory_usage().unwrap().unwrap().total_bytes());
assert!(stats.total_bytes() < stats.shared_bytes);
```

`LayeredGraph::default()` is a layer over an empty base.

## How it works

- **Handles** (`LayeredVertex`, `LayeredEdge`) are 8 bytes. One bit of the slot number tells a
  base element from an element the layer added. A base element's handle is the base's own
  handle, so reading it needs no lookup.
- **Dirty bits:** the layer keeps one bit for every base vertex slot and every base edge slot.
  A clear bit means "untouched: read the base". An untouched element costs one bit test and
  never a hash lookup.
- **The delta** holds:
  - the added vertices and edges, in a slot store of its own with its own id and label indexes;
  - a hidden *stub* for each base vertex that gets new incident edges, which also carries the
    copied-up state of a modified base vertex;
  - the copied-up state of each modified base edge;
  - *tombstones* (bitsets) for removed base elements.

  The first write that actually changes a base element copies it up. A write of an identical
  value copies nothing. An element written back to its base state reads the base again.
- **Ids** are unique across the base and the layer. Auto ids continue the base's sequences. A
  removed base element's id is free again in the layer.
- **Units:** the layer keeps its own undo log. A traversal that fails leaves nothing behind,
  nested units are savepoints, and the layer captures changes: commit hooks see the same
  `ChangeSet` a `TraversalGraph` with the same data reports, and dry runs work.
- **Schema and catalog:** the layer copies the base's schema and catalog when it is created.
  From then on it reads and writes only its copies: `set_schema` checks the layer's own data,
  with the errors of a `TraversalGraph`.

## Capabilities

| Capability | Layer |
|---|---|
| `writable` | yes |
| `atomic` (units, rollback, savepoints) | yes |
| `change_capture` (commit hooks, dry runs) | yes |
| `property_indexes` (creating indexes) | yes: the layer defines, drops and redefines indexes in its own catalog (see below) |
| `exact_property_index_lookups` | as the base. The base's indexes answer only while the layer has not touched that element kind; after that the engine scans |
| `durability` | no |
| `isolation`, `concurrent_writers` | as `TraversalGraph`: none; `Graph<LayeredGraph>` serializes writers |

The engine runs the same TinkerPop and openCypher TCK scenarios on a layer as on a
`TraversalGraph`, with the same results. The DSL test suite compares every documented
example and the curated scripts on both. The `GraphStorage` conformance suite passes on a layer.

## Memory

`memory_usage()` on a layer (in the DSL, `g.memory_usage()`) reports the layer's **own**
footprint as `total_bytes`: the delta, its indexes and maps, and the dirty bits (1/8 byte per
base slot, the only part that grows with the base). The base's footprint is reported separately
as `shared_bytes` and is not part of the total. Otherwise every layer would "weigh" the whole
base. The memory of several layers over one base is the sum of their totals plus
`shared_bytes` once.

Measured (estimates, as every `memory_usage` figure is):

| Layer | `total_bytes` | `shared_bytes` |
|---|---|---|
| over the modern graph, untouched | 32 | 4 010 |
| over 50 000 vertices + 50 000 edges, untouched | 12 528 | 17 980 778 |
| the same after 1 % writes (250 added vertices, 250 added edges between base vertices, 250 base vertices modified, 250 base edges dropped) | 206 982 | 17 980 778 |

`shrink_to_fit()` releases the delta's spare capacity. It never touches the base.

The process memory ceiling (`memory.limit`, see
[Resource Limits](../users_guide/resource_limits.md)) checks a layer's unit at its commit the
way it checks a `TraversalGraph`'s, measured on the layer's own footprint: a unit that grows
the layer over the ceiling is rolled back. Without a tracking allocator the estimate leaves out
the shared base, just as it leaves out the other graphs of the process. With the tracking
allocator the live heap counts everything.

`memory_report()` is not layer-aware yet: its per-label figures cover everything the layer
shows (base and delta), and it has no structure section. A per-label breakdown of the delta
alone, with the base as one line, is planned.

## Limitations

- **No persistence yet.** A layer cannot be saved, opened from a Store or journaled. `mark()`
  finds no journal.
- **The base is a `TraversalGraph`** with fewer than 2^31 vertex slots and fewer than 2^31
  edge slots. Nobody can change it while layers share it: it sits behind the `Arc`.
- **Refused**, with `GraphError::Unsupported` ("... not supported on a layered graph yet"):
  - a layer over a base whose schema has vector rules, and a schema with vector rules on a
    layer;
  - compression rule and embedding model definitions in the layer's catalog, and compression
    estimates over element kinds the layer changed.

  A base with plain or unique indexes or compression rules is fine: the layer reads it. The
  layer stores its own values uncompressed. `compression_stats` reports the base's figures.
  The base's unique indexes are enforced over base + delta: every write `TraversalGraph`
  checks (creates, property and path writes, a label added, an edge relabeled) is checked at
  the same point of its write rule, against the base elements the layer neither dropped nor
  copied up plus the layer's own elements, with the same `GraphError::UniqueViolation` (index,
  value, the other element's id). A unique index the base keeps disabled is not enforced and
  `unique_index_duplicates` answers the base's report until the layer drops or redefines it.
- **Indexes the layer defines.** The layer's catalog takes property index definitions as a
  `TraversalGraph`'s does (same validation, the other index kind's definition of the name
  replaced, the same conflict errors), each define and drop recorded in the layer's undo log
  and change set. A definition equal to the base's (an inherited index, or one dropped and
  defined again) reads the base's index in place. Any other builds a base part of its own
  over **every** base element of its label (a full scan, its bytes in the layer's
  `property_index_bytes`, never in `shared_bytes`), so an index on a large base label costs
  that label's index once per layer. A unique define is checked over the layer's data (the
  base elements the layer neither dropped nor copied up plus its own): repeated values keep
  it disabled with the layer's own report, and the front ends refuse it with the same
  `DefinitionError::UniqueIndexDuplicates` as on a `TraversalGraph`. Dropping an index (also
  one of the base) removes it for the layer: lookups, plans and unique checks stop using it,
  the base keeps its own. A rollback rebuilds the overlay from the catalog and data as they
  were.
- **Order.** Unordered traversals list the same elements as a `TraversalGraph` holding the
  same data, and in the same order for every write except these:
  - after a drop followed by a create, a `TraversalGraph` reuses the freed slot and the layer
    appends;
  - after a drop or a label removal, a label lookup lists the base bucket minus the removed
    element, where a `TraversalGraph` moves the bucket's last member into the gap;
  - an edge relabeled to a label that also has added edges is listed before those edges.

  Order is not part of the TinkerPop contract for unordered traversals; use `order()` when it
  matters.
