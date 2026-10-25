# Python

The Python binding (`bindings/py-graphersal`, package `graphersal`) has the packed snapshot, the
journal and recovery on `graphersal.Graph`, and the Store as `graphersal.Store`. Errors raise
`graphersal.GraphersalError` with the diagnostic and its help text. Build and test instructions:
`bindings/py-graphersal/README.md`.

## Packed snapshots, journal and recovery

```python
import graphersal

graph = graphersal.Graph.tinkerpop_modern()
graph.save("g.gsnap")                                     # {"commit_seq", "vertex_count", "edge_count"}; name= optional
graph = graphersal.Graph.load("g.gsnap")                  # no schema re-validation, checksummed
graph = graphersal.Graph.load("g.gsnap", memory_limit="auto")   # refuse a snapshot that does not fit

graph.start_journal("g.wal")                              # durability="every_commit" (default) | "os" | seconds
graph.execute('g.addV("person").property("name", "ann").next()')
graph.mark("after-ann")                                   # {"name", "commit_seq", "time"}

graph, report = graphersal.Graph.recover("g.gsnap", ["g.wal"])                    # to the end
graph, report = graphersal.Graph.recover("g.gsnap", ["g.wal"], mark="after-ann")  # or commit_seq=, time=
```

`Graph.recover` takes one target: `commit_seq=`, `time=` (microseconds since the Unix epoch) or
`mark=`; without one it replays to the end. An interrupted last write is ignored
(`report["torn_tail"]`); damage raises `GraphersalError` with its location. The recovered graph
has no journal: `save` and `start_journal` again to continue (see
[Journal and Recovery](journal_and_recovery.md)).

## The Store

```python
with graphersal.Store.create("data") as store:            # or Store.open("data"); "x.gstore": one file
    store.graph.execute('g.addV("person").property("name", "ann").next()')   # durable when it returns
    store.mark("after-ann")
    store.checkpoint("nightly")
    store.snapshots(); store.marks(); store.verify()       # listings and the scrub
    store.fork("branch", mark="after-ann")                 # a new store (commit_seq=, time=, mark=)
    store.backup("bk")                                     # full, then incremental: {"incremental", "commit_seq", ...}
    store.open_report()                                    # {"clean_close", "commit_seq", "commits_replayed", ...}
```

| Method | Does | Page |
|---|---|---|
| `Store.create(path, on_damage="maintenance")`, `Store.open(path)` | create (missing or empty `path`; `on_damage` is the [damage policy](creation_parameters.md#damage-policy), fixed for the store's life) / open read-write, with recovery; a second writer raises | [Opening and Closing](store_open_close.md) |
| `Store.create_file(path)`, `Store.open_file(path)` | a [single-file store](single_file_store.md) (`create`/`open` of a `.gstore` path do the same) | |
| `store.graph` | the store's `Graph`; every commit is durable before it returns | [Commits](store_commits.md) |
| `store.close()`, `with ...:` | a clean close; the graph refuses commits afterwards | |
| `store.open_report()` | what the open did | |
| `store.workload_report()` | the [workload statistics](workload_statistics.md): queries, property uses, lock times, jobs (Python's queries under the channel `python`) | |
| `store.checkpoint(name=None)` | write a snapshot | [Checkpoints](store_checkpoints.md) |
| `store.mark(name)`, `store.marks()` | name the position; list the marks | [Marks](store_marks.md) |
| `store.snapshots()` | the snapshots, oldest first | |
| `store.fork(path, commit_seq=, time=, mark=)` | a new store from a point in time; the dict lists `"marks_not_carried"` | [Fork](store_fork.md) |
| `store.rollback(commit_seq=, time=, mark=, delete=False)` | roll back in place; the rest goes to the attic | [Rollback](store_rollback.md) |
| `store.attic()`, `store.attic_restore(id)` | the attic entries; undo a rollback | |
| `store.backup(path, full=False, from_memory=False)` | full or incremental backup, every checksum read in the store and in the copy (damage raises, nothing kept); `from_memory=True` after damage was found: the graph in memory, nothing written to the store | [Backup and Restore](store_backups.md) |
| `store.store_id`, `store.damage_policy`, `store.damage_found`, `store.frozen_by_damage` | the store id and the policy, fixed at creation; damage found on disk while the store is open; whether it turned read-only | [Damage](store_damage.md#damage-found-while-the-store-is-open) |
| `Store.open_backup(path)`, `store.backup_marker` | a backup, read-only; its marker | |
| `Store.restore_backup(path)` | make a backup the live store (same graph id) | |
| `store.prune(up_to)`, `store.compact()`, `store.compaction_advice(min_ratio=, min_reclaimable=, min_size=)` | retention and disk space | [Prune, Compact](store_prune.md) |
| `store.verify()` | the scrub: `{"ok", "problems", "notes"}` | [Verify](store_verify.md) |
| `store.maintenance` | `None`, or the damage report of a store that opened read-only in maintenance mode | [Damage](store_damage.md) |
| `store.repair_to(path)` | a repaired copy: `{"commit_seq", "lossless", "marks_not_carried", "lineage_approximate_before", "report"}` | [Damage](store_damage.md) |
| `Store.convert(source, target, single_file=False)` | copy a closed store between a directory and a single file | [Single-File Store](single_file_store.md) |
| `Store.upgrade(source, target=None, in_place=False, single_file=False)` | convert a store (or backup) of Graphersal 0.1.x to the current format, into `target` or in place | [Format Versions](store_versions.md#upgrading-a-store-of-an-older-version) |

```python
with graphersal.Store.open_backup("bk") as backup:        # read-only; backup.backup_marker
    backup.snapshots()
graphersal.Store.restore_backup("bk")                      # the live store from now on: Store.open("bk")

with graphersal.Store.create("graph.gstore") as store:     # one file
    if store.compaction_advice()["recommended"]:
        store.compact()
graphersal.Store.convert("graph.gstore", "data")           # a closed store into a directory (or back)
graphersal.Store.upgrade("old_store", "new_store")         # a store of Graphersal 0.1.x, upgraded (or in_place=True)
```

Times in Python are microseconds since the Unix epoch (`time=`), as in Rust.
