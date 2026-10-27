# The `graphersal store` Command

`graphersal store <COMMAND> ...` manages a [Store](store.md) from the command line. The store
itself is used with `graphersal --graph <DIR>` (the REPL, `-e`, `--in`, `--server`): every commit
of a query is durable before the query returns.

```bash
graphersal store help                 # the overview (also: graphersal store, without a command)
graphersal store help rollback        # one command
graphersal store rollback --help      # the same (-h works too)
```

## Conventions

- **`DIR`** is a store: a directory, or a [single-file store](single_file_store.md) (an existing
  file, or a new path ending in `.gstore`). Every command works on both.
- **`TARGET`** is one of `--at-commit <N>` (right after commit N), `--at-time <TIME>` (after the
  last commit at or before TIME) or `--at-mark <NAME>`. `TIME` is an RFC 3339 / ISO 8601
  date-time with a zone (`2026-10-08T02:43:00Z`, `2026-10-08T02:43Z`,
  `2026-10-08T04:43:00+02:00`) or microseconds since the Unix epoch; it covers the whole second
  (minute) it names. A bare date (`2026-10-08`) is refused with the explicit forms to write
  instead (`2026-10-08T23:59:59Z` for the end of that day in UTC, `2026-10-08T00:00:00+02:00`
  with an offset). See [Point in Time](store_point_in_time.md#the-time-format).
- Options take their value as the next argument or after `=` (`--up-to 5`, `--up-to=5`).
  An option a command does not know is a usage error.
- **Exit codes**: `0` ok; `1` the operation failed (the diagnostic with a `Help:` line is on
  stderr), `verify` found damage, or `repair` lost data; `2` invalid invocation (unknown command
  or option, a missing argument or option value, a bad time, `graphersal store` without a
  command). The same holds for `graphersal --graph <DIR>`: a store that cannot be opened (in
  use, damaged beyond a read-only open, an older format) exits `1`; a `<DIR>` that is not a
  store (without `--create-store`) or a wrong option exits `2`. A command whose stdout was
  closed before its output was written (`graphersal store info x | head -1`) completes, closes
  the store and exits `141` (unless it failed: then its own code).
- **Warnings** of the store library (damage found, a read-only open, a failed write of `GRAPH`,
  a failed automatic checkpoint) are printed on stderr as `WARN graphersal::persist::store: ...`,
  never on stdout: by the `store` commands and by every mode of `graphersal --graph <DIR>` (`-e`,
  `--in`, a script file, the REPL, `--server`).
  `GRAPHERSAL_LOG` sets the level: `off`, `error`, `warn` (default), `info`, `debug`, `trace`.
- A read-write open (`--graph <DIR>`, `mark`, `prune`, `rollback`, `compact`, `attic
  restore|remove`) that finds one `GRAPH` copy missing, damaged or stale rewrites it from the
  other and says so in one line: `note: the store data/: GRAPH.copy is not usable (...); GRAPH
  is used; the other copy was rewritten from it`.
- Times are printed in UTC (`2026-10-08T07:05:08Z`), in the form `--at-time` accepts back.
- **While the store is open elsewhere** (a dev server, a REPL): `info`, `list`, `marks`,
  `verify`, `fork`, `backup`, `export`, `repair`, `compact-advice` and `attic` (list, changes,
  fork) work; `mark`, `checkpoint`, `prune`, `compact`, `rollback`, `attic restore|remove` and
  `convert` fail with "in use" (exit 1): use the dev server's Store menu, or stop the writer.
- **On a backup directory** the reading commands work, `prune` prunes the backup, `restore`
  makes it the live store, and the commands that write report that it is a backup.

## Commands

| Command | Does |
|---|---|
| [`create`](#create) | create a store |
| [`info`](#info) | identity, position, snapshots, marks, WAL size |
| [`list`](#list) | the snapshots |
| [`marks`](#marks) | the marks |
| [`mark`](#mark) | name the current position |
| [`checkpoint`](#checkpoint) | merge a snapshot now (closed store) |
| [`verify`](#verify) | scrub every checksum |
| [`fork`](#fork) | a past state as a new store |
| [`rollback`](#rollback) | take the store back in place |
| [`attic`](#attic) | list, inspect, restore, fork, remove rolled-back history |
| [`backup`](#backup) | full, incremental or ZIP backup |
| [`restore`](#restore) | a backup (or ZIP) as the live store |
| [`export`](#export) | a stored snapshot as a `.gsnap` |
| [`models`](#models) | the embedding models the store keeps; remove one |
| [`prune`](#prune) | remove old history |
| [`compact`](#compact) | prune to now; give a single file's free space back |
| [`compact-advice`](#compact-advice) | is `compact` worth it? |
| [`convert`](#convert) | a directory into a single file, or back |
| [`upgrade`](#upgrade) | a store of an older Graphersal, converted to the current format |
| [`repair`](#repair) | a damaged store, repaired into a new directory |

### create

```text
graphersal store create <DIR> [--from <SOURCE>] [--chunk-size <BYTES>] [--single-file]
                        [--on-damage maintenance|continue]
```

Creates a store in the new (missing or empty) `DIR`, at commit 0 with one snapshot. The chunk
size, the damage policy and the backend are [fixed for the store's life](creation_parameters.md).

| Option | |
|---|---|
| `--from <SOURCE>` | the initial graph: the `--graph` sources `modern`, `empty`, `large`, `tree`, a GraphSON/GraphML file, or a packed snapshot (`.gsnap`, copied as is). Default: an empty graph |
| `--chunk-size <BYTES>` | the snapshot chunk size (default 1 MiB), fixed for the store's life |
| `--single-file` | a single-file store whatever the name (a `DIR` ending in `.gstore` is one anyway) |
| `--on-damage <POLICY>` | what the store does when damage is found on disk while it is open: `maintenance` (default: it turns read-only) or `continue` (it keeps committing and reports the damage); fixed for the store's life |

```text
$ graphersal store create data/ --from modern
Created the store data/: graph 01a11a54-8209-7661-92e2-147ae6d8bbd3, commit 0, 1 snapshot(s); damage policy maintenance (fixed for its life).
Use it: graphersal --graph data/ (or --server)
$ graphersal store create graph.gstore --from modern --on-damage continue
Created the single-file store graph.gstore: graph 01a11a54-8ca5-7fd3-8ed5-fcd7729f0e80, commit 0, 1 snapshot(s); damage policy continue (fixed for its life).
```

Exit 2 when the source cannot be loaded or `--on-damage` names no policy; 1 when `DIR` is a
directory that is not empty (`cannot create a store in data/: the directory is not empty`, with
the help to choose an empty or new directory, or to look at it with `graphersal store info
data/` if it is a store) or a store already (`it is a store already`). `graphersal --graph <DIR> --create-store [--on-damage <POLICY>]` creates a store on first
use instead (`--on-damage` without `--create-store` is a usage error; for an existing store with
another policy it is refused: the policy never changes).

### info

```text
graphersal store info <DIR>
```

```text
$ graphersal store info data/
store       data/
format      version 2
graph id    01a11a54-8209-7661-92e2-147ae6d8bbd3
state       closed cleanly
commit      3 (2026-10-08T07:05:08Z)
snapshots   2
  latest    commit 3 (8 vertices, 6 edges)
marks       1
definitions 2 (compression 1, query 1)
wal         2 segment(s), 462 bytes
file        a directory
fixed at creation (never changed):
  store id      01a11a54-820a-7c33-b1f0-5d2e9a7c4e10
  created       2026-10-08T07:05:08Z
  chunk size    1048576 bytes
  on damage     maintenance (damage found while open makes the store read-only)
  backend       directory (format version 3)
```

Also printed when they apply: `parent` and `branched at` (a fork, a rolled-back store), `backup
up to commit N, taken T` (a backup), `attic  N entr(y/ies)`, the embedding models the store keeps
(a `models` block: one line per model with its identity, size, licence and where the licence text
comes from, `used by the schema`; a model the schema names but the store lacks as `missing`, see
[`models`](#models)), and for a single file
`file  single file, 23.0 KiB (23.0 KiB live, 0 B free space inside: ...)`. `state` is `open (in
use, or not closed cleanly)` while a writer has it (or after a crash).

A store written by a **newer** Graphersal (its `GRAPH`, a snapshot or WAL file, or the single
file's container of a newer format version) is not read: `info`, like every `store` command
(`upgrade` and `repair` included) and `--graph`, exits **1** with the refusal and changes nothing
(no lock file either):

```text
$ graphersal store info data/
Error: Unsupported format version 4 of the store data/ (store format version): written by a newer Graphersal (this build reads up to version 3)
Help: The store or file was written by a newer Graphersal; open it with that version (or a later one). ...
```

See [Format Versions](store_versions.md).

### list

```text
graphersal store list <DIR>
```

```text
      commit  last commit             vertices       edges  name
           0  -                              6           6
           3  2026-10-08T07:05:08Z           8           6  nightly
```

The snapshots, oldest first: the commit, the time of that commit, the counts, the name. Reads only
the manifests' first 4 KiB.

### marks

```text
graphersal store marks <DIR>
```

```text
           1  2026-10-08T07:05:08Z  before-import
```

The marks: commit, time, name.

### mark

```text
graphersal store mark <DIR> <NAME>
```

Sets the mark `NAME` at the current position (`Mark "before-import" at commit 1.`). Names are
unique along the store's whole history. "In use" while a server has the store open (its Store
menu has **Mark**).

### checkpoint

```text
graphersal store checkpoint <DIR> [--name <NAME>]
```

Writes a snapshot of the current state of the CLOSED store, merged from the last snapshot and the
WAL after it without loading the graph ([how](store_checkpoints.md#how-a-checkpoint-is-written)):

```text
Snapshot at commit 3 (8 vertices, 6 edges).
Merged from the snapshot at commit 0 and 3 commits: 4 segment files reused, 0 copied, 1 written.
```

`Nothing was committed after it: no new snapshot written.` when the newest snapshot is current.
"In use" while another process has the store open (a server's Store menu has **Checkpoint**,
which merges the same way while commits go on). Damage in what it reads stops it with exit 1 and
nothing written; run `verify`.

### verify

```text
graphersal store verify <DIR>
```

Reads every checksum (snapshots, WAL, `GRAPH`, marks). Exit **0** with `ok: no damage found`;
exit **1** with every problem and the damage report (the donor of each damaged item). See
[Verify](store_verify.md). Files of a newer format version are never listed as damage: `verify`
exits **1** with the "written by a newer Graphersal" refusal shown under [info](#info) (no
`DAMAGE` lines, no repair advice), and `store repair` refuses such a store the same way.

### fork

```text
graphersal store fork <DIR> <NEW_DIR> [TARGET]
```

A new store in `NEW_DIR` (it must not exist, or be empty) from the state at `TARGET` (default:
the latest), with a new lineage that records `DIR` as its parent. `DIR` is not changed.

```text
$ graphersal store fork data/ branch/ --at-mark before-import
Forked into branch/ at commit 1: graph 01a11a54-87d2-7995-b2cb-e450e4319735 (parent 01a11a54-8209-7661-92e2-147ae6d8bbd3).
Marks not carried (1; they stay targets of data/ only):
  before-import @ commit 1
Open it: graphersal --graph branch/ --server (or the REPL: graphersal --graph branch/).
data/ is unchanged; to undo the fork, delete the directory branch/.
```

A fork starts without marks: the `Marks not carried` lines (printed only when there are any) name
the source's marks at or before the fork's position. Exit 1 when the target is not reached
(`Recovery target mark "nosuch" not reached`).

### rollback

```text
graphersal store rollback <DIR> TARGET [--delete]
```

Rolls the store back **in place** to `TARGET`: everything after it moves to the attic (with
`--delete`: deleted, no undo), and the store continues as a new lineage. Prints the attic id and
the commands that undo it. Exit 2 without a target; exit 1 when there is nothing after the target
or the store is in use. See [Rollback and the Attic](store_rollback.md).

```text
$ graphersal store rollback data/ --at-commit 1 --delete
Rolled back from commit 4 to commit 1: new lineage 01a11a54-8b90-7dba-b74a-cadc851e226f.
The history after it (commits 2..=4, 1 snapshot(s), 2 WAL file(s)) was deleted: this cannot be undone.
```

### attic

```text
graphersal store attic <DIR>                       list the entries
graphersal store attic <DIR> changes <ID>          the commits of an entry
graphersal store attic <DIR> restore <ID>          undo its rollback
graphersal store attic <DIR> fork <ID> <NEW_DIR>   the entry as a new store
graphersal store attic <DIR> remove <ID>           delete the entry for good
```

```text
$ graphersal store attic data/
20261008T070510Z-00000000000000000002  commits 2..=4 rolled back at 2026-10-08T07:05:10Z (rollback to mark "before-import"), 9.3 KiB, 1 snapshot(s)
$ graphersal store attic data/ restore 20261008T070510Z-00000000000000000002
Restored 20261008T070510Z-00000000000000000002: the store is at commit 4 again.
```

`restore` works only while nothing was committed or marked since that rollback; `fork` always
does (and lists the marks of the entry's history it does not carry, like `store fork`). `restore` and `remove` are "in use" while a server has the store open.

### backup

```text
graphersal store backup <DIR> <BACKUP_DIR> [--full]
graphersal store backup <DIR> <FILE.zip> --zip [--without-models]
```

A consistent copy up to the last durable commit; works while a server has the store open. Into an
empty or new `BACKUP_DIR`: a full backup. Into an existing backup of `DIR`: an **incremental**
one. `--full` forces a full backup (the directory must be empty or new). `--zip` writes one ZIP
archive (always full; the file must not exist; feature `persist-zip`, on in the CLI).

```text
$ graphersal store backup data/ backup/
Full backup into backup/: snapshot 3, the WAL up to commit 3 (1 segment(s)); 8 file(s), 9605 bytes.
The backup is read-only (graphersal --graph backup/ opens it so); `graphersal store restore backup/` makes it the live store.
$ graphersal store backup data/ backup/
Incremental backup into backup/: nothing new since commit 4; the backup is current.
The backup is read-only (graphersal --graph backup/ opens it so); `graphersal store restore backup/` makes it the live store.
$ graphersal store backup data/ data.zip --zip
Full backup of snapshot 3 and the WAL up to commit 4: 8 file(s), 9688 bytes, in data.zip; every checksum read back.
```

The store's [embedding models](store_models.md) are copied too, with their cards and licence
files, and the report lists them (`It carries 1 embedding model(s) with their licences (handing it
on redistributes them):` and one line per model): who hands a backup on redistributes them.
`--without-models` (with `--zip` only; a usage error otherwise) leaves them out: such an archive is
also restored by Graphersal 0.3.0, which refuses one with models as written by a newer version.

Every checksum of what is copied is read in `DIR` first and again in the copy (a ZIP archive is
read back). **Damage** stops the backup (exit 1): nothing of it is kept (a new directory or archive
is removed, an increment rolled back), and the report names the damaged files and recommends
repairing the STORE (`store repair DIR --to NEW_DIR`), never the backup. Damage of a redundant
copy (one `GRAPH` copy, one manifest copy, the `marks` file) is a `warning:` on stderr; the backup
is intact.

```text
$ graphersal store backup data/ backup/
Error: The backup stopped: the store data/ is damaged: wal/00000000000000000001.wal at byte 64: record of commit 1: checksum mismatch; nothing of the backup into backup/ was kept
Help: Repair the MAIN store, never the backup: graphersal store repair data/ --to <new_dir> ...
```

Refused (exit 1, the backup unchanged): a backup of another graph, a directory holding a store
that is not a backup, a non-empty directory without a backup, `--full` into a non-empty directory,
a rollback of the store behind the backup, a pruned gap, a fork, repair or conversion of the
backed-up store (another store id). `--zip --full` is a usage error (exit 2: "--zip backups are
always full: drop --full").

`--from-memory` is a usage error here (exit 2) that says where it works: a backup from memory
writes the graph of a store **open in this process** (whose files are damaged), and this command
runs in a new process. Use the dev server's Store menu (Back up from memory), `POST
/api/store/backup {"from_memory": true}`, or Python `store.backup(path, from_memory=True)`. See
[Backup and Restore](store_backups.md).

### restore

```text
graphersal store restore <BACKUP_DIR>
graphersal store restore <FILE.zip> <DIR>
```

With one argument: makes the backup in `BACKUP_DIR` the **live** store, in place, with the
original's graph id (a backup directory of an older Graphersal: [`upgrade`](#upgrade) it first).
With two: unpacks a ZIP backup into the new directory `DIR` as the live store; a ZIP of an older
Graphersal's store is upgraded to the current format as it is unpacked (verified). To keep a backup as it is, fork it instead (`store fork BACKUP_DIR NEW_DIR`).

```text
$ graphersal store restore backup/
Restored: backup/ is now the live store (the backup up to commit 4, taken 2026-10-08T07:05:09Z; the same graph id). Use it: graphersal --graph backup/
$ graphersal store restore data.zip restored/
Restored into restored/ (the live store). Use it: graphersal --graph restored/
```

### export

```text
graphersal store export <DIR> <FILE.gsnap> [--commit <N>] [--without-models]
```

Writes the latest stored snapshot (or the one at commit `N`, which must be a snapshot's commit:
`store list`) as one packed file (`Exported the snapshot at commit 3 to data.gsnap.`). Load it
with `--graph FILE.gsnap`, `store create --from FILE.gsnap`, or the playground.

The file carries the store's [embedding models](store_models.md) with their cards and licences
(listed after the first line), unless `--without-models`. `store create --from FILE.gsnap` and the
playground put them back into a store; Graphersal 0.3.0 loads such a file and ignores them. A
damaged model stops the export (exit 1; export `--without-models`, or remove the model).

### models

```text
graphersal store models <DIR>
graphersal store models remove <DIR> <IDENTITY>
```

Lists the [embedding models](store_models.md) the store keeps, one line each: the identity a
schema's `embedder` records (`model2vec/<repo>@<12 hex>`), the size, the licence and where its text
comes from (`text from the repository`, `standard text, written by Graphersal`, `no licence
text`), `gated repository` when it applies, and whether the schema uses it; then every model the
schema names that the store does not keep (`missing`). Works while a server has the store open.

```text
$ graphersal store models data/
model2vec/minishlab/potion-base-8M@fac4fa854e68  29.5 MiB  licence mit (standard text, written by Graphersal)  used by the schema
```

`remove` deletes one model, by its identity or its id `model2vec-<12 hex>` (refused while a writer
has the store open, and on a backup; an unknown model exits 1). It is the only way a model leaves a
store. Models enter a store by `graphersal embed-model download <REPO> --into <DIR>` and
`graphersal embed-model import <MODEL_DIR> --into <DIR>` (see
[Embedding Models in a Store](store_models.md)).

### prune

```text
graphersal store prune <DIR> --up-to <COMMIT> [--drop-marks]
```

Removes snapshots and WAL only needed for targets before `COMMIT`; the last two snapshots and the
attic's bases always stay (`Kept from commit 0; removed 0 snapshot(s) [] and 0 WAL segment(s).`).
Without `--up-to` it is a usage error (exit 2: nothing is pruned implicitly). On a backup
directory it prunes the backup.

A prune never makes a [mark](store_marks.md) unreachable unless told to: when it would remove the
history a mark needs, it names those marks, changes nothing and exits 1:

```text
$ graphersal store prune data --up-to 4
Error: `store prune` would make the mark(s) "m0", "m1" unreachable: it removes the history they need, so they cannot be opened, forked or rolled back to afterwards (only from a backup that holds older snapshots). Nothing was changed.
Help: run it again with --drop-marks to prune anyway, or back the store up first (`graphersal store backup data <BACKUP_DIR>`).
```

With `--drop-marks` it prunes and names them on a second line ("No longer reachable (their
history was pruned): the mark(s) ..."); `store marks` no longer lists them. Without affected
marks the flag changes nothing. See [Prune, Compact and Retention](store_prune.md).

### compact

```text
graphersal store compact <DIR> [--drop-marks]
```

Prunes up to the current commit, then, for a single-file store, rewrites the file without its
free space (the disk needs room for the live data meanwhile). On a directory the prune is all.
Like `prune`, it refuses (exit 1, nothing changed) when the prune would make marks unreachable,
naming them, unless `--drop-marks` is given; `compact-advice` lists them beforehand
(`marks lost  m0, m1 (...)`).

```text
$ graphersal store compact graph.gstore
Pruned: kept from commit 0; removed 0 snapshot(s) and 0 WAL segment(s).
Compacted the file: 23.7 KiB -> 22.8 KiB (930 B given back).
```

### compact-advice

```text
graphersal store compact-advice <DIR> [--min-ratio <R>] [--min-reclaimable <BYTES>] [--min-size <BYTES>]
```

Whether `compact` is worth it now, and the numbers; cheap (reads only names, sizes and small metadata files, no element data) and works while a server
has the store open. The rule: at least `--min-ratio` of the store (0.0..1.0, default 0.5) and
`--min-reclaimable` bytes (default 64 MiB) to gain, in a store of at least `--min-size` bytes
(default 64 MiB), and enough free disk space. Always exit 0 (the answer is in the text: `compact
now: ...` with a `Run: graphersal store compact <DIR>` line, or `no compaction needed: ...`).

```text
$ graphersal store compact-advice data/
no compaction needed: the store is smaller than the minimum of 64.0 MiB
store       9.6 KiB (directory: compact = prune)
prune       0 B
reclaimable 0 B (0 % of the store)
decided by  total_bytes
```

### convert

```text
graphersal store convert <DIR> <NEW> [--single-file]
```

Copies the whole closed store (snapshots, WAL, marks, attic) into `NEW`, byte for byte: a directory
into a single file (`NEW` ending in `.gstore`, or `--single-file`) or back. `NEW` must not exist
(or be an empty directory); the copy is verified; `DIR` is not changed. The copy is a new store
with its own store id ([creation parameters](creation_parameters.md)): it does not continue the
backups of `DIR` (its first backup is a full one into a new directory).

```text
$ graphersal store convert data/ graph.gstore
Copied the store data/ into the single file graph.gstore: 9 file(s), 9.6 KiB; verified.
Use it: graphersal --graph graph.gstore. data/ is unchanged.
```

### upgrade

```text
graphersal store upgrade <DIR> <NEW> [--single-file]
graphersal store upgrade <DIR> --in-place
```

Converts a store written by an older Graphersal (store format version 2: Graphersal 0.1.x) to the
current format, **without the older binary**. `DIR` is a store directory, a single-file store or a
backup directory (it stays a backup). With `NEW`: a new store in `NEW` (a new or empty directory,
or a single file: `NEW` ending in `.gstore`, or `--single-file`); `DIR` is not changed. With
`--in-place`: `DIR` itself; back it up first (a copy of the closed directory or file will do):
afterwards the older Graphersal refuses it as newer.

Everything is carried: the graph, its schema and catalog, the history (snapshots and WAL), the
marks, the lineage, the attic entries and the store's identity and
[creation parameters](creation_parameters.md); only `GRAPH` and `GRAPH.copy` are rewritten, every
other file stays byte for byte. `DIR` is verified first. Exit **1** when it is damaged (nothing is
written; `store repair DIR --to NEW` writes a repaired store of the current format), in use (a
server or another writer has it open), of a newer or a pre-release version, or a copy of a store
that is already current; exit **2** without `NEW` or `--in-place`, or with a `NEW` that exists. An
upgrade in place of a current store prints `nothing to do` (exit 0). An interrupted upgrade leaves
no store in `NEW` (run it again: it clears what was left), and in place leaves the store either
older or upgraded (run it again). A ZIP backup of an older store is upgraded by
`store restore FILE.zip DIR`. See [Format Versions](store_versions.md).

Which versions are upgraded, and how, is decided by one table (the same in
[Format Versions](store_versions.md#the-upgrade-table) and the
[format specification 15.1](format.md#151-upgrading-a-store)); a version without a supported row is
refused before anything is written, a row "into a copy only" refuses `--in-place`
(`StoreFailure::UpgradeNeedsCopy`), and a store with any file of a newer version is refused like
every other command does (below):

<!-- upgrades:begin -->
| From store format version | Written by | To | Upgrade |
|---|---|---|---|
| 1 | never released (no store id) | 3 | unsupported (packed snapshot) |
| 2 | Graphersal 0.1.x | 3 | in place or into a copy (version word) |
| 3 | Graphersal 0.2.x and later | 3 | nothing to do |
<!-- upgrades:end -->

```text
$ graphersal store upgrade old/ new/
Verified the store old/ (store format version 2): 2 snapshot(s), 2 WAL segment(s), 10 record(s).
Upgraded into new/ (store format version 3): 15 file(s), 21.2 KiB; verified. old/ is unchanged (keep using new/: it is the same store, with the same identity).
Use it: graphersal --graph new/
$ graphersal store upgrade old/ --in-place
Verified the store old/ (store format version 2): 2 snapshot(s), 2 WAL segment(s), 10 record(s).
Upgraded old/ in place to store format version 3: GRAPH rewritten, every other file kept. The older Graphersal no longer opens it.
Use it: graphersal --graph old/
```

### repair

```text
graphersal store repair <DIR> --to <NEW_DIR>
```

Builds a repaired copy of a damaged store in `NEW_DIR` (never in place; the damaged files are not
changed) and reports what was repaired, lost and diverged. A store of an older released format
(Graphersal 0.1.x) is read as it is; the repaired store has the current format. Exit **0** with `ok: nothing lost`,
exit **1** when data is lost or diverged (the repaired store is written anyway: read the lists),
exit **2** without `--to`. The repaired store starts without marks: one `mark not carried: name @
commit N` line per mark of the damaged store at or before the repaired position. When both
`GRAPH` copies are damaged, `repair` and `verify` print the note `lineage before commit N is
approximate (both GRAPH copies damaged)` (`verify` then exits 1 with the damage report instead of
"not a store"). See [Damage, Maintenance and Repair](store_damage.md).
