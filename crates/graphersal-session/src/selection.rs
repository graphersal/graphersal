//! What the Graph panel draws when the whole graph does not fit: the neighbourhood of vertices
//! (expanded on demand), a sample of one vertex label or one label pair, and the label
//! meta-graph (vertex labels with their counts, edges between them counted per label pair).
//!
//! Every function here is bounded: at most `maxNodes` vertices and `maxEdges` edges (clamped to
//! [`MAX_SELECTION_NODES`] / [`MAX_SELECTION_EDGES`]), at most [`META_SCAN_MAX_EDGES`] edges
//! scanned for the label-pair counts, and each loop checks the session's cancellation token
//! (the dev server's Stop) every [`CANCEL_CHECK_EVERY`] steps. Results are
//! [`CompactGraph`] columns like every drawing, so the page draws them with the same code.

use crate::error::SessionError;
use std::collections::{HashMap, HashSet};

use graphersal::exec::CancelToken;
use graphersal::prelude::*;
use serde::{Deserialize, Serialize};

use crate::display::CompactGraph;

/// Most vertices a selection may ask for (the Fast tier's range, like the engine layout).
pub const MAX_SELECTION_NODES: usize = crate::layout::MAX_LAYOUT_VERTICES;

/// Most edges a selection may ask for.
pub const MAX_SELECTION_EDGES: usize = crate::layout::MAX_LAYOUT_EDGES;

/// Vertices of a selection when the request names no limit.
pub const DEFAULT_SELECTION_NODES: usize = 2_000;

/// Edges of a selection when the request names no limit.
pub const DEFAULT_SELECTION_EDGES: usize = 5_000;

/// Most hops of one neighbourhood request (each further hop is a double click away).
pub const MAX_HOPS: u32 = 5;

/// Most left-out vertices a neighbourhood counts after its vertex limit (the search goes on only
/// to count them; above, `leftOut.complete` is false).
pub const LEFT_OUT_COUNT_MAX: usize = 1_000_000;

/// Most vertex labels the meta-graph shows as their own node (the largest by vertex count); the
/// rest are one "other labels" node.
pub const META_MAX_LABELS: usize = 40;

/// Most label-pair edges the meta-graph shows (the largest by edge count); the rest are counted
/// in `otherEdges`.
pub const META_MAX_EDGES: usize = 200;

/// Most edges scanned for the label-pair counts; above, the counts are those of the first ones
/// (`complete: false`).
pub const META_SCAN_MAX_EDGES: usize = 5_000_000;

/// How often (in loop steps) a selection checks the cancellation token.
pub const CANCEL_CHECK_EVERY: usize = 4096;

/// The error of a cancelled selection.
const CANCELLED: &str = "cancelled (Stop)";

/// A drawing selected from the graph: the columns plus what the panel's notices need.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionView {
    /// The vertices and edges to draw (`totalNodes`/`totalEdges` count the whole graph;
    /// `truncated`: a limit stopped the selection).
    #[serde(flatten)]
    pub graph: CompactGraph,
    /// Indices (into the vertex columns) of the vertices the request started from.
    pub seeds: Vec<u32>,
    /// Requested ids that name no vertex (dropped since they were drawn, or mistyped).
    pub missing: Vec<String>,
    /// Edges of the graph that touch a drawn vertex but lead to one that is not drawn (how much
    /// expanding could still add). With a filter, only the edges the filter shows that lead
    /// from a drawn vertex to one the filter shows.
    pub hidden_edges: usize,
    /// A filtered neighbourhood: what it held before the filter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unfiltered: Option<Unfiltered>,
    /// A neighbourhood the limits cut: how much is not drawn.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub left_out: Option<LeftOut>,
    /// Set for a sample of a label or a label pair: what was sampled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample: Option<SampleInfo>,
}

/// A filtered neighbourhood before its filter (after the limits): the strip's "X of Y" and the
/// vertex set a later request keeps (`keep` with `hops: 0`, so a filter change or an expansion
/// redraws every vertex the user drilled into, hidden ones included).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Unfiltered {
    /// Vertices of the neighbourhood.
    pub nodes: usize,
    /// Edges among them.
    pub edges: usize,
    /// The ids of its vertices, in the drawing's (breadth-first) order.
    pub vertex_ids: Vec<String>,
}

/// What a sample shows: `kind` `"vertices"` (of a label filter) or `"edges"` (of a label pair),
/// `shown` of `total` (`total` counts every match; `complete: false` when the scan stopped at a
/// limit before counting them all).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SampleInfo {
    /// `"vertices"` or `"edges"`.
    pub kind: &'static str,
    /// Matching elements drawn.
    pub shown: usize,
    /// Matching elements counted.
    pub total: usize,
    /// Every matching element was counted.
    pub complete: bool,
}

/// Limits of a selection request (`maxNodes`, `maxEdges`).
fn limits(max_nodes: Option<usize>, max_edges: Option<usize>) -> (usize, usize) {
    (
        max_nodes
            .unwrap_or(DEFAULT_SELECTION_NODES)
            .clamp(1, MAX_SELECTION_NODES),
        max_edges
            .unwrap_or(DEFAULT_SELECTION_EDGES)
            .min(MAX_SELECTION_EDGES),
    )
}

