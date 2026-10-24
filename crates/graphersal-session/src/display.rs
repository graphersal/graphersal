//! Display shapes of results (a table, JSON text) within the display limits, and the elements
//! the results refer to (the "subgraph" the Graph panel highlights).
//!
//! Display is bounded, data never: `execution.results` always holds everything; only what is
//! shown here is cut, with the CLI's `…` marker. Values stay [`ElementProperty`] until they are
//! serialized, so map keys keep the engine's order (a `serde_json::Value` would sort them).

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use graphersal::exec::RenderOptions;
use graphersal::prelude::CompactString;
use graphersal::prelude::*;
use graphersal::storage::ElementKind;
use serde::Serialize;

/// The `…` marker of a cut collection, as in the CLI.
const CUT: &str = "…";

/// Vertices and edges a result's subgraph carries by default ([`crate::RunOptions`]
/// `subgraph_max_elements`, the playground's setting "Result graph"). Measured in GR4
/// (`playground/README.md`, "Graph drawing"): the compact columns of 50 000 elements are about
/// 1.4 MB of JSON and reach the page in tens of milliseconds; the Fast tier draws them smoothly.
pub const DEFAULT_SUBGRAPH_MAX_ELEMENTS: usize = 50_000;

/// The largest subgraph a run may ask for (the end of the Fast tier's range,
/// `TIER_TUNING.fastMax` in `playground/js/graph/tiers.js`).
pub const SUBGRAPH_MAX_ELEMENTS: usize = 200_000;

/// The byte bound of a result's subgraph: the estimated size of its JSON columns (ids, captions,
/// indices). A result whose elements carry very long ids or names stops here before the element
/// limit (`stoppedBy: "bytes"`).
pub const SUBGRAPH_MAX_BYTES: usize = 16 * 1024 * 1024;

/// Most columns a table of maps gets before it falls back to one `value` column.
const MAX_COLUMNS: usize = 50;

/// The components of a vector a table cell previews.
pub const VECTOR_PREVIEW: usize = 16;

/// A table cell's value: a vector becomes the marker the playground shows as
/// `vector[768 f32]` with an expandable preview, `{"@type": "graphersal:Vector", "element",
/// "dimension", "preview": [first values]}` (the JSON view keeps the full list of values);
/// anything else is unchanged, except that its non-finite floats become their
/// [`non_finite_marker`] ([`json_safe`]).
pub fn table_cell(mut value: ElementProperty) -> ElementProperty {
    let ElementProperty::Vector(vector) = &value else {
        make_json_safe(&mut value);
        return value;
    };
    let mut marker = PropertyMap::default();
    marker.insert("@type", ElementProperty::from("graphersal:Vector"));
    marker.insert("element", ElementProperty::from(vector.element().name()));
    marker.insert(
        "dimension",
        ElementProperty::from(i64::try_from(vector.dimension()).unwrap_or(i64::MAX)),
    );
    marker.insert(
        "preview",
        ElementProperty::Array(
            vector
                .to_f32_vec()
                .into_iter()
                .take(VECTOR_PREVIEW)
                .map(|value| ElementProperty::from(f64::from(value)))
                .collect(),
        ),
    );
    ElementProperty::Object(Box::new(marker))
}

/// The GraphSON 3 tag of a double, which carries a non-finite float in the session's JSON.
pub const DOUBLE_TAG: &str = "g:Double";

/// The JSON form of a non-finite float in every answer of the session (results, the JSON and
/// Table tabs, the inspector): GraphSON's typed double `{"@type": "g:Double", "@value": "NaN"}`
/// (payloads `"NaN"`, `"Infinity"`, `"-Infinity"`), the text the engine's own `to_json()` and
/// `json` visualizer write. JSON has no number for these, and the tag keeps the value apart from
/// the string `"NaN"`; the playground shows it as `NaN` / `Infinity` / `-Infinity`
/// (`playground/js/util.js`). `None` for a finite number or another value.
pub fn non_finite_marker(value: &ElementProperty) -> Option<ElementProperty> {
    let ElementProperty::Number(number) = value else {
        return None;
    };
    if number.as_i64().is_some() {
        return None;
    }
    let f = number.as_f64()?;
    let text = if f.is_nan() {
        "NaN"
    } else if f == f64::INFINITY {
        "Infinity"
    } else if f == f64::NEG_INFINITY {
        "-Infinity"
    } else {
        return None;
    };
    let mut marker = PropertyMap::default();
    marker.insert("@type", ElementProperty::from(DOUBLE_TAG));
    marker.insert("@value", ElementProperty::from(text));
    Some(ElementProperty::Object(Box::new(marker)))
}

