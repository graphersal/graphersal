//! The read-only views of the playground: schema, graph drawing, memory usage, completions.

use std::collections::BTreeMap;

use graphersal::prelude::*;
use graphersal::schema::PropertySchema;
use graphersal::script::rhai;
use graphersal::storage::MemoryReportRequest;
use serde_json::{Map, Value, json};

use crate::display::{CompactGraph, Table};

/// Most vertex labels plus edge labels the schema views show; the generated large graph has one
/// label per vertex (110k), which no table, drawing or diagram can show usefully.
pub const SCHEMA_MAX_LABELS: usize = 300;

/// `{mode, source, table, json, mermaid, graph: {nodes, edges}, truncated, total_labels}` of the
/// graph's schema: the stored one (`source: "stored"`, `mode` its mode), or one inferred from the
/// data (`source: "inferred"`, `mode: "inferred"`). `json` is the canonical schema format. Beyond
/// [`SCHEMA_MAX_LABELS`] labels only the first ones are shown (`truncated: true`).
pub fn schema_view<S: GraphStorage>(g: &S) -> Value {
    match g.get_schema() {
        Some(schema) => {
            let mode = schema.mode.as_str();
            schema_view_with(g, schema, mode, "stored")
        }
        None => schema_view_with(g, g.infer_schema(), "inferred", "inferred"),
    }
}

/// [`schema_view`] of a given schema (`source: "given"`, `mode` its mode), with the label
/// counts of the graph `g`: what the playground draws for the schema its editor shows (a
/// draft, or one inferred earlier), so the drawings always show what the tables show.
pub fn schema_view_of<S: GraphStorage>(g: &S, schema: GraphSchema) -> Value {
    let mode = schema.mode.as_str();
    schema_view_with(g, schema, mode, "given")
}

fn schema_view_with<S: GraphStorage>(g: &S, full: GraphSchema, mode: &str, source: &str) -> Value {
    let total_labels = full.vertices.len() + full.edges.len();
    let truncated = total_labels > SCHEMA_MAX_LABELS;
    let schema = if truncated {
        let mut shown = full.clone();
        shown.vertices.truncate(SCHEMA_MAX_LABELS);
        shown.edges.truncate(SCHEMA_MAX_LABELS);
        let kept: std::collections::HashSet<String> = shown.vertices.keys().cloned().collect();
        for e in shown.edges.values_mut() {
            e.connections.retain(|c| {
                (c.from == "*" || kept.contains(&c.from)) && (c.to == "*" || kept.contains(&c.to))
            });
        }
        shown
    } else {
        full
    };
    let stats = g.statistics();
    let type_map = |root: &PropertySchema| -> Value {
        Value::Object(
            root.properties
                .iter()
                .map(|(k, node)| (k.clone(), Value::String(node.non_null_type_name())))
                .collect(),
        )
    };
    let mut nodes: Vec<Value> = schema
        .vertices
        .iter()
        .map(|(label, v)| {
            let mut node = json!({
                "id": format!("L:{label}"), "label": label,
                "properties": type_map(&v.schema),
            });
            if let Some(stats) = stats {
                node["count"] = json!(stats.vertex_label_count(label));
            }
            node
        })
        .collect();
    let mut edges = Vec::new();
    let mut wildcard = false;
    for (label, e) in &schema.edges {
        for c in &e.connections {
            wildcard |= c.from == "*" || c.to == "*";
            edges.push(json!({
                "id": format!("E:{label}:{}:{}", c.from, c.to),
                "label": label,
                "source": format!("L:{}", c.from),
                "target": format!("L:{}", c.to),
                "properties": type_map(&e.schema),
            }));
        }
    }
    if wildcard {
        nodes.push(json!({"id": "L:*", "label": "* (any label)", "properties": {}}));
    }
    json!({
        "mode": mode,
        "source": source,
        "table": schema_table(&schema),
        "json": schema.to_json_pretty(),
        "mermaid": schema.to_mermaid(),
        "graph": {"nodes": nodes, "edges": edges},
        "truncated": truncated,
        "total_labels": total_labels,
    })
}

