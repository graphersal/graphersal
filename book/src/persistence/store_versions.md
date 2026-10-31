# Format Versions and Upgrade

Every file of the format starts with a magic and a version. There are two axes (and the
single-file container has its own container version):

- the **store format version**, in the store's `GRAPH` file: this release writes and reads
  **version 4** (`graphersal::persist::STORE_FORMAT_VERSION`); `graphersal store info` prints it
  (`format  version 4`);
- the **file format version** of snapshot manifests and segments, WAL segments and packed
  snapshots (`.gsnap`): this release writes **version 4** (`graphersal::persist::FORMAT_VERSION`)
  and reads versions 1 to 4.

| Version | What changed |
|---|---|
| store 1 | the first store format |
| store 2 (0.1.0) | the store identity `store_id` in `GRAPH` (offset 100), which incremental backups check |
| store 3 (0.2.0) | the same `GRAPH` layout; marks a store whose files are of file format version 2, so that 0.1.x refuses it as newer instead of taking its files for damage |
| store 4 (0.5.0) | the same `GRAPH` layout; marks a store that may hold blobs, their ledger and the system journal, whose embedding models are blobs referenced by catalog definitions (0.4.0 kept them as files under `models/`), so that 0.4.x refuses it as newer |
| file 1 (0.1.x) | the first file format |
| file 2 (0.2.0) | the value tags of the date and time types (`date`, `localdatetime`, `datetime`, `time`, `duration`) and of vectors (`vector`); a version 1 file is read as it is |
| file 3 (0.3.0) | nanosecond value tags for `localdatetime`, `datetime` (with or without a zone name), `time`, `offsettime` and `duration` (the whole `java.time.Duration` range); every such value is written with them. Version 1 and 2 files are read as they are |
| file 4 (0.5.0) | the blob reference value tag (`blobref`, a content-addressed file of the store), the snapshot manifest's blob table and the system journal's segments; version 1 to 3 files are read as they are, and Graphersal 0.4.x refuses a version 4 file as newer, never as damage |

**Store format version 4 needs the upgrade** (`graphersal store upgrade`, below): a store of
0.2.x to 0.4.x is refused as older until it is upgraded, because a 0.4.0 store may keep
embedding models as files under `models/`, which become blobs ("0.4.0 -> 0.5.0" below). Its
older files stay as they are; everything written from then on is of file format version 4.

**File format version 3 (0.3.0) needed no upgrade.** The store format version stayed 3, so a
store written by 0.2.x opened in 0.3.x as it was: its version 2 snapshots and WAL segments stay and are read
as they are, and everything written from then on (a new WAL segment, new snapshots, backups,
`.gsnap` exports) is of file format version 3. From the first such file on, Graphersal 0.2.x
refuses the store, its backups and those packed snapshots as written by a newer version
(`PersistError::UnsupportedVersion`, the row below), never as damage. To hand data back to
0.2.x, export it as GraphSON. Files of version 2 never hold a value with digits below the
microsecond, an `offsettime` or a zone name: 0.2.x could not write them.

| The store or file is | Then |
|---|---|
| the current version | read and written as usual |
| **newer** than this build | refused with `PersistError::UnsupportedVersion` ("written by a newer Graphersal") by every operation (open, read-only open, `store info`, `verify`, backup, restore, fork, repair, conversion, upgrade, ...), before any lock, read of data or write: never guessed at, never reported as damage, never repaired. This holds for a newer `GRAPH` (either copy), for any snapshot manifest, snapshot segment or WAL segment of a newer file format version inside an otherwise current or older store (attic entries included), for a single-file container of a newer container version, and for a packed snapshot. Use a newer Graphersal |
| a store of an **older** released store format version (2: Graphersal 0.1.x; 3: Graphersal 0.2.x to 0.4.x) | refused with `PersistError::OlderFormat` by every operation but the converter and `repair` (open, read-only open, `store info`, `verify`, backup, restoring a backup directory, fork, convert; a ZIP backup is upgraded as it is restored); nothing is changed, never reported as damage. Its help names the command that upgrades it |
| a store of store format version 1 (never released) | refused with `PersistError::OlderFormat` everywhere; the migration is a packed snapshot exported by the build that wrote it (below) |

