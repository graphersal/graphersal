//! The playground session: one graph, one Rhai engine, and the operations of the playground's
//! engine boundary (`playground/js/engine/engine.js`) as plain Rust returning JSON.
//!
//! Nothing here depends on wasm-bindgen or HTTP: `graphersal-wasm` wraps it for JavaScript and
//! the dev server of `graphersal-cli` for HTTP, so both answer with the same structures. Result
//! data stays [`ElementProperty`] (or the engine's own JSON text) until it is written out, so map
//! keys keep the engine's order.

use crate::error::SessionError;
use std::io::Cursor;
use std::sync::{Arc, Mutex};

use graphersal::auth::{AccessPolicy, ActionKind, Authorizer, ResourceKind};
use graphersal::catalog::{Definition, DefinitionKind};
use graphersal::changes::RollbackReason;
use graphersal::exec::CancelToken;
use graphersal::exec::{RenderOptions, Spelling};
use graphersal::io::GraphImport;
use graphersal::prelude::*;
use graphersal::schema::{PropertySchema, SchemaPatch};
use graphersal::script::rhai::{self, Dynamic, EvalAltResult};
use graphersal::script::{
    RenderedText, ScriptExecution, ScriptGraphTraversalSource, ScriptLimits,
    graph_scope_with_options, render_scope_options, schema_of_value, script_value_to_property,
    try_render_value,
};
use graphersal::storage::{AnyGraph, ElementKind};
use graphersal::vector::Embedders;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use serde_json::{Value, json};

use crate::display::{
    Bounds, CompactGraph, Display, Table, bounded_display_at, json_safe, values_json,
};
use crate::storage::{GraphFactory, IntoSessionGraph, SessionGraph, StorageKind};

/// The diagnostic of a script stopped through [`Session::cancel_token`] outside a traversal (a
/// traversal reports `TraverserError::Cancelled` itself).
const CANCELLED_DIAGNOSTIC: &str = "Error: Script cancelled\n\
Help: The query was stopped on request (the Stop button) and everything it changed was rolled \
back; nothing of it is kept. Run it again to get its results.";

/// Default display limits, identical to the CLI's.
pub const DEFAULT_MAX_ROWS: usize = 100;
/// Default number of items shown of every nested collection.
pub const DEFAULT_MAX_ITEMS: usize = 100;

/// Options of [`Session::execute`] (the JSON object the playground passes; snake_case keys).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RunOptions {
    /// Collect `TraversalMetrics` for a traversal without a terminal.
    pub profile: bool,
    /// Extra metric sections, e.g. `["memory"]`; ignored without `profile`.
    pub profile_types: Vec<String>,
    /// Display limit: top-level results shown (data is never cut).
    pub max_rows: Option<usize>,
    /// Display limit: items shown of every nested collection.
    pub max_items: Option<usize>,
    /// Display everything.
    pub no_limit: bool,
    /// Display paging: the top-level results skipped before the shown ones (default 0); the
    /// display then shows the `max_rows` results after them. The query itself is unchanged
    /// (every result is still computed). Ignored for a single value. It pages the JSON and
    /// Table views; the Raw view stays the engine visualizer's output of the first results.
    pub offset: Option<usize>,
    /// Spelling of step names in the profile and error messages: `"snake"` (default) or
    /// `"camel"` (the session option `render.spelling`).
    pub spelling: Spelling,
    /// Most vertices + edges of the response's `subgraph` (the elements the results refer to,
    /// drawn by the Graph panel): default [`crate::display::DEFAULT_SUBGRAPH_MAX_ELEMENTS`], at
    /// most [`crate::display::SUBGRAPH_MAX_ELEMENTS`]; a byte bound applies as well.
    pub subgraph_max_elements: Option<usize>,
    /// Add the `vertexInDegree` / `vertexOutDegree` columns (degrees in the whole graph) to the
    /// response's `subgraph`.
    pub subgraph_degrees: bool,
}

impl RunOptions {
    fn render_options(&self) -> RenderOptions {
        if self.no_limit {
            return RenderOptions::unlimited().with_spelling(self.spelling);
        }
        RenderOptions::new()
            .with_max_rows(self.max_rows.unwrap_or(DEFAULT_MAX_ROWS).max(1))
            .with_max_items(self.max_items.unwrap_or(DEFAULT_MAX_ITEMS).max(1))
            .with_spelling(self.spelling)
    }
}

/// Host presets of a [`Session`]: the resource limits every run of it gets. The default (what
/// [`Session::new`] uses, and the playground) is no limit at all: no Rhai resource limits, no
/// preset traversal option, no script budget. The dev server of `graphersal-cli` fills them from
/// its `--timeout`, `--memory-limit`, `--max-*` ... flags.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SessionPresets {
    /// The Rhai resource limits of the session's engine.
    pub limits: ScriptLimits,
    /// Traversal options preset on every traversal source of a run (`g`, `_g`, `__` and the
    /// sources a script creates): `evaluationTimeout`, `traversal.max_traversers`,
    /// `memory.limit`, ... A query overrides one with `g.with(..)` unless the host locks it.
    pub options: Vec<(String, ElementProperty)>,
    /// Budget of a whole run in milliseconds: the Rhai code between the traversals (a `loop
    /// {}`) is stopped once the run is older than this. `None`: no budget.
    pub script_timeout_ms: Option<u64>,
    /// The embedders every run's `embed` step and `embed(text)` call (and `g.embedders()`
    /// lists). The default holds only the built-in `hash-ngram/1`; a host that loads models
    /// registers them here ([`Self::with_embedders`]).
    pub embedders: Arc<Embedders>,
}

impl Default for SessionPresets {
    fn default() -> Self {
        Self {
            limits: ScriptLimits::unlimited(),
            options: Vec::new(),
            script_timeout_ms: None,
            embedders: Arc::new(Embedders::default()),
        }
    }
}

impl SessionPresets {
    /// No limits (the default): [`ScriptLimits::unlimited`], no options, no script budget.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the Rhai resource limits.
    pub fn with_limits(mut self, limits: ScriptLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Adds a preset traversal option (`evaluationTimeout`, `memory.limit`, ...).
    pub fn with_option(mut self, key: impl Into<String>, value: ElementProperty) -> Self {
        self.options.push((key.into(), value));
        self
    }

    /// Sets the embedders of every run (the default: only the built-in `hash-ngram/1`).
    pub fn with_embedders(mut self, embedders: Arc<Embedders>) -> Self {
        self.embedders = embedders;
        self
    }

    /// Sets the budget of a whole run in milliseconds (`None` or `0`: no budget).
    pub fn with_script_timeout_ms(mut self, ms: Option<u64>) -> Self {
        self.script_timeout_ms = ms.filter(|ms| *ms > 0);
        self
    }

    /// The presets as data, as `GET /api/info` and the statistics panel show them:
    /// `{options: {key: value}, scriptLimits: {max_operations, ...}, scriptTimeoutMs}`. Only the
    /// Rhai limits that bound anything are listed (`0` is unlimited and left out); values go
    /// through the physical codec (serde), never `Debug`.
    pub fn to_value(&self) -> Value {
        let mut options = serde_json::Map::new();
        for (key, value) in &self.options {
            options.insert(
                key.clone(),
                serde_json::to_value(value).unwrap_or(Value::Null),
            );
        }
        let limits = &self.limits;
        let mut script = serde_json::Map::new();
        for (key, value) in [
            ("max_operations", limits.max_operations),
            ("max_string_size", limits.max_string_size as u64),
            ("max_array_size", limits.max_array_size as u64),
            ("max_map_size", limits.max_map_size as u64),
        ] {
            if value > 0 {
                script.insert(key.to_string(), json!(value));
            }
        }
        json!({
            "options": options,
            "scriptLimits": script,
            "scriptTimeoutMs": self.script_timeout_ms,
        })
    }

    fn option_slice(&self) -> Vec<(&str, ElementProperty)> {
        self.options
            .iter()
            .map(|(key, value)| (key.as_str(), value.clone()))
            .collect()
    }
}

/// One loaded graph and the engine that runs scripts against it.
///
/// The graph is of the host's storage ([`Self::with_storage`], a [`GraphFactory`]); every
/// operation goes through the object-safe [`SessionGraph`], so the session's JSON is the same
/// for every storage. [`Self::new`] serves [`graphersal::TraversalGraph`].
pub struct Session {
    pub(crate) graph: Arc<dyn SessionGraph>,
    /// The host's storage: how loaded graphs and stores are built.
    pub(crate) factory: Arc<dyn GraphFactory>,
    pub(crate) name: String,
    kind: String,
    pub(crate) engine: rhai::Engine,
    /// The host presets every run gets ([`Self::with_presets`]).
    presets: SessionPresets,
    /// When the running [`Self::execute`] started, for the script budget.
    started: Arc<Mutex<Option<web_time::Instant>>>,
    /// What the last load skipped (a GraphSON import report), shown in `info().warnings`.
    pub(crate) warnings: Vec<String>,
    /// Cancels the running [`Self::execute`] ([`Self::cancel_token`]).
    cancel: CancelToken,
    /// The token the running script checks in its progress callback: [`Self::cancel`], or the
    /// caller's token of [`Self::execute_with_cancel`].
    run_cancel: Arc<Mutex<CancelToken>>,
    /// The stores the session serves ([`crate::store`]): `None` without a store.
    pub(crate) stores: Option<crate::store::Stores>,
    /// Every loaded graph becomes a store in memory ([`Self::set_memory_stores`]).
    pub(crate) memory_stores: bool,
    /// The force layouts of the Graph panel: running jobs and finished ones ([`crate::layout`]).
    layouts: crate::layout::LayoutJobs,
    /// The scheduled jobs of the active store and the time zone they run in ([`crate::jobs`]).
    pub(crate) jobs: crate::jobs::SessionJobs,
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

/// `{data, schema}` of a response: what the committed run changed.
#[derive(Serialize, Default, Clone, Copy)]
struct Changes {
    data: bool,
    schema: bool,
    /// The catalog of definitions (saved queries) changed: the playground reloads its Saved
    /// queries menu.
    definitions: bool,
}

/// `change_summary` of a response: the net vertex and edge count changes of a committed run that
/// changed the data, from the graph's exact counts before and after the run (no commit hook,
/// so a write pays nothing for it).
#[derive(Serialize)]
struct ChangeSummary {
    /// After minus before.
    vertices: i64,
    /// After minus before.
    edges: i64,
    /// Both as one line (`+3 vertices, +2 edges`, `-1 vertex`); empty when neither count moved
    /// (a run that only set properties). The MCP answers and the playground's result footer
    /// show it after "changed the graph".
    text: String,
}

impl ChangeSummary {
    fn of(vertices: i64, edges: i64) -> Self {
        let part = |n: i64, one: &str, many: &str| {
            (n != 0).then(|| format!("{n:+} {}", if n.abs() == 1 { one } else { many }))
        };
        let text = [
            part(vertices, "vertex", "vertices"),
            part(edges, "edge", "edges"),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(", ");
        Self {
            vertices,
            edges,
            text,
        }
    }
}

/// The answer to [`Session::execute`] (`ExecuteResponse` in engine.js).
#[derive(Serialize)]
struct Response {
    ok: bool,
    terminal: &'static str,
    implicit: bool,
    /// The run committed a change to the graph (its commit sequence number advanced): exact,
    /// a failed or rolled-back statement does not count.
    mutated: bool,
    /// What the run changed and kept: `{data, schema}` (a schema change is a change too; the
    /// playground refreshes its schema panel on `schema: true`).
    changes: Changes,
    /// Present only when the run committed a data change.
    #[serde(skip_serializing_if = "Option::is_none")]
    change_summary: Option<ChangeSummary>,
    execution: Box<RawValue>,
    display: Option<Display>,
    subgraph: Option<CompactGraph>,
    #[serde(skip_serializing_if = "Option::is_none")]
    preferred_view: Option<&'static str>,
    elapsed_ms: f64,
    graph: Value,
}

impl Session {
    /// A session over an empty graph. The engine runs without Rhai resource limits, like the
    /// CLI: the playground is a local front end; its Stop button ends a runaway script by
    /// terminating the Web Worker, the dev server's through [`Self::cancel_token`].
    pub fn new() -> Self {
        Self::with_presets(SessionPresets::default())
    }

    /// A session over an empty graph whose runs get the host `presets`: the engine's Rhai
    /// limits, the traversal options preset on every source, and the budget of a whole run.
    pub fn with_presets(presets: SessionPresets) -> Self {
        Self::with_storage_and_presets(
            Arc::new(StorageKind::<graphersal::TraversalGraph>::persistent()),
            presets,
        )
    }

    /// A session over an empty graph of the host's storage: every graph it builds (an empty
    /// one, a sample, a GraphML/GraphSON/`.gsnap` load, a store in memory, a fork) comes from
    /// `factory` ([`StorageKind::new`] for any storage, [`StorageKind::persistent`] for a
    /// [`graphersal::storage::PersistentStorage`], which adds `.gsnap` loads and stores). No
    /// limits, like [`Self::new`].
    pub fn with_storage(factory: Arc<dyn GraphFactory>) -> Self {
        Self::with_storage_and_presets(factory, SessionPresets::default())
    }

    /// [`Self::with_storage`] with the host `presets` of [`Self::with_presets`].
    pub fn with_storage_and_presets(
        factory: Arc<dyn GraphFactory>,
        presets: SessionPresets,
    ) -> Self {
        let cancel = CancelToken::new();
        let mut engine = graphersal::script::engine_with_options(
            &presets.limits,
            playground_authorizer(),
            &presets.option_slice(),
        );
        graphersal::script::set_engine_embedders(&mut engine, presets.embedders.clone());
        // A script loop that runs no traversal is stopped here, on Stop or past the script
        // budget; traversals check the token and their own evaluationTimeout themselves.
        let run_cancel = Arc::new(Mutex::new(cancel.clone()));
        let token = run_cancel.clone();
        let started: Arc<Mutex<Option<web_time::Instant>>> = Arc::new(Mutex::new(None));
        let clock = started.clone();
        let budget = presets
            .script_timeout_ms
            .map(std::time::Duration::from_millis);
        engine.on_progress(move |ops| {
            if ops & 255 != 0 {
                return None;
            }
            // Both slots hold a plain value (a token, an instant) that a panic cannot leave
            // half-written: a poisoned lock is read through, never taken as "not cancelled".
            if token
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_cancelled()
            {
                return Some(Dynamic::from(CANCELLED_DIAGNOSTIC));
            }
            let limit = budget?;
            let start = (*clock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner))?;
            let elapsed = start.elapsed();
            (elapsed >= limit).then(|| Dynamic::from(script_timeout_diagnostic(limit, elapsed)))
        });
        Self {
            graph: factory.empty(),
            factory,
            name: "Empty graph".to_string(),
            kind: "empty".to_string(),
            engine,
            presets,
            started,
            warnings: Vec::new(),
            cancel,
            run_cancel,
            stores: None,
            memory_stores: false,
            layouts: crate::layout::LayoutJobs::default(),
            jobs: crate::jobs::SessionJobs::default(),
        }
    }

    /// The token that cancels the running [`Self::execute`] from another thread: the query
    /// stops at its next check with a "cancelled" error and its unit rolls back (the response
    /// is `ok: false`). `execute` resets the token when it starts, so a cancel reaches only the
    /// run in progress.
    pub fn cancel_token(&self) -> CancelToken {
        self.cancel.clone()
    }

    /// The sample graphs this engine can build.
    pub fn sample_graphs() -> Value {
        json!([
            {"kind": "empty", "title": "Empty graph", "available": true,
             "description": "Start from nothing and add vertices and edges with add_v() / add_e() / property()."},
            {"kind": "modern", "title": "Modern (TinkerPop)", "available": true,
             "description": "The TinkerPop sample: 4 people and 2 pieces of software, 6 knows/created edges with weights."},
            {"kind": "large", "title": "Large generated graph", "available": true,
             "description": "The built-in generator of about 110k elements (the CLI's --graph large), for profiling and optimizer experiments."},
            {"kind": "tree", "title": "File tree", "available": true,
             "description": "A filesystem-like sample tree (37 vertices, 38 edges) for recursive traversals and glob_path()."},
        ])
    }

    /// Replaces the graph by a sample (`empty`, `modern`, `large`, `tree`).
    pub fn load_sample(&mut self, kind: &str) -> Result<Value, SessionError> {
        let (graph, name) = match kind {
            "empty" => (GraphSource::empty(), "Empty graph"),
            "modern" => (GraphSource::tinkerpop_modern(), "Modern (TinkerPop)"),
            "large" => (GraphSource::large_110k(), "Large generated graph"),
            "tree" => (GraphSource::file_tree(), "File tree"),
            other => return Err(format!("Unknown sample graph: {other}").into()),
        };
        let graph = self.factory.adopt(graph.into_inner())?;
        self.replace(graph, name, kind);
        self.info()
    }

    /// Replaces the graph by one imported from GraphML text ("Load graph"). The new graph starts
    /// WITHOUT the current graph's schema: GraphML carries none, and a previous graph's schema
    /// has nothing to do with an unrelated file. `schema` is the optional schema file (JSON text
    /// in the schema format): it is set on the new empty graph first and the data is imported
    /// against it as one unit, so the declared logical types (`uuid`, `array`, `object`) come
    /// back and `open`/`closed` are enforced (a violation fails the load). The saved queries (the
    /// catalog) are kept: GraphML carries none. On failure the current graph stays.
    pub fn load_graphml(
        &mut self,
        text: &[u8],
        name: &str,
        schema: Option<&str>,
    ) -> Result<Value, SessionError> {
        let schema = parse_schema_file(schema)?;
        let graph = import_with_schema(text, schema)?;
        crate::queries::carry_catalog(&mut *graph.write(), self.catalog()?)?;
        let graph = self.factory.adopt(graph.into_inner())?;
        self.replace(graph, name, "file");
        self.info()
    }

