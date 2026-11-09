# Release 0.6.0

A minor release. Property indexes grow up: label-less indexes, indexes over array elements
(`each`), unique indexes, exact index reads and counts from an index, on a layout that takes a
fraction of the memory. The graph itself takes less memory (smaller id and label indexes, nested
objects stored as shapes, `dedup` for every value type, a smaller embedding model in memory). The
servers get their own building blocks: the MCP endpoint becomes the crate `graphersal-mcp`
(shorter tool descriptions, a compact schema summary, MCP over a byte stream, per-connection
profiles) and the dev server's hosting core the new crate `graphersal-serve`; read-only serving
gets a reader pool. A series of store, dev-server and playground fixes lands as well.

It changes some public types (property index labels, transforms, advisor actions) and what the
MCP tool `get_schema` answers by default: read "Upgrading from 0.5.2" first. Snapshot and journal
formats are unchanged and no store needs an upgrade step; a store that uses one of the new index
forms opens read-only in 0.5.x, and one with a `dedup` rule of `min_bytes: 0` is reported as
damaged by 0.5.2 and older.

## Upgrading from 0.5.2

**Stores.** Stores, snapshots, journals and backups of 0.5.x open unchanged, with no upgrade
step. What 0.6.0 writes can stop an older version from using a store:

- A new catalog kind 6 `property_index_ext` (`DefinitionKind::PROPERTY_INDEX_EXT`, CRITICAL) holds
  the new index forms (label-less, `each`, unique); once a store has one, Graphersal 0.5.x opens it
  read-only. Plain per-label indexes stay kind 2. Index names are one namespace across both kinds.
- A `dedup` compression rule with `min_bytes: 0` (the extended `dedup`, below) makes Graphersal
  0.5.2 and older report the store as damaged. There is no format marker for it: users upgrade,
  older versions need not open newer stores.

**Rust API (breaking).**

- `PropertyIndexDefinition::label()` returns `Option<&str>`; `PropertyIndexInfo.label` is
  `Option<String>` and `PropertyIndexInfo::new` takes `Option<&str>`.
- `GraphStorage::vertices_by_property_index`, `edges_by_property_index`,
  `vertices_by_transformed_property_index` and `edges_by_transformed_property_index` take
  `label: Option<&str>` (`None` = the label-less index).
- `RecommendedAction::CreateIndex.label` and `Recommendation.label` are `Option<String>`; the advice
  JSON and the memory report's index JSON show `"label": null` for a label-less index.
- The struct variants of `RecommendedAction` (`CreateIndex`, `DropIndex`, `AddCompression`,
  `DropCompression`) are `#[non_exhaustive]`: patterns outside the crate need `..`, and the
  variants cannot be built outside it. `RecommendedAction::CreateIndex` gains `transform`.
- `PropertyIndexTransform` gains `Each` and `Chain`, and `PropertyIndexTransform::KNOWN_NAMES`
  holds two names. A transform chain is an array in the payload, the session JSON, the DSL and
  Python. Indexes with `each` are kind 6 even with a label.
- `StoreFailure` gained `FilesInUse` in the middle of its variants: code that casts the enum to a
  number sees shifted discriminants.
- The Rust `from`/`to` of `add_e` take `impl Into<EndpointArgs>` instead of
  `impl Into<ArgStrings>` (neither type is nameable outside the crate; every argument type that
  compiled still does, see "Fixed").
- `graphersal-session` (internal): `Session::set_cancel_reset_by_host` is removed; every
  cancellable run of the dev server has a token of its own.

**Behaviour.**

- `create_index` without a label no longer fails: it creates a label-less index.
- The MCP tools `get_schema` and `infer_schema` answer a compact text summary by default, without
  the stored schema's descriptions. A client that parses the default answer as JSON must pass
  `format: "json"`; one that needs the descriptions passes `descriptions: true` or reads the
  resource `graphersal://schema-summary`.
- The MCP tool descriptions are the short ones (the long texts of 0.5.2 are gone), and a saved
  query offered as a tool (`--mcp-query-tools`) lists the paging parameters `offset`, `max_rows`
  and `max_items` only with `saved_query_tools.paging_params: true` in `--mcp-config`; a call may
  still pass them.