Two rules bind every release ([format specification, section 15](format.md#15-versioning)):

- **A version from the future is not damage, and a store holding one is never worked on.** A
  file of a newer version (intact magic and header checksum, a version word above this
  build's) MUST NOT be taken for damage, and a store, backup or single-file store that holds
  even one such file MUST NOT be worked on in any way: no write, no lock, no repair, no
  salvage, no maintenance mode, no upgrade, no checkpoint, no backup into or out of it. It is
  refused as a whole, so this build can never corrupt what a newer one wrote.
- **Format versions move only with a release that needs them.** A patch release (x.y.Z)
  never changes any format version (store, file or container). A format version changes only
  with a release whose new features need it; a release that changes no format reads and writes
  the stores of the release before it as they are, with no upgrade.

## The upgrade table

Which older store format versions this build upgrades, and how, is one table in the code
(`UPGRADES`, `persist/store/upgrade.rs`); this copy, and those in the
[format specification 15.1](format.md#151-upgrading-a-store) and the
[`store upgrade` command](store_cli.md#upgrade), are generated from it and checked by a test.
An upgrade is done only as its row says: "version word" rows only rewrite `GRAPH` and
`GRAPH.copy` and are safe in place; "migration" rows do that and migrate the embedding models of
a 0.4.0 store (below), in place too; "into a copy only" rows refuse `--in-place`
(`StoreFailure::UpgradeNeedsCopy`) before anything is written; an unsupported version (or one
without a row) is refused with `PersistError::OlderFormat`, whose help is the migration through
a packed snapshot (below). A bump of a format version fails that test until the table has a
row for the previous version.

<!-- upgrades:begin -->
| From store format version | Written by | To | Upgrade |
|---|---|---|---|
| 1 | never released (no store id) | 4 | unsupported (packed snapshot) |
| 2 | Graphersal 0.1.x (no models: the version word only) | 4 | in place or into a copy (migration) |
| 3 | Graphersal 0.2.x - 0.4.x | 4 | in place or into a copy (migration) |
| 4 | Graphersal 0.5.x and later | 4 | nothing to do |
<!-- upgrades:end -->

## Upgrading a store of an older version

**Every build reads every released format.** A release keeps the readers of every store and
file format version released before it (from 0.1.0 on), and the converter that upgrades such a
store needs no older Graphersal: `graphersal store upgrade` reads the older store with this
build. The golden fixtures written by the released 0.1.0 binary
(`crates/graphersal/tests/data/v0.1.0/`: a store with history, marks, an attic entry, schema and catalog; a single-file store;
a backup directory; a ZIP backup) are upgraded by every test run.

```bash
graphersal store upgrade old_store/ new_store/      # into a NEW store; old_store/ is not changed
graphersal store upgrade old.gstore new.gstore      # the same for a single-file store (any backend: new.gstore, a directory, --single-file)
graphersal store upgrade old_store/ --in-place      # the store itself (back it up first)
graphersal store upgrade old_backup/ new_backup/    # a backup directory: it stays a backup
graphersal store restore old_backup.zip new_store/  # a ZIP backup is upgraded as it is restored
```

In Rust `Store::upgrade(from, to)` and `Store::upgrade_in_place(dir)` (any `StoreDir`: a path, a
`.gstore` file, a `MemDir`), in Python `graphersal.Store.upgrade(source, target)` or
`graphersal.Store.upgrade(source, in_place=True)`; each returns what it did (`UpgradeReport`).

What it does:

- **Everything is carried, exactly**: the graph, its schema and catalog (saved queries,
  compression rules), the whole history (snapshots and WAL: every point in time stays a recovery
  target), the marks, the lineage, the attic entries (they stay restorable or forkable) and every
  [creation parameter](creation_parameters.md), the store id included: the upgraded store is the
  SAME store in the new format, so an upgraded backup of it continues to take increments.
- **Only `GRAPH` and `GRAPH.copy` are rewritten** (the store format version word); every
  snapshot, WAL segment, the `marks` file and the attic stay byte for byte. (A 0.4.0 store with
  embedding models also gets their blobs and one commit, and loses `models/`: below.) This build reads them
  as they are (each file names its own file format version), so nothing is decoded and encoded
  again and the history stays exactly what was written. The first commit after the upgrade goes
  into a NEW WAL segment of the current file format version: a record of the new version is
  never appended to a segment of the older one.
- **The older store is verified first** (every checksum, as `store verify` does). A damaged store
  is refused before anything is written (`StoreFailure::DamagedSource`); repair it instead:
  `graphersal store repair old_store/ --to new_store/` reads a store of an older released
  version and writes the repaired store in the current format.
- **Into a copy** (the default) the older store is only read; its writer lock is held while it is
  copied, so a store still open by a writer (an older Graphersal, a server) is refused as in use.
  The copy is verified before its `GRAPH` is written: an interrupted upgrade never leaves
  something that opens as a store in the target, and running the upgrade again into it clears
  what was left and completes.
- **In place** the store is verified, then `GRAPH.copy` and `GRAPH` are replaced, each
  atomically: whatever moment a crash hits, the store is either still the older one (run the
  upgrade again) or already the upgraded one (a reader takes the intact copy), never lost.
  Without models no other file changes, so no `INTENT` is needed (the model migration has
  one: below). Back the store up before (a file copy of the closed store will do): afterwards
  the older Graphersal refuses it as newer.
- A store that already has the current format is left alone in place (`already_current`); a copy
  of it is refused with `StoreFailure::AlreadyCurrent` (use `store convert` or a backup).

## 0.4.0 -> 0.5.0: models become blobs

Graphersal 0.4.0 kept the embedding models of a store as files under `models/<id>/` (the
model files, card, licence files and a manifest). 0.5.0 keeps each model as ONE blob (a
deterministic tar) referenced by a model definition of the catalog
([Embedding Models in a Store](store_models.md)). The upgrade converts them:

1. Every model under `models/` is checked against its manifest (every byte hashed) BEFORE
   anything is written. A damaged or incomplete model refuses the upgrade
   (`StoreFailure::UpgradeModelDamaged`, nothing written): restore `models/<id>` from a backup,
   or move it away, upgrade and import the model again afterwards.
2. An `INTENT` (`upgrade_models`) is written, then `GRAPH` gets version 4: from here on 0.4.0
   refuses the store (as newer), and an interruption is completed by the next read-write open or
   upgrade.
3. Each model becomes a blob; ONE commit (principal `upgrade`, attribute `upgrade` = `3->4`)
   defines all of them, in a new WAL segment. The history before that commit has no models: a
   view of a position before it whose schema names a model is refused ("not in the database at
   this position").
4. `models/` and the `INTENT` are removed.

### After the upgrade

The upgrade prints (and `UpgradeReport` holds) what still needs your decision; the exit code is
0 in every case.

- **No default model** is set: `hash-ngram/1` stays the default until you choose one
  (`graphersal store models` lists them; `Store::set_default_model`).
- A model 0.4.0 kept **without a repository name** (imported from a directory without `--repo`;
  identity `model2vec@<12 hex>`) gets the repository `local/<12 hex>` (a definition needs one),
  and the schema's `embedder` naming it is renamed in the same commit; the report and the CLI
  say so. Import it again with `--repo owner/name` to give it its real name. The same happens to
  a second revision of a repository another model of the store takes (the catalog keeps one
  model per repository).
- **Models the schema needs that the store does not keep** (0.4.0 served them through
  its `--embedder` flag, removed in 0.5.0) are listed (`UpgradeReport::models_missing`, a CLI `warning:` with the
  commands): such a store opens once they are imported (`graphersal embed-model download
  <repo>`, `graphersal embed-model import <repo> --into <store>`). The upgrade itself succeeds.
- **Vector properties without a rule.** 0.4.0's `embed_property` recorded only the model
  (`embedder`), never the property the text came from, so nothing keeps such vectors current.
  The upgrade never guesses the source and recomputes nothing: it lists each such property
  (`UpgradeReport::vectors_without_rule`: element kind, label, property, model, how many
  elements hold a vector, counted in the newest snapshot) and the CLI prints ONE `note:` per
  model with the ready schema patch (`UpgradeReport::vector_notes`). A model with one such
  property gets its own note, for example

  ```text
  note: doc.emb has vectors of acme/tiny (3 element(s)) but no source property; to keep it current add "from": "<source property>" to its schema declaration (graphersal --graph store -e 'g.patch_schema(#{vertices: #{doc: #{schema: #{properties: #{emb: #{from: "<source property>"}}}}}})'); the first fill re-embeds every doc (the whole database is locked while it runs), and from then on emb itself is not written directly (write the source property)
  ```

  A model with several such properties gets one note listing them (element kind, label,
  property, count) with ONE combined patch for all of them, for example

  ```text
  note: 3 vector properties are declared with vectors of acme/tiny (5 vector(s) in all) but have no source property: doc.emb (3), note.emb (2), draft.emb (no element holds one yet); to keep them current add "from": "<source property>" to each declaration, all in one change (graphersal --graph store -e 'g.patch_schema(#{vertices: #{doc: #{schema: #{properties: #{emb: #{from: "<source property>"}}}}, note: ..., draft: ...}})'); ...
  ```

  See [Vector Rules](../users_guide/vector_rules.md): adding `from` makes it a rule whose first
  fill re-embeds EVERY element of the label while the whole database is locked. Several labels
  can take their rule in ONE patch (one fill, one lock), and the preview says what it fills
  before anything is written:

  ```rhai
  g.validate_schema_patch(#{vertices: #{doc: #{schema: #{properties: #{emb: #{from: "text"}}}}}, note: #{schema: #{properties: #{emb: #{from: "text"}}}}}})["fill_warning"]
  ```

  From then on a direct write of the vector is refused (`GraphError::DerivedProperty`): a loader
  or saved query of 0.4.0 that computed the vector itself (`property("emb", ..)`) writes the
  source property instead. The upgrade lists every saved query whose body writes such a
  property by its literal name on a label its effects may write
  (`UpgradeReport::saved_queries_writing_vectors`, Python
  `report["saved_queries_writing_vectors"]`), one `note:` each, for example `note: saved query
  "load_docs" writes emb directly (doc.emb): once the vector rule exists, direct writes of it
  are refused: remove that write (the rule computes emb from its source property); the query
  is not rewritten`; the preview of the patch names them too (`fill[..].direct_writes`, and a
  line of `fill_warning`). A property declared with a model but holding no vector yet is listed
  too ("no element holds one yet").
- **Saved queries calling a removed function.** A saved query of 0.4.0 that calls
  `g.embed_property(..)` (removed in 0.5.0) is kept as it is, and a call of it fails with a
  help naming the vector rule. The upgrade lists each such query
  (`UpgradeReport::saved_queries_using_removed`, Python `report["saved_queries_using_removed"]`)
  and the CLI prints a `note:` with the ready command when the call names its label and
  properties literally, for example

  ```text
  note: saved query "fill" calls embed_property, removed in 0.5.0: a call of the query fails now, and it is not rewritten; replace the call by the vector rule (graphersal --graph store -e 'g.patch_schema(#{vertices: #{doc: #{schema: #{type: "object", properties: #{emb: #{type: "array", format: "vector", embedder: "hash-ngram/1", from: "text"}}}}}})'); its first fill embeds every element of the label (the whole database is locked while it runs)
  ```

  Then redefine or drop the query. `graphersal store create <dir> --from <0.4.0 .gsnap>` prints
  the same notes and warnings (`Store::create_from_packed_dir` returns them as an
  `UpgradeReport`; the store is created even when the schema needs a model the file does not
  carry, and opens once it is imported); from Rust, `graphersal::script::saved_queries_using_removed(&definitions)`
  scans any catalog.
- **Take a full backup after the upgrade.** A backup directory of the 0.4.0 store upgrades too
  (it stays a backup), but it gets its own migration commit, so it no longer takes increments of
  the upgraded store (`PersistError::BackupRefused`): back the upgraded store up into a new
  directory.
- A **ZIP backup** of 0.4.0 with models is migrated as it is restored (`graphersal store
  restore`); a **`.gsnap`** of 0.4.0 with model rows becomes a store whose first commit defines
  the models (`graphersal store create <dir> --from file.gsnap`), and
  `Store::import_models_from_packed` imports them into an open store. Attic entries are left as
  they are (their history predates the models).

The golden fixtures written by the released 0.4.0 binary (`crates/graphersal/tests/data/v0.4.0/`)
are upgraded by every test run.

**Packed snapshots need no converter.** A `.gsnap` written by 0.1.x (file format version 1)
loads as it is (`--graph graph.gsnap`, `TraversalGraph::read_snapshot`), and `graphersal store
create new_store/ --from graph.gsnap` makes a current store from it (its version 1 segment files
are kept byte for byte). That route carries only the graph state, its schema and its catalog.

**Store format version 1** was never released. A store of it (no store id) keeps the migration
through a packed snapshot: the build that wrote it exports the current state (`graphersal --graph
old_store/ -e 'g.export_snapshot("graph.gsnap")'`), and this build creates a new store from the
file. The full recipe:
[Command Cheat Sheet](../users_guide/command_cheat_sheet.md#migrate-a-store-from-an-older-format).

What counts as a format change, and the compatibility promises for third-party readers and
writers, are in the [format specification, section 15](format.md#15-versioning).

## Growing without a new version

New kinds of definitions in the catalog (saved queries, property indexes and compression rules
today; procedures and others later), new keys of a definition, and new kinds of snapshot files do **not** change the
format version ([format specification, section 16](format.md#16-extensibility)). A build that
meets something it does not know:

| It finds | Then |
|---|---|
| a definition of an unknown kind | kept byte for byte and written back unchanged at every checkpoint, fork, backup and repair |
| an unknown key of a saved query | kept and written back |
| a **critical** definition of an unknown kind (one that must stay consistent with the data, such as an index), or a snapshot file of a critical unknown kind | the store opens **read-only** (`Store::read_only_reason`, `PersistError::ReadOnly`): reading, `verify`, export, fork and backups work; writes need the newer version that wrote it |

Stores written before the catalog existed open unchanged: their catalog is empty, the first
saved query goes into the WAL and the next checkpoint writes it into the snapshot.