/// One row per (element kind, label, property path), nested keys as `address.zip`: `element,
/// label, property, type, required, nullable, constraints, notes`.
pub fn schema_table(schema: &GraphSchema) -> Table {
    let rows = schema
        .property_rows()
        .into_iter()
        .map(|row| {
            let label = if row.connections.is_empty() {
                row.label
            } else {
                format!("{} ({})", row.label, row.connections)
            };
            vec![
                row.element.into(),
                label.into(),
                row.property.into(),
                row.ty.into(),
                row.required.into(),
                row.nullable.into(),
                row.constraints.into(),
                row.notes.into(),
            ]
        })
        .collect();
    Table {
        columns: [
            "element",
            "label",
            "property",
            "type",
            "required",
            "nullable",
            "constraints",
            "notes",
        ]
        .map(String::from)
        .to_vec(),
        rows,
    }
}
/// The whole graph to draw, as columns ([`CompactGraph`]): at most `max_nodes` vertices (the
/// first ones) and the edges between them (at most `max_edges`). Properties are fetched per
/// element by the inspector ([`crate::display::element_of`]).
pub fn graph_view<S: GraphStorage>(g: &S, max_nodes: usize, max_edges: usize) -> CompactGraph {
    let vertices: Vec<_> = g.vertices().take(max_nodes).collect();
    let ids: std::collections::HashSet<_> = vertices.iter().copied().collect();
    let edges: Vec<_> = g
        .edges()
        .filter(|e| {
            g.edge_endpoints(*e)
                .is_some_and(|(s, t)| ids.contains(&s) && ids.contains(&t))
        })
        .take(max_edges)
        .collect();
    let (total_nodes, total_edges) = (g.vertex_count(), g.edge_count());
    let mut view = CompactGraph::build(g, &vertices, &edges);
    view.truncated = vertices.len() < total_nodes || edges.len() < total_edges;
    view.total_nodes = total_nodes;
    view.total_edges = total_edges;
    view
}

/// A Graph panel request (`engine.graphView(limits, filter)`): `{maxNodes?, maxEdges?,
/// degrees?, filter?}`.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct GraphViewRequest {
    /// Most vertices drawn (default 500).
    pub max_nodes: Option<usize>,
    /// Most edges drawn (default 1000).
    pub max_edges: Option<usize>,
    /// Add the `vertexInDegree` / `vertexOutDegree` columns.
    pub degrees: bool,
    /// The filter ([`crate::filter`]); absent or `null`: everything.
    pub filter: Option<Value>,
}

/// [`graph_view`] for a request: without a filter exactly [`graph_view`] (no extra work), with
/// one the filter applied BEFORE the limits: the first `maxNodes` visible vertices (allow-list
/// form: labels in name order, each in the engine's order; otherwise the storage order) and the
/// visible edges between them (at most `maxEdges`), with `matchedNodes` / `matchedEdges` (every
/// element that passes, counted exactly) next to the totals. An invalid filter is an error.
pub fn graph_view_request<S: GraphStorage>(
    g: &S,
    request: &GraphViewRequest,
) -> Result<CompactGraph, String> {
    let max_nodes = request.max_nodes.unwrap_or(500);
    let max_edges = request.max_edges.unwrap_or(1000);
    let mut view = match request.filter.as_ref().filter(|f| !f.is_null()) {
        None => graph_view(g, max_nodes, max_edges),
        Some(spec) => {
            let filter = crate::filter::GraphFilter::compile(g, spec)?;
            filtered_graph_view(g, &filter, max_nodes, max_edges)?
        }
    };
    if request.degrees {
        view.add_degrees(g);
    }
    Ok(view)
}

/// The filtered drawing of [`graph_view_request`].
fn filtered_graph_view<S: GraphStorage>(
    g: &S,
    filter: &crate::filter::GraphFilter,
    max_nodes: usize,
    max_edges: usize,
) -> Result<CompactGraph, String> {
    use std::collections::HashSet;
    // Visible vertices: `None` = every vertex.
    let listed = filter.vertices(g, None)?;
    let listed_set: Option<HashSet<S::VertexId>> =
        listed.as_ref().map(|l| l.iter().copied().collect());
    let visible = |v: &S::VertexId| listed_set.as_ref().is_none_or(|set| set.contains(v));
    let endpoints_visible = |e: &S::EdgeId| {
        g.edge_endpoints(*e)
            .is_some_and(|(s, t)| visible(&s) && visible(&t))
    };
    // Visible edges: passing the edge filter, both endpoints visible.
    let edges: Vec<S::EdgeId> = match filter.edges(g, None)? {
        Some(passing) => passing.into_iter().filter(endpoints_visible).collect(),
        None => g.edges().filter(endpoints_visible).collect(),
    };
    let vertices: Vec<S::VertexId> = if filter.hide_isolated() {
        let mut touched = HashSet::with_capacity(edges.len());
        for e in &edges {
            if let Some((s, t)) = g.edge_endpoints(*e) {
                touched.insert(s);
                touched.insert(t);
            }
        }
        match listed {
            Some(list) => list.into_iter().filter(|v| touched.contains(v)).collect(),
            None => g.vertices().filter(|v| touched.contains(v)).collect(),
        }
    } else {
        match listed {
            Some(list) => list,
            None => g.vertices().collect(),
        }
    };
    let (matched_nodes, matched_edges) = (vertices.len(), edges.len());
    let drawn: Vec<S::VertexId> = vertices.into_iter().take(max_nodes).collect();
    let drawn_set: HashSet<S::VertexId> = drawn.iter().copied().collect();
    let drawn_edges: Vec<S::EdgeId> = edges
        .into_iter()
        .filter(|e| {
            g.edge_endpoints(*e)
                .is_some_and(|(s, t)| drawn_set.contains(&s) && drawn_set.contains(&t))
        })
        .take(max_edges)
        .collect();
    let mut view = CompactGraph::build(g, &drawn, &drawn_edges);
    view.truncated = drawn.len() < matched_nodes || drawn_edges.len() < matched_edges;
    view.total_nodes = g.vertex_count();
    view.total_edges = g.edge_count();
    view.matched_nodes = Some(matched_nodes);
    view.matched_edges = Some(matched_edges);
    Ok(view)
}