/// `value` ready for JSON: a copy with every non-finite float (at any depth: lists, maps, map
/// keys, the properties of a vertex or edge value) replaced by its [`non_finite_marker`], or
/// `value` itself when it holds none (no copy). The plain serde of a value refuses a non-finite
/// float, so every value the session serializes goes through this (or [`json_safe_map`]) first;
/// the display logic (table columns, the result graph) keeps working on the value itself.
pub fn json_safe(value: &ElementProperty) -> Cow<'_, ElementProperty> {
    if has_non_finite(value) {
        let mut safe = value.clone();
        make_json_safe(&mut safe);
        Cow::Owned(safe)
    } else {
        Cow::Borrowed(value)
    }
}

/// The JSON text of a list of values ([`json_safe`] of each).
pub fn values_json(values: &[ElementProperty]) -> String {
    let safe: Vec<Cow<'_, ElementProperty>> = values.iter().map(json_safe).collect();
    serde_json::to_string(&safe).unwrap_or_else(|_| "[]".into())
}

/// [`json_safe`] in place, for every value of a property map.
pub fn json_safe_map(map: &mut PropertyMap) {
    map.values_mut().for_each(make_json_safe);
}

fn make_json_safe(value: &mut ElementProperty) {
    if let Some(marker) = non_finite_marker(value) {
        *value = marker;
        return;
    }
    match value {
        ElementProperty::Array(items) => items.iter_mut().for_each(make_json_safe),
        ElementProperty::Object(map) => json_safe_map(map),
        ElementProperty::Vertex(vertex) => json_safe_map(vertex.properties_mut()),
        ElementProperty::Edge(edge) => json_safe_map(edge.properties_mut()),
        ElementProperty::Map(map)
            if map
                .iter()
                .any(|(k, v)| has_non_finite(k) || has_non_finite(v)) =>
        {
            let mut safe = ValueMap::with_capacity(map.len());
            for (key, item) in map.iter() {
                let (mut key, mut item) = (key.clone(), item.clone());
                make_json_safe(&mut key);
                make_json_safe(&mut item);
                safe.insert(key, item);
            }
            *value = safe.into_property();
        }
        _ => {}
    }
}

/// Whether `value` holds a non-finite float anywhere.
fn has_non_finite(value: &ElementProperty) -> bool {
    match value {
        ElementProperty::Number(_) => non_finite_marker(value).is_some(),
        ElementProperty::Array(items) => items.iter().any(has_non_finite),
        ElementProperty::Object(map) => map.values().any(has_non_finite),
        ElementProperty::Vertex(vertex) => vertex.properties().values().any(has_non_finite),
        ElementProperty::Edge(edge) => edge.properties().values().any(has_non_finite),
        ElementProperty::Map(map) => map
            .iter()
            .any(|(k, v)| has_non_finite(k) || has_non_finite(v)),
        _ => false,
    }
}

/// Display limits: `None` is unbounded.
#[derive(Debug, Clone, Copy)]
pub struct Bounds {
    /// Top-level rows shown.
    pub max_rows: Option<usize>,
    /// Items shown of every nested collection.
    pub max_items: Option<usize>,
}

impl Bounds {
    /// The limits of a [`RenderOptions`].
    pub fn of(render: &RenderOptions) -> Self {
        Self {
            max_rows: render.max_rows,
            max_items: render.max_items,
        }
    }

    /// Show everything.
    pub fn unlimited() -> Self {
        Self {
            max_rows: None,
            max_items: None,
        }
    }
}

/// `{columns, rows}`: what the Results panel's Table tab shows.
#[derive(Debug, Clone, Serialize)]
pub struct Table {
    /// Column names.
    pub columns: Vec<String>,
    /// One list of cells per row.
    pub rows: Vec<Vec<ElementProperty>>,
}

/// The display of a result: `{table, json, raw, notice, total_rows}` (engine.js).
#[derive(Debug, Clone, Serialize)]
pub struct Display {
    /// The Table tab.
    pub table: Table,
    /// The JSON tab (pretty-printed, within the limits).
    pub json: String,
    /// The Raw tab: the engine's own visualizer output (what the CLI prints).
    pub raw: String,
    /// The truncation notice, when something was cut.
    pub notice: Option<String>,
    /// How many results there are (a list result), `None` for a single value.
    pub total_rows: Option<usize>,
}

