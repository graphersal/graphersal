# Rollback and the Attic

An **in-place rollback** takes the store itself back to a commit, a time or a mark. Everything
after the target is not deleted: it moves into the **attic**, so the rollback can be undone.

```rust,no_run
use graphersal::persist::{RecoveryTarget, RollbackMode, Store, StoreOptions};

let store = Store::open("data", StoreOptions::new())?;
let report = store.rollback_to(RecoveryTarget::Mark("before-import".into()), RollbackMode::Move)?;
println!("{} -> {}", report.previous_commit_seq, report.target);
let id = report.attic.unwrap();
for change in store.attic_changes(&id)? {               // what was rolled back
    println!("{}", change?.commit_seq());
}
store.attic_restore(&id)?;            // undo the rollback (only while nothing was committed since)
// store.attic_fork(&id, "branch")?;  // or: the rolled-back history as a new store
// store.attic_remove(&id)?;          // or: delete it for good
# Ok::<(), Box<dyn std::error::Error>>(())
```

```text
$ graphersal store rollback data/ --at-mark before-import
Rolled back from commit 4 to commit 1: new lineage 01a11a54-8977-7c30-b874-01937ab2e3a4.
The history after it (commits 2..=4, 1 snapshot(s), 2 WAL file(s)) is in the attic as 20261008T070510Z-00000000000000000002.
Undo (only while nothing new is committed or marked): graphersal store attic data/ restore 20261008T070510Z-00000000000000000002
Keep it as a new store instead: graphersal store attic data/ fork 20261008T070510Z-00000000000000000002 <NEW_DIR>; delete it: graphersal store attic data/ remove 20261008T070510Z-00000000000000000002
```

## What a rollback does

- Everything after the target (the later snapshots and the WAL after it; the WAL segment holding
  the target is split at the first commit after it) moves into
  `attic/<time>-<first moved commit>/`, with an `ATTIC` file that says what it is.
- The store continues as **a new lineage**: a new `graph_id` whose parent is the old one at the
  target, with a new WAL segment. Point-in-time loads and `verify` follow the lineage chain, so the
  history before the target stays reachable.
- The marks after the target go with the history into the attic.
- `store.graph()` is replaced **in place**: the `Arc<Graph>` your application holds now holds
  the state at the target, and the commit hooks you registered on it stay.
- `RollbackMode::Delete` (CLI `--delete`) deletes the history instead: there is no undo.
- The rollback runs under an `INTENT` file with idempotent steps: a crash in the middle is
  completed at the next open, never left half done. Commits wait while it runs. A backup, a
  verification, a fork or an export started meanwhile waits for it too.
- A rollback, an attic restore and an attic remove are refused at once (`FilesInUse`) while a
  backup, a verification, a fork or an export reads the store's files, in this or another process:
  they would move files away under it. Nothing was changed; run it again when that has finished.
- Refused: a target at or after the current commit ("there is nothing after commit N"), a target
  before the oldest retained snapshot, a store that is a backup, read-only or in maintenance
  mode. From the CLI the store must not be open elsewhere ("in use"); with a dev server running,
  use its Store menu (**Roll back here...**), which cancels a running query first.

## The attic

```text
$ graphersal store attic data/
20261008T070510Z-00000000000000000002  commits 2..=4 rolled back at 2026-10-08T07:05:10Z (rollback to mark "before-import"), 9.3 KiB, 1 snapshot(s)
$ graphersal store attic data/ changes 20261008T070510Z-00000000000000000002
commit        2  2026-10-08T07:05:08Z  1 mutation(s)
commit        3  2026-10-08T07:05:08Z  1 mutation(s)
commit        4  2026-10-08T07:05:09Z  1 mutation(s)
```

| Operation | Library | CLI |
|---|---|---|
| list the entries | `store.attic_list()`, `Store::attic_list_dir(dir)` | `store attic <DIR>` |
| the commits of an entry (who, when, what) | `store.attic_changes(id)` (`ChangeSet`s) | `store attic <DIR> changes <ID>` |
| undo the rollback | `store.attic_restore(id)` | `store attic <DIR> restore <ID>` |
| can it still be undone, and why not | `store.attic_can_restore(id)` | (the restore says why) |
| the rolled-back history as a new store | `store.attic_fork(id, dir)`, `Store::attic_fork_dir(..)` | `store attic <DIR> fork <ID> <NEW_DIR>` |
| delete it for good | `store.attic_remove(id)` | `store attic <DIR> remove <ID>` |