/// The Rhai query selecting what the filter `spec` shows ([`crate::filter::filter_query`]).
pub fn graph_filter_query<S: GraphStorage>(g: &S, spec: &Value) -> Result<String, String> {
    let filter = crate::filter::GraphFilter::compile(g, spec)?;
    Ok(crate::filter::filter_query(&filter))
}

/// Most labels per kind the statistics view lists (the largest counts first).
pub const STATISTICS_MAX_LABELS: usize = 50;

/// `g.statistics()` as data for the statistics panel, or `null` when the storage keeps none:
/// `{vertex_count, edge_count, vertex_labels: [{label, count, primary}], edge_labels: [{label,
/// count}], vertex_label_total, edge_label_total, truncated}`. `count` is the vertices carrying
/// the label anywhere in their label set, `primary` those whose first label it is. Labels are
/// ordered by count (largest first, then by name); beyond [`STATISTICS_MAX_LABELS`] per kind only
/// the first are listed (`truncated: true`; the totals count all).
pub fn statistics_view<S: GraphStorage>(g: &S) -> Value {
    let Some(stats) = g.statistics() else {
        return Value::Null;
    };
    let mut vertex_labels = stats.vertex_label_counts();
    let mut edge_labels = stats.edge_label_counts();
    for counts in [&mut vertex_labels, &mut edge_labels] {
        counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    }
    let truncated =
        vertex_labels.len() > STATISTICS_MAX_LABELS || edge_labels.len() > STATISTICS_MAX_LABELS;
    let vertices: Vec<Value> = vertex_labels
        .iter()
        .take(STATISTICS_MAX_LABELS)
        .map(|(label, count)| {
            json!({
                "label": label,
                "count": count,
                "primary": stats.primary_vertex_label_count(label),
            })
        })
        .collect();
    let edges: Vec<Value> = edge_labels
        .iter()
        .take(STATISTICS_MAX_LABELS)
        .map(|(label, count)| json!({ "label": label, "count": count }))
        .collect();
    json!({
        "vertex_count": stats.vertex_count(),
        "edge_count": stats.edge_count(),
        "vertex_labels": vertices,
        "edge_labels": edges,
        "vertex_label_total": vertex_labels.len(),
        "edge_label_total": edge_labels.len(),
        "truncated": truncated,
    })
}

/// `g.memory_usage()` as data: `{structure_bytes, index_bytes, property_bytes, total_bytes,
/// total_mb, compressed_values, compressed_plain_bytes, compressed_stored_bytes,
/// compression_dictionary_bytes, property_index_bytes, deduped_values, deduped_plain_bytes,
/// dedup_table_bytes, shape_bytes, workload_bytes}`, or `null`. `shape_bytes` and
/// `dedup_table_bytes` are parts of `property_bytes`, never to be added to the total again;
/// `workload_bytes` (the graph's workload statistics, held beside the storage) is not part of the
/// total either and is `0` from a storage alone ([`SessionGraph::memory_view`] fills it in).
pub fn memory_usage<S: GraphStorage>(g: &S) -> Value {
    match g.memory_usage() {
        Some(stats) => json!({
            "structure_bytes": stats.structure_bytes,
            "index_bytes": stats.index_bytes,
            "property_bytes": stats.property_bytes,
            "total_bytes": stats.total_bytes(),
            "total_mb": stats.total_mb(),
            "compressed_values": stats.compressed_values,
            "compressed_plain_bytes": stats.compressed_plain_bytes,
            "compressed_stored_bytes": stats.compressed_stored_bytes,
            "compression_dictionary_bytes": stats.compression_dictionary_bytes,
            "property_index_bytes": stats.property_index_bytes,
            "deduped_values": stats.deduped_values,
            "deduped_plain_bytes": stats.deduped_plain_bytes,
            "dedup_table_bytes": stats.dedup_table_bytes,
            "shape_bytes": stats.shape_bytes,
            "workload_bytes": stats.workload_bytes,
        }),
        None => Value::Null,
    }
}

