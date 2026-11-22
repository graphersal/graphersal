# Release 0.7.0

A minor release. Its main feature is **layers** (EXPERIMENTAL). A layer is a writable graph
over a frozen base graph and stores only its own difference from that base. Several layers can
share one base in a process. To a query a layer looks like any other graph. Three new functions
say where an element comes from: `layer()`, `has_layer()` and `g.layer_diff()`. A layer can be
saved as a layer snapshot (file format version 5) or kept durably in a layer store. From the
command line it runs with `--layer` and `--layer-store`, and the playground marks what a layer
added or modified.

The servers get the pieces a host needs to serve many graphs from one process. `graphersal-serve`
can launch any source into a session, mount it under a URL prefix and switch it to read-only at
run time. `graphersal-mcp` gets a directory mode: one endpoint over many graphs. The playground
gets a server mode, and it now recognizes a Graphersal host by a protocol marker. Stores get
`Store::convert_with`, which fills missing blobs while converting, and hosts can declare their
own system journal event kinds ([#35](https://github.com/graphersal/graphersal/issues/35)).

The public API of `graphersal` is unchanged apart from additions (`cargo semver-checks` reports
no break against 0.6.2). The breaking changes are in the EXPERIMENTAL crate `graphersal-serve`
and in how the playground recognizes a host: read "Upgrading from 0.6.x" first. Plain stores,
snapshots, journals and backups keep their format. Only layer snapshots and layer stores are
new, and older releases refuse them.

## Upgrading from 0.6.x

**Stores and files.** Stores, snapshots, journals and backups of 0.6.x open unchanged, with no
upgrade step. Everything 0.7.0 writes for a plain graph is still file format version 4, so
0.6.x keeps reading it. As always, every release reads the formats of every release before it.
The one new format is for layers:

- A layer snapshot (a `.gsnap` of a layer, a snapshot inside a layer store) carries **file
  format version 5** in its packed header, its manifest and its layer file. Graphersal 0.6.x
  and older refuse it as written by a newer version. They never load the layer's own elements
  as if they were a whole graph. The same holds for a layer store and its backups. To hand a
  layer's graph to an older release, export it whole: `graphersal store materialize <layer
  store> whole.gsnap --base <base>` (or `Store::export_materialized`) writes a plain format-4
  snapshot that needs no base ([format spec 7.5](../persistence/format.md)).

**`graphersal-serve` (EXPERIMENTAL, breaking).** These changes affect hosts built on the crate.
The `graphersal --server` dev server behaves as before.

- `graphersal_serve::SERVER_ID` is renamed to **`DEFAULT_SERVER_NAME`** (same value,
  `"graphersal-dev"`). Rename it where you use it. There is no alias.
- `GET /api/info` answers **`"dev": false` by default**. Before, it was always `true`. A host
  that should show the playground's DEV SERVER badge calls `ContextBuilder::dev(true)`, as the
  CLI's dev server does. `ContextBuilder::server_name` sets the `server` field, which is now
  only the host's name.
- `GET /api/info` carries **`"protocol": "graphersal/1"`** (`graphersal_serve::PROTOCOL`). A
  host built on `graphersal-serve` sends it automatically.

**Playground (breaking for hosts that answer `/api/info` themselves).** The playground uses a
server as its engine only when that server's `/api/info` answers `"protocol": "graphersal/1"`.
It no longer compares `server` with `"graphersal-dev"`. A host that answers `/api/info` without
`graphersal-serve` must add the field, or the playground falls back to its in-browser engine.
The number changes only when the HTTP API breaks (an endpoint or field removed or renamed, or a
meaning changed). Additions keep it.

**Behaviour.**

- `.profile()` no longer shows `[index: ..]` or `[count from index]` when the storage declined
  the lookup and the source scanned instead. It shows `, declined, scanned` inside the index
  brackets, or `[count from index declined]`. A `TraversalGraph` never declines. A host storage
  or a layer can.
- The dev server with `--read-only` disables the playground's editing (edit mode, the
  inspector's Edit and Delete, the element editor). Before, the playground offered edits and
  they then failed.

**`graphersal-session` (internal, breaking).** The public structs `Node`, `Edge` and
`CompactGraph` of its display model gain the fields `origin`, `vertex_origin` and `edge_origin`
(`None` on a graph without layers). Code that builds them with a struct literal adds the new
fields.

**For `GraphStorage` implementers (EXPERIMENTAL trait).** The new trait methods
`vertex_layer`, `edge_layer`, `layer_diff` and `layer_summary` have defaults: every element is
`"base"` and the diff is empty. A wrapper storage that forwards to another storage should
forward them too, so that a layer under it still reports its origins. The same goes for the new
`PersistentStorage` methods `load_base_patch` and `layer_parts`. `MemoryStats` gains
`shared_bytes`.

## Layers (EXPERIMENTAL)

A [layer](../users_guide/layered_graphs.md) is the storage `LayeredGraph` over a frozen
`Arc<TraversalGraph>`. It stores only what it adds, and the base elements it modifies or
removes. Every step, saved query, schema rule, GQL query and transaction runs on it as on a
plain graph. The typical use is a product graph shared by many projects: each project gets a
layer, and that layer costs about what the project adds or changes, not a copy of the product.
How the storage works, its capabilities and its memory figures are on the developer page
[Layered Graphs](../for_developers/layered_graphs.md).

- **Where an element comes from.** These are new DSL functions, Graphersal extensions in snake
  case only:
  - [`layer()`](../traversal/steps/layer.md) answers `"base"`, `"added"` or `"modified"`.
  - [`has_layer(origins...)`](../traversal/steps/has_layer.md) keeps the elements with one of
    the named origins. An unknown origin is the new `TraverserError::UnknownLayerOrigin`,
    raised before the query runs.
  - [`g.layer_diff(#{kinds, labels, limit})`](../traversal/steps/layer_diff.md) answers the
    whole difference against the base as one map: `added`, `modified` (each changed property
    as `base`/`layer`), `removed` and `truncated`.

  On a graph without layers, everything is `"base"` and the diff is empty, so one query runs on
  both kinds of graph. Rust: `GraphTraversalSource::layer_diff(&LayerDiffRequest)`, and
  `LayerOrigin`, `LayerDiff`, `LayerSummary` in `graphersal::storage`. Python:
  `Graph.layer_diff(kinds=, labels=, limit=)`.
- **Writes.** A layer can add elements and edges that touch base vertices. It can write to
  base elements through copy-up images and drop base elements, which leave tombstones. It has
  its own schema and catalog (copied from the base when the layer is created), and its own
  units, savepoints, rollback, change capture, commit hooks and dry runs.
- **Property indexes and vector rules.** A layer serves the base's property indexes for its
  own data, lookups and counts included, and enforces the base's unique indexes over base plus
  delta. It can create, drop and redefine indexes for itself. Vector rules keep the derived
  vectors in step, using the embedding models of the base (`LayerBase`). Compression rule and
  model definitions on a layer are refused for now.
- **Memory.** `g.memory_usage()` on a layer reports its own footprint as `total_bytes` and
  the shared base beside it as `shared_bytes`. `g.memory_report(#{scope: "delta"})`
  (`MemoryReportScope::Delta`) walks only the layer's added and modified elements. The REPL
  equivalent is `/memory delta`. On the `large` graph with 1 600 changed elements, a layer's own
  total was 475 KB beside a shared base of 51 MB.
- **Layer snapshots and layer stores.** `persist::read_layer_snapshot` reads a layer snapshot
  back over its base. `Store::<LayeredGraph>::create_layer` and `Store::open_layer` keep a
  layer durable and recoverable on every backend. The base must be the same one (`BaseRef`:
  lineage, commit and time). Another base is refused with `PersistError::LayerBaseMismatch`.
  A plain open of a layer store is refused with `StoreFailure::LayerStore`. A layer store's
  base is a creation parameter, and every copy path keeps it: backups, restores, verify,
  views, checkpoints, fork, attic, rollback, convert, upgrade. A backup of a layer store is
  useless without its base, and `graphersal store backup` warns about this. A damaged layer
  store is not repaired; restore a backup of it instead. `Store::export_materialized` writes
  the whole graph the layer shows as a plain snapshot. A base loads from a read-only Store or
  a `.gsnap` with its embedding models: `LayerBase::from_store_read_only` and
  `LayerBase::from_snapshot_file`.
- **CLI.** `graphersal --graph <base> --layer` runs the one-shot runner, the REPL or the dev
  server on an empty layer kept in memory. `--layer-store <dir>` runs it on a layer store
  instead (`--create-store` creates the store). The base is a Store opened read-only without
  its lock, or a `.gsnap`. The `graphersal store` commands that load a layer take
  `--base <SOURCE>` (`verify`, `checkpoint`, `fork`, `rollback`, `attic restore|remove|fork`).
  The new `graphersal store materialize` writes the whole graph as a standalone `.gsnap`, and
  `store info` names the base ([Layers](../users_guide/layered_graphs.md#keeping-a-layer-durably---layer-store)).
- **Dev server, MCP and playground.** `/api/info` describes the layer, and
  `POST /api/layer/diff` answers the diff. MCP `graph_info` shows a `layer` block, and the
  `initialize` instructions name the three functions (on a layer only). The playground shows
  a layer badge with the change counts, draws added and modified elements with their own
  outlines in colour-blind-safe colours, and adds a Layer changes dialog and a shared-base
  track in the memory bar. The browser playground can start a layer over a loaded graph
  (**Store ▸ Start a layer over this graph**) and drop it again. The UI profile key
  `layerDiff` turns these on or off.
- **Serving many layers.** MCP session route tags (`Host::session_route`,
  `ContextBuilder::mcp_session_route`) pin an MCP session to the graph resolved at
  `initialize`. The book page [Serving Layers](../for_developers/serving_layers.md) describes
  how a host serves many customers' layers over shared product versions behind one endpoint.

## Hosting pieces for multi-graph servers (EXPERIMENTAL)

These additions to `graphersal-serve` and `graphersal-mcp` let one process serve many graphs.
Their single-graph behaviour is unchanged, as is the dev server's output.

- **Launch a source.** `graphersal_serve::launch` launches a `ServedSource` (a sample, a file,
  a Store, a layer store or a host graph) into a session and returns a pre-filled
  `ContextBuilder`. `open_store`, `open_layer_store` and `layer_base_of_store` return their
  open notes as data (`OpenNote`). New error: `ServeError::Open`. The CLI's dev server
  launches through it.
- **Read-only at run time.** With `ContextBuilder::runtime_read_only`, a host can switch a
  served graph read-only and back without reopening it: `Context::set_read_only`,
  `Context::is_read_only`. Writes from the HTTP API, MCP and jobs are refused, and scheduled
  jobs pause. `/api/changes` gains `readOnly`, and `/api/info`'s `readOnly` follows the switch.
- **Mount under a URL prefix.** `ContextBuilder::mount_path` adds `mountPath` to `/api/info`
  and prefixes the `openapi` and MCP endpoint paths. `HostCheck::with_hint` puts the host's
  own advice into its 403 answers. `Request::copy_body_to` streams an upload into a host sink.
- **MCP directory mode.** `Mcp::handle_directory` serves one endpoint over a host's
  `GraphDirectory`: the tools `list_graphs` and `use_graph`, plus an optional `graph` argument
  on every data tool. `Mcp::with_max_sessions` sizes the session table, and
  `Mcp::with_endpoint` sets the status path.
- **Protocol marker.** Every `/api/info` answers `protocol: "graphersal/1"`. The
  `ContextBuilder::server_name` and `ContextBuilder::dev` settings are described under
  "Upgrading from 0.6.x".

## Playground

- **Server mode.** Under a mount (`/db/<name>/`) the playground keeps its tabs, history and
  layout separately for each mount. Keys at the root are unchanged, and the theme is shared.
  The header shows the database name, linked to the host's `home` from `/api/info`. A
  read-only badge follows `/api/changes` live, and the MCP endpoint comes from `/api/info`.
- **UI profiles** (EXPERIMENTAL): any host can now deliver a profile, not only the dev server.
  The new optional key `styles` lists the profile's own style sheets, and a profile's modules
  read the host's raw `/api/info` as `api.info.host`
  ([UI profiles](../users_guide/web_playground.md#ui-profiles)).

## Stores and the system journal

- **Convert a store and fill its missing blobs.** `Store::convert_with(from, to,
  ConvertOptions)` with `ConvertOptions::with_fill_missing_blobs(BlobSource)` fills every blob
  the copy references but the store lacks, or holds damaged. The blob source can be a shared
  models directory, another store, a backup, a ZIP backup or a `.gsnap`. Each filled blob is
  checked by length, CRC-32C and hash, and only the copy changes. A blob that is still missing
  is `StoreFailure::BlobUnavailable`; one with other bytes is `BlobDamaged`. Either way the copy
  is undone. `ConvertReport` lists the filled blobs. CLI: `graphersal store convert
  --fill-blobs <SOURCE>`. `Store::convert` is unchanged
  ([Blobs](../persistence/store_blobs.md)).
- **Host events in the system journal**
  ([#35](https://github.com/graphersal/graphersal/issues/35)). A host built on the engine
  declares its own event kinds with `system_journal::declare(HostEventKind)`, under its own
  first code segment, and records them with `SystemJournal::record(HostEvent)`. They go
  through the same collapsing, sinks and listings as the engine's events.
  `system_journal::declared_kinds()` lists the declared kinds. New errors:
  `JournalError::InvalidDeclaration`, `Undeclared` and `InvalidEvent`. The record layout is
  unchanged ([Host events](../persistence/system_journal.md#host-events)).

## Fixed

- `.profile()` showed an index lookup or a count from an index when the storage had declined
  it at run time and the source had scanned. It now shows that the lookup was declined
  (see "Behaviour" above).
