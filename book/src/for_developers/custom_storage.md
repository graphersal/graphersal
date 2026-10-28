# Custom Storages

Graphersal's reference storage is `TraversalGraph`, an in-memory graph. A host (a server, an
application) can bring its own in-memory storage instead: any type that implements the
`GraphStorage` trait. The engine, the Rhai DSL, saved queries, the session of the playground and
the dev server (`graphersal-session`) and the persist Store then work over it.

> **EXPERIMENTAL in 0.2.x.** `GraphStorage`, `PersistentStorage`, `AnyGraph` and the conformance
> crate `graphersal-storage-tests` may change in any 0.2.x release. Pin the exact versions
> (`graphersal = "=0.2.0"`, `graphersal-storage-tests = "=0.2.0"`). `graphersal-session` is
> internal and has no stability guarantees at all.

## How it fits together

The engine (`GraphTraversalSource<'g, S>`) is generic over the storage and statically
dispatched: a traversal over a host storage is compiled for that storage, with no virtual call
per element. Everything above the engine holds one object-safe type per layer instead, and makes
one virtual call per *operation* (a terminal, a schema method, a catalog call, a view):

| Layer | Holds | Implemented for |
|---|---|---|
| Rhai DSL, saved queries (`graphersal::script`) | `Arc<dyn AnyGraph>` | every `Graph<S>` (sealed blanket implementation) |
| Session (`graphersal-session`) | `Arc<dyn SessionGraph>`, `Arc<dyn SessionStore>`, `Arc<dyn GraphFactory>` | every `Graph<S>`, every `Store<S>`, `StorageKind<S>` |
| Store (`graphersal::persist`) | `Store<S>`, internally a non-generic core | every `S: PersistentStorage` |

`Graph<S>` (in `graphersal::storage`, also in the prelude) is the shared form of a storage: `S`
behind a read-write lock, with `read()`/`write()` guards that dereference to `S`. `Graph` without
a parameter means `Graph<TraversalGraph>`.

## What a host implements

1. **`GraphStorage`** (required). Only the read methods, the element-inserting and
   element-removing methods and the property writes are required; everything else has a default
   that is correct for a simple, non-transactional storage. The trait's rustdoc ("A guide for
   storage implementers") is the reference: handles that never alias, the write checks and their
   public helpers, units of atomicity, capabilities, change capture, statistics, schema and the
   catalog. The schema's [defaults on create](../users_guide/schema_enforcement.md#defaults-on-create)
   come for free: you implement the primitive `insert_vertex`/`insert_edge` (store exactly this;
   imports, change sets, replay and copies call it), and the queries create through the provided
   `add_vertex`/`add_edge` (`add_vertex_with`/`add_edge_with` with a pinned time), which fill the
   defaults in from `get_schema()` and then call your `insert_vertex`/`insert_edge` (override
   `add_vertex_with`/`add_edge_with` only to avoid that schema copy).
2. **`Default`** (required by the DSL and the session). The session builds every new graph (a
   sample, a file load) by reading it into a `TraversalGraph` and copying it into an
   `S::default()` (through the load sink of a `PersistentStorage`, through the write methods
   otherwise). A whole script runs as one unit that its thread holds across locks
   (`Graph::begin_unit`), on your storage in place: the graph holds the lock upgradable until
   the script's first write and only then opens your storage's unit (`begin_unit_at` with the
   time it pinned from `next_unit_started_at`, both defaulted), so opening and closing a unit
   that never wrote take no exclusive moment. A unit you open yourself through a write guard
   (`graph.write().begin_unit()`) must be closed under that same guard, else other threads read
   its uncommitted changes and a Store checkpoint may persist them.
3. **`Send + Sync + 'static`**, so the graph can be shared between threads.
4. **Change capture** (optional; needed for commit hooks, dry runs and the Store). A storage that
   claims `change_capture` (and therefore `atomic`) in `capabilities()` builds the `ChangeSet` of
   every committed unit itself with the public constructors (`ChangeSet::builder`, one
   `Mutation` constructor per kind). There is no reusable undo log or capture component: the
   storage derives what a unit changed from its own bookkeeping. It embeds a `CommitHooks`
   (`graphersal::storage`) and calls it at its commit point, so the order of the hooks (user
   hooks in registration order, then the journal) is implemented once; the rustdoc of
   `CommitHooks` says when to call each event.
5. **`PersistentStorage`** (optional, feature `persist`; needed for snapshots and the Store): the
   lineage id, the commit position, a load sink outside units, `clear`, the journal slot and
   replay. See [Your Own Storage](../persistence/store_own_storage.md).