/// The request of a memory report spec `{sample_every?, max_elements?, max_rows?,
/// path_depth?}` (`null`: a full report; [`crate::Session::memory_report`]), or what is wrong
/// with it.
pub fn memory_report_request(spec: &Value) -> Result<MemoryReportRequest, String> {
    const KEYS: [&str; 5] = [
        "sample_every",
        "max_elements",
        "max_rows",
        "max_labels",
        "path_depth",
    ];
    let mut request = MemoryReportRequest::new();
    let fields = match spec {
        Value::Null => return Ok(request),
        Value::Object(fields) => fields,
        _ => {
            return Err(
                "the memory report spec is a JSON object {sample_every?, max_elements?, \
                 max_rows?, max_labels?, path_depth?}"
                    .into(),
            );
        }
    };
    if let Some(unknown) = fields.keys().find(|key| !KEYS.contains(&key.as_str())) {
        return Err(format!(
            "unknown key '{unknown}' in the memory report spec (keys: {})",
            KEYS.join(", ")
        ));
    }
    let positive = |key: &str| -> Result<Option<u64>, String> {
        match fields.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(value) => match value.as_u64() {
                Some(n) if n > 0 => Ok(Some(n)),
                _ => Err(format!("{key} must be a positive integer, not {value}")),
            },
        }
    };
    if let Some(k) = positive("sample_every")? {
        request = request.with_sample_every(k);
    }
    if let Some(n) = positive("max_elements")? {
        request = request.with_max_elements(n);
    }
    if let Some(n) = positive("max_rows")? {
        request = request.with_max_rows(usize::try_from(n).unwrap_or(usize::MAX));
    }
    if let Some(n) = positive("max_labels")? {
        request = request.with_max_labels(usize::try_from(n).unwrap_or(usize::MAX));
    }
    if let Some(depth) = positive("path_depth")? {
        request = request.with_path_depth(u32::try_from(depth).unwrap_or(u32::MAX));
    }
    request
        .validate()
        .map_err(|err| graphersal::error::format_diagnostic(&err))?;
    Ok(request)
}

/// The receiver a DSL function is called on, from the Rhai type of its first parameter (the
/// display name or the Rust type path the metadata shows): `traversal` for `g.x()`/`__.x()`,
/// the token class for `P.x()`, ..., `global` for a free function (`jpath(..)`, `gt(30)`). The
/// same classification as the engine's doc table (`graphersal/src/script/register/docs.rs`).
fn receiver_of(first_param_type: Option<&str>) -> &'static str {
    let Some(ty) = first_param_type else {
        return "global";
    };
    let ty = ty.trim_start_matches("&mut ").trim_start_matches('&');
    let base = ty.split('<').next().unwrap_or(ty);
    match base.rsplit("::").next().unwrap_or(base) {
        "ScriptGraphTraversalSource" | "GraphTraversalSource" => "traversal",
        "ScriptP" | "P" => "P",
        "ScriptTextP" | "TextP" => "TextP",
        "ScriptBy" | "By" => "By",
        "ScriptUuidTokens" | "UUIDTokens" => "UUID",
        "ScriptSetTokens" | "SetTokens" => "Set",
        "ScriptMap" | "Map" => "Map",
        "TraversalMetrics" => "TraversalMetrics",
        "ScriptExecution" | "Execution" => "Execution",
        "ScriptGraphSource" | "GraphSource" => "GraphSource",
        "ScriptCardinality" | "CardinalityTokens" => "Cardinality",
        "ScriptV" | "V" => "V",
        "ScriptCastPolicy" | "CastPolicy" => "CastPolicy",
        "ArgP" | "Predicate" => "Predicate",
        "ScriptUuid" | "Uuid" => "Uuid",
        "ScriptVector" | "Vector" => "Vector",
        "ScriptFake" | "Fake" => "Fake",
        "DateTime" | "datetime" | "Date" | "date" | "LocalDateTime" | "localdatetime" | "Time"
        | "time" | "Duration" | "duration" => "temporal",
        "BTreeMap" | "map" => "map",
        _ => "global",
    }
}

