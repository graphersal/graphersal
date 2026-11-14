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
| `kind` | the stable event code (`store.backup_done`); every kind is declared: the engine's below, a host's by the host ([Host events](#host-events)) |
| `message` | the rendered English text |
| `op`, `parent` | the operation a record belongs to, and its parent operation (when set) |
| `commit` | the commit in the WAL the event relates to (when set) |
| `principal` | who: the channel the work came through (`cli`, `mcp`, `playground`, `python`, `host`) |
| `fields` | the kind's declared fields |

A reader of an older version shows a record of a kind it does not know by its message and
fields.

A `recommendation` field says how to fix what the record reports, in the layout of every
Graphersal note: a short headline, then each command on its own line under a `# comment` saying
what it does. The text form of a record (`JournalRecord::to_line`, `graphersal store journal`,
the playground's journal panel) keeps the one-line fields in parentheses on the record's line
and puts a field of several lines below it, indented:

```text
2026-10-09T08:53:20.000000Z #12 error  model.unavailable the model acme/tiny is missing: queries with it fail (repo=acme/tiny, identity=model2vec/acme/tiny@0123456789ab, cause=missing)
  recommendation: download the model again and import it into the store, or restore the store from a backup or replica
    # download the model's files again
    graphersal embed-model download acme/tiny
    # import them into the store in place of the damaged copy
    graphersal embed-model import acme/tiny --into <store> --replace
```

The JSON forms (`--json`, `GET /api/journal`, MCP, Python) keep the field as one string with its
line breaks.

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
- `error` records are written and synced at once. The others are batched in memory and
  written as described in [When records appear](#when-records-appear), so the journal never
  writes in the middle of another operation of the store; a crash loses at most about the last
  second of records.
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

## When records appear

- **In the process that holds the graph or the store** a record is visible at once: every read
  (`Graph::system_journal()`, `Store::journal()`, the session, `GET /api/journal`, the MCP
  tool `journal`, the playground, Python's `graph.journal`/`store.journal`, the REPL) answers
  from the records in memory, written or not, and reads only older records from the segments,
  so a page never has a gap or a duplicate, before or after the batch is written.
- **On disk, and so for another process** (`graphersal store journal` or
  `Store::journal_dir` on a store a server holds open), a record appears within about a second:
  a store on a file system writes the batch half a second after its first record, from a
  thread of its own, independent of the statistics job `stats_flush`. The thread writes only
  while no other operation of the store runs (a checkpoint, prune, compaction, rollback,
  attic change, verify, backup, fork, export or close; commits are not waited for, they write
  other files); during a long checkpoint the batch waits for its end.
- Without threads (wasm32), and for a store in memory or in a host's own `StoreDir`, the host
  calls `Store::flush_journal()`: the playground does on its every-second tick
  (`Session::tick_jobs`). The system job `stats_flush`, `SystemJournal::flush` (which also
  closes the groups of repeats), the close and the next `error` record write the batch too,
  as does a batch of 1000 records or 1 MiB.

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
  `JournalQuery::newest_first()` answers the NEWEST matching records instead, newest first
  (a stored journal is read from its last segment backwards, one segment in memory at a time,
  and no older segment than needed is read): `next_before` is the cursor of the next older
  page (`before(seq)`), `more` says that older ones exist, and `next_after` is the newest
  record covered, so a reader shows the end of a long journal at once and then follows it.
  `SystemJournal::subscribe(after)` follows the journal (`wait(timeout)` blocks natively,
  WebAssembly polls); `add_sink` forwards every written record to a host's `JournalSink`;
  `open_operations()` is the last record of each operation without a `_done`/`_failed` record.
  Facade: `graphersal::system_journal`.
- **tracing**: every written record is mirrored under the target `graphersal::journal` at its
  level (`notice` as `info`); the CLI shows them on stderr with `GRAPHERSAL_LOG=info`.
- **CLI**: `graphersal store journal <DIR> [--level L] [--since TIME] [--source P] [--kind K]
  [--after SEQ | --last N] [--limit N] [--follow] [--json]`; `--last N` prints only the newest
  N records ([The graphersal store Command](store_cli.md)).
- **Dev server**: `GET /api/journal?after=N&level=..&source=..&kind=..&limit=..`
  (`newest_first=true&before=N`: newest first, paging backwards), and the read-only MCP tool
  `journal` (the same keys) ([Dev Server and MCP](dev_server.md)).
- **Python**: `graph.journal(...)` (every graph; without a store its in-memory journal),
  `store.journal(...)` and `Store.journal_dir(path, ...)` (a closed store, or one open
  elsewhere), with the keyword arguments `after`, `before`, `newest_first`, `since`, `until`,
  `level`, `source`, `kind`, `op`, `origin`, `limit` and `policy`. A page is a dict `{"records", "next_after",
  "truncated", "more", "notes"}`; each record has the keys of `store journal --json`
  ([Python](python.md#the-system-journal)).
- **Playground**: **Catalog ▸ System journal…** shows the served graph's journal, newest first,
  with a level and a source filter, in the WebAssembly playground (the graph's store in memory)
  and over the dev server. It opens on the newest 300 records (a newest-first page), then reads
  only the records written since, every five seconds; "Show more" shows 300 more rows, reading
  older pages backwards ([Web Playground](../users_guide/web_playground.md#system-journal)).
- **Permissions**: reading is the resource `Journal` (`Resource::Journal`, `Read`); the session,
  the dev server and MCP ask the authorizer before they answer. `AccessPolicy::read_only()`
  allows it. The Rust API itself asks nothing (it is the host's).

The journal has no DSL access yet.

## Host events

A host built on the engine (a server, a daemon embedding a Store) records its own operational
events in the same journal, beside the engine's: a panic it caught, an item it failed to process,
its own start and stop, an install change. They then show wherever the journal shows
(`graphersal store journal`, the MCP `journal` tool, `/api/journal`, the playground), with the
same filters, collapsing, sinks and `tracing` mirror.

A host kind is declared once per process with `graphersal::system_journal::declare`, like a kind
of the catalogue below: a stable code, a source, a level, one sentence and its fields, and
optionally a collapse key ("Repeats and states"; host kinds are events, not states).

```rust
# use graphersal::prelude::*;
use graphersal::system_journal::{HostEvent, HostEventKind, Level, declare};
# fn main() -> Result<(), Box<dyn std::error::Error>> {
# let graph = Graph::new(TraversalGraph::new());

declare(
    HostEventKind::new("myhost.job_failed", "myhost.jobs", Level::Error,
                       "A background job of the host failed.")
        .field("job", "the job's name")
        .optional_field("cause", "the error")
        .collapse_by(["job"]),
)?;

let seq = graph.system_journal()?.record(
    HostEvent::new("myhost.job_failed", "the job nightly panicked")
        .field("job", "nightly")
        .field("cause", "index out of bounds"),
)?;
# assert!(seq.is_some());
# Ok(())
# }
```

The rules:

- The code has two or more dotted segments, the source one or more, each of lowercase ASCII
  letters, digits and `_`, starting with a letter. The first segment of both names the host: it
  may not be a first segment of the engine's own codes or sources (`store`, `journal`, `auth`,
  `clock`, ...) nor `graphersal`, so `source_prefix("store")` never returns a host record. Pick
  a prefix naming your product: an engine release that adds a new first segment is listed in its
  release notes.
- A declaration holds for the whole process and every journal in it. Declaring the same kind
  again with the same declaration is a no-op (a component may declare its kinds at every start);
  a different declaration of a declared code, or a code of the engine's catalogue, is refused
  (`JournalError::InvalidDeclaration`). `declared_kinds()` lists the declarations.
- `SystemJournal::record` refuses an undeclared kind (`JournalError::Undeclared`), an engine kind,
  a field that is not declared or given twice, and a missing required field
  (`JournalError::InvalidEvent`): nothing is written then. Fields are kept in declaration order.
- Otherwise a host record is written like an engine record: timed by the instance clock, `error`
  records durable at once and the others batched, and a failed write of the stored journal is a
  lost record, never an error of `record`. It returns the record's `seq` (the `op` of later
  records of the same operation, `HostEvent::op`), or `None` when it was counted as a repeat.
  The principal defaults to the thread's channel (`host` unless a front end set one).
- Records keep the engine's layout: a reader of any version shows a host record by its message
  and fields, like a kind of a newer version. A host record holds names, counts, durations and
  ids like every record: never data values, query text or secrets.

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
| `store.upgraded` | notice | `store.upgrade` | event | `from_version`: the old store format version; `to_version`: the new one; `in_place`: whether it was upgraded in place; `models`: the embedding models of models/ that became blobs (Graphersal 0.4.0 stores) | The store was upgraded to this version's store format. |
| `store.verified` | info | `store.verify` | event | `problems`: damage found; `warnings`: warnings; `notes`: notes (the system journal's among them) | The store's files were verified (level warn when problems or warnings were found). |
| `auth.denied` | warn | `auth` | event, collapsed by `action`, `resource_kind`, `channel` | `action`: the action asked; `resource_kind`: the kind of resource; `channel`: the channel the work came through | The authorizer denied a request (one per execution; no query text). |
| `blob.damaged` | error | `blob` | event, collapsed by `hash`, `failure` | `hash`: the blob's hash (hex); `length`: its length in bytes; `failure`: what failed: missing, length, crc or hash; `found_by`: the operation that found it; `referrers`?: the catalog definitions that reference it, as kind:name; `recommendation`: how to get the bytes back | A blob is missing or damaged (never store damage: the store keeps working). |
| `blob.repaired` | notice | `blob` | event | `hash`: the blob's hash (hex); `how`: rewritten (written again) or restored (copied back); `source`?: where a restored blob came from (graphersal store restore-blobs --from) | A damaged blob was replaced by intact bytes of the same hash. |
| `blob.dropped` | warn | `blob` | event | `hash`: the blob's hash (hex); `referrers`: the catalog definitions left out with it, as kind:name | A repair left out a missing or damaged blob and the catalog definitions referencing it (graphersal store repair --drop-damaged-blobs). |
| `blob.collected` | info | `blob` | event | `count`: blobs removed; `bytes`: bytes freed; `by`: prune, attic_remove or rollback_delete | Blobs no retained history references were removed. |
| `catalog.closed_commit` | notice | `catalog` | event | `ops`: the catalog and schema changes of the commit; `kinds`: the definition kinds it changed; `wal_tail_commits`: the commits after the newest snapshot read to find the current catalog; `ms`: how long it took | A catalog change was appended to a CLOSED store's WAL as one commit, without loading the graph (format spec 12.12). |
| `ledger.rebuilt` | notice | `ledger` | event | `frames`: the frames written; `reason`: missing or damaged | The derived blob ledger was missing or damaged and was rebuilt from the WAL. |
| `model.imported` | notice | `model` | event | `repo`: its repository; `identity`: its identity; `bytes`: the bytes of its blob; `default`: whether it became the default model; `closed`?: true when the change was committed to a closed store (format spec 12.12) | An embedding model was imported into the store (one commit defining it). |
| `model.migrated` | notice | `model` | event | `repo`: its repository (local/<12 hex> when 0.4.0 kept none); `identity`: its identity now; `former_identity`: its identity in the 0.4.0 store; `bytes`: the bytes of its blob; `closed`?: true when the change was committed to a closed store (format spec 12.12) | An embedding model a Graphersal 0.4.0 store kept under models/ became a blob with a catalog definition (store upgrade to store format version 4; the migration commit). |
| `model.replaced` | notice | `model` | event | `repo`: its repository; `old_identity`: the replaced revision's identity; `new_identity`: the new revision's identity; `closed`?: true when the change was committed to a closed store (format spec 12.12) | A new revision of an embedding model replaced the current one (one commit). |
| `model.removed` | notice | `model` | event | `repo`: its repository; `identity`: its identity; `closed`?: true when the change was committed to a closed store (format spec 12.12) | An embedding model was removed from the store's current state (one commit). |
| `model.default_changed` | notice | `model` | event | `from`: the repository of the previous default (empty: none); `to`: the repository of the new default (empty: none); `closed`?: true when the change was committed to a closed store (format spec 12.12) | The default embedding model changed (one commit). |
| `model.loaded` | info | `model` | event, collapsed by `identity` | `repo`: its repository; `identity`: its identity; `load_bytes`: the memory the loaded model takes, measured at the load (counted by the tracking allocator, else the loader's estimate), not its size on disk; `ms`: how long the load took | An embedding model was loaded into memory. |
| `model.unloaded` | info | `model` | event, collapsed by `identity` | `repo`: its repository; `identity`: its identity | An embedding model no open position uses was unloaded. |
| `model.load_failed` | error | `model` | event, collapsed by `identity`, `cause` | `repo`: its repository; `identity`: its identity; `cause`: damaged, missing, does_not_fit or invalid; `needed`: the bytes a load needs (blob and loaded model); `recommendation`: how to bring the model back | An embedding model the schema needs did not load: the store did not open. |
| `model.unavailable` | error | `model` | event, collapsed by `identity`, `cause` | `repo`: its repository; `identity`: its identity; `cause`: damaged, missing, does_not_fit or invalid; `recommendation`: how to bring the model back | An embedding model nothing needs at open is missing or damaged: the store works, queries with the model fail. |
| `vector_fill.started` | notice | `vector_fill` | event | `reason`: rule_added, rule_changed or model_replaced; `rules`: the rules filled: element, label, from, to, model; `elements`: the elements to embed; `estimate_ms`?: the estimated time, when the model's speed is known | A vector fill started under the write lock (a vector rule added or changed, or its model replaced): the whole database is locked until it ends. |
| `vector_fill.done` | notice | `vector_fill` | event | `embedded`: the elements embedded; `removed`: the vectors removed (a source without text); `elapsed_ms`: how long the fill held the lock | A vector fill ended with the commit of its unit (`commit`); `op` is its start record. |
| `vector_fill.failed` | error | `vector_fill` | event | `cause`: the error; `elapsed_ms`: how long the fill held the lock; `embedded_before_failure`: the elements embedded before it failed | A vector fill failed (the model, the memory ceiling, a cancel): its unit was rolled back, nothing was written. |
| `index.unique_duplicates` | warn | `index` | event, collapsed by `name` | `name`: the index; `key`: its property key; `repeated`: how many values repeat; `values`: the first repeated values with their counts and example ids; `recommendation`: how to fix it | A unique property index is not served after the open: its data holds repeated values (a store written without the constraint, or an index stored without the create check). Queries scan it and writes are not checked against it until it is dropped and created again after the repeats are removed. |
<!-- journal-events:end -->
