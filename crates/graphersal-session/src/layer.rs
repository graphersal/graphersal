//! Layers in the session: what the playground shows of a layered graph and how the browser
//! playground starts one.
//!
//! - [`Session::info`] carries `layer` on a layered graph ([`layer_info`], from
//!   `AnyGraph::layer_summary`: no diff walk), absent on any other graph:
//!   `{base, baseRef: {graphId, commitSeq, committedAt} | null, added: {vertices, edges},
//!   modified: {vertices, edges}, removed: {vertices, edges}, ownBytes, sharedBytes}`.
//! - The drawing's columns carry `vertexOrigin` / `edgeOrigin` and the inspector's element
//!   `origin` on a layered graph only (`crate::display`).
//! - [`Session::layer_diff`] answers `{ok: true, diff}` (the engine's `LayerDiff::to_property`
//!   form, snake_case keys like the DSL's `g.layer_diff()`) or `{ok: false, error}`; spec
//!   `{kinds?, labels?, limit?}` as the DSL takes it ([`layer_diff_request`]).
//! - [`Session::start_layer`] freezes the session's own graph as the base of a new, empty layer
//!   in memory (the browser playground's demo; the dev server gets its layer from the CLI
//!   flags); [`Session::drop_layer`] discards that layer and serves the base again.

use std::sync::Arc;

use graphersal::prelude::*;
use graphersal::storage::{AnyGraph, ElementKind, LayerBase, LayerDiffRequest, LayerSummary};
use serde_json::{Value, json};

use crate::display::json_safe;
use crate::error::SessionError;
use crate::session::Session;

/// The keys of a layer diff spec.
const DIFF_KEYS: [&str; 3] = ["kinds", "labels", "limit"];

/// The `info.layer` block of a layer summary (module docs).
pub fn layer_info(summary: &LayerSummary) -> Value {
    let counts = |counts: &graphersal::storage::LayerCounts| json!({"vertices": counts.vertices, "edges": counts.edges});
    let base_ref = summary.base_ref.map(|base_ref| {
        json!({
            "graphId": format_uuid(base_ref.graph_id()),
            "commitSeq": base_ref.commit_seq(),
            "committedAt": base_ref.committed_at(),
        })
    });
    json!({
        "base": summary.base.as_str(),
        "baseRef": base_ref,
        "added": counts(&summary.added),
        "modified": counts(&summary.modified),
        "removed": counts(&summary.removed),
        "ownBytes": summary.own_bytes,
        "sharedBytes": summary.shared_bytes,
    })
}

