# Checkpoints and Snapshots

A **snapshot** is a full copy of the graph at one commit, in `snapshots/<commit>/`. A
**checkpoint** writes a new one from the current state. Snapshots make opening fast (only the WAL
after the latest snapshot is replayed) and are the starting points of every
[point-in-time](store_point_in_time.md) load. The WAL is not shortened by a checkpoint: old
history stays until you [prune](store_prune.md) it.

```rust,no_run
# use graphersal::persist::{Store, StoreOptions};
# let store = Store::open("data", StoreOptions::new())?;
let info = store.checkpoint(Some("nightly"))?;        // a name is optional
println!("snapshot at commit {} ({} vertices)", info.commit_seq, info.vertex_count);
for snapshot in store.snapshots()? {
    println!("{} {} {}", snapshot.commit_seq, snapshot.name, snapshot.vertex_count);
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

```text
$ graphersal store checkpoint data/ --name nightly
Snapshot at commit 3 (8 vertices, 6 edges).
Merged from the snapshot at commit 0 and 3 commits: 0 segment files reused, 0 copied, 2 written.
$ graphersal store list data/
      commit  last commit             vertices       edges  name
           0  -                              6           6
           3  2026-10-08T07:05:08Z           8           6  nightly
```

## How a checkpoint is written

A checkpoint is **merged**: the new snapshot is built from the newest snapshot and the WAL after
it, not from the graph in memory, and **without the graph's lock**: commits go on while it runs
and land after its position (the next checkpoint picks them up).

1. The WAL records after the newest snapshot are read (every checksum) and the ids they touch
   are collected: the vertices and edges a change names, and the endpoints of the edges added
   or dropped.
2. The chunks of the newest snapshot that hold one of these ids (found through its manifest's
   chunk index, each checksum checked) are decoded into a small *delta graph*, with the
   snapshot's schema, catalog of saved queries, position and auto-id sequences.
3. The WAL is replayed onto the delta graph exactly as an open replays it, and every change's
   *before* image must match: a WAL that does not continue the snapshot is damage.
4. The touched segments are read again as streams and merged on the sorted id with the delta
   into new segment files (planned first, then written chunk by chunk and checked against the
   plan). A segment no touched id falls into keeps its bytes under the
   [donor rule](#the-donor-rule): it is **reused by reference** (the manifest names a file of an
   older snapshot's directory: an *incremental* snapshot) when the snapshot before the old one
   holds an identical copy in another file, and **copied** into a new file otherwise.
5. The new snapshot goes into a temporary directory, is synced, **verified** (every file it
   names, the referenced ones too, read back one chunk at a time) and only then renamed to its
   final name. A Checkpoint record goes to the WAL and the next commit starts a new WAL segment.

**Memory** is proportional to the changes since the last snapshot (the delta graph, one decoded
chunk, one chunk being written), not to the graph. Beyond a bound,
`StoreOptions::with_checkpoint_merge_bytes(bytes)` (the WAL bytes after the last snapshot; 1 GiB by
default, 64 MiB in the browser; `0` = never merge), the checkpoint encodes the graph in memory
instead, under its read lock (writers wait, readers do not; memory: about 24 bytes per element and
one chunk). The same streaming encoder writes a store's first snapshot, a fork, a repaired store
and packed snapshots.

**Damage** in anything the merge reads (a manifest, `schema.json`, a chunk, a segment file, a WAL
record, a gap in the commits, a foreign lineage, a WAL that does not continue the snapshot) stops
the checkpoint with the error: nothing visible is written (the temporary directory is removed) and
the store keeps running. Run [`graphersal store verify`](store_verify.md) then.

- A checkpoint at a commit that already has a snapshot returns that snapshot (nothing committed
  since: nothing written).
- The live store merges up to the WAL's **durable end** (with `Durability::EveryCommit`, the last
  commit; a manual checkpoint or a checkpoint on close syncs the WAL first).
- Segment files are the unit of reuse: a change rewrites the whole segment holding it (64 MiB by
  default for a store, `with_snapshot_segment_bytes`), and the next checkpoint copies it once
  (the donor rule). Smaller segments mean less rewriting per checkpoint and more files.
- Files of an older snapshot that a newer one uses stay when [prune](store_prune.md) removes the
  older snapshot; a [packed export](#snapshots-as-gsnap-files) of an incremental snapshot is
  self-contained.
- In a [backup](store_backups.md), in [maintenance mode](store_damage.md) or in a store this
  version may only read, checkpoints are refused; while a dev server holds the store, use its
  Store menu (**Checkpoint**).

### A closed store

`graphersal store checkpoint <dir>` (Rust: `Store::checkpoint_dir(dir, options, name)`) merges a
CLOSED store without loading its graph: it takes the writer lock (a store open elsewhere is
refused as "in use"), completes an interrupted operation and removes temporary files as an open
would, merges the newest snapshot with the whole WAL (an interrupted last record is ignored; the
next open cuts it), and writes nothing else (no Checkpoint record, `GRAPH` unchanged). The report
says what it did:

```text
$ graphersal store checkpoint data/ --name nightly
Snapshot at commit 3 (8 vertices, 6 edges).
Merged from the snapshot at commit 0 and 3 commits: 4 segment files reused, 0 copied, 1 written.
```

## The donor rule

A [repair](store_damage.md) rebuilds a damaged chunk of the newest snapshot from the previous
snapshot plus the WAL between them: the previous snapshot is the **donor**. A file both used
would be damaged in both. So a checkpoint never uses a file the newest snapshot (its base) uses:

- a segment it rewrites is a new file anyway;
- an unchanged segment is referenced only through a **twin**: the snapshot before the base holds
  the same bytes (same id range, element count, length, checksum and chunk index) in another
  file, and the new snapshot names that file;
- without a twin, its bytes are **copied** into a new file of the new snapshot (every byte read
  and checked).

An unchanged range therefore alternates between two files from checkpoint to checkpoint (the
newest snapshot uses one, the previous one the other): only the first checkpoint after a range
was rewritten copies it (the store's second snapshot copies everything unchanged once).
`CheckpointReport::segments_reused`, `segments_copied` and `bytes_copied` say what happened.
[`verify`](store_verify.md) warns (not damage) when the newest snapshot shares a file with the
previous one, which only a store written otherwise can show; the next checkpoint copies it.

## Automatic checkpoints

When the WAL since the last snapshot passes 256 MiB, a checkpoint runs automatically on a
background thread (`StoreOptions::with_checkpoint_after_bytes(bytes)`; `0` = never; a store
opened with more WAL than that checkpoints right away). It merges like a manual one, so commits
do not wait for it. Without
threads (WebAssembly) it runs when the host calls `store.run_due_checkpoint()` (the playground
does after every run; the default threshold there is 4 MiB). `store.last_auto_checkpoint_error()`
reports a failed background checkpoint as its own `PersistError` (shared, `Arc`);
`store.wal_bytes_since_checkpoint()` the current amount.

When it runs:

- Every commit wakes the background thread while the WAL since the last snapshot is at or above
  the threshold (wakes coalesce: a commit during a running checkpoint leaves one pending).
- After a checkpoint that wrote a snapshot, the thread checks the level again at once: commits
  that landed while it ran (a single commit can be larger than the threshold) get the next
  checkpoint without waiting for another commit.
- A failed automatic checkpoint is retried at the next commit, as long as the level is still at
  or above the threshold. It is never retried in a loop of its own: a checkpoint that keeps
  failing (a full disk, say) costs at most one attempt per commit, and none while nothing is
  committed. `last_auto_checkpoint_error()` keeps the last failure until a checkpoint succeeds.
- `run_due_checkpoint()` follows the same rule: one attempt per call, while the level is at or
  above the threshold.
`StoreOptions::with_checkpoint_on_close(true)` writes one at every clean close.

## The chunk size

Elements are stored sorted by id in compressed, self-contained **chunks** (1 MiB uncompressed by
default), grouped into segment files (64 MiB in a store, 256 MiB in a packed snapshot;
`with_snapshot_segment_bytes`). The chunk is the
unit of [repair](store_damage.md): a damaged chunk is rebuilt from an older snapshot's chunks for
the same id range. Its size is a property of the store, chosen at creation
(`StoreOptions::with_chunk_bytes`, `graphersal store create --chunk-size`) and fixed for the
store's life. Smaller chunks mean finer repair and more overhead.

## Snapshots as `.gsnap` files

A stored snapshot is exactly a [packed snapshot](packed_snapshots.md) laid out as files:

```bash
graphersal store export data/ data.gsnap              # the latest snapshot
graphersal store export data/ first.gsnap --commit 0  # Exported the snapshot at commit 0 to first.gsnap.
graphersal store create copy/ --from data.gsnap       # a new store from a .gsnap
```

`store.export_packed(commit, writer)` and `Store::create_from_packed(dir, reader, options)` do the
same in Rust; the dev server's Store menu offers each snapshot as a `.gsnap` download. A
`.gsnap` of a state that has no snapshot: open it read-only at that target and write it with
`write_snapshot`, or download **Download current state (.gsnap)** from a read-only view.
