# Release 0.4.0

A minor release about **errors that are never swallowed** and **readers that keep reading**. Every
public method that touches shared state (the graph's lock, a storage, a Store, files, embedders)
now returns a `Result` and never answers a failure with an empty or default value; a host's own
storage reports a backend failure through every method of `GraphStorage`, and the engine fails
the query with it. A whole-script unit and a playground run no longer block other readers while
they only read, and a checkpoint taken while a run is open no longer persists data the run later
rolls back. A Store can keep the **embedding models** it needs inside it.

It is not a drop-in update for Rust callers: read "Upgrading from 0.3" first. The DSL, GQL, the
schema format and the storage formats are unchanged; scripts and stores of 0.3 work as they are.

## Upgrading from 0.3

- **Result for shared state (Rust API).** Reads of `AnyGraph`, `Graph`, `Store` and
  `GraphTraversalSource` (`statistics()`, `memory_usage()`) that answered a plain value now
  return a `Result` with a `#[non_exhaustive]` error type (`GraphError`, `PersistError`,
  `TraverserError`). The upgrade step for each is "handle the `Result`" (`?`, or `.unwrap()`
  where the caller knows better). Every changed signature is listed under "Breaking changes in
  detail" below.
- **Storage implementers (EXPERIMENTAL `GraphStorage`, `PersistentStorage`, `StoreDir`).** Every
  method of `GraphStorage` returns `Result<_, GraphError>`, the element reads included
  (`vertex_count`, `vertices`, `out_edges`, `get_vertex_by_id`, `vertex_properties`, ...: one
  `Result` per call, never per item); the defaults return `Ok` of the old default. Wrap a
  backend failure with `GraphError::storage_failed`; the engine fails the reading query with it,
  located at its step. Return `Err` from `rollback_unit` only when the storage really is in an
  unknown state: a failed rollback now **poisons** the `Graph` that holds the storage (below). A
  wrapper storage over a storage held elsewhere forwards the new
  `GraphStorage::storage_identity()`. `TraversalGraph` never fails these reads and its impls are
  inlined, so the `Result` costs nothing on the hot paths. The guide is
  [Custom Storages](../for_developers/custom_storage.md); `graphersal-storage-tests` gains the
  `failing::Failing` storage, which fails each group of methods on purpose.
- **A failed rollback poisons the graph** (behaviour). When a host storage's `rollback_unit`
  (or `discard_unit`) fails in a unit the engine closes on a `Graph`, that graph refuses every
  later data operation with the new `GraphError::Poisoned`; `try_read`/`try_write` answer
  `None`, `read()`/`write()` panic. There is no unpoison: drop the graph and reopen the storage
  (`Graph::is_poisoned`, `Graph::poison_cause`). Exactly the graph whose storage failed is
  poisoned, never another one. `TraversalGraph` and the Store cannot get there (their rollback
  cannot fail) ([Transactions](../users_guide/transactions.md)).
- **Errors that surface now.** Failures that used to read as "no data" are errors: an
  unreadable store directory or single-file store (`Store::is_store`,
  `SingleFileDir::is_single_file`), an unreadable attic or models directory (`Store::info`), a
  failing compression estimate (`Graph::advise`), a read the current thread may not have
  (`GraphError::GraphViewHeld` instead of an empty answer), a poisoned internal lock of a Store
  (`StoreFailure::LockPoisoned` instead of a default answer). The CLI, the dev server, MCP and
  Python report them.
- **`graphersal-session`** (internal) returns the typed `SessionError` instead of `String`;
  `graphersal-wasm`'s JS methods throw instead of answering an empty value.
- **Python**: `Store.close()` raises on a poisoned store lock (was "already closed"),
  `Graph.execute` raises on a poisoned limits lock (was the default limits). Everything else is
  unchanged.
- **DSL**: a predicate whose operand is a list, map or traversal (`P.gt(["a"])`) is an error
  now; it used to compare with the operand's text (issue #14, under "Fixed").

## Added

- **Embedding models inside a Store**
  ([Embedding Models in a Store](../persistence/store_models.md), issue #10):
  `graphersal embed-model download <repo> --into <store>` and
  `embed-model import <dir> --into <store>` put a Model2Vec model into a store, with its model
  card, its licence files (a standard licence text when the model ships none, and the texts of
  its base models' licences) and a manifest with a SHA-256 per file. Opening the store registers
  its models, so a text query embeds with the model the schema recorded wherever the store goes.
  Every copy carries them (backups, ZIP backups, fork, attic fork, repair, convert, upgrade,
  restore, packed snapshots); `store verify` checks every file. `graphersal store models` lists
  and removes them, `store info` shows them, `store export` and `store backup --zip` take
  `--without-models`. Rust `Store::add_model`/`models`/`remove_model`, Python `Store.models()`
  and `Store.export(without_models=)`, the playground's Store menu (downloads with or without
  models).
- **Units held across locks** ([Transactions](../users_guide/transactions.md)):
  `Graph::begin_unit`/`commit_unit`/`commit_unit_with`/`rollback_unit`/`discard_unit`/
  `unit_changes`/`holds_unit` keep one unit over several locks on the opening thread. Other
  threads read on while the unit has only read and wait from its first write until it closes,
  so nobody sees uncommitted changes. Whole-script units (`eval_value_atomic`, dry runs, writing
  saved queries, query jobs) and the playground's and dev server's runs use them: readers are no
  longer blocked by a script that only reads. `Graph::try_write_checked` tells "busy" from
  "forbidden on this thread"; `Graph::read_checked`/`write_checked`/`try_read` report an
  aliasing view as `GraphError::GraphViewHeld` instead of panicking.
- **AI Chat export**: the AI Chat tab's **Export** menu downloads the transcript as Markdown or
  JSON ([AI Chat](../persistence/dev_server.md#ai-chat), issue #18). The AI Chat keeps its
  history compressed, shows the input, output and cached token counts of each answer, and
  answers from what it has when it reaches the tool-call limit.
- **Storage advisor**: every recommendation has an impact (low, medium, high: its share of the
  graph's memory, or the scans and reads it saves) beside its confidence, and the list is
  ordered by it ([Optimize](../persistence/optimize.md)).
- **Dev server on any address, and in Docker**
  ([Listening address and Docker](../persistence/dev_server.md#listening-address-and-docker)):
  the dev server (`graphersal --server`, with `--mcp` and `--ai`) has a new `--host <ADDR>`
  option to listen on any address (for example `--host 0.0.0.0`), and inside a Docker or Podman
  container it now listens on `0.0.0.0` by default, so
  `docker run -p 8080:8080 ... graphersal --server --port 8080` works without flags. A
  non-loopback address prints a warning that the server has no authentication; on the loopback
  the Host/Origin checks stay as strict as before, and on a network address they still refuse
  DNS-rebinding requests. A Kubernetes pod (`KUBERNETES_SERVICE_HOST`) counts as a container.
- **Running in Docker and Kubernetes** ([Running in Docker](../persistence/docker.md)): a
  `Dockerfile` in the repository builds the static `graphersal` binary into a small
  distroless image (non-root, `HEALTHCHECK`, `STOPSIGNAL SIGTERM`) whose default command serves
  `/data/graph.gstore` read-only with a read-only MCP endpoint.
  - **Health endpoints**: `GET`/`HEAD /healthz` (liveness: 200 while the process answers HTTP)
    and `/readyz` (readiness: 200 when the graph is loaded, 503 while it loads or the server
    stops). They take no lock, never wait for a running query and are exempt from the
    Host/Origin checks, so Kubernetes probes and Docker health checks work in every bind mode.
    The server listens before the graph loads: during a long Store open `/healthz` answers 200,
    `/readyz` and the API 503.
  - **`graphersal health [--ready] [--host ADDR] [--port N] [--timeout MS]`**: the probe for
    images without curl or wget (exit 0 when the endpoint answers 200, 1 otherwise).
  - **SIGTERM**: the dev server always stops on SIGINT/SIGTERM, also as PID 1 of a container
    (where a process without a handler ignores SIGTERM and `docker stop` waits for its kill):
    the running query is cancelled, `/readyz` turns 503, a Store is closed cleanly, and the
    process exits with 0. As PID 1, a one-shot script and the REPL exit with 130 on them too.
  - **`--read-only`**: the served graph refuses every write (queries, schema, catalog, "Save to
    file"; MCP offers only reading tools), and a Store is loaded without its lock and without
    writing anything, so a `:ro` mount, a read-only root file system or a file without write
    permission works, models included. A Store that cannot be opened for writing falls back to
    it with a note. Rust: `Store::stored_embedders_dir` (the models of a store loaded with
    `Store::open_read_only`); a single-file store on a read-only file system now reads like
    one without write permission.
- **Dev server**: the start output is grouped in sections (Store, Graph, Embedding models,
  Server) and names every embedding model with its repository, licence and source; the
  playground shows no Examples menu with the dev server (the examples are written for the sample
  graphs).
- **New Rust items** (additive): `Graph::begin_unit` and its closes, `Graph::try_write_checked`,
  `Graph::is_poisoned`, `Graph::poison_cause`, `GraphError::StorageFailed`, `RollbackFailed`,
  `Poisoned`, `GraphViewHeld` and `UnitNotHeld` (with constructors such as
  `GraphError::storage_failed`), `GraphStorage::storage_identity` and
  `storage::StorageIdentity`, `GraphStorage::next_unit_started_at`/`begin_unit_at` (defaulted),
  `UserJobs` and `NotScheduledJob`, `RepairReport::marks_unreadable`,
  `StoreFailure::AutoCheckpointFailed`, and the model API of the Store.
- **Benchmarks**: the Divan bench gains mutation cases (bulk writes through traversals, small
  write queries through the `Graph<S>` lock, Rhai scripts per statement and as one unit, GQL
  writes).

## Fixed

- **A checkpoint while a run is open no longer persists rolled-back data.** A playground or dev
  server run kept its unit open between per-traversal locks, so a Store checkpoint taken in that
  window could write data the run later rolled back. Runs now hold their unit across the locks,
  so a checkpoint never sees a run's uncommitted changes.
- **Predicates take single values** (issue #14): `P.gt(["a"])` compared with the text `["a"]`.
  Every step that takes a predicate now refuses a list, map or traversal operand with a help
  naming the predicate ([Predicates](../users_guide/predicates.md)); `P.within()` and
  `P.without()` without arguments are the empty set.
- **Saved queries with closures** (issue #16): a closure in a saved query's body is part of its
  one function, and a capturing closure no longer makes a writing body dynamic.
- **`define_query` takes the `params` list `get_query` returns** (issue #17), so get, change and
  define round-trip ([Saved Queries](../users_guide/saved_queries.md)).
- **Swallowed errors**: a session run's commit or rollback, the Store menu's compaction advice
  (now `compactionError`), an unknown job time zone, a failed system job, the workload and memory
  report answers, a stored model or embedder a store cannot load, and an AI Chat saved query
  whose entry cannot be read (now treated as writing) report their error.
- A storage read that fails while the engine builds the message of another error no longer
  replaces that error.

## Performance

- The `Result` of the element reads costs nothing measurable: the Divan bench of 0.4.0 against
  0.3.0 is within noise overall (geometric mean -0.4 %); the `glob_path()` walk, which first got
  4-9 % slower, reads the storage through a plain match again and is back at its 0.3.0 speed.

## Format

- No format version changes: the store format and the file format stay at version 3. Embedding
  models live in a new optional `models/` area of a store (format spec, section 4.5) and as
  ignorable rows of kind 3 in a packed snapshot's segment table (section 8). Graphersal 0.3.0
  opens a store with models and ignores them, loads a `.gsnap` with models, and refuses a ZIP
  backup with models as written by a newer version (make one with `--without-models` for it).

## Project

- Contributions are accepted under a Contributor License Agreement
  ([Version Control and Contributions](../for_developers/version_control.md)) instead of the DCO.

## Breaking changes in detail

One user decision (2026-10-28) drives them: a public method that touches shared state (the
graph's lock, the storage, a Store, files, embedders) returns a `Result` with a
`#[non_exhaustive]` error type, even where it cannot fail today, and never answers a failure with
an empty or default value.

`graphersal`:

- `AnyGraph` (EXPERIMENTAL): `capabilities() -> Result<StorageCapabilities, GraphError>`,
  `last_commit_seq() -> Result<u64, GraphError>`,
  `statistics() -> Result<Option<GraphStatistics>, GraphError>`,
  `memory_usage() -> Result<Option<MemoryStats>, GraphError>`,
  `get_schema() -> Result<Option<GraphSchema>, GraphError>`,
  `definitions(kind) -> Result<Vec<Definition>, GraphError>`,
  `definition(kind, name) -> Result<Option<Definition>, GraphError>`,
  `compression_stats(name) -> Result<Option<CompressionStats>, GraphError>`,
  `property_indexes() -> Result<Vec<PropertyIndexInfo>, GraphError>` (were the plain values; a
  read the current thread may not have was answered as "no data", now `GraphError::GraphViewHeld`).
- `Graph::advise` fails when a compression estimate fails for another reason than
  `GraphError::Unsupported` (was: advice without compression recommendations).
- `Store`: `read_only_reason() -> Result<Option<String>, PersistError>`,
  `stopped_reason() -> Result<Option<String>, PersistError>`,
  `damage_policy() -> Result<DamagePolicy, PersistError>`,
  `store_id() -> Result<u128, PersistError>`,
  `damage_found() -> Result<Vec<FoundDamage>, PersistError>`,
  `last_auto_checkpoint_error() -> Result<Option<Arc<PersistError>>, PersistError>` (was
  `Option<String>`), `user_jobs() -> Result<UserJobs, PersistError>` (was `Vec<Job>`;
  `UserJobs { jobs, not_scheduled }` lists the definitions it does not schedule with the reason),
  `stored_embedders(&registered) -> Result<StoredEmbedders, PersistError>`,
  `Store::is_store(dir) -> Result<bool, PersistError>` and
  `SingleFileDir::is_single_file(path) -> Result<bool, PersistError>` (an unreadable directory or
  file is `PersistError::Io`, not `false`). A poisoned internal lock is
  `StoreFailure::LockPoisoned` (was a default answer).
- `Store::info` (and `StoreInfo` everywhere) fails when the attic or the models directory cannot
  be read (was an empty list); a fork or attic fork fails before writing when the marks it does not
  carry cannot be listed.
- `GraphStorage` (EXPERIMENTAL; implementers and callers): the methods beyond single elements
  return `Result<_, GraphError>` (defaults: `Ok` of the old default): `statistics`,
  `infer_schema`, `get_schema`, `declared_property_schema`, `definitions` (`Result<impl
  Iterator>`), `definition`, `compression_stats`, `property_indexes`, the four property index
  lookups (`Result<Option<Box<dyn Iterator>>>`), `shrink_to_fit`, `memory_usage`,
  `memory_structure`, `memory_footprint_bytes`, `execution_policy`, `begin_unit`,
  `begin_unit_at`, `next_unit_started_at`, `rollback_unit`, `unit_changes`, `set_unit_metadata`,
  `capabilities`, `last_commit_seq`. A failed `rollback_unit` still closes the unit; the engine
  reports `GraphError::RollbackFailed`.
- Behaviour (user decision 2026-10-28): a failed rollback POISONS the `Graph` (was: "the graph
  stays usable"). When a host storage's `rollback_unit` (or `discard_unit`, other than the
  `Unsupported` of a storage without change capture) fails in any unit the engine closes on a
  `Graph` (a traversal, a transaction or dry run on a write view, an import, a change-set apply,
  a held unit's close, a panic unwinding through a unit), the graph refuses every later data
  operation with the new `GraphError::Poisoned { cause }` (`read_checked`, `write_checked`,
  `try_write_checked`, `begin_unit`, the closes of a held unit, the `AnyGraph` methods, sessions;
  `try_read`/`try_write` answer `None`) and `Graph::read()`/`write()` panic. No unpoison: drop the
  graph and reopen the storage. `TraversalGraph` and the Store cannot get there (their rollback
  cannot fail). Upgrade step for a host storage: return `Err` from `rollback_unit` only when the
  storage really is in an unknown state. The poison is isolated per graph: the failing storage is
  named by the new `GraphStorage::storage_identity() -> StorageIdentity` (opaque; default
  `StorageIdentity::of(self)`; a wrapper over a storage held elsewhere forwards it, as the shipped
  wrapper impls do; the pinned clock of a held unit is now found the same way, also for
  `Graph<Box<TraversalGraph>>` and wrapper storages), so a failure never
  poisons another graph on the same thread, and a bare storage poisons nothing.
- Behaviour: a storage read that fails while the engine builds the diagnostic of another error
  (the value origin of cast, numeric and incomparable-value errors, the type a string step, a
  list function or `T.value` names) no longer replaces that error with the storage's: the
  original error is reported without the unreadable detail.
- `GraphStorage`, the element reads too: `vertex_count`/`edge_count` -> `Result<usize, _>`,
  `vertices`/`edges`/`out_edges`/`in_edges` -> `Result<impl Iterator, _>` (one `Result` per
  call, never per item), `vertices_by_label`/`edges_by_label` -> `Result<Box<dyn Iterator>, _>`
  (their defaults now collect the matches first, so a failing label read is the call's error),
  `get_vertex_by_id`/`get_edge_by_id`/`vertex_id`/`edge_id`/`vertex_label`/`edge_label`/
  `vertex_properties`/`edge_properties`/`edge_endpoints` -> `Result<Option<_>, _>`,
  `vertex_labels` -> `Result<&[CompactString], _>`, `vertex_has_label`/`contains_vertex`/
  `contains_edge` -> `Result<bool, _>`, `unit_started_at` -> `Result<Option<i128>, _>`. The
  engine fails the reading query with the error, located at its step. `TraversalGraph`'s own
  inherent `vertex_count()`/`edge_count()` stay plain `usize` (an in-memory count that cannot
  fail).
- `CreateContext::for_storage` and `GraphStatistics::from_storage` return
  `Result<_, GraphError>` (they read the storage).
- `PersistentStorage` (EXPERIMENTAL): `set_graph_id`, `raise_position`, `reserve_load`,
  `finish_load`, `clear` return `Result<(), GraphError>`.
- `StoreDir` (EXPERIMENTAL): `space_usage() -> io::Result<Option<SpaceUsage>>`,
  `free_space() -> io::Result<Option<u64>>`, `check() -> io::Result<DirCheck>`. A failing
  `free_space` fails the compaction advice (`PersistError::Io`) instead of reading as unknown; a
  failing `check` opens the Store in maintenance mode with that damage (as `SingleFileDir` did
  for an unreadable file).
- `GraphTraversalSource::statistics() -> TraverserResult<Option<&GraphStatistics>>` and
  `memory_usage() -> TraverserResult<Option<MemoryStats>>` (were the plain values).

- Added (not breaking): `Graph::try_write_checked`, `UserJobs`, `NotScheduledJob`,
  `RepairReport::marks_unreadable`, `StoreFailure::AutoCheckpointFailed`,
  `GraphError::StorageFailed` (`GraphError::storage_failed`), `GraphError::RollbackFailed`
  (`GraphError::rollback_failed`), `GraphError::Poisoned` (`GraphError::poisoned`),
  `Graph::is_poisoned`, `Graph::poison_cause`, `GraphStorage::storage_identity` (with a
  default) and `storage::StorageIdentity`, `graphersal-storage-tests`' `failing::Failing` (with the `discard_unit`,
  `element_identity` and `fail_element_reads_after` faults).

`graphersal-session` (internal, listed for the CLI and the wasm crate):

- `SessionError` replaces `String` as the error of every public method that returned
  `Result<_, String>` (session methods, `SessionGraph`, `GraphFactory`, the generic helpers of
  `views`, `filter`, `selection`, `create`, `edit`, `queries`, `compression`, `layout`, `advisor`,
  `store::parse_time`).
- Now `Result<_, SessionError>`: `Session::info`, `statistics`, `get_schema`, `get_schema_text`,
  `infer_schema`, `infer_schema_text`, `load_shared`, `attach_store`, `list_queries`, `get_query`,
  `query_count`, `save_catalog_text`, `list_compressions`, `get_compression`,
  `compression_count`, `list_indexes`, `get_index`, `index_count`, `jobs_view`, `job_entry`,
  `workload_report`, `tick_jobs`, `read_only_reason`.
- `SessionGraph`: `vertex_count`, `edge_count`, `inferred_schema`, `schema_view`,
  `schema_view_of`, `graph_view`, `add_degrees`, `statistics_view`, `memory_view`, `element`,
  `subgraph`, `element_view`, `drawn`, `incident_edges`, `queries`, `query`, `count_kind`,
  `catalog_text`, `compressions`, `compression`, `indexes`, `index`, `run_changes`, `rollback_run`
  return `Result<_, GraphError>`.
- The generic helpers over a storage's element reads return `Result<_, GraphError>` too:
  `CompactGraph::build`, `CompactGraph::add_degrees`, `display::element_of`
  (`Result<Option<_>, _>`), `display::node_of`, `display::edge_of`, `display::collect_subgraph`
  (`Result<Option<_>, _>`), `views::graph_view`, `create::drawn_in`, `create::incident_edges`
  (`Result<Option<usize>, _>`), `edit::element_view`.
- `SessionStore`: `read_only_reason`, `damage_found`, `damage_policy`,
  `last_auto_checkpoint_error`, `user_jobs` return `Result<_, PersistError>` like `Store`; the
  trait gains `models`, `import_models_from_packed` and `export_packed_with` (embedding models
  in a Store), implemented for every `Store<S>`.
- The generic helpers `compressions_in`, `compression_in`, `indexes_in`, `index_in`,
  `queries_in`, `query_in`, `count_kind`, `catalog_text`, `views::schema_view`,
  `schema_view_of`, `statistics_view`, `memory_usage` return `Result<_, GraphError>` (they read
  through the `GraphStorage` methods that now return one).
- `store::found_damage_value -> Result<Value, PersistError>`; `StoreError` is
  `#[non_exhaustive]` (build it with `StoreError::new`, `bad_request`, `conflict`, `refused`,
  `internal`, `with_data`, or `From` a `SessionError`, `PersistError` or `GraphError`).
- The Store menu's JSON gains `compactionError` (beside `compaction: null` when the advice
  cannot be computed).

`graphersal-wasm` (internal JS API): `info`, `getSchema`, `inferSchema`, `schema`, `graphView`,
`memoryUsage`, `statistics`, `tickJobs`, `jobs`, `listQueries`, `getQuery`, `saveCatalog`,
`listCompressions`, `listIndexes` throw the error instead of answering an empty value.

Python: `Store.close()` raises on a poisoned store lock (was "already closed"), `Graph.execute`
raises on a poisoned limits lock (was the default limits).
