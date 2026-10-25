# Release 0.3.0

A minor release. The engine learns what its graph holds and how it is used, and acts on it:
**workload statistics**, **scheduled jobs**, a **memory report** and a **storage advisor** with an
**Optimize** panel in the playground. Dates and times reach nanosecond resolution, the in-memory
graph takes markedly less memory, and the DSL spells Graphersal's own extensions in snake_case
only.

It is not a drop-in update for every caller: read "Upgrading from 0.2" first.

## Upgrading from 0.2

- **Graphersal's own DSL functions are snake_case only.** The camelCase twins of 63 extensions are
  removed (`createIndex`, `defineQuery`, `memoryReport`, `removeProperty`, `globPath`, `addLabel`,
  `setLabel`, `dropLabel`, `toJson`, `toTable`, `toMermaid`, `toPlantUML`, `firstName`, `dateTrunc`,
  `localDateTime`, ...). TinkerPop's steps and functions keep both spellings (`hasLabel` and
  `has_label`), as do five names of the Java API (`containsKey`, `isEmpty`, `toMap`, `Set.ofList`,
  `UUID.fromString`). A script that still calls a removed name fails with a help that names the
  snake_case function ([Writing Queries](../dsl/writing_queries.md)). Rendering in camelCase
  (`render.spelling`) keeps these names in snake_case, so the text still runs.
- **File format version 3.** Every file this version writes (snapshots, journal segments, `.gsnap`,
  backups) is of format version 3, which holds the new temporal values. 0.3.0 reads every older
  file; 0.2.x refuses a version 3 file with a clear "unsupported version" error (never as damage).
  Keep a backup made by 0.2.x if you may need to go back. The store format version is unchanged
  ([Format Versions and Upgrade](../persistence/store_versions.md)).
- **Temporal values hold nanoseconds** (were microseconds); see "Dates and times" below.
- **Rust API:** the `DT` token gained `DT::Nanosecond` as its first unit, so the numeric values of
  its variants moved (only code casting a `DT` with `as` sees it; `cargo-semver-checks` reports
  nothing else). Every other change is an addition: new modules `advisor`, `jobs` and `workload`,
  new methods (`Graph::advise`, `Graph::attach_workload_stats`, `memory_report`, ...) and
  `#[non_exhaustive]` structs.

## Added

- **Memory report** ([Memory and Profiling](../users_guide/memory_and_profiling.md)):
  `g.memory_report()` (REPL `/memory`, MCP `memory_report`, dev server `POST /api/memory/report`,
  Python `memory_report()`): one row per label and property with the bytes it takes, value types,
  a size histogram, distinct strings, what is compressed or indexed, and the structure's own share
  (slots, ids, labels, shapes, indexes). Sampled on large labels.
- **Workload statistics** ([Workload Statistics](../persistence/workload_statistics.md)): a store
  counts what its queries do, per minute, hour and day (kept for 60 minutes, 48 hours and 90 days,
  persisted in the sidecar file `WORKLOAD`): queries by normalized plan (a `pg_stat_statements`
  analogue), how each property is used (filtered by a scan or an index, read, ordered, compared by
  similarity, written), the graph lock's wait and hold times, and the scheduled jobs; per channel
  (playground, MCP, CLI, Python, job, host). The playground's own drawing and panels are not
  counted. Read through `GET /api/workload[?history=minutes|hours|days]`, the MCP tool
  `workload_statistics`, REPL `/workload`, Python `workload_report()`; their memory shows as
  `workload_bytes` in `g.memory_usage()`.
- **Scheduled jobs** ([Scheduled Jobs](../persistence/scheduled_jobs.md)): a cron scheduler the
  host ticks (no thread in the engine; the dev server and the browser playground tick it). System
  jobs (`stats_flush`, `compact`) whose schedule and switch can be changed, and **jobs of users**
  as catalog definitions (kind `job`): `g.define_job(#{name, schedule, action, ...})` runs a saved
  query with fixed arguments, a checkpoint or a backup, with the definer's rights checked once and
  stored as the job's effects; limits per run (timeout, memory); every run recorded. Managed in
  the playground's **Catalog ▸ Scheduled jobs**, over `/api/jobs*`, MCP `list_jobs`.
- **Storage advisor and the Optimize panel** ([Optimize](../persistence/optimize.md)):
  recommendations from the memory report, the workload statistics and the catalog: create an index
  for a key filtered often by a full scan, drop an index no query used, compress or deduplicate
  large strings (savings measured on the data), stop compressing a key read often. Each names the
  catalog command that applies it; **Apply** runs it, **Snooze** and **Dismiss** hide it, and an
  applied one reports what it changed for 30 days. The playground's **Optimize** panel also shows
  the workload statistics as tables with sparklines and charts, and the memory report. Over HTTP
  `/api/advise*`, MCP `advise`, Rust `Graph::advise` (module `graphersal::advisor`).
- **Process memory** in the statistics: `process.memory` (resident set size where the system
  reports it, the live heap, WebAssembly memory), loaded models and the AI Chat history, beside the
  graph's own footprint (playground Profile ▸ Statistics).
- **Dates and times** ([Dates and Times](../dates/index.md)): nanosecond resolution for `time`,
  `localdatetime`, `datetime` and `duration`; `offsettime`; a zone name given to a `datetime` is
  kept; the whole `java.time.Duration` range; ISO GQL temporal literals and clocks; Python returns
  nanosecond values as `pandas.Timestamp` / `pandas.Timedelta` where pandas is installed.
- **Playground**: a filter for the drawn graph (labels and property conditions), a compact Graph
  toolbar with a Display menu (vertex size by degree), in/out degree in the inspector, reloads
  throttled while the graph changes fast, shape bytes on the Statistics tab, four `subgraph()`
  examples; the Share link is hidden with the dev server (it cannot carry a server's graph).
- **OpenAPI description of the dev server's HTTP API** (EXPERIMENTAL,
  [The HTTP API](../persistence/dev_server.md#the-http-api-experimental)):
  `crates/graphersal-cli/openapi.json` in the repository, served by `graphersal --server` at
  `GET /openapi.json`, its version the release's; a test keeps it in step with the router.
- **CLI and dev server**: `--args-file FILE` / `@FILE` (a JSON file of arguments),
  `--mcp-instructions FILE` (operator notes in the MCP instructions); MCP answers are compact JSON,
  cut at whole results, with `offset` paging; the AI Chat accepts a custom `system_prompt`, reports
  lenient keys with hints, and shows each answer's time.

## Changed

- **Less memory for the same graph.** Stored property maps share their key lists (property
  shapes), the id index keeps no second copy of an id, vertex and edge slots are an own topology
  (petgraph is no longer a dependency) with interned label sets, and a load leaves no spare
  capacity. On the project's own knowledge graph (40k vertices, 190k edges) the graph took about
  half the memory of 0.2.3.
- **GQL `MERGE` without an index** finds its match through a scan lookup, about ten times faster.
- The MCP instructions and tool descriptions prefer GQL for reading and writing data.

## Format

- File format version 3 (format spec, section 3): new value tags for the nanosecond temporal
  values; versions 1 and 2 stay readable.
- The workload statistics sidecar `WORKLOAD` (format spec, section 4.4, version 2): instance data,
  never in the journal, a snapshot or a backup; an unreadable or newer file means the statistics
  start anew. It also keeps the system jobs' settings and the advisor's marks.
- Catalog kind 4: a scheduled job of a user (format spec, section 6.3).