/// A counter that checks the cancellation token every [`CANCEL_CHECK_EVERY`] ticks.
struct Ticker<'a> {
    cancel: &'a CancelToken,
    n: usize,
}

impl Ticker<'_> {
    fn tick(&mut self) -> Result<(), SessionError> {
        self.n += 1;
        if self.n.is_multiple_of(CANCEL_CHECK_EVERY) && self.cancel.is_cancelled() {
            return Err(CANCELLED.to_string().into());
        }
        Ok(())
    }
}

/// The edges between `vertices` (at most `max_edges`; each edge once, found from its
/// out-vertex), the number of edges leading outside, and the number of edges between them left
/// out by `max_edges`.
fn edges_among<S: GraphStorage>(
    g: &S,
    vertices: &[S::VertexId],
    max_edges: usize,
    ticker: &mut Ticker<'_>,
) -> Result<(Vec<S::EdgeId>, usize, usize), SessionError> {
    let inside: HashSet<S::VertexId> = vertices.iter().copied().collect();
    let mut edges = Vec::new();
    let mut hidden = 0usize;
    let mut cut = 0usize;
    for v in vertices {
        for e in g.out_edges(*v)? {
            ticker.tick()?;
            match g.edge_endpoints(e)? {
                Some((_, t)) if inside.contains(&t) => {
                    if edges.len() < max_edges {
                        edges.push(e);
                    } else {
                        cut += 1;
                    }
                }
                Some(_) => hidden += 1,
                None => {}
            }
        }
        for e in g.in_edges(*v)? {
            ticker.tick()?;
            if let Some((s, _)) = g.edge_endpoints(e)?
                && !inside.contains(&s)
            {
                hidden += 1;
            }
        }
    }
    Ok((edges, hidden, cut))
}

/// The columns of `vertices` and `edges` as a [`SelectionView`]; `seeds` are vertex handles.
fn view_of<S: GraphStorage>(
    g: &S,
    vertices: &[S::VertexId],
    edges: &[S::EdgeId],
    seeds: &[S::VertexId],
    truncated: bool,
) -> Result<(CompactGraph, Vec<u32>), GraphError> {
    let mut graph = CompactGraph::build(g, vertices, edges)?;
    graph.truncated = truncated;
    graph.total_nodes = g.vertex_count()?;
    graph.total_edges = g.edge_count()?;
    let position: HashMap<S::VertexId, u32> = vertices
        .iter()
        .enumerate()
        .filter_map(|(i, v)| u32::try_from(i).ok().map(|i| (*v, i)))
        .collect();
    let seeds = seeds
        .iter()
        .filter_map(|s| position.get(s).copied())
        .collect();
    Ok((graph, seeds))
}

/// Which edges lead to a vertex's neighbours: `both` (default), `out` (the vertex is the edge's
/// out-vertex: the vertices it points to) or `in` (the vertices pointing to it).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    /// Edges in either direction.
    #[default]
    Both,
    /// Outgoing edges.
    Out,
    /// Incoming edges.
    In,
}

/// A neighbourhood request (`{ids, hops?, direction?, keep?, maxNodes?, maxEdges?}`): the
/// vertices within `hops` steps along `direction` of the vertices `ids` (the centre), plus the
/// vertices `keep` (the drawing so far, when a double click expands one of its vertices), and the
/// edges between all of them.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct NeighbourhoodRequest {
    /// The vertices to start from (expanded by `hops`).
    pub ids: Vec<String>,
    /// The depth: hops from `ids` (default 1 = the direct neighbours, 2 = their neighbours too,
    /// ...; at most [`MAX_HOPS`]; 0 draws `ids` alone).
    pub hops: Option<u32>,
    /// Which edges lead to neighbours (default `both`).
    pub direction: Direction,
    /// Vertices drawn as they are (not expanded), before the new ones.
    pub keep: Vec<String>,
    /// Most vertices (default [`DEFAULT_SELECTION_NODES`]).
    pub max_nodes: Option<usize>,
    /// Most edges (default [`DEFAULT_SELECTION_EDGES`]).
    pub max_edges: Option<usize>,
    /// A display filter ([`crate::filter`]): the neighbourhood is computed as without it (the
    /// limits included), then hidden vertices and edges are dropped from the drawing.
    pub filter: Option<serde_json::Value>,
    /// Add the `vertexInDegree` / `vertexOutDegree` columns (degrees in the whole graph).
    pub degrees: bool,
}

/// What the limits left out of a neighbourhood: `vertices` within the depth that are not drawn
/// (counted up to [`LEFT_OUT_COUNT_MAX`]; `complete: false` when the count stopped there) and
/// `edges` between drawn vertices that are not drawn.
#[derive(Debug, Clone, Default, Serialize)]
pub struct LeftOut {
    /// Vertices within the depth that are not drawn.
    pub vertices: usize,
    /// Edges between drawn vertices that are not drawn (the edge limit).
    pub edges: usize,
    /// `vertices` counts all of them (the count stops at [`LEFT_OUT_COUNT_MAX`]).
    pub complete: bool,
}