**Errors of a backend.** Every trait method that reads or changes state returns a `Result`
(since 0.4.0). The element reads (`vertex_count`, `edge_count`, `vertices`, `edges`,
`vertices_by_label`, `edges_by_label`, `get_vertex_by_id`, `get_edge_by_id`, `out_edges`,
`in_edges`, `vertex_id`, `edge_id`, `vertex_label`, `edge_label`, `vertex_labels`,
`vertex_has_label`, `vertex_properties`, `edge_properties`, `edge_endpoints`, `contains_vertex`,
`contains_edge`) and `unit_started_at` answer `Ok(None)` / `Ok(false)` / an empty label set for
what does not exist and `Err` only when the backend cannot answer; an iterating one returns
`Result<impl Iterator>`, one `Result` for starting the iteration, never one per item (a backend
that can fail mid-iteration collects first, like the defaults of `vertices_by_label` and
`edges_by_label`). The query that read fails with the error, located at its step. The methods
beyond single elements are the unit methods (`begin_unit`, `begin_unit_at`,
`next_unit_started_at`, `rollback_unit`, `unit_changes`, `set_unit_metadata`), `capabilities`,
`last_commit_seq`, `statistics`, the schema, catalog, compression, property index and memory
methods, `shrink_to_fit`, `execution_policy`, and on `PersistentStorage` `clear`, `set_graph_id`,
`raise_position`, `reserve_load` and `finish_load`. A storage whose state lives behind a backend
(a file, a service, a lock of its own) returns `GraphError::storage_failed(operation, source)`
instead of an empty or made-up answer; the defaults return `Ok` of what a storage without the
feature reports. The engine reads these once per query or unit, never per traverser, and fails
the query, import or transaction with the error, before anything executes where it can. A failed
`rollback_unit` still closes the unit and the caller gets `GraphError::RollbackFailed` (naming
the error that caused the rollback), but a storage that returns `Err` from `rollback_unit` is
treated as being in an unknown state: it must either have fully undone the unit (then it returns
`Ok`) or consider itself failed. Behind a `Graph` the failure **poisons the graph**: every later
read, write, unit, traversal, import and session operation on it fails with
`GraphError::Poisoned` (whose help says to reopen or recover the storage), and `Graph::read()` /
`write()` panic. The same holds for a `discard_unit` that fails other than with the `Unsupported`
of a storage without change capture, and for a rollback during a panic's unwinding. There is no
way back in place: the host drops the graph and reopens the storage; `Graph::is_poisoned()` and
`Graph::poison_cause()` tell what happened, and `Graph::into_inner()` still hands the storage out
to close or salvage it. A bare storage (a `&mut S` with `Transactional`) has no graph to poison:
the caller only gets the error. `TraversalGraph` never fails an element
read and its methods are inlined, so the `Result` costs nothing on the hot paths (measured with
the Divan bench before the change). The conformance suite has a storage that fails each of these
groups on purpose (`graphersal-storage-tests`, `failing`: element scans, property reads,
adjacency, the unit clock, and the methods beyond single elements).

Units and transactions come for free: the `Transactional` extension trait (`transaction`,
`transaction_with`, `dry_run`) is implemented for every `GraphStorage` on top of the trait's
unit methods. `dry_run` needs `change_capture`.

## Plugging it in

| Front end | Entry point |
|---|---|
| Engine | `GraphTraversalSource::new(&storage)` (reads), `GraphTraversalSource::new(&mut storage)` (writes) |
| Shared graph | `Graph::new(storage)`; `Graph::new_persistent(storage)` for a `PersistentStorage` (adds `g.export_snapshot(path)` in the DSL) |
| Rhai DSL | `graph_scope(graph)`, `graph_scope_with_options(..)`, every `eval_*` function: they take `impl IntoAnyGraph`, i.e. an `Arc<Graph<S>>` or an `Arc<dyn AnyGraph>` |
| Session | `Session::with_storage(Arc::new(StorageKind::<S>::persistent()))` for a `PersistentStorage` (graphs, `.gsnap` loads, stores in memory), `StorageKind::<S>::new()` for any other storage (graphs only); `session.load_shared(graph, name, kind)` serves a graph the host already holds, `session.attach_store(Arc::new(store), label)` a Store |
| Store | `Store::create_in(dir, storage, options)`, `Store::open_in(dir, options, S::default)`, `open_backup_in`, `open_read_only_in`, `create_from_packed_in`; the [Store page](../persistence/store_own_storage.md) has the table |
| Snapshots | `persist::write_snapshot(&storage, writer, &options)`, `persist::read_snapshot_into(reader, &options, S::default())` |