/// The display of `results` (one row per item when `as_rows`, else the single value). `raw` is
/// filled in by the caller from the engine's visualizer.
pub fn bounded_display(results: &[ElementProperty], as_rows: bool, bounds: &Bounds) -> Display {
    let total = results.len();
    let shown_rows = bounds.max_rows.map_or(total, |n| n.min(total));
    let mut nested_cut = false;
    let rows: Vec<ElementProperty> = results[..shown_rows]
        .iter()
        .map(|row| cut_nested(row, bounds.max_items, &mut nested_cut))
        .collect();
    let notice = if shown_rows < total {
        Some(format!(
            "{CUT} showing the first {shown_rows} rows; more exist. Raise \"Max rows\" in the \
             settings, or override one call with to_table(#{{max_rows: n}}) / #{{no_limit: true}}."
        ))
    } else if nested_cut {
        Some(format!(
            "{CUT} nested collections are cut to {} items; raise \"Max items\" in the settings.",
            bounds.max_items.unwrap_or_default()
        ))
    } else {
        None
    };
    let json = if as_rows {
        let safe: Vec<Cow<'_, ElementProperty>> = rows.iter().map(json_safe).collect();
        serde_json::to_string_pretty(&safe)
    } else {
        serde_json::to_string_pretty(&json_safe(rows.first().unwrap_or(&ElementProperty::Null)))
    }
    .unwrap_or_default();
    Display {
        table: table_of(&rows),
        json,
        raw: String::new(),
        notice,
        total_rows: as_rows.then_some(total),
    }
}

/// The entries of a map-like value (a map, or a materialized vertex's or edge's properties).
fn entries_of(value: &ElementProperty) -> Option<&PropertyMap> {
    value.map_entries()
}

fn cut_nested(
    value: &ElementProperty,
    max_items: Option<usize>,
    cut: &mut bool,
) -> ElementProperty {
    let Some(limit) = max_items else {
        return value.clone();
    };
    if let ElementProperty::Array(items) = value {
        let mut out: Vec<ElementProperty> = items
            .iter()
            .take(limit)
            .map(|item| cut_nested(item, max_items, cut))
            .collect();
        if items.len() > limit {
            *cut = true;
            out.push(ElementProperty::from(format!(
                "{CUT} {} more",
                items.len() - limit
            )));
        }
        return ElementProperty::Array(out);
    }
    // A vertex or an edge keeps its kind; its property values are cut, not its property set
    // (a row's columns).
    match value {
        ElementProperty::Vertex(vertex) => {
            let mut vertex = vertex.clone();
            let cut_properties = cut_values(vertex.properties(), max_items, cut);
            *vertex.properties_mut() = cut_properties;
            return ElementProperty::Vertex(vertex);
        }
        ElementProperty::Edge(edge) => {
            let mut edge = edge.clone();
            let cut_properties = cut_values(edge.properties(), max_items, cut);
            *edge.properties_mut() = cut_properties;
            return ElementProperty::Edge(edge);
        }
        _ => {}
    }
    if let Some(map) = entries_of(value) {
        let mut out: PropertyMap = map
            .iter()
            .take(limit)
            .map(|(k, v)| (k.clone(), cut_nested(v, max_items, cut)))
            .collect();
        if map.len() > limit {
            *cut = true;
            out.insert(
                CompactString::new(CUT),
                ElementProperty::from(format!("{} more", map.len() - limit)),
            );
        }
        return ElementProperty::Object(Box::new(out));
    }
    value.clone()
}

/// Every value of `map` cut by [`cut_nested`], all keys kept.
fn cut_values(map: &PropertyMap, max_items: Option<usize>, cut: &mut bool) -> PropertyMap {
    map.iter()
        .map(|(k, v)| (k.clone(), cut_nested(v, max_items, cut)))
        .collect()
}

/// The columns of a vertex row that are the vertex's own, before its properties.
const VERTEX_COLUMNS: [&str; 2] = ["id", "label"];
/// The columns of an edge row that are the edge's own (GraphSON's names), before its properties.
const EDGE_COLUMNS: [&str; 4] = ["id", "label", "outV", "inV"];