- `graphersal store repair` reports `all data kept` instead of `nothing lost`. Scripts that matched
  `nothing lost` should check the exit code instead (0: no data lost) or match `ok: all data kept`.

## Property indexes

- **Property indexes take far less memory.** Each indexed value is held once with its elements as
  compact slot lists: a low-cardinality index drops from about 48 to about 4 bytes per entry
  (field graph: an edge `_owner` index 28.2 -> 2.4 MB; a synthetic 1M-entry index over 500 values
  48 -> 4 MB); building one is 2-8x faster, lookups and maintenance equal or faster. Near-unique
  keys cost as before. No API or format change.
- **Label-less property indexes.** `g.create_index(#{key: "name"})` (no `label`) indexes every
  vertex (or, with `element: "edge"`, every edge) whatever its labels, unlabeled ones included.
  `g.v().has("name", x)` and `g.e().has("_owner", o)` without a label now read it instead of
  scanning (field graph: an edge owner lookup 46 ms -> 0.09 ms); a labelled source reads it when it
  is cheaper than its per-label indexes and checks the labels. `.profile()` shows
  `[index: vertex name, any label]`. GQL `MATCH (n {name: $x})` uses it too. Rust
  `PropertyIndexDefinition::label_less`, Python `graph.create_index(None, "name")`, session/HTTP
  with `label` absent or `null`, MCP `create_index` without `label`, playground "any label"
  ([Property Indexes](../users_guide/property_indexes.md)).
- **Array indexes: the `each` transform.** `g.create_index(#{key: "_owners", transform: "each"})`
  holds one entry per array element, so `g.v().has("_owners", __.unfold().is(o))` reads the index
  (field graph: 105 ms -> 5-13 ms); `transform: ["each", "to_lower"]` serves
  `__.unfold().to_lower().is(..)`. A whole-value `has(k, x)` can use it too (the value is
  re-checked). `.profile()` shows `[index: vertex _owners each, any label]`. New
  `PropertyIndexTransform::Each`/`Chain` and `PropertyIndexTransformChain`; the advisor recommends
  `each` for keys holding arrays.
- **Unique indexes.** `g.create_index(#{label: "User", key: "email", unique: true})` refuses a
  write that would give a second element the same value (per label; label-less = over all vertices
  or edges; with `transform: "to_lower"` case-insensitive). The check is immediate at each write,
  so swapping two values needs an intermediate one; `null` and values the index does not hold are
  outside uniqueness. Creating one over existing duplicates is refused with up to 10 of them and a
  query listing all. A store holding duplicates under a unique index (written by an older version)
  still opens: the index is not served, listed with the reason, and the journal records
  `index.unique_duplicates`. New `GraphError::UniqueViolation`,
  `DefinitionError::UniqueIndexDuplicates`, `GraphStorage::unique_index_duplicates`; Python
  `unique=True`; playground checkbox. Overhead: +1.5-4 % per insert over a plain index.
- **Exact index reads skip the re-check.** In a reading query, a condition a property index answers
  exactly (a string, boolean, uuid or temporal value, `P.eq` or `P.within`; whole-value, `each`
  under `__.unfold().is(..)`, `to_lower` and `["each","to_lower"]` while the index skipped nothing)
  is no longer re-checked on each candidate: the `has` step leaves the plan, and `count()` over one
  such point is answered from the index. Field graph: an owner lookup `.id()` 21-27 ms -> 1-2 ms,
  `.count()` -> microseconds. `.profile()` shows `, exact` in the index marker. Results are
  unchanged; writing queries, numbers, ranges and supersets keep the re-check. New
  `StorageCapabilities::exact_property_index_lookups` (EXPERIMENTAL trait; a host storage claims it
  only when its lookups return exactly the matching elements); `&G` and a shared
  `Arc<TraversalGraph>` forward it.