    /// Replaces the graph by one imported from GraphSON text (line or wrapped form). Like
    /// [`Self::load_graphml`], the new graph starts without the current graph's schema and
    /// `schema` (optional JSON text) is set first, the data imported against it as one unit.
    /// Data the import skips (dates, unsupported types, dangling edges) is listed in
    /// `info().warnings`. The saved queries (the catalog) are kept: GraphSON carries none. On
    /// failure the current graph stays.
    pub fn load_graphson(
        &mut self,
        text: &[u8],
        name: &str,
        schema: Option<&str>,
    ) -> Result<Value, SessionError> {
        let schema = parse_schema_file(schema)?;
        let (graph, report) = import_graphson_with_schema(text, schema)?;
        crate::queries::carry_catalog(&mut *graph.write(), self.catalog()?)?;
        let graph = self.factory.adopt(graph.into_inner())?;
        self.replace(graph, name, "file");
        self.warnings = report_warnings(&report);
        self.info()
    }

    /// Replaces the graph by one read from a packed snapshot (`.gsnap`): the lossless format,
    /// with its own schema, lineage id and commit position (the current schema is not kept).
    /// On failure (a damaged file) the current graph stays. Only a session over a
    /// [`graphersal::storage::PersistentStorage`] reads snapshots ([`StorageKind::persistent`]);
    /// any other fails with the "not supported by this storage" error.
    pub fn load_snapshot(&mut self, bytes: &[u8], name: &str) -> Result<Value, SessionError> {
        let mut reader = bytes;
        let graph = self.factory.load_snapshot(&mut reader)?;
        self.replace(graph, name, "file");
        // The embedding models the file carries go into the store in memory (format spec
        // 4.5): kept and handed on with it, even where they cannot run (the browser).
        if let Some(store) = self.active_store() {
            let mut packed = bytes;
            if let Err(err) = store.import_models_from_packed(&mut packed) {
                self.warnings.push(format!(
                    "the embedding models of the file were not kept: {}",
                    format_diagnostic(&err)
                ));
            }
        }
        self.info()
    }

    /// "Save graph" as a packed snapshot: the bytes of a `.gsnap` file (data, schema, lineage
    /// id, commit position), checked by reading them back.
    pub fn save_snapshot(&self) -> Result<Vec<u8>, SessionError> {
        let mut bytes = Vec::new();
        self.graph
            .write_snapshot(&mut bytes)
            .map_err(|err| format_diagnostic(&err))?;
        graphersal::persist::read_snapshot_info(bytes.as_slice())
            .map_err(|err| format_diagnostic(&err))?;
        Ok(bytes)
    }

    /// Sets the graph's schema from its JSON text in the schema format ("Load schema",
    /// `set_schema`). `mode` (`"none"`, `"open"`, `"closed"`) replaces the schema's own mode:
    /// the inferred schema with a mode freezes the structure the graph has. In mode
    /// `open`/`closed` the existing data is validated first; on violations nothing changes and
    /// the error carries the report. Returns the graph info.
    pub fn set_schema(&mut self, json: &str, mode: Option<&str>) -> Result<Value, SessionError> {
        let mut schema = GraphSchema::from_json(json).map_err(|err| format_diagnostic(&err))?;
        if let Some(name) = mode {
            schema =
                schema.with_mode(SchemaMode::parse(name).map_err(|err| format_diagnostic(&err))?);
        }
        self.graph
            .set_schema(schema)
            .map_err(|err| format_diagnostic(&*err))?;
        self.info()
    }

    /// `get_schema`: the stored schema (the schema format), or `null` without one. Never
    /// inferred. Fails when the graph cannot be read ([`SessionError::Graph`]).
    pub fn get_schema(&self) -> Result<Value, SessionError> {
        Ok(parse_schema_text(&self.get_schema_text()?))
    }

    /// [`Self::get_schema`] as JSON text in the canonical form, keys in the canonical order
    /// (property order kept), `null` without a stored schema. A [`Value`] would sort the keys.
    pub fn get_schema_text(&self) -> Result<String, SessionError> {
        Ok(match self.graph.get_schema()? {
            Some(schema) => schema.to_json(),
            None => "null".to_string(),
        })
    }

    /// `patch_schema`: applies a JSON Merge Patch to the stored schema and stores the result
    /// (validated against the data, like `set_schema`); returns the new schema.
    pub fn patch_schema(&mut self, patch: &str) -> Result<Value, SessionError> {
        self.patch_schema_text(patch)
            .map(|text| parse_schema_text(&text))
    }

    /// [`Self::patch_schema`] returning the new schema as canonical JSON text.
    pub fn patch_schema_text(&mut self, patch: &str) -> Result<String, SessionError> {
        let patch = SchemaPatch::from_json(patch).map_err(|err| format_diagnostic(&err))?;
        let schema = self
            .graph
            .patch_schema(&patch)
            .map_err(|err| format_diagnostic(&*err))?;
        Ok(schema.to_json())
    }

    /// `validate_schema`: `{violations, diff, compatible}` of a proposed schema against the
    /// data; nothing is applied.
    pub fn validate_schema(&self, json: &str) -> Result<Value, SessionError> {
        let schema = GraphSchema::from_json(json).map_err(|err| format_diagnostic(&err))?;
        let validation = self
            .graph
            .validate_schema(&schema)
            .map_err(|err| format_diagnostic(&*err))?;
        value_json(&validation.to_value())
    }

    /// The schema views (`{mode, source, table, json, mermaid, graph, truncated,
    /// total_labels}`, [`crate::views::schema_view_of`]) of the schema `json`, e.g. the
    /// editor's draft, with the graph's label counts. Nothing is stored or checked against data.
    pub fn schema_view_of(&self, json: &str) -> Result<Value, SessionError> {
        let schema = GraphSchema::from_json(json).map_err(|err| format_diagnostic(&err))?;
        Ok(self.graph.schema_view_of(schema)?)
    }

    /// `validate_schema_patch`: [`Self::validate_schema`] of the stored schema patched.
    pub fn validate_schema_patch(&self, patch: &str) -> Result<Value, SessionError> {
        let patch = SchemaPatch::from_json(patch).map_err(|err| format_diagnostic(&err))?;
        let validation = self
            .graph
            .validate_schema_patch(&patch)
            .map_err(|err| format_diagnostic(&*err))?;
        value_json(&validation.to_value())
    }

    /// `infer_schema`: the schema the data satisfies (mode `none`); stores nothing. Fails when
    /// the graph cannot be read ([`SessionError::Graph`]).
    pub fn infer_schema(&self) -> Result<Value, SessionError> {
        Ok(parse_schema_text(&self.infer_schema_text()?))
    }

    /// [`Self::infer_schema`] as canonical JSON text (keys in the canonical order).
    pub fn infer_schema_text(&self) -> Result<String, SessionError> {
        Ok(self.graph.inferred_schema()?.to_json())
    }

    /// `diff_schema`: `{changes, compatible}` from the stored schema to a proposed one.
    pub fn diff_schema(&self, json: &str) -> Result<Value, SessionError> {
        let schema = GraphSchema::from_json(json).map_err(|err| format_diagnostic(&err))?;
        let diff = self
            .graph
            .diff_schema(&schema)
            .map_err(|err| format_diagnostic(&*err))?;
        value_json(&diff.to_value())
    }

    /// Serves a graph that another owner keeps (the dev server's Store: its graph journals
    /// every commit; a host's own graph of any storage, [`IntoSessionGraph`]). Returns the
    /// graph info. The graph is served from then on; an error reading its info is returned
    /// (and the graph stays served).
    pub fn load_shared(
        &mut self,
        graph: impl IntoSessionGraph,
        name: &str,
        kind: &str,
    ) -> Result<Value, SessionError> {
        self.warnings.clear();
        self.graph = graph.into_session_graph();
        self.name = name.to_string();
        self.kind = kind.to_string();
        self.info()
    }

    /// The definitions of the current graph's catalog (saved queries, ...).
    fn catalog(&self) -> Result<Vec<Definition>, SessionError> {
        Ok(self.graph.definitions(None)?)
    }

    fn replace(&mut self, graph: Arc<dyn SessionGraph>, name: &str, kind: &str) {
        self.warnings.clear();
        self.graph = graph;
        self.name = name.to_string();
        self.kind = kind.to_string();
        if self.memory_stores {
            self.start_memory_store();
        } else {
            self.stores = None;
        }
    }

    /// `{name, kind, vertexCount, edgeCount, schemaMode, savedQueries, compressionRules,
    /// propertyIndexes, catalog, warnings}` of the loaded graph (`savedQueries`,
    /// `compressionRules`, `propertyIndexes`: how many of each its catalog holds; `catalog`:
    /// every kind's count by kind name). Fails when the graph cannot be read
    /// ([`SessionError::Graph`]).
    pub fn info(&self) -> Result<Value, SessionError> {
        let g = &self.graph;
        let definitions = g.definitions(None)?;
        let mut catalog = serde_json::Map::new();
        for definition in &definitions {
            let count = catalog
                .entry(definition.kind().to_string())
                .or_insert(json!(0));
            *count = json!(count.as_u64().unwrap_or(0) + 1);
        }
        Ok(json!({
            "name": self.name,
            "kind": self.kind,
            "vertexCount": g.vertex_count()?,
            "edgeCount": g.edge_count()?,
            "schemaMode": g.get_schema()?.map(|s| s.mode.as_str()),
            "savedQueries": definitions
                .iter()
                .filter(|d| d.kind() == DefinitionKind::QUERY)
                .count(),
            "compressionRules": definitions
                .iter()
                .filter(|d| d.kind() == DefinitionKind::COMPRESSION)
                .count(),
            "propertyIndexes": definitions
                .iter()
                .filter(|d| d.kind() == DefinitionKind::PROPERTY_INDEX)
                .count(),
            "catalog": catalog,
            "warnings": self.warnings,
        }))
    }

    /// The memory the process holds beside the graph: `{models: [{name, model, dimension,
    /// bytes, default}] }`, one entry per registered embedder that reports its size
    /// ([`graphersal::vector::Embedder::memory_bytes`]: a loaded model; the built-in
    /// `hash-ngram/1` and remote providers report none and are left out). A host adds its own
    /// members: `memory`, the whole process ([`crate::process::add_to_statistics`]; volatile,
    /// so `Session::statistics` itself stays deterministic), and the dev server's `aiChat`. Separate from `GraphStatistics`, which holds data
    /// counts only.
    pub fn process_memory(&self) -> Value {
        let embedders = &self.presets.embedders;
        let default = embedders.default_embedder().info().name.clone();
        let models: Vec<Value> = embedders
            .infos()
            .filter_map(|info| {
                let bytes = embedders.get(&info.name)?.memory_bytes()?;
                Some(json!({
                    "name": info.name,
                    "model": info.model,
                    "dimension": info.dimension,
                    "bytes": bytes,
                    "default": info.name == default,
                }))
            })
            .collect();
        json!({ "models": models })
    }

    /// The statistics panel: `{statistics, memory, process, limits: {presets, defaults}}`
    /// (`process`: [`Self::process_memory`]).
    /// `statistics` is [`crate::views::statistics_view`] (`g.statistics()`, `null` when the storage
    /// keeps none), `memory` the graph's own footprint ([`crate::views::memory_usage`]); `limits`
    /// are the session's host presets ([`SessionPresets::to_value`]) and, beside them, the
    /// engine's defaults of every traversal limit the presets do not set
    /// ([`graphersal::exec::traversal_limit_defaults`]: what a query runs under unless it sets the
    /// option itself). Limits are not data counts, so they stay out of `GraphStatistics`.
    /// Fails when the graph cannot be read ([`SessionError::Graph`]) or a limit default does not
    /// serialize.
    pub fn statistics(&self) -> Result<Value, SessionError> {
        let mut defaults = serde_json::Map::new();
        for (key, value) in graphersal::exec::traversal_limit_defaults() {
            if self.presets.options.iter().any(|(preset, _)| preset == key) {
                continue;
            }
            let value = serde_json::to_value(&value).map_err(|err| {
                SessionError::failed(format!("the default of {key} is not JSON: {err}"))
            })?;
            defaults.insert(key.to_string(), value);
        }
        Ok(json!({
            "statistics": self.graph.statistics_view()?,
            "memory": self.graph.memory_view()?,
            "process": self.process_memory(),
            "limits": {
                "presets": self.presets.to_value(),
                "defaults": defaults,
            },
        }))
    }

    /// The memory report ([`graphersal::storage::MemoryReport::to_property`] as JSON, snake_case
    /// keys like the DSL's `g.memory_report()`): `{ok: true, report}` or `{ok: false, error}`
    /// (an invalid spec, a cancelled report). `spec` is `{sample_every?, max_elements?,
    /// max_rows?, max_labels?, path_depth?}` (or `null`: a full report); cancellable through
    /// [`Self::cancel_token`]. Read-only.
    pub fn memory_report(&self, spec: &Value) -> Value {
        let answer = crate::views::memory_report_request(spec).and_then(|request| {
            let cancel = self.cancel_token();
            cancel.reset();
            self.graph
                .memory_report(&request.with_cancel(cancel))
                .map_err(|err| graphersal::error::format_diagnostic(&*err))
                .map_err(SessionError::from)
        });
        let answer = answer.and_then(|report| {
            serde_json::from_str::<Value>(&report.to_json()).map_err(|err| {
                SessionError::failed(format!("the memory report is not JSON: {err}"))
            })
        });
        match answer {
            Ok(report) => json!({"ok": true, "report": report}),
            Err(error) => json!({"ok": false, "error": error}),
        }
    }

    /// One element for the inspector, fetched when it is clicked (the drawings carry no
    /// properties), as JSON text: `kind` `"vertex"` or `"edge"`, `id` its id. `{kind, id, label,
    /// labels, properties, inDegree, outDegree}` for a vertex (the degrees count the edges in the
    /// whole graph, drawn or not), `{kind, id, label, source, target, properties}` for an
    /// edge, the properties in the engine's order; `null` when no element of that kind has the
    /// id (it was dropped since it was drawn). An unknown `kind` is an error.
    pub fn element(&self, kind: &str, id: &str) -> Result<String, SessionError> {
        let kind = match kind {
            "vertex" => ElementKind::Vertex,
            "edge" => ElementKind::Edge,
            other => {
                return Err(format!(
                    "unknown element kind \"{other}\": expected \"vertex\" or \"edge\""
                )
                .into());
            }
        };
        serde_json::to_string(&self.graph.element(kind, id)?)
            .map_err(|e| SessionError::failed(e.to_string()))
    }

    /// The whole graph to draw (`request`: JSON of [`crate::views::GraphViewRequest`],
    /// `{maxNodes?, maxEdges?, degrees?, filter?}`): the drawing's columns, with a filter
    /// ([`crate::filter`]) applied before the limits plus `matchedNodes` / `matchedEdges`, with
    /// `degrees: true` the `vertexInDegree` / `vertexOutDegree` columns. An invalid request or
    /// filter is an error naming the label, key and operator at fault.
    pub fn graph_view(&self, request: &str) -> Result<Value, SessionError> {
        let request: crate::views::GraphViewRequest = serde_json::from_str(request)
            .map_err(|e| format!("invalid graph view request: {e}"))?;
        let view = self.graph.graph_view_request(&request)?;
        serde_json::to_value(view)
            .map_err(|e| e.to_string())
            .map_err(SessionError::from)
    }

    /// The Rhai query that selects the vertices and edges the filter `filter` (JSON, see
    /// [`crate::filter`]) shows, for the Graph panel's "Open as query".
    pub fn graph_filter_query(&self, filter: &str) -> Result<String, SessionError> {
        let filter: Value =
            serde_json::from_str(filter).map_err(|e| format!("invalid filter: {e}"))?;
        self.graph.graph_filter_query(&filter)
    }

    /// The neighbourhood of vertices for the Graph panel (`request`: JSON of
    /// [`crate::selection::NeighbourhoodRequest`], `{ids, hops?, keep?, maxNodes?, maxEdges?}`):
    /// a [`crate::selection::SelectionView`] (the drawing's columns plus `seeds`, `missing`,
    /// `hiddenEdges`). Bounded by its limits; the session's cancellation token stops it (like
    /// [`Self::execute`], it resets the token when it starts).
    pub fn neighbourhood(&self, request: &str) -> Result<Value, SessionError> {
        let request: crate::selection::NeighbourhoodRequest = serde_json::from_str(request)
            .map_err(|e| format!("invalid neighbourhood request: {e}"))?;
        self.cancel.reset();
        let view = self.graph.neighbourhood(&request, &self.cancel)?;
        serde_json::to_value(view)
            .map_err(|e| e.to_string())
            .map_err(SessionError::from)
    }

    /// A sample of one vertex label or one label pair (`request`: JSON of
    /// [`crate::selection::SampleRequest`]) as a [`crate::selection::SelectionView`] with
    /// `sample: {kind, shown, total, complete}`. Bounded and cancellable like
    /// [`Self::neighbourhood`].
    pub fn label_sample(&self, request: &str) -> Result<Value, SessionError> {
        let request: crate::selection::SampleRequest =
            serde_json::from_str(request).map_err(|e| format!("invalid sample request: {e}"))?;
        self.cancel.reset();
        let view = self.graph.label_sample(&request, &self.cancel)?;
        serde_json::to_value(view)
            .map_err(|e| e.to_string())
            .map_err(SessionError::from)
    }

