# The Store

For an application that keeps its graph, the **Store** does everything of the previous pages for
you: one directory holds the graph's snapshots and its write-ahead log, every commit is durable
before it returns, and opening the directory brings the graph back exactly as the last commit
left it, also after a crash. One directory = one graph, one writer: "SQLite for graphs".

The directory is usually a path on disk. It can also be [one single file](single_file_store.md)
(`graph.gstore`, for embedded use) or live [in memory](store_backends.md) (every target, the
browser included).

```rust,no_run
use graphersal::persist::{RecoveryTarget, Store, StoreOptions};

let store = Store::create("data", StoreOptions::new())?;      // or Store::open("data", ..)
store.graph().write().traversal_mut().add_v("person").property("name", "ann").to_list()?;
store.mark("after-ann")?;                                      // a point-in-time target
store.checkpoint(Some("nightly"))?;                            // a snapshot of the current state
store.close()?;                                                // a clean close (Drop does it too)

let past = Store::open_read_only("data", RecoveryTarget::Mark("after-ann".into()))?;
assert_eq!(past.graph.vertex_count(), 1);
# Ok::<(), Box<dyn std::error::Error>>(())
```

```bash
graphersal store create data/ --from modern     # the same from the command line
graphersal --graph data/ -e 'g.add_v("person").property("name", "ann").to_list()'
graphersal store info data/
```

## The files

```text
data/
├── GRAPH, GRAPH.copy        identity, lineage, state (two copies of the same small file)
├── LOCK                     held by the one writer (an operating-system lock)
├── BACKUP                   the backup pin: shared while a backup, verify, fork or export reads
├── INTENT                   only while a multi-file operation runs (rollback, prune, ...)
├── marks                    the marks, rebuilt from the WAL when lost
├── snapshots/
│   └── 00000000000000000003/      one snapshot, named by its commit (20 digits)
│       ├── manifest               identity, position, counts, the segment list, every chunk's checksum
│       ├── schema.json            the stored schema
│       ├── v-000000.seg           vertices, sorted by id, in compressed chunks
│       └── e-000000.seg           edges
├── wal/
│   └── 00000000000000000004.wal   write-ahead log segments, named by their first commit (16 MiB each)
└── attic/
    └── 20261008T070510Z-00000000000000000002/   history a rollback moved aside
```

| File | What it is | When it changes |
|---|---|---|
| `GRAPH`, `GRAPH.copy` | the store's identity (`graph_id`, the lineage chain of forks and rollbacks it descends from), its state (closed cleanly or open), the chunk size, and a few recovery facts (the newest WAL segment, the commit of the last clean close, a backup marker) | rarely: open, close, a new WAL segment, a rollback; always both copies, each written atomically |
| `LOCK` | the writer's operating-system lock (`flock` / `LockFileEx`), released when the process ends | created empty by the first writer, never written |
| `BACKUP` | a lock file: backups hold it shared while they copy, `prune` takes it exclusively | created empty by the first backup or prune, never written |
| `INTENT` | the plan of a multi-file operation in progress; an interrupted one is completed at the next open | during a rollback, an attic restore, a prune, a backup increment (in the backup) |
| `marks` | the list of marks, so listing them reads no WAL | after every mark |
| `snapshots/<commit>/` | full copies of the graph; immutable once written | a checkpoint adds one, `prune` removes old ones |
| `wal/<first commit>.wal` | every commit since the oldest snapshot, in order; append-only | every commit and mark appends a record |
| `attic/<time>-<commit>/` | snapshots and WAL that an in-place rollback moved aside, with an `ATTIC` description | a rollback adds an entry; restore and remove take it away |

Every name is relative to the store's root, and no file records a path: move or copy the whole
directory and open it there. The exact byte layouts are the
[Storage Format Specification](format.md).

## The pages of the Store

| Page | |
|---|---|
| [Opening and Closing](store_open_close.md) | create, open, recovery on open, the lock, read-only opens, clean close and Ctrl+C |
| [Commits and Durability](store_commits.md) | fsync per commit, `Durability`, refused records, the durable end, change data capture |
| [Checkpoints and Snapshots](store_checkpoints.md) | manual and automatic checkpoints, the chunk size, `.gsnap` export and import |
| [Marks](store_marks.md) | naming a position; snapshots vs marks |
| [Point in Time](store_point_in_time.md) | targets (commit, time, mark), the time format, read-only views |
| [Fork](store_fork.md) | a past state as a new, writable store |
| [Rollback and the Attic](store_rollback.md) | going back in place, and undoing it |
| [Backup and Restore](store_backups.md) | full, incremental and ZIP backups, restore |
| [Prune, Compact and Retention](store_prune.md) | removing old history explicitly; giving disk space back |
| [Verify](store_verify.md) | the scrub |
| [Damage, Maintenance and Repair](store_damage.md) | what happens when the disk rots; the runbook |
| [Format Versions and Upgrade](store_versions.md) | |
| [Directory Backends](store_backends.md) | `FsDir`, `MemDir`, your own `StoreDir`; relocation and split stores |
| [Single-File Store](single_file_store.md) | the whole store in one `.gstore` file |
| [Your Own Storage](store_own_storage.md) | `Store<S>` over a host's `PersistentStorage`: `open_in`, `create_in` |
| [Creation Parameters](creation_parameters.md) | what is fixed when a store is created (store id, damage policy, chunk size, backend, format version) and the per-open options |

What works while the store is open elsewhere (a dev server, a REPL, another program):

| | While another process writes |
|---|---|
| `info`, `list`, `marks`, `verify`, `fork`, `backup`, `export`, `repair`, `compact-advice`, `attic` (list, changes, fork); `Store::open_read_only`, `Store::inspect` | work (no lock taken) |
| `mark`, `checkpoint`, `prune`, `compact`, `rollback`, `attic restore`/`remove`, `convert`; `Store::open` | refused: "in use" (`PersistError::Locked`); use the dev server's Store menu, or stop the writer |
