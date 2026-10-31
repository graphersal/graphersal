# Journal and Recovery

The stream-level durability API: a packed snapshot as the base, a **journal** (write-ahead log)
that records every commit after it, and **recovery** that replays the journals onto the
snapshot, up to the end or to a target. Everything works over `std::io::Write` / `Read`, also
in WebAssembly. [The Store](store.md) does all of this for you on a directory, a file or memory;
use the journal directly when you manage the streams yourself.

```rust
use graphersal::prelude::*;
use graphersal::persist::{self, Durability, Journal, RecoveryTarget, SharedBuffer};

// A graph, its base snapshot, and a journal from then on.
let mut graph = TraversalGraph::tinkerpop_modern();
let mut snapshot = Vec::new();
graph.write_snapshot(&mut snapshot).unwrap();
let wal = SharedBuffer::new();                        // an in-memory Write; a File in practice
Journal::new(wal.clone(), Durability::EveryCommit).attach(&mut graph).unwrap();

graph.traversal_mut().add_v("person").property("name", "ann").to_list().unwrap();
graph.mark("after-ann").unwrap();
graph.traversal_mut().v("1").drop().to_list().unwrap();

// Recovery to the mark: ann is there, vertex 1 is not dropped yet.
let recovered = persist::recover(
    snapshot.as_slice(),
    [wal.contents().as_slice()],
    RecoveryTarget::Mark("after-ann".into()),
)
.unwrap();
assert_eq!(recovered.graph.vertex_count(), 7);
assert_eq!(recovered.report.commit_seq, 1);
```

## The journal