/// The spelling-independent key of a function name, which merges its spellings into one entry:
/// lowercase without underscores (`hasLabel`, `has_label` -> `haslabel`; `in_` -> `in`;
/// `toPlantUML`, `to_plantuml` -> `toplantuml`).
fn spelling_key(name: &str) -> String {
    name.chars()
        .filter(|c| *c != '_')
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// One DSL function of the completion catalogue ([`completion_entries`]), all its spellings
/// merged: what the playground's completion list and tooltips and the CLI's Tab completion and
/// `/help` show.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct CompletionEntry {
    /// The preferred spelling: the shortest snake_case one (`has_label`, `in`).
    pub name: String,
    /// The other spellings (`hasLabel`, `in_`).
    pub aliases: Vec<String>,
    /// What the function is called on: `traversal` (steps, terminals and source methods of
    /// `g`/`__`), a token class (`P`, `TextP`, `UUID`, ...), or `global` for a free function.
    pub receiver: String,
    /// The argument counts of its overloads, ascending.
    pub arities: Vec<usize>,
    /// The overloads as argument lists (`(label: string)`), short ones first.
    pub signatures: Vec<String>,
    /// The engine's doc text: one sentence, an empty line, `Example: ...` with its result.
    pub doc: String,
}

impl CompletionEntry {
    /// Every spelling, the preferred one first.
    pub fn spellings(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.name.as_str()).chain(self.aliases.iter().map(String::as_str))
    }

    /// The Gremlin camelCase spelling (`hasLabel`, `V`) when the function has one, else the
    /// preferred spelling (one-word names such as `out` are the same in both).
    pub fn camel_name(&self) -> &str {
        self.spellings()
            .find(|s| s.chars().any(|c| c.is_ascii_uppercase()) && !s.contains('_'))
            .unwrap_or(&self.name)
    }
}

/// Every DSL function the engine registers, from the Rhai engine's own metadata (names,
/// parameter names and types per overload, doc comments): `[{name, aliases, receiver, arities,
/// signatures, doc}]`, all spellings of one function merged. `doc` is the function's text from
/// the engine's doc table: one sentence, an empty line, `Example: ...` (with its result).
pub fn completions(engine: &rhai::Engine) -> Value {
    let list: Vec<Value> = completion_entries(engine)
        .into_iter()
        .map(|e| {
            let mut obj = Map::new();
            obj.insert("name".into(), Value::String(e.name));
            obj.insert("aliases".into(), json!(e.aliases));
            obj.insert("receiver".into(), Value::String(e.receiver));
            obj.insert("arities".into(), json!(e.arities));
            obj.insert("signatures".into(), json!(e.signatures));
            obj.insert("doc".into(), Value::String(e.doc));
            Value::Object(obj)
        })
        .collect();
    Value::Array(list)
}

/// The engine's function metadata as JSON (`Null` when it cannot be produced).
fn fn_metadata(engine: &rhai::Engine) -> Value {
    engine
        .gen_fn_metadata_to_json(false)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(Value::Null)
}