/// The neighbours of `v` along `direction`.
fn neighbours<S: GraphStorage>(
    g: &S,
    v: S::VertexId,
    direction: Direction,
) -> Result<Vec<S::VertexId>, GraphError> {
    let mut found = Vec::new();
    if matches!(direction, Direction::Both | Direction::Out) {
        for e in g.out_edges(v)? {
            if let Some((_, t)) = g.edge_endpoints(e)? {
                found.push(t);
            }
        }
    }
    if matches!(direction, Direction::Both | Direction::In) {
        for e in g.in_edges(v)? {
            if let Some((s, _)) = g.edge_endpoints(e)? {
                found.push(s);
            }
        }
    }
    Ok(found)
}

/// The neighbourhood of a request, breadth first (nearer vertices before farther ones; `keep`
/// first, then `ids`). When `maxNodes` or `maxEdges` stops it, `truncated` is set and `leftOut`
/// says how much is not drawn (the search goes on counting, at most [`LEFT_OUT_COUNT_MAX`]
/// vertices). An error when no id of `ids` and `keep` names a vertex, for a depth above
/// [`MAX_HOPS`], or when cancelled.
pub fn neighbourhood<S: GraphStorage>(
    g: &S,
    request: &NeighbourhoodRequest,
    cancel: &CancelToken,
) -> Result<SelectionView, SessionError> {
    let (max_nodes, max_edges) = limits(request.max_nodes, request.max_edges);
    let hops = request.hops.unwrap_or(1);
    if hops > MAX_HOPS {
        return Err(format!(
            "the depth is at most {MAX_HOPS} hops, got {hops} (expand further by double-clicking a vertex)"
        ).into());
    }
    let mut ticker = Ticker { cancel, n: 0 };
    let mut missing = Vec::new();
    let mut seen: HashSet<S::VertexId> = HashSet::new();
    let mut vertices: Vec<S::VertexId> = Vec::new();
    let mut left_out = 0usize;
    // Adds `v` unless seen: drawn while there is room, else counted as left out. True when new.
    let mut add = |v: S::VertexId, vertices: &mut Vec<S::VertexId>, left_out: &mut usize| -> bool {
        if !seen.insert(v) {
            return false;
        }
        if vertices.len() < max_nodes {
            vertices.push(v);
        } else {
            *left_out += 1;
        }
        true
    };
    for id in &request.keep {
        match g.get_vertex_by_id(id)? {
            Some(v) => {
                add(v, &mut vertices, &mut left_out);
            }
            None => missing.push(id.clone()),
        }
    }
    let mut seeds = Vec::new();
    for id in &request.ids {
        match g.get_vertex_by_id(id)? {
            Some(v) => {
                seeds.push(v);
                add(v, &mut vertices, &mut left_out);
            }
            None => missing.push(id.clone()),
        }
    }
    if vertices.is_empty() {
        return Err(match request.ids.first().or(request.keep.first()) {
            Some(id) => format!("there is no vertex with id \"{id}\"").into(),
            None => "name at least one vertex id".to_string().into(),
        });
    }
    let mut complete = true;
    let mut frontier = seeds.clone();
    'hops: for _ in 0..hops {
        let mut next = Vec::new();
        for v in &frontier {
            for w in neighbours(g, *v, request.direction)? {
                ticker.tick()?;
                if add(w, &mut vertices, &mut left_out) {
                    if left_out >= LEFT_OUT_COUNT_MAX {
                        complete = false;
                        break 'hops;
                    }
                    next.push(w);
                }
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    let (mut edges, hidden_edges, edges_left_out) =
        edges_among(g, &vertices, max_edges, &mut ticker)?;
    let truncated = left_out > 0 || edges_left_out > 0;
    let filter = match request.filter.as_ref().filter(|f| !f.is_null()) {
        Some(spec) => Some(crate::filter::GraphFilter::compile(g, spec)?),
        None => None,
    };
    let mut hidden_edges = hidden_edges;
    let mut unfiltered = None;
    if let Some(filter) = &filter {
        ticker.tick()?;
        unfiltered = Some(Unfiltered {
            nodes: vertices.len(),
            edges: edges.len(),
            vertex_ids: {
                let mut ids = Vec::with_capacity(vertices.len());
                for v in &vertices {
                    if let Some(id) = g.vertex_id(*v)? {
                        ids.push(id.to_string());
                    }
                }
                ids
            },
        });
        (vertices, edges) = filter_drawing(g, filter, vertices, edges)?;
        hidden_edges = edges_leading_out(g, filter, &vertices, &mut ticker)?;
    }
    let (mut graph, seeds) = view_of(g, &vertices, &edges, &seeds, truncated)?;
    if filter.is_some() {
        graph.matched_nodes = Some(vertices.len());
        graph.matched_edges = Some(edges.len());
    }
    if request.degrees {
        graph.add_degrees(g)?;
    }
    Ok(SelectionView {
        graph,
        seeds,
        missing,
        hidden_edges,
        unfiltered,
        left_out: truncated.then_some(LeftOut {
            vertices: left_out,
            edges: edges_left_out,
            complete,
        }),
        sample: None,
    })
}

/// The edges a filtered drawing could still show by expanding: edges the filter shows that lead
/// from a drawn vertex to a vertex not drawn that the filter shows (each counted once; the
/// `hideIsolated` rule is not applied to the far vertex, the edge itself would keep it).
fn edges_leading_out<S: GraphStorage>(
    g: &S,
    filter: &crate::filter::GraphFilter,
    vertices: &[S::VertexId],
    ticker: &mut Ticker<'_>,
) -> Result<usize, SessionError> {
    let inside: HashSet<S::VertexId> = vertices.iter().copied().collect();
    let mut leading: Vec<(S::EdgeId, S::VertexId)> = Vec::new();
    for v in vertices {
        for e in g.out_edges(*v)?.chain(g.in_edges(*v)?) {
            ticker.tick()?;
            if let Some((s, t)) = g.edge_endpoints(e)? {
                let far = if s == *v { t } else { s };
                if !inside.contains(&far) {
                    leading.push((e, far));
                }
            }
        }
    }
    if leading.is_empty() {
        return Ok(0);
    }
    let edge_ids: Vec<S::EdgeId> = leading.iter().map(|(e, _)| *e).collect();
    let shown_edges: Option<HashSet<S::EdgeId>> = filter
        .edges(g, Some(&edge_ids))?
        .map(|l| l.into_iter().collect());
    let mut far: Vec<S::VertexId> = Vec::new();
    let mut seen = HashSet::new();
    for (_, v) in &leading {
        if seen.insert(*v) {
            far.push(*v);
        }
    }
    let shown_far: Option<HashSet<S::VertexId>> = filter
        .vertices(g, Some(&far))?
        .map(|l| l.into_iter().collect());
    Ok(leading
        .iter()
        .filter(|(e, v)| {
            shown_edges.as_ref().is_none_or(|s| s.contains(e))
                && shown_far.as_ref().is_none_or(|s| s.contains(v))
        })
        .count())
}

/// The vertices and edges of a drawing.
type Drawing<S> = (
    Vec<<S as GraphStorage>::VertexId>,
    Vec<<S as GraphStorage>::EdgeId>,
);

/// A drawing with what `filter` hides dropped: vertices it hides, edges it hides or whose
/// endpoint is hidden, and with `hideIsolated` the vertices left without a visible edge. Orders
/// are kept.
fn filter_drawing<S: GraphStorage>(
    g: &S,
    filter: &crate::filter::GraphFilter,
    vertices: Vec<S::VertexId>,
    edges: Vec<S::EdgeId>,
) -> Result<Drawing<S>, SessionError> {
    let vertices = filter.vertices(g, Some(&vertices))?.unwrap_or(vertices);
    let visible: HashSet<S::VertexId> = vertices.iter().copied().collect();
    let mut kept: Vec<S::EdgeId> = Vec::new();
    for e in filter.edges(g, Some(&edges))?.unwrap_or(edges) {
        if let Some((s, t)) = g.edge_endpoints(e)?
            && visible.contains(&s)
            && visible.contains(&t)
        {
            kept.push(e);
        }
    }
    let edges = kept;
    if !filter.hide_isolated() {
        return Ok((vertices, edges));
    }
    let mut touched = HashSet::with_capacity(edges.len());
    for e in &edges {
        if let Some((s, t)) = g.edge_endpoints(*e)? {
            touched.insert(s);
            touched.insert(t);
        }
    }
    let vertices = vertices
        .into_iter()
        .filter(|v| touched.contains(v))
        .collect();
    Ok((vertices, edges))
}

/// A set of vertex labels: the labels listed, or with `exclude` every label NOT listed (the
/// meta-graph's "other labels" node). A vertex matches by its primary (first) label; the empty
/// label is the unlabeled vertices.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LabelFilter {
    /// The labels.
    pub labels: Vec<String>,
    /// Match every label not in `labels`.
    pub exclude: bool,
}

impl LabelFilter {
    fn matches(&self, label: &str) -> bool {
        self.labels.iter().any(|l| l == label) != self.exclude
    }
}

/// The edge part of a sample request: edges labelled `label` from a vertex matching `from` to
/// one matching `to` (a missing filter matches every vertex).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EdgeFilter {
    /// The edge label.
    pub label: String,
    /// The out-vertex's primary label.
    pub from: Option<LabelFilter>,
    /// The in-vertex's primary label.
    pub to: Option<LabelFilter>,
}

