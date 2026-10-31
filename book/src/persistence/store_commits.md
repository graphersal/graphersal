# Commits and Durability

Every commit of `store.graph()` (every traversal, every `transaction(..)`, every import; see
[Transactions](../users_guide/transactions.md)) is written to the store's write-ahead log as one
record and made durable **before** it returns. A commit that changes nothing writes nothing.

```rust,no_run
use std::time::Duration;
use graphersal::persist::{Durability, Store, StoreOptions};

let store = Store::open("data", StoreOptions::new())?;                    // fsync per commit
let fast = Store::open(
    "other",
    StoreOptions::new().with_durability(Durability::Interval(Duration::from_millis(200))),
)?;
store.graph().write().traversal_mut().add_v("person").to_list()?;          // on disk when it returns
# Ok::<(), Box<dyn std::error::Error>>(())
```

## Durability

| `Durability` | A commit returns after | A crash of the process | A crash of the machine (power) |
|---|---|---|---|
| `EveryCommit` (default) | the record is written and `fsync`ed | loses nothing | loses nothing |
| `Interval(d)` | the record is written; `fsync` at most every `d` | loses nothing | may lose the commits of the last interval, never one in the middle |
| `Os` | the record is handed to the operating system | loses nothing | may lose what the OS had not written yet |

## The write-ahead guarantees

- **The journal runs last.** The store's journal is a commit hook in its own slot after every
  other hook (your own hooks, schema checks, a veto), and the commit's sequence number is assigned
  before it writes. Once the record is written nothing can veto the commit any more: the WAL
  never holds a commit that did not happen.
- **A failed write or fsync vetoes the commit**: it rolls back and the caller gets
  `GraphError::CommitRejected` with the reason. Memory and disk never diverge, and nobody gets
  "ok" for a commit that is not on disk.
- **Refused records are cut off.** Before the store gives up, the refused record is cut off the
  WAL again (and synced), so a commit the caller was told failed is never replayed. If even that
  cut fails, its position is recorded in `GRAPH`, and the next open cuts there (every reader stops
  there meanwhile).
- **A panicking hook rolls back.** A commit hook of your own that panics in `before_commit`
  rolls the unit back (the panic then continues to the caller); the journal runs after it and
  never writes that unit ([Transactions](../users_guide/transactions.md#panics)). A panic inside
  the journal itself (a bug, or a store backend that panics while the WAL is written) cuts the
  record off again, poisons the journal and rolls the unit back too. Every case, with what a
  reopen gives, is in the table
  [Failures while committing](journal_and_recovery.md#failures-while-committing).
- **Poisoning.** After a failed fsync the state of the file is unknown, so the store refuses
  every later commit and mark (`PersistError::Poisoned`) until it is reopened. Reopening runs the
  recovery and continues from the last durable commit.
- **After `close()`** the graph refuses commits.

## The durable end

`store.durable_end()` is the position after the last record whose sync completed (its WAL
segment, byte offset and commit). It is readable without any lock: a backup copies the WAL only
up to it, because a record after it may still be vetoed.

## WAL segments

The WAL is a sequence of segment files `wal/<first commit>.wal`, 16 MiB each by default
(`StoreOptions::with_wal_segment_bytes`; a record is never split, a segment closes after the
record that passes the size). A segment is closed with an `fsync` before the next one starts, so
only the last segment can end in an interrupted write. Starting a segment is recorded in `GRAPH`,
so a lost, emptied or replaced newest segment is reported as damage instead of opening the store
at an earlier commit ([Damage](store_damage.md)).

Large records (from 4 KiB) are compressed (`StoreOptions::with_codec`).

## The size of one commit

A commit whose body (every change of the unit with its before and after values, a dropped
element with its full properties included) is at most **64 MiB before compression** is one WAL
record; a larger one is written as several records, *parts* of 64 MiB each, of the same commit
(format spec 9.3.1). The parts always lie in one WAL segment, the commit is synced once after
its last part, and every reader (recovery, checkpoints, backups, `changes_since`, rollback)
sees one commit. A crash in the middle of a commit in parts leaves a torn tail that the next
open cuts at its first part: the commit is lost as a whole, never half. There is no practical
size limit (2^20 parts, 64 TiB); a unit beyond it is refused with
`PersistError::CommitTooLarge` (inside `GraphError::CommitRejected` from the hook `journal`).
What bounds a large unit is memory: its change set and its encoded body are held while it
commits, checked against the [process memory ceiling](../users_guide/resource_limits.md#the-process-memory-ceiling).
See [Resource Limits](../users_guide/resource_limits.md#the-size-of-one-commit-journal).

## Reading the committed changes (CDC)

```rust,no_run
# use graphersal::persist::{Store, StoreOptions};
# let store = Store::open("data", StoreOptions::new())?;
for change in store.changes_since(10)? {        // every commit after commit 10, in order
    let change = change?;
    println!("commit {} at {}: {} mutation(s)", change.commit_seq(), change.committed_at(),
             change.mutations().len());
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

`changes_since(commit)` streams the committed `ChangeSet`s from the WAL, up to the durable end:
every mutation with its before and after values, the commit time and metadata. It is the basis
for change data capture, replication and audit. It fails when the WAL it needs was
[pruned](store_prune.md). For a live process, a [commit hook](../users_guide/transactions.md)
gets every `ChangeSet` as it commits; the WAL's byte format is public
([format specification, section 9](format.md#9-the-write-ahead-log)) for readers in other
processes and languages.
