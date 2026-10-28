# Directory Backends

The Store is written against a directory abstraction, the `persist::StoreDir` trait
(**EXPERIMENTAL in 0.4.x**, like `GraphStorage`: it may still change; its rustdoc is the
implementer's guide). The byte format is the same on every backend.

| Backend | Where the files live | Targets | Use it for |
|---|---|---|---|
| `persist::FsDir` | a directory of the file system | native | the usual store: a server, an application, the CLI |
| `persist::SingleFileDir` | ONE log-structured file (`.gstore`) | native | embedded use: one thing to ship, copy, attach ([Single-File Store](single_file_store.md)) |
| `persist::MemDir` | memory | every target, WebAssembly included | the browser playground, tests, a server that keeps its graphs in memory but wants rollback, fork and verify |
| your own `StoreDir` | anywhere: an object store, a database, an encrypting wrapper | yours | |

Every function that takes a store directory takes anything that is `IntoStoreDir`: a path (`&str`,
`String`, `&Path`, `PathBuf`: `persist::store_dir_at(path)` decides between `FsDir` and
`SingleFileDir`: an existing file, or a new path ending in `.gstore`, is a single file), an
`FsDir`, a `MemDir` (or `&MemDir`), or any `Arc<dyn StoreDir>`.

## In memory: `MemDir`

```rust
use graphersal::persist::{MemDir, RecoveryTarget, Store, StoreOptions};

let dir = MemDir::named("memory:main");
let store = Store::create(&dir, StoreOptions::new())?;
store.graph().write().traversal_mut().add_v("person").to_list()?;
store.checkpoint(None)?;
let fork = MemDir::new();
store.fork(RecoveryTarget::CommitSeq(0), &fork)?;            // a second store in memory
assert_eq!(Store::open(&fork, StoreOptions::new())?.graph().read().vertex_count(), 0);
# Ok::<(), Box<dyn std::error::Error>>(())
```

- Every Store operation works: commits, checkpoints, marks, read-only views, fork, rollback and
  the attic, verify, backup into another `MemDir`, convert into a directory or a file.
- Syncs are no-ops, the writer lock is an in-process flag, and everything is gone with the last
  clone of the `MemDir` (clones share the files; `deep_copy()` makes an independent copy, the
  way tests simulate a crash image).
- Without threads (WebAssembly) the automatic checkpoint runs when the host calls
  `store.run_due_checkpoint()` (default threshold 4 MiB there).
- `Store::convert(memdir, "data")` writes a store in memory to disk (and back).

The [web playground](playground_store.md) keeps every loaded graph this way.

## On disk: `FsDir`, relocation and split stores

- **Relocatable.** No store file records a path, only names relative to the store's root: move or
  copy the whole directory (while no process has it open) and open it there.
- **Split over disks.** Parts of a store may live on other disks as **symbolic links**: `wal/`,
  `snapshots/`, `attic/`, even one snapshot directory. Links are never resolved or recorded.
- A rollback or an attic restore that moves files between parts on different file systems copies
  them into the target directory, syncs, renames there and only then removes the source; a crash
  in between is completed at the next open.
- Removing a linked directory (prune, attic removal) removes its contents along with the link. A
  link that a rollback moves (one linked snapshot directory) moves as a link: give it an
  **absolute** target.

```bash
mv data/wal /fast-disk/data-wal && ln -s /fast-disk/data-wal data/wal   # the WAL on another disk (store closed)
```

## Writing your own `StoreDir`

The contract, in short (the trait's rustdoc has every detail):

- **Logical names.** Files are named `GRAPH`, `wal/00000000000000000013.wal`,
  `snapshots/<commit>/manifest`, ...: `/`-separated, relative, without empty, `.` or `..` parts,
  `\`, `:` or NUL. "Directories" are namespaces; nothing assumes that two names are two OS files.
- **Writes are appends or whole replacements.** A file is only written at its end (or cut back);
  its content changes as a whole only through `write_atomic`. Lengths are logical bytes.
- **Atomicity per method.** `write_atomic` leaves the old or the new content after a crash, never
  a mix, durable when it returns; `rename` is atomic, durable after `sync_dir`; appended bytes are
  durable after `StoreFile::sync` (a crash may keep any prefix of unsynced appends: the WAL is
  built for that); `truncate` is durable when it returns.
- **Locks.** `lock(LockKind::Writer)` is exclusive and never waits; the backup pin is shared
  (`BackupShared`, for backups) or exclusive (`BackupExclusive`, for prune, never waits). A
  backend shared between processes must lock between processes.
- **Optional capabilities** with default implementations: `space_usage` and `compact` (a backend
  that keeps garbage), `free_space`, `check` (damage of the backend's own metadata, which opens the
  store in maintenance mode), `is_volatile` (front ends say "lost on reload").

| Methods | |
|---|---|
| `location`, `local_path`, `is_volatile` | identity, for diagnostics |
| `entry_kind`, `list`, `file_len` | names |
| `read`, `read_range`, `open_read` | reading (bounded, ranged, streamed) |
| `create_dir_all`, `create`, `open_write`, `append`, `write_atomic` | writing |
| `rename`, `truncate`, `remove_file`, `remove_tree`, `sync_dir` | changing names and lengths |
| `lock` | the writer lock and the backup pin |
| `space_usage`, `compact`, `free_space`, `check` | optional |

The trait is object-safe, so a decorator (for example a transparent encryption layer over any
backend) holds an inner `Box<dyn StoreDir>` and forwards. Test a backend with the same scenarios
as the built-in ones: crash images (a copy of the files at any point), cuts at every offset,
flipped bits; `graphersal`'s own tests (`tests/all/mem_store_tests.rs`,
`single_file_store_tests.rs`) show how.

## Which backend for which server

| Situation | Backend |
|---|---|
| one process serves one graph that must survive restarts | `FsDir` (`graphersal --graph data/ --server`) |
| an application ships its data as one document | `SingleFileDir` (`graph.gstore`) |
| a browser tab, a test, a demo server: history, fork and rollback without a disk | `MemDir` (`Store::convert` to disk when it should stay) |
| many graphs on one server | one store per graph (one directory or one file each) |