/// The values of the DSL's token classes (`Order` -> `asc`, `desc`, `shuffle`; `T` -> `id`,
/// `label`, ...), from the getters the engine registers on their marker types: class name ->
/// member names, sorted. A member registered in two cases (`asc` and `Asc`) is listed once, in
/// the lowercase form. The map also holds the getters of non-token types (`Execution` ->
/// `results`, ...); a caller keeps the classes its scope defines.
pub fn token_values(engine: &rhai::Engine) -> BTreeMap<String, Vec<String>> {
    let metadata = fn_metadata(engine);
    let mut classes: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for f in metadata
        .get("functions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(member) = f
            .get("name")
            .and_then(Value::as_str)
            .and_then(|n| n.strip_prefix("get$"))
        else {
            continue;
        };
        let Some(ty) = f
            .get("params")
            .and_then(Value::as_array)
            .and_then(|p| p.first())
            .and_then(|p| p.get("type"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        let ty = ty.trim_start_matches("&mut ").trim_start_matches('&');
        let ty = ty.rsplit("::").next().unwrap_or(ty);
        let class = ty.strip_suffix("Tokens").unwrap_or(ty);
        let class = class
            .strip_prefix("Script")
            .filter(|c| !c.is_empty())
            .unwrap_or(class);
        let members = classes.entry(class.to_string()).or_default();
        if !members.iter().any(|m| m == member) {
            members.push(member.to_string());
        }
    }
    for members in classes.values_mut() {
        let lower: Vec<String> = members
            .iter()
            .filter(|m| m.starts_with(|c: char| c.is_ascii_lowercase()))
            .cloned()
            .collect();
        // `Asc` goes when `asc` exists; an all-uppercase or single-form member stays.
        members.retain(|m| {
            !m.starts_with(|c: char| c.is_ascii_uppercase())
                || m.chars().all(|c| !c.is_ascii_lowercase())
                || !lower.iter().any(|l| l.eq_ignore_ascii_case(m))
        });
        members.sort();
    }
    classes
}

/// The completion catalogue as typed entries: every DSL function the engine registers (see
/// [`completions`], its JSON form), sorted by receiver and spelling-independent name.
pub fn completion_entries(engine: &rhai::Engine) -> Vec<CompletionEntry> {
    let metadata = fn_metadata(engine);
    // (receiver, canonical name) -> entry
    struct Entry {
        spellings: Vec<String>,
        arities: Vec<usize>,
        signatures: Vec<String>,
        doc: String,
    }
    let mut entries: BTreeMap<(String, String), Entry> = BTreeMap::new();
    let functions = metadata
        .get("functions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for f in &functions {
        let Some(name) = f.get("name").and_then(Value::as_str) else {
            continue;
        };
        if name.starts_with(|c: char| !c.is_ascii_alphabetic())
            || name.starts_with("get$")
            || name.starts_with("set$")
            || matches!(name, "to_string" | "to_debug")
            || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            continue;
        }
        let params: Vec<&Value> = f
            .get("params")
            .and_then(Value::as_array)
            .map(|p| p.iter().collect())
            .unwrap_or_default();
        let first_type = params
            .first()
            .and_then(|p| p.get("type"))
            .and_then(Value::as_str);
        let receiver = receiver_of(first_type);
        // A method's first parameter is its receiver; a free function's are all arguments
        // (some module functions only report their count).
        let skip = usize::from(receiver != "global");
        let mut args: Vec<String> = params
            .iter()
            .skip(skip)
            .enumerate()
            .map(|(i, p)| {
                let pname = p.get("name").and_then(Value::as_str).unwrap_or("_");
                let pname = if pname == "_" || pname.is_empty() {
                    format!("arg{}", i + 1)
                } else {
                    pname.to_string()
                };
                match p.get("type").and_then(Value::as_str) {
                    Some(ty) if !ty.is_empty() => format!("{pname}: {}", short_type(ty)),
                    _ => pname,
                }
            })
            .collect();
        if params.is_empty() {
            let count = f.get("numParams").and_then(Value::as_u64).unwrap_or(0);
            args = (1..=count).map(|i| format!("arg{i}")).collect();
        }
        // One element per comment block; a plugin function's block holds several `///` lines.
        let doc = f
            .get("docComments")
            .and_then(Value::as_array)
            .map(|blocks| {
                blocks
                    .iter()
                    .filter_map(Value::as_str)
                    .flat_map(str::lines)
                    .map(|l| l.trim_start_matches("///").trim_start_matches("/**").trim())
                    .collect::<Vec<_>>()
                    .join("\n")
                    .trim()
                    .to_string()
            })
            .unwrap_or_default();
        let entry = entries
            .entry((receiver.to_string(), spelling_key(name)))
            .or_insert_with(|| Entry {
                spellings: Vec::new(),
                arities: Vec::new(),
                signatures: Vec::new(),
                doc: String::new(),
            });
        if !entry.spellings.iter().any(|s| s == name) {
            entry.spellings.push(name.to_string());
        }
        if !entry.arities.contains(&args.len()) {
            entry.arities.push(args.len());
        }
        let signature = format!("({})", args.join(", "));
        if !entry.signatures.contains(&signature) {
            entry.signatures.push(signature);
        }
        if entry.doc.is_empty() {
            entry.doc = doc;
        }
    }
    entries
        .into_iter()
        .map(|((receiver, _), mut e)| {
            e.arities.sort_unstable();
            // Rhai lists same-arity overloads in hash-map order, which depends on registration
            // history: order them by arity, then text, so the list is stable.
            let arity = |s: &str| {
                if s == "()" {
                    0
                } else {
                    s.matches(", ").count() + 1
                }
            };
            e.signatures
                .sort_by(|a, b| arity(a).cmp(&arity(b)).then_with(|| a.cmp(b)));
            // A variadic step registers one overload per arity: show the short ones and one
            // summary line instead of ten.
            if e.signatures.len() > 4 {
                let max = e.arities.last().copied().unwrap_or_default();
                e.signatures.retain(|s| s.matches(", ").count() < 2);
                e.signatures.push(format!("(arg1, …, arg{max})"));
            }
            // Preferred spelling: the shortest snake_case one (`in` over `in_`, `to_plantuml`
            // over `toPlantUML`), else the first registered.
            let name = e
                .spellings
                .iter()
                .filter(|s| !s.chars().any(|c| c.is_ascii_uppercase()))
                .min_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)))
                .or(e.spellings.first())
                .cloned()
                .unwrap_or_default();
            let aliases: Vec<String> = e.spellings.into_iter().filter(|s| *s != name).collect();
            CompletionEntry {
                name,
                aliases,
                receiver,
                arities: e.arities,
                signatures: e.signatures,
                doc: e.doc,
            }
        })
        .collect()
}