- **Counts from an index.** `g.v().has(k, "x").count()` served by an exact index (label-less, or the
  single label's) is answered from the index without visiting elements; `.profile()` shows
  `[count from index]`. New trait method `GraphStorage::count_by_property_index` (default
  `Ok(None)`).
- **Advisor**: recommends label-less indexes for scans without one label and hints when a key is
  indexed both per label and label-less. `help()` of a timed-out unlabeled filter names
  `g.create_index(#{key: ..})`.
- Fixed: a `to_lower` index no longer answers `has(k, __.to_lower().is(P.eq(())))` (null) with no
  rows where the scan finds them.

## Memory

- **Smaller id and label indexes.** The label index keeps handles plus one position per slot, the
  id index a slot and a hash: on a 534k-vertex / 2.3M-edge graph 157 -> 81 MB, the whole graph
  1.82 -> 1.64 GB.
- **Nested objects are stored as shapes**, also straight from a snapshot load, like top-level maps.
- **Embedding models take less memory**: a compact tokenizer (931 -> 514 MiB for the same vectors),
  streaming model load, bounded slot growth after a load (no doubling at the first insert)
  ([#24](https://github.com/graphersal/graphersal/issues/24)).
- **The memory report attributes the whole process memory** (graph, models, script engine, Stores,
  session, MCP, AI Chat); the playground and the REPL show a process memory bar.
- **The memory report needs less transient memory**: its per-row distinct-string sketch stays a
  small exact list up to 32 distinct texts (`--graph large`: peak 478 -> 408 MB); distinct counts
  up to 32 are exact.

## Compression rules

- **`dedup` codes top-level values of every type**, with an advisor pass for repeated values;
  compression rules are applied in bulk (one unit, one commit, one walk), several advisor
  recommendations at once, and a snapshot load codes rows straight into the dictionaries
  ([Compressed Properties](../users_guide/compressed_properties.md)).
- **`g.drop_compressions([names])`**: several rules dropped in one commit and one walk over the
  data, like `g.define_compressions`. Also `GraphStorage::remove_definitions` (with a default),
  `GraphTraversalSource::remove_definitions`, `AnyGraph::remove_definitions`,
  `POST /api/compression/drop-many`, Python `graph.drop_compressions(names)`.
- **Catalog ▸ Compression in the playground handles many rules.** The table no longer scrolls
  sideways, sorts by any column and groups the rules by codec or label with each group's total; a
  checkbox per rule and group and **Drop selected** drop them in one commit.
- **Advice to write a snapshot after changing compression rules.** Until a snapshot holds them,
  every open re-encodes their labels, with a higher memory peak (74 rules: 2.2-2.9 GiB against
  2.0 GiB). The Optimize panel shows a note with **Write a snapshot now**; the book pages
  [Optimize](../persistence/optimize.md), Compressed Properties and the
  [FAQ](../persistence/faq.md) say so.
- **A store whose compression rules are only in the WAL opens much faster.** An open walked the
  data once per rule, then once more per rule at its end. Now rule changes in a row, even over many
  commits, take one walk, and a dictionary just filled by it is not rebuilt: 74 `dedup` rules on a
  real graph of 2.8 million elements opened in 8.5-10.5 s instead of 26.7-28.7 s, about as fast as
  from a snapshot. A repair walks the same way, `TraversalGraph::replay` once per change set, and
  `apply_change_set` once per run of rule changes; a mix of drops and new rules in
  `apply_recommendations` takes two walks. Dictionaries that grew are rebuilt in one walk as well,
  also at the end of a commit and in `shrink_to_fit`. New
  `PersistentStorage::begin_replay`/`finish_replay` (defaults: nothing): between them
  `PersistentStorage::replay` walks once over the rule changes of many change sets.

## MCP and the servers

- **The MCP endpoint is its own crate, `graphersal-mcp`** (internal, EXPERIMENTAL), configured with
  the new `--mcp-config` file; `graphersal mcp-cost` shows what a configuration costs in context,
  and the resource `graphersal://gql-reference` (the whole GQL subset) is listed by default ([Dev
  Server and MCP](../persistence/dev_server.md#mcp-an-ai-agent-on-the-same-graph)).
- **One set of short tool descriptions.** About 6.7K tokens of context read-write and 4.6K
  read-only (12.6K / 10.0K before). In A/B tests with Claude Code and Sonnet on 48 read and write
  tasks the accuracy was the same, with a third of the failed calls and 19-27K fewer input tokens
  per task. The `gql` text names `element_id(n)`, `labels`/`type`, anchoring of variable-length
  patterns and, on writable servers, the write clauses; the `query` text the write steps and that
  a traversal before the last statement needs a terminal; the AI Chat prompt carries the GQL
  reference. Saved-query tools use the query's own description and list paging parameters only
  with `saved_query_tools.paging_params: true`. `--mcp-config` accepts `"descriptions": "short"`
  (it changes nothing) and refuses any other style.
- **Schema summary.** `get_schema` and `infer_schema` answer a compact text map of the graph by
  default: labels with counts, keys with types and how often they occur, edge from->to label
  pairs, indexes and the schema mode (field graph: 24k chars, where the JSON did not fit the answer
  limit at all). `labels` details some labels; `format: "json"` returns the JSON as before. A/B
  with Sonnet on 16 schema tasks: 48/48 correct vs 46/48 with the JSON, half the calls and half the
  time. Resources mirror it: `graphersal://schema-summary` (text, new) and `graphersal://schema`
  (JSON), both listed by default. Rust `GraphTraversalSource::schema_summary` /
  `AnyGraph::schema_summary` with `graphersal::schema::{SchemaSummary, SchemaSummaryRequest}`;
  REPL `/schema [label ...]`; the AI Chat prompt carries the summary.
- **The schema summary leaves out the stored schema's descriptions by default**
  ([#34](https://github.com/graphersal/graphersal/issues/34)): 24,182 -> 15,459 characters on a
  real knowledge graph (-36 %). `descriptions: true` or the resource `graphersal://schema-summary`
  give the full summary. A/B on 16 schema tasks: billed cost -13 %, input tokens -9 %, accuracy
  within noise (46/48 vs 48/48).
- **MCP over a byte stream.** Besides HTTP, `graphersal-mcp` serves MCP over any reader and writer:
  `Connection` (one MCP session with its own host) and `serve_lines` (the MCP stdio framing,
  newline-delimited JSON-RPC). On stream connections the server sends
  `notifications/tools/list_changed` (when saved queries are tools) and `notifications/progress`
  heartbeats for tool calls that carry a `progressToken`; `Mcp::check_tools_changed` lets a host
  trigger the notice. Cancellation works the same over HTTP and streams. The dev server's HTTP MCP
  endpoint is unchanged (no SSE).
- **Per-connection profiles.** A `Profile` (an `McpConfig`, read-only, an optional `AccessPolicy`
  the engine enforces, an answer limit) is chosen by the host for each connection; the session's
  writing tools follow the policy too. A host per connection, and an identity carried to the
  session jobs (`ClientRef::identity`).
- **Read-only serving has a reader pool**: stateless MCP session ids, per-client limits, parallel
  readers ([Read-only serving mode](../persistence/dev_server.md#read-only-serving-mode)).
- **MCP usage statistics in memory**: `/api/mcp/stats`, a playground dialog and the opt-in tool
  `usage_statistics`.
- **A self-contained knowledge-base image** from prebuilt binaries, and `--allowed-host`.
- **New crate `graphersal-serve`** (internal, EXPERIMENTAL): the hosting core of the dev server
  (session thread and job queue, job workers, the run registry, the playground HTTP API as an
  HTTP-library-free boundary, Host/Origin/loopback checks, the embedded UI behind the feature
  `ui-assets`, the `graphersal-mcp` glue behind the feature `mcp`; no default features).
  `graphersal --server` behaves as before (flags, HTTP API, MCP endpoint). Its extension points:
  a graph resolver (default: one graph), a run observer with a cancel handle, a request filter
  (it runs after the always-on Host/Origin checks), extra routes for unknown `/api/` paths,
  lifecycle and activity hooks, an executor policy (`JobQueue`), one MCP endpoint shared by several
  contexts with a profile per context (`Context::mcp_connection`), and the request filter's
  identity reaching MCP runs. A `Server` type answers the health probes and the loading/stopping
  503s before the graph is loaded.
- `graphersal-mcp` and `graphersal-serve` have no stability guarantees during 0.x: a dependent pins
  their exact versions ([Releases and Versioning](../for_developers/release_plan.md)). Their
  errors follow the crate rule for shared state: `serve_lines` returns `Result<(), McpError>`,
  `Context::mcp_connection` and the run registry's methods return `Result`, and a session's setup
  error keeps the host's error as its source (`ServeError::Setup`).
- A panicking session job of the dev server is logged through `tracing` (target
  `graphersal::server`) instead of a `warning:` line on stderr; an unreadable store space usage
  answers 500 instead of ending the server, and a refused job-ticker time zone is an error.
  `--ui-dir` is documented as an overlay over the embedded UI (only the files to override).

## Dev server and playground

- **A running scheduled job no longer holds up the dev server**
  ([#26](https://github.com/graphersal/graphersal/issues/26)). Requests are answered while a
  backup, a checkpoint or a reading query job runs; `GET /api/jobs` says `running`, and the jobs
  dialog shows a running badge. While a user's job runs, Store-menu operations on the files answer
  409. New in `graphersal::jobs`: `Scheduler::start_due`/`start_now`/`finish`/`running`/
  `set_time_zone`, `JobRun`, `JobFinished`, `Job::with_group`, `JobInfo::running`,
  `JobError::Running`.
- The Scheduled jobs dialog lists one wrapping block per job instead of a table that scrolled
  sideways. The AI Chat shows a thinking timer, and its Markdown export holds the answers only.

## Fixed

- **A Stop hits the right query**
  ([#30](https://github.com/graphersal/graphersal/issues/30),
  [#31](https://github.com/graphersal/graphersal/issues/31)). A shared, reused cancel token lost a
  cancel or stopped the wrong work: on the read-write server a Stop before a query reached the
  session thread was reset by the query (or stopped another page's work), and the drain of the
  read-only serving mode cancelled worker tokens that an AI Chat query does not check. Every query,
  layout slice, MCP query and AI Chat tool call now registers a token of its own under its client:
  a Stop cancels the page's run (or all its runs), MCP `notifications/cancelled` the client's own
  request, the graceful stop every run, never another client's run. Store views and rollbacks wait
  for running queries instead of cancelling them.
- **Read-only serving: a Stop just before a query starts is no longer lost**
  ([#29](https://github.com/graphersal/graphersal/issues/29)). `POST /api/cancel` answered
  `cancelled: true` and the query still ran to the end; a graceful stop past its drain timeout had
  the same gap.
- **A backup or a verification beside a rollback no longer freezes an undamaged store**
  ([#27](https://github.com/graphersal/graphersal/issues/27)). They wait while a rollback or an
  attic change moves files; a rollback, attic restore or attic remove is refused with the new cause
  `StoreFailure::FilesInUse` while files are being read.
- **A fork or a packed export beside a rollback no longer fails with an I/O error or calls a
  manifest corrupt** ([#28](https://github.com/graphersal/graphersal/issues/28)). `Store::fork`,
  `Store::attic_fork` and `Store::export_packed` hold the same pin as a backup: they wait while
  files move; while they run, a rollback, an attic restore or remove (`FilesInUse`) and a prune
  (`BackupRunning`) are refused, a slow export writer included.
- **Playground: "Download current state (.gsnap)" no longer drops embedding models silently**
  ([#23](https://github.com/graphersal/graphersal/issues/23)). It asks, with the models' sizes;
  `GET /api/export?format=gsnap` carries them unless `&models=without`.
- **Saving a graph no longer loses its embedding models.** `g.export_snapshot`,
  `AnyGraph::write_snapshot`, Python `graph.save` and the dev server's "Save to file" carry the
  models the graph serves; before, "Save to file" over a `--graph x.gsnap` destroyed them in the
  file. These writes replace the file only when finished, so the path may be the file the graph was
  loaded from. New: `AnyGraph::write_snapshot_with`, `persist::write_graph_snapshot`,
  `SnapshotInfo::blobs_not_carried`.
- **A snapshot that reuses segment files of an older snapshot downloads with its models.** A
  checkpoint references the unchanged segment files of an earlier snapshot instead of copying
  them. Exporting such a snapshot WITH the store's embedding models failed with `I/O error while
  reading a snapshot segment: No such file or directory`, while the same snapshot exported
  without models, and snapshots that hold all their files, worked. This hit the Store menu's
  download (the dev server answered `404`), `graphersal store export` without
  `--without-models`, Python `Store.export_packed` and `Store::export_packed(_with)` in Rust.
  Nothing in the store was wrong, and no broken `.gsnap` was ever written: the export stopped
  before its first byte. Backups (directory, increment, ZIP), forks and repairs were not
  affected. The dev server now answers a failed download with `404` only when the snapshot does
  not exist, `422` when the store refuses it (damage, a missing or damaged model) and `500` when
  a file cannot be read.
- **`addE().from(v)` / `.to(v)` take a vertex** (TinkerPop's `from(Vertex)`/`to(Vertex)`). In Rhai
  and Python a vertex value (a `next()` result, a Python `Vertex` parameter, an edge's
  `out_v`/`in_v`) failed with "expected vertex or vertex id, got null"; in Rust a vertex was read as
  a name, so a step label or side-effect key equal to its id won, and an edge value connected the
  vertex of the edge's id. Now a vertex is that vertex by its id; an edge value is an error.
  Rust: `from`/`to` take `impl Into<EndpointArgs>` instead of `impl Into<ArgStrings>` (neither
  type is nameable outside the crate; every argument type that compiled still does, plus
  `VertexValue`, `&VertexRef` and `Vec<ElementProperty>`).
- **An upgraded store whose newest WAL segment holds only a checkpoint record opens and takes
  commits again.** A checkpoint taken while a store is open starts a new WAL segment named by
  the next commit and writes its checkpoint record there (a mark set afterwards adds its record
  too). After `graphersal store upgrade` of a store of format version 3 (Graphersal 0.2.x to
  0.4.x) that segment is of the older file format, and the first write after the upgrade
  refused the whole store as damaged: `Corrupt …/wal/<N>.wal at byte 64: it starts at commit N,
  which is not committed yet`. This hit:
  - opening the upgraded store;
  - a commit to the closed store, such as `graphersal embed-model import <repo> --into <store>`,
    which an upgraded store needs when its schema names a model it does not keep yet (0.4.x
    served such models through `--embedder`);
  - the upgrade of a 0.4.x store with models under `models/`: its migration commit failed, and
    an in-place upgrade left the store half converted (`GRAPH` already in the new version, the
    `INTENT` and `models/` still there), so neither an open nor a second upgrade worked.

  Nothing was lost: `store verify` found no damage, and `store repair` built a complete copy. Now
  such a segment is rewritten in the current format with its checkpoint and mark records kept,
  and the commit follows them. A store that is already stuck needs no manual step: with this
  release the open, the import or the repeated `store upgrade --in-place` completes. A store
  repaired with 0.5.x as a workaround is a complete store as well; only its history before the
  repaired commit and its marks stayed in the original
  ([Store Format Versions](../persistence/store_versions.md),
  [format spec 15.1](../persistence/format.md)).
- **`store repair` says what a repair never carries.** A repair writes the readable state as one
  snapshot of a new store; the commits before it (rollback, views of past states, the attic), the
  marks and the store id stay in the store it was built from. The report used to say `nothing
  lost` right next to `mark not carried`. It now reads `all data kept` and names the rest in one
  line, also when no data is lost:

  ```text
  Repaired store repaired/ at commit 3 (graph …): all data kept
    mark not carried: before-import @ commit 1
    not carried: the history before commit 3, 1 mark and the store id (backups start anew); the store it was built from keeps them

  ok: all data kept
    not carried: the history before commit 3, 1 mark and the store id (backups start anew)
    kept in: damaged/ (keep it while you need them)
  ```

  The damage report of `store verify` and of a store in maintenance mode says `a repair keeps all
  data` instead of `a repair loses nothing`, and so does the playground's Store menu. In Rust
  `RepairReport::not_carried()` returns the same words, in Python the report of
  `Store.repair_to()` has them under the new key `"not_carried"`. The exit codes do not change
  ([Damage, Maintenance and Repair](../persistence/store_damage.md)).
- `graphersal embed-model import` printed a library error with a doubled headline (`Error:
  Error: …`); it now has one `Error:` like every other command.
- **Playground: closing the Store menu with its button no longer reloads it**
  ([#25](https://github.com/graphersal/graphersal/issues/25)).