/// A sample request: `{vertices: LabelFilter}` (vertices of those primary labels and the edges
/// between them) or `{edge: EdgeFilter}` (edges of one label pair with their endpoints), with
/// `maxNodes`/`maxEdges`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct SampleRequest {
    /// Sample the vertices of these labels.
    pub vertices: Option<LabelFilter>,
    /// Sample the edges of this label pair.
    pub edge: Option<EdgeFilter>,
    /// Most vertices (default [`DEFAULT_SELECTION_NODES`]).
    pub max_nodes: Option<usize>,
    /// Most edges (default [`DEFAULT_SELECTION_EDGES`]).
    pub max_edges: Option<usize>,
    /// Add the `vertexInDegree` / `vertexOutDegree` columns (degrees in the whole graph).
    pub degrees: bool,
}

fn primary<S: GraphStorage>(g: &S, v: S::VertexId) -> Result<&str, GraphError> {
    Ok(g.vertex_labels(v)?.first().map_or("", |l| l.as_str()))
}

/// A sample of a label or a label pair: the FIRST matching elements in storage order (not a
/// random choice), at most `maxNodes` vertices and `maxEdges` edges. Every match is counted
/// (`sample.total`), so the panel can say how much is not drawn.
pub fn label_sample<S: GraphStorage>(
    g: &S,
    request: &SampleRequest,
    cancel: &CancelToken,
) -> Result<SelectionView, SessionError> {
    let (max_nodes, max_edges) = limits(request.max_nodes, request.max_edges);
    let mut ticker = Ticker { cancel, n: 0 };
    match (&request.vertices, &request.edge) {
        (Some(filter), None) => {
            let mut vertices = Vec::new();
            let mut total = 0usize;
            for v in g.vertices()? {
                ticker.tick()?;
                if filter.matches(primary(g, v)?) {
                    total += 1;
                    if vertices.len() < max_nodes {
                        vertices.push(v);
                    }
                }
            }
            let (edges, hidden_edges, cut) = edges_among(g, &vertices, max_edges, &mut ticker)?;
            let shown = vertices.len();
            let (mut graph, seeds) = view_of(g, &vertices, &edges, &[], shown < total || cut > 0)?;
            if request.degrees {
                graph.add_degrees(g)?;
            }
            Ok(SelectionView {
                graph,
                seeds,
                missing: Vec::new(),
                hidden_edges,
                unfiltered: None,
                left_out: None,
                sample: Some(SampleInfo {
                    kind: "vertices",
                    shown,
                    total,
                    complete: true,
                }),
            })
        }
        (None, Some(filter)) => {
            let any = LabelFilter {
                labels: Vec::new(),
                exclude: true,
            };
            let from = filter.from.as_ref().unwrap_or(&any);
            let to = filter.to.as_ref().unwrap_or(&any);
            let mut vertices = Vec::new();
            let mut seen = HashSet::new();
            let mut edges = Vec::new();
            let mut total = 0usize;
            let mut full = false;
            for e in g.edges()? {
                ticker.tick()?;
                // An edge without a label has the empty label (as in the meta-graph).
                if g.edge_label(e)?.unwrap_or("") != filter.label {
                    continue;
                }
                let Some((s, t)) = g.edge_endpoints(e)? else {
                    continue;
                };
                if !from.matches(primary(g, s)?) || !to.matches(primary(g, t)?) {
                    continue;
                }
                total += 1;
                if full {
                    continue;
                }
                let new =
                    usize::from(!seen.contains(&s)) + usize::from(s != t && !seen.contains(&t));
                if edges.len() >= max_edges || vertices.len() + new > max_nodes {
                    full = true;
                    continue;
                }
                for v in [s, t] {
                    if seen.insert(v) {
                        vertices.push(v);
                    }
                }
                edges.push(e);
            }
            let shown = edges.len();
            let (mut graph, seeds) = view_of(g, &vertices, &edges, &[], shown < total)?;
            if request.degrees {
                graph.add_degrees(g)?;
            }
            // Edges of the drawn vertices that are not drawn: their degree minus the drawn edges.
            let mut degree = 0usize;
            for v in &vertices {
                degree += g.out_edges(*v)?.count() + g.in_edges(*v)?.count();
            }
            Ok(SelectionView {
                graph,
                seeds,
                missing: Vec::new(),
                hidden_edges: degree.saturating_sub(2 * shown),
                unfiltered: None,
                left_out: None,
                sample: Some(SampleInfo {
                    kind: "edges",
                    shown,
                    total,
                    complete: true,
                }),
            })
        }
        _ => {
            Err("a sample names either `vertices` (a label filter) or `edge` (a label pair)".into())
        }
    }
}

