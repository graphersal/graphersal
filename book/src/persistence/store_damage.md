# Damage, Maintenance and Repair

A disk that starts to rot in random places must not lose committed data silently. The store
detects damage, locates it, keeps everything readable that is intact, and repairs from donors
that already exist, into a new directory.

## Detection

- Every header, chunk and WAL record carries a CRC-32C; the manifest keeps the id range and CRC
  of every chunk, so a damaged chunk's elements are known even when its segment is damaged.
- `GRAPH` exists twice (`GRAPH`, `GRAPH.copy`), and so does each manifest's fixed part (at its
  head and tail). Damage to one copy loses nothing: a read uses the intact copy, and `verify`
  reports the damaged one (a `DAMAGE` item whose donor is "the other copy", exit 1). A read-write
  open rewrites both `GRAPH` copies from the intact one and records that in
  `OpenReport::graph_file_repaired` (the CLI, its `store` commands and the dev server print it as
  one `note:` line on stderr); a backup copies
  the intact one and warns.
- WAL records carry a sync marker and their commit number outside the payload, so a reader
  resynchronises after a damaged record; a separate header checksum tells a damaged length from
  an interrupted write.
- Only an incomplete last record of the last WAL segment is a **torn tail** (an interrupted
  write, cut at the next open). A complete record with a bad checksum, anywhere, is **damage**,
  never cut. A lost, emptied or replaced newest WAL segment, or a cleanly closed store whose files
  end earlier, is damage too: never a silently shorter history.
- `prune` always keeps the last two verified snapshots and the WAL between them: the **donors**.
- What a **newer Graphersal** wrote is never damage (format spec 16 rule 9): an unknown value in
  a catalog definition (a compression codec, an index type or element kind, a saved query's
  parameter schema) keeps the definition, not served; a graph schema this version cannot parse
  opens the store **read-only** (the data without the schema, no damage report); an unknown
  interrupted operation or store state is refused with *Unsupported content ... written by a
  newer Graphersal, not damage*. Such a store is never repaired by this version (a repair would
  drop what it cannot read): open it with the newer version. `verify` lists newer content as a
  `note:`, not as a problem.

## Maintenance mode

Opening a store with damage does not fail and does not repair. It opens **read-only** in
maintenance mode with a `DamageReport` (`store.maintenance()`):

```text
$ graphersal --graph damaged/ -e 'g.v().count().next()'
warning: the store damaged/ has DAMAGE and opened READ-ONLY in maintenance mode: queries read the intact data, writes are refused.
2 damaged item(s); readable state: snapshot 1 + WAL at commit 2; a repair LOSES data (see the items without a donor)
  DAMAGE wal/00000000000000000002.wal at byte 188: record checksum mismatch; 83 bytes up to the next valid record; commits from 3 on; no donor: LOST
  DAMAGE wal: the store was closed cleanly at commit 3, but its files end at commit 2; commit 3; no donor: LOST
  note: the open found damage: Corrupt journal 0 at byte 188: record of commit 3: checksum mismatch
  note: built on snapshot 1 (snapshots/00000000000000000001)
Help: repair it into a new directory (the damaged files stay untouched), or restore a backup
        # build a repaired copy in a new directory
        graphersal store repair damaged/ --to <new_dir>
8
```

- The report lists the damaged files, chunks and records, the element id ranges and commits
  affected, and for each the **donor** its data can come from: an older snapshot's chunks for that
  id range plus the WAL up to the damaged one; a later snapshot that covers a damaged WAL record;
  or the other copy. Items without a donor are data that only a backup still has.
- When **both `GRAPH` copies** are damaged, the identity is rebuilt from the snapshot manifests
  and the WAL segment headers. They name the lineages only from the base snapshot on (the first
  WAL segment without one), so the report says so explicitly:
  `note: lineage before commit N is approximate (both GRAPH copies damaged)`
  (`DamageReport::lineage_approximate_before`, `lineage_note()`; the dev server's damage view,
  `graphersal store verify` and `repair`, Python `repair_to(..)["lineage_approximate_before"]`).
  Older ancestors, their branch points and times are then unknown; the data is not affected.
- Everything readable is queryable: damaged chunks are filled in from donors, so the state you
  query is exactly what a repair would write.
- Commits, marks, checkpoints, prune, rollback and compaction are refused with
  `PersistError::Maintenance` ("the store ... is in maintenance mode"). Nothing is written to the
  damaged store: no state change, no torn-tail cut.
- The dev server shows maintenance mode as a banner and the damage report in its Store menu;
  rollback and the attic's restore and remove are refused there. Python: `store.maintenance` is
  the report text (or `None`).

## Damage found while the store is open