/// The cells of one table row by column name: a map's entries; a vertex's id and label, an
/// edge's id, label, `outV` and `inV`, then the element's properties, where a property named
/// like one of those columns is headed `<name> (property)`. `None` for a value that is no row of
/// columns.
fn row_cells(value: &ElementProperty) -> Option<Vec<(String, ElementProperty)>> {
    fn with_properties(
        own: Vec<(&str, ElementProperty)>,
        properties: &PropertyMap,
    ) -> Vec<(String, ElementProperty)> {
        let own_names: Vec<&str> = own.iter().map(|(name, _)| *name).collect();
        let mut cells: Vec<(String, ElementProperty)> = own
            .into_iter()
            .map(|(name, value)| (name.to_string(), value))
            .collect();
        for (key, value) in properties.iter() {
            let column = if own_names.contains(&key.as_str()) {
                format!("{key} (property)")
            } else {
                key.to_string()
            };
            cells.push((column, value.clone()));
        }
        cells
    }
    let text = |text: Option<&str>| text.map_or(ElementProperty::Null, ElementProperty::from);
    match value {
        ElementProperty::Vertex(vertex) => Some(with_properties(
            VERTEX_COLUMNS
                .into_iter()
                .zip([ElementProperty::from(vertex.id()), text(vertex.label())])
                .collect(),
            vertex.properties(),
        )),
        ElementProperty::Edge(edge) => Some(with_properties(
            EDGE_COLUMNS
                .into_iter()
                .zip([
                    ElementProperty::from(edge.id()),
                    text(edge.label()),
                    ElementProperty::from(edge.out_v().id()),
                    ElementProperty::from(edge.in_v().id()),
                ])
                .collect(),
            edge.properties(),
        )),
        ElementProperty::Object(map) => Some(
            map.iter()
                .map(|(key, value)| (key.to_string(), value.clone()))
                .collect(),
        ),
        _ => None,
    }
}

/// Column layout: rows that are all maps, vertices or edges get one column per key, in first-seen
/// order (`id` and `label` first, then an edge's `outV`, `inV`, see `row_cells`); anything else
/// is a single `value` column.
pub fn table_of(rows: &[ElementProperty]) -> Table {
    if rows.is_empty() {
        return Table {
            columns: Vec::new(),
            rows: Vec::new(),
        };
    }
    let cells: Option<Vec<Vec<(String, ElementProperty)>>> = rows.iter().map(row_cells).collect();
    if let Some(cells) = cells {
        let mut keys: Vec<String> = Vec::new();
        // A map naming an element (`element_map()`) shows its `id` and `label` first.
        for first in ["id", "label"] {
            if cells
                .iter()
                .any(|row| row.iter().any(|(key, _)| key == first))
            {
                keys.push(first.to_string());
            }
        }
        for row in &cells {
            for (key, _) in row {
                if !keys.contains(key) {
                    keys.push(key.clone());
                }
            }
        }
        if keys.len() <= MAX_COLUMNS {
            let rows = cells
                .iter()
                .map(|row| {
                    keys.iter()
                        .map(|k| {
                            row.iter()
                                .find(|(key, _)| key == k)
                                .map_or(ElementProperty::Null, |(_, value)| {
                                    table_cell(value.clone())
                                })
                        })
                        .collect()
                })
                .collect();
            return Table {
                columns: keys,
                rows,
            };
        }
    }
    Table {
        columns: vec!["value".to_string()],
        rows: rows.iter().map(|r| vec![table_cell(r.clone())]).collect(),
    }
}

/// A vertex as the playground draws it.
#[derive(Debug, Clone, Serialize)]
pub struct Node {
    /// The vertex id.
    pub id: String,
    /// The primary label (`""` when unlabeled).
    pub label: String,
    /// The full label set.
    pub labels: Vec<String>,
    /// The stored properties, in order.
    pub properties: PropertyMap,
}

/// An edge as the playground draws it.
#[derive(Debug, Clone, Serialize)]
pub struct Edge {
    /// The edge id.
    pub id: String,
    /// The edge label.
    pub label: String,
    /// The out-vertex id.
    pub source: String,
    /// The in-vertex id.
    pub target: String,
    /// The stored properties, in order.
    pub properties: PropertyMap,
}