/// A Rust type path as a short, readable type name (`graphersal::script::ArgP` -> `ArgP`,
/// `rhai::Dynamic` -> `any`).
fn short_type(ty: &str) -> String {
    let base = ty.trim_start_matches("&mut ").trim_start_matches('&');
    let last = base.rsplit("::").next().unwrap_or(base);
    match last {
        "Dynamic" => "any".to_string(),
        "ImmutableString" | "String" | "&str" | "str" => "string".to_string(),
        "INT" => "int".to_string(),
        "FLOAT" => "float".to_string(),
        "ScriptGraphTraversalSource" => "traversal".to_string(),
        "ArgP" | "Predicate" => "predicate".to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completions_cover_both_spellings_of_steps() {
        let engine = graphersal::script::engine();
        let list = completions(&engine);
        let list = list.as_array().unwrap();
        let has_label = list
            .iter()
            .find(|e| e["name"] == "has_label" && e["receiver"] == "traversal")
            .expect("has_label");
        assert!(
            has_label["aliases"]
                .as_array()
                .unwrap()
                .iter()
                .any(|a| a == "hasLabel"),
            "{has_label}"
        );
        assert!(list.iter().any(|e| e["receiver"] == "P"), "P predicates");
        assert!(list.len() > 100, "{}", list.len());
    }

    #[test]
    fn token_values_come_from_the_registered_getters() {
        let engine = graphersal::script::engine();
        let tokens = token_values(&engine);
        assert_eq!(tokens["Order"], ["asc", "desc", "shuffle"]);
        assert_eq!(tokens["T"], ["id", "key", "label", "value"]);
        assert_eq!(tokens["Pop"], ["all", "first", "last", "mixed"]);
        assert!(tokens["GType"].iter().any(|m| m == "LONG"), "{tokens:?}");
        assert!(tokens["Column"].iter().any(|m| m == "keys"), "{tokens:?}");
    }

    #[test]
    fn typed_entries_name_their_camel_spelling() {
        let engine = graphersal::script::engine();
        let entries = completion_entries(&engine);
        let find = |name: &str| {
            entries
                .iter()
                .find(|e| e.receiver == "traversal" && e.name == name)
                .unwrap_or_else(|| panic!("{name}"))
        };
        assert_eq!(find("has_label").camel_name(), "hasLabel");
        assert_eq!(find("out").camel_name(), "out");
        assert_eq!(
            completions(&engine).as_array().unwrap().len(),
            entries.len()
        );
    }

    #[test]
    fn every_completion_has_a_doc_with_an_example() {
        let engine = graphersal::script::engine();
        let list = completions(&engine);
        let list = list.as_array().unwrap();
        for entry in list {
            let doc = entry["doc"].as_str().unwrap();
            assert!(
                doc.contains("\n\nExample: `") && !doc.contains("///"),
                "{entry}"
            );
        }
        let find = |receiver: &str, name: &str| {
            list.iter()
                .find(|e| e["receiver"] == receiver && e["name"] == name)
                .unwrap_or_else(|| panic!("{receiver}.{name}"))
        };
        // All spellings merge into one entry, named by the shortest snake_case spelling.
        assert_eq!(find("traversal", "in")["aliases"], json!(["in_"]));
        assert_eq!(
            find("traversal", "has_label")["aliases"],
            json!(["hasLabel"])
        );
        // A Graphersal extension has one spelling (snake_case only since 0.3.0).
        assert_eq!(find("traversal", "to_plantuml")["aliases"], json!([]));
        // Free functions are listed too, with all their parameters.
        assert_eq!(
            find("global", "jpath")["signatures"],
            json!(["(arg1: string)"])
        );
        assert!(
            find("traversal", "has_label")["doc"]
                .as_str()
                .unwrap()
                .starts_with("Keeps elements whose label")
        );
    }

    #[test]
    fn schema_view_infers_without_a_stored_schema() {
        let graph = GraphSource::tinkerpop_modern();
        let view = schema_view(&*graph.read());
        assert_eq!(view["mode"], "inferred");
        assert_eq!(view["source"], "inferred");
        assert_eq!(view["table"]["columns"][4], "required");
        assert_eq!(view["graph"]["nodes"].as_array().unwrap().len(), 2);
        assert_eq!(view["graph"]["edges"].as_array().unwrap().len(), 2);
        assert!(view["mermaid"].as_str().unwrap().contains("person"));
    }

    #[test]
    fn graph_view_respects_the_caps() {
        let graph = GraphSource::tinkerpop_modern();
        let view = serde_json::to_value(graph_view(&*graph.read(), 3, 100)).unwrap();
        assert_eq!(view["vertexIds"].as_array().unwrap().len(), 3);
        assert_eq!(view["truncated"], true);
        assert_eq!(view["totalNodes"], 6);
        assert_eq!(view["totalEdges"], 6);
    }

    #[test]
    fn graph_view_is_columnar_without_properties() {
        let graph = GraphSource::tinkerpop_modern();
        let view = serde_json::to_value(graph_view(&*graph.read(), 100, 100)).unwrap();
        let ids = view["vertexIds"].as_array().unwrap();
        assert_eq!(ids.len(), 6);
        assert_eq!(view["vertexName"].as_array().unwrap().len(), 6);
        assert_eq!(view["edgeIds"].as_array().unwrap().len(), 6);
        assert_eq!(view["truncated"], false);
        assert!(view.get("nodes").is_none() && view.get("properties").is_none());
        let labels: Vec<&str> = view["labels"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l.as_str().unwrap())
            .collect();
        assert_eq!(labels, ["person", "software", "knows", "created"]);
        // Vertex 1 (marko) knows vertex 2 (vadas): edge 0, by indices into the vertex columns.
        let index_of = |id: &str| ids.iter().position(|v| v == id).unwrap() as u64;
        let e0 = view["edgeIds"]
            .as_array()
            .unwrap()
            .iter()
            .position(|e| e == "0")
            .unwrap();
        assert_eq!(view["edgeSource"][e0].as_u64().unwrap(), index_of("1"));
        assert_eq!(view["edgeTarget"][e0].as_u64().unwrap(), index_of("2"));
        assert_eq!(
            labels[view["edgeLabel"][e0].as_u64().unwrap() as usize],
            "knows"
        );
        let marko = index_of("1") as usize;
        assert_eq!(view["vertexName"][marko], "marko");
        assert_eq!(
            labels[view["vertexLabel"][marko].as_u64().unwrap() as usize],
            "person"
        );
    }

    #[test]
    fn graph_view_keeps_only_edges_between_drawn_vertices() {
        let graph = GraphSource::tinkerpop_modern();
        let view = graph_view(&*graph.read(), 2, 100);
        let n = view.vertex_ids.len() as u32;
        assert!(
            view.edge_source
                .iter()
                .chain(&view.edge_target)
                .all(|i| *i < n)
        );
        // Unnamed and unlabeled vertices have a null caption and the empty label.
        let graph = GraphSource::empty();
        graph
            .write()
            .add_vertex(None, Vec::new(), PropertyMap::default())
            .unwrap();
        let view = serde_json::to_value(graph_view(&*graph.read(), 10, 10)).unwrap();
        assert_eq!(view["vertexName"], json!([null]));
        assert_eq!(view["labels"], json!([""]));
    }

    /// The modern graph twice: as a `TraversalGraph` and wrapped in the `Forwarding` storage
    /// (not `TraversalGraph`, own handle types), for the generic-helper comparisons.
    fn modern_pair() -> (
        TraversalGraph,
        graphersal_storage_tests::forwarding::Forwarding,
    ) {
        (
            TraversalGraph::tinkerpop_modern(),
            graphersal_storage_tests::forwarding::Forwarding::new(
                TraversalGraph::tinkerpop_modern(),
            ),
        )
    }

    #[test]
    fn views_on_a_foreign_storage_equal_those_of_a_traversal_graph() {
        let (tg, fw) = modern_pair();
        assert_eq!(schema_view(&tg), schema_view(&fw));
        let drawn = |g: CompactGraph| serde_json::to_value(g).unwrap();
        assert_eq!(drawn(graph_view(&tg, 4, 3)), drawn(graph_view(&fw, 4, 3)));
        assert_eq!(
            drawn(graph_view(&tg, 100, 100)),
            drawn(graph_view(&fw, 100, 100))
        );
        assert_eq!(statistics_view(&tg), statistics_view(&fw));
        assert!(statistics_view(&fw).is_object());
        assert_eq!(memory_usage(&tg), memory_usage(&fw));
    }
}