    /// The label meta-graph ([`crate::selection::MetaGraph`]): vertex labels with their counts
    /// and the label pairs of the edges with theirs. Cancellable like [`Self::neighbourhood`].
    pub fn meta_graph(&self) -> Result<Value, SessionError> {
        self.cancel.reset();
        let meta = self.graph.meta_graph(&self.cancel)?;
        serde_json::to_value(meta)
            .map_err(|e| e.to_string())
            .map_err(SessionError::from)
    }

    /// Starts a force layout of a drawing's topology (`request`: `{vertexCount, edgeSource,
    /// edgeTarget, seed?, iterations?}`, see [`crate::layout::LayoutRequest`]) and runs its first
    /// slice within `budget_ms`. Answers the job's status `{job, state, iterations,
    /// totalIterations, progress, elapsedMs, vertexCount, cached}` (`state` `running`, `done` or
    /// `cancelled`). A layout of the same topology and options as a recent one is answered from
    /// the cache (`state: done`, `cached: true`): the cache is keyed by what is drawn, so it
    /// survives graph changes that do not change the drawing and misses on every one that does.
    pub fn layout_start(
        &mut self,
        request: crate::layout::LayoutRequest,
        budget_ms: f64,
    ) -> Result<Value, SessionError> {
        let options = request.options();
        let job = self.layouts.start(request.input, options)?;
        self.layout_step(job, budget_ms)
    }

    /// [`Self::layout_start`] with the request as JSON text.
    pub fn layout_start_json(
        &mut self,
        request: &str,
        budget_ms: f64,
    ) -> Result<Value, SessionError> {
        let request =
            serde_json::from_str(request).map_err(|e| format!("invalid layout request: {e}"))?;
        self.layout_start(request, budget_ms)
    }

    /// Runs the next slice of layout `job` within `budget_ms` (clamped to
    /// [`crate::layout::MAX_SLICE_MS`]; at least one iteration). The session's cancellation token
    /// ([`Self::cancel_token`], the dev server's Stop) ends it as `cancelled`; like
    /// [`Self::execute`], a slice resets the token when it starts.
    pub fn layout_step(&mut self, job: u64, budget_ms: f64) -> Result<Value, SessionError> {
        self.cancel.reset();
        let budget = crate::layout::slice_budget(budget_ms);
        self.layouts.step(job, budget, &self.cancel)
    }

    /// Cancels layout `job`; it keeps the positions reached so far ([`Self::layout_positions`]).
    pub fn layout_cancel(&mut self, job: u64) -> Result<Value, SessionError> {
        self.layouts.cancel(job)
    }

    /// The positions of layout `job`: `x, y` per vertex, centred, the median edge 70 units long.
    /// A job that is no longer running is forgotten afterwards.
    pub fn layout_positions(&mut self, job: u64) -> Result<Vec<f32>, SessionError> {
        let status = self.layouts.status(job)?;
        let positions = self.layouts.positions(job)?;
        if status["state"] != "running" {
            self.layouts.finish(job);
        }
        Ok(positions)
    }

    /// The loaded graph, of the session's storage: the views ([`SessionGraph::schema_view`],
    /// [`SessionGraph::graph_view`], ...) and, through [`graphersal::storage::AnyGraph`], the
    /// commit position and the schema.
    pub fn graph(&self) -> &Arc<dyn SessionGraph> {
        &self.graph
    }

    /// The session's storage (its [`GraphFactory`]).
    pub fn factory(&self) -> &Arc<dyn GraphFactory> {
        &self.factory
    }

    /// The Rhai engine, for completions.
    pub fn engine(&self) -> &rhai::Engine {
        &self.engine
    }

    /// The values of the DSL's token classes for completion: `{class: [member, ...]}` (`Order` ->
    /// `["asc", "desc", "shuffle"]`), from the engine's registered getters
    /// ([`crate::views::token_values`], the CLI REPL's source too), restricted to the classes a
    /// script's scope defines (so `Execution`, `TraversalMetrics`, ... are left out).
    pub fn token_values(&self) -> Value {
        let any: Arc<dyn AnyGraph> = self.graph.clone();
        let scope = graph_scope_with_options(any, &[], playground_authorizer());
        let mut tokens = crate::views::token_values(&self.engine);
        tokens.retain(|class, _| scope.contains(class));
        json!(tokens)
    }

    /// Runs a script and returns the `ExecuteResponse` JSON (engine.js). A traversal left without
    /// a terminal runs through `execute(#{profile, profile_types})`; explicit terminals are
    /// honoured. Never fails: a failing script is reported in `execution.error`.
    ///
    /// The whole run is ONE unit of atomicity: it commits when the response is `ok` and is
    /// rolled back completely otherwise (also when a statement before the failing one changed
    /// the graph, and when the error comes from an `execute()` result the script returns).
    pub fn execute(&mut self, script: &str, options: &RunOptions) -> String {
        self.execute_with_authorizer(script, options, playground_authorizer())
    }

    /// [`Self::execute`] under another authorizer than the playground's: the dev server's MCP
    /// endpoint in read-only mode passes [`read_only_authorizer`], so a script that would write
    /// fails with a `PermissionDenied` diagnostic and changes nothing.
    pub fn execute_with_authorizer(
        &mut self,
        script: &str,
        options: &RunOptions,
        authorizer: Arc<dyn Authorizer>,
    ) -> String {
        let cancel = self.cancel.clone();
        cancel.reset();
        self.execute_with_cancel(script, options, authorizer, &cancel)
    }

    /// [`Self::execute_with_authorizer`] stopped by the caller's own `cancel` token instead of
    /// the session's ([`Self::cancel_token`]): a host that runs queries for several clients
    /// (the dev server's AI Chat conversations) cancels exactly the run it started, never
    /// another client's. The token is NOT reset here: a token cancelled before the run starts
    /// stops it at its first check.
    pub fn execute_with_cancel(
        &mut self,
        script: &str,
        options: &RunOptions,
        authorizer: Arc<dyn Authorizer>,
        cancel: &CancelToken,
    ) -> String {
        // The slot holds a plain token: a poisoned lock is used as it is (module docs of the
        // progress check above), so a run is always stoppable through its own token.
        let previous = std::mem::replace(
            &mut *self
                .run_cancel
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            cancel.clone(),
        );
        // The user's query counts in the workload statistics under the host's channel, even
        // where the host marks the session's other work (drawing, panels) as technical.
        let answer =
            graphersal::workload::scope(graphersal::workload::current_channel(), false, || {
                cancel.scope(|| self.execute_in_scope(script, options, authorizer))
            });
        *self
            .run_cancel
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = previous;
        // Without a checkpoint thread (wasm32) the automatic checkpoint runs here.
        self.run_due_checkpoint();
        answer
    }

    /// The host presets of this session.
    pub fn presets(&self) -> &SessionPresets {
        &self.presets
    }

    fn execute_in_scope(
        &mut self,
        script: &str,
        options: &RunOptions,
        authorizer: Arc<dyn Authorizer>,
    ) -> String {
        let started = web_time::Instant::now();
        *self
            .started
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(started);
        let render = options.render_options();
        let mut scope_options = self.presets.option_slice();
        scope_options.extend(render_scope_options(&render));
        let any: Arc<dyn AnyGraph> = self.graph.clone();
        let mut scope = graph_scope_with_options(any, &scope_options, authorizer);
        graphersal::script::set_scope_embedders(&mut scope, self.presets.embedders.clone());
        let hint = trailing_terminal(script);
        // Read before the run: a graph that cannot be read runs nothing.
        let before = self
            .graph
            .last_commit_seq()
            .and_then(|seq| Ok((seq, (self.graph.vertex_count()?, self.graph.edge_count()?))));
        let (seq_before, counts_before) = match before {
            Ok(before) => before,
            Err(err) => {
                let diagnostic = format_diagnostic(&TraverserError::from(err));
                let out = Outcome::failed("unknown", &error_of_diagnostic(&diagnostic));
                return self.response_of_refused_run(out, started);
            }
        };
        // The whole run is one unit (the host option of `book/src/users_guide/transactions.md`):
        // every statement and the implicit execution of a trailing traversal are savepoints of
        // it, and it commits only when the run is reported as `ok`, so a run that shows an
        // error leaves nothing behind. The session is single-threaded and owns the graph, so
        // nobody else writes between the statements.
        let mark = match self.graph.begin_run() {
            Ok(mark) => mark,
            Err(err) => {
                let diagnostic = format_diagnostic(&TraverserError::from(err));
                let out = Outcome::failed("unknown", &error_of_diagnostic(&diagnostic));
                return self.response_of_refused_run(out, started);
            }
        };
        // A panic in the run (an engine bug; the dev server catches it and keeps the session)
        // rolls the unit back, so the thread does not keep holding it.
        let unwind_guard = RunUnitGuard {
            graph: self.graph.clone(),
            mark: Some(mark),
        };
        let mut out = match self.engine.eval_with_scope::<Dynamic>(&mut scope, script) {
            Ok(value) => self.classify(value, hint, options, &render),
            Err(err) => Outcome::failed(
                "unknown",
                &script_error(
                    &graphersal::script::map_eval_error(err, &self.presets.limits),
                    &self.presets.limits,
                ),
            ),
        };
        drop(scope);
        unwind_guard.disarm();
        let mut changes = Changes::default();
        let mut change_summary = None;
        // The first failed read after the run: it turns the answer into an error (the
        // `changes` and `mutated` fields still say what was committed), never an empty part.
        let mut read_error: Option<GraphError> = None;
        if out.ok {
            let committed = self.graph.run_changes(mark).and_then(|unit| {
                changes = Changes {
                    data: unit.data,
                    schema: unit.schema,
                    definitions: unit.definitions,
                };
                self.graph.commit_run(mark)
            });
            if let Err(err) = committed {
                // A failed commit rolled the unit back itself (`UnitNotHeld` of another thread
                // has nothing to roll back).
                changes = Changes::default();
                let diagnostic = format_diagnostic(&TraverserError::from(err));
                out = Outcome::failed(out.terminal, &error_of_diagnostic(&diagnostic));
            } else if changes.data {
                let delta = |after: usize, before: usize| after as i64 - before as i64;
                match (self.graph.vertex_count(), self.graph.edge_count()) {
                    (Ok(vertices), Ok(edges)) => {
                        change_summary = Some(ChangeSummary::of(
                            delta(vertices, counts_before.0),
                            delta(edges, counts_before.1),
                        ));
                    }
                    (Err(err), _) | (_, Err(err)) => read_error = Some(err),
                }
            }
        } else if let Err(err) = self
            .graph
            .rollback_run(mark, &RollbackReason::failed(&"the playground run failed"))
        {
            read_error.get_or_insert(err);
        }
        let mut subgraph = match self.graph.subgraph(
            &out.results,
            options
                .subgraph_max_elements
                .unwrap_or(crate::display::DEFAULT_SUBGRAPH_MAX_ELEMENTS),
        ) {
            Ok(subgraph) => subgraph,
            Err(err) => {
                read_error.get_or_insert(err);
                None
            }
        };
        if options.subgraph_degrees
            && let Some(view) = subgraph.as_mut()
            && let Err(err) = self.graph.add_degrees(view)
        {
            read_error.get_or_insert(err);
        }
        let mutated = match self.graph.last_commit_seq() {
            Ok(seq) => seq > seq_before,
            Err(err) => {
                read_error.get_or_insert(err);
                false
            }
        };
        let graph_info = match self.info() {
            Ok(info) => info,
            Err(err) => {
                if read_error.is_none() {
                    out = Outcome::failed(out.terminal, &error_of_diagnostic(&err.to_string()));
                }
                Value::Null
            }
        };
        if let Some(err) = read_error {
            let diagnostic = format_diagnostic(&TraverserError::from(err));
            out = Outcome::failed(out.terminal, &error_of_diagnostic(&diagnostic));
        }
        let response = Response {
            ok: out.ok,
            terminal: out.terminal,
            implicit: out.implicit,
            mutated,
            changes,
            change_summary,
            execution: RawValue::from_string(out.execution).unwrap_or_else(|_| null_raw()),
            display: out.display,
            subgraph,
            preferred_view: out.preferred_view,
            elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
            graph: graph_info.clone(),
        };
        serde_json::to_string(&response).unwrap_or_else(|err| {
            json!({"ok": false, "terminal": "unknown", "implicit": false, "mutated": false,
                   "changes": {"data": false, "schema": false, "definitions": false},
                   "execution": {"results": [], "error": {"message": err.to_string(),
                   "diagnostic": err.to_string()}, "profile": null},
                   "display": null, "subgraph": null, "elapsed_ms": 0, "graph": graph_info})
            .to_string()
        })
    }

    /// The answer to a run the graph refused to open a unit for (`begin_run` failed: a view of
    /// the graph alive on this thread); nothing ran, nothing changed.
    fn response_of_refused_run(&self, out: Outcome, started: web_time::Instant) -> String {
        // The answer is already the refusal: a graph whose info cannot be read either is shown
        // without it (`graph: null`), the refusal names the cause.
        let graph_info = self.info().unwrap_or(Value::Null);
        let response = Response {
            ok: out.ok,
            terminal: out.terminal,
            implicit: out.implicit,
            mutated: false,
            changes: Changes::default(),
            change_summary: None,
            execution: RawValue::from_string(out.execution).unwrap_or_else(|_| null_raw()),
            display: out.display,
            subgraph: None,
            preferred_view: out.preferred_view,
            elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
            graph: graph_info.clone(),
        };
        serde_json::to_string(&response).unwrap_or_else(|err| {
            json!({"ok": false, "terminal": "unknown", "implicit": false, "mutated": false,
                   "changes": {"data": false, "schema": false, "definitions": false},
                   "execution": {"results": [], "error": {"message": err.to_string(),
                   "diagnostic": err.to_string()}, "profile": null},
                   "display": null, "subgraph": null, "elapsed_ms": 0, "graph": graph_info})
            .to_string()
        })
    }

