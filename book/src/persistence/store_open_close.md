# Opening and Closing

## Create

```rust,no_run
use graphersal::prelude::*;
use graphersal::persist::{Store, StoreOptions};

let empty = Store::create("data", StoreOptions::new())?;                  // an empty graph
let modern = Store::create_with("modern-data", TraversalGraph::tinkerpop_modern(), StoreOptions::new())?;
let file = std::io::BufReader::new(std::fs::File::open("m.gsnap")?);
let restored = Store::create_from_packed("from-gsnap", file, StoreOptions::new())?;  // from a .gsnap
# Ok::<(), Box<dyn std::error::Error>>(())
```

The directory must be missing or empty. A new store has one snapshot (commit 0, the initial
graph) and an empty WAL, and is open read-write when `create` returns. A path ending in
`.gstore` (or `Store::create_file`) makes a [single-file store](single_file_store.md).
`StoreOptions::with_chunk_bytes` sets the snapshot chunk size, which is fixed for the store's
life ([Checkpoints](store_checkpoints.md#the-chunk-size)).

```bash
graphersal store create data/ --from modern       # --from: modern, empty, large, tree, a GraphSON/GraphML file, a .gsnap
graphersal store create small-chunks/ --chunk-size 262144   # 256 KiB chunks
graphersal --graph new/ --create-store -e 'g.add_v("person").to_list()'   # create when missing ("note: created the store new/ (damage policy maintenance)")
```

## Open: recovery on open

`Store::open(dir, options)` opens a store read-write and always runs its recovery:

1. takes the writer lock (a second writer, in this or another process, gets
   `PersistError::Locked`: "the store ... is in use");
2. completes an interrupted multi-file operation (an `INTENT` file: a rollback, an attic
   restore, a prune) and removes temporary files of an interrupted checkpoint;
3. checks the set of WAL segments against `GRAPH` (a lost, emptied or replaced newest segment is
   damage, never a shorter history);
4. loads the latest snapshot and replays the WAL after it;
5. cuts an interrupted last write of the WAL (a torn tail) and rebuilds a lost `marks` file;
6. marks the store as open and attaches the journal: from now on every commit is durable.

`store.open_report()` says what happened: whether the last close was clean (`clean_close`), the
snapshot it started from and the commits replayed, a cut torn tail, a completed intent, removed
temporary files, a repaired `GRAPH` copy, warnings. A store that was not closed cleanly (a
crash, `kill -9`, a power cut) is not damaged: the WAL replay brings back every commit; the CLI
then notes "not closed cleanly", which is harmless.

```text
$ graphersal --graph data/ -e 'g.v().count().next()'
9
```

**Damage never fails the open and is never skipped silently**: the store opens read-only in
[maintenance mode](store_damage.md) with a damage report. **A backup** directory is refused by
`Store::open` (`PersistError::IsBackup`): it opens read-only through `Store::open_backup` until
it is [restored](store_backups.md#restore). **A store that holds something critical this build
does not know** (a critical definition of an unknown kind, written by a newer Graphersal) opens
read-only too: `Store::read_only_reason()` says why, `OpenReport::read_only` records it, and
every write is refused with `PersistError::ReadOnly` ([Format Versions](store_versions.md)).

In the CLI, `--graph <path>` opens a Store when the path is a directory, ends in `/`, is a
missing path without an extension, ends in `.gstore`, or is an existing single-file store.
`--create-store` creates it when it does not exist yet (missing or empty directory).

## Read without opening

None of these take the writer lock; all work while a writer has the store open:

| | |
|---|---|
| `Store::inspect(dir)` | the `StoreInfo`: identity, lineage, position, snapshots, marks, WAL size, attic, state (`graphersal store info`) |
| `Store::snapshots_dir(dir)`, `Store::marks_dir(dir)` | the listings (`store list`, `store marks`) |
| `Store::open_read_only(dir, target)` | the graph at any [point in time](store_point_in_time.md), as an unpublished `TraversalGraph` |
| `Store::verify_dir(dir)` | the [scrub](store_verify.md) |
| `Store::is_store(dir)` | whether `dir` holds a store (`Ok(false)` for nothing there; a directory that cannot be read is an error) |

## Close

`store.close()` (and `Drop`) closes cleanly: it syncs, records the last commit and marks the
store "closed cleanly" in `GRAPH`, and releases the lock. After `close()` the shared graph
refuses commits (another holder of the `Arc<Graph>` gets an error, not a silent loss).
`store.shutdown()` does the same through a shared reference. With
`StoreOptions::with_checkpoint_on_close(true)` a checkpoint is written first, so the next open
replays nothing.

**Ctrl+C** (SIGINT, SIGTERM) in the CLI, its REPL and the dev server closes an open store
cleanly: the running query is cancelled first (it rolls back), then the store is closed. A second
Ctrl+C exits at once (harmless too: every commit is already durable). The dev server exits with
0, the REPL and `-e` runs with 130.

```text
Stopping: closing the store data/ (Ctrl+C again to exit at once)...
The store data/ was closed cleanly.
```

## Locking

One store has at most one writer: an operating-system lock on `LOCK` (on a single file, the lock
on the file itself; in memory, an in-process flag), released by the operating system when the
process ends, so a crashed writer never leaves a stale lock. Readers (`inspect`, listings,
`open_read_only`, `verify`, `fork`, backups, export) take no lock. Two `Store` values of the
same directory in one process are two writers: the second gets `PersistError::Locked` too.
