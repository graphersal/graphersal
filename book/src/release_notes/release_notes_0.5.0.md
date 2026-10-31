# Release 0.5.0

A minor release about **models as data** and **memory that never runs out unannounced**. A
Store keeps its embedding models in its history, as blobs referenced by catalog definitions, so
every position, copy and backup has exactly the models of its moment; **vector rules** in the
schema keep vectors in step with their text; the process has a **memory ceiling, on by
default**, so a query, load or write that would exhaust the memory fails and rolls back instead
of the operating system killing the process. A **system journal** records what each instance
did, all times of an instance come from one monotonic clock, and a commit of any size is
written to the WAL in parts.

It is not a drop-in update: the store format changes (stores need `graphersal store upgrade`),
the external embedders and `embed_property` are removed, and the memory limit moves from the
execution options to the process. Read "Upgrading from 0.4" first; every removed or changed
API, with its replacement, is in "Breaking changes in detail" at the end of this page.

## Upgrading from 0.4

- **Stores need an upgrade.** The store format version is now 4 (file format version 4).
  Graphersal 0.5.0 refuses a store of 0.1.x to 0.4.x as "older format" until
  `graphersal store upgrade <dir> <new_dir>` (or `--in-place`; Rust `Store::upgrade`,
  `Store::upgrade_in_place`; Python `Store.upgrade`) converts it, and 0.4.x refuses an upgraded
  store as newer. The upgrade checks every 0.4.0 model file before it writes anything, turns
  each model under `models/` into a blob and defines them all in ONE commit; no default model
  is set ([Format Versions and Upgrade](../persistence/store_versions.md#040---050-models-become-blobs)).
- **What the upgrade tells you** (CLI `note:`/`warning:` lines, `UpgradeReport`, Python report
  dict; the exit code is 0): models converted or renamed (a model without a repository becomes
  `local/<12 hex>`), models the schema needs but the store does not keep (0.4.0 served them
  through `--embedder`; import them, or the store does not open), vector properties without a
  rule with a ready schema patch adding `"from"`, and saved queries calling the removed
  `embed_property` (never rewritten). The vector notes come as ONE note per model listing its
  properties with their counts and one combined patch (`UpgradeReport::vector_notes`,
  `VectorWithoutRule::combined_schema_patch`; a real 0.4.0 file went from 18 notes to one), and
  the saved queries that write a vector property directly (`property("emb", ..)` with a literal
  key) are listed too, one note each (`UpgradeReport::saved_queries_writing_vectors`): such a
  write fails once the rule exists, so remove it (queries are never rewritten; a computed key
  and `merge_v`/`merge_e` maps are not found). The `.gsnap` notes say the same.
- **Take a full backup after the upgrade.** An upgraded backup directory stays a backup, but it
  no longer takes increments of the upgraded store (`BackupRefused`): back the upgraded store up
  into a new directory.
- **A 0.4.0 `.gsnap`** loads as it is (`--graph file.gsnap`, `GraphSource::from_file`): its
  model rows are converted in memory, the file is not changed. To work on it durably,
  `graphersal store create <dir> --from file.gsnap` makes a store without opening it and prints
  the same report as the upgrade, also when the schema needs a model the file does not carry;
  import the model, then open the store.
- **Embedders come from the database only.** The CLI flags `--embedder`/`--default-embedder`
  (and their args-file keys), the Python `Embedder`/`Model2Vec` classes and keywords and the
  Rust host registry are removed. Import a model once
  (`graphersal embed-model import <repo> --into <store>`, `Store.import_model`,
  `Store::import_model`); it then travels with the store
  ([Embedding Models in a Store](../persistence/store_models.md)).
- **`embed_property` is removed**: declare a vector rule instead (`"from"` beside `embedder`
  in the schema). Its first fill embeds every element of the label while the whole database is
  locked; the preview says how many and how long first
  ([Vector Rules](../users_guide/vector_rules.md)).
- **The memory ceiling is on by default.** Unless the host sets it, it is the cgroup limit minus
  a headroom, else 80 % of the physical memory (3.5 GiB on wasm32). A query, import, load or
  store open that would take the process over it now FAILS (`ResourceLimitExceeded` or
  `PersistError::MemoryBudget`, naming the minimum ceiling that fits) where 0.4 went on.
  `memory.limit` is a process setting now: `exec::set_memory_limit`, CLI `--memory-limit`,
  Python `graphersal.set_memory_limit`; as a `g.with()` option, policy default or session preset
  it fails with `InvalidOption`
  ([The process memory ceiling](../users_guide/resource_limits.md#the-process-memory-ceiling)).
- **`traversal.max_materialized_bytes` means "held at once".** The per-query limit now counts
  the live bytes one execution holds at the same time (freed memory is given back), no longer
  the size of one value; every profile reports the query's peak against it
  ([Memory: materialized bytes and expansions](../users_guide/resource_limits.md#memory-materialized-bytes-and-expansions)).
  A query that built many medium values may now reach the default of 1 GiB: raise it per
  query, or for the CLI with `--max-materialized-bytes`, for Python with
  `graph.set_limits(max_materialized_bytes=...)`.

## Added

- **Models as data** ([Embedding Models in a Store](../persistence/store_models.md)): a model
  is ONE blob (a deterministic tar of its files, card, licence files and manifest) referenced by
  a catalog definition of kind 5. Importing, replacing, removing and choosing the default model
  are commits, so every position (commit, mark, view, rollback, backup) has the models of its
  moment. A model the current schema needs loads when the store opens, in every mode; a missing,
  damaged or too large one refuses the open with the commands that fix it. CLI
  `embed-model import <repo|dir> --into <store> [--replace] [--default]`,
  `embed-model download <repo> --into <store>`, `store models`, `store models default`,
  `store models remove`; Rust `Store::import_model`/`set_default_model`/`remove_model`/
  `models` and the closed-store forms `import_model_dir`/`set_default_model_dir`/
  `remove_model_dir`; Python `Store.import_model`, `Store.import_model_into`,
  `store.set_default_model`; the playground's Store menu marks the default model. A store that
  holds models but has no default says so (CLI note, Python `UserWarning`, `modelsNote` of the
  dev server and playground).
- **Catalog commits on a closed store**: a model import, removal or default change on a CLOSED
  store appends ONE commit without loading the graph (format spec 12.12); a replace whose old
  revision feeds vector rules opens the store and re-embeds, and says so.
- **A `.gsnap` without a Store serves its models**: `--graph file.gsnap`, Python `Graph.load`,
  `GraphSource::from_file` and `persist::read_snapshot_file` read a model's blob from the file
  by offset when the model is first used (no copy is kept in memory); a file changed meanwhile
  is refused for the model, a loaded model keeps running. A graph with no packed models has the
  built-in `hash-ngram/1` only.
- **Vector rules** ([Vector Rules](../users_guide/vector_rules.md)): a vector property with
  `embedder` and `"from": "<property>"` is recomputed at the end of every unit that writes its
  source, in the same unit; adding a rule (or replacing its model) fills every element of the
  label while the database is locked. Previews (`validate_schema`, `validate_schema_patch`,
  `diff_schema`) report the planned fill and one warning text; MCP `set_schema`/`patch_schema`
  take `confirm_lock` above the threshold; the playground asks "Apply and lock"; Python issues
  `graphersal.VectorFillWarning`. The estimate of the locked time includes the schema check
  (`PlannedFill::scan_elements`/`scan_ms`; `validate_schema` measures it), and the preview names
  the saved queries writing the filled property directly (`PlannedFill::direct_writes`, a line
  of the warning). A direct write of a rule's vector is refused
  (`GraphError::DerivedProperty`); replay and recovery never call a model.
- **Blobs** ([Blobs](../persistence/store_blobs.md)): immutable files of a store named by their
  SHA-256, referenced by `BlobRef` values (`graphersal::blob`), kept as long as retained history
  references them and collected by prune; `Store::write_blob`/`read_blob`. A missing or
  damaged blob is never store damage: only what needs its bytes fails. Every copy carries the
  blobs its history references (backups, increments, ZIP, fork, attic fork, repair, convert,
  upgrade, `.gsnap`). **Blob recovery**: `graphersal store restore-blobs <dir> --from
  <backup|zip|gsnap|store>` (`Store::restore_blobs_from`) copies missing or damaged blobs back
  from any copy, and `graphersal store repair --drop-damaged-blobs`
  (`RepairOptions::with_drop_damaged_blobs`) builds a repaired store without what references a
  lost blob.
- **The system journal** ([System Journal](../persistence/system_journal.md)): declared
  operational events (opens, closes, checkpoints, backups, prunes, damage, rollbacks, model
  loads, vector fills, permission denials) with levels and collapsed repeats, kept in memory per
  graph and in a store's `journal/`. It never decides anything and is never damage. Read it with
  `graphersal store journal <dir> [--level] [--since] [--kind] [--follow] [--json]`,
  `GET /api/journal`, the MCP tool `journal`, `Store::journal`/`Store::journal_dir`,
  `Graph::system_journal`, in Python with `graph.journal(...)`, `store.journal(...)` and
  `Store.journal_dir(path, ...)` (pages of dicts with the keys of `--json`), and in the
  playground under **Catalog ▸ System journal…** (newest first, level and source filters,
  incremental refresh; WebAssembly and dev server), where new records also pop up as
  notifications in the top-right corner (coloured by level; warn and error stay until closed;
  a click opens the journal at the record; the level is a setting, default notice); a newest-first page
  (`JournalQuery::newest_first`/`before`, `newest_first=true&before=N`, `--last N`) reads a
  stored journal from its end; reading needs `Read` on the new
  `Resource::Journal` (`AccessPolicy::read_only()` allows it; Python `policy=`).
- **One clock per instance** (format spec 18): commit, mark and checkpoint times and journal
  times never go backwards, also when the system clock steps back; the floor is restored from
  the files at open (`storage::CommitClock`, `GraphStorage::commit_clock`).
- **Commits of any size**: a commit body over 64 MiB is written as consecutive WAL records of
  64 MiB (format spec 9.3.1); the 1 GiB limit of one commit is gone (up to 2^20 parts). Memory
  bounds it now: a commit needs several times its size in RAM (a single commit of a little over
  1 GiB peaked at 4.4 to 5.7 GB in our measurements), so split very large writes into units
  ([The size of one commit](../users_guide/resource_limits.md#the-size-of-one-commit-journal)).
- **The process memory ceiling** covers every allocation proportional to data: queries
  (estimated or tracked live bytes), imports, snapshot loads, store opens and recovery (a graph
  that does not fit refuses the open, naming the minimum ceiling, never maintenance mode),
  checkpoints, backup verifies, model loads and vector fills
  ([What the ceiling covers](../users_guide/resource_limits.md#what-the-ceiling-covers)).
  `exec::check_memory_for` and `exec::memory_room` let a host check its own allocations.
- **Seeing how full the process is**: `process.memory` of the statistics (`/api/statistics`, MCP
  `statistics`) gains `usedBytes`, `limitBytes`, `limitSource`, `usedPercent` and the segments
  `structure`, `properties`, `compressed`, `indexes`, `models` and `other`, with the same names
  and figures as `g.memory_usage()` (`structure` is `structure_bytes`, `indexes` the id and
  label indexes plus the property indexes). The playground shows the bar in Profile ->
  Statistics and in Optimize -> Memory, the REPL before `/memory`, the dev server in its
  banner. PROFILE adds a "Peak memory X of Y" line (`profile::QueryMemory`,
  `TraversalMetrics::memory()`, JSON key `query_memory`). `g.memory_usage()` reports the loaded
  models as `model_bytes` (`MemoryStats::model_bytes`, the session's memory JSON, a row of the
  playground's statistics), beside `total_bytes`, not in it.
- **Bounded memory report**: `g.memory_report()` and `MemoryReportRequest` return the 500
  largest rows per list (a `limit` parameter; `usize::MAX` keeps every row) plus a "remaining N
  labels, X bytes" count; the totals cover everything. The MCP tool `memory_report` defaults to
  50 per list. On a graph with 111 000 labels the dev server's answer went from 207 MB in 4.2 s
  to 386 KB in 0.15 s.
- **CLI**: `--max-materialized-bytes` (the per-query limit), `--memory-limit <bytes|auto|none>`
  for the process, `store journal`, `store restore-blobs`, `store repair --drop-damaged-blobs`,
  `store models default`, `embed-model import --replace/--default`; `store info` shows
  the blobs, `store models` marks the default model. Python: `graphersal.set_memory_limit`,
  `graphersal.memory_limit`, `graph.limits()`, `memory_report(limit=)`, `VectorFillWarning`.

## Fixed

- **A job's `memory_limit`** lowered the process ceiling (it compared with the whole process);
  it is now the per-execution limit of the job's traversals.
- **PROFILE memory figures of nested steps**: the filter, `as` and side-effect steps inside a
  short-circuiting child traversal (`filter(__.out_v().not(__.has_label(..)))`) were measured
  against a zeroed snapshot and showed trillions of allocations under the tracking allocator.
- **`embed-model import`** takes a model from the download cache before looking for a
  directory of that name.
- **`--license`** of `embed-model import` now applies only when the model's manifest states no
  licence (the library's rule); before, it overrode the manifest.
- A ceiling `help()` no longer names a Rust crate path to a script user.
- **A closure using `g` inside a chain on `g`** (`g.queries().map(|q| g.get_query(q.name))`)
  failed with Rhai's bare "Data race detected": the error now explains it and its help names the
  forms that work (`let qs = g.queries(); qs.map(..)`, or a `for` loop).
- A text query on a property whose model the database does not keep now says "no model of this
  database has the identity ..." (`EmbedError::ModelNotRegistered`; there is no registry since
  0.5.0); its help names `g.embedders()` and `graphersal embed-model import`.

## Performance

- The short-circuiting child runs of `filter()`, `where()` and `not()` reuse their output
  buffers: `g.e().filter(__.out_v()...)` on the `large` graph makes 1 allocation instead of
  111 100 and runs 12 % faster (`filter(__.both().has(..))`: 28 %).
- A snapshot load passes under a ceiling of 1.00x its manifest estimate (the graph is no longer
  counted twice; it needed about 1.4x).
- The memory report picks its largest rows in one selection pass before building any row.
- **A schema change checks only the labels it changes** when the mode stays `open` or `closed`
  (the apply and the `validate_schema` preview); a change of the mode checks everything. Adding
  vector rules to labels of 9,000 to 30,500 elements in a graph of 2.5 million elements: the
  preview went from 587-668 ms to 6-15 ms, the apply from 616-679 ms to 39-99 ms (a store:
  709-824 ms to 82-183 ms). This relies on the data satisfying the stored schema, which every
  write checks: a storage of a host must reject writes that violate the schema in force
  (`GraphStorage::set_schema`, [Custom storage](../for_developers/custom_storage.md)).
- Blobs are written at about 276 MB/s (SHA-256, CRC-32C and the disk together); SHA-256 is the
  engine's own scalar code.

## Format

- **Store format version 4** (Graphersal 0.5.0): models are blobs referenced by catalog
  definitions of kind 5 (spec 4.5, 6.3), with the blob ledger (10.1) and the system journal
  (4.7); the 0.4.0 `models/` area is migrated by the upgrade (15.1, `INTENT` op
  `upgrade_models`).
- **File format version 4**: the blob reference value tag 21 (3), the manifest's blob table
  (6.2 item 5), system-journal segments (4.7) and the WAL Commit part record type 4 (9.3.1). A
  packed snapshot carries blobs as segment rows of kind 4 (7.5, 8).
- New spec sections: 4.6 (blobs), 4.7 (system journal), 9.3.1 (commit in parts), 10.1
  (`ledger`), 12.12 (catalog commits on a closed store), 18 (the instance clock); the vector-rule
  rule for replay in 9.3, blob recovery in 13.4, and the conformance notes of 17. No section was
  renumbered.
- Nothing new is a creation parameter: the hash algorithm is stated per blob, and the WAL part
  size is a writer's choice ([Creation Parameters](../persistence/creation_parameters.md)).
- `WORKLOAD` (the workload statistics) is no longer copied by a conversion (`store convert`),
  as the specification already said; an upgrade into a copy keeps it.

## Breaking changes in detail

Each row: what 0.4.x had, what 0.5.0 has instead.

| 0.4.x | 0.5.0 |
|---|---|
| `graphersal --embedder ml=<repo> --default-embedder ml --graph s.gstore` | `graphersal embed-model import <repo> --into s.gstore --default` once, then `graphersal --graph s.gstore` ([Embedding Models in a Store](../persistence/store_models.md#importing-a-model)) |
| `--embedder`/`--default-embedder` in an args file | key removed: a usage error (exit 2) naming the import command |
| `g.embed_property(#{label: "doc", from: "text", to: "emb", embedder: "ml"})` | a vector rule: schema `"emb": {"type": "array", "format": "vector", "embedder": "<model>", "from": "text"}` (`g.patch_schema(..)`); applying it fills every element while the database is locked ([Vector Rules](../users_guide/vector_rules.md)). A call of `embed_property`, also in a saved query, fails with a help showing the rule |
| a loader or saved query writing the vector itself (`property("emb", embed(..))`) | under a rule: write the source property; a direct write is `GraphError::DerivedProperty` |
| Rust `Embedders::builder().with(Arc::new(my))`, `GraphTraversalSource::with_embedders`, `Embedders::scope`, `script::set_engine_embedders`, `script::set_scope_embedders` | removed: compute vectors outside and write them as values, or import a Model2Vec model into the store (`Store::import_model(ModelSource::Dir(..), ImportModelOptions::new())`); a host building its own Rhai engine calls `script::set_engine_graph` |
| Python `graphersal.Embedder`, `graphersal.Model2Vec`, `embedders=`, `default_embedder=`, `graph.register_embedder`, `graph.embed_property`, `EmbedPropertyError` | `store.import_model(repo or dir, repo=, license=, replace=, default=)`, `Store.import_model_into(path, ..)`; vector rules ([Python](../library/python.md)). A removed keyword raises `TypeError` |
| `memory.limit`/`memory.headroom` as a `g.with()` option, an `ExecutionPolicy` default or a session preset | the process ceiling: `exec::set_memory_limit`/`set_memory_headroom`, CLI `--memory-limit`/`--memory-headroom`, Python `graphersal.set_memory_limit(.., headroom=)`; per query `traversal.max_materialized_bytes`. The old keys fail with `InvalidOption` |
| Python `memory_limit=`/`memory_headroom=` constructor and loader keywords | `graphersal.set_memory_limit(..)` (process); `graph.set_limits(max_materialized_bytes=..)` (per query) |
| `presets` of `AnyGraph::import_graphml`/`import_graphson` | removed (they carried only `memory.limit`) |
| `traversal.max_materialized_bytes`: the size of one value or buffer | the live bytes one execution holds at once; the profile reports the peak |
| no default memory ceiling | on by default (cgroup minus headroom, else 80 % of RAM; 3.5 GiB on wasm32); `none` turns it off |
| a commit body over 1 GiB vetoed (`CommitTooLarge`) | written in parts; `CommitTooLarge` only beyond 2^20 parts |
| `g.memory_report()`, `MemoryReportRequest::new()`: every row | the 500 largest rows per list plus the count of the rest (`limit`/`with_limit(usize::MAX)` for all) |
| `graphersal store export`: the snapshot plus the `models/` files as they were | the stored snapshot with the models of ITS position: a model imported after the last checkpoint is exported after the next `store checkpoint` |
| `embed-model import --license X` overriding the manifest | applies only when the manifest states no licence |
| a 0.1.x-0.4.x store opened as it is | `graphersal store upgrade` first (store format version 4) |

`graphersal` (Rust), removed from the public API (checked with `cargo semver-checks` against
v0.4.0):

- `vector::{Embedder, EmbedderInfo, EmbedContext, Embedders, EmbeddersBuilder, HashNgram,
  Model2Vec, Model2VecOptions, EmbedJob, EmbedReport, EmbedJobError, DEFAULT_MAX_BATCH,
  MAX_EMBEDDER_NAME_BYTES}`; the `vector` facade keeps the `Vector*` types,
  `MAX_VECTOR_DIMENSION` and `EmbedError`. Feature `model2vec` now means "the engine can load
  the models kept in the database" and has no public item of its own.
- `Graph::embed_property`, `AnyGraph::embed_property`,
  `ScriptGraphTraversalSource::embed_property`, `GraphTraversalSource::with_embedders`,
  `script::set_engine_embedders`, `script::set_scope_embedders`.
- `EmbedError::Registry` and `EmbedError::EmbedderMismatch`; the constructors
  `EmbedError::unavailable`/`failed`/`bad_output`/`no_features` are crate-internal.
- `persist::{StoredModel, ModelFile, AddModelReport, StoredEmbedders}`, `Store::add_model`,
  `read_model_file`, `stored_embedders`, `stored_embedders_dir`, `add_model_dir`,
  `read_model_file_dir`.

Changed:

- `persist::BaseModel` moved to `catalog::BaseModel`.
- `Store::models`/`models_dir` return `Vec<ModelInfo>`; `Store::remove_model(repo)` and
  `remove_model_dir(dir, repo, ClosedCommitOptions)` take a repository and return
  `ModelChangeReport`; `StoreInfo::models` and the `models` of `BackupReport`, `ForkReport` and
  `RepairReport` are `Vec<ModelInfo>`; `Store::import_models_from_packed` returns
  `Vec<BlobRef>`; `persist::ModelCopy` is an alias of `BlobCopy` (`Exclude` leaves out models
  and blobs).
- `AnyGraph::import_graphml`/`import_graphson` take no `presets` argument.
- `Store::import_model` with replace re-embeds the vectors of rules naming the old revision;
  vectors of it without a rule refuse the replace with `StoreFailure::ModelInUse`.
- `set_schema` may write data: the first fill of a vector rule.
- `Graph` and `Recovered` are no longer `UnwindSafe`/`RefUnwindSafe` (they hold the model
  registry and the instance clock).
- The implicit discriminants of `Resource::Custom` and `StoreFailure::AutoCheckpointFailed`
  changed (new variants before them); only an `as` cast sees it.
- `MemoryReportRequest::new()` is bounded (above).

Added (not breaking), beyond the features above: `GraphError::{DerivedProperty, VectorRuleFailed,
VectorRuleModelMissing}`, the field `needed` of `GraphError::ResourceLimitExceeded`,
`PersistError::MemoryBudget` fields `operation`, `in_use`, `setting`, `PersistError::Journal`, the
`StoreFailure` causes `BlobUnavailable`, `BlobDamaged`, `ModelUnavailable`, `ModelDoesNotFit`,
`ModelInUse`, `ModelRevisionExists`, `UpgradeModelDamaged` (each with a `faq.md` row),
`ElementProperty::Blob` (refused as element data), `SchemaChangeKind::{VectorRuleAdded,
VectorRuleChanged, VectorRuleRemoved}`, `VectorSchema::from`, `schema::{PlannedFill, FillReason,
fill_warning}`, `exec::FillProgress`, `Graph::vector_fill_progress`,
`UpgradeReport::{models_converted, migration_commit, models_missing, vectors_without_rule,
saved_queries_using_removed, saved_queries_writing_vectors, vector_notes}`,
`SnapshotLoadReport::saved_queries_writing_vectors`, `VectorWithoutRule::combined_schema_patch`,
`PlannedFill::{scan_elements, scan_ms, direct_writes}`, `catalog::{RemovedFunctionUse,
VectorPropertyWrite}`, `script::{saved_queries_using_removed, saved_queries_writing_vectors}`,
`persist::{ModelSource, ImportModelOptions, ModelInfo, ModelState, ImportModelReport,
ModelChangeReport, ClosedCommitOptions, BlobSource, RestoreBlobsReport, RepairOptions, ModelCache,
ConvertedModel, MissingModel, VectorWithoutRule, SnapshotLoadReport, read_snapshot_file}`,
`Store::create_from_packed_dir`, `Recovered::into_graph`, `RecoveryReport::clock_floor`, the report
fields for blobs, models and journal files.

`graphersal-session` and `graphersal-wasm` (internal): the session JSON carries `modelsNote`,
the models' `state`, `loadBytes`, `neededBy` and `default`, `fill`/`fill_warning` in schema
previews (each fill with `scan_elements`, `scan_ms`, `direct_writes`), `model_bytes` in the
memory JSON (`views::memory_stats_view`), `process.memory` with the bar keys, `query_memory` in profiles, and a `limit` for the
memory report; `Session::journal` and wasm `journal(spec)` read the system journal.