/// Vertices and edges to draw as columns (`GraphView` in engine.js): what the Graph panel needs to
/// draw, nothing more. Properties are not part of it: the inspector fetches the clicked element's
/// with [`element_of`] (`Session::element`).
///
/// Vertex `i` is `vertexIds[i]` with label `labels[vertexLabel[i]]` (its primary label, `""` when
/// unlabeled) and caption `vertexName[i]` (its `name` property as text, `null` without one); edge
/// `j` is `edgeIds[j]` from vertex `edgeSource[j]` to vertex `edgeTarget[j]` (indices into the
/// vertex columns) with label `labels[edgeLabel[j]]`. Every edge's endpoints are in the vertex
/// columns. `truncated` says that the graph (or the result) has more than is drawn; `totalNodes` /
/// `totalEdges` count what there is.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactGraph {
    /// The vertex ids.
    pub vertex_ids: Vec<String>,
    /// Per vertex: the index of its primary label in `labels`.
    pub vertex_label: Vec<u32>,
    /// Per vertex: its `name` property as text (the caption), or `None`.
    pub vertex_name: Vec<Option<String>>,
    /// The edge ids.
    pub edge_ids: Vec<String>,
    /// Per edge: the index of its out-vertex in the vertex columns.
    pub edge_source: Vec<u32>,
    /// Per edge: the index of its in-vertex in the vertex columns.
    pub edge_target: Vec<u32>,
    /// Per edge: the index of its label in `labels`.
    pub edge_label: Vec<u32>,
    /// The label table shared by vertices and edges, in first-use order.
    pub labels: Vec<String>,
    /// More vertices or edges exist than are drawn.
    pub truncated: bool,
    /// Vertices in the graph (or the result).
    pub total_nodes: usize,
    /// Edges in the graph (or the result).
    pub total_edges: usize,
    /// Why a result's subgraph stopped early: `"elements"` (its element limit) or `"bytes"`
    /// ([`SUBGRAPH_MAX_BYTES`]); absent when nothing stopped it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stopped_by: Option<&'static str>,
    /// With a filter (`crate::filter`): the vertices that pass it, before the drawing limits
    /// (for a neighbourhood: of the neighbourhood). Absent without a filter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_nodes: Option<usize>,
    /// With a filter: the edges that pass it (between visible vertices), before the limits.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_edges: Option<usize>,
    /// Asked for (`degrees: true`): per vertex, its incoming edges in the WHOLE graph (not only
    /// the drawn ones; a self-loop counts once in and once out). Absent otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vertex_in_degree: Option<Vec<u32>>,
    /// Asked for: per vertex, its outgoing edges in the whole graph.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vertex_out_degree: Option<Vec<u32>>,
}

impl CompactGraph {
    /// The columns of `vertices` and `edges`; an edge whose endpoint is not among `vertices` is
    /// left out. `truncated` and the totals are the caller's.
    pub fn build<S: GraphStorage>(g: &S, vertices: &[S::VertexId], edges: &[S::EdgeId]) -> Self {
        let mut view = CompactGraph {
            vertex_ids: Vec::with_capacity(vertices.len()),
            vertex_label: Vec::with_capacity(vertices.len()),
            vertex_name: Vec::with_capacity(vertices.len()),
            edge_ids: Vec::with_capacity(edges.len()),
            edge_source: Vec::with_capacity(edges.len()),
            edge_target: Vec::with_capacity(edges.len()),
            edge_label: Vec::with_capacity(edges.len()),
            ..CompactGraph::default()
        };
        let mut labels = LabelTable::default();
        let mut position = HashMap::with_capacity(vertices.len());
        for v in vertices {
            let Ok(index) = u32::try_from(view.vertex_ids.len()) else {
                break;
            };
            position.insert(*v, index);
            view.vertex_ids
                .push(g.vertex_id(*v).unwrap_or_default().to_string());
            let label = g.vertex_labels(*v).first().map_or("", |l| l.as_str());
            view.vertex_label.push(labels.index(label));
            view.vertex_name.push(
                g.vertex_properties(*v)
                    .and_then(|p| p.get("name"))
                    .and_then(caption_text),
            );
        }
        for e in edges {
            let Some((s, t)) = g.edge_endpoints(*e) else {
                continue;
            };
            let (Some(s), Some(t)) = (position.get(&s), position.get(&t)) else {
                continue;
            };
            view.edge_ids
                .push(g.edge_id(*e).unwrap_or_default().to_string());
            view.edge_source.push(*s);
            view.edge_target.push(*t);
            view.edge_label
                .push(labels.index(g.edge_label(*e).unwrap_or_default()));
        }
        view.labels = labels.names;
        view
    }
}