A `Store` requires the capabilities `atomic` and `change_capture` and fails with
`StoreFailure::MissingCapability` without them. The files a `Store<S>` writes do not depend on
`S`: a store written by a host storage opens as `Store<TraversalGraph>` (in `graphersal store`,
the dev server, Python) and the other way round.

### A complete host

The example below is a test of `graphersal-session` (`crates/graphersal-session/tests/custom_storage_example.rs`),
compiled and run by `cargo test -p graphersal-session`. The `Forwarding` storage of
`graphersal-storage-tests` stands in for the host's storage (see [below](#the-forwarding-pattern)).

```rust,ignore
{{#include ../../../crates/graphersal-session/tests/custom_storage_example.rs:example}}
```

## What stays `TraversalGraph`-only

- **Scratch graphs.** Graphs a script creates itself are always `TraversalGraph`s, whatever the
  host's storage: `GraphSource::*`, `__`, `_g`, the results of `subgraph()`/`cap()`/`to_graph()`
  and `schema.to_graph()`. A host-chosen scratch storage is not planned.
- **`withSideEffect(name, graph)` into a host graph.** A `subgraph()` writes only into a
  `TraversalGraph`; a foreign target fails with `GraphError::Unsupported`. Let the subgraph land
  in a scratch graph and copy what is needed.
- **Compressed properties.** A foreign storage keeps compression rules in its catalog like any
  definition (defined, listed, persisted, dropped), but nothing applies them: `recompress` fails
  with `GraphError::Unsupported` and the compression statistics are absent. The session's
  compression manager lists and edits the rules and says "not supported by this storage" for the
  rest.
- **The undo log and its change-capture derivation.** A foreign storage brings its own (see
  "Change capture" above).
- **The Store's internal working graphs.** The merge checkpoint's delta graph, salvage
  (maintenance mode) and repair work on internal `TraversalGraph`s and write files; they never
  touch the host's graph.
- **The shipped front ends.** The CLI (REPL, one-shot runner), the dev server and its MCP
  endpoint, the Python binding and the WebAssembly playground serve `TraversalGraph`. A host that
  serves its own storage builds on `graphersal-session` instead.
- **Sample graphs and `TraversalGraph::with_execution_policy`.** The sample constructors
  (`TraversalGraph::tinkerpop_modern()`, ...) build `TraversalGraph`s (the session copies them
  into the host storage); the trait method `execution_policy()` gives a host storage the same
  policy hook.

Operations a storage does not implement fail with `GraphError::Unsupported`, which names the
operation; its help points to the storage's documentation and capabilities.

## Testing a storage

### The conformance suite

Add `graphersal-storage-tests` as a dev-dependency (with its `persist` feature to test the
`PersistentStorage` side) and invoke the macro in one test of the implementing crate:

```rust,ignore
graphersal_storage_tests::conformance!(my_storage, base = MyStorage::default() => base);
```

Each contract item becomes its own test; the capability-conditional items (rollback, savepoints,
hooks, change sets, dry runs) run for what `capabilities()` claims. See
[Storage Conformance](storage_conformance.md).

### The `Forwarding` pattern

`graphersal_storage_tests::forwarding::Forwarding` forwards every `GraphStorage` (and, with
`persist`, `PersistentStorage`) method to a wrapped `TraversalGraph`, but has its own element
handle types. Graphersal uses it to prove that the front ends are storage-independent:

- `crates/graphersal/tests/all/foreign_storage_tests.rs` runs every documented DSL example and a
  curated set of mutating, whole-script, dry-run and saved-query scripts on both storages and
  requires the same results;
- `crates/graphersal/tests/all/foreign_store_tests.rs` runs the Store on `Forwarding` over every
  backend (commits, reopen, checkpoints, rollback, backups, ZIP, marks, damage) and opens the
  files with the other storage type;
- `crates/graphersal-session/src/storage/tests.rs` requires the same JSON from a session over
  either storage;
- `GRAPHERSAL_TEST_STORAGE=forwarding` runs the whole TinkerPop suite on it, gated against the
  same baseline;
- the first three run in every `cargo test` of the workspace; the TinkerPop run on `Forwarding` is a
  check before every release.

A host can follow the same pattern for its own storage: run the same scripts on a
`Graph<TraversalGraph>` and on a `Graph<MyStorage>` loaded with the same data and compare the
rendered results. Differences are either bugs of the storage or behaviour the host chose (and
documents).

### Cost

A storage behind `Graph<S>` runs within 1-4 % of `TraversalGraph` itself on the measured
traversals (measured with the `Forwarding` test storage), because the engine is monomorphised for it.
The price is build time: one engine instantiation per storage type a host uses.