/// One vertex label of the meta-graph.
#[derive(Debug, Clone, Serialize)]
pub struct MetaLabel {
    /// The label (`""`: vertices without a label).
    pub label: String,
    /// Vertices whose primary label it is.
    pub count: usize,
}

/// One label pair of the meta-graph: `count` edges labelled `label` from a vertex of label
/// `from` to one of label `to` (`from`/`to` index `labels`; [`MetaGraph::other`]'s index is
/// `labels.len()`).
#[derive(Debug, Clone, Serialize)]
pub struct MetaEdge {
    /// The edge label.
    pub label: String,
    /// Index of the out-vertices' label.
    pub from: u32,
    /// Index of the in-vertices' label.
    pub to: u32,
    /// Edges counted.
    pub count: usize,
}

/// The labels beyond [`META_MAX_LABELS`], as one node.
#[derive(Debug, Clone, Serialize)]
pub struct MetaOther {
    /// How many labels.
    pub labels: usize,
    /// Their vertices.
    pub count: usize,
}

/// The label meta-graph: vertex labels as nodes with their vertex counts (by primary label, from
/// the graph's statistics), label pairs as edges with their edge counts (one scan of the edges).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetaGraph {
    /// The largest vertex labels (at most [`META_MAX_LABELS`]), largest first.
    pub labels: Vec<MetaLabel>,
    /// The other labels as one node, when there are more.
    pub other: Option<MetaOther>,
    /// The largest label pairs (at most [`META_MAX_EDGES`]), largest first.
    pub edges: Vec<MetaEdge>,
    /// Label pairs not shown.
    pub other_pairs: usize,
    /// Edges of the pairs not shown.
    pub other_edges: usize,
    /// Vertex labels in the graph.
    pub label_total: usize,
    /// Vertices in the graph.
    pub vertex_count: usize,
    /// Edges in the graph.
    pub edge_count: usize,
    /// Edges scanned for the pair counts.
    pub scanned_edges: usize,
    /// Every edge was scanned (at most [`META_SCAN_MAX_EDGES`]).
    pub complete: bool,
}

