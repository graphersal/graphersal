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

## The base and its models

`LayeredGraph::over(Arc<TraversalGraph>)` takes the bare base graph. The embedding models of
the base's database are not part of the storage: a Store, a recovered view or a `.gsnap` read
attaches their registry to the `Graph<TraversalGraph>` wrapper, and `Graph::into_inner` leaves
it behind. `graphersal::storage::LayerBase` (EXPERIMENTAL) is the frozen base together with
that registry, a display name and the base's identity:

- `LayerBase::from_graph(Graph<TraversalGraph>)` takes the storage and its registry (a
  poisoned graph is refused); `LayerBase::new(Arc<TraversalGraph>)` takes a graph without
  models.
- A clone is one `Arc`: every layer made from it shares one base graph and one registry. The
  registry's commit hook sits on the frozen base, which never commits, so the shared registry
  never changes under the layers.
- `new_layer()` returns a `Graph<LayeredGraph>` with the registry attached: vector rules on
  catalog models, `embed()`, text `nearest` and `g.embedders()` on the layer use the base's
  models. `LayeredGraph::over_base(&base)` is the bare storage, without models.
- The layer only uses the models: model definitions in the layer's catalog stay refused.
- `base_ref()` is `Option<BaseRef>`, `None` for now. It will name the base's identity (graph
  lineage id, commit sequence and commit time) when layers can be saved, so a saved layer is
  checked against its base.

```rust
use std::sync::Arc;
use graphersal::prelude::*;
use graphersal::storage::{LayerBase, LayeredGraph};

let base = LayerBase::new(Arc::new(TraversalGraph::tinkerpop_modern())).with_name("modern");
assert!(!base.serves_models());
let shared = base.new_layer().unwrap(); // a Graph<LayeredGraph>
let bare = LayeredGraph::over_base(&base).unwrap();
assert!(Arc::ptr_eq(bare.base(), base.graph()));
assert_eq!(shared.read().vertex_count().unwrap(), 6);
```

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
| `layered` | yes (a `TraversalGraph` and other storages: no) |
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

`memory_report()` on a layer has a structure section (`GraphStorage::memory_structure`, built
by `Delta::structure`): the totals of `memory_usage()`, the delta's slots (added elements and
stubs), its id and label indexes and shape table, and `base_stubs`, `removed_base_vertices` and
`removed_base_edges` (`StructureMemory::with_base_changes`). `MemoryReport::shared_bytes` is
`totals.shared_bytes`, and the process components add the base as `graph` with the detail
`shared base`. The rows depend on the scope:

- `MemoryReportScope::All` (the default) walks every visible element, base elements included,
  so a report on a layer equals one on the materialized graph row for row. Only `structure`
  and `shared_bytes` tell that it is a layer.
- `MemoryReportScope::Delta` (`MemoryReportRequest::with_scope`, DSL `#{scope: "delta"}`) skips
  the elements whose `vertex_layer`/`edge_layer` is `Base`. That check is one bit test each, so
  the walk is O(visible elements) in bit tests plus O(delta) in property reads. With an element
  cap (`max_elements`), a first pass counts the layer's own elements to compute the sampling
  step. The rows of a delta report add up to the layer's `memory_usage().property_bytes`
  exactly, as a full report of a `TraversalGraph` does (tested on a 1 % layer). The scope
  applies only when `capabilities().layered` is set. Every other storage owns all it shows, so
  there `Delta` reports the whole graph.

The `indexes` rows come from `property_indexes()`, whose `bytes` are the layer-owned parts
(masked, delta, and an owned base part): `0` for a shared index the layer did not touch.

## Limitations

- **No persistence yet.** A layer cannot be saved, opened from a Store or journaled. `mark()`
  finds no journal.
- **The base is a `TraversalGraph`** with fewer than 2^31 vertex slots and fewer than 2^31
  edge slots. Nobody can change it while layers share it: it sits behind the `Arc`.
- **Refused**, with `GraphError::Unsupported` ("... not supported on a layered graph yet"):
  compression rule and embedding model definitions in the layer's catalog, and compression
  estimates over element kinds the layer changed.
- **Vector rules** run on the layer's own units with `TraversalGraph`'s rule pass (the same
  code, generic over the `RuleLog` trait, `graph/vector_rules.rs`): the layer compiles the
  rules of its own schema (when it is created, at every schema write and its undo), refuses
  a direct write of a rule's `to` inside a live unit through the shared write rules (the same
  `GraphError::DerivedProperty` at the same point), runs the pass at the outermost commit
  before the memory check and in the discard of a dry run, and keeps `apply_change_set` exempt.
  The derived vectors, errors, rollbacks, journal records and change sets equal a
  `TraversalGraph`'s with the layer's data. A base with rules costs nothing at creation; a
  vector recomputed to the value a base element holds stores nothing (the equal-value skip),
  so only an element whose sources changed is copied up, except by the first fill of a rule
  the layer adds or changes on a base label, which copies every element of the label it
  writes. `PlannedFill::base_copies` counts them (exact in `validate_schema`, the label's
  untouched base elements in `diff_schema`), from the new capability
  `StorageCapabilities::layered` and the element origins. Models: the pass finds a catalog
  model in the registry the `Graph` guard attaches to the thread, so a
  `Graph<LayeredGraph>` made by `LayerBase::new_layer()` embeds with the base's models (see
  [The base and its models](#the-base-and-its-models)). A bare layer (`LayeredGraph::over`,
  or `Graph::new` of one) serves only `hash-ngram/1`: a rule on a catalog model of the base
  passes the schema check (the catalog is copied) and its pass fails with
  `GraphError::VectorRuleFailed` ("not served here"), rolling the unit back, as on a
  `Graph<TraversalGraph>` without the model.

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
