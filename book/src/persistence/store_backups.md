# Backup and Restore

A backup is a consistent copy of the store's files, taken **without any graph lock**: writers go
on while it copies. It holds the latest snapshot and the WAL after it, up to the last **durable**
commit (a commit whose fsync completed). While it copies, the files are pinned against `prune`.
Every checksum of what it copies is read in the store first and again in the copy: a backup never
copies damage silently, and never leaves a copy that does not read back (see
[Every checksum, twice](#every-checksum-twice)).

A backup also holds the store's [embedding models](store_models.md) with their licence files
(checked like the rest; `BackupReport::models` lists them), so a restored store opens with its
models. `Store::backup_zip_with(archive, ModelCopy::Exclude)` (CLI `store backup --zip
--without-models`) leaves them out of a ZIP archive meant to be handed on.

```rust,no_run
use graphersal::persist::{BackupMode, RecoveryTarget, Store, StoreOptions};

let store = Store::open("data", StoreOptions::new())?;
store.backup("backup")?;                          // an empty or new directory: a FULL backup
// ... commits ...
let report = store.backup("backup")?;             // the same directory again: an INCREMENT
println!("{} -> {} ({} bytes)", report.previous_commit_seq, report.commit_seq, report.bytes);
store.backup_with("weekly", BackupMode::Full)?;   // always full (the directory must be empty)

let past = Store::open_read_only("backup", RecoveryTarget::CommitSeq(3))?;  // any commit it holds
Store::restore_backup("backup")?;                 // the original is lost: continue from the backup
let live = Store::open("backup", StoreOptions::new())?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

```bash
graphersal store backup data/ /mnt/backup/data       # full the first time, incremental afterwards
graphersal store backup data/ /mnt/backup/data       # "nothing new since commit N" when current
graphersal store backup data/ weekly/ --full         # always full (an empty or new directory)
graphersal store backup data/ data.zip --zip         # one ZIP archive (always full)
graphersal store info /mnt/backup/data               # "backup  up to commit N, taken T"
graphersal store prune /mnt/backup/data --up-to 5000 # the backup's own retention
graphersal store restore /mnt/backup/data            # make it the live store (same graph id)
graphersal store restore data.zip restored/          # unpack a ZIP backup as the live store
```

```text
$ graphersal store backup data/ backup/
Full backup into backup/: snapshot 3, the WAL up to commit 3 (1 segment(s)); 8 file(s), 9605 bytes.
The backup is read-only (graphersal --graph backup/ opens it so); `graphersal store restore backup/` makes it the live store.
$ graphersal store backup data/ backup/
Incremental backup into backup/: commits 4..4 (1 new); copied 0 snapshot(s), 0 WAL segment(s), 83 bytes appended to the last segment; 83 bytes in total.
```

`Store::backup_dir(dir, target)` (and `backup_dir_with`, `backup_zip_dir`) back up a store
without opening it: a backup works while a dev server or another program has the store open.

## Every checksum, twice

A backup (full, increment, ZIP) checks everything it copies **in the store before it writes
anything**: both `GRAPH` copies, the `marks` file, the snapshot (manifest, `schema.json`, every
segment and chunk, the files it references) and every WAL record (header and body checksum, the
segment headers, commit continuity, a missing or emptied newest segment). After writing it reads
**the copy** back the same way; a ZIP archive is read back entry by entry through the same handle
(`backup_zip` takes `Read + Write + Seek`: a file opened for reading and writing, a
`Cursor<Vec<u8>>`).

- **Damage in the store** stops the backup before a byte is written: `PersistError::BackupDamaged`
  with `side: DamageSide::Store` and the problems as `verify` lists them (file, byte, reason).
  Repair the **store**, never the backup: `graphersal store repair <dir> --to <new_dir>`. While
  the store is open in a process whose graph is intact (the dev server, a Python program), save
  that graph first with a [backup from memory](#a-backup-from-memory).
- **A copy that does not read back** (a bad backup disk) is `side: DamageSide::Backup`: a new
  backup directory is removed, an increment is rolled back to the backup's old state. The store
  is fine; check the backup's disk and back up again.
- **Redundant metadata** (one `GRAPH` copy, one copy of a manifest's fixed part, the derived
  `marks` file) is a **warning** (`BackupReport::warnings`, printed by the CLI): the backup is
  intact (two fresh `GRAPH` copies, the manifest with the intact copy in both places, `marks`
  rebuilt from the WAL it holds). Plan a repair of the store.
- In an open store, damage a backup finds is damage found while the store is open: the store's
  [damage policy](creation_parameters.md#damage-policy) applies (by default it turns read-only).

```text
$ graphersal store backup data/ backup/
Error: The backup stopped: the store data/ is damaged: wal/00000000000000000001.wal at byte 64: record of commit 1: checksum mismatch; nothing of the backup into backup/ was kept
Help: Repair the MAIN store, never the backup: graphersal store repair data/ --to <new_dir> (Store::repair_dir) builds a verified, repaired copy in a new directory and reports what was repaired, lost and diverged; the damaged files are never changed. The full damage report: graphersal store verify data/. Existing backups stay valid: keep them until the repaired store is backed up.
```

## A backup from memory

When damage was found while the store is open (by a backup, a checkpoint, `verify`), its graph in
memory is still intact: it was verified when the store was opened and changed only by commits
since. `store.backup_from_memory(target, mode)` writes **that graph** into a backup directory with
the streaming encoder (under the graph's read lock; commits wait meanwhile) and reads or writes
**nothing** in the store's directory:

- one snapshot at the graph's commit, an empty WAL segment after it, `GRAPH` with the backup
  marker and the store's identity, lineage and [creation parameters](creation_parameters.md);
- into an empty or new directory a full backup; into a backup of the same store an increment
  (its snapshot is newer than everything the backup holds; a later normal increment of it needs a
  full backup and says so);
- the result is an ordinary backup: `graphersal store restore <dir>` makes it the live store
  (same graph id), `store fork` a new one. Marks are not carried (like a fork).

It is refused while no damage was found (a normal backup is the right tool and checks every
checksum), for a store opened in maintenance mode (its graph was salvaged from donors: use
`repair_to`) and for a backup opened read-only. Where: Rust `Store::backup_from_memory`; the dev
server's Store menu (Back up from memory) and `POST /api/store/backup {"from_memory": true}`;
Python `store.backup(path, from_memory=True)`. The `graphersal store backup` command runs in a
new process, so `--from-memory` there explains where to do it.

```python
store = graphersal.Store.open("data")
try:
    store.backup("/mnt/backup/data")
except graphersal.GraphersalError as err:          # "the store data is damaged: ..."
    store.backup("/mnt/backup/saved", from_memory=True)
```

## A backup is marked and read-only

A backup's `GRAPH` file says "a backup of graph X up to commit N, taken at T". It keeps the
original's `graph_id` and lineage. It opens **read-only**: `Store::open` refuses it
(`PersistError::IsBackup`), so nothing writes to it by accident.

- `Store::open_backup(dir, options)` is a read-only `Store` over it: info, listings, views at any
  commit it holds (`read_only_at`), fork, verify, export and backups of it work; every write is
  refused. `store.backup_marker()` returns the marker.
- `Store::open_read_only(dir, target)` loads any commit it holds.
- `graphersal --graph backup/` (REPL, `-e`, `--server`) opens it read-only with a note:
  `note: backup/ is a BACKUP (up to commit 4, taken 2026-10-08T07:05:09Z): opened READ-ONLY, writes are refused. Help: ...`
  (the help names `store restore` and `store fork`).
- The dev server shows a "Backup: read-only" banner; its Store menu offers View and Fork only.

## Restore

- **In place**: `Store::restore_backup(dir)` (`graphersal store restore <BACKUP_DIR>`) clears the
  marker: the directory becomes the live store with the **same graph id**. This is the case "the
  original is lost, continue from the backup". Stop anything that has the backup open first.
- **As an independent copy** that leaves the backup as it is: fork it
  (`graphersal store fork backup/ copy/`, a new graph id).
- **From a ZIP** (`persist-zip`): `Store::restore_zip(reader, dir)` (`graphersal store restore
  data.zip restored/`) unpacks it as the **live** store (unpacking is the explicit restore).

## Incremental backups

A backup into a directory that holds a backup of the same store copies only what the backup does
not hold yet:

- newer snapshots, new WAL segments, and the new end of its last WAL segment, after checking that
  the backup's WAL is a prefix of the store's (the last record's frame);
- every file it copies is checked in the store first and read back in the backup (every chunk
  and record checksum), and the commits must continue the backup's without a gap or an overlap;
- the increment runs under an `INTENT` file in the backup directory: interrupted (a crash, a full
  disk), it is rolled back at the next backup, restore or prune of the backup. The backup is a
  valid store after every step;
- the `BackupReport` says what was copied (`previous_commit_seq`, `commit_seq`,
  `snapshots_copied`, `segments_copied`, `tail_bytes`, `bytes`, `lineage_changed`); with nothing
  new it copies nothing.

**After an in-place rollback** of the store, the next increment follows the store's new lineage
when the rollback's target is at or after the backup's position. A rollback **behind** the
backup's position is refused: the backup holds history the store moved to its attic, so it is
kept as it is, the archive of the abandoned history; take a full backup into a new directory
(`--full`) for the new one. "Behind" counts every rollback since the backup: a
rollback of a later lineage to before that lineage's own start branches from the backup's lineage
at that earlier commit. An attic restore re-splits the WAL segment the rollback cut, so the next
increment may find its last segment no longer a prefix of the store's and asks for a full
backup.

**Only the same store continues a backup.** Every store has a store id
([creation parameter](creation_parameters.md)), recorded in each of its backups; an increment
requires the same one. A fork, an attic fork, a repair or a `store convert` copy shares the
original's lineage history up to the copy, but it is a different store with a new id: its
increment into the original's backup is refused, take a full backup into a new directory. An
in-place rollback, an attic restore and a restored backup (`store restore`, from a directory or a
ZIP) keep the id: a restored backup is the store and continues its other backups.

## Refused

`PersistError::BackupRefused` leaves the backup unchanged. It is returned for:

- a backup of another graph;
- a backup of a different store: a fork, repair or copy of the backed-up store (its store id
  differs; above);
- a directory that holds a store that is not a backup (a live store is never written to);
- a non-empty directory without a store, or `--full` into a non-empty directory;
- a rollback of the store behind the backup's position (above);
- **a pruned gap**: the store's `prune` removed WAL the backup still needs. Backups do not pin
  the store's WAL against `prune`: back up more often than you prune, or take a full backup into
  a new directory.

## Retention of backups

A backup has its own retention: `Store::prune_backup(dir, up_to)` (`graphersal store prune
<BACKUP_DIR> --up-to N`) prunes it by the store's rules (the last two snapshots stay); the next
increment continues it. Keep several generations by backing up into several directories (for
example one per week with `--full`).

## ZIP archives

With the `persist-zip` feature, `store.backup_zip(archive)` writes a full backup as one archive:
stored entries (the chunks are already compressed), zip64, every file's CRC. The archive is read
back through the same handle and checked (every entry's CRC-32, every checksum of the store files
in it), so it takes `Read + Write + Seek`: a file opened for reading and writing, or a
`std::io::Cursor<Vec<u8>>` for an upload. The archive's `GRAPH` carries the backup marker;
`Store::restore_zip` clears it. ZIP backups are always full. The CLI refuses an existing target
file and removes the archive when the backup fails.

## What is not in a backup

The attic (rolled-back history), the `LOCK` and `BACKUP` lock files, and any commit after the
durable end at the moment of the copy (it is in the next increment).
