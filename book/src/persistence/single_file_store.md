# Single-File Store

A [Store](store.md) is usually a directory. For an application that embeds Graphersal, one
**file** is often handier: one thing to ship, copy, attach or back up. A single-file store holds
exactly what a store directory holds (the snapshots, the write-ahead log, the marks, the attic,
`GRAPH`), with the same guarantees, in one file (native targets; in the browser a store lives
[in memory](store_backends.md#in-memory-memdir)).

```rust,no_run
use graphersal::persist::{Store, StoreOptions};

// A path ending in .gstore is a single file (an existing file is one, whatever its name).
let store = Store::create("graph.gstore", StoreOptions::new())?;  // or Store::create_file(path, ..)
store.graph().write().traversal_mut().add_v("person").property("name", "ann").to_list()?;
store.checkpoint(None)?;
store.close()?;

let store = Store::open("graph.gstore", StoreOptions::new())?;    // or Store::open_file(path, ..)
# Ok::<(), Box<dyn std::error::Error>>(())
```

Every Store operation works on it unchanged: commits and their durability, recovery on open,
checkpoints, marks, read-only views of any commit or time, fork, rollback and the attic, backups
(into a directory, a ZIP, another single file, or memory), prune, verify, maintenance mode and
repair. `persist::SingleFileDir` is the backend; anything that takes a store directory takes it
(`Store::create(SingleFileDir::new(path), ..)`).

## From the command line, the dev server and Python

```bash
graphersal store create graph.gstore --from modern   # or any name with --single-file
graphersal --graph graph.gstore -e 'g.v().count().next()'   # REPL, -e, --server: as on a directory
graphersal store info graph.gstore                   # ... "file  single file, 23.0 KiB", then its live and free bytes
graphersal store compact-advice graph.gstore         # [--min-ratio 0.3] [--min-reclaimable BYTES] [--min-size BYTES]
graphersal store compact graph.gstore                # prune + rewrite (refused while a server has it open)
graphersal store convert graph.gstore graph-dir/     # one file into a directory (a closed store), verified
graphersal store convert graph-dir/ graph2.gstore    # and back into one file
```

The dev server on a single-file store (`graphersal --graph graph.gstore --server`,
`--create-store` creates it) shows the compaction advice in its Store menu under **Disk space**,
with a **Compact** button (it asks first when a compaction is not recommended); a fork of a
single-file store becomes a `.gstore` file next to it (`graph-fork-<target>.gstore`).

```python
with graphersal.Store.create("graph.gstore") as store:     # or Store.create_file(path)
    advice = store.compaction_advice()                      # min_ratio=, min_reclaimable=, min_size=
    if advice["recommended"]:
        store.compact()                                     # {"bytes_freed", ...}
graphersal.Store.convert("graph.gstore", "data")            # single_file=True for the other way
```

## How it works

The file is **log-structured**: every change of a store file is appended as a record (an append
to a file, a whole-file replacement, a new name, a rename, a removal), and from time to time the
whole list of names and where their bytes are (the table) is appended too. Two header slots at
fixed positions, each with a generation number and a checksum, point at the latest table; they
are written alternately. Nothing inside the file is ever overwritten.

- **Crash-safe.** An interrupted write leaves the previous table and every earlier record intact;
  the next open replays the records after the latest table and cuts an interrupted last write.
  Each record says up to where the file had been synced when it was written, so an unreadable
  region that nothing synced follows is the torn tail of a crash, never damage, and a damaged
  region that later synced records follow is damage, never silently cut.
- **Damage** of the store's own files (a snapshot chunk, a WAL record) is found by the store's own
  checksums exactly as on a directory, with the same donors and repair. Damage of the container's
  records (a flipped bit in a record header, a lost record) opens the store read-only in
  [maintenance mode](store_damage.md) (`DamageKind::Container`; `verify` lists it). A damaged
  header slot or table loses nothing: the other generation and the records after it give the
  same names.
- **One writer.** The writer lock is the operating system's lock on the file itself; a second
  writer, in this or another process, gets `PersistError::Locked`. Readers (`store info`,
  `Store::open_read_only`, `verify`, backups) work while a writer has it open.
- **Relocatable.** The file holds no path: move or copy it (while no process has it open) and open
  it there.

## Free space and compaction

A removal (a pruned snapshot, a replaced `GRAPH`, an old table) frees its space only *inside* the
file: the file does not shrink by itself. `store.compact()` gives the space back: it prunes up to
the current commit (the last two snapshots, the WAL between them and every attic entry's base
stay) and then copies the live data into a new file next to the old one and replaces it
atomically (like SQLite's `VACUUM`: the disk needs room for the live data meanwhile, and commits
wait while it runs).

`store.compaction_advice()` says cheaply whether that is worth it (the file's total, live and
garbage bytes, what a prune would free first, the free disk space needed); see
[Prune, Compact and Retention](store_prune.md#is-it-worth-it-the-compaction-advice) for the rules
and `CompactionPolicy`.

The container's byte layout is normative: [format specification, section
14](format.md#14-the-single-file-container).
