# Your Own Storage

A Store keeps a graph in memory and its history on disk. By default that graph is a
`TraversalGraph`, and `Store` is short for `Store<TraversalGraph>`. A host that brings its own
in-memory storage (a type implementing `GraphStorage`) can keep it in a Store too: the Store is
`Store<S>` for any `S` that implements `PersistentStorage`.

> **EXPERIMENTAL.** `PersistentStorage`, like `GraphStorage` and `StoreDir`, may change in any
> 0.4.x release.

The overview of everything a host storage implements and where it plugs in (the DSL, the
session, the Store) is [Custom Storages](../for_developers/custom_storage.md).

## What the storage implements

`PersistentStorage` (in `graphersal::storage`, feature `persist`) is what snapshots and the
Store need on top of `GraphStorage`:

| Group | Methods |
|---|---|
| Position | `graph_id`, `set_graph_id`, `last_committed_at`, `id_sequences`, `raise_position` (raise only, never lower) |
| Load sink (outside units: nothing recorded, no hook, no schema scan) | `load_schema`, `load_definition`, `load_vertex`, `load_edge`, `finish_load` |
| Reload in place | `clear`: empties data, schema, catalog and position, KEEPS the registered hooks and the journal slot |
| Journal | `attach_journal(JournalSlot)`: the storage keeps it in its `CommitHooks`, whose journal slot runs after every user hook |
| Replay | `replay(&ChangeSet, verify)`: has a default over the trait's write methods |

The trait's rustdoc is the implementer's guide.

**Precondition.** The storage must claim the capabilities `atomic` and `change_capture`
(`StorageCapabilities`). The journal writes a commit's record in `before_commit`, so the storage
must be able to roll a vetoed unit back, and it must build the unit's `ChangeSet` itself (with the
public `ChangeSet`/`Mutation` constructors). `Store::create_in`, `Store::open_in` and
`Store::open_backup_in` check this before they touch a file. Without the capabilities they fail
with `PersistError::Store` of cause `StoreFailure::MissingCapability`, and the message names the
missing capability.

## Opening and creating

```rust,ignore
use graphersal::persist::{Store, StoreOptions};

// A new store whose first snapshot is `my_graph` (any S: PersistentStorage + Default).
let store = Store::create_in("data/graph", my_graph, StoreOptions::new())?;
store.graph().write().transaction(|g| { /* commits go to the WAL */ Ok::<_, GraphError>(()) })?;
store.close()?;

// Open it again: `empty` builds the empty instance the snapshot is loaded into.
let store: Store<MyStorage> = Store::open_in("data/graph", StoreOptions::new(), MyStorage::default)?;
let graph: &Arc<Graph<MyStorage>> = store.graph();
```

| `Store<TraversalGraph>` | `Store<S>` |
|---|---|
| `Store::create(dir, options)`, `create_with(dir, graph, options)` | `Store::create_in(dir, graph, options)` |
| `Store::create_from_packed(dir, packed, options)` | `Store::create_from_packed_in(dir, packed, options, empty)` |
| `Store::open(dir, options)` (and `open_file`) | `Store::open_in(dir, options, empty)` (a `SingleFileDir` is a `dir` too) |
| `Store::open_backup(dir, options)` | `Store::open_backup_in(dir, options, empty)` |
| `Store::open_read_only(dir, target)` | `Store::open_read_only_in(dir, target, empty_instance)` |

Every other method (checkpoints, marks, point in time, fork, rollback and the attic, backups,
prune, compact, verify, close) has the same signature for every `S`. `store.read_only_at(target)`
loads the past state into a new `S::default()`. The functions that take a directory and no open
store (`Store::verify_dir`, `Store::inspect`, `Store::fork_dir`, `Store::restore_zip`, ...) work
on the files and are the same for every storage type.

## The files do not depend on the storage

A snapshot and the WAL hold the graph, not the storage. A store written as `Store<MyStorage>`
opens as `Store<TraversalGraph>` (in `graphersal store`, the dev server or the Python binding)
and the other way round. The storage type is therefore not a
[creation parameter](creation_parameters.md).

## What stays a `TraversalGraph` inside

Some operations never touch the host's graph. They work on a `TraversalGraph` of their own and
write files:

- the **merge checkpoint** decodes the touched part of the last snapshot into a small delta
  graph and replays the WAL onto it (beyond the merge bound the host's graph is encoded through
  its read API instead);
- **salvage** (maintenance mode) and **repair** rebuild the state from damaged files. A store
  that opens in maintenance mode serves the salvaged state copied into a new `S` built by
  `empty`. `repair_to` writes a new store, which a host then opens with `Store::open_in`;
- **fork** and **attic fork** load the past state and write a new store; open it with
  `Store::open_in`.

Compressed properties (`ElementProperty::Compressed`) are a `TraversalGraph` feature. A foreign
storage keeps compression rules in its catalog like any definition, and nothing applies them.