/// The canonical text of a UUID (8-4-4-4-12 lowercase hex digits).
fn format_uuid(value: u128) -> String {
    let hex = format!("{value:032x}");
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// The request of a layer diff spec `{kinds?, labels?, limit?}` (`null`: the whole diff), the
/// options of the DSL's `g.layer_diff(#{..})`: `kinds` `"vertex"`, `"edge"` or a non-empty list
/// of them; `labels` a label or a list of labels (an element matches with any label of its
/// base or layer label set; `[]` matches nothing); `limit` a positive integer (the entries of
/// the three lists together; `truncated` says when more exist). A key set to `null` counts as
/// absent. Otherwise what is wrong with it.
pub fn layer_diff_request(spec: &Value) -> Result<LayerDiffRequest, SessionError> {
    let mut request = LayerDiffRequest::new();
    let fields = match spec {
        Value::Null => return Ok(request),
        Value::Object(fields) => fields,
        _ => {
            return Err(SessionError::invalid(
                "the layer diff spec is a JSON object {kinds?, labels?, limit?}",
            ));
        }
    };
    if let Some(unknown) = fields.keys().find(|key| !DIFF_KEYS.contains(&key.as_str())) {
        return Err(SessionError::invalid(format!(
            "unknown key '{unknown}' in the layer diff spec (keys: {})",
            DIFF_KEYS.join(", ")
        )));
    }
    if let Some(kinds) = strings(fields.get("kinds")).map_err(|value| kinds_error(&value))? {
        let (mut vertex, mut edge) = (false, false);
        for kind in &kinds {
            match kind.as_str() {
                "vertex" => vertex = true,
                "edge" => edge = true,
                _ => return Err(kinds_error(&Value::from(kind.as_str()))),
            }
        }
        match (vertex, edge) {
            (true, false) => request = request.kind(ElementKind::Vertex),
            (false, true) => request = request.kind(ElementKind::Edge),
            (true, true) => {}
            (false, false) => return Err(kinds_error(&json!([]))),
        }
    }
    if let Some(labels) = strings(fields.get("labels")).map_err(|value| {
        SessionError::invalid(format!(
            "labels must be a label or a list of labels, not {value}"
        ))
    })? {
        request = request.labels(labels);
    }
    match fields.get("limit") {
        None | Some(Value::Null) => {}
        Some(value) => match value.as_u64() {
            Some(n) if n > 0 => request = request.limit(usize::try_from(n).unwrap_or(usize::MAX)),
            _ => {
                return Err(SessionError::invalid(format!(
                    "limit must be a positive integer, not {value}"
                )));
            }
        },
    }
    Ok(request)
}

fn kinds_error(value: &Value) -> SessionError {
    SessionError::invalid(format!(
        "kinds must be \"vertex\", \"edge\" or a non-empty list of them, not {value}"
    ))
}

/// A string or a list of strings as a list; `None` for an absent or `null` value; `Err` with
/// the value for anything else.
fn strings(value: Option<&Value>) -> Result<Option<Vec<String>>, Value> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(vec![text.clone()])),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| item.as_str().map(str::to_string))
            .collect::<Option<Vec<_>>>()
            .map(Some)
            .ok_or_else(|| Value::Array(items.clone())),
        Some(other) => Err(other.clone()),
    }
}

impl Session {
    /// The `layer` block of [`Session::info`] (module docs), `None` on a graph without layers.
    /// Fails when the graph cannot be read.
    pub fn layer_info(&self) -> Result<Option<Value>, SessionError> {
        Ok(self.graph.layer_summary()?.as_ref().map(layer_info))
    }

    /// What the layer changed against its base (module docs): `{ok: true, diff}` with `diff`
    /// = `{added: [entry], removed: [entry], modified: [change], truncated}` (entry `{kind, id,
    /// labels, properties, out?, in?}`, change `{kind, id, base_labels, labels, out?, in?,
    /// properties: {name: {base?, layer?}}}`, `out`/`in` only on edges), or `{ok: false,
    /// error}` (an invalid spec, a graph that cannot be read). An empty diff on a graph without
    /// layers. Read-only. Keys of JSON objects keep no order on the wire: the lists do.
    pub fn layer_diff(&self, spec: &Value) -> Value {
        let answer = layer_diff_request(spec).and_then(|request| {
            let diff = self.graph.layer_diff(&request)?;
            serde_json::to_value(json_safe(&diff.to_property()))
                .map_err(|err| SessionError::failed(format!("the layer diff is not JSON: {err}")))
        });
        match answer {
            Ok(diff) => json!({"ok": true, "diff": diff}),
            Err(error) => json!({"ok": false, "error": error.to_string()}),
        }
    }

    /// Turns the session's graph into the frozen base of a new, empty layer kept in memory, and
    /// serves the layer from now on (the browser playground's "Start a layer over this graph"):
    /// `{ok: true, info}` ([`Session::info`], now with `layer`) or `{ok: false, error}`, the
    /// session unchanged then (except as said below). The base is the graph itself, not a
    /// copy: the session must be its only owner. The store in memory that held the graph is
    /// closed (a layer is not kept as a store, so the Store menu is off on a layer; its history
    /// goes with it). [`Session::drop_layer`] goes back. Refused on a layer, on a read-only
    /// graph (a store's read-only view, a read-only serving mode), for a storage other than
    /// `TraversalGraph`, and for a base the layer cannot carry (the graph is served again as
    /// it was, without its store).
    pub fn start_layer(&mut self) -> Value {
        match self.try_start_layer() {
            Ok(info) => json!({"ok": true, "info": info}),
            Err(error) => json!({"ok": false, "error": error.to_string()}),
        }
    }