Not every commit of a Store comes from an open graph: a catalog change of a CLOSED store
(`graphersal embed-model import --into`, `store models remove`) appends one commit to its WAL
without loading the graph ([format spec 12.12](format.md#1212-catalog-commits-on-a-closed-store)).
Recovery replays it like any other.

A `Journal` is a [commit hook](../users_guide/transactions.md) that writes each committed unit (a
lossless `ChangeSet`: every mutation with its before and after values, the commit time, the
commit metadata, the auto-id sequences) to its writer **before** the commit becomes final.

```rust,no_run
use graphersal::prelude::*;
use graphersal::persist::{Durability, Journal};

let mut graph = TraversalGraph::new();
let file = std::fs::File::create("graph.wal").unwrap();
Journal::for_file(file, Durability::EveryCommit)       // fsync per commit
    .attach(&mut graph)
    .unwrap();
graph.traversal_mut().add_v("person").to_list().unwrap(); // written and synced, then committed
```

- **Write-ahead, with a veto.** A failed write or sync rolls the unit back and the caller gets
  `GraphError::CommitRejected`: memory and journal never diverge, and nobody gets "ok" for a
  commit that is not on disk.
- **Always last.** The journal has its own slot after every other commit hook, whatever the
  registration order, and the commit sequence number is assigned before it writes. Once its
  record is written nothing can veto the unit any more, so a journal never holds a commit that
  did not happen.
- **Refused records are cut off.** When a write or sync fails, the record is cut off the stream
  again (with the truncate function: `Journal::for_file` sets `set_len` + `fsync`,
  `with_truncate(f)` sets one for any writer), so a later recovery never replays a commit the
  caller was told failed. A writer without a truncate function cannot do this.
- **Poisoning.** After a failed write or sync the state of the stream is unknown; the journal
  then rejects every later commit and mark (`PersistError::Poisoned`). Recover the graph and
  attach a new journal.
- **Durability** (`Durability`): `EveryCommit` (the default: sync per commit and mark),
  `Interval(duration)` (sync at most that often; a crash of the machine may lose the last
  interval, never a commit in the middle), `Os` (flush only; the operating system writes; a crash
  of the process loses nothing, a crash of the machine may). `Journal::new(writer, ..)` syncs
  with `flush`; `with_sync(f)` or `Journal::for_file` give it a real `fsync`.
- **Durable end.** `attach` returns a `DurableEnd`: the byte offset after the last synced
  record and its commit number, readable without any lock (a backup copies a journal up to it).
- **One journal per graph.** `attach` writes the stream header (the lineage id and the next
  commit number) and fails when the graph already has one or a unit is open.
- Large records (from 4 KiB) are LZ4-compressed (`with_codec`).
- **A large commit is written in parts.** A commit of more than 64 MiB of uncompressed record
  body (every change with its before and after values) is written as several records of 64 MiB,
  one after the other in the same segment, synced once after the last one (format spec 9.3.1);
  readers see one commit. Beyond 2^20 parts (64 TiB) the unit is vetoed with
  `PersistError::CommitTooLarge` (inside `GraphError::CommitRejected` from the hook `journal`):
  it rolls back, nothing is written, and the journal stays usable
  ([Resource Limits](../users_guide/resource_limits.md#the-size-of-one-commit-journal)).
- **A user hook that panics** in `before_commit` rolls the unit back before the journal runs, so
  the journal never holds it ([Transactions](../users_guide/transactions.md#panics)).
- **A panic inside the journal** (in its own write path: an engine bug, or a writer, sync or
  store backend that panics) is handled like a failed write: the record is cut off again, the
  journal poisons itself, and the panic continues; the unit is rolled back on the way out. See
  [Failures while committing](#failures-while-committing).

### Failures while committing

A commit asks the user hooks first (in registration order), then the journal. Whatever goes wrong
there, the promise is the same: **the unit is either committed in memory AND in the journal, or in
neither.** What you see, and what a reopen (a recovery, `Store::open`) gives:

| What fails | The unit | The journal afterwards | What the caller sees | A reopen gives |
|---|---|---|---|---|
| A user hook's `before_commit` returns `Err` (a veto) | rolled back; every hook gets `after_rollback` | untouched (it never ran) and usable | `GraphError::CommitRejected { hook, source }` | the state before the unit |
| A user hook's `before_commit` **panics** | rolled back; every hook gets `after_rollback` (`RollbackReason::Failed`) | untouched (it never ran) and usable | the panic continues (a bug: it is not turned into an error) | the state before the unit |
| The journal's write or sync **fails** (`Err`: a full disk, an I/O error) | rolled back | the refused record is cut off again and synced; the journal is **poisoned** | `CommitRejected` from the hook `journal`, its source `PersistError::Poisoned` ("... (the refused record was cut off again)") | the state before the unit |
| The journal's write or sync fails and **the cut fails too** | rolled back | poisoned; a Store records the valid end of the WAL in `GRAPH` (`wal_cut`), a bare `Journal` cannot (the record may stay) | as above, the reason says the position was recorded | a Store: the state before the unit (the next open cuts the record off, `OpenReport::refused_cut`; every reader stops there meanwhile) |
| The journal **panics** in its write path (after part of the record, or after all of it, e.g. in the sync) | rolled back on the way out; every hook gets `after_rollback` | treated like a failed write: the record is cut off again (or, when that cut fails or panics too, its position recorded as above); the journal is **poisoned** with the reason `a panic while writing a commit record: <message>` | the panic continues | the state before the unit |
| The write or sync of a **commit in parts** fails at any of its parts | rolled back | every part written is cut off again (the cut starts at the first part) and synced; the journal is **poisoned** | as for a failed write | the state before the unit |
| The commit needs more than 2^20 parts (`CommitTooLarge`), or encoding it does not fit under the process memory ceiling (`ResourceLimitExceeded`, operation `commit encoding`) | rolled back | nothing written; usable | `CommitRejected` from the hook `journal` | the state before the unit |
| A hook's `after_commit` panics | stays committed (it is final and durable) | holds the unit | the panic continues | the state with the unit |

A **poisoned** journal refuses every later commit and mark with `PersistError::Poisoned` (inside
`CommitRejected` from the hook `journal`; its help says what to do), also once the disk works
again: after a failure in the middle of a record the state of the stream is unknown, so nothing
may follow it. A Store's `close()` then fails with `StoreFailure::JournalSyncFailed` and does not
mark the store as closed cleanly. **What to do:** check the disk (or report the panic: it is a
bug), then reopen the store (or restart the server): the recovery continues from the last durable
commit and the store accepts commits again. With a bare `Journal`, recover the graph
(`persist::recover`) and attach a new journal.

The same applies to a mark (`graph.mark(..)`) and to the Store's own records (a checkpoint record,
the sync at `close()`): a panic poisons the journal and continues.

### Marks

A mark names a point in the journal: the state right after the last commit.

```rust,ignore
graph.mark("before-import")?;   // Rhai: g.mark("before-import"); Python: graph.mark(..)
```

Names are unique within a journal (pass the names of earlier journals with
`Journal::with_known_marks`, e.g. from `RecoveryReport::marks`). A graph without a journal, or a
mark inside an open transaction, is an error (`GraphError::MarkUnavailable`). In the DSL `g.mark`
needs the `Update` permission on data. The Store keeps mark names unique along its whole history.

### Commit times

Commit, mark and checkpoint times come from one clock per instance (`CommitClock`, microseconds
since the Unix epoch, UTC): every time is `max(system clock, the latest time the instance
handed out)`, so a mark is never earlier than the commit it names and a commit never earlier than
a record written before it. Equal times are allowed. The clock's floor is not stored: a recovery
and a Store's open raise it again to the latest time the files hold (the last commit, every mark
and checkpoint record read, the snapshot's creation time), and a rollback in place keeps it. When
the system clock steps back by more than one second behind the floor (a resumed virtual machine, a
manual change), times stay at the floor until the system clock catches up and Graphersal writes
a warning; it is never an error.

## Recovery

```rust,ignore
use graphersal::persist::{self, Recovery, RecoveryTarget};

let recovered = persist::recover(snapshot_reader, [journal_1, journal_2], RecoveryTarget::Latest)?;
let graph = recovered.graph;              // unpublished, no hooks: attach a new journal
println!("{:?}", recovered.report);       // position, commits replayed, marks, torn tail

// The builder: no snapshot (start from an empty graph), read options, a target.
let recovered = Recovery::new()
    .journal(journal_reader)
    .target(RecoveryTarget::CommitSeq(12))
    .run()?;
```

| Target | Stops |
|---|---|
| `Latest` | at the end of the journals |
| `CommitSeq(n)` | right after commit `n` (an error if the journals end before it) |
| `Time(t)` | after the last commit with a time at or before `t` (µs since the epoch, UTC) |
| `Mark(name)` | at the mark (an error if there is no such mark: `PersistError::TargetNotReached`) |

- Journals are replayed with `TraversalGraph::replay`: outside units, without hooks and without
  schema validation. Commits already in the snapshot are skipped; the first one after it must be
  the next number, and so on (a missing journal is `PersistError::Gap`). A journal of another
  lineage is refused (`PersistError::LineageMismatch`). Without a snapshot the replay starts from
  an empty graph and the first journal must start at commit 1.
- **The report** (`RecoveryReport`): the snapshot's position, the position reached and its time,
  the number of commits replayed, the marks seen, whether the end was reached, and a torn tail.
- **Torn tail vs damage.** Only an incomplete record at the very end of the **last** journal (an
  interrupted write) is a torn tail: it is ignored and reported. A complete record whose
  checksum fails, anywhere, is damage and fails the recovery with its location
  (`PersistError::Corrupt`). Every record header (sync marker, length, commit number, type) has
  its own checksum, separate from the payload's, so a damaged length is damage too, never taken
  for an interrupted write; zeros after the last complete record (space the file system
  allocated but the write never filled) are a torn tail.
- **A past target is a past state.** Attach a journal to a graph recovered to a target before
  the end only as a new history (a new lineage: give it a new id with
  `PersistentStorage::set_graph_id` first), never to continue the old journal.
- Reading a journal without replaying it: `persist::read_journal(r)` iterates its records
  (`JournalRecord`: commits as `ChangeSet`s, marks, checkpoints) and reports a torn tail.

### Lineage

`PersistentStorage::graph_id()` (implemented by `TraversalGraph`; import the trait from `graphersal::storage`) is a UUID (version 7) created on first use and carried by every
snapshot and journal; `read_snapshot` restores it. Recovery replays a journal only onto a
snapshot of the same lineage. In a Store, a fork, an in-place rollback and a repair start a new
lineage that records its parent and the commit it branched at; recovery follows that chain.

## Python

```python
graph.save("g.gsnap")                                       # the base
graph.start_journal("g.wal", durability="every_commit")     # or "os", or seconds (an interval)
graph.execute('g.addV("person").property("name", "ann").next()')
graph.mark("after-ann")
graph, report = graphersal.Graph.recover("g.gsnap", ["g.wal"], mark="after-ann")   # or commit_seq=, time=
```

The byte layouts of journal records are section 9 of the
[format specification](format.md#9-the-write-ahead-log).
