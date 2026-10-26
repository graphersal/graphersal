# Verify

`verify` is the store's scrub: it reads every byte the store holds and checks it.
It also checks every [embedding model](store_models.md) the store keeps (each file's size and
SHA-256 against the model's manifest, `VerifyReport::models_checked`): a damaged model is a
problem under `models/<id>/`, but it never makes an open store read-only (it is no graph data);
remove it and add it again.

```rust,no_run
# use graphersal::persist::{Store, StoreOptions};
let report = Store::verify_dir("data")?;              // or store.verify() on an open store
println!("{} snapshot(s), {} WAL record(s), commits {}..{}",
         report.snapshots_checked, report.records_checked, report.first_commit, report.last_commit);
for problem in &report.problems {
    println!("DAMAGE: {} at {:?}: {}", problem.path, problem.offset, problem.reason);
}
assert!(report.is_ok());
# Ok::<(), Box<dyn std::error::Error>>(())
```

```text
$ graphersal store verify data/
2 snapshot(s), 2 WAL segment(s), 5 record(s), commits 1..3
ok: no damage found
```

It checks:

- both `GRAPH` copies, and the lineage chain;
- every snapshot: the manifest (both copies of its fixed part), `schema.json`, every segment's
  length and checksum, every chunk's checksum against the manifest's chunk index, and every
  element decoded (ids in order, ranges not overlapping);
- every WAL record's checksums, commit continuity (each commit is the previous + 1, along the
  lineage chain), the set of WAL segments against `GRAPH` (a missing, emptied or replaced newest
  segment, a segment whose header does not match its name, a clean close the files do not reach);
- every attic entry: its history is rebuilt (its base snapshot and the WAL after it), its
  snapshots verified;
- the `marks` file;
- the [donor rule](store_checkpoints.md#the-donor-rule): a segment file the newest snapshot
  shares with the previous one is a **warning** (`report.without_donor`, also a note), not
  damage: that id range has no independent donor until the next checkpoint copies it.

On damage, the CLI prints every problem and the [damage report](store_damage.md) with the donor
of each damaged item, and exits with **1**:

```text
$ graphersal store verify damaged/
2 snapshot(s), 2 WAL segment(s), 3 record(s), commits 1..2
DAMAGE: wal/00000000000000000002.wal: Corrupt journal 1 at byte 188: record of commit 3: checksum mismatch
DAMAGE: wal: the store was closed cleanly at commit 3, but its files end at commit 2
2 damaged item(s); readable state: snapshot 1 + WAL at commit 2; a repair LOSES data (see the items without a donor)
  DAMAGE wal/00000000000000000002.wal at byte 188: record checksum mismatch; 83 bytes up to the next valid record; commits from 3 on; no donor: LOST
  ...
Error: 2 problem(s) found. Help: keep the store's files (do not prune); build a repaired copy with `graphersal store repair <dir> --to <new_dir>`, restore a backup, or fork a target before the damage.
```

- `verify` takes no lock and changes nothing: run it while a dev server or another program has the
  store open, and on a backup.
- A normal open checks only what it loads (the latest snapshot, the WAL after it, `GRAPH`);
  `verify` checks everything, including the older snapshots that are the donors of a repair.
  **Run it regularly** (a nightly job, before a prune, after copying a store): it finds damage
  while donors still exist.
- Notes (`note: ...`) are not damage: for example a repaired copy of redundant metadata.
- The dev server's Store menu has **Verify the store**, the playground too (`POST
  /api/store/verify`); Python `store.verify()` returns `{"ok", "problems", "notes"}`.
