# Layered Graphs

A **layered graph** (`graphersal::storage::LayeredGraph`) is a frozen base graph that any number
of layers share, plus a layer of its own: the elements the layer adds, and the base elements it
changes or removes. From outside a layer is an ordinary storage (`GraphStorage`). The typed
traversal API, the Rhai DSL, GQL, saved queries and the session all run on it the same way they
run on a `TraversalGraph`. The base never changes: each layer stores only its own difference.

> **EXPERIMENTAL in 0.7.x**, like the `GraphStorage` trait. A layer can be saved as a layer
> snapshot or kept in a layer Store; a damaged layer Store is not repaired, and a few catalog
> kinds are not supported on a layer yet (see [Limitations](#limitations)).

What a layer is for and how queries see it (`layer()`, `has_layer()`, `g.layer_diff()`, the
CLI's `--layer` and `--layer-store`, the `store` commands' `--base`, Python) is on the user page [Layers](../users_guide/layered_graphs.md). This
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
- `base_ref()` is the base's identity, a `BaseRef` read from the frozen graph
  (`BaseRef::of`): its lineage id, last commit sequence number and that commit's time (always
  `Some` in this version). `LayeredGraph::base_ref()` answers the same for a bare layer. A
  layer snapshot records it and is loaded only over a base of the same identity (below).
- `LayerBase::from_store_read_only(dir, target)` (feature `persist`) opens a Store read-only
  at `target` (no lock, no file changed) and keeps the models of that position;
  `LayerBase::from_snapshot_file(path, &options)` (feature `persist`, native only) reads a
  `.gsnap` and returns the load report beside the base. Both name the base after its location
  (the path, a `MemDir`'s name).
- The identity is stable across loads: every read-only open of the same commit and a `.gsnap`
  export of that commit's snapshot have equal `BaseRef`s (the snapshot records the lineage id,
  commit sequence number and commit time; the WAL replay restores the last two). A commit to
  the base store makes a new identity. `.gsnap` exports copy the store's newest *snapshot*, so
  commits still only in the WAL are not in the export: checkpoint first to export the latest
  commit. Samples and GraphSON/GraphML files get a new lineage id at every load, so they are
  never the same base twice.
- `BaseRef` is `Copy + Hash + Eq`: a host serving many layers pools its bases in a
  `HashMap<BaseRef, LayerBase>` (or `Weak` handles) and drops a second load of a pooled commit.
  The `LayerBase` rustdoc has a pool example; the pool's lifetime and eviction stay the host's.
  [Serving Layers](serving_layers.md) shows a whole server: pool, catalog of names and
  aliases, and MCP sessions pinned to a version.

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

- **Layer snapshots and layer Stores.** A layer implements `PersistentStorage` (load
  sink, replay of journaled change sets, `clear`, the journal slot, its own lineage id and
  auto-id sequences). `persist::write_snapshot` (or `write_graph_snapshot` of a
  `Graph<LayeredGraph>`) writes a **layer snapshot**: only the elements the layer added, as
  ordinary segments, plus a layer file (format spec 7.5, segment kind 128) with the base's
  identity and the layer's changes to the base (`BasePatch` entries by base element id:
  removed vertices and edges, images of changed ones, base definitions it removed). Its size
  follows the delta: a layer touching about 1 % of a 20,000-vertex base wrote under 3 % of the
  base's snapshot. `persist::read_layer_snapshot(reader, options, &base)` (or
  `read_snapshot_into` an empty `LayeredGraph::over(base)`) reads it back over the base:
  the patch first, then the added elements. The kinds must agree (`read_snapshot` refuses a
  layer snapshot, a layer refuses a whole-graph snapshot: `PersistError::SnapshotKindMismatch`)
  and so must the base (`PersistError::LayerBaseMismatch` names both bases: rebuild the layer
  or load it over its own base). The base's own snapshot or Store keeps its identity, so a
  base saved and loaded again carries its layers. A layer snapshot's header, manifest and
  layer file are of file format version 5 (plain snapshots stay at 4), so Graphersal 0.6.x
  and older refuse it as written by a newer version.

  A **layer Store** keeps one layer durable and recoverable on every backend (directory,
  single file, memory): `Store::<LayeredGraph>::create_layer(dir, &base, options)` (or
  `create_layer_with` for a layer built in memory) and `Store::open_layer(dir, &base,
  options)`. Its snapshots are layer snapshots, its WAL holds the layer's commits; an open
  loads the newest layer snapshot over the base and replays the WAL, as for any store. The
  store records only the base's identity (`BaseRef`), not where the base lives: the base may be
  a Store opened read-only, a `.gsnap` or a graph in memory, as long as it is the same graph at
  the same commit. Another base is refused with `PersistError::LayerBaseMismatch` naming both;
  `Store::open` of a layer store is refused with `StoreFailure::LayerStore` (it needs its
  base), `open_layer` of a plain store with `PersistError::SnapshotKindMismatch`; all of them
  before the lock and before any file changes, never damage. `Store::info`/`inspect` name the
  base (`StoreInfo::layer_base`; `graphersal store info` prints it). A checkpoint encodes the
  loaded layer (a merge checkpoint works on whole graphs), so its snapshot stays the size of
  the delta. The embedding models are the base's: the store keeps none, and its graph serves
  the `LayerBase`'s registry.

  The base a layer store was built over is a **creation parameter**
  ([Creation Parameters](../persistence/creation_parameters.md)): every copy carries it. The
  copies and checks of the files work as for a plain store and need no base: backups (full,
  incremental, ZIP, from memory), `restore_backup`/`restore_zip`, `export_packed` (a layer
  `.gsnap`, read with `read_layer_snapshot` over the base), `verify_dir` (every checksum, the
  layer files decoded, the snapshots not loaded; a note says so) and `damage_report_dir`. What
  loads the layer takes the base: `verify` of the open store and `Store::verify_layer_dir`,
  `Store::open_layer_read_only` and `read_only_at` of the open store (views over the base,
  serving its models), `Store::open_layer_backup`, `Store::checkpoint_layer_dir` (always an
  encoded layer snapshot, never a merge); another base is `LayerBaseMismatch`, a plain store
  `SnapshotKindMismatch`, and their plain forms (`open_read_only`, `open_backup`,
  `checkpoint_dir`) refuse a layer store with `StoreFailure::LayerStore`. A restored backup is a
  layer store over the same base: it opens with `open_layer` over that base only. A backup of
  a layer store holds no base, so it is **useless without it**: `BackupReport::layer_base`
  names the base and `BackupReport::warnings` says so (the CLI prints it). The portable form
  is the **materialized export**: `Store::export_materialized(writer, models)` (or
  `WriteOptions::with_materialize(true)` with any snapshot writer) writes the whole graph the
  layer shows as a plain snapshot of file format version 4, which `read_snapshot`, a plain
  Store and older releases read without the base; its size follows the whole graph. A layer
  store's catalog names the base's models; their blobs are the base's, so verification notes
  them instead of reporting them missing, a layer `.gsnap` carries none, and a materialized
  export carries them from the base's registry.

  Forks, rollbacks and the attic work on a layer store and keep it a layer over the same
  base: `fork` and `attic_fork` of the open store and their closed forms
  `Store::fork_layer_dir`/`Store::attic_fork_layer_dir` (with the base; the plain `fork_dir`/
  `attic_fork_dir` refuse a layer store) load the layer over its base and write a new layer
  store over it (a new lineage, the creation parameters carried; blobs of the base are not
  listed as "not carried"); `rollback_to` (to a commit, a time or a mark), `attic_restore` and
  `attic_can_restore` reload the layer over its base in place and never rebuild the base's
  model registry, which the base owns (the layer store keeps no model). `Store::convert`
  copies a layer store byte for byte onto another backend (a new store id, verified without
  the base); `Store::upgrade_in_place` leaves it as it is and `Store::upgrade` refuses it as
  current (`AlreadyCurrent`): a layer store exists only in the current store format.

  Refused for layer stores (`StoreFailure::LayerStore`, nothing written): the **repair**
  (`repair_to`, `repair_dir`): a salvage rebuilds a whole graph from damaged files (chunk
  donors, placeholders for lost elements, divergence against before images, all over a
  `TraversalGraph`), and a layer store's files hold only the layer's changes to a base the
  salvage does not read; a damaged layer store refuses the open with the damage (no
  maintenance mode): restore a backup of it, or rebuild the layer. The **closed-store catalog
  commits** (`import_model_dir`, `set_default_model_dir`, `remove_model_dir`): they change
  embedding models, which a layer store does not keep and a layer refuses to define. And
  creating a layer store from a `.gsnap`.
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
