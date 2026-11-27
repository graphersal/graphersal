# Release 0.9.0

A minor release for hosts. A server can now download embedding models from Hugging Face through
the library (feature `model-download`) instead of shelling out to the command line, refuse a bad
download request before it starts a job, and match a store's waiting model to a fresh download
by its blob. A layer can be reset to its base in one commit (in Rust, in the session, on the dev
server and in the playground's "Discard all changes…"). A served context changes its query
limits without a reload, and the runs of the hosting core belong to the caller's identity, not
only to the page id the client chose.

The public API is unchanged apart from additions: `cargo semver-checks` against 0.8.0 reports
no break of `graphersal`, `graphersal-session`, `graphersal-serve`, `graphersal-mcp` and
`graphersal-storage-tests`. Two changes of behaviour need a look before upgrading: an identical
re-create of a removed base element in a layer now restores it, and the EXPERIMENTAL
`graphersal-serve` keys runs by the caller's identity (read "Upgrading from 0.8.x"). No format changes; stores, snapshots, journals and backups of
0.8.x are read and written exactly as before. The `graphersal --server` dev server answers as
before, plus the new `POST /api/layer/reset`.

## Upgrading from 0.8.x

**Stores and files.** A drop-in replacement: no format change, no upgrade step. 0.8.x reads
everything 0.9.0 writes.

**Layers: an identical re-create restores the base element.** A vertex created in a layer with
the id and exactly the state of a base vertex the layer removed (the same labels in the same
order, the same properties in the same order) is now that base vertex again, with its base
handle and in its base position, and `g.layer_diff()` no longer lists it; the same holds for an
edge with the id, endpoints, label and properties of a removed base edge. Before, it was a new
element of the layer, listed as removed plus added. A create that differs in anything stays a
new element. The rule holds for live writes and for replay alike, so a layer store reopened
from its journal and one reopened from a snapshot show the same diff
([Discarding a layer's changes](../users_guide/layered_graphs.md#discarding-a-layers-changes)).

**`graphersal-serve` (EXPERIMENTAL): runs are keyed by identity.** When the host's request filter
attaches an identity (`Request::set_identity`), the key of a run, of the Stop of a page
(`POST /api/cancel`), of MCP request cancellation and the fairness key of a host's `JobQueue` is
`<byte length>:<identity>/` before the plain key, for example `5:alice/page:tab`. That key is
what `RunInfo::client` and the run administration's `client` now show. A caller with another
identity, or an anonymous one, that sends the same page id cancels nothing. Without an identity
the key is the plain one, as before (the CLI dev server attaches none). A host that parsed
`RunInfo::client` strips the prefix; the identity is in its own field
([Runs belong to their identity](../for_developers/serving_layers.md#runs-belong-to-their-identity)).

**`graphersal-serve`: the change feed masks other identities.** `GET /api/changes` names who made
a change (`recent[].by`, `detail`) only to a request of the same identity; a change made under
another identity, anonymous included (also the job ticker's), reads `by: "another identity"`
without `detail`. Without identities every change is shown as before.

**Additions only.** The new field `ModelInfo::blob_hash` is additive (`ModelInfo` is
`#[non_exhaustive]`), as are the new types (`LayerReset`, `ModelDownloadReport`,
`ModelDownloadError`, `DownloadEvent`, ...), and `SessionPresets` gains `PartialEq`. No code
change is needed to build against 0.9.0.

## Embedding models for hosts

- **Hugging Face download as a library feature.** The download `graphersal embed-model
  download` runs is now `persist::ModelCache::download` behind the new `graphersal` feature
  `model-download` (native only; it implies `persist` and `model2vec` and adds the HTTPS client
  `ureq` with `ring`). `ModelDownload` is the request (repository, revision, endpoint, token,
  `CancelToken`), the callback gets `DownloadEvent`s (start, progress per file, file done,
  notes), the answer is a `ModelDownloadReport` (identity, dimension, commit, licence, base
  models, gated flag, the files with their sizes and SHA-256, notes, licence warnings), and a
  failure is a `ModelDownloadError` (`InvalidRequest`, `Refused`, `NotFound`, `Transfer`,
  `Integrity`, `NotAModel`, `Io`, `Cancelled`). Any failure, cancellation or kill leaves no
  model. `ModelCache::download_staged` stops before the move, so `StagedModel::new_model()`
  imports the download straight into a store; `persist::model_licence_warnings` gives the
  licence warnings for a model from elsewhere. The library reads no environment variable: the
  command line passes `$GRAPHERSAL_HF_ENDPOINT` and `$HF_TOKEN`, and its output is unchanged
  ([Downloading from a host](../persistence/store_models.md#downloading-from-a-host)).
- **Validate before the job.** `ModelDownload::validate()` runs the download's first check on
  its own (the repository name, `ModelDownload::is_repo_name`, and the revision) and returns the
  same `InvalidRequest`, so a host refuses a bad request before it queues a download.
- **The same commit gives the same blob.** A model's blob holds its files and their stable
  description, never the download time, so two downloads of the same repository and revision at
  the same commit give the same blob whenever and however they are imported, and a store that
  waits for that blob (missing, or exported without it) is served by a fresh download (now
  covered by tests).
- **A model names its blob.** `ModelInfo::blob_hash` is the lowercase hex SHA-256 of the
  model's blob, which is its file name `blobs/<first 2 hex>/<hash>`; it is given in every state,
  also for a missing or damaged blob, so a problem `Store::verify` reports on a blob names its
  model. `graphersal store models` (and the model sections of the copy reports) show it as the
  last row, `blob`, and the session's model JSON as `blobHash`
  ([Listing](../persistence/store_models.md#listing)).

## Layers

- **Reset a layer.** `LayeredGraph::reset_layer()` and `Graph<LayeredGraph>::reset_layer()`
  (also `store.graph().reset_layer()` on a layer store) discard everything the layer changed in
  one commit: added elements are dropped, changed base elements read as the base again, removed
  base elements come back with their ids, labels, properties and edges, and the layer's own
  schema and catalog changes are undone. The base is never written. The answer, `LayerReset`
  (in `storage`), counts what was added, modified and removed and whether the schema and
  definitions changed. It is an ordinary unit: a dry run, a failing transaction or a vetoing
  hook leaves the layer as it was, hooks get its change set, a layer store journals it (a reopen
  gives the reset layer, `store rollback` can go back before it), and the id sequences go on (an
  element created afterwards never reuses an id). The session has `Session::reset_layer()`, the
  WebAssembly engine `resetLayer`, the dev server `POST /api/layer/reset` (a write; refused on a
  read-only server), and the playground's Layer changes dialog **Discard all changes…** with a
  confirmation
  ([Discarding a layer's changes](../users_guide/layered_graphs.md#discarding-a-layers-changes)).

## Hosting pieces (EXPERIMENTAL)

- **Query limits at run time.** `Context::set_presets(SessionPresets)` changes the presets of a
  served context without reloading the graph: every run that starts afterwards (HTTP API, MCP
  tool calls also of sessions opened before, scheduled queries handed out later, every worker of
  a host's `JobQueue`) runs under them; a run already running keeps its own. `Context::presets()`,
  `GET /api/info` `presets` and the statistics answer the current presets. Beneath it,
  `graphersal-session` has `Session::set_presets`, which rebuilds the engine and keeps the graph,
  stores, jobs and runs, and `SessionPresets` now derives `PartialEq`
  ([Query limits at run time](../for_developers/serving_layers.md#query-limits-at-run-time)).
- **Runs belong to their identity** (above, under "Upgrading").

## Fixes

- The dev server's OpenAPI description (`crates/graphersal-serve/openapi.json`) documents the
  `409` answers of a read-only request and the `403` answers of the file root added in 0.8.0.
- `cargo test -p graphersal-cli --no-default-features` builds again (a unit test used a field
  that exists only with the `dev-server` feature).
