# Persistence

Graphersal keeps the graph in memory. The `persist` feature keeps it on disk too: in
Graphersal's own binary format, losslessly, with every committed change made durable before the
commit returns, so nothing committed is lost on a restart or a crash.

Looking for a command? The [Command Cheat Sheet](../users_guide/command_cheat_sheet.md) has
every one of them, ready to copy.

## What to choose

| You want | Use | API | Page |
|---|---|---|---|
| save a graph now and load it later (a script, the browser, a download) | a **packed snapshot**: the whole graph in one file or stream (`.gsnap`) | `graph.write_snapshot(w)`, `TraversalGraph::read_snapshot(r)`; DSL `g.export_snapshot(path)`; CLI `--graph g.gsnap` | [Packed Snapshots](packed_snapshots.md) |
| durability over streams you manage yourself (a socket, an upload, any `Write`), also in WebAssembly | a snapshot plus a **journal** (write-ahead log) and **recovery** | `persist::Journal`, `persist::recover` | [Journal and Recovery](journal_and_recovery.md) |
| an application that simply keeps its graph ("SQLite for graphs") | the **Store**: one directory (or one file) = one graph; recovery on open, checkpoints, marks, point-in-time views, fork, rollback, backups, verify, repair | `persist::Store`; CLI `graphersal store ...`, `--graph data/` | [The Store](store.md) |

GraphML and GraphSON stay the *exchange* formats. The snapshot is the *lossless* one: every value
type (`uuid`, nested arrays and objects, NaN payloads, property order), multi-label and
unlabeled vertices, unlabeled edges, the schema, the catalog of definitions (saved queries),
and the graph's position (commit sequence number, commit time, auto-id sequences).

```toml
graphersal = { version = "0.4", features = ["persist"] }        # LZ4 chunks, the Store, journals
graphersal = { version = "0.4", features = ["persist-zstd"] }   # also zstd chunks (pure Rust)
graphersal = { version = "0.4", features = ["persist-zip"] }    # also ZIP backups of a Store
```

Where it runs:

| | Packed snapshot | Journal and recovery | Store |
|---|---|---|---|
| Rust, native | yes | yes | directory, single file, memory |
| Rust, WebAssembly | yes | yes (no `fsync`) | in memory ([`MemDir`](store_backends.md)) |
| CLI | `--graph g.gsnap`, `g.export_snapshot` | | `graphersal store ...`, `--graph data/` |
| Dev server | `.gsnap` download | | `--graph data/ --server`, the Store menu |
| Web playground | Save as snapshot, load `.gsnap` | | every loaded graph is a [store in memory](playground_store.md) |
| Python | `graph.save`, `Graph.load` | `graph.start_journal`, `Graph.recover` | `graphersal.Store` |

## Concepts

| Term | Meaning |
|---|---|
| **commit** | one committed unit of change. Every traversal is one (see [Transactions](../users_guide/transactions.md)); so is an explicit `transaction(..)`, a whole playground script, an import |
| **commit sequence number** (`commit_seq`) | the number of a commit: 1, 2, 3, ... per graph, never reused. The **position** of a graph is the number of its last commit (0 = nothing committed yet) |
| **commit time** | the time of a commit (UTC, microseconds); monotonic within a history |
| **snapshot** | the whole graph at one position. In a Store it is a directory under `snapshots/`, packed it is one `.gsnap` file |
| **WAL** (journal) | the write-ahead log: one record per commit with every change, its before and after values, in order; one record per mark |
| **checkpoint** | writing a new snapshot of the current state, so the next open replays less WAL |
| **mark** | a name for a position ("before-import"); costs a few bytes |
| **target** | a point to go back to: a commit, a time, or a mark |
| **lineage** (`graph_id`) | a UUID that names one history. A fork, an in-place rollback and a repair start a new lineage that records its parent |
| **attic** | where an in-place rollback keeps the history it rolled back, so the rollback can be undone |
| **backup** | a consistent copy of a store; marked, so it opens read-only until it is restored |
| **donor** | an intact older copy (an older snapshot plus the WAL) that damaged data can be rebuilt from |
| **maintenance mode** | how a damaged store opens: read-only, with a damage report |

## Guarantees

- **Durable commits.** A Store commit (and a journal commit with `Durability::EveryCommit`, the
  default) is written and `fsync`ed before it returns. A failed write or sync rolls the commit
  back: the caller never gets "ok" for a commit that is not on disk, and the WAL never holds a
  commit that did not happen.
- **Crash safety.** Opening a store after a crash (or `kill -9`) brings back exactly the last
  committed state: an interrupted last write is cut off, an interrupted multi-file operation is
  completed.
- **Damage is found, never silently lost.** Every header, chunk and record carries a CRC-32C;
  the small critical metadata exists twice. A damaged store opens read-only in maintenance mode
  with a report, and repair writes a new store elsewhere, never in place.
- **Nothing is deleted implicitly.** History goes away only through an explicit `prune`, a
  rollback with `--delete`, or removing an attic entry.
- **Point in time.** Every commit, time and mark that the retained history covers can be viewed
  read-only, forked into a new store, or rolled back to.
- **Portable bytes.** Little-endian, the same bytes on every platform and in WebAssembly; no
  file holds an absolute path, so a store can be moved or copied anywhere. The format is a
  public [specification](format.md).
- **One writer.** A store has at most one writer (an operating-system lock); readers, backups and
  forks work while it runs.

## This part of the book

- [Quick Start](quick_start.md): the CLI, Rust, Python and the playground in five minutes.
- [Packed Snapshots](packed_snapshots.md) and [Journal and Recovery](journal_and_recovery.md):
  the stream-level API.
- [The Store](store.md) and its pages: opening, commits, checkpoints, marks, point in time, fork,
  rollback, backups, prune and compaction, verify, damage and repair, format versions, backends,
  the single-file store, a Store over your own storage, and the creation parameters.
- Tools: [the `graphersal store` command](store_cli.md), [the dev server and
  MCP](dev_server.md), [the store in the playground](playground_store.md),
  [Python](python.md).
- [Storage Format Specification](format.md): the normative byte layouts.
- [FAQ and Troubleshooting](faq.md).