Damage can appear while a store is open (a disk that rots under a running server). A backup (it
reads every checksum of what it copies), a checkpoint (its merge reads the newest snapshot and the
WAL after it) and `verify` find it. What the store then does is its **damage policy**, chosen
when it was created and never changed ([Creation Parameters](creation_parameters.md#damage-policy)):

| Policy | Commits | Reads | Shown |
|---|---|---|---|
| `maintenance` (default) | refused from then on (`PersistError::DamageFound`: "read-only: backup found damage on disk"), as are marks, checkpoints, prune, rollback, compaction; the close writes nothing but the WAL sync | go on | `store.damage_found()`, `store.is_frozen_by_damage()`, `StoreInfo`, a log warning; the dev server's banner and Store menu, MCP `graph_info` |
| `continue` | go on | go on | the same, without the freeze |

The damage stays reported for as long as the store is open (a repaired copy is a new store).
Either way the graph in memory is intact: save it with a **backup from memory**
([Backup and Restore](store_backups.md#a-backup-from-memory)), which writes nothing to the
damaged store, then repair the store from its files (below), or restore the backup.

The policy does not apply to damage found when the store is **opened**: a store that cannot be
loaded intact always opens in maintenance mode, as described above. Problems in attic entries
only are reported by `verify` but do not freeze the store (they are history moved aside, not the
store's own).

```text
$ curl -s -X POST localhost:8080/api/store/backup -d '{}'
{"error": "Error: The backup stopped: the store data is damaged: wal/...: record of commit 7: checksum mismatch; ..."}
$ curl -s localhost:8080/api/info | jq .damageFound
{"text": "DAMAGE found on disk by backup at 2026-10-09T08:12:03Z (1 problem(s); ...): the store is READ-ONLY now (damage policy maintenance). ...", "frozen": true, "foundBy": "backup", "policy": "maintenance", "problems": 1}
$ curl -s -X POST localhost:8080/api/store/backup -d '{"dir": "saved", "from_memory": true}' | jq .backup.fromMemory
true
```

## Repair

Repair is explicit and **never in place**: it writes a new, verified store in another directory
(do not write more to a failing disk), and the damaged store is never changed.

```rust,no_run
# use graphersal::persist::{Store, StoreOptions};
let report = Store::repair_dir("damaged", "repaired")?;   // or store.repair_to(dir) on an open store
print!("{report}");
if !report.is_lossless() { /* read report.lost_commits, lost_ranges, lost_elements, diverged */ }
# Ok::<(), Box<dyn std::error::Error>>(())
```

```text
$ graphersal store repair damaged2/ --to repaired2/
1 damaged item(s); readable state: snapshot 1 + WAL at commit 3; a repair loses nothing
  DAMAGE snapshots/00000000000000000000/v-000000.seg at byte 64: chunk checksum mismatch; vertex ids "1"..="6"; donor: not needed: snapshot 1 covers it
  note: built on snapshot 1 (snapshots/00000000000000000001)
Repaired store repaired2/ at commit 3 (graph 01a11a54-c795-7b87-ad5b-6e723413bc32): nothing lost
  mark not carried: before-import @ commit 1
ok: nothing lost. Use it: graphersal --graph repaired2/
```

- Damaged snapshot chunks are rebuilt from a donor: an older snapshot's intact chunks for the same
  id range plus the WAL records for those elements.
- A damaged WAL record is skipped when a later snapshot covers it. Otherwise the replay continues
  after the gap, and every element whose state differs from a later record's *before* image is
  reported as **diverged** (the later value is kept) instead of being guessed.
- The `RepairReport` lists what was repaired, the lost commits and id ranges, the lost elements
  (for example an edge whose endpoint was lost), and the diverged elements.
- The repaired store has a new lineage id whose chain continues the damaged store's, one snapshot
  named `repaired`, and is verified before the call returns. It is a new store with its own
  store id: the damaged store's backups are not continued by it (its first backup is a full one
  into a new directory; the old backups stay valid archives).
- **Marks are not carried**: the repaired store starts without marks. `RepairReport::marks_not_carried`
  (CLI `mark not carried: name @ commit N`, Python `"marks_not_carried"`) lists the damaged store's
  marks at or before the repaired position (as far as its `marks` file or WAL still yields them).
- Exit code of `graphersal store repair`: **0** when nothing was lost, **1** when data is lost or
  diverged (the repaired store is still written: read the lists), **2** without `--to`.

## Runbook: "the disk started to fail"

1. **Stop writing.** Stop the server or program (Ctrl+C closes the store cleanly). Do not prune:
   older snapshots are donors.
2. `graphersal store verify <dir>`: the damage report with the donor of each item. Items
   without a donor are data that only a backup still has.
3. Copy the store directory to a healthy disk if you can (`cp -a`); work on the copy.
4. `graphersal store repair <dir> --to <new_dir>` on the healthy disk. Exit 0 and "nothing
   lost": switch to the new directory. Exit 1: read the lost and diverged lists; restore a
   [backup](store_backups.md) for what is lost (or fork the backup and compare), check the
   diverged elements by hand.
5. `graphersal store verify <new_dir>`, start using it, take a fresh full backup (the repaired
   store is a new lineage), and run `verify` regularly from now on: it finds damage while donors
   still exist.

Damage of a [single-file store](single_file_store.md)'s own container records is handled the
same way (`DamageKind::Container`). The rules that tell a torn tail from damage are normative:
[format specification, sections 9.6, 13 and 14.4](format.md#13-damage-handling).