    fn classify(
        &self,
        value: Dynamic,
        hint: Option<&'static str>,
        options: &RunOptions,
        render: &RenderOptions,
    ) -> Outcome {
        // A graph value (`g.e(0).to_graph()`, `cap("sg").next()`, `g`): one result
        // `{"vertices": [..], "edges": [..]}`, shown like `cap("sg")` shows a graph inside a
        // traversal (the CLI renders the same value through `try_render_value`). Only
        // `max_items + 1` elements of each kind are read, so a huge graph is never listed whole.
        if let Some(graph) = value
            .read_lock::<ScriptGraphTraversalSource>()
            .filter(|source| source.is_graph_value())
            .map(|source| source.graph_listing(render.max_items.map(|n| n.saturating_add(1))))
        {
            let listing = match graph {
                Ok(listing) => listing,
                Err(err) => {
                    let diagnostic = format_diagnostic(&err);
                    return Outcome::failed(
                        hint.unwrap_or("value"),
                        &error_of_diagnostic(&diagnostic),
                    );
                }
            };
            let results = vec![listing];
            let mut display =
                bounded_display_at(&results, true, &Bounds::of(render), offset_of(options));
            match try_render_value(value, VisualizeFormat::Auto, render) {
                Ok(rendered) => display.raw = rendered.text,
                Err(diagnostic) => display.raw = diagnostic,
            }
            let results_json = values_json(&results);
            return Outcome {
                ok: true,
                terminal: hint.unwrap_or("value"),
                implicit: false,
                execution: execution_json(&results_json, "null", "null"),
                results,
                display: Some(display),
                preferred_view: None,
            };
        }

        // A traversal without a terminal: run it as `execute(#{profile, profile_types})`.
        let value = match value.try_cast_result::<ScriptGraphTraversalSource>() {
            Ok(mut traversal) => {
                let mut opts = rhai::Map::new();
                opts.insert("profile".into(), Dynamic::from_bool(options.profile));
                if options.profile && !options.profile_types.is_empty() {
                    let types: rhai::Array = options
                        .profile_types
                        .iter()
                        .map(|t| Dynamic::from(t.clone()))
                        .collect();
                    opts.insert("profile_types".into(), Dynamic::from_array(types));
                }
                let exec = traversal.execute_dynamic(Dynamic::from_map(opts));
                let mut out = execution_outcome(exec, render, offset_of(options));
                out.implicit = true;
                return out;
            }
            Err(value) => value,
        };

        let value = match value.try_cast_result::<ScriptExecution>() {
            Ok(exec) => {
                return execution_outcome(exec, render, offset_of(options));
            }
            Err(value) => value,
        };

        // TinkerPop's `profile()`: the value IS the metrics.
        let value = match value.try_cast_result::<TraversalMetrics>() {
            Ok(metrics) => {
                let profile_text = metrics.to_json();
                let profile: Value = serde_json::from_str(&profile_text).unwrap_or(Value::Null);
                let display = Display {
                    table: metrics_table(&profile),
                    json: serde_json::to_string_pretty(&profile).unwrap_or_default(),
                    raw: metrics.to_string(),
                    notice: None,
                    total_rows: None,
                    offset: 0,
                    shown_rows: None,
                    items_cut: false,
                };
                return Outcome {
                    ok: true,
                    terminal: "profile",
                    implicit: false,
                    execution: execution_json(&format!("[{profile_text}]"), "null", &profile_text),
                    results: Vec::new(),
                    display: Some(display),
                    preferred_view: None,
                };
            }
            Err(value) => value,
        };

        // A schema (`g.get_schema()`, `g.infer_schema()`, ...): the schema table.
        if let Some(schema) = schema_of_value(&value) {
            let display = Display {
                table: crate::views::schema_table(&schema),
                json: schema.to_json_pretty(),
                raw: schema.visualize(VisualizeFormat::Table),
                notice: None,
                total_rows: None,
                offset: 0,
                shown_rows: None,
                items_cut: false,
            };
            return Outcome {
                ok: true,
                terminal: "value",
                implicit: false,
                execution: execution_json(&format!("[{}]", schema.to_json()), "null", "null"),
                results: Vec::new(),
                display: Some(display),
                preferred_view: None,
            };
        }

        // An explicit visualizer (`to_table()`, `to_json()`, `visualize(..)`): text.
        if value.is::<RenderedText>() {
            let terminal = hint.unwrap_or("value");
            let rendered = match try_render_value(value, VisualizeFormat::Auto, render) {
                Ok(rendered) => rendered,
                Err(diagnostic) => {
                    return Outcome::failed(terminal, &error_of_diagnostic(&diagnostic));
                }
            };
            let notice = rendered.notice();
            let text = rendered.text;
            let is_json = serde_json::from_str::<serde::de::IgnoredAny>(&text).is_ok();
            let table = Table {
                columns: vec!["text".to_string()],
                rows: text
                    .lines()
                    .map(|line| vec![ElementProperty::from(line)])
                    .collect(),
            };
            let display = Display {
                table,
                json: if is_json {
                    text.clone()
                } else {
                    serde_json::to_string(&text).unwrap_or_default()
                },
                raw: text.clone(),
                notice,
                total_rows: None,
                offset: 0,
                shown_rows: None,
                items_cut: false,
            };
            let results = format!("[{}]", serde_json::to_string(&text).unwrap_or_default());
            return Outcome {
                ok: true,
                terminal,
                implicit: false,
                execution: execution_json(&results, "null", "null"),
                results: Vec::new(),
                display: Some(display),
                preferred_view: Some(if terminal == "to_json" { "json" } else { "raw" }),
            };
        }

        // Any other script value: `to_list()`, `next()`, a map, a scalar, `()`.
        let raw = try_render_value(value.clone(), VisualizeFormat::Auto, render);
        let is_list = value.is_array();
        let results: Vec<ElementProperty> = if value.is_unit() {
            Vec::new()
        } else {
            match script_value_to_property(value, self.presets.limits.max_value_depth) {
                Ok(ElementProperty::Array(items)) if is_list => items,
                Ok(other) => vec![other],
                Err(err) => {
                    let diagnostic = format_diagnostic(&err);
                    return Outcome::failed(
                        hint.unwrap_or("value"),
                        &error_of_diagnostic(&diagnostic),
                    );
                }
            }
        };
        let terminal = hint.unwrap_or(if is_list { "to_list" } else { "value" });
        let offset = offset_of(options);
        let mut display = bounded_display_at(&results, is_list, &Bounds::of(render), offset);
        match raw {
            Ok(rendered) => {
                // The visualizer's notice speaks of the first rows: not of a later page.
                if let Some(notice) = rendered.notice().filter(|_| offset == 0 || !is_list) {
                    display.notice = Some(notice);
                }
                display.raw = rendered.text;
            }
            Err(diagnostic) => display.raw = diagnostic,
        }
        let results_json = values_json(&results);
        Outcome {
            ok: true,
            terminal,
            implicit: false,
            execution: execution_json(&results_json, "null", "null"),
            results,
            display: Some(display),
            preferred_view: None,
        }
    }