    fn try_start_layer(&mut self) -> Result<Value, SessionError> {
        if self.graph.layer_summary()?.is_some() {
            return Err(SessionError::invalid(
                "the graph is already a layer: drop it first to start a new one",
            ));
        }
        if let Some(reason) = self.write_refusal() {
            return Err(SessionError::invalid(format!(
                "a layer cannot start over this graph: {reason}"
            )));
        }
        if !self.graph.as_any().is::<Graph<TraversalGraph>>() {
            return Err(SessionError::invalid(format!(
                "a layer starts only over a TraversalGraph, not over {}",
                self.graph.storage_type_name()
            )));
        }
        // The store in memory holds the graph too: close it, so the session owns the graph; the
        // store's job scheduler (built by the first job tick) holds it through its system jobs.
        self.stores = None;
        self.release_scheduler();
        if Arc::strong_count(&self.graph) != 1 {
            return Err(SessionError::invalid(
                "a layer cannot start over this graph: it is shared with another owner",
            ));
        }
        let graph = std::mem::replace(&mut self.graph, self.factory.empty());
        let any: Arc<dyn AnyGraph> = graph;
        let graph = any
            .into_any_arc()
            .downcast::<Graph<TraversalGraph>>()
            .ok()
            .and_then(|shared| Arc::try_unwrap(shared).ok())
            .ok_or_else(|| SessionError::failed("internal: the graph changed owner"))?;
        let base = LayerBase::from_graph(graph)?.with_name(self.name.clone());
        let layer = match base.new_layer() {
            Ok(layer) => layer,
            Err(err) => {
                // No layer reads the base: it is the graph again.
                if let Ok(graph) = base.into_graph() {
                    self.graph = Arc::new(graph);
                }
                return Err(err.into());
            }
        };
        self.graph = Arc::new(layer);
        self.layer_base = Some(base);
        self.info()
    }

    /// Discards the layer [`Session::start_layer`] started (its changes are lost) and serves
    /// its base again as a writable graph (kept as a store in memory again where the session
    /// keeps one): `{ok: true, info}` or `{ok: false, error}` (the graph is not a layer this
    /// session started, e.g. the dev server's `--layer`; the layer is shared with another
    /// owner), the session unchanged then.
    pub fn drop_layer(&mut self) -> Value {
        match self.try_drop_layer() {
            Ok(info) => json!({"ok": true, "info": info}),
            Err(error) => json!({"ok": false, "error": error.to_string()}),
        }
    }

    fn try_drop_layer(&mut self) -> Result<Value, SessionError> {
        if self.layer_base.is_none() {
            return Err(SessionError::invalid(
                "the graph is not a layer started in this session: there is no layer to drop",
            ));
        }
        if Arc::strong_count(&self.graph) != 1 {
            return Err(SessionError::invalid(
                "the layer cannot be dropped now: it is shared with another owner",
            ));
        }
        let Some(base) = self.layer_base.take() else {
            return Err(SessionError::failed("internal: the layer's base is gone"));
        };
        drop(std::mem::replace(&mut self.graph, self.factory.empty()));
        match base.into_graph() {
            Ok(graph) => {
                let (name, kind) = (self.name.clone(), self.kind.clone());
                self.replace(Arc::new(graph), &name, &kind);
                self.info()
            }
            Err(base) => {
                // Something else still reads the base: serve a fresh layer over it.
                self.graph = Arc::new(base.new_layer()?);
                self.layer_base = Some(base);
                Err(SessionError::failed(
                    "the base is still shared with another owner: the layer's changes were \
                     dropped, the layer stays",
                ))
            }
        }
    }
}
