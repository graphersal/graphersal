# Prune, Compact and Retention

**Nothing is deleted implicitly.** Snapshots and WAL accumulate until you remove old history
explicitly, the way a database `VACUUM` or a WAL archive cleanup is explicit. A store therefore
grows with every commit and checkpoint; `prune` and `compact` give the space back.

## Prune

```rust,no_run
# use graphersal::persist::{Store, StoreOptions};
# let store = Store::open("data", StoreOptions::new())?;
let report = store.prune(1200)?;          // history only needed for targets before commit 1200
println!("kept from commit {}; removed snapshots {:?}, {} WAL segment(s)",
         report.kept_from, report.snapshots_removed, report.segments_removed.len());
# Ok::<(), Box<dyn std::error::Error>>(())
```

```bash
# prune: "Pruned data/" with "kept from: commit N", "snapshots removed", "WAL segments removed"
graphersal store prune data/ --up-to 1200
graphersal store prune data/                 # refused: "needs --up-to <COMMIT> (nothing is pruned implicitly)", exit 2
```

`prune(up_to)` removes the snapshots and WAL segments that only serve targets **before**
`up_to`. What it keeps:

- the newest snapshot at or before `up_to`, everything after it, and the WAL from it on;
- **always the last two snapshots and the WAL between them**, whatever `up_to` says: these are the
  *donors* a [repair](store_damage.md) rebuilds damaged data from;