    /// "Save graph": `{text, schema, warnings}`. `text` is the graph as GraphML (which carries
    /// no schema), `schema` the stored schema as JSON text in the schema format, or `null`
    /// when the graph stores none. The result is checked by loading it back the way the
    /// playground does (the schema first, then the GraphML): a warning names a property whose
    /// logical type does not survive (an undeclared `uuid`, `array` or `object` comes back as a
    /// string, a key with different types on different labels), or says why loading fails.
    pub fn save_graphml(&self) -> Result<Value, SessionError> {
        let mut buf = Vec::new();
        self.graph
            .export_graphml(&mut buf, true)
            .map_err(|err| format_diagnostic(&err))?;
        let text = String::from_utf8(buf).map_err(|err| err.to_string())?;
        let stored = self.graph.get_schema()?;
        let schema_json = stored.as_ref().map(GraphSchema::to_json_pretty);
        let warnings = match import_with_schema(text.as_bytes(), stored) {
            Err(err) => vec![format!(
                "Loading the saved files back fails: {}",
                first_line(&err.to_string())
            )],
            Ok(loaded) => type_drift(
                &self.graph.inferred_schema()?,
                &loaded.read().infer_schema(),
            ),
        };
        Ok(json!({"text": text, "schema": schema_json, "warnings": warnings}))
    }
}

impl Session {
    /// "Save graph" as GraphSON: `{text, schema, warnings}`. `text` is the graph as GraphSON 3.0
    /// in the line form (which keeps `uuid`, `array` and `object` values typed and carries no
    /// schema), `schema` the stored schema JSON text or `null`. A warning says why loading the
    /// files back (schema first) would fail.
    pub fn save_graphson(&self) -> Result<Value, SessionError> {
        let mut buf = Vec::new();
        self.graph
            .export_graphson(&mut buf)
            .map_err(|err| format_diagnostic(&err))?;
        let text = String::from_utf8(buf).map_err(|err| err.to_string())?;
        let stored = self.graph.get_schema()?;
        let schema_json = stored.as_ref().map(GraphSchema::to_json_pretty);
        let warnings = match import_graphson_with_schema(text.as_bytes(), stored) {
            Err(err) => vec![format!(
                "Loading the saved files back fails: {}",
                first_line(&err.to_string())
            )],
            Ok((loaded, report)) => {
                let mut warnings = report_warnings(&report);
                warnings.extend(type_drift(
                    &self.graph.inferred_schema()?,
                    &loaded.read().infer_schema(),
                ));
                warnings
            }
        };
        Ok(json!({"text": text, "schema": schema_json, "warnings": warnings}))
    }
}

/// One warning line per kind of data a GraphSON import skipped.
fn report_warnings(report: &graphersal::io::ImportReport) -> Vec<String> {
    report
        .notes
        .iter()
        .filter(|note| note.kind.is_skip())
        .map(|note| {
            let mut line = format!("GraphSON import {}: {}", note.kind, note.count);
            if !note.samples.is_empty() {
                line.push_str(&format!(" (e.g. {})", note.samples.join("; ")));
            }
            line
        })
        .collect()
}

/// A new graph that stores `schema` (if any), with the GraphSON `text` imported into it.
fn import_graphson_with_schema(
    text: &[u8],
    schema: Option<GraphSchema>,
) -> Result<(Graph, graphersal::io::ImportReport), SessionError> {
    let Some(schema) = schema else {
        // A graph nobody sees yet: loaded outside units (no undo log, no change capture).
        return graphersal::io::GraphSONImport::new()
            .load(Cursor::new(text))
            .map_err(|err| format_diagnostic(&err))
            .map_err(SessionError::from);
    };
    let mut graph = GraphSource::empty();
    graph
        .write()
        .set_schema(schema)
        .map_err(|err| format_diagnostic(&err))?;
    let report = graph
        .import_graphson_reader(Cursor::new(text))
        .map_err(|err| format_diagnostic(&err))?;
    Ok((graph, report))
}

/// What a script produced, before it becomes the response.
struct Outcome {
    ok: bool,
    terminal: &'static str,
    implicit: bool,
    /// The `Execution` JSON text: `{results, error, profile}`.
    execution: String,
    /// The results as data, to find the elements they refer to.
    results: Vec<ElementProperty>,
    display: Option<Display>,
    preferred_view: Option<&'static str>,
}

impl Outcome {
    fn failed(terminal: &'static str, error: &Value) -> Self {
        Self {
            ok: false,
            terminal,
            implicit: false,
            execution: execution_json("[]", &error.to_string(), "null"),
            results: Vec::new(),
            display: None,
            preferred_view: None,
        }
    }
}

/// What a playground script may do: everything except file access
/// (`AccessPolicy::allow_all().deny_resource(ResourceKind::File)`). The playground is a local
/// front end over the user's own graph; the browser has no file system (graphs are loaded and
/// saved through the page, not by the script), so `export_graphml`, `import_graphml`,
/// `export_graphson`, `import_graphson`, `GraphSource::file` and `import` fail with a clear
/// `PermissionDenied` instead of an I/O error.
pub fn playground_authorizer() -> Arc<dyn Authorizer> {
    Arc::new(AccessPolicy::allow_all().deny_resource(ResourceKind::File))
}

/// The playground's authorizer without writes: file access, and creating, updating or deleting
/// data, the schema or catalog definitions (saved queries, compression rules) are denied;
/// reading, calling saved queries and options are allowed. A query that would write
/// fails with `PermissionDenied` before it runs a step, and changes nothing.
pub fn read_only_authorizer() -> Arc<dyn Authorizer> {
    let mut policy = AccessPolicy::allow_all().deny_resource(ResourceKind::File);
    for resource in [
        ResourceKind::Data,
        ResourceKind::Schema,
        ResourceKind::Definition,
    ] {
        for action in [ActionKind::Create, ActionKind::Update, ActionKind::Delete] {
            policy = policy.deny(action, resource);
        }
    }
    Arc::new(policy)
}

fn null_raw() -> Box<RawValue> {
    RawValue::from_string("null".to_string()).unwrap_or_default()
}

/// `{"results": .., "error": .., "profile": ..}` from JSON text parts (the Rust map shape).
fn execution_json(results: &str, error: &str, profile: &str) -> String {
    format!(r#"{{"results":{results},"error":{error},"profile":{profile}}}"#)
}

/// The response parts of an `Execution`: its own JSON (`{results, error, profile}`, the Rust
/// map shape, keys in the engine's order), and the results rendered within the display limits.
/// The display offset of a run (`RunOptions::offset`, default 0).
fn offset_of(options: &RunOptions) -> usize {
    options.offset.unwrap_or(0)
}

fn execution_outcome(mut exec: ScriptExecution, render: &RenderOptions, offset: usize) -> Outcome {
    let ok = exec.is_ok();
    let execution = match exec.to_json() {
        Ok(text) => text,
        Err(err) => {
            let error = error_of_diagnostic(&err.to_string());
            execution_json("[]", &error.to_string(), "null")
        }
    };
    // The execution's own result values keep the engine's key order (script maps are sorted by
    // key) and their kind: a vertex or an edge stays one, so the table shows its id and label
    // apart from its properties and the result graph finds it.
    let results: Vec<ElementProperty> = if ok {
        match exec.result_values() {
            Ok(values) => values,
            // The values that the execution reported as its results cannot be read back: the
            // run's answer is that error, never an empty result list.
            Err(err) => {
                return Outcome::failed("execute", &error_of_diagnostic(&err.to_string()));
            }
        }
    } else {
        Vec::new()
    };
    let display = ok.then(
        || match exec.render_results(VisualizeFormat::Auto, render) {
            Ok(rendered) => {
                // The visualizer applied the session limits (including the query's own
                // `g.with("render.max_rows", n)`): show exactly the rows and items it showed.
                let bounds = match &rendered.truncation {
                    Some(cut) => Bounds {
                        max_rows: Some(cut.rows_shown),
                        max_items: cut.max_items,
                    },
                    None => Bounds::unlimited(),
                };
                let mut display = bounded_display_at(&results, true, &bounds, offset);
                // The visualizer's notice speaks of the first rows: not of a later page.
                if offset == 0 {
                    display.notice = rendered.notice();
                }
                display.raw = rendered.text;
                display
            }
            Err(diagnostic) => {
                let mut display = bounded_display_at(&results, true, &Bounds::of(render), offset);
                display.raw = diagnostic;
                display
            }
        },
    );
    Outcome {
        ok,
        terminal: "execute",
        implicit: false,
        execution,
        results,
        display,
        preferred_view: None,
    }
}

/// A new graph that stores `schema` (if any), with the GraphML `text` imported into it: the
/// order "schema first, then the graph" that restores the declared logical types.
/// The optional schema file of a graph load, parsed (`None`: the graph gets no schema). An
/// error names the schema file, so it is not mistaken for a problem of the graph file.
fn parse_schema_file(schema: Option<&str>) -> Result<Option<GraphSchema>, SessionError> {
    schema
        .map(|json| {
            GraphSchema::from_json(json)
                .map_err(|err| format!("Schema file: {}", format_diagnostic(&err)))
        })
        .transpose()
        .map_err(SessionError::from)
}

fn import_with_schema(text: &[u8], schema: Option<GraphSchema>) -> Result<Graph, SessionError> {
    let Some(schema) = schema else {
        // A graph nobody sees yet: loaded outside units (no undo log, no change capture).
        return GraphSource::from_graphml_reader(Cursor::new(text))
            .map_err(|err| format_diagnostic(&err))
            .map_err(SessionError::from);
    };
    let mut graph = GraphSource::empty();
    graph
        .write()
        .set_schema(schema)
        .map_err(|err| format_diagnostic(&err))?;
    graph
        .import_graphml_reader(Cursor::new(text))
        .map_err(|err| format_diagnostic(&err))?;
    Ok(graph)
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or_default()
}

/// The metrics of a `profile()` result as display rows, one per step, nested steps indented.
fn metrics_table(profile: &Value) -> Table {
    fn walk(list: &Value, depth: usize, rows: &mut Vec<Vec<ElementProperty>>) {
        let number = |v: &Value| {
            v.as_i64()
                .map(ElementProperty::from)
                .or_else(|| v.as_f64().map(ElementProperty::from))
                .unwrap_or(ElementProperty::Null)
        };
        for m in list.as_array().into_iter().flatten() {
            rows.push(vec![
                ElementProperty::from(format!(
                    "{}{}",
                    "  ".repeat(depth),
                    m["name"].as_str().unwrap_or_default()
                )),
                number(&m["calls"]),
                number(&m["count_in"]),
                number(&m["counts"]["traverser_count"]),
                number(&m["duration_ns"]),
                number(&m["percent_duration"]),
            ]);
            walk(&m["nested"], depth + 1, rows);
        }
    }
    let mut rows = Vec::new();
    walk(&profile["metrics"], 0, &mut rows);
    Table {
        columns: [
            "step",
            "calls",
            "count_in",
            "traverser_count",
            "duration_ns",
            "percent_duration",
        ]
        .map(String::from)
        .to_vec(),
        rows,
    }
}

/// The diagnostic of a run stopped by [`SessionPresets::script_timeout_ms`] outside a traversal.
fn script_timeout_diagnostic(limit: std::time::Duration, elapsed: std::time::Duration) -> String {
    format!(
        "Error: Script exceeded evaluationTimeout of {} ms (ran {} ms)\n\
         Help: The script itself ran past the session's budget between traversals, for example \
         in a loop; everything it changed was rolled back. Bound the loop, or raise the budget \
         (the dev server: --timeout <ms>).",
        limit.as_millis(),
        elapsed.as_millis()
    )
}

/// The error map of a script that failed outside an `execute()`: `{message, help?, diagnostic,
/// position?}` (`position` 1-based, when Rhai knows it).
pub(crate) fn script_error(err: &EvalAltResult, limits: &ScriptLimits) -> Value {
    let inner = err.unwrap_inner();
    let diagnostic = if let Some(limit) = graphersal::script::resource_limit_from_rhai(err, limits)
    {
        format_diagnostic(&limit)
    } else {
        match inner {
            EvalAltResult::ErrorRuntime(d, ..) | EvalAltResult::ErrorTerminated(d, ..)
                if d.is_string() =>
            {
                d.to_string()
            }
            _ => format!("Error: {inner}"),
        }
    };
    let mut error = error_of_diagnostic(&diagnostic);
    let position = err.position();
    if let (Some(line), Some(column)) = (position.line(), position.position()) {
        error["position"] = json!({"line": line, "column": column});
    }
    error
}

/// Splits a rendered diagnostic (`Error: ...` lines, then `Help: ...` with continuation lines
/// indented by six spaces) into `{message, help?, diagnostic}`.
fn error_of_diagnostic(diagnostic: &str) -> Value {
    let (head, help) = match diagnostic.find("\nHelp: ") {
        Some(idx) => {
            let help = diagnostic[idx + 1..]
                .lines()
                .enumerate()
                .map(|(i, line)| {
                    if i == 0 {
                        line.trim_start_matches("Help: ")
                    } else {
                        line.strip_prefix("      ").unwrap_or(line)
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            (&diagnostic[..idx], Some(help))
        }
        None => (diagnostic, None),
    };
    let message = first_line(head.trim_start())
        .trim_start_matches("Error: ")
        .to_string();
    let mut error = json!({"message": message, "diagnostic": diagnostic});
    if let Some(help) = help {
        error["help"] = Value::String(help);
    }
    error
}

/// The terminal a script ends with (`g.v().to_list()` -> `to_list`), from the text of its last
/// call; `None` when it does not end in one of the known terminals.
pub fn trailing_terminal(script: &str) -> Option<&'static str> {
    let code = script.trim_end().trim_end_matches(';').trim_end();
    let bytes = code.as_bytes();
    if bytes.last() != Some(&b')') {
        return None;
    }
    let mut depth = 0usize;
    let mut open = None;
    for (i, b) in bytes.iter().enumerate().rev() {
        match b {
            b')' => depth += 1,
            b'(' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    open = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let before = code[..open?].trim_end();
    let name_start = before
        .rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .map_or(0, |i| i + 1);
    if !before[..name_start].trim_end().ends_with('.') {
        return None;
    }
    Some(match &before[name_start..] {
        "to_list" | "toList" => "to_list",
        "next" => "next",
        "iterate" => "iterate",
        "profile" | "profile_with" => "profile",
        "execute" => "execute",
        "to_table" | "toTable" => "to_table",
        "to_json" | "toJson" => "to_json",
        "to_markdown" | "toMarkdown" => "to_markdown",
        "visualize" => "visualize",
        _ => return None,
    })
}

/// Schema JSON text as a [`Value`] (for the native callers and tests; the key order is lost).
fn parse_schema_text(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or(Value::Null)
}

/// An engine value (a validation report, a diff) as a JSON value.
fn value_json(value: &ElementProperty) -> Result<Value, SessionError> {
    serde_json::to_value(json_safe(value))
        .map_err(|err| err.to_string())
        .map_err(SessionError::from)
}

/// Properties whose logical type differs between the saved graph and the graph loaded back from
/// the saved file, as readable warnings (at most 10).
fn type_drift(saved: &GraphSchema, loaded: &GraphSchema) -> Vec<String> {
    let mut out = Vec::new();
    let mut compare = |kind: &str, label: &str, key: &str, a: &str, b: Option<String>| {
        if b.as_deref() != Some(a) && out.len() < 10 {
            let b_text = b.unwrap_or_else(|| "missing".to_string());
            out.push(format!(
                "{kind} '{label}', property '{key}': saved as {a}, loads back as {b_text}. \
                 Declare it in the schema (saved as schema.json; load it before the graph) or \
                 store it as a string to keep it exact."
            ));
        }
    };
    for (label, v) in &saved.vertices {
        for (key, node) in &v.schema.properties {
            let other = loaded
                .vertices
                .get(label)
                .and_then(|l| l.schema.properties.get(key))
                .map(PropertySchema::type_name);
            compare("vertex label", label, key, &node.type_name(), other);
        }
    }
    for (label, e) in &saved.edges {
        for (key, node) in &e.schema.properties {
            let other = loaded
                .edges
                .get(label)
                .and_then(|l| l.schema.properties.get(key))
                .map(PropertySchema::type_name);
            compare("edge label", label, key, &node.type_name(), other);
        }
    }
    out
}

/// Rolls a run's unit back when the run unwinds before closing it
/// (`Session::execute_in_scope`); disarmed once the run closes the unit itself.
struct RunUnitGuard {
    graph: Arc<dyn SessionGraph>,
    mark: Option<graphersal::storage::UnitMark>,
}

impl RunUnitGuard {
    /// The run closes its unit itself from here on.
    fn disarm(mut self) {
        self.mark = None;
    }
}

impl Drop for RunUnitGuard {
    fn drop(&mut self) {
        if let Some(mark) = self.mark.take() {
            // Unwinding: the panic is what the host reports (the dev server catches it); a
            // failing rollback here (`UnitNotHeld` of another thread) has no caller to go to.
            let _ = self.graph.rollback_run(
                mark,
                &RollbackReason::failed(&"a panic unwound through the playground run"),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_with(session: &mut Session, script: &str, options: &RunOptions) -> Value {
        serde_json::from_str(&session.execute(script, options)).expect("response JSON")
    }

    fn run(session: &mut Session, script: &str) -> Value {
        run_with(
            session,
            script,
            &RunOptions {
                profile: true,
                ..RunOptions::default()
            },
        )
    }

    fn modern() -> Session {
        let mut session = Session::new();
        session.load_sample("modern").unwrap();
        session
    }

    fn double(text: &str) -> Value {
        json!({"@type": "g:Double", "@value": text})
    }

    /// A non-finite float reaches the page as GraphSON's typed double through every result
    /// path (a terminal, a traversal without one, `execute()`), in the results, the Table tab and
    /// the JSON tab, at any depth and as a map key.
    #[test]
    fn non_finite_floats_are_typed_doubles_in_results() {
        let mut session = modern();
        let nan = double("NaN");
        let inf = double("Infinity");
        let ninf = double("-Infinity");
        for script in [
            "g.inject(0.0 / 0.0, 1.0 / 0.0, -1.0 / 0.0).to_list()",
            "g.inject(0.0 / 0.0, 1.0 / 0.0, -1.0 / 0.0)",
            "g.inject(0.0 / 0.0, 1.0 / 0.0, -1.0 / 0.0).execute()",
        ] {
            let response = run(&mut session, script);
            assert_eq!(response["ok"], true, "{script}: {response}");
            assert_eq!(
                response["execution"]["results"],
                json!([nan, inf, ninf]),
                "{script}"
            );
            assert_eq!(
                response["display"]["table"]["rows"],
                json!([[nan], [inf], [ninf]]),
                "{script}"
            );
            let shown: Value =
                serde_json::from_str(response["display"]["json"].as_str().unwrap()).unwrap();
            assert_eq!(shown, json!([nan, inf, ninf]), "{script}");
        }
        // Nested in a list and a map, a map key, and a string "NaN" stays a string.
        let response = run(
            &mut session,
            r#"g.inject(#{a: [1.5, 0.0 / 0.0], s: "NaN"}).to_list()"#,
        );
        assert_eq!(
            response["execution"]["results"],
            json!([{"a": [1.5, nan], "s": "NaN"}])
        );
        let response = run(&mut session, "g.inject(1.0 / 0.0).group_count().next()");
        assert_eq!(response["ok"], true, "{response}");
        assert_eq!(
            response["execution"]["results"][0],
            json!({"@type": "g:Map", "@value": [inf, 1]})
        );
    }

    /// The inspector shows a stored non-finite property as its typed double.
    #[test]
    fn the_inspector_types_a_stored_non_finite_property() {
        let mut session = Session::new();
        let response = run(
            &mut session,
            r#"g.add_v("x").property(T.id, "a").property("w", 0.0 / 0.0).property("xs", [-1.0 / 0.0, 2.0]).to_list()"#,
        );
        assert_eq!(response["ok"], true, "{response}");
        assert_eq!(
            response["execution"]["results"][0]["properties"]["w"],
            double("NaN")
        );
        let element: Value =
            serde_json::from_str(&session.element("vertex", "a").unwrap()).unwrap();
        assert_eq!(element["properties"]["w"], double("NaN"));
        assert_eq!(
            element["properties"]["xs"],
            json!([double("-Infinity"), 2.0])
        );
    }

    /// The token values come from the engine and cover at least what the playground's former
    /// hard-coded list (`TOKEN_VALUES` in `js/editor/completion.js`) offered; classes that are
    /// not scope globals (`Execution`, `TraversalMetrics`) are left out.
    #[test]
    fn token_values_cover_the_former_playground_list() {
        let session = modern();
        let tokens = session.token_values();
        let former: &[(&str, &[&str])] = &[
            ("T", &["id", "label"]),
            ("Order", &["asc", "desc", "shuffle"]),
            ("Scope", &["local", "global"]),
            ("Column", &["keys", "values"]),
            ("ProfileType", &["Memory"]),
            ("Pop", &["first", "last", "all", "mixed"]),
            ("Cardinality", &["single", "list", "set"]),
            ("Direction", &["OUT", "IN", "BOTH"]),
            ("By", &["Id", "Label", "Count"]),
        ];
        for (class, members) in former {
            let offered = tokens[*class]
                .as_array()
                .unwrap_or_else(|| panic!("class {class} missing: {tokens}"));
            for member in *members {
                assert!(
                    offered.iter().any(|m| m == member),
                    "{class}.{member} missing: {tokens}"
                );
            }
        }
        // Classes the old list lacked are offered now too.
        for class in [
            "GType",
            "Operator",
            "Merge",
            "Pick",
            "Barrier",
            "SchemaMode",
        ] {
            assert!(tokens.get(class).is_some(), "{class}: {tokens}");
        }
        for class in ["Execution", "TraversalMetrics", "ArgP"] {
            assert!(tokens.get(class).is_none(), "{class}: {tokens}");
        }
    }

    /// The playground allows everything but files: data, schema and administrative options
    /// work, every file request (`export_graphml`, `GraphSource::file`, Rhai's `import`) is a
    /// `PermissionDenied` naming its `File` request.
    #[test]
    fn the_playground_denies_files_and_allows_everything_else() {
        let mut session = modern();
        let r = run(
            &mut session,
            r#"g.with("evaluationTimeout", 0).addV("x").next(); g.V().count().next()"#,
        );
        assert_eq!(r["ok"], true, "{r}");
        for (script, request) in [
            (
                r#"g.export_graphml("out.graphml")"#,
                r#"Create File(\"out.graphml\")"#,
            ),
            (
                r#"GraphSource::file("g.graphml")"#,
                r#"Read File(\"g.graphml\")"#,
            ),
            (
                r#"g.export_graphson("out.json")"#,
                r#"Create File(\"out.json\")"#,
            ),
            (r#"GraphSource::file("g.json")"#, r#"Read File(\"g.json\")"#),
            (
                r#"import "module" as m; 1"#,
                r#"Execute File(\"module.rhai\")"#,
            ),
        ] {
            let r = run(&mut session, script);
            assert_eq!(r["ok"], false, "{script}: {r}");
            assert!(r.to_string().contains("Permission denied"), "{script}: {r}");
            assert!(r.to_string().contains(request), "{script}: {r}");
        }
    }

    #[test]
    fn implicit_traversal_returns_results_and_profile() {
        let r = run(&mut modern(), r#"g.v().has_label("person").values("name")"#);
        assert_eq!(r["ok"], true);
        assert_eq!(r["terminal"], "execute");
        assert_eq!(r["implicit"], true);
        assert_eq!(r["mutated"], false);
        assert_eq!(r["execution"]["results"].as_array().unwrap().len(), 4);
        assert!(r["execution"]["profile"]["metrics"].is_array());
        assert!(
            r["execution"]["profile"]["metrics"][0]
                .get("memory")
                .is_none()
        );
        assert_eq!(r["display"]["total_rows"], 4);
        assert!(r["display"]["raw"].as_str().unwrap().contains("marko"));
        assert_eq!(r["display"]["table"]["columns"], json!(["value"]));
    }

    #[test]
    fn element_keys_keep_the_engine_order() {
        let text = modern().execute("g.v(1)", &RunOptions::default());
        let results = text
            .find(r#""results":[{"id":"1","label":"person","properties":{"name":"marko","age":29"#);
        assert!(results.is_some(), "{text}");
        assert!(
            text.contains(r#""columns":["id","label","name","age"]"#),
            "{text}"
        );
    }

    #[test]
    fn vertices_and_edges_fill_the_subgraph() {
        let r = run(&mut modern(), r#"g.v(1).out_e()"#);
        let sg = &r["subgraph"];
        assert_eq!(sg["edgeIds"].as_array().unwrap().len(), 3);
        assert_eq!(sg["vertexIds"].as_array().unwrap().len(), 4);
        assert_eq!(sg["edgeSource"].as_array().unwrap().len(), 3);
        assert_eq!(sg["truncated"], false);
        assert_eq!(
            (sg["totalNodes"].as_u64(), sg["totalEdges"].as_u64()),
            (Some(4), Some(3))
        );
        // Columns only: the properties come from `element()` when an element is clicked.
        assert!(sg.get("nodes").is_none(), "{sg}");
        assert_eq!(sg["vertexIds"][0], "1");
        assert_eq!(sg["vertexName"][0], "marko");
        let none = run(&mut modern(), r#"g.v().values("name")"#);
        assert!(none["subgraph"].is_null());
        // Edges without a label (`label: null`, the `large` sample's) are found too.
        let mut large = Session::new();
        large.load_sample("large").unwrap();
        let r = run(&mut large, r#"g.v("V_0_0").out_e()"#);
        assert_eq!(
            r["subgraph"]["edgeIds"].as_array().unwrap().len(),
            10,
            "{r}"
        );
        assert_eq!(r["subgraph"]["vertexIds"].as_array().unwrap().len(), 11);
    }

    /// The subgraph limit is an option of the run: the default, a smaller one (`stoppedBy`), and
    /// a request above the hard maximum (clamped).
    #[test]
    fn the_subgraph_limit_is_a_run_option() {
        let mut session = modern();
        let all = run(&mut session, "g.v()");
        assert_eq!(all["subgraph"]["vertexIds"].as_array().unwrap().len(), 6);
        assert!(all["subgraph"].get("stoppedBy").is_none(), "{all}");
        let few = run_with(
            &mut session,
            "g.v()",
            &RunOptions {
                subgraph_max_elements: Some(2),
                ..RunOptions::default()
            },
        );
        assert_eq!(few["subgraph"]["vertexIds"].as_array().unwrap().len(), 2);
        assert_eq!(few["subgraph"]["truncated"], true);
        assert_eq!(few["subgraph"]["stoppedBy"], "elements");
        let huge = run_with(
            &mut session,
            "g.v()",
            &RunOptions {
                subgraph_max_elements: Some(usize::MAX),
                ..RunOptions::default()
            },
        );
        assert_eq!(huge["subgraph"]["truncated"], false);
        let bad: Result<RunOptions, _> =
            serde_json::from_str(r#"{"subgraph_max_elements": "many"}"#);
        assert!(bad.is_err());
    }

    /// The Graph panel's selections through the session: JSON in, JSON out, errors as text.
    #[test]
    fn neighbourhood_sample_and_meta_graph_answer_json() {
        let session = modern();
        let n = session
            .neighbourhood(r#"{"ids": ["1"], "hops": 1, "maxNodes": 10}"#)
            .unwrap();
        assert_eq!(n["vertexIds"].as_array().unwrap().len(), 4, "{n}");
        assert_eq!(n["seeds"], json!([0]));
        assert_eq!(n["hiddenEdges"], 2);
        assert!(n.get("sample").is_none());
        assert!(n.get("leftOut").is_none());
        // Depth and direction: josh (4) points at 3 and 5; at depth 2 outgoing nothing more.
        let out = session
            .neighbourhood(r#"{"ids": ["4"], "hops": 2, "direction": "out"}"#)
            .unwrap();
        assert_eq!(out["vertexIds"].as_array().unwrap().len(), 3, "{out}");
        let cut = session
            .neighbourhood(r#"{"ids": ["1"], "hops": 2, "direction": "both", "maxNodes": 2}"#)
            .unwrap();
        assert_eq!(cut["truncated"], true);
        assert_eq!(cut["leftOut"]["vertices"], 4, "{cut}");
        assert_eq!(cut["leftOut"]["complete"], true);
        assert!(
            session
                .neighbourhood(r#"{"ids": ["1"], "hops": 6}"#)
                .unwrap_err()
                .to_string()
                .contains("at most 5")
        );
        let grown = session
            .neighbourhood(r#"{"ids": ["4"], "keep": ["1", "2", "3", "4"]}"#)
            .unwrap();
        assert_eq!(grown["vertexIds"].as_array().unwrap().len(), 5, "{grown}");
        assert!(session.neighbourhood(r#"{"ids": ["x"]}"#).is_err());
        assert!(session.neighbourhood(r#"{"id": "1"}"#).is_err());
        let sample = session
            .label_sample(r#"{"vertices": {"labels": ["software"]}}"#)
            .unwrap();
        assert_eq!(sample["sample"]["total"], 2, "{sample}");
        assert_eq!(sample["vertexIds"].as_array().unwrap().len(), 2);
        let meta = session.meta_graph().unwrap();
        assert_eq!(meta["labels"][0]["label"], "person", "{meta}");
        assert_eq!(meta["edges"][0]["label"], "created");
        assert_eq!(meta["vertexCount"], 6);
        assert_eq!(meta["complete"], true);
    }

    #[test]
    fn element_returns_one_element_with_its_properties() {
        let session = modern();
        let text = session.element("vertex", "1").unwrap();
        assert!(
            text.contains(r#""properties":{"name":"marko","age":29}"#),
            "the engine's property order: {text}"
        );
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["kind"], "vertex");
        assert_eq!(v["label"], "person");
        assert_eq!(v["labels"], json!(["person"]));
        assert_eq!(v["properties"], json!({"name": "marko", "age": 29}));
        let e: Value = serde_json::from_str(&session.element("edge", "0").unwrap()).unwrap();
        assert_eq!(e["kind"], "edge");
        assert_eq!(
            (e["source"].as_str(), e["target"].as_str()),
            (Some("1"), Some("2"))
        );
        assert_eq!(e["properties"]["weight"], 0.5);
        // Gone (or never there), or the id of the other kind: null, not an error.
        assert_eq!(session.element("vertex", "999").unwrap(), "null");
        assert_eq!(session.element("edge", "6").unwrap(), "null");
        assert!(
            session
                .element("graph", "1")
                .unwrap_err()
                .to_string()
                .contains("unknown element kind")
        );
    }

    #[test]
    fn a_vertex_carries_its_in_and_out_degree_from_the_whole_graph() {
        let session = modern();
        let degrees = |id: &str| {
            let v: Value = serde_json::from_str(&session.element("vertex", id).unwrap()).unwrap();
            (v["inDegree"].clone(), v["outDegree"].clone())
        };
        assert_eq!(degrees("1"), (json!(0), json!(3)));
        assert_eq!(degrees("3"), (json!(3), json!(0)));
        assert_eq!(degrees("4"), (json!(1), json!(2)));
        let e: Value = serde_json::from_str(&session.element("edge", "0").unwrap()).unwrap();
        assert!(e.get("inDegree").is_none(), "{e}");
    }

    #[test]
    fn memory_profile_type_adds_the_memory_section() {
        let r = run_with(
            &mut modern(),
            r#"g.v().values("name").fold()"#,
            &RunOptions {
                profile: true,
                profile_types: vec!["memory".into()],
                ..RunOptions::default()
            },
        );
        let memory = &r["execution"]["profile"]["metrics"][0]["memory"];
        assert!(memory.is_object(), "{r}");
    }

    #[test]
    fn the_spelling_setting_respells_profile_and_error() {
        let camel = RunOptions {
            profile: true,
            spelling: Spelling::Camel,
            ..RunOptions::default()
        };
        let r = run_with(&mut modern(), r#"g.v().out_e().group_count()"#, &camel);
        let names: Vec<&str> = r["execution"]["profile"]["metrics"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["v()", "outE()", "groupCount()"], "{r}");
        let r = run_with(
            &mut modern(),
            r#"g.v().has_label("person").values("name").sum()"#,
            &camel,
        );
        let diagnostic = r["execution"]["error"]["diagnostic"].as_str().unwrap();
        assert!(
            diagnostic.contains(r#"v().hasLabel("person").values("name").sum()"#),
            "{diagnostic}"
        );
        // the JSON of the playground carries it as a snake_case key
        let options: RunOptions = serde_json::from_str(r#"{"spelling": "camel"}"#).unwrap();
        assert_eq!(options.spelling, Spelling::Camel);
    }

    #[test]
    fn runtime_error_carries_the_step_location_and_a_partial_profile() {
        let r = run(
            &mut modern(),
            r#"g.v().has_label("person").union(__.values("name"), __.values("age").property("x", 1))"#,
        );
        assert_eq!(r["ok"], false);
        let error = &r["execution"]["error"];
        assert!(error["step_id"].is_string(), "{error}");
        assert!(error["step_path"].as_array().unwrap().len() >= 2, "{error}");
        assert!(error["message"].is_string());
        assert!(error["diagnostic"].is_string());
        assert!(r["execution"]["profile"].is_object());
        assert!(r["display"].is_null());
        // The failing traversal was rolled back: nothing changed.
        assert_eq!(r["mutated"], false);
    }

    #[test]
    fn script_errors_have_help_and_a_position() {
        let r = run(&mut modern(), "g.v()\n  .has_lable(\"person\")");
        assert_eq!(r["ok"], false);
        let error = &r["execution"]["error"];
        assert_eq!(error["position"]["line"], 2, "{error}");
        assert!(!error["message"].as_str().unwrap().starts_with("Error: "));
    }

    #[test]
    fn the_memory_report_answers_the_report_or_an_error() {
        let s = modern();
        let full = s.memory_report(&Value::Null);
        assert_eq!(full["ok"], true, "{full}");
        let report = &full["report"];
        assert_eq!(report["properties"].as_array().unwrap().len(), 6);
        assert_eq!(
            report["property_bytes"],
            report["structure"]["totals"]["property_bytes"]
        );
        let capped = s.memory_report(&json!({"max_rows": 1, "max_labels": 2}));
        assert_eq!(capped["report"]["rows_omitted"], 5);
        assert_eq!(capped["report"]["labels_omitted"], 2);
        for (spec, needle) in [
            (json!({"rows": 1}), "unknown key 'rows'"),
            (json!({"max_rows": 0}), "positive integer"),
            (json!([1]), "JSON object"),
            (json!({"path_depth": 2}), "path depth"),
        ] {
            let answer = s.memory_report(&spec);
            assert_eq!(answer["ok"], false);
            let error = answer["error"].as_str().unwrap();
            assert!(error.contains(needle), "{spec}: {error}");
        }
    }

    #[test]
    fn explicit_terminals_are_reported() {
        let mut s = modern();
        assert_eq!(
            run(&mut s, "g.v().values(\"name\").to_list()")["terminal"],
            "to_list"
        );
        assert_eq!(run(&mut s, "g.v().count().next()")["terminal"], "next");
        let p = run(&mut s, "g.v().count().profile()");
        assert_eq!(p["terminal"], "profile");
        assert!(
            p["display"]["raw"]
                .as_str()
                .unwrap()
                .contains("Traversal Metrics")
        );
        assert!(p["execution"]["profile"]["metrics"].is_array());
        let t = run(&mut s, "g.v().to_json()");
        assert_eq!(t["terminal"], "to_json");
        assert_eq!(t["preferred_view"], "json");
        let schema = run(&mut s, "g.infer_schema()");
        assert_eq!(schema["ok"], true);
        assert_eq!(
            schema["display"]["table"]["columns"][4], "required",
            "{schema}"
        );
        assert!(
            schema["execution"]["results"][0]["$schema"].is_string(),
            "{schema}"
        );
        let memory = run(&mut s, "g.memory_usage()");
        assert!(memory["execution"]["results"][0]["total_bytes"].is_number());
        let exec = run(&mut s, "g.v().count().execute(#{profile: true})");
        assert_eq!(exec["terminal"], "execute");
        assert_eq!(exec["implicit"], false);
        assert!(exec["execution"]["profile"].is_object());
    }

    #[test]
    fn display_limits_cut_the_display_never_the_data() {
        let r = run_with(
            &mut modern(),
            "g.v()",
            &RunOptions {
                max_rows: Some(2),
                ..RunOptions::default()
            },
        );
        assert_eq!(r["execution"]["results"].as_array().unwrap().len(), 6);
        assert_eq!(r["display"]["table"]["rows"].as_array().unwrap().len(), 2);
        assert!(
            r["display"]["notice"]
                .as_str()
                .unwrap()
                .contains("first 2 rows")
        );
        let own = run(&mut modern(), r#"g.with("render.max_rows", 3).v()"#);
        assert_eq!(own["display"]["table"]["rows"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn mutations_change_the_graph_and_are_flagged() {
        let mut s = modern();
        let r = run(&mut s, r#"g.add_v("person").property("name", "zoe")"#);
        assert_eq!(r["mutated"], true);
        assert_eq!(r["graph"]["vertexCount"], 7);
        let explicit = run(&mut s, r#"g.add_v("x").to_list()"#);
        assert_eq!(explicit["mutated"], true);
        let read = run(&mut s, r#"g.v().has("name", "property").to_list()"#);
        assert_eq!(read["mutated"], false);
    }

    #[test]
    fn a_run_is_one_unit_a_failing_run_leaves_nothing() {
        let mut s = modern();
        let failed = run(
            &mut s,
            r#"g.add_v("a").next(); g.v("1").property("age", 99).next(); g.v().values("name").as_number(GType::LONG)"#,
        );
        assert_eq!(failed["ok"], false);
        assert_eq!(failed["mutated"], false);
        assert_eq!(failed["graph"]["vertexCount"], 6);
        let age = run(&mut s, r#"g.v("1").values("age").next()"#);
        assert_eq!(age["execution"]["results"][0], 29);
        // An error the script returns as an `execute()` result fails the run as well.
        let returned = run(
            &mut s,
            r#"g.add_v("a").next(); g.v().values("name").as_number(GType::LONG).execute()"#,
        );
        assert_eq!(returned["ok"], false);
        assert_eq!(returned["graph"]["vertexCount"], 6);
        // A successful run keeps every statement.
        let ok = run(&mut s, r#"g.add_v("a").next(); g.add_v("b").to_list()"#);
        assert_eq!(ok["ok"], true);
        assert_eq!(ok["mutated"], true);
        assert_eq!(ok["graph"]["vertexCount"], 8);
    }

    #[test]
    fn file_io_is_an_error_not_a_panic() {
        // Natively the path does not exist; in the browser there is no file system at all.
        let r = run(
            &mut modern(),
            r#"g.import_graphml("/no/such/file.graphml")"#,
        );
        assert_eq!(r["ok"], false);
    }

    #[test]
    fn save_and_load_round_trip() {
        let mut s = modern();
        run(&mut s, r#"g.v(1).property("nick", "m")"#);
        let saved = s.save_graphml().unwrap();
        assert_eq!(saved["warnings"], json!([]));
        assert_eq!(
            saved["schema"],
            Value::Null,
            "no stored schema, no schema file"
        );
        assert!(
            !saved["text"]
                .as_str()
                .unwrap()
                .contains("graphersal.schema")
        );
        let text = saved["text"].as_str().unwrap().to_string();
        let mut t = Session::new();
        let info = t
            .load_graphml(text.as_bytes(), "saved.graphml", None)
            .unwrap();
        assert_eq!(info["vertexCount"], 6);
        assert_eq!(info["edgeCount"], 6);
        let nick = run(&mut t, r#"g.v(1).values("nick").next()"#);
        assert_eq!(nick["execution"]["results"][0], "m");
    }

    /// GraphSON keeps every logical type without a schema, so nothing warns.
    #[test]
    fn graphson_save_and_load_round_trip_keeps_types() {
        let mut s = modern();
        let set = run(
            &mut s,
            r#"g.v(1).property("tags", ["a", "b"]).property("uid", UUID.fromString("550e8400-e29b-41d4-a716-446655440000")).property("config", #{cpu: 2}).next()"#,
        );
        assert_eq!(set["ok"], true, "{set}");
        let saved = s.save_graphson().unwrap();
        assert_eq!(saved["warnings"], json!([]), "{saved}");
        assert_eq!(saved["schema"], Value::Null);
        let text = saved["text"].as_str().unwrap().to_string();
        assert!(text.contains(r#""@type":"g:UUID""#), "{text}");
        let mut t = Session::new();
        let info = t
            .load_graphson(text.as_bytes(), "saved.json", None)
            .unwrap();
        assert_eq!(info["vertexCount"], 6);
        assert_eq!(info["edgeCount"], 6);
        assert_eq!(info["warnings"], json!([]));
        let typed = run(
            &mut t,
            r#"[g.v(1).values("uid").is(P.typeOf(GType.UUID)).count().next(), g.v(1).values("tags").next(), g.v(1).values("config").select("cpu").next()]"#,
        );
        let results = &typed["execution"]["results"];
        assert_eq!(results[0], 1, "{typed}");
        assert_eq!(results[1], json!(["a", "b"]), "{typed}");
        assert_eq!(results[2], 2, "{typed}");
    }

    /// The packed snapshot keeps everything, the schema included; a damaged one leaves the
    /// current graph.
    #[test]
    fn snapshot_save_and_load_round_trip_keeps_types_and_schema() {
        let mut s = modern();
        s.set_schema(r#"{"mode": "open"}"#, None).unwrap();
        let set = run(
            &mut s,
            r#"g.v(1).property("uid", UUID.fromString("550e8400-e29b-41d4-a716-446655440000")).property("tags", ["a"]).next()"#,
        );
        assert_eq!(set["ok"], true, "{set}");
        let bytes = s.save_snapshot().unwrap();
        let mut t = Session::new();
        let info = t.load_snapshot(&bytes, "saved.gsnap").unwrap();
        assert_eq!(info["vertexCount"], 6);
        assert_eq!(info["schemaMode"], "open", "{info}");
        let typed = run(
            &mut t,
            r#"g.v(1).values("uid").is(P.typeOf(GType.UUID)).count().next()"#,
        );
        assert_eq!(typed["execution"]["results"][0], 1, "{typed}");
        let mut damaged = bytes.clone();
        let middle = damaged.len() / 2;
        damaged[middle] ^= 1;
        assert!(t.load_snapshot(&damaged, "bad.gsnap").is_err());
        assert_eq!(t.info().unwrap()["name"], "saved.gsnap");
    }

    /// Skipped data is listed in the info's warnings until the next load.
    #[test]
    fn graphson_load_lists_skipped_data_as_warnings() {
        let mut s = Session::new();
        let text = r#"{"id":"v","label":"x","properties":{"born":[{"value":{"@type":"gx:Year","@value":"2016"}}],"seen":[{"value":{"@type":"g:Date","@value":1}}]}}"#;
        let info = s
            .load_graphson(text.as_bytes(), "dates.json", None)
            .unwrap();
        assert_eq!(info["vertexCount"], 1);
        let warnings = info["warnings"].as_array().unwrap();
        assert_eq!(warnings.len(), 1, "{info}");
        assert!(
            warnings[0]
                .as_str()
                .unwrap()
                .contains("skipped date/time values (gx:Year, a type Graphersal cannot hold): 1"),
            "{info}"
        );
        // A date/time value Graphersal holds is imported, not a warning.
        assert_eq!(
            run(&mut s, r#"g.v().has("seen").count().next()"#)["execution"]["results"][0],
            1
        );
        let info = s.load_sample("modern").unwrap();
        assert_eq!(info["warnings"], json!([]));
        // A malformed file fails and keeps the current graph.
        assert!(s.load_graphson(b"{bad", "bad.json", None).is_err());
        assert_eq!(s.info().unwrap()["vertexCount"], 6);
    }

    #[test]
    fn save_warns_about_types_graphml_cannot_keep() {
        let mut s = modern();
        run(&mut s, r#"g.v(1).property("tags", ["a", "b"])"#);
        let saved = s.save_graphml().unwrap();
        let warnings = saved["warnings"].as_array().unwrap();
        assert_eq!(warnings.len(), 1, "{saved}");
        assert!(warnings[0].as_str().unwrap().contains("'tags'"));
    }

    /// The schema of a declared `uuid`/`array`/`object` property for `person`.
    const TYPED_SCHEMA: &str = r#"{"mode": "open", "vertices": {"person": {"schema": {
        "type": "object", "properties": {
        "uid": {"type": ["string", "null"], "format": "uuid"},
        "tags": {"type": ["array", "null"], "items": {"type": "string"}},
        "config": {"type": ["object", "null"], "properties": {"cpu": {"type": "integer"}},
                   "required": ["cpu"]}
    }}}}}"#;

    /// "Save graph" writes the GraphML and the schema separately; "Load graph" with the schema
    /// file restores every declared type, containers included (F2).
    #[test]
    fn schema_saved_separately_and_loaded_with_the_graph_restores_types() {
        let mut s = modern();
        s.set_schema(TYPED_SCHEMA, None).expect("schema applies");
        let set = run(
            &mut s,
            r#"g.v(1).property("uid", UUID.fromString("550e8400-e29b-41d4-a716-446655440000")).property("tags", ["a", "b"]).property("config", #{cpu: 2}).next()"#,
        );
        assert_eq!(set["ok"], true, "{set}");
        let saved = s.save_graphml().unwrap();
        assert_eq!(saved["warnings"], json!([]), "{saved}");
        let text = saved["text"].as_str().unwrap();
        assert!(!text.contains("graphersal.schema"), "{text}");
        let schema = saved["schema"].as_str().expect("schema JSON");

        let mut t = Session::new();
        let info = t
            .load_graphml(text.as_bytes(), "saved.graphml", Some(schema))
            .unwrap();
        assert_eq!(info["vertexCount"], 6);
        assert_eq!(info["schemaMode"], "open");
        let typed = run(
            &mut t,
            r#"[g.v(1).values("uid").is(P.typeOf(GType.UUID)).count().next(), g.v(1).values("tags").next(), g.v(1).values("config").select("cpu").next()]"#,
        );
        assert_eq!(typed["ok"], true, "{typed}");
        let results = &typed["execution"]["results"];
        assert_eq!(results[0], 1, "the uuid type survives: {typed}");
        assert_eq!(results[1], json!(["a", "b"]), "{typed}");
        assert_eq!(results[2], 2, "{typed}");

        // Without the schema the same file loads, with the typed values as strings.
        let mut u = Session::new();
        u.load_graphml(text.as_bytes(), "saved.graphml", None)
            .unwrap();
        let plain = run(&mut u, r#"g.v(1).values("tags").next()"#);
        assert_eq!(plain["execution"]["results"][0], r#"["a","b"]"#, "{plain}");
    }

    /// "Load graph" never carries the current graph's schema onto the new graph, for GraphML
    /// and GraphSON alike; the catalog is still carried.
    #[test]
    fn load_graph_starts_without_the_previous_schema() {
        let text = modern().save_graphson().unwrap()["text"]
            .as_str()
            .unwrap()
            .to_string();
        let graphml = modern().save_graphml().unwrap()["text"]
            .as_str()
            .unwrap()
            .to_string();
        let mut s = modern();
        s.set_schema(r#"{"mode": "closed"}"#, Some("closed"))
            .expect_err("closed without declarations rejects the modern data");
        s.set_schema(TYPED_SCHEMA, None).expect("schema applies");
        assert_eq!(s.info().unwrap()["schemaMode"], "open");
        let info = s.load_graphson(text.as_bytes(), "g.json", None).unwrap();
        assert_eq!(info["schemaMode"], Value::Null, "{info}");
        assert_eq!(s.get_schema().unwrap(), Value::Null);
        s.set_schema(TYPED_SCHEMA, None).expect("schema applies");
        let info = s
            .load_graphml(graphml.as_bytes(), "g.graphml", None)
            .unwrap();
        assert_eq!(info["schemaMode"], Value::Null, "{info}");
        assert_eq!(s.get_schema().unwrap(), Value::Null);
    }

    /// A schema the data violates fails the load with the violation, and nothing changes: the
    /// current graph (and its schema) stays. A bad schema file is named as such.
    #[test]
    fn load_graph_with_a_violated_schema_fails_and_keeps_the_current_graph() {
        let text = modern().save_graphson().unwrap()["text"]
            .as_str()
            .unwrap()
            .to_string();
        let graphml = modern().save_graphml().unwrap()["text"]
            .as_str()
            .unwrap()
            .to_string();
        let strict = r#"{"mode": "closed", "vertices": {"person": {"schema": {"type": "object",
            "properties": {"name": {"type": "string"}, "age": {"type": "string"}}}}}}"#;
        let mut s = Session::new();
        s.load_sample("tree").unwrap();
        let before = s.info().unwrap();
        let err = s
            .load_graphson(text.as_bytes(), "g.json", Some(strict))
            .unwrap_err();
        assert!(err.to_string().contains("person.age"), "{err}");
        let err = s
            .load_graphml(graphml.as_bytes(), "g.graphml", Some(strict))
            .unwrap_err();
        assert!(err.to_string().contains("age"), "{err}");
        assert_eq!(s.info().unwrap(), before);
        let err = s
            .load_graphson(text.as_bytes(), "g.json", Some("{not json"))
            .unwrap_err();
        assert!(err.to_string().starts_with("Schema file: "), "{err}");
        assert_eq!(s.info().unwrap(), before);
        // An open schema the data satisfies loads with it.
        let info = s
            .load_graphson(text.as_bytes(), "g.json", Some(r#"{"mode": "open"}"#))
            .unwrap();
        assert_eq!(info["schemaMode"], "open", "{info}");
        assert_eq!(info["vertexCount"], 6);
    }

    #[test]
    fn set_schema_rejects_bad_json_and_violating_data() {
        let mut s = modern();
        let err = s.set_schema("{not json", None).unwrap_err();
        assert!(err.to_string().contains("not valid JSON"), "{err}");
        let err = s.set_schema(r#"{"mdoe": "closed"}"#, None).unwrap_err();
        assert!(err.to_string().contains("unknown key 'mdoe'"), "{err}");
        let err = s
            .set_schema(r#"{"mode": "closed", "vertices": {"person": {}}}"#, None)
            .unwrap_err();
        assert!(!err.to_string().is_empty());
        assert_eq!(
            s.info().unwrap()["schemaMode"],
            Value::Null,
            "nothing changed"
        );
    }

    /// "Freeze as open/closed": the inferred schema with a mode.
    #[test]
    fn set_schema_with_a_mode_freezes_the_inferred_schema() {
        let mut s = modern();
        let inferred = s.infer_schema_text().unwrap();
        let info = s.set_schema(&inferred, Some("closed")).unwrap();
        assert_eq!(info["schemaMode"], "closed");
        let err = s.set_schema(&inferred, Some("Closed")).unwrap_err();
        assert!(err.to_string().contains("unknown mode 'Closed'"), "{err}");
        assert_eq!(s.info().unwrap()["schemaMode"], "closed", "nothing changed");
        // Unlabeled elements: closed names them, open freezes.
        let mut e = Session::new();
        let r = run(&mut e, r#"g.add_v().next(); g.add_v("a").next()"#);
        assert_eq!(r["ok"], true, "{r}");
        let inferred = e.infer_schema_text().unwrap();
        let err = e.set_schema(&inferred, Some("closed")).unwrap_err();
        assert!(
            err.to_string().contains("1 vertex without a label"),
            "{err}"
        );
        assert_eq!(
            e.set_schema(&inferred, Some("open")).unwrap()["schemaMode"],
            "open"
        );
    }

    /// The memory section carries the parts of `property_bytes` the Statistics tab shows: the
    /// shared key lists (`shape_bytes`) and the dedup fields (all `0` without dedup rules).
    #[test]
    fn statistics_memory_carries_shape_and_dedup_bytes() {
        let s = modern();
        let stats = s.statistics().unwrap();
        let memory = &stats["memory"];
        let shapes = memory["shape_bytes"].as_u64().expect("shape_bytes");
        let properties = memory["property_bytes"].as_u64().expect("property_bytes");
        assert!(shapes > 0 && shapes <= properties, "{memory}");
        for key in ["deduped_values", "deduped_plain_bytes", "dedup_table_bytes"] {
            assert_eq!(memory[key], 0, "{key}: {memory}");
        }
    }

    /// The statistics panel's data: counts, memory, and the limits (presets and defaults).
    #[test]
    fn statistics_report_counts_memory_and_limits() {
        let mut s = modern();
        let stats = s.statistics().unwrap();
        let counts = &stats["statistics"];
        assert_eq!(counts["vertex_count"], 6, "{stats}");
        assert_eq!(counts["edge_count"], 6, "{stats}");
        assert_eq!(
            counts["vertex_labels"][0],
            json!({"label": "person", "count": 4, "primary": 4})
        );
        assert_eq!(
            counts["edge_labels"][0],
            json!({"label": "created", "count": 4})
        );
        assert_eq!(counts["vertex_label_total"], 2);
        assert_eq!(counts["truncated"], false);
        assert!(
            stats["memory"]["total_bytes"].as_u64().unwrap() > 0,
            "{stats}"
        );
        // The playground has no presets: every default is listed.
        let limits = &stats["limits"];
        assert_eq!(limits["presets"]["options"], json!({}));
        assert_eq!(limits["presets"]["scriptLimits"], json!({}));
        assert_eq!(limits["presets"]["scriptTimeoutMs"], Value::Null);
        assert_eq!(limits["defaults"]["traversal.max_traversers"], 10_000_000);
        assert_eq!(
            limits["defaults"]["traversal.max_materialized_bytes"],
            1024 * 1024 * 1024
        );
        assert_eq!(limits["defaults"].as_object().unwrap().len(), 6);
        // A run that adds data is counted.
        run(&mut s, r#"g.add_v(["person", "admin"]).next()"#);
        let counts = &s.statistics().unwrap()["statistics"];
        assert_eq!(counts["vertex_count"], 7);
        assert_eq!(
            counts["vertex_labels"][0],
            json!({"label": "person", "count": 5, "primary": 5})
        );
        assert_eq!(
            counts["vertex_labels"][1],
            json!({"label": "software", "count": 2, "primary": 2})
        );
        assert_eq!(
            counts["vertex_labels"][2],
            json!({"label": "admin", "count": 1, "primary": 0})
        );
    }

    #[test]
    fn statistics_of_a_preset_session_and_of_many_labels() {
        let mut s = Session::with_presets(
            SessionPresets::new()
                .with_option("traversal.max_traversers", ElementProperty::from(5_i64))
                .with_script_timeout_ms(Some(100)),
        );
        let limits = &s.statistics().unwrap()["limits"];
        assert_eq!(limits["presets"]["options"]["traversal.max_traversers"], 5);
        assert_eq!(limits["presets"]["scriptTimeoutMs"], 100);
        assert!(limits["defaults"].get("traversal.max_traversers").is_none());
        assert_eq!(limits["defaults"].as_object().unwrap().len(), 5);
        // The large sample has one label per vertex: the list is cut, the totals are not.
        s.load_sample("large").unwrap();
        let counts = &s.statistics().unwrap()["statistics"];
        assert_eq!(counts["truncated"], true);
        assert_eq!(
            counts["vertex_labels"].as_array().unwrap().len(),
            crate::views::STATISTICS_MAX_LABELS
        );
        assert_eq!(counts["vertex_label_total"], counts["vertex_count"]);
    }

    /// The seven schema methods as the playground calls them (JSON text in, JSON out), and the
    /// `changes` of a run.
    #[test]
    fn the_schema_methods_and_the_changes_of_a_run() {
        let mut s = modern();
        assert_eq!(s.get_schema().unwrap(), Value::Null);
        let inferred = s.infer_schema().unwrap();
        assert_eq!(inferred["mode"], "none");
        assert_eq!(
            s.get_schema().unwrap(),
            Value::Null,
            "inference stores nothing"
        );
        let validation = s
            .validate_schema(r#"{"mode": "closed", "vertices": {"person": {}}}"#)
            .unwrap();
        assert_eq!(validation["compatible"], false);
        assert!(
            validation["violations"]["total_violations"]
                .as_u64()
                .unwrap()
                > 0
        );
        assert!(validation["diff"].is_array());
        s.set_schema(&inferred.to_string(), None).unwrap();
        let patched = s
            .patch_schema(r#"{"vertices": {"person": {"schema": {"properties": {"x": {"type": "integer"}}}}}}"#)
            .unwrap();
        assert!(patched["vertices"]["person"]["schema"]["properties"]["x"].is_object());
        assert_eq!(s.get_schema().unwrap(), patched);
        let check = s.validate_schema_patch(r#"{"mode": "closed"}"#).unwrap();
        assert_eq!(check["violations"]["total_violations"], 0, "{check}");
        let diff = s.diff_schema(&inferred.to_string()).unwrap();
        assert_eq!(diff["changes"][0]["kind"], "property_removed");
        assert!(s.patch_schema("[").is_err());

        // A run reports what it changed: the schema refreshes on `schema: true`.
        let read = run(&mut s, "g.V().count().next()");
        assert_eq!(
            read["changes"],
            json!({"data": false, "schema": false, "definitions": false})
        );
        let data = run(
            &mut s,
            r#"g.addV("person").property("name", "x").property("age", 1).next()"#,
        );
        assert_eq!(
            data["changes"],
            json!({"data": true, "schema": false, "definitions": false}),
            "{data}"
        );
        let schema = run(&mut s, r#"g.patch_schema(#{mode: "open"})"#);
        assert_eq!(
            schema["changes"],
            json!({"data": false, "schema": true, "definitions": false}),
            "{schema}"
        );
        assert_eq!(schema["mutated"], true);
        let failed = run(
            &mut s,
            r#"g.patch_schema(#{mode: "closed"}); g.V("nope").next().unwrap()"#,
        );
        assert_eq!(
            failed["changes"],
            json!({"data": false, "schema": false, "definitions": false})
        );
        assert_eq!(
            s.get_schema().unwrap()["mode"],
            "open",
            "the failed run is rolled back"
        );
    }

    /// A Rhai string literal as the playground's schema editor writes one (`JSON.stringify`).
    fn rhai_string(text: &str) -> String {
        serde_json::to_string(&Value::String(text.to_string())).unwrap()
    }

    /// The calls of the playground's schema editor (`playground/js/schema/`), in its order: a
    /// draft from the inferred schema, "What changes", "Check against data" through a merge
    /// patch, the query a violation opens, the backfill script, and a patch that removes a
    /// member with `null`.
    #[test]
    fn the_schema_editor_flow() {
        let mut s = modern();
        // No stored schema: the editor shows the inferred one, and its patch is the whole draft.
        let mut draft = s.infer_schema().unwrap();
        draft["mode"] = json!("closed");
        let person = &mut draft["vertices"]["person"]["schema"];
        person["properties"]["email"] = json!({"type": "string", "default": "n/a"});
        person["required"] = json!(["name", "age", "email"]);
        let text = draft.to_string();

        let diff = s.diff_schema(&text).unwrap();
        assert_eq!(diff["compatible"], false, "{diff}");
        assert!(diff["changes"].as_array().is_some_and(|c| !c.is_empty()));
        let check = s.validate_schema_patch(&text).unwrap();
        let groups = check["violations"]["groups"].as_array().unwrap();
        let missing = groups
            .iter()
            .find(|g| g["kind"] == "missing_required")
            .expect("email is missing");
        assert_eq!(missing["label"], "person");
        assert_eq!(missing["field"], "email");
        assert_eq!(missing["count"], 4);
        // Apply is blocked by the check; the engine refuses the patch too and changes nothing.
        assert!(s.patch_schema(&text).is_err());
        assert_eq!(s.get_schema().unwrap(), Value::Null);

        // `violationQuery` of that group shows exactly the offending vertices.
        let shown = run(
            &mut s,
            "// person.email: missing required, 4 vertices\ng.V().hasLabel(\"person\").not(__.has(\"email\"))",
        );
        assert_eq!(shown["ok"], true, "{shown}");
        assert_eq!(shown["execution"]["results"].as_array().unwrap().len(), 4);

        // `backfillScript`: relax (mode none), fill with the default, set the draft; one run.
        let mut relaxed = draft.clone();
        relaxed["mode"] = json!("none");
        let script = format!(
            "g.set_schema({});\n\
             g.V().hasLabel(\"person\").not(__.has(\"email\")).property(\"email\", \"n/a\").toList();\n\
             g.set_schema({});\ng.get_schema()",
            rhai_string(&relaxed.to_string()),
            rhai_string(&text)
        );
        let backfilled = run(&mut s, &script);
        assert_eq!(backfilled["ok"], true, "{backfilled}");
        assert_eq!(
            backfilled["changes"],
            json!({"data": true, "schema": true, "definitions": false})
        );
        assert_eq!(s.get_schema().unwrap()["mode"], "closed");
        let filled = run(&mut s, r#"g.V().has("email", "n/a").count().next()"#);
        assert_eq!(filled["execution"]["results"][0], 4, "{filled}");

        // Making the key optional again: a compatible change, applied as a merge patch.
        let patch = r#"{"vertices": {"person": {"schema": {"required": ["name", "age"]}}}}"#;
        let check = s.validate_schema_patch(patch).unwrap();
        assert_eq!(check["violations"]["total_violations"], 0, "{check}");
        assert_eq!(check["compatible"], true, "{check}");
        // `null` removes a member (here the annotation `default`).
        let removed = s
            .patch_schema(r#"{"vertices": {"person": {"schema": {"required": ["name", "age"], "properties": {"email": {"default": null}}}}}}"#)
            .unwrap();
        let email = &removed["vertices"]["person"]["schema"]["properties"]["email"];
        assert_eq!(email, &json!({"type": "string"}), "{removed}");
    }

    /// The playground gets schemas as canonical text: envelope keys in the canonical order and
    /// the property order kept (the editor shows it; a `Value` would sort the keys).
    #[test]
    fn schemas_reach_the_playground_in_the_canonical_order() {
        let mut s = modern();
        assert_eq!(s.get_schema_text().unwrap(), "null");
        s.set_schema(r#"{"vertices": {"person": {"schema": {"type": "object", "properties": {"zeta": {"type": "string"}, "alpha": {"type": "integer"}}}}}, "mode": "open"}"#, None)
            .unwrap();
        let text = s.get_schema_text().unwrap();
        let order = |keys: &[&str]| {
            let at: Vec<usize> = keys.iter().map(|k| text.find(k).unwrap()).collect();
            at.windows(2).all(|w| w[0] < w[1])
        };
        assert!(
            order(&["\"$schema\"", "\"mode\"", "\"vertices\"", "\"edges\""]),
            "{text}"
        );
        assert!(order(&["\"zeta\"", "\"alpha\""]), "{text}");
        let patched = s
            .patch_schema_text(r#"{"vertices": {"person": {"schema": {"properties": {"beta": {"type": "boolean"}}}}}}"#)
            .unwrap();
        let at = |k: &str| patched.find(k).unwrap();
        assert!(
            at("\"zeta\"") < at("\"alpha\"") && at("\"alpha\"") < at("\"beta\""),
            "{patched}"
        );
        assert!(s.infer_schema_text().unwrap().starts_with("{\"$schema\""));
    }

    /// The force layout through the session: a JSON request, slices, the cancel token (the dev
    /// server's Stop) ending a running slice, positions of a finished job.
    #[test]
    fn the_force_layout_runs_in_slices_and_stops_on_the_cancel_token() {
        let mut session = modern();
        let n = 4000u32;
        let request = json!({
            "vertexCount": n,
            "edgeSource": (0..n).collect::<Vec<_>>(),
            "edgeTarget": (0..n).map(|i| (i + 1) % n).collect::<Vec<_>>(),
            "iterations": 1_000_000,
        });
        let status = session
            .layout_start_json(&request.to_string(), 0.0)
            .unwrap();
        assert_eq!(status["state"], "running", "{status}");
        let job = status["job"].as_u64().unwrap();
        let token = session.cancel_token();
        let canceller = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            token.cancel();
        });
        // Slices of the longest budget until the token ends one.
        let mut status = session.layout_step(job, 500.0).unwrap();
        while status["state"] == "running" {
            status = session.layout_step(job, 500.0).unwrap();
        }
        canceller.join().unwrap();
        assert_eq!(status["state"], "cancelled", "{status}");
        let positions = session.layout_positions(job).unwrap();
        assert_eq!(positions.len(), 2 * n as usize);
        assert!(positions.iter().all(|v| v.is_finite()));
        assert!(
            session.layout_step(job, 0.0).is_err(),
            "answered jobs are forgotten"
        );
        // A small one runs to the end; bad requests are errors.
        let small = session
            .layout_start_json(
                r#"{"vertexCount": 2, "edgeSource": [0], "edgeTarget": [1]}"#,
                500.0,
            )
            .unwrap();
        assert_eq!(small["state"], "done", "{small}");
        assert!(session.layout_start_json("{", 0.0).is_err());
        assert!(
            session
                .layout_start_json(
                    r#"{"vertexCount": 1, "edgeSource": [0], "edgeTarget": [1]}"#,
                    0.0
                )
                .is_err()
        );
    }

    /// The cancel token (the dev server's Stop) ends a running traversal and a script loop from
    /// another thread; the run is rolled back, and the next run is not affected.
    #[test]
    fn the_cancel_token_stops_a_run_and_rolls_it_back() {
        let mut session = modern();
        for script in [
            r#"g.inject(1).addV("ghost").repeat(__.identity()).times(1000000000).count()"#,
            r#"g.addV("ghost").next(); let n = 0; loop { n += 1; }"#,
        ] {
            let token = session.cancel_token();
            let canceller = std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(100));
                token.cancel();
            });
            let r = run(&mut session, script);
            canceller.join().unwrap();
            assert_eq!(r["ok"], false, "{r}");
            let diagnostic = r["execution"]["error"]["diagnostic"].as_str().unwrap();
            assert!(diagnostic.contains("cancelled"), "{diagnostic}");
            assert_eq!(session.info().unwrap()["vertexCount"], 6);
        }
        // `execute` resets the token: the next run is not cancelled.
        assert_eq!(run(&mut session, "g.v().count().next()")["ok"], true);
    }

    /// `execute_with_cancel` is stopped by the caller's token only: the session's token does
    /// not reach it, its own token stops traversals and script loops.
    #[test]
    fn a_run_with_its_own_token_is_cancelled_by_that_token_only() {
        let mut session = modern();
        let options = RunOptions::default();
        let own = CancelToken::new();
        session.cancel_token().cancel();
        let r: Value = serde_json::from_str(&session.execute_with_cancel(
            "g.v().count().next()",
            &options,
            playground_authorizer(),
            &own,
        ))
        .unwrap();
        assert_eq!(r["ok"], true, "{r}");
        for script in [
            r#"g.inject(1).addV("ghost").repeat(__.identity()).times(1000000000).count()"#,
            r#"g.addV("ghost").next(); let n = 0; loop { n += 1; }"#,
        ] {
            let own = CancelToken::new();
            let token = own.clone();
            let canceller = std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(100));
                token.cancel();
            });
            let r: Value = serde_json::from_str(&session.execute_with_cancel(
                script,
                &options,
                playground_authorizer(),
                &own,
            ))
            .unwrap();
            canceller.join().unwrap();
            assert_eq!(r["ok"], false, "{r}");
            assert_eq!(session.info().unwrap()["vertexCount"], 6);
        }
    }

    #[test]
    fn trailing_terminal_reads_the_last_call() {
        assert_eq!(trailing_terminal("g.v().toList();"), Some("to_list"));
        assert_eq!(
            trailing_terminal("g.v().to_table(#{max_rows: 2})"),
            Some("to_table")
        );
        assert_eq!(trailing_terminal("g.v().out()"), None);
        assert_eq!(trailing_terminal("let x = 1"), None);
        assert_eq!(trailing_terminal("next()"), None);
    }

    /// Host presets: options on every source of a run (a query still overrides them), Rhai
    /// limits on the engine, and the budget of a run as a whole.
    #[test]
    fn presets_bound_every_run() {
        let presets = SessionPresets::new()
            .with_option("traversal.max_traversers", ElementProperty::from(3_i64))
            .with_limits(ScriptLimits::unlimited().with_max_operations(1_000_000))
            .with_script_timeout_ms(Some(100));
        let mut s = Session::with_presets(presets);
        s.load_sample("modern").unwrap();
        assert_eq!(s.presets().script_timeout_ms, Some(100));
        let capped = run(&mut s, "g.v().to_list()");
        assert_eq!(capped["ok"], false, "{capped}");
        let message = capped["execution"]["error"]["diagnostic"].as_str().unwrap();
        assert!(message.contains("traversal.max_traversers"), "{message}");
        let own = run(
            &mut s,
            r#"g.with("traversal.max_traversers", 10).v().to_list()"#,
        );
        assert_eq!(own["ok"], true, "{own}");
        let looped = run(&mut s, "let n = 0; loop { n += 1; }");
        assert_eq!(looped["ok"], false, "{looped}");
        let message = looped["execution"]["error"]["diagnostic"].as_str().unwrap();
        assert!(
            message.contains("Resource limit") || message.contains("Script exceeded"),
            "{message}"
        );
        // The default session has none of them.
        let mut free = modern();
        assert_eq!(run(&mut free, "g.v().to_list()")["ok"], true);
        assert!(free.presets().options.is_empty());
    }

    /// The process memory block lists the embedders that report a size and nothing else; the
    /// graph statistics beside it are untouched.
    #[test]
    fn statistics_carry_the_process_memory_of_the_loaded_models() {
        use graphersal::vector::{
            EmbedContext, EmbedError, Embedder, EmbedderInfo, Vector, VectorElement,
        };
        struct Sized(EmbedderInfo);
        impl Embedder for Sized {
            fn info(&self) -> &EmbedderInfo {
                &self.0
            }
            fn embed(
                &self,
                texts: &[&str],
                _ctx: &EmbedContext<'_>,
            ) -> Result<Vec<Vector>, EmbedError> {
                texts
                    .iter()
                    .map(|_| {
                        Vector::from_f32(vec![1.0]).map_err(|e| EmbedError::failed(e.to_string()))
                    })
                    .collect()
            }
            fn memory_bytes(&self) -> Option<u64> {
                Some(1234)
            }
        }
        let embedders = Embedders::builder()
            .with(Arc::new(Sized(EmbedderInfo::new(
                "acme/m",
                1,
                VectorElement::F32,
            ))))
            .unwrap()
            .build();
        let mut s =
            Session::with_presets(SessionPresets::new().with_embedders(Arc::new(embedders)));
        s.load_sample("modern").unwrap();
        let stats = s.statistics().unwrap();
        let models = stats["process"]["models"].as_array().unwrap();
        assert_eq!(models.len(), 1, "{stats}");
        assert_eq!(models[0]["name"], "acme/m");
        assert_eq!(models[0]["bytes"], 1234);
        assert_eq!(models[0]["default"], false);
        assert_eq!(stats["statistics"]["vertex_count"], 6);
        // The default session has only the built-in embedder, which reports no size.
        assert_eq!(
            modern().statistics().unwrap()["process"]["models"],
            json!([])
        );
    }

    /// `process.memory` is always present with a `source`; its figures are optional.
    #[test]
    fn statistics_carry_the_memory_of_the_whole_process() {
        let mut stats = modern().statistics().unwrap();
        assert!(
            stats["process"].get("memory").is_none(),
            "volatile: hosts add it"
        );
        crate::process::add_to_statistics(&mut stats);
        let memory = &stats["process"]["memory"];
        assert!(memory["source"].is_string(), "{stats}");
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        assert!(memory["rssBytes"].as_u64().unwrap() > 0, "{stats}");
    }

    /// The host's embedders reach every run: the `embed` step (also inside the MCP read-only
    /// authorizer), `embed(text)` and `g.embedders()`; the default session has `hash-ngram/1`.
    #[test]
    fn the_session_embedders_reach_every_run() {
        let small = graphersal::vector::HashNgram::with_dimension("acme/small", 8).unwrap();
        let embedders = Embedders::builder()
            .with(Arc::new(small))
            .unwrap()
            .default_name("acme/small")
            .unwrap()
            .build();
        let mut s =
            Session::with_presets(SessionPresets::new().with_embedders(Arc::new(embedders)));
        s.load_sample("modern").unwrap();
        let shape = run(
            &mut s,
            r#"[g.v().embed("name").next().dimension, embed("x").dimension, g.embedders().len()]"#,
        );
        assert_eq!(shape["ok"], true, "{shape}");
        assert_eq!(shape["execution"]["results"], json!([8, 8, 2]), "{shape}");
        let read_only: Value = serde_json::from_str(&s.execute_with_authorizer(
            r#"g.v().embed("name").next().dimension"#,
            &RunOptions::default(),
            read_only_authorizer(),
        ))
        .unwrap();
        assert_eq!(read_only["ok"], true, "{read_only}");
        let mut free = modern();
        let builtin = run(&mut free, r#"g.embedders()[0].name"#);
        assert!(
            builtin["display"]["raw"]
                .as_str()
                .unwrap()
                .contains("hash-ngram/1"),
            "{builtin}"
        );
    }

    /// A GQL write under the read-only authorizer (the dev server's read-only endpoint and MCP
    /// `--mcp-read-only`) is refused before it runs and changes nothing; the same write runs
    /// with the playground's authorizer and is reported as mutating.
    #[test]
    fn a_gql_write_is_refused_read_only_and_runs_otherwise() {
        let mut s = modern();
        let refused: Value = serde_json::from_str(&s.execute_with_authorizer(
            r#"g.gql("MATCH (p:person) INSERT (p)-[:owns]->(:pet)")"#,
            &RunOptions::default(),
            read_only_authorizer(),
        ))
        .unwrap();
        assert_eq!(refused["ok"], false, "{refused}");
        let diagnostic = refused["execution"]["error"]["diagnostic"]
            .as_str()
            .unwrap_or_default();
        assert!(
            diagnostic
                .contains(r#"Permission denied for 'gql:insert_node' (Create Data(vertex "pet"))"#),
            "{refused}"
        );
        assert_eq!(
            run(&mut s, "g.V().count().next()")["execution"]["results"],
            json!([6])
        );
        let written = run(
            &mut s,
            r#"g.gql("MATCH (p:person) INSERT (p)-[:owns]->(:pet)")"#,
        );
        assert_eq!(written["ok"], true, "{written}");
        assert_eq!(written["mutated"], true, "{written}");
        assert_eq!(
            run(&mut s, "g.V().count().next()")["execution"]["results"],
            json!([10])
        );
    }

    /// A committed data change carries the one-line summary of the net vertex and edge count
    /// changes (W40); a read, a failed run and a schema-only change carry none.
    #[test]
    fn a_data_change_carries_its_change_summary() {
        let mut s = modern();
        let inserted = run(
            &mut s,
            r#"g.gql("MATCH (p:person) INSERT (p)-[:owns]->(:pet)")"#,
        );
        assert_eq!(
            inserted["change_summary"],
            json!({"vertices": 4, "edges": 4, "text": "+4 vertices, +4 edges"}),
            "{inserted}"
        );
        let dropped = run(
            &mut s,
            r#"g.v().has_label("pet").limit(1).drop().iterate()"#,
        );
        assert_eq!(
            dropped["change_summary"],
            json!({"vertices": -1, "edges": -1, "text": "-1 vertex, -1 edge"}),
            "{dropped}"
        );
        let set = run(
            &mut s,
            r#"g.v().has_label("pet").property("x", 1).iterate()"#,
        );
        assert_eq!(
            set["change_summary"],
            json!({"vertices": 0, "edges": 0, "text": ""}),
            "{set}"
        );
        for script in [
            "g.V().count().next()",
            r#"g.gql("FOR x IN [1, 0] INSERT (:n {v: 1 / x})")"#,
            r#"g.patch_schema(#{mode: "open"})"#,
        ] {
            let answer = run(&mut s, script);
            assert!(answer.get("change_summary").is_none(), "{answer}");
        }
    }

    #[test]
    fn the_script_budget_stops_a_loop() {
        let mut s = Session::with_presets(SessionPresets::new().with_script_timeout_ms(Some(50)));
        let looped = run(&mut s, "let n = 0; loop { n += 1; }");
        let message = looped["execution"]["error"]["diagnostic"].as_str().unwrap();
        assert!(
            message.contains("Script exceeded evaluationTimeout of 50 ms"),
            "{message}"
        );
        // The next run gets its own budget.
        assert_eq!(run(&mut s, "1 + 1")["ok"], true);
    }
    /// A graph value as the final value (`to_graph()`, `cap("sg").next()`, `g`) is one result
    /// `{"vertices", "edges"}` like `cap("sg")` gives inside a traversal, cut to the item limit.
    #[test]
    fn a_graph_value_is_listed_like_cap() {
        let mut s = modern();
        let r = run(&mut s, "g.e(0).to_graph()");
        assert_eq!(r["ok"], true, "{r}");
        let results = &r["execution"]["results"];
        assert_eq!(results.as_array().map(Vec::len), Some(1), "{r}");
        assert_eq!(
            results[0]["vertices"].as_array().map(Vec::len),
            Some(2),
            "{r}"
        );
        assert_eq!(results[0]["edges"][0]["id"], "0", "{r}");
        assert_eq!(
            r["display"]["table"]["columns"],
            json!(["vertices", "edges"]),
            "{r}"
        );
        assert!(
            r["display"]["raw"].as_str().unwrap_or("").contains("v[1]"),
            "{r}"
        );
        assert_eq!(r["subgraph"]["totalEdges"], 1, "{r}");

        let r = run(&mut s, r#"g.e().subgraph("sg").cap("sg").next()"#);
        assert_eq!(
            r["execution"]["results"][0]["edges"]
                .as_array()
                .map(Vec::len),
            Some(6)
        );

        // `g` itself, bounded: only max_items + 1 elements of each kind are read.
        let options = RunOptions {
            max_items: Some(2),
            ..RunOptions::default()
        };
        let r = run_with(&mut s, "g", &options);
        assert_eq!(r["ok"], true, "{r}");
        assert_eq!(
            r["execution"]["results"][0]["vertices"]
                .as_array()
                .map(Vec::len),
            Some(3),
            "{r}"
        );
        assert!(r["display"]["notice"].as_str().is_some(), "{r}");
    }

    /// GQL runs through the session's JSON API: rows keep the written column order in
    /// `results`, `$params` come from a Rhai map, and a parse error is an ordinary failed run.
    #[test]
    fn gql_runs_through_the_session() {
        let mut s = modern();
        let text = s.execute(
            r#"g.gql("MATCH (p:person)-[:created]->(s) RETURN p.name AS who, s.name AS what ORDER BY who, what")"#,
            &RunOptions::default(),
        );
        assert!(
            text.contains(r#""results":[{"who":"josh","what":"lop"},{"who":"josh","what":"ripple"},{"who":"marko","what":"lop"},{"who":"peter","what":"lop"}]"#),
            "{text}"
        );
        let r = run(
            &mut s,
            r#"g.gql("MATCH (p:person) WHERE p.age > $min RETURN p.name AS name ORDER BY name", #{min: 30})"#,
        );
        assert_eq!(r["ok"], true, "{r}");
        assert_eq!(
            r["execution"]["results"],
            json!([{"name": "josh"}, {"name": "peter"}])
        );
        let r = run(&mut s, r#"g.gql("MATCH (p:person) RETURN")"#);
        assert_eq!(r["ok"], false, "{r}");
        assert!(r["execution"]["error"]["help"].is_string(), "{r}");
        assert_eq!(r["mutated"], false, "{r}");
    }

    /// The deepest GQL nesting the parser accepts runs on an ordinary (2 MiB) thread, and one
    /// level more is refused with GQLSTATUS 54001, never a stack overflow.
    #[test]
    fn deeply_nested_gql_runs_on_an_ordinary_thread() {
        let mut s = modern();
        let nested = |n: usize| {
            format!(
                r#"g.gql("MATCH (p:person) WHERE {}p.age > 1{} RETURN p.name AS n")"#,
                "(".repeat(n),
                ")".repeat(n)
            )
        };
        let r = run(&mut s, &nested(126));
        assert_eq!(r["ok"], true, "{r}");
        assert_eq!(r["execution"]["results"].as_array().unwrap().len(), 4);
        let r = run(&mut s, &nested(127));
        assert_eq!(r["ok"], false, "{r}");
        assert!(r.to_string().contains("54001"), "{r}");
        let ors = (0..510)
            .map(|i| format!("p.age = {i}"))
            .collect::<Vec<_>>()
            .join(" OR ");
        let r = run(
            &mut s,
            &format!(r#"g.gql("MATCH (p:person) WHERE {ors} RETURN p.name AS n")"#),
        );
        assert_eq!(r["ok"], true, "{r}");
        assert_eq!(r["execution"]["results"].as_array().unwrap().len(), 4);
    }

    /// `offset` pages the display of a list result (traversal, `to_list()`) after the first
    /// `offset` results; the response reports `offset`, `shown_rows` and `items_cut`, and a
    /// single value ignores it.
    #[test]
    fn offset_pages_the_display() {
        let page = |script: &str, offset: usize, max_rows: usize| {
            run_with(
                &mut modern(),
                script,
                &RunOptions {
                    offset: Some(offset),
                    max_rows: Some(max_rows),
                    ..RunOptions::default()
                },
            )
        };
        let names = r#"g.v().order().by("name").values("name")"#;
        for script in [names.to_string(), format!("{names}.to_list()")] {
            let r = page(&script, 2, 3);
            let shown: Value =
                serde_json::from_str(r["display"]["json"].as_str().unwrap()).unwrap();
            assert_eq!(shown, json!(["marko", "peter", "ripple"]), "{r}");
            assert_eq!(r["display"]["offset"], 2, "{r}");
            assert_eq!(r["display"]["shown_rows"], 3, "{r}");
            assert_eq!(r["display"]["total_rows"], 6, "{r}");
            assert!(r["display"].get("items_cut").is_none(), "{r}");
            let notice = r["display"]["notice"].as_str().unwrap();
            assert!(notice.contains("showing rows 3 to 5 of 6"), "{notice}");
            // Past the end: nothing shown, the offset as asked.
            let r = page(&script, 9, 3);
            assert_eq!(r["display"]["json"], "[]", "{r}");
            assert_eq!(r["display"]["offset"], 9, "{r}");
            assert_eq!(r["display"]["shown_rows"], 0, "{r}");
        }
        // No offset: the fields stay out of the response (the playground's shape).
        let r = page(names, 0, 100);
        assert!(r["display"].get("offset").is_none(), "{r}");
        assert_eq!(r["display"]["shown_rows"], 6, "{r}");
        // A single value ignores the offset.
        let r = page(r#"g.v().count().next()"#, 3, 100);
        assert_eq!(r["display"]["json"], "6", "{r}");
        assert!(r["display"].get("offset").is_none(), "{r}");
        // A nested list cut to max_items is reported.
        let r = run_with(
            &mut modern(),
            "[[1, 2, 3]]",
            &RunOptions {
                max_items: Some(2),
                ..RunOptions::default()
            },
        );
        assert_eq!(r["display"]["items_cut"], true, "{r}");
    }
}