impl CompactGraph {
    /// Adds the `vertexInDegree` / `vertexOutDegree` columns: each vertex's edges in the whole
    /// graph, counted from its adjacency like the inspector's `inDegree`/`outDegree` (a vertex
    /// dropped since has 0).
    pub fn add_degrees<S: GraphStorage>(&mut self, g: &S) {
        let count = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
        let (mut ins, mut outs) = (
            Vec::with_capacity(self.vertex_ids.len()),
            Vec::with_capacity(self.vertex_ids.len()),
        );
        for id in &self.vertex_ids {
            let (i, o) = g
                .get_vertex_by_id(id)
                .map_or((0, 0), |v| (g.in_edges(v).count(), g.out_edges(v).count()));
            ins.push(count(i));
            outs.push(count(o));
        }
        self.vertex_in_degree = Some(ins);
        self.vertex_out_degree = Some(outs);
    }
}

/// Interned label names: index of a name in `names`, added on first use.
#[derive(Default)]
struct LabelTable {
    names: Vec<String>,
    index: HashMap<String, u32>,
}

impl LabelTable {
    fn index(&mut self, label: &str) -> u32 {
        if let Some(i) = self.index.get(label) {
            return *i;
        }
        let i = u32::try_from(self.names.len()).unwrap_or(u32::MAX);
        self.names.push(label.to_string());
        self.index.insert(label.to_string(), i);
        i
    }
}

/// A caption from a scalar property value: a string as it is, a number or boolean as text;
/// `None` for anything else (null, lists, maps, UUIDs).
pub(crate) fn caption_text(value: &ElementProperty) -> Option<String> {
    match value {
        ElementProperty::String(s) => Some(s.to_string()),
        ElementProperty::Number(n) => Some(n.to_string()),
        ElementProperty::Boolean(b) => Some(b.to_string()),
        _ => None,
    }
}

/// One element with everything the inspector shows, tagged with its `kind`: a vertex
/// (`{kind: "vertex", id, label, labels, properties, inDegree, outDegree}`) or an edge (`{kind: "edge", id, label,
/// source, target, properties}`). Serialized directly (never through a `serde_json::Value`), so
/// the properties keep the engine's order.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum InspectedElement {
    /// A vertex, with its degrees.
    Vertex(InspectedVertex),
    /// An edge.
    Edge(Edge),
}

/// A [`Node`] with the number of edges it has in the graph, for the inspector: counted from the
/// graph's adjacency (no edge is loaded), so the figures are right for a vertex of any drawing
/// tier, also one whose neighbours are not drawn.
#[derive(Debug, Clone, Serialize)]
pub struct InspectedVertex {
    /// The vertex as drawn.
    #[serde(flatten)]
    pub node: Node,
    /// The number of incoming edges.
    #[serde(rename = "inDegree")]
    pub in_degree: usize,
    /// The number of outgoing edges.
    #[serde(rename = "outDegree")]
    pub out_degree: usize,
}

/// The element of `kind` with `id`, or `None` when there is none (dropped since it was drawn).
pub fn element_of<S: GraphStorage>(g: &S, kind: ElementKind, id: &str) -> Option<InspectedElement> {
    Some(match kind {
        ElementKind::Vertex => {
            let v = g.get_vertex_by_id(id)?;
            InspectedElement::Vertex(InspectedVertex {
                node: node_of(g, v),
                in_degree: g.in_edges(v).count(),
                out_degree: g.out_edges(v).count(),
            })
        }
        ElementKind::Edge => InspectedElement::Edge(edge_of(g, g.get_edge_by_id(id)?)),
    })
}

/// A vertex as the playground draws it.
pub fn node_of<S: GraphStorage>(g: &S, v: S::VertexId) -> Node {
    let labels: Vec<String> = g.vertex_labels(v).iter().map(|l| l.to_string()).collect();
    Node {
        id: g.vertex_id(v).unwrap_or_default().to_string(),
        label: labels.first().cloned().unwrap_or_default(),
        labels,
        properties: inspected_properties(g.vertex_properties(v)),
    }
}

/// A copy of an element's properties for the inspector, non-finite floats typed ([`json_safe`]).
fn inspected_properties(properties: Option<&PropertyMap>) -> PropertyMap {
    let mut properties = properties.cloned().unwrap_or_default();
    json_safe_map(&mut properties);
    properties
}

