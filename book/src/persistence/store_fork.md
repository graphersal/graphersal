# Fork

A fork makes a past (or the current) state of a store into a **new, writable store** in another
directory. The source store is not changed; to undo a fork, delete the new directory. A fork of an
open store waits while a rollback moves files; while it runs a rollback or a prune of that store is
refused.

```rust,no_run
use graphersal::persist::{RecoveryTarget, Store, StoreOptions};

let store = Store::open("data", StoreOptions::new())?;
let report = store.fork(RecoveryTarget::Mark("before-import".into()), "branch")?;
let info = &report.snapshot;
println!("forked at commit {}: graph {:032x}, parent {:032x}",
         info.commit_seq, info.graph_id, info.parent_graph_id);
for mark in &report.marks_not_carried {
    println!("mark not carried: {} @ commit {}", mark.name(), mark.commit_seq());
}
let branch = Store::open("branch", StoreOptions::new())?;
// Without opening the source (works while a writer has it open):
Store::fork_dir("data", RecoveryTarget::CommitSeq(2), "branch2")?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

```text
$ graphersal store fork data/ branch/ --at-mark before-import
Forked into branch/ at commit 1: graph 01a11a54-87d2-7995-b2cb-e450e4319735 (parent 01a11a54-8209-7661-92e2-147ae6d8bbd3).
Marks not carried (1; they stay targets of data/ only):
  before-import @ commit 1
Open it: graphersal --graph branch/ --server (or the REPL: graphersal --graph branch/).
data/ is unchanged; to undo the fork, delete the directory branch/.
```

- The target is any [point in time](store_point_in_time.md); without one, the latest state.
- The new store has **a new lineage id** that records its parent and the commit it branched at
  (`graphersal store info branch/` shows `parent` and `branched at`), one snapshot at the
  target, and an empty WAL. It keeps the source's chunk size and damage policy, but it is
  another store with **its own store id**: it never continues the source's incremental backups
  (back it up into a new directory).
- **Marks are not carried**: the new lineage starts without marks (they are names in the
  source's history, which the fork does not copy). The `ForkReport` (`marks_not_carried`), the
  CLI (`Marks not carried ...`, one `name @ commit N` line each), Python (`"marks_not_carried"`)
  and the dev server (`marksNotCarried`) list the source's marks at or before the fork's
  position; they stay targets of the source store. Set them again in the fork if you need them
  there (`store.mark(..)`, `graphersal store mark`).
- `NEW_DIR` must not exist (or be empty). A fork of a single-file store can be a `.gstore` file
  (the dev server names it `<store>-fork-<target>.gstore` next to the source).
- A fork reads the source without any lock: it works while a dev server or a REPL has the
  source open, and on a [backup](store_backups.md) (the way to get a writable copy that leaves
  the backup as it is).
- A fork of a past state is how you continue writing from it **without** touching the source.
  To take the source itself back, use an [in-place rollback](store_rollback.md).
- An [attic entry](store_rollback.md#the-attic) can be forked too: `store.attic_fork(id, dir)`,
  `graphersal store attic <DIR> fork <ID> <NEW_DIR>`. Its report lists the marks of the entry's
  history (the store's before the rollback target, the entry's own after it), not carried either.

Python: `store.fork("branch", mark="before-import")` (or `commit_seq=`, `time=` in
microseconds) returns the snapshot's dict plus `"marks_not_carried"`: `[{"name", "commit_seq"}]`.
