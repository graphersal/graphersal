# System Journal

The **system journal** records what an instance DID, one record per operational event: a store
opened or closed, a checkpoint written, a backup taken or failed, damage found, a rollback, a
permission denied, the system clock stepping back. A person reads it later ("what happened to
this store last night?"); a host forwards it to its own logging.

It sits beside three other streams and replaces none of them:

| Stream | Holds |
|---|---|
| the WAL ([Journal and Recovery](journal_and_recovery.md)) | the changes to the DATA, commit by commit |
| [Workload statistics](workload_statistics.md) | numbers per time bucket |
| `tracing` (the CLI's `GRAPHERSAL_LOG`) | developer diagnostics, unstable |
| the system journal | individual operational events, declared and stable |

The journal decides nothing: no part of Graphersal reads it to decide anything, and a failed
journal write never fails the operation it describes (it is a `tracing` warning and a counted
lost record, `SystemJournal::lost_records`). Records hold names, counts, durations and ids:
never data values, query text or secrets.

## Records

Every record has the same envelope plus typed fields:

| Field | Meaning |
|---|---|
| `seq` | the local sequence number: monotonic, never reused; the cursor of a reader |
| `origin` | the instance that created the record: a random id and a readable name |
| `origin_seq` | its sequence number there; `(origin, origin_seq)` identifies a record everywhere |
| `time` | microseconds since the Unix epoch (UTC), from the instance clock that also times the commits: a record about commit N is never earlier than commit N |
| `level` | `error`, `warn`, `notice` or `info` (most severe first) |
| `source` | a dotted hierarchy: `store`, `store.backup`, `store.damage`, `auth`, `clock` |
| `kind` | the stable event code (`store.backup_done`); every kind is declared below |
| `message` | the rendered English text |
| `op`, `parent` | the operation a record belongs to, and its parent operation (when set) |
| `commit` | the commit in the WAL the event relates to (when set) |
| `principal` | who: the channel the work came through (`cli`, `mcp`, `playground`, `python`, `host`) |
| `fields` | the kind's declared fields |

A reader of an older version shows a record of a kind it does not know by its message and
fields.

## Repeats and states

The journal is append-only, so a repeat is collected BEFORE it is written, never by rewriting
a record. Every kind declares a collapse key (its kind, its source and some of its fields; never
the time). The first occurrence is written at once. A record whose key equals one of the last
five written records is counted in memory instead; the group is closed by ONE
`journal.repeated` record (with the original's `origin_seq`, the count and the time range) when
the original leaves the last five records, when an hour has passed since the original (a
continuing problem shows again), or when the journal is flushed or closed. A crash loses only
the count of an open group.

A **state** (`store.damage_found`, `store.read_only`) is written once, its repeats are counted
while it holds, a `journal.state_reminder` repeats its current values once per hour, and its
`..._cleared` kind ends it.

## Where it is kept

- A graph without a store keeps its journal **in memory**: the newest 10 000 records
  (`JournalPage::truncated` says when older ones are gone). The playground's graphs too.
- A **Store** keeps it in its directory (format spec 4.7): segments `journal/<20 digits>.jnl`
  (the name is the first `seq` the segment may hold), append-only, framed like the WAL (sync
  marker, length, header and body CRC: a torn tail is cut by the next writer, a damaged record
  is skipped by resyncing on the next frame). A new segment starts past 4 MiB
  (`StoreOptions::with_journal_segment_bytes`), at an open by another instance, and after a
  failed write. There is no retention limit.
- `error` records are written and synced at once. The others are kept in memory and written in
  batches: before every checkpoint, by the system job `stats_flush` (every minute, when the
  host runs the [scheduled jobs](scheduled_jobs.md)), by `SystemJournal::flush`, and at close.
  So the journal never writes in the middle of another operation of the store; a crash loses
  at most the records since the last batch.
- A store opened read-only (a backup, maintenance mode, a store holding something critical of
  a newer version) reads its stored journal; its new records stay in memory. Once damage is
  found while a store is open (either damage policy), its journal writes nothing more to the
  store's files: the records stay in memory and in the `tracing` mirror.
- The instance identity lives in the instance-local file `INSTANCE` at the store root (a random
  id and a name: `StoreOptions::with_instance_name`, else the host name). No copy carries it: a
  restored backup or a converted store opened elsewhere is another instance, which writes into
  a new segment.
- The journal is never store damage: `verify` lists its problems as notes
  (`VerifyReport::journal_notes`), and its files are left out of the damage found while open.

## Copies

| Path | The journal |
|---|---|
| full backup, ZIP backup | every segment (the newest up to its last complete record), checked frame by frame in the store and in the copy; a problem is a `BackupReport::warnings` entry, never a failed backup; listed in `BackupReport::journal_files` |
| incremental backup | segments the backup lacks are copied, the backup's newest one gets the new tail appended (a backup segment that is not a prefix of the store's is left as it is, with a warning) |
| restore | the backup's journal becomes the store's; the restoring instance continues in a new segment |
| backup from memory | what is readable is copied |
| convert | carried (`INSTANCE` is not) |
| fork, attic fork, repair | NOT carried: the new store's first record `store.created` names the store and the commit it was made from |
| `.gsnap` export | not carried (a state, not operations) |

## Reading it

- **Rust**: `Graph::system_journal()` (every graph), `Store::journal()` (an open store),
  `Store::journal_dir(dir, &query)` (a closed store, or one open elsewhere: no lock, nothing
  written). `SystemJournal::query(&JournalQuery)` filters by time range (`since`/`until`),
  level (`min_level`), source (`source_prefix`: `store` matches `store.backup`), `kind`, `op`
  and `origin`, from a cursor (`after(seq)`), at most `limit` records (default 1000):
  `JournalPage::next_after` is the next cursor, `more` says that more follow.
  `SystemJournal::subscribe(after)` follows the journal (`wait(timeout)` blocks natively,
  WebAssembly polls); `add_sink` forwards every written record to a host's `JournalSink`;
  `open_operations()` is the last record of each operation without a `_done`/`_failed` record.
  Facade: `graphersal::system_journal`.
- **tracing**: every written record is mirrored under the target `graphersal::journal` at its
  level (`notice` as `info`); the CLI shows them on stderr with `GRAPHERSAL_LOG=info`.
- **CLI**: `graphersal store journal <DIR> [--level L] [--since TIME] [--source P] [--kind K]
  [--after SEQ] [--limit N] [--follow] [--json]` ([The graphersal store Command](store_cli.md)).
- **Dev server**: `GET /api/journal?after=N&level=..&source=..&kind=..&limit=..`, and the
  read-only MCP tool `journal` ([Dev Server and MCP](dev_server.md)).
- **Permissions**: reading is the resource `Journal` (`Resource::Journal`, `Read`); the session,
  the dev server and MCP ask the authorizer before they answer. `AccessPolicy::read_only()`
  allows it. The Rust API itself asks nothing (it is the host's).

The journal has no DSL access yet.

## Event catalogue

Every kind the journal writes, generated from the declarations in the code
(`GRAPHERSAL_UPDATE_BOOK=1 cargo test -p graphersal --lib catalogue`):

<!-- journal-events:begin -->
| Kind | Level | Source | Form | Fields | Description |
|---|---|---|---|---|---|
| `journal.repeated` | info | `journal` | event | `of_origin_seq`: the origin_seq of the original record; `of_kind`: the kind of the original record; `count`: how many more times it occurred; `first_us`: the first repeat (µs since the Unix epoch); `last_us`: the last repeat (µs since the Unix epoch) | Closes a group of repeats: the record `of_origin_seq` occurred `count` more times between `first_us` and `last_us` (written when the original leaves the last five records, after an hour, or at close). Its level is the original's. |
| `journal.state_reminder` | info | `journal` | event, collapsed by `state` | `state`: the kind of the state; `since_us`: when the state began (µs since the Unix epoch); `count`: how many times it was reported again since the last record | A state still holds (once per hour while it does), with its current values. Its level is the state's. |
| `journal.records_lost` | warn | `journal` | event, collapsed by kind | `count`: the records not written | Writing journal records failed; this many records are missing from the stored journal (they were kept in memory only). |
| `clock.step_back` | warn | `clock` | event, collapsed by kind | `system_us`: the system clock's reading (µs); `floor_us`: the instance clock (µs); `behind_ms`: how far the system clock is behind (ms) | The system clock stepped back more than a second behind the instance clock; times stay at the instance clock until the system clock catches up. |
| `store.opened` | info | `store` | event | `commit_seq`: the position after the open; `replayed`: WAL commits replayed onto the snapshot; `load_ms`: how long the open took (ms); `read_only`: whether it opened read-only (a backup, maintenance mode, ...) | The store was opened. |
| `store.recovered` | notice | `store.recovery` | event | `torn_tail_cut_bytes`: bytes of an incomplete last WAL record cut off; `intent_completed`: the interrupted operation completed (empty: none); `tmp_removed`: temporary files removed; `wal_cut`: whether a refused WAL record was cut off | The open repaired the traces of a crash or an interrupted operation. |
| `store.closed` | info | `store` | event | `commit_seq`: the last commit; `clean`: whether it was closed cleanly | The store was closed. |
| `store.read_only` | warn | `store` | state until `store.read_only_cleared` | `reason`: why | The store is read-only: it holds something critical this version does not know. |
| `store.read_only_cleared` | notice | `store` | ends `store.read_only` | - | The store is writable again. Not emitted yet: a later version (a store turns writable only at a new open today). |
| `store.checkpoint_done` | info | `store.checkpoint` | event | `commit_seq`: the snapshot's position; `merged`: whether it was merged from the last snapshot and the WAL; `bytes`: the snapshot's size; `ms`: how long it took; `automatic`: whether the store started it itself | A checkpoint wrote a snapshot. |
| `store.checkpoint_failed` | error | `store.checkpoint` | event, collapsed by kind | `error`: the error; `automatic`: whether the store started it itself | A checkpoint failed; nothing visible was written (level warn when the memory ceiling refused it). |
| `store.backup_done` | notice | `store.backup` | event | `target`: where it went; `mode`: full, increment, zip or memory; `commit_seq`: the backup's position; `bytes`: the bytes written | A backup was written and verified. |
| `store.backup_failed` | error | `store.backup` | event, collapsed by `target` | `target`: where it should have gone; `side`: where damage was found: store, backup or none; `error`: the error | A backup failed (level warn when the memory ceiling refused it). |
| `store.prune_done` | notice | `store.prune` | event | `up_to`: the position pruned up to; `snapshots_removed`: snapshots removed; `segments_removed`: WAL segments removed; `bytes`: bytes freed; `marks_unreachable`: marks no longer reachable | Old snapshots and WAL were removed. |
| `store.damage_found` | error | `store.damage` | state per `first_path` until `store.damage_cleared` | `found_by`: the operation that found it; `problems`: how many problems; `first_path`: the first damaged file | Damage was found in the store's files while it is open. |
| `store.damage_cleared` | notice | `store.damage` | ends `store.damage_found` | `first_path`: the file that was damaged | Damage found earlier is gone. Not emitted yet: a later version (damage is cleared by a repair into a new store today). |
| `store.maintenance_mode` | error | `store.damage` | event | `trigger`: what the open ran into | The store opened in maintenance mode (read-only): its files are damaged. |
| `store.repaired` | notice | `store.damage` | event | `target`: the new store; `commit_seq`: its position; `diverged`: whether the salvaged state diverged from the WAL | The store's salvaged state was written as a new store. |
| `store.rolled_back` | notice | `store.rollback` | event | `target`: the commit rolled back to; `mode`: move or delete; `attic_entry`?: the attic entry of the later history | The store was rolled back; the later history moved into the attic (or was deleted). |
| `store.attic_restored` | notice | `store.attic` | event | `entry`: the attic entry | An attic entry was restored as the store's current history. |
| `store.attic_removed` | notice | `store.attic` | event | `entry`: the attic entry | An attic entry was removed. |
| `store.created` | notice | `store` | event | `how`: create, packed, fork, attic_fork or repair; `from_store_id`?: the store it was made from; `from_commit`?: the position it was made from | The store was created (the first record of every store). |
| `store.upgraded` | notice | `store.upgrade` | event | `from_version`: the old store format version; `to_version`: the new one; `in_place`: whether it was upgraded in place | The store was upgraded to this version's store format. Not emitted yet: the store migrations of a later version. |
| `store.verified` | info | `store.verify` | event | `problems`: damage found; `warnings`: warnings; `notes`: notes (the system journal's among them) | The store's files were verified (level warn when problems or warnings were found). |
| `auth.denied` | warn | `auth` | event, collapsed by `action`, `resource_kind`, `channel` | `action`: the action asked; `resource_kind`: the kind of resource; `channel`: the channel the work came through | The authorizer denied a request (one per execution; no query text). |
<!-- journal-events:end -->