/// The label meta-graph of `g` (see [`MetaGraph`]). Vertex counts come from the statistics
/// (a scan of the vertices for a storage without them); the pair counts from one scan of at most
/// [`META_SCAN_MAX_EDGES`] edges.
pub fn meta_graph<S: GraphStorage>(g: &S, cancel: &CancelToken) -> Result<MetaGraph, SessionError> {
    let mut ticker = Ticker { cancel, n: 0 };
    let mut counts: Vec<(String, usize)> = match g.statistics()? {
        Some(stats) => stats
            .primary_vertex_label_counts()
            .into_iter()
            .map(|(l, c)| (l.to_string(), c))
            .collect(),
        None => {
            let mut map: HashMap<String, usize> = HashMap::new();
            for v in g.vertices()? {
                ticker.tick()?;
                *map.entry(primary(g, v)?.to_string()).or_default() += 1;
            }
            map.into_iter().collect()
        }
    };
    counts.retain(|(_, c)| *c > 0);
    counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let label_total = counts.len();
    let rest = counts.split_off(counts.len().min(META_MAX_LABELS));
    let other = (!rest.is_empty()).then(|| MetaOther {
        labels: rest.len(),
        count: rest.iter().map(|(_, c)| c).sum(),
    });
    let other_index = u32::try_from(counts.len()).unwrap_or(u32::MAX);
    let index: HashMap<&str, u32> = counts
        .iter()
        .enumerate()
        .map(|(i, (l, _))| (l.as_str(), u32::try_from(i).unwrap_or(u32::MAX)))
        .collect();
    let bucket = |v: S::VertexId| -> Result<u32, GraphError> {
        Ok(index.get(primary(g, v)?).copied().unwrap_or(other_index))
    };
    let mut pairs: HashMap<(u32, &str, u32), usize> = HashMap::new();
    let mut scanned = 0usize;
    for e in g.edges()? {
        if scanned >= META_SCAN_MAX_EDGES {
            break;
        }
        ticker.tick()?;
        scanned += 1;
        let Some((s, t)) = g.edge_endpoints(e)? else {
            continue;
        };
        let label = g.edge_label(e)?.unwrap_or("");
        *pairs.entry((bucket(s)?, label, bucket(t)?)).or_default() += 1;
    }
    let mut pairs: Vec<((u32, &str, u32), usize)> = pairs.into_iter().collect();
    pairs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let hidden = pairs.split_off(pairs.len().min(META_MAX_EDGES));
    Ok(MetaGraph {
        labels: counts
            .into_iter()
            .map(|(label, count)| MetaLabel { label, count })
            .collect(),
        other,
        edges: pairs
            .into_iter()
            .map(|((from, label, to), count)| MetaEdge {
                label: label.to_string(),
                from,
                to,
                count,
            })
            .collect(),
        other_pairs: hidden.len(),
        other_edges: hidden.iter().map(|(_, c)| c).sum(),
        label_total,
        vertex_count: g.vertex_count()?,
        edge_count: g.edge_count()?,
        scanned_edges: scanned,
        complete: scanned >= g.edge_count()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn modern() -> Graph {
        GraphSource::tinkerpop_modern()
    }

    fn ids(view: &SelectionView) -> Vec<&str> {
        let mut out: Vec<&str> = view.graph.vertex_ids.iter().map(String::as_str).collect();
        out.sort_unstable();
        out
    }

    #[test]
    fn neighbourhood_of_one_vertex_follows_both_directions() {
        let graph = modern();
        let g = graph.read();
        let token = CancelToken::new();
        let req = NeighbourhoodRequest {
            ids: vec!["2".into()],
            ..Default::default()
        };
        // vadas (2) is known by marko (1) only.
        let view = neighbourhood(&*g, &req, &token).unwrap();
        assert_eq!(ids(&view), ["1", "2"]);
        assert_eq!(view.graph.edge_ids, ["0"]);
        assert_eq!(view.seeds.len(), 1);
        assert_eq!(view.graph.vertex_ids[view.seeds[0] as usize], "2");
        // marko has two more edges leading out of the drawing.
        assert_eq!(view.hidden_edges, 2);
        assert!(!view.graph.truncated);
        let two = NeighbourhoodRequest {
            ids: vec!["2".into()],
            hops: Some(2),
            ..Default::default()
        };
        assert_eq!(
            ids(&neighbourhood(&*g, &two, &token).unwrap()),
            ["1", "2", "3", "4"]
        );
    }

    #[test]
    fn depth_and_direction_choose_the_neighbours() {
        let graph = modern();
        let g = graph.read();
        let token = CancelToken::new();
        let at = |hops: u32, direction: Direction| {
            let req = NeighbourhoodRequest {
                ids: vec!["4".into()],
                hops: Some(hops),
                direction,
                ..Default::default()
            };
            let view = neighbourhood(&*g, &req, &token).unwrap();
            assert!(view.left_out.is_none());
            ids(&view).into_iter().map(String::from).collect::<Vec<_>>()
        };
        // josh (4): created 3 and 5, known by marko (1).
        assert_eq!(at(1, Direction::Out), ["3", "4", "5"]);
        assert_eq!(at(1, Direction::In), ["1", "4"]);
        assert_eq!(at(1, Direction::Both), ["1", "3", "4", "5"]);
        // Depth 2 along incoming edges: nobody points at marko.
        assert_eq!(at(2, Direction::In), ["1", "4"]);
        // Depth 2 in both directions reaches vadas (2) and peter (6) too.
        assert_eq!(at(2, Direction::Both), ["1", "2", "3", "4", "5", "6"]);
        assert_eq!(at(0, Direction::Both), ["4"]);
        let bad: Result<NeighbourhoodRequest, _> =
            serde_json::from_str(r#"{"ids": ["1"], "direction": "sideways"}"#);
        assert!(bad.is_err());
        let out: NeighbourhoodRequest =
            serde_json::from_str(r#"{"ids": ["1"], "direction": "out"}"#).unwrap();
        assert_eq!(out.direction, Direction::Out);
    }

    #[test]
    fn expanding_keeps_the_drawing_and_adds_the_neighbours() {
        let graph = modern();
        let g = graph.read();
        let req = NeighbourhoodRequest {
            ids: vec!["4".into()],
            keep: vec!["1".into(), "2".into(), "gone".into()],
            ..Default::default()
        };
        let view = neighbourhood(&*g, &req, &CancelToken::new()).unwrap();
        // keep first, in order, then the seed and its neighbours (1 already drawn, 3 and 5 new).
        assert_eq!(&view.graph.vertex_ids[..3], ["1", "2", "4"]);
        assert_eq!(ids(&view), ["1", "2", "3", "4", "5"]);
        assert_eq!(view.missing, ["gone"]);
    }

    #[test]
    fn neighbourhood_is_bounded_and_reports_errors() {
        let graph = modern();
        let g = graph.read();
        let token = CancelToken::new();
        let capped = NeighbourhoodRequest {
            ids: vec!["1".into()],
            max_nodes: Some(2),
            ..Default::default()
        };
        let view = neighbourhood(&*g, &capped, &token).unwrap();
        assert_eq!(view.graph.vertex_ids.len(), 2);
        assert!(view.graph.truncated);
        // marko has three neighbours: one is drawn, two are left out.
        let left = view.left_out.as_ref().unwrap();
        assert_eq!((left.vertices, left.edges, left.complete), (2, 0, true));
        let no_edges = NeighbourhoodRequest {
            ids: vec!["1".into()],
            max_edges: Some(0),
            ..Default::default()
        };
        let view = neighbourhood(&*g, &no_edges, &token).unwrap();
        assert!(view.graph.edge_ids.is_empty() && view.graph.truncated);
        // 1 knows 2 and 4, created 3; 4 created 3.
        assert_eq!(view.left_out.as_ref().unwrap().edges, 4);
        let unknown = NeighbourhoodRequest {
            ids: vec!["nope".into()],
            ..Default::default()
        };
        assert!(
            neighbourhood(&*g, &unknown, &token)
                .unwrap_err()
                .to_string()
                .contains("\"nope\"")
        );
        let far = NeighbourhoodRequest {
            ids: vec!["1".into()],
            hops: Some(MAX_HOPS + 1),
            ..Default::default()
        };
        assert!(neighbourhood(&*g, &far, &token).is_err());
    }

    #[test]
    fn a_cancelled_selection_stops() {
        let graph = GraphSource::large_110k();
        let g = graph.read();
        let token = CancelToken::new();
        token.cancel();
        let err = meta_graph(&*g, &token).unwrap_err();
        assert!(err.to_string().contains("cancelled"), "{err}");
        let req = SampleRequest {
            vertices: Some(LabelFilter {
                labels: vec!["x".into()],
                exclude: true,
            }),
            ..Default::default()
        };
        assert!(label_sample(&*g, &req, &token).is_err());
    }

    #[test]
    fn label_samples_count_every_match() {
        let graph = modern();
        let g = graph.read();
        let token = CancelToken::new();
        let people = SampleRequest {
            vertices: Some(LabelFilter {
                labels: vec!["person".into()],
                exclude: false,
            }),
            max_nodes: Some(2),
            ..Default::default()
        };
        let view = label_sample(&*g, &people, &token).unwrap();
        let sample = view.sample.as_ref().unwrap();
        assert_eq!(
            (sample.kind, sample.shown, sample.total),
            ("vertices", 2, 4)
        );
        assert!(view.graph.truncated);
        let created = SampleRequest {
            edge: Some(EdgeFilter {
                label: "created".into(),
                from: Some(LabelFilter {
                    labels: vec!["person".into()],
                    exclude: false,
                }),
                to: None,
            }),
            ..Default::default()
        };
        let view = label_sample(&*g, &created, &token).unwrap();
        let sample = view.sample.as_ref().unwrap();
        assert_eq!((sample.kind, sample.shown, sample.total), ("edges", 4, 4));
        assert_eq!(ids(&view), ["1", "3", "4", "5", "6"]);
        assert!(label_sample(&*g, &SampleRequest::default(), &token).is_err());
    }

    #[test]
    fn the_meta_graph_counts_labels_and_label_pairs() {
        let graph = modern();
        let g = graph.read();
        let meta = meta_graph(&*g, &CancelToken::new()).unwrap();
        let labels: Vec<(&str, usize)> = meta
            .labels
            .iter()
            .map(|l| (l.label.as_str(), l.count))
            .collect();
        assert_eq!(labels, [("person", 4), ("software", 2)]);
        assert!(meta.other.is_none() && meta.complete);
        let pairs: Vec<(&str, u32, u32, usize)> = meta
            .edges
            .iter()
            .map(|e| (e.label.as_str(), e.from, e.to, e.count))
            .collect();
        assert_eq!(pairs, [("created", 0, 1, 4), ("knows", 0, 0, 2)]);
    }

    #[test]
    fn the_meta_graph_groups_labels_beyond_the_limit() {
        // The large sample graph has one label per vertex.
        let graph = GraphSource::large_110k();
        let g = graph.read();
        let meta = meta_graph(&*g, &CancelToken::new()).unwrap();
        assert_eq!(meta.labels.len(), META_MAX_LABELS);
        let other = meta.other.as_ref().unwrap();
        assert_eq!(other.labels + META_MAX_LABELS, meta.label_total);
        assert_eq!(
            other.count + meta.labels.iter().map(|l| l.count).sum::<usize>(),
            g.vertex_count()
        );
        assert!(meta.edges.len() <= META_MAX_EDGES);
        assert_eq!(
            meta.edges.iter().map(|e| e.count).sum::<usize>() + meta.other_edges,
            g.edge_count()
        );
        assert!(meta.complete);
        // Its edges have no label: a pair sample of the empty label finds them.
        let pair = &meta.edges[0];
        assert_eq!(pair.label, "");
        let req = SampleRequest {
            edge: Some(EdgeFilter {
                label: String::new(),
                from: None,
                to: None,
            }),
            max_nodes: Some(50),
            ..Default::default()
        };
        let view = label_sample(&*g, &req, &CancelToken::new()).unwrap();
        assert!(!view.graph.edge_ids.is_empty());
        assert_eq!(view.sample.unwrap().total, g.edge_count());
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
    fn selections_on_a_foreign_storage_equal_those_of_a_traversal_graph() {
        let (tg, fw) = modern_pair();
        let token = CancelToken::new();
        let json = |view: SelectionView| serde_json::to_value(view).unwrap();
        let near = NeighbourhoodRequest {
            ids: vec!["4".into()],
            hops: Some(2),
            keep: vec!["2".into()],
            max_nodes: Some(4),
            ..Default::default()
        };
        assert_eq!(
            json(neighbourhood(&tg, &near, &token).unwrap()),
            json(neighbourhood(&fw, &near, &token).unwrap())
        );
        let created = SampleRequest {
            edge: Some(EdgeFilter {
                label: "created".into(),
                from: None,
                to: None,
            }),
            ..Default::default()
        };
        assert_eq!(
            json(label_sample(&tg, &created, &token).unwrap()),
            json(label_sample(&fw, &created, &token).unwrap())
        );
        assert_eq!(
            serde_json::to_value(meta_graph(&tg, &token).unwrap()).unwrap(),
            serde_json::to_value(meta_graph(&fw, &token).unwrap()).unwrap()
        );
    }
}