- **Restore** brings the store back to where it was before the rollback (the old lineage, the
  moved snapshots and WAL, the marks). It is possible only while **nothing was committed or
  marked** since the rollback; after that, `attic_fork` still keeps the rolled-back history as a
  new store.
- [`prune`](store_prune.md) never removes the snapshot an attic entry is based on; remove the
  entry first. The attic is not part of a [backup](store_backups.md).
- **Every entry stays whole**, whatever you do afterwards: a later rollback to a commit *before*
  an entry's history copies the WAL that entry needs into it first (and points it at its own
  base snapshot; the entry's own snapshots may go then, its WAL rebuilds them), so no entry ever
  depends on another. Removing one never touches another. `verify` rebuilds every entry's
  history. Several rollbacks restore **newest first** (see [Restore order](#restore-order)).
- A rollback or restore that fails half way (a full disk, an I/O error) **stops** the store:
  it writes nothing more (commits, marks, checkpoints, prune, rollback and restore are refused
  with the cause `stopped`, and closing it writes no `GRAPH`) until it is closed and opened
  again; the open completes the operation from its `INTENT` (or undoes it). The reason is in
  `store.stopped_reason()`, `StoreInfo::stopped` and the Store menu (**Store stopped**).
- If the files were changed but loading the new state into the open graph fails (an I/O error,
  damage, out of memory, a storage refusing the data), the graph in memory is **emptied**, never
  left half-loaded: queries read an empty graph rather than plausible but incomplete data. The
  error (cause `reload_failed`) says that the reload after the rollback or restore failed, with
  its cause, that the graph was emptied and the store stopped, and that the files on disk are
  intact. Close the store and open it again (restart the dev server): the open completes the
  change and loads the full graph.

## Restore order

After several rollbacks, attic entries **restore newest first**. This is a deliberate rule
(by design), not a limitation to be lifted:

- An entry can be restored into the live store only while the store is on the lineage that entry's
  rollback started. A newer rollback starts another lineage, so the older entry becomes restorable
  again once the newer rollbacks are undone (restored) one by one, newest first. Each restore
  brings back exactly the lineage the next older entry needs.
- **Any entry forks at any time** (`attic_fork`, `graphersal store attic <DIR> fork <ID>
  <NEW_DIR>`): the history of an older entry is never out of reach, it only cannot be restored
  *in place* out of order.
- A restore out of order is refused, changes nothing, and names the newer entry to restore first:

```text
$ graphersal store rollback data/ --at-commit 4        # entry ...-05: commits 5..=6
$ graphersal store rollback data/ --at-commit 2        # entry ...-03: commits 3..=4
$ graphersal store attic data/ restore 20261009T010700Z-00000000000000000005
Error: Cannot restore the attic entry: the store's lineage changed since that rollback (another
rollback or restore; attic entries restore newest first); restore the newer entry
20261009T010702Z-00000000000000000003 first, or fork the entry instead (the help shows the commands)
Help: Another rollback or restore came after this entry's rollback, and attic entries restore newest first: ...
        # restore the newer entry first (the message names it)
        graphersal store attic <dir> restore <newer_id>
        # or fork this entry into a new store
        graphersal store attic <dir> fork <id> <new_dir>
$ graphersal store attic data/ restore 20261009T010702Z-00000000000000000003   # back at commit 4
$ graphersal store attic data/ restore 20261009T010700Z-00000000000000000005   # back at commit 6
```

The rule keeps restores simple and safe: a restore always puts back one whole, consistent
history on top of the lineage it branched from, never a mix of lineages.

## How to undo what

| You did | Undo |
|---|---|
| a rollback (attic mode) | `attic_restore(id)` / `graphersal store attic <DIR> restore <ID>` / Store menu > Attic > Restore, as long as nothing was committed or marked since. After that, `attic_fork` keeps the rolled-back history as a new store |
| a rollback with `--delete` | nothing: the history is gone (restore a backup) |
| a fork | delete the new directory (the source store is not changed) |
| an attic restore | roll back again to the same target |

`graphersal store rollback` and `store fork` print these commands after they ran.
