//! `graphersal-wasm`: the Graphersal engine compiled to WebAssembly for the browser playground.
//!
//! The JavaScript API ([`Playground`]) is **internal to the playground**: it is shaped by
//! the playground's engine boundary (`playground/js/engine/engine.js`) and runs inside a module
//! Web Worker (`playground/js/engine/worker.js`). Publishing a designed, semver-stable JS API on
//! npm is planned. Every method takes and returns plain strings (JSON text for
//! structured data), so the worker passes them on without conversion glue.
//!
//! Build: `playground/build.sh` (wasm-bindgen CLI, target `web`, output `playground/pkg/`).

#![deny(missing_docs)]

use graphersal_session::views;
use serde_json::Value;
use wasm_bindgen::prelude::*;

pub use graphersal_session::{ForkInto, RunOptions, Session, StoreError};

// Exact per-step memory figures for `profile_types: ["memory"]` (`"source": "tracked"`): the
// module installs the library's counting allocator as its own global allocator, as the CLI does
// in its `main.rs`. wasm32-unknown-unknown is single-threaded, so the allocator's thread-local
// counters are simply the module's counters and a query never hops threads. The wrapped
// allocator is the target's default (`System`, dlmalloc); the cost is one counter update per
// allocation. Only on wasm32: the native unit tests of this crate keep the platform allocator.
#[cfg(target_arch = "wasm32")]
#[global_allocator]
static ALLOC: graphersal::alloc_tracking::TrackingAllocator =
    graphersal::alloc_tracking::TrackingAllocator::new();

fn to_text(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".to_string())
}

fn store_text(result: Result<Value, StoreError>) -> Result<String, JsError> {
    result
        .map(|v| to_text(&v))
        .map_err(|e| JsError::new(&e.message))
}

fn body_json(text: &str) -> Result<Value, JsError> {
    serde_json::from_str(text).map_err(|e| JsError::new(&format!("malformed JSON body: {e}")))
}

/// One playground session: a graph and the engine that runs scripts against it. All structured
/// results are JSON text.
#[wasm_bindgen]
pub struct Playground {
    session: Session,
}

impl Default for Playground {
    fn default() -> Self {
        Self::new()
    }
}

