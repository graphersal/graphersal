| `Cannot create a store in data/: the directory is not empty`, `PersistError::DirectoryNotEmpty` | a new store (`Store::create`, the target of `Store::convert`) needs a new or empty directory | choose an empty or new directory, or open the existing store if it is one (`graphersal store info data/`) |
# FAQ and Troubleshooting

## Choosing

**Snapshot, journal or Store?** A `.gsnap` when the application decides when to save (a script,
the browser, a download). A journal when you need durability over streams you manage yourself.
The Store for everything else: it is a snapshot plus a journal plus recovery, retention, backups
and repair. See [Persistence](overview.md#what-to-choose).

**Directory or single file?** A directory for a server (parts can live on other disks, the files
are easy to inspect); a `.gstore` file when the store travels as one document. Both have the same
guarantees, and `graphersal store convert` switches between them.

**Is GraphSON or GraphML not enough?** They are exchange formats: they do not carry the schema,
the commit position or the auto-id sequences, and GraphML loses value types without a schema. A
snapshot is lossless.

## Errors and what to do

| Message | Cause | What to do |
|---|---|---|
| `The store data/ is in use: another writer holds its lock` | a dev server, a REPL or a program has the store open read-write | use the reading commands (they work), the dev server's Store menu, or stop the writer. A crashed writer never leaves a stale lock (the OS releases it) |
| `data/ is not a Graphersal store: no valid GRAPH file (...)` | the path is not a store (a typo, or not created yet), or both `GRAPH` copies are unreadable | check the path; `graphersal --graph <dir> --create-store` creates a store; for damage see [Damage](store_damage.md) |
| `... is a BACKUP (up to commit N, ...)`, `PersistError::IsBackup` | a backup opens read-only | `graphersal store restore <dir>` makes it the live store, `store fork <dir> <new>` a writable copy |
| `warning: the store ... has DAMAGE and opened READ-ONLY in maintenance mode` | checksums failed, a WAL segment is missing, ... | follow the [runbook](store_damage.md#runbook-the-disk-started-to-fail): stop writing, `verify`, `repair --to` |
| `The commit was rejected by the commit hook 'maintenance'` | a write to a store in maintenance mode | the same: repair into a new directory |
| `The journal is poisoned by an earlier failure` | an fsync or a write failed (a full disk, an I/O error) | fix the cause, then reopen the store (or restart the server): recovery continues from the last durable commit |
| `The journal is poisoned by an earlier failure: a panic while writing a commit record: ...` | the journal panicked while it wrote a record (a bug, or a store backend that panics); the record was cut off again and the unit rolled back | report the panic; reopen the store (or restart the server): it gives the state before that unit. Every case: [Failures while committing](journal_and_recovery.md#failures-while-committing) |
| `Commit N is too large for the journal: its uncompressed record body is B bytes, more than the limit of 1073741824 bytes per commit` / `CommitTooLarge` (in `CommitRejected` from the hook `journal`) | one unit changed more than 1 GiB of WAL record (every change with its before and after values: dropping and re-creating a large graph in one script of the dev server or the playground) | split the work: drop in a run of its own, load in several runs; see [The size of one commit](store_commits.md#the-size-of-one-commit). The unit rolled back; the store keeps working |
| `Recovery target mark "x" not reached` | no such mark, a commit after the end, or a target before the oldest retained snapshot (pruned) | `store marks`, `store list`, `store info` show what exists |
| `Cannot roll the store back: ... there is nothing after commit N` | the target is the current commit or later | choose an earlier target |
| `Cannot <operation>: <reason>` / `PersistError::Store` | the store's state refuses the operation; its `cause` (`StoreFailure`, Python `err.cause`) says why, see the table below | follow the help printed with it |
| `--at-time: "..." is not a date-time: add a zone` | a time without a zone, or a bare date | write `2026-10-08T02:43:00Z` (UTC) or an offset like `+02:00` |
| `Incremental backup ... refused` / `BackupRefused` | another graph, a pruned gap, a rollback behind the backup, a non-empty directory | back up into a new directory with `--full` |
| `The backup stopped: the store ... is damaged: <file> at byte N: ...` / `BackupDamaged`, side `Store` | a checksum of something the backup copies failed in the store | repair the STORE (`graphersal store repair <dir> --to <new_dir>`), never the backup; while the store is open in a server or a program, save its graph first with a [backup from memory](store_backups.md#a-backup-from-memory). Nothing of the failed backup was kept |
| `The backup stopped: the backup ... does not verify` / side `Backup` | the copy did not read back intact (the backup's disk) | check the backup's disk and back up again (into a new directory with `--full` when the old backup is damaged); the store is fine |
| `The store ... is read-only: backup found damage on disk ... damage policy "maintenance"` / `DamageFound` | damage was found while the store is open, and its policy (fixed at creation) is `maintenance` | the data in memory is intact: back it up from memory, then repair the store; see [Damage found while the store is open](store_damage.md#damage-found-while-the-store-is-open) |
| `--on-damage ... applies only to a store being created` | `--on-damage` for an existing store with another policy | the [damage policy](creation_parameters.md) never changes; drop the option |
| `The store ... has the older format version N; this build reads and writes only version M` / `OlderFormat` | a store written by an older Graphersal (version 2: Graphersal 0.1.x; version 1: a pre-release before the store id) | version 2: upgrade it with this build, no older binary needed: `graphersal store upgrade <dir> <new_dir>` (or `--in-place`), see [Format Versions](store_versions.md#upgrading-a-store-of-an-older-version); version 1: migrate it through a packed snapshot, [the recipe](../users_guide/command_cheat_sheet.md#migrate-a-store-from-an-older-format). The error's help names the command |
| `Unsupported format version N of the store ...: written by a newer Graphersal` / `UnsupportedVersion` | a store, backup, single-file store, packed snapshot or one of a store's snapshot or WAL files written by a NEWER Graphersal (a newer store format version in `GRAPH`, file format version, or container version) | open it with that version; this build refuses it in every operation (open, `info`, `verify`, backup, restore, repair, upgrade, ...) without changing anything, and never takes it for damage or offers a repair; see [Format Versions](store_versions.md) |
| `The snapshot needs about N bytes in memory, more than the memory limit` | a memory budget (`--memory-limit`, `ReadOptions::with_memory_limit`) smaller than the graph | raise the budget, or load on a bigger machine |

### Refused store operations (`PersistError::Store`)

A refused operation reads `Cannot <operation>: <reason>`. Its `cause` is one of a closed set
(`graphersal::persist::StoreFailure`, `#[non_exhaustive]`), so a program or a server can react to
it with a `match`; `StoreFailure::name()` is the stable name, which the Python exception carries as
`err.cause` (`None` for every other error). Each cause has its own help:

| Cause (`name()`) | When | What to do |
|---|---|---|
| `Closed` (`closed`) | the store was closed (`close`, `shutdown`) | open it again |
| `LockPoisoned` (`lock_poisoned`) | a thread panicked while it held a lock of the open store | end the process and reopen the store |
| `NameTaken` (`name_taken`) | a snapshot name in use, or a mark name used before (also by a mark a prune made unreachable) | choose another name; `store list`, `store marks` |
| `NameTooLong` (`name_too_long`) | a snapshot name over 255 bytes | a shorter name |
| `SnapshotExists` (`snapshot_exists`) | a named checkpoint at a commit that already has a snapshot under another name | commit something first, or checkpoint without a name |
| `NoSnapshot` (`no_snapshot`) | an export of a commit without a snapshot, a backup of a store without one | `store list`, `store checkpoint` |
| `BackupRunning` (`backup_running`) | a prune while a backup copies the files | prune again afterwards |
| `PruneRunning` (`prune_running`) | a backup while a prune deletes files | back up again afterwards |
| `NothingAfterTarget` (`nothing_after_target`) | a rollback to the current commit or later | an earlier target |
| `UnknownAtticEntry` (`unknown_attic_entry`) | no attic entry with that id | `store attic <dir>` lists them |
| `AtticLineageChanged` (`attic_lineage_changed`) | a later rollback or restore came after the entry's rollback | restore the newer entry first, or fork the entry |
| `ChangedSinceRollback` (`changed_since_rollback`) | commits or marks since the entry's rollback | fork the entry (`store attic <dir> fork <id> <new_dir>`) |
| `AtticBaseMissing` (`attic_base_missing`) | the snapshot an attic entry continues is gone | `store verify`; take the entry from a backup or remove it |
| `NotABackup` (`not_a_backup`) | a backup-only operation (`prune_backup`) on a live store | open and prune it as a store |
| `KeptSnapshotDamaged` (`kept_snapshot_damaged`) | a snapshot a prune would keep fails verification; nothing was removed | `store verify`, then `store repair <dir> --to <new_dir>` |
| `CopyDoesNotVerify` (`copy_does_not_verify`) | a converted or repaired copy does not read back intact | check the target disk, repeat into a new directory |
| `ChangedConcurrently` (`changed_concurrently`) | a store file changed or vanished while the operation read it | never edit a store's files; repeat, then `store verify` |
| `JournalSyncFailed` (`journal_sync_failed`) | writing or syncing the journal failed | fix the disk, reopen the store |
| `MissingCapability` (`missing_capability`) | the graph's storage does not claim `atomic` and `change_capture`, so no journal can be attached (never for `TraversalGraph`) | keep the graph in a storage that claims both |
| `ReloadFailed` (`reload_failed`) | the files of a rollback or attic restore changed, but loading the new state into the open graph failed: the graph in memory was emptied and the store stopped | close the store and open it again (restart the server): the open completes the change |
| `Stopped` (`stopped`) | an earlier rollback or attic restore failed part-way, so the store writes nothing more (`Store::stopped_reason`) | close the store and open it again |
| `AlreadyCurrent` (`already_current`) | `store upgrade <dir> <new>` of a store that already has the current format | open it as it is; for a copy use `store convert` or a backup |
| `DamagedSource` (`damaged_source`) | the store to upgrade does not verify (the message lists the first problems); nothing was written | `graphersal store repair <dir> --to <new_dir>` reads the older store and writes a repaired one in the current format; or upgrade a backup instead |
| `UpgradeNeedsCopy` (`upgrade_needs_copy`) | `store upgrade <dir> --in-place` of a store whose version the [upgrade table](store_versions.md#the-upgrade-table) upgrades only into a copy; nothing was written | `graphersal store upgrade <dir> <new_dir>` (the older store stays as it is) |
| `UnknownModel` (`unknown_model`) | `store models remove`, `Store::remove_model` or `read_model_file` of an embedding model the store does not keep | `graphersal store models <dir>` lists the identities and ids |
| `InvalidModel` (`invalid_model`) | an embedding model to add lacks its config, tokenizer or weights file, has a file name that is not allowed, a file over 8 GiB, or a repository or licence that is not a valid name; nothing was added | check the model directory (`--embedder name=<dir>` loads it) and `graphersal embed-model import <dir> --into <store>` again |

## Questions

**Can I change the damage policy (or the chunk size) of a store?** No. Both are [fixed at
creation](creation_parameters.md) and carried by every fork, repair, conversion and backup. To get
another policy, create a new store with it and load the data (for example `graphersal store export
old/ graph.gsnap`, then `graphersal store create new/ --from graph.gsnap --on-damage continue`).

**Does a backup check what it copies?** Yes, every checksum, in the store before anything is
written and again in the copy. A backup never copies damage silently; see [Every checksum,
twice](store_backups.md#every-checksum-twice).

**Is a commit really on disk when the query returns?** Yes, with the default
`Durability::EveryCommit`: the WAL record is written and `fsync`ed first; a failed sync rolls
the commit back. With `Interval` or `Os` a machine crash may lose the last commits (never one in
the middle). See [Commits and Durability](store_commits.md).

**The dev server (or my program) was killed. Is the store damaged?** No. The next open replays the
WAL and cuts an interrupted last write; stderr notes that the store was not closed cleanly, which
is harmless. Ctrl+C closes it cleanly in the first place.

**Why does the store keep growing?** Nothing is deleted implicitly: every commit is in the WAL and
every checkpoint adds a snapshot until you [prune](store_prune.md). `store compact-advice` tells
you what a prune and a compaction would give back.

**How do I see the graph as it was yesterday?** `Store::open_read_only(dir,
RecoveryTarget::Time(..))`, the dev server's **Go to time...**, or `graphersal store fork data/
yesterday/ --at-time 2026-10-07T18:00:00Z` and open the fork. See
[Point in Time](store_point_in_time.md).

**I rolled back by mistake.** `graphersal store attic data/` lists the rolled-back history;
`store attic data/ restore <ID>` undoes the rollback as long as nothing was committed or marked
since, otherwise `store attic data/ fork <ID> <NEW_DIR>` keeps it as a new store. A rollback with
`--delete` cannot be undone (restore a backup).

**Can I copy a store with `cp` or `rsync`?** While no process has it open, yes: no file records a
path. While it is open, use `graphersal store backup` (it copies up to the durable end and pins the
files against a concurrent prune); a plain copy of a live store may catch a half-written file.

**Can two processes use one store?** One writer at a time (an operating-system lock). Any number
of readers (`info`, `verify`, `fork`, `backup`, `Store::open_read_only`) at the same time as the
writer.

**Does a backup include the attic?** No. It holds the snapshots and the WAL up to the durable
end, and the marks.

**Where is the time zone?** Everything is stored and printed in UTC. `--at-time` takes any offset,
but refuses a time without one.

**Does it work in the browser?** Snapshots, journals and recovery work over any stream in
WebAssembly; the Store works there over a `MemDir` (the [playground](playground_store.md) does
that). There is no disk: download a `.gsnap` to keep a state.

**Can another program read the WAL (change data capture)?** Yes. The byte format is public
([specification](format.md)); a reader tails the WAL segments, reads complete records only, and
treats records after the writer's durable end as possibly not final. In Rust,
`store.changes_since(commit)` yields the `ChangeSet`s.

**Is the format stable?** Store format version 3 (with file format version 3) is specified on the
[specification](format.md) page; a newer version is refused, never guessed at, and a store of an
older released version is upgraded by `graphersal store upgrade` without the older build (every
build keeps the readers of every released format; [Format Versions](store_versions.md)).