- the base snapshot of every [attic](store_rollback.md#the-attic) entry (remove the entry first).
- the segment files of removed snapshots that kept ones still use: a
  [merged checkpoint](store_checkpoints.md#how-a-checkpoint-is-written) references the segment
  files it did not change in the older snapshot's directory. Such a directory loses its manifest
  (it is no snapshot any more) and keeps only those files (`report.files_retained`); a later prune
  removes them once no kept snapshot, and no snapshot of an attic entry, uses them.

Every snapshot it keeps is verified (read back, every checksum) **before** anything is deleted,
and the removal runs under an `INTENT` file (an interruption is completed at the next open).
Prune is refused while a backup is copying the store's files, and on a store open elsewhere
("in use": use the dev server's Store menu > Compact, which prunes up to the current commit).

After a prune, [point-in-time](store_point_in_time.md) targets before the oldest kept snapshot are
gone, and [`changes_since`](store_commits.md#reading-the-committed-changes-cdc) cannot read
before it. An **incremental backup** that still needs the removed WAL is refused (a full backup
is needed): back up before you prune.

The last two snapshots never share a segment file (the
[donor rule](store_checkpoints.md#the-donor-rule)), so each is the other's independent copy for
a repair. Holder files that nothing uses any more (after a rollback with `--delete`, or after
removing the attic entry whose snapshots used them) are removed by that operation itself.

A backup has its own retention: `Store::prune_backup(dir, up_to)`, or `graphersal store prune` on
the backup directory.

### Prune and marks

Prune does not look at [marks](store_marks.md): a mark before the oldest kept snapshot loses the
history it needs and becomes **unreachable**. The report names those marks
(`report.marks_unreachable`; `graphersal store prune` prints `marks no longer reachable: ...
(their history was pruned)`), and so does the compaction advice *before* anything is removed
(`advice.prune.marks_unreachable`, the Store menu's Disk space block), so look there before
`compact`. From then on the listings (`store.marks()`, `store marks`, `store info`, the Store menu)
leave such a mark out, opening it fails with "a prune removed the history it needs", and its name
stays taken. The `marks` file itself keeps every mark: a backup with older snapshots of its own
still reaches them and lists them.

The library calls (`Store::prune`, `Store::compact`, Python `store.prune()`/`store.compact()`)
never ask: they report the marks afterwards. To ask first, read the estimate:
`Store::prune_estimate_dir(dir, up_to)` (any bound, no lock, nothing removed) or
`store.compaction_advice()` (a Compact, up to the current commit). The front ends guard every
prune with it, so a mark is never lost without a question:

| Front end | Guard |
|---|---|
| CLI `store prune`, `store compact` | names the marks, changes nothing and exits 1 unless `--drop-marks` is given ([store CLI](store_cli.md#prune)) |
| `store compact-advice` | lists them (`marks lost  ...`) and suggests `--drop-marks` |
| Dev server `POST /api/store/compact` | 409 `{"error", "marksUnreachable": [...]}` unless the body has `{"dropMarks": true}` ([dev server](dev_server.md)) |
| Playground Store menu, Compact (server and WebAssembly) | always asks when marks would become unreachable, recommended or not, naming them; sends `dropMarks` only after the confirmation |

## Compact

```rust,no_run
# use graphersal::persist::{Store, StoreOptions};
# let store = Store::open("data", StoreOptions::new())?;
let report = store.compact()?;            // prune up to the current commit, then the backend's compaction
println!("{} bytes given back", report.bytes_freed());
# Ok::<(), Box<dyn std::error::Error>>(())
```

`compact()` is `prune(current commit)` followed by the backend's compaction: a
[single-file store](single_file_store.md) copies its live data into a new file next to the old
one and replaces it atomically (like SQLite's `VACUUM`: the disk needs room for the live data
meanwhile, and commits wait while it runs). On a directory (or in memory) the prune is all: its
removals free the space at once.

```text
$ graphersal store compact data/
Compacted data/
  kept from: commit 0
  snapshots removed: 0
  WAL segments removed: 0
  file: a directory store: the prune gave the space back (nothing to compact)
$ graphersal store compact graph.gstore
Compacted graph.gstore
  kept from: commit 0
  snapshots removed: 0
  WAL segments removed: 0
  file: 23.7 KiB -> 22.8 KiB (930 B given back)
```

## Is it worth it? The compaction advice

Whether a compaction is worth it is a cheap question to ask, as often as you like, from any
thread, while the store is in use:

```rust,no_run
# use graphersal::persist::{CompactionPolicy, Store, StoreOptions};
# let store = Store::open("graph.gstore", StoreOptions::new())?;
let advice = store.compaction_advice()?;
if advice.recommended {
    let report = store.compact()?;                  // prune + rewrite
    println!("gave back {} bytes", report.bytes_freed());
} else {
    println!("not now: {}", advice.reason);         // which threshold decided, and why
}
// Another rule for one call, or for the store (StoreOptions::with_compaction_policy):
let eager = CompactionPolicy::new()
    .with_min_garbage_ratio(0.3)
    .with_min_reclaimable_bytes(16 << 20)
    .with_min_total_bytes(0);
let advice = store.compaction_advice_with(eager)?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

```text
$ graphersal store compact-advice graph.gstore
no compaction needed: the store is smaller than the minimum of 64.0 MiB
  store: 23.0 KiB (single file)
  live: 23.0 KiB
  garbage: 0 B (0 %)
  prune: 0 B
  reclaimable: 0 B (0 % of the store)
  disk needed: 23.0 KiB
  disk free: 77.4 GiB
  decided by: total_bytes
```

`CompactionAdvice` carries the numbers behind the answer: the store's total, live and garbage
bytes, what a prune would remove first (`prune_first`, `prune_bytes`, the snapshots and WAL
segments in `prune`), the bytes a compaction gives back in all (`reclaimable_bytes`), the free
disk space it needs and the free space there is, and the marks a prune would make unreachable
(`prune.marks_unreachable`). It reads no element data: only the backend's bookkeeping, the names
and sizes of the store's files (snapshot directories and WAL segments alike), and, only when a
prune would remove something, the small manifests of the kept and the attic snapshots (the files
they still reference stay and are not counted, so `prune_bytes` is what the prune removes) and the
`marks` file when a snapshot would go. It is never `recommended` while the
store cannot change (maintenance mode, a backup, closed) or when the disk lacks the space.
`decided_by` names the rule that decided, checked in this order:

| `decided_by` | meaning |
|---|---|
| `not_writable` | the store cannot change now |
| `total_bytes` | the store is smaller than `min_total_bytes` (default 64 MiB; CLI `--min-size`) |
| `reclaimable_bytes` | less than `min_reclaimable_bytes` to gain (default 64 MiB; `--min-reclaimable`) |
| `garbage_ratio` | less than `min_garbage_ratio` of the store to gain (default 0.5; `--min-ratio`) |
| `disk_space` | the file system lacks the free space the new file needs (skipped where the free space is unknown, `free_space: None`: on Windows today) |
| `thresholds_met` | recommended: compact now |

The defaults are conservative. For a directory store (or one in memory) the advice is about the
prune only (`compactable` is `false`, `garbage_bytes` 0). `StoreOptions::with_auto_compact(true)`
compacts after every `prune()` whose advice recommends it (off by default).

## A retention routine

1. Take a backup (incremental is cheap): `graphersal store backup data/ /mnt/backup/data`.
2. `graphersal store verify data/` (do not prune a store with damage: the old snapshots are its
   donors).
3. Prune what you no longer need to reach: `graphersal store prune data/ --up-to <COMMIT>`
   (`store list` shows the snapshots and their commits; `store compact` prunes to now).
4. On a single file: `graphersal store compact-advice graph.gstore`, then `compact` when it says so.