#[wasm_bindgen]
impl Playground {
    /// A session over an empty graph. Every graph it loads is kept as a Store in memory
    /// (commit 0 = the loaded state): the Store menu's operations below work on it, and all of
    /// it is lost with the page.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Playground {
        let mut session = Session::new();
        session.set_memory_stores(true);
        Playground { session }
    }

    /// The engine version (the workspace version).
    pub fn version() -> String {
        env!("CARGO_PKG_VERSION").to_string()
    }

    /// Whether `.profile_with(ProfileType::Memory)` reports exact (`"tracked"`) figures.
    #[wasm_bindgen(js_name = memoryTracked)]
    pub fn memory_tracked() -> bool {
        cfg!(target_arch = "wasm32")
    }

    /// `[{kind, title, description, available}]`: the sample graphs.
    #[wasm_bindgen(js_name = sampleGraphs)]
    pub fn sample_graphs() -> String {
        to_text(&Session::sample_graphs())
    }

    /// Replaces the graph by a sample (`empty`, `modern`, `large`, `tree`); returns the graph
    /// info JSON.
    #[wasm_bindgen(js_name = loadSample)]
    pub fn load_sample(&mut self, kind: &str) -> Result<String, JsError> {
        self.session
            .load_sample(kind)
            .map(|v| to_text(&v))
            .map_err(|e| JsError::new(&e))
    }

    /// Replaces the graph by one imported from GraphML text; returns the graph info JSON. The new
    /// graph starts WITHOUT the current graph's schema (GraphML carries none). `schema` is the
    /// optional schema file (JSON text): it is set on the new empty graph first and the data is
    /// imported against it as one unit, so declared logical types come back and `open`/`closed`
    /// are enforced. The current graph stays when the load fails.
    #[wasm_bindgen(js_name = loadGraphml)]
    pub fn load_graphml(
        &mut self,
        text: &str,
        name: &str,
        schema: Option<String>,
    ) -> Result<String, JsError> {
        self.load_graphml_bytes(text.as_bytes(), name, schema)
    }

    /// [`Self::load_graphml`] from the bytes of a file (UTF-8 GraphML).
    #[wasm_bindgen(js_name = loadGraphmlBytes)]
    pub fn load_graphml_bytes(
        &mut self,
        bytes: &[u8],
        name: &str,
        schema: Option<String>,
    ) -> Result<String, JsError> {
        self.session
            .load_graphml(bytes, name, schema.as_deref())
            .map(|v| to_text(&v))
            .map_err(|e| JsError::new(&e))
    }

    /// Replaces the graph by one imported from GraphSON text (line or wrapped form); returns the
    /// graph info JSON, whose `warnings` list what the import skipped (dates, unsupported types,
    /// dangling edges). Like [`Self::load_graphml`], the new graph starts without the current
    /// graph's schema and the optional `schema` (JSON text) is set first. The current graph
    /// stays when the load fails.
    #[wasm_bindgen(js_name = loadGraphson)]
    pub fn load_graphson(
        &mut self,
        text: &str,
        name: &str,
        schema: Option<String>,
    ) -> Result<String, JsError> {
        self.load_graphson_bytes(text.as_bytes(), name, schema)
    }

    /// [`Self::load_graphson`] from the bytes of a file (UTF-8 GraphSON).
    #[wasm_bindgen(js_name = loadGraphsonBytes)]
    pub fn load_graphson_bytes(
        &mut self,
        bytes: &[u8],
        name: &str,
        schema: Option<String>,
    ) -> Result<String, JsError> {
        self.session
            .load_graphson(bytes, name, schema.as_deref())
            .map(|v| to_text(&v))
            .map_err(|e| JsError::new(&e))
    }

    /// Replaces the graph by one read from a packed snapshot (`.gsnap`, the lossless binary
    /// format with its own schema); returns the graph info JSON. The current graph stays when
    /// the file is damaged.
    #[wasm_bindgen(js_name = loadSnapshotBytes)]
    pub fn load_snapshot_bytes(&mut self, bytes: &[u8], name: &str) -> Result<String, JsError> {
        self.session
            .load_snapshot(bytes, name)
            .map(|v| to_text(&v))
            .map_err(|e| JsError::new(&e))
    }

    /// "Save graph" as a packed snapshot: the bytes of a `.gsnap` file (data, schema, lineage
    /// id, commit position; lossless).
    #[wasm_bindgen(js_name = saveSnapshot)]
    pub fn save_snapshot(&self) -> Result<Vec<u8>, JsError> {
        self.session.save_snapshot().map_err(|e| JsError::new(&e))
    }

    /// `set_schema` ("Load schema"): replaces the graph's schema with JSON text in the schema
    /// format; `mode` (`"none"`, `"open"`, `"closed"`, optional) replaces the schema's own mode
    /// ("Freeze as open/closed": the inferred schema with a mode). Returns the graph info JSON.
    /// Fails (and changes nothing) on an invalid schema or mode, or when the existing data
    /// violates an `open`/`closed` schema.
    #[wasm_bindgen(js_name = setSchema)]
    pub fn set_schema(&mut self, json: &str, mode: Option<String>) -> Result<String, JsError> {
        self.session
            .set_schema(json, mode.as_deref())
            .map(|v| to_text(&v))
            .map_err(|e| JsError::new(&e))
    }

    /// `get_schema`: the stored schema as JSON text (the schema format, canonical key and
    /// property order), or `null`.
    #[wasm_bindgen(js_name = getSchema)]
    pub fn get_schema(&self) -> String {
        self.session.get_schema_text()
    }

    /// `patch_schema`: applies a JSON Merge Patch (JSON text) to the stored schema and stores
    /// the result; returns the new schema JSON. Fails (and changes nothing) like `setSchema`.
    #[wasm_bindgen(js_name = patchSchema)]
    pub fn patch_schema(&mut self, patch: &str) -> Result<String, JsError> {
        self.session
            .patch_schema_text(patch)
            .map_err(|e| JsError::new(&e))
    }

    /// `validate_schema`: `{violations, diff, compatible}` of a proposed schema (JSON text)
    /// against the data; nothing is applied.
    #[wasm_bindgen(js_name = validateSchema)]
    pub fn validate_schema(&self, json: &str) -> Result<String, JsError> {
        self.session
            .validate_schema(json)
            .map(|v| to_text(&v))
            .map_err(|e| JsError::new(&e))
    }

    /// `validate_schema_patch`: `validateSchema` of the stored schema patched with `patch`.
    #[wasm_bindgen(js_name = validateSchemaPatch)]
    pub fn validate_schema_patch(&self, patch: &str) -> Result<String, JsError> {
        self.session
            .validate_schema_patch(patch)
            .map(|v| to_text(&v))
            .map_err(|e| JsError::new(&e))
    }

    /// `infer_schema`: the schema the data satisfies, as JSON text; stores nothing.
    #[wasm_bindgen(js_name = inferSchema)]
    pub fn infer_schema(&self) -> String {
        self.session.infer_schema_text()
    }

    /// `diff_schema`: `{changes, compatible}` from the stored schema to a proposed one.
    #[wasm_bindgen(js_name = diffSchema)]
    pub fn diff_schema(&self, json: &str) -> Result<String, JsError> {
        self.session
            .diff_schema(json)
            .map(|v| to_text(&v))
            .map_err(|e| JsError::new(&e))
    }

    /// The loaded graph: `{name, kind, vertexCount, edgeCount, schemaMode, warnings}`.
    pub fn info(&self) -> String {
        to_text(&self.session.info())
    }

    /// Runs a script; `options` is the JSON of `RunOptions` (`{profile, profile_types,
    /// max_rows, max_items, no_limit, spelling}`, all optional). Returns the `ExecuteResponse` JSON
    /// (engine.js). A failing script is a response with `ok: false`, never an exception; only
    /// malformed `options` throw.
    pub fn execute(&mut self, script: &str, options: &str) -> Result<String, JsError> {
        let options: RunOptions = if options.trim().is_empty() {
            RunOptions::default()
        } else {
            serde_json::from_str(options).map_err(|e| JsError::new(&e.to_string()))?
        };
        Ok(self.session.execute(script, &options))
    }

    /// The schema views: `{mode, source, table, json, mermaid, graph: {nodes, edges}, truncated,
    /// total_labels}`.
    pub fn schema(&self) -> String {
        to_text(&self.session.graph().schema_view())
    }

    /// The schema views of the schema `json` (the editor's draft) with the graph's label
    /// counts: the same structure as [`Self::schema`], `source: "given"`.
    #[wasm_bindgen(js_name = schemaViewOf)]
    pub fn schema_view_of(&self, json: &str) -> Result<String, JsError> {
        self.session
            .schema_view_of(json)
            .map(|v| to_text(&v))
            .map_err(|e| JsError::new(&e))
    }

    /// The graph to draw as columns (`CompactGraph`: `{vertexIds, vertexLabel, vertexName,
    /// edgeIds, edgeSource, edgeTarget, edgeLabel, labels, truncated, totalNodes, totalEdges}`),
    /// without properties (see [`Self::element`]).
    #[wasm_bindgen(js_name = graphView)]
    pub fn graph_view(&self, max_nodes: usize, max_edges: usize) -> String {
        serde_json::to_string(&self.session.graph().graph_view(max_nodes, max_edges))
            .unwrap_or_else(|_| "null".to_string())
    }

    /// The graph to draw for a request (JSON `{maxNodes?, maxEdges?, degrees?, filter?}`,
    /// graphersal-session `views::GraphViewRequest`): [`Self::graph_view`]'s columns with the
    /// filter applied before the limits (`matchedNodes`/`matchedEdges`) and, with `degrees:
    /// true`, `vertexInDegree`/`vertexOutDegree`. An invalid filter throws its message.
    #[wasm_bindgen(js_name = graphViewWith)]
    pub fn graph_view_with(&self, request: &str) -> Result<String, JsError> {
        self.session
            .graph_view(request)
            .map(|v| to_text(&v))
            .map_err(|e| JsError::new(&e))
    }

    /// The Rhai query selecting what a Graph panel filter (JSON text) shows, as JSON text of a
    /// string (the "Open as query" link). An invalid filter throws its message.
    #[wasm_bindgen(js_name = graphFilterQuery)]
    pub fn graph_filter_query(&self, filter: &str) -> Result<String, JsError> {
        self.session
            .graph_filter_query(filter)
            .map(|text| to_text(&Value::String(text)))
            .map_err(|e| JsError::new(&e))
    }

    /// One element with its properties, for the inspector: `kind` `"vertex"` or `"edge"`.
    /// `null` when it does not exist (any more); an unknown kind throws.
    pub fn element(&self, kind: &str, id: &str) -> Result<String, JsError> {
        self.session.element(kind, id).map_err(|e| JsError::new(&e))
    }

    /// One element for the property editor, its properties in the typed form
    /// (`graphersal_session::edit`): `{kind, id, label, labels, source?, target?, properties:
    /// [[key, typed]], readOnly}`; `null` when it does not exist; an unknown kind throws.
    #[wasm_bindgen(js_name = elementForEdit)]
    pub fn element_for_edit(&self, kind: &str, id: &str) -> Result<String, JsError> {
        self.session
            .element_for_edit(kind, id)
            .map(|v| to_text(&v))
            .map_err(|e| JsError::new(&e))
    }

    /// The property editor's Save (or, with `validateOnly`, its check): `request` is JSON
    /// `{kind, id, set: [[key, typed]], remove: [key], validateOnly?}`, written as one unit;
    /// answers `{ok, changed, element}` or `{ok: false, errors: [{field, message, help?}]}`.
    #[wasm_bindgen(js_name = updateElement)]
    pub fn update_element(&mut self, request: &str) -> Result<String, JsError> {
        let request = body_json(request)?;
        Ok(to_text(&self.session.update_element(&request)))
    }

    /// The label choices of the Graph panel's creation dialog (`graphersal_session::create`):
    /// `request` is JSON `{kind: "vertex"}` or `{kind: "edge", from, to}`; answers `{kind, mode,
    /// free, labels: [{label, declared, count, connects}], readOnly}`. A bad request throws.
    #[wasm_bindgen(js_name = labelChoices)]
    pub fn label_choices(&self, request: &str) -> Result<String, JsError> {
        let request = body_json(request)?;
        self.session
            .label_choices(&request)
            .map(|v| to_text(&v))
            .map_err(|e| JsError::new(&e))
    }

    /// Creates a vertex (`{kind: "vertex", labels, id?, properties, validateOnly?}`) or an edge
    /// (`{kind: "edge", label, from, to, id?, properties, validateOnly?}`) as one unit, values in
    /// the typed form; answers `{ok, created, element, drawn}` or `{ok: false, errors}`.
    #[wasm_bindgen(js_name = createElement)]
    pub fn create_element(&mut self, request: &str) -> Result<String, JsError> {
        let request = body_json(request)?;
        Ok(to_text(&self.session.create_element(&request)))
    }

    /// Deletes a vertex (with its edges) or an edge as one unit: `{kind, id, validateOnly?}`;
    /// answers `{ok, deleted, edges}` or `{ok: false, errors}`.
    #[wasm_bindgen(js_name = deleteElement)]
    pub fn delete_element(&mut self, request: &str) -> Result<String, JsError> {
        let request = body_json(request)?;
        Ok(to_text(&self.session.delete_element(&request)))
    }

    /// The neighbourhood of vertices for the Graph panel: `request` is JSON `{ids, hops?,
    /// direction?, keep?, maxNodes?, maxEdges?}` (`hops` the depth, at most 5; `direction`
    /// `both`, `out` or `in`); answers the drawing's columns plus `seeds`, `missing`,
    /// `hiddenEdges` and, when the limits cut it, `leftOut: {vertices, edges, complete}`
    /// (graphersal-session `selection::SelectionView`). An unknown id or a
    /// malformed request throws.
    pub fn neighbourhood(&self, request: &str) -> Result<String, JsError> {
        self.session
            .neighbourhood(request)
            .map(|v| to_text(&v))
            .map_err(|e| JsError::new(&e))
    }

    /// A sample of one vertex label (`{vertices: {labels, exclude?}}`) or one label pair
    /// (`{edge: {label, from?, to?}}`), with `maxNodes`/`maxEdges`: the drawing's columns plus
    /// `sample: {kind, shown, total, complete}`.
    #[wasm_bindgen(js_name = labelSample)]
    pub fn label_sample(&self, request: &str) -> Result<String, JsError> {
        self.session
            .label_sample(request)
            .map(|v| to_text(&v))
            .map_err(|e| JsError::new(&e))
    }

    /// The label meta-graph: `{labels: [{label, count}], other, edges: [{label, from, to,
    /// count}], otherPairs, otherEdges, labelTotal, vertexCount, edgeCount, scannedEdges,
    /// complete}`.
    #[wasm_bindgen(js_name = metaGraph)]
    pub fn meta_graph(&self) -> Result<String, JsError> {
        self.session
            .meta_graph()
            .map(|v| to_text(&v))
            .map_err(|e| JsError::new(&e))
    }

    /// Starts the engine's force layout of a drawing (`vertexCount` vertices, edges from
    /// `edgeSource[j]` to `edgeTarget[j]`, the drawing's index columns as `Uint32Array`s) and runs
    /// its first slice of `budgetMs`. `options` is JSON `{seed?, iterations?}` (or empty). Answers
    /// the status JSON `{job, state, iterations, totalIterations, progress, elapsedMs,
    /// vertexCount, cached}`; a finished (`done`) or `cancelled` job has its positions in
    /// [`Self::layout_positions`]. A layout of a drawing laid out recently is answered from the
    /// cache.
    #[wasm_bindgen(js_name = layoutStart)]
    pub fn layout_start(
        &mut self,
        vertex_count: u32,
        edge_source: Vec<u32>,
        edge_target: Vec<u32>,
        options: &str,
        budget_ms: f64,
    ) -> Result<String, JsError> {
        let options: Value = if options.trim().is_empty() {
            Value::Null
        } else {
            body_json(options)?
        };
        let request = graphersal_session::layout::LayoutRequest {
            input: graphersal_session::layout::LayoutInput {
                vertex_count: vertex_count as usize,
                edge_source,
                edge_target,
            },
            seed: options.get("seed").and_then(Value::as_u64),
            iterations: options
                .get("iterations")
                .and_then(Value::as_u64)
                .map(|n| n.min(u64::from(u32::MAX)) as u32),
        };
        self.session
            .layout_start(request, budget_ms)
            .map(|v| to_text(&v))
            .map_err(|e| JsError::new(&e))
    }

    /// Runs the next slice (`budgetMs`) of layout `job`; answers its status (see
    /// [`Self::layout_start`]).
    #[wasm_bindgen(js_name = layoutStep)]
    pub fn layout_step(&mut self, job: u32, budget_ms: f64) -> Result<String, JsError> {
        self.session
            .layout_step(u64::from(job), budget_ms)
            .map(|v| to_text(&v))
            .map_err(|e| JsError::new(&e))
    }

    /// Cancels layout `job`, keeping the positions reached so far; answers its status.
    #[wasm_bindgen(js_name = layoutCancel)]
    pub fn layout_cancel(&mut self, job: u32) -> Result<String, JsError> {
        self.session
            .layout_cancel(u64::from(job))
            .map(|v| to_text(&v))
            .map_err(|e| JsError::new(&e))
    }

    /// The positions of layout `job` as a `Float32Array` (`x, y` per vertex); a job that is no
    /// longer running is forgotten afterwards.
    #[wasm_bindgen(js_name = layoutPositions)]
    pub fn layout_positions(&mut self, job: u32) -> Result<Vec<f32>, JsError> {
        self.session
            .layout_positions(u64::from(job))
            .map_err(|e| JsError::new(&e))
    }

    /// The graph's own memory footprint (`g.memory_usage()`), or `null`.
    #[wasm_bindgen(js_name = memoryUsage)]
    pub fn memory_usage(&self) -> String {
        to_text(&self.session.graph().memory_view())
    }

    /// The statistics panel: `{statistics, memory, process, limits: {presets, defaults}}` (see
    /// [`Session::statistics`]).
    pub fn statistics(&self) -> String {
        let mut statistics = self.session.statistics();
        graphersal_session::process::add_to_statistics(&mut statistics);
        to_text(&statistics)
    }

    /// Every DSL function, from the Rhai engine's metadata (completion catalogue).
    pub fn completions(&self) -> String {
        to_text(&views::completions(self.session.engine()))
    }

    /// The values of the DSL's token classes, `{class: [member, ...]}` (completion after `T.`,
    /// `Order.`, ...; see [`Session::token_values`]).
    #[wasm_bindgen(js_name = tokenValues)]
    pub fn token_values(&self) -> String {
        to_text(&self.session.token_values())
    }

    /// "Save graph": `{text, schema, warnings}`: the graph as GraphML text, the stored schema as
    /// JSON text (`null` without one), checked by loading both back (schema first).
    #[wasm_bindgen(js_name = saveGraphml)]
    pub fn save_graphml(&self) -> Result<String, JsError> {
        self.session
            .save_graphml()
            .map(|v| to_text(&v))
            .map_err(|e| JsError::new(&e))
    }

    /// The Store menu (the store in memory): identity, position, snapshots, marks, attic, the
    /// open view, sizes and the databases of the session (see `graphersal_session::store`).
    #[wasm_bindgen(js_name = storeInfo)]
    pub fn store_info(&self) -> Result<String, JsError> {
        store_text(self.session.store_info())
    }

    /// Checkpoint: a snapshot of the current state (`name` optional): `{snapshot}`.
    pub fn checkpoint(&mut self, name: &str) -> Result<String, JsError> {
        store_text(self.session.store_checkpoint(name))
    }

    /// Mark: names the current position: `{mark}`.
    pub fn mark(&mut self, name: &str) -> Result<String, JsError> {
        store_text(self.session.store_mark(name))
    }

    /// The stored snapshot at `commit` (`undefined`: the latest) as `.gsnap` bytes.
    #[wasm_bindgen(js_name = storeSnapshot)]
    pub fn store_snapshot(&self, commit: Option<f64>) -> Result<Vec<u8>, JsError> {
        let commit = commit.map(|c| c.max(0.0) as u64);
        self.session
            .store_snapshot(commit)
            .map(|(bytes, _)| bytes)
            .map_err(|e| JsError::new(&e.message))
    }

    /// Opens a read-only view of the state at `{target}` (JSON text): `{view, graph}`.
    #[wasm_bindgen(js_name = openView)]
    pub fn open_view(&mut self, body: &str) -> Result<String, JsError> {
        let body = body_json(body)?;
        store_text(self.session.store_open_view(&body))
    }

    /// Back to the live graph: `{view: null, graph}`.
    #[wasm_bindgen(js_name = closeView)]
    pub fn close_view(&mut self) -> Result<String, JsError> {
        store_text(self.session.store_close_view())
    }

    /// Forks the state at `{target}` into a new store in memory, added to the databases:
    /// `{fork, store}`.
    pub fn fork(&mut self, body: &str) -> Result<String, JsError> {
        let body = body_json(body)?;
        store_text(self.session.store_fork(&body, ForkInto::Memory))
    }

    /// Rolls the store back to `{target, delete}`: `{rollback, graph, store}`.
    pub fn rollback(&mut self, body: &str) -> Result<String, JsError> {
        let body = body_json(body)?;
        store_text(self.session.store_rollback(&body))
    }

    /// One page of attic entry `id`'s commits (`limit` 0: the default page of 100):
    /// `{commitCount, totals, offset, limit, commits, hasMore}`.
    #[wasm_bindgen(js_name = atticChanges)]
    pub fn attic_changes(&self, id: &str, offset: u32, limit: u32) -> Result<String, JsError> {
        let limit = (limit > 0).then_some(limit as usize);
        store_text(self.session.store_attic_changes(id, offset as usize, limit))
    }

    /// Undoes the rollback of attic entry `id`: `{restored, graph, store}`.
    #[wasm_bindgen(js_name = atticRestore)]
    pub fn attic_restore(&mut self, id: &str) -> Result<String, JsError> {
        store_text(self.session.store_attic_restore(id))
    }

    /// Attic entry `id` as a new store in memory: `{fork, store}`.
    #[wasm_bindgen(js_name = atticFork)]
    pub fn attic_fork(&mut self, id: &str) -> Result<String, JsError> {
        store_text(self.session.store_attic_fork(id, ForkInto::Memory))
    }

    /// Deletes attic entry `id`: `{removed}`.
    #[wasm_bindgen(js_name = atticRemove)]
    pub fn attic_remove(&mut self, id: &str) -> Result<String, JsError> {
        store_text(self.session.store_attic_remove(id))
    }

    /// Scrubs the store: `{verify}`.
    #[wasm_bindgen(js_name = verifyStore)]
    pub fn verify_store(&self) -> Result<String, JsError> {
        store_text(self.session.store_verify())
    }

    /// Whether Compact is worth it now (in memory: a prune), with the numbers: `{compaction}`.
    #[wasm_bindgen(js_name = compactionAdvice)]
    pub fn compaction_advice(&self) -> Result<String, JsError> {
        store_text(self.session.store_compaction_advice())
    }

    /// Compact (in memory: a prune up to the current commit): `{compact, compaction}`. Refused
    /// (nothing changed) when it would make marks unreachable and `drop_marks` is false: the
    /// Store menu asks the user first and passes `true` after the confirmation.
    #[wasm_bindgen(js_name = compactStore)]
    pub fn compact_store(&mut self, drop_marks: bool) -> Result<String, JsError> {
        store_text(self.session.store_compact(drop_marks))
    }

    /// Switches to database `id` (a fork): `{graph, store}`.
    #[wasm_bindgen(js_name = switchDatabase)]
    pub fn switch_database(&mut self, id: &str) -> Result<String, JsError> {
        store_text(self.session.store_switch(id))
    }

    /// Closes database `id` (not the active one): `{closed, store}`.
    #[wasm_bindgen(js_name = closeDatabase)]
    pub fn close_database(&mut self, id: &str) -> Result<String, JsError> {
        store_text(self.session.store_close(id))
    }

    /// The saved queries (`folder`: only those in it and below it), as JSON text:
    /// `[{name, folder, description, params, status, error?}]` (`graphersal_session::queries`).
    #[wasm_bindgen(js_name = listQueries)]
    pub fn list_queries(&self, folder: Option<String>) -> String {
        to_text(&self.session.list_queries(folder.as_deref()))
    }

    /// The saved query `name` with its body, or `null`.
    #[wasm_bindgen(js_name = getQuery)]
    pub fn get_query(&self, name: &str) -> String {
        to_text(&self.session.get_query(name))
    }

    /// Defines or replaces a saved query from the editor's spec (JSON text): `{ok, query}` or
    /// `{ok: false, errors: [{field, message, help?}]}`; never throws for a refused definition.
    #[wasm_bindgen(js_name = defineQuery)]
    pub fn define_query(&mut self, spec: &str) -> Result<String, JsError> {
        let spec = body_json(spec)?;
        Ok(to_text(&self.session.define_query(&spec)))
    }

    /// Drops the saved query `name`: `{dropped}`.
    #[wasm_bindgen(js_name = dropQuery)]
    pub fn drop_query(&mut self, name: &str) -> Result<String, JsError> {
        self.session
            .drop_query(name)
            .map(|dropped| to_text(&serde_json::json!({ "dropped": dropped })))
            .map_err(|e| JsError::new(&e))
    }

    /// Moves the saved query `name` into `folder` (empty: the root); its entry.
    #[wasm_bindgen(js_name = moveQuery)]
    pub fn move_query(&mut self, name: &str, folder: &str) -> Result<String, JsError> {
        self.session
            .move_query(name, Some(folder))
            .map(|v| to_text(&v))
            .map_err(|e| JsError::new(&e))
    }

    /// The call text of a saved query with the named arguments `args` (JSON text of an
    /// object): `g.query("name", #{..})`.
    #[wasm_bindgen(js_name = queryCallText)]
    pub fn query_call_text(&self, name: &str, args: &str) -> Result<String, JsError> {
        let args: Value = if args.trim().is_empty() {
            Value::Null
        } else {
            body_json(args)?
        };
        graphersal_session::queries::query_call_text(name, &args)
            .map(|text| to_text(&Value::String(text)))
            .map_err(|e| JsError::new(&e))
    }

    /// The preview of a call of the saved query `name` with the named arguments `args` (JSON
    /// text of an object), run as a dry run and rolled back: `{ok, changes}` or `{ok: false,
    /// error}` (`graphersal_session::Session::preview_query`).
    #[wasm_bindgen(js_name = previewQuery)]
    pub fn preview_query(&mut self, name: &str, args: &str) -> Result<String, JsError> {
        let args: Value = if args.trim().is_empty() {
            Value::Null
        } else {
            body_json(args)?
        };
        Ok(to_text(&self.session.preview_query(name, &args)))
    }

    /// "Catalog ▸ Save catalog": the whole catalog (saved queries, compression rules, every
    /// other kind) as its JSON exchange form (`{"definitions": [..]}`).
    #[wasm_bindgen(js_name = saveCatalog)]
    pub fn save_catalog(&self) -> String {
        self.session.save_catalog_text()
    }

    /// "Catalog ▸ Load catalog": stores the definitions of a catalog file (JSON text) in one
    /// commit; `replace` drops those the file does not have. `{loaded, graph}`.
    #[wasm_bindgen(js_name = loadCatalog)]
    pub fn load_catalog(&mut self, text: &str, replace: bool) -> Result<String, JsError> {
        self.session
            .load_catalog(text, replace)
            .map(|v| to_text(&v))
            .map_err(|e| JsError::new(&e))
    }

    /// The compression rules with their current stats, as JSON text: `[{name, element, label,
    /// path, codec, minBytes, dictionary, stats}]` (`graphersal_session::compression`).
    #[wasm_bindgen(js_name = listCompressions)]
    pub fn list_compressions(&self) -> String {
        to_text(&self.session.list_compressions())
    }

    /// Defines or replaces a compression rule from the form's spec (JSON text): `{ok, rule}` or
    /// `{ok: false, errors: [{field, message, help?}]}`; never throws for a refused rule.
    #[wasm_bindgen(js_name = defineCompression)]
    pub fn define_compression(&mut self, spec: &str) -> Result<String, JsError> {
        let spec = body_json(spec)?;
        Ok(to_text(&self.session.define_compression(&spec)))
    }

    /// Drops the compression rule `name` (its values are decompressed): `{dropped}`.
    #[wasm_bindgen(js_name = dropCompression)]
    pub fn drop_compression(&mut self, name: &str) -> Result<String, JsError> {
        self.session
            .drop_compression(name)
            .map(|dropped| to_text(&serde_json::json!({ "dropped": dropped })))
            .map_err(|e| JsError::new(&e))
    }

    /// Rebuilds the rule's dictionary and re-encodes its values (no data change): its entry.
    #[wasm_bindgen(js_name = recompress)]
    pub fn recompress(&mut self, name: &str) -> Result<String, JsError> {
        self.session
            .recompress(name)
            .map(|v| to_text(&v))
            .map_err(|e| JsError::new(&e))
    }

    /// Estimates compression rules for an estimate spec (JSON text `{label, path, element?,
    /// codec?, minBytes?, dictionary?}`) without changing the graph: `{ok, estimate}` or `{ok:
    /// false, errors: [{field, message, help?}]}` (`graphersal_session::compression`); never
    /// throws for a refused spec.
    #[wasm_bindgen(js_name = estimateCompression)]
    pub fn estimate_compression(&self, spec: &str) -> Result<String, JsError> {
        let spec = body_json(spec)?;
        Ok(to_text(&self.session.estimate_compression(&spec)))
    }

    /// The property indexes with their current stats, as JSON text: `[{name, element, label,
    /// key, served, stats}]` (`graphersal_session::indexes`).
    #[wasm_bindgen(js_name = listIndexes)]
    pub fn list_indexes(&self) -> String {
        to_text(&self.session.list_indexes())
    }

    /// Creates a property index from the form's spec (JSON text `{label, key, element?,
    /// name?}`): `{ok, index}` or `{ok: false, errors: [{field, message, help?}]}`; never throws
    /// for a refused index.
    #[wasm_bindgen(js_name = createIndex)]
    pub fn create_index(&mut self, spec: &str) -> Result<String, JsError> {
        let spec = body_json(spec)?;
        Ok(to_text(&self.session.create_index(&spec)))
    }

    /// Drops the property index `name`: `{dropped}`.
    #[wasm_bindgen(js_name = dropIndex)]
    pub fn drop_index(&mut self, name: &str) -> Result<String, JsError> {
        self.session
            .drop_index(name)
            .map(|dropped| to_text(&serde_json::json!({ "dropped": dropped })))
            .map_err(|e| JsError::new(&e))
    }

    /// "Save graph" as GraphSON: `{text, schema, warnings}`: the graph as GraphSON 3.0 text (line
    /// form), the stored schema as JSON text (`null` without one), checked by loading both back.
    #[wasm_bindgen(js_name = saveGraphson)]
    pub fn save_graphson(&self) -> Result<String, JsError> {
        self.session
            .save_graphson()
            .map(|v| to_text(&v))
            .map_err(|e| JsError::new(&e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Store menu in memory, natively: a loaded graph is a store; checkpoint, mark, view,
    /// rollback, attic, fork into a second database, switch, close, verify.
    #[test]
    fn a_loaded_graph_is_an_in_memory_store_with_the_store_menu() {
        let json = |text: String| -> Value { serde_json::from_str(&text).expect("JSON") };
        let mut p = Playground::new();
        p.load_sample("modern").unwrap();
        let info = json(p.store_info().unwrap());
        assert_eq!(info["memory"], true);
        assert_eq!(info["commitSeq"], 0);
        let r = json(p.execute("g.add_v(\"person\").next()", "{}").unwrap());
        assert_eq!(r["ok"], true);
        p.mark("m1").unwrap();
        assert_eq!(json(p.checkpoint("").unwrap())["snapshot"]["commitSeq"], 1);
        assert!(!p.store_snapshot(None).unwrap().is_empty());
        let view = json(p.open_view(r#"{"target": {"commit": 0}}"#).unwrap());
        assert_eq!(view["graph"]["vertexCount"], 6);
        p.close_view().unwrap();
        let fork = json(p.fork(r#"{"target": {"mark": "m1"}}"#).unwrap());
        let id = fork["fork"]["database"].as_str().unwrap().to_string();
        let rolled = json(p.rollback(r#"{"target": {"commit": 0}}"#).unwrap());
        let entry = rolled["rollback"]["attic"].as_str().unwrap().to_string();
        let page = json(p.attic_changes(&entry, 0, 0).unwrap());
        assert_eq!(page["commitCount"], 1);
        assert_eq!(page["limit"], 100);
        json(p.attic_restore(&entry).unwrap());
        assert_eq!(json(p.verify_store().unwrap())["verify"]["ok"], true);
        // In memory compaction is a prune: the advice is about prune only.
        let advice = json(p.compaction_advice().unwrap());
        assert_eq!(advice["compaction"]["compactable"], false);
        assert_eq!(advice["compaction"]["garbageBytes"], 0);
        // m1 is at commit 1, the oldest snapshot at 0 stays (the last two stay): nothing to ask.
        let compacted = json(p.compact_store(false).unwrap());
        assert_eq!(compacted["compact"]["compacted"], false);
        assert_eq!(
            json(p.switch_database(&id).unwrap())["graph"]["vertexCount"],
            7
        );
        p.switch_database("main").unwrap();
        let closed = json(p.close_database(&id).unwrap());
        assert_eq!(closed["store"]["databases"].as_array().unwrap().len(), 1);
    }

    /// The saved-query methods, natively: define, list, call text, file round trip, move, drop.
    #[test]
    fn saved_queries_through_the_playground_api() {
        let json = |text: String| -> Value { serde_json::from_str(&text).expect("JSON") };
        let mut p = Playground::new();
        p.load_sample("modern").unwrap();
        let spec = r#"{"name": "names", "body": "fn names() { g.v().values(\"name\") }"}"#;
        assert_eq!(json(p.define_query(spec).unwrap())["ok"], true);
        assert_eq!(json(p.list_queries(None))[0]["name"], "names");
        assert_eq!(json(p.get_query("names"))["status"], "ok");
        assert_eq!(
            json(p.query_call_text("names", "").unwrap()),
            "g.query(\"names\")"
        );
        assert_eq!(json(p.move_query("names", "x").unwrap())["folder"], "x");
        let file = p.save_catalog();
        assert_eq!(json(p.drop_query("names").unwrap())["dropped"], true);
        assert_eq!(json(p.load_catalog(&file, true).unwrap())["loaded"], 1);
        assert_eq!(json(p.info())["savedQueries"], 1);
    }

    #[test]
    fn a_compression_estimate_through_the_playground_api() {
        let json = |text: String| -> Value { serde_json::from_str(&text).expect("JSON") };
        let mut p = Playground::new();
        p.load_sample("modern").unwrap();
        let answer = json(
            p.estimate_compression(r#"{"label": "person", "path": "name", "minBytes": 1}"#)
                .unwrap(),
        );
        assert_eq!(answer["ok"], true, "{answer}");
        assert_eq!(answer["estimate"]["valuesFound"], 4);
        assert_eq!(answer["estimate"]["graphChanged"], false);
        assert_eq!(
            answer["estimate"]["candidates"].as_array().unwrap().len(),
            2
        );
        let refused = json(
            p.estimate_compression(r#"{"label": "person", "path": "a[0]"}"#)
                .unwrap(),
        );
        assert_eq!(refused["errors"][0]["field"], "path");
        assert_eq!(json(p.info())["compressionRules"], 0);
    }

    #[test]
    fn compression_rules_through_the_playground_api() {
        let json = |text: String| -> Value { serde_json::from_str(&text).expect("JSON") };
        let mut p = Playground::new();
        p.load_sample("modern").unwrap();
        let spec = r#"{"name": "names", "label": "person", "path": "name", "minBytes": 1}"#;
        let defined = json(p.define_compression(spec).unwrap());
        assert_eq!(defined["ok"], true, "{defined}");
        assert_eq!(json(p.list_compressions())[0]["path"], "$.name");
        assert_eq!(json(p.recompress("names").unwrap())["name"], "names");
        let refused = json(
            p.define_compression(r#"{"name": "x", "label": "person", "path": "a[0]"}"#)
                .unwrap(),
        );
        assert_eq!(refused["errors"][0]["field"], "path");
        let file = p.save_catalog();
        assert!(file.contains("\"compression\""), "{file}");
        assert_eq!(json(p.drop_compression("names").unwrap())["dropped"], true);
        assert_eq!(json(p.info())["compressionRules"], 0);
        assert_eq!(json(p.load_catalog(&file, true).unwrap())["loaded"], 1);
        assert_eq!(json(p.info())["compressionRules"], 1);
        assert_eq!(json(p.info())["catalog"]["compression"], 1);
    }

    #[test]
    fn property_indexes_through_the_playground_api() {
        let json = |text: String| -> Value { serde_json::from_str(&text).expect("JSON") };
        let mut p = Playground::new();
        p.load_sample("modern").unwrap();
        let created = json(
            p.create_index(r#"{"label": "person", "key": "age"}"#)
                .unwrap(),
        );
        assert_eq!(created["ok"], true, "{created}");
        assert_eq!(created["index"]["name"], "person_age");
        assert_eq!(json(p.list_indexes())[0]["stats"]["entries"], 4);
        let refused = json(p.create_index(r#"{"label": "person"}"#).unwrap());
        assert_eq!(refused["errors"][0]["field"], "key");
        assert_eq!(json(p.info())["propertyIndexes"], 1);
        let file = p.save_catalog();
        assert!(file.contains("\"property_index\""), "{file}");
        assert_eq!(json(p.drop_index("person_age").unwrap())["dropped"], true);
        assert_eq!(json(p.info())["propertyIndexes"], 0);
        assert_eq!(json(p.load_catalog(&file, true).unwrap())["loaded"], 1);
        assert_eq!(json(p.info())["catalog"]["property_index"], 1);
    }

    /// The property editor: the typed edit view, a dry run, a save, a refused value.
    #[test]
    fn element_editing_through_the_playground_api() {
        let json = |text: String| -> Value { serde_json::from_str(&text).expect("JSON") };
        let mut p = Playground::new();
        p.load_sample("modern").unwrap();
        let view = json(p.element_for_edit("vertex", "1").unwrap());
        assert_eq!(view["readOnly"], Value::Null);
        let request =
            r#"{"kind": "vertex", "id": "1", "set": [["age", {"type": "int64", "value": "30"}]]}"#;
        let checked = request.replace("\"set\"", "\"validateOnly\": true, \"set\"");
        assert_eq!(json(p.update_element(&checked).unwrap())["changed"], false);
        let saved = json(p.update_element(request).unwrap());
        assert_eq!(saved["changed"], true, "{saved}");
        let bad =
            r#"{"kind": "vertex", "id": "1", "set": [["age", {"type": "uuid", "value": "x"}]]}"#;
        assert_eq!(
            json(p.update_element(bad).unwrap())["errors"][0]["field"],
            "age"
        );
    }

    /// The edit mode: label choices, a created vertex and edge, a deleted vertex.
    #[test]
    fn creating_and_deleting_through_the_playground_api() {
        let json = |text: String| -> Value { serde_json::from_str(&text).expect("JSON") };
        let mut p = Playground::new();
        p.load_sample("modern").unwrap();
        let choices = json(
            p.label_choices(r#"{"kind": "edge", "from": "1", "to": "2"}"#)
                .unwrap(),
        );
        assert_eq!(choices["free"], true, "{choices}");
        let vertex = json(
            p.create_element(
                r#"{"kind": "vertex", "labels": ["person"],
                    "properties": [["name", {"type": "string", "value": "ann"}]]}"#,
            )
            .unwrap(),
        );
        assert_eq!(vertex["created"], true, "{vertex}");
        let id = vertex["drawn"]["id"].as_str().unwrap().to_string();
        let edge = json(
            p.create_element(&format!(
                r#"{{"kind": "edge", "label": "knows", "from": "1", "to": "{id}"}}"#
            ))
            .unwrap(),
        );
        assert_eq!(edge["drawn"]["source"], "1", "{edge}");
        let deleted = json(
            p.delete_element(&format!(r#"{{"kind": "vertex", "id": "{id}"}}"#))
                .unwrap(),
        );
        assert_eq!(deleted["edges"], 1, "{deleted}");
    }

    /// The script an example of the Examples menu runs: its `script`, or for a GQL example
    /// (`lang: "gql"`, opened in a GQL tab) the call the playground runs for it,
    /// `g.gql("<script>", #{<params>})` (`graphersal_session::queries::gql_call_text`).
    fn example_script(ex: &Value) -> String {
        let script = ex["script"].as_str().expect("script");
        if ex["lang"].as_str() == Some("gql") {
            graphersal_session::queries::gql_call_text(script, &ex["params"]).expect("params")
        } else {
            script.to_string()
        }
    }

    /// Every example of the playground's Examples menu (`playground/js/examples.json`) runs on
    /// its sample graph; the ones marked `expect: "error"` fail, all others succeed.
    #[test]
    fn playground_examples_run_against_the_engine() {
        let examples: Vec<Value> =
            serde_json::from_str(include_str!("../../../playground/js/examples.json"))
                .expect("examples.json");
        assert!(!examples.is_empty());
        let options = RunOptions {
            profile: true,
            ..RunOptions::default()
        };
        for ex in &examples {
            let title = ex["title"].as_str().expect("title");
            let script = &example_script(ex);
            let graph = match ex["graph"].as_str().expect("graph") {
                "any" => "modern",
                kind => kind,
            };
            let mut session = Session::new();
            session.load_sample(graph).expect("sample graph");
            let r: Value =
                serde_json::from_str(&session.execute(script, &options)).expect("response JSON");
            let expect_error = ex["expect"].as_str() == Some("error");
            assert_eq!(r["ok"], !expect_error, "example {title:?} on {graph}: {r}");
        }
    }

    /// The GQL group of the Examples menu has one example per stage 1b construct, and every
    /// GQL example that returns several rows orders them (`ORDER BY`), since GQL rows come in
    /// no defined order.
    #[test]
    fn playground_gql_examples_cover_stage_1b_and_order_their_rows() {
        let examples: Vec<Value> =
            serde_json::from_str(include_str!("../../../playground/js/examples.json"))
                .expect("examples.json");
        let gql: Vec<String> = examples
            .iter()
            .filter(|ex| ex["group"] == "GQL")
            .map(example_script)
            .collect();
        for construct in [
            "count(",
            "collect_list(",
            "sum(",
            "avg(",
            "min(",
            "max(",
            "count(DISTINCT",
            "OPTIONAL MATCH",
            "WITH ",
            "LET ",
            "FILTER ",
            "->{1,2}",
            "*1..2]",
            "p = (",
            "path_length(p)",
        ] {
            assert!(
                gql.iter().any(|script| script.contains(construct)),
                "no GQL example uses {construct:?}"
            );
        }
        let options = RunOptions::default();
        let mut tables = 0;
        for script in &gql {
            let mut session = Session::new();
            session.load_sample("modern").expect("sample graph");
            let r: Value =
                serde_json::from_str(&session.execute(script, &options)).expect("response JSON");
            let rows = r["display"]["table"]["rows"].as_array().map_or(0, Vec::len);
            // A Gremlin step after gql() decides the shape itself (`select`, `count`).
            tables += usize::from(rows > 1);
            if rows > 1 && !script.contains(").select(") && !script.contains(".count()") {
                assert!(
                    script.contains("ORDER BY"),
                    "GQL example without ORDER BY: {script}"
                );
            }
        }
        assert!(tables > 10, "the row tables were read ({tables})");
    }
}