/// An edge as the playground draws it.
pub fn edge_of<S: GraphStorage>(g: &S, e: S::EdgeId) -> Edge {
    let (source, target) = g
        .edge_endpoints(e)
        .map(|(s, t)| {
            (
                g.vertex_id(s).unwrap_or_default().to_string(),
                g.vertex_id(t).unwrap_or_default().to_string(),
            )
        })
        .unwrap_or_default();
    Edge {
        id: g.edge_id(e).unwrap_or_default().to_string(),
        label: g.edge_label(e).unwrap_or_default().to_string(),
        source,
        target,
        properties: inspected_properties(g.edge_properties(e)),
    }
}

/// Estimated JSON bytes of a vertex in the columns: its id, its caption and the separators and
/// index digits around them.
fn vertex_bytes<S: GraphStorage>(g: &S, v: S::VertexId) -> usize {
    let name = g
        .vertex_properties(v)
        .and_then(|p| p.get("name"))
        .and_then(ElementProperty::as_str)
        .map_or(4, str::len);
    g.vertex_id(v).map_or(0, str::len) + name + 12
}

/// The vertices and edges `results` refer to, as a [`CompactGraph`]: every vertex or edge value,
/// and every map with a string `id` and `label` that names a stored element (an edge's
/// `element_map()` has `OUT`/`IN`), searched through
/// nested lists and maps (paths, projections). Edge endpoints are included so the drawing is
/// connected. At most `max_elements` vertices + edges (clamped to [`SUBGRAPH_MAX_ELEMENTS`])
/// and about [`SUBGRAPH_MAX_BYTES`] of columns; `truncated` and `stopped_by` say when the
/// search stopped there. `None` when the results refer to no element.
pub fn collect_subgraph<S: GraphStorage>(
    g: &S,
    results: &[ElementProperty],
    max_elements: usize,
) -> Option<CompactGraph> {
    let max_elements = max_elements.min(SUBGRAPH_MAX_ELEMENTS);
    let mut vertices = Vec::new();
    let mut edges = Vec::new();
    let mut seen_v = HashSet::new();
    let mut seen_e = HashSet::new();
    let mut stack: Vec<&ElementProperty> = results.iter().rev().collect();
    let mut stopped_by = None;
    let mut bytes = 0usize;
    while let Some(value) = stack.pop() {
        if vertices.len() + edges.len() >= max_elements {
            stopped_by = Some("elements");
            break;
        }
        if bytes >= SUBGRAPH_MAX_BYTES {
            stopped_by = Some("bytes");
            break;
        }
        if let ElementProperty::Array(items) = value {
            stack.extend(items.iter().rev());
            continue;
        }
        // A vertex or an edge value names its element by its own id (and label), which a
        // property can never shadow; its properties may hold further elements.
        let (id, label, is_edge) = match value {
            ElementProperty::Vertex(vertex) => (Some(vertex.id()), Some(vertex.label()), false),
            ElementProperty::Edge(edge) => (Some(edge.id()), Some(edge.label()), true),
            // A map naming an element (`element_map()`): a string `id` and a `label` (null for
            // an element without one); an edge's map has its `OUT`/`IN` endpoints.
            _ => {
                let Some(map) = entries_of(value) else {
                    continue;
                };
                let label = match map.get("label") {
                    Some(ElementProperty::Null) => Some(None),
                    Some(other) => other.as_str().map(Some),
                    None => None,
                };
                (
                    map.get("id").and_then(ElementProperty::as_str),
                    label,
                    map.contains_key("OUT") && map.contains_key("IN"),
                )
            }
        };
        if let (Some(id), Some(label)) = (id, label) {
            if is_edge {
                if let Some(e) = g.get_edge_by_id(id)
                    && g.edge_label(e).filter(|l| !l.is_empty()) == label.filter(|l| !l.is_empty())
                    && seen_e.insert(e)
                {
                    edges.push(e);
                    bytes += id.len() + 24;
                    if let Some((s, t)) = g.edge_endpoints(e) {
                        for v in [s, t] {
                            if seen_v.insert(v) {
                                bytes += vertex_bytes(g, v);
                                vertices.push(v);
                            }
                        }
                    }
                }
            } else if let Some(v) = g.get_vertex_by_id(id)
                && label.map_or(g.vertex_labels(v).is_empty(), |l| g.vertex_has_label(v, l))
                && seen_v.insert(v)
            {
                bytes += vertex_bytes(g, v);
                vertices.push(v);
            }
        }
        if let Some(map) = entries_of(value) {
            stack.extend(map.values());
        }
    }
    if vertices.is_empty() && edges.is_empty() {
        return None;
    }
    let (total_nodes, total_edges) = (vertices.len(), edges.len());
    let mut view = CompactGraph::build(g, &vertices, &edges);
    view.truncated = stopped_by.is_some();
    view.stopped_by = stopped_by;
    view.total_nodes = total_nodes;
    view.total_edges = total_edges;
    Some(view)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(items: Vec<i64>) -> ElementProperty {
        ElementProperty::Array(items.into_iter().map(ElementProperty::from).collect())
    }

    #[test]
    fn rows_and_nested_items_are_cut_with_a_notice() {
        let rows = vec![list(vec![1, 2, 3]), list(vec![4]), list(vec![5])];
        let d = bounded_display(
            &rows,
            true,
            &Bounds {
                max_rows: Some(2),
                max_items: Some(2),
            },
        );
        assert_eq!(d.total_rows, Some(3));
        assert_eq!(d.table.rows.len(), 2);
        let text = serde_json::to_string(&d.table.rows[0][0]).unwrap();
        assert_eq!(text, r#"[1,2,"… 1 more"]"#);
        assert!(d.notice.unwrap().contains("first 2 rows"));
    }

    #[test]
    fn maps_become_columns_in_engine_order_with_element_keys_first() {
        let mut map = PropertyMap::default();
        map.insert("name", "a".into());
        map.insert("age", 1i64.into());
        map.insert("id", "1".into());
        let t = table_of(&[ElementProperty::Object(Box::new(map))]);
        assert_eq!(t.columns, vec!["id", "name", "age"]);
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
    fn elements_and_subgraphs_on_a_foreign_storage_equal_those_of_a_traversal_graph() {
        let (tg, fw) = modern_pair();
        for (kind, id) in [
            (ElementKind::Vertex, "1"),
            (ElementKind::Edge, "0"),
            (ElementKind::Vertex, "none"),
        ] {
            assert_eq!(
                serde_json::to_value(element_of(&tg, kind, id)).unwrap(),
                serde_json::to_value(element_of(&fw, kind, id)).unwrap()
            );
        }
        let element = |pairs: &[(&str, &str)]| {
            let mut map = PropertyMap::default();
            for (k, v) in pairs {
                map.insert(*k, (*v).into());
            }
            ElementProperty::Object(Box::new(map))
        };
        let results = vec![
            element(&[("id", "4"), ("label", "person")]),
            ElementProperty::Array(vec![ElementProperty::Edge(Box::new(
                graphersal::prelude::EdgeValue::new(
                    "0",
                    Some("knows".into()),
                    graphersal::prelude::VertexRef::new("1", Some("person".into())),
                    graphersal::prelude::VertexRef::new("2", Some("person".into())),
                    PropertyMap::default(),
                ),
            ))]),
        ];
        let tg_view = collect_subgraph(&tg, &results, 100).unwrap();
        let fw_view = collect_subgraph(&fw, &results, 100).unwrap();
        assert_eq!(tg_view.vertex_ids, ["4", "1", "2"]);
        assert_eq!(
            serde_json::to_value(tg_view).unwrap(),
            serde_json::to_value(fw_view).unwrap()
        );
    }

    #[test]
    fn a_vector_cell_is_a_marker_with_a_preview_and_the_json_keeps_the_values() {
        let vector = ElementProperty::Vector(Vector::from_f32(vec![0.5; 40]).unwrap());
        let d = bounded_display(
            std::slice::from_ref(&vector),
            true,
            &Bounds {
                max_rows: None,
                max_items: None,
            },
        );
        let cell = serde_json::to_value(&d.table.rows[0][0]).unwrap();
        assert_eq!(cell["@type"], "graphersal:Vector");
        assert_eq!(cell["element"], "f32");
        assert_eq!(cell["dimension"], 40);
        assert_eq!(
            cell["preview"].as_array().map(Vec::len),
            Some(VECTOR_PREVIEW)
        );
        let json: serde_json::Value = serde_json::from_str(&d.json).unwrap();
        assert_eq!(json[0].as_array().map(Vec::len), Some(40));
        // In a map's column too.
        let mut map = PropertyMap::default();
        map.insert("emb", vector);
        let table = table_of(&[ElementProperty::Object(Box::new(map))]);
        let cell = serde_json::to_value(&table.rows[0][0]).unwrap();
        assert_eq!(cell["@type"], "graphersal:Vector");
    }
}
