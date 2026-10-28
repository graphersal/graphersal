//! Snapshot copy of a set of edges (and their endpoint vertices) into another graph.
//!
//! This is the engine of `subgraph("sg")` + `cap("sg")` and of the `to_graph()` terminal. It reads
//! the source through [`GraphStorage`] and writes the target only through the [`GraphStorage`]
//! mutation API (`insert_vertex`/`insert_edge`), so any storage can be a target, and every property
//! travels at full depth (nested objects and arrays are cloned, never flattened).

use std::collections::HashMap;

use compact_str::CompactString;

use crate::error::{TraverserError, TraverserResult};
use crate::graph::storage::{stored_edge_id, stored_vertex_id};
use crate::graph::{GraphStorage, TraversalGraph};
use crate::value::{ElementProperty, PropertyMap};

/// Copies `edges` and both endpoint vertices of every edge from `source` into `target`.
///
/// - Ids are preserved (ids are strings, `insert_vertex`/`insert_edge` take them as `Some(id)`).
/// - A vertex keeps its whole label set, in order, and all of its properties; an edge keeps its
///   label and properties.
/// - A vertex that is an endpoint of several edges is copied once. A vertex whose id the target
///   already holds is reused, and an edge whose id the target already holds is skipped, so copying
///   the same edge twice (or into a partly filled target) is idempotent.
/// - The schema is applied last, only when `target` stores none yet (`copy_schema`), so existing
///   data is never re-validated or re-coerced.
///
/// The cost is what a copy needs: one handle map for the vertices plus the cloned properties.
pub(crate) fn copy_edges_into<S, T>(
    source: &S,
    edges: impl IntoIterator<Item = S::EdgeId>,
    target: &mut T,
    copy_schema: bool,
) -> TraverserResult<()>
where
    S: GraphStorage,
    T: GraphStorage,
{
    let mut vertices: HashMap<S::VertexId, T::VertexId> = HashMap::new();
    for edge in edges {
        let edge_id = stored_edge_id(source, edge)?;
        if target.get_edge_by_id(edge_id)?.is_some() {
            continue;
        }
        let (from, to) = source.edge_endpoints(edge)?.ok_or_else(|| {
            Box::new(TraverserError::QueryExecution {
                query: "subgraph".to_string(),
                reason: format!("edge '{edge_id}' has no endpoints"),
            })
        })?;
        let from = ensure_vertex(source, target, &mut vertices, from)?;
        let to = ensure_vertex(source, target, &mut vertices, to)?;
        target.insert_edge(
            Some(CompactString::new(edge_id)),
            source.edge_label(edge)?.map(CompactString::new),
            from,
            to,
            source
                .edge_properties(edge)?
                .map(crate::value::logical_element)
                .unwrap_or_default(),
        )?;
    }
    if copy_schema
        && target.get_schema()?.is_none()
        && let Some(schema) = source.get_schema()?
    {
        target.set_schema(schema)?;
    }
    Ok(())
}

/// The target handle of source vertex `vertex`: reused when already copied or already present in
/// the target, otherwise added with its id, label set and properties.
fn ensure_vertex<S, T>(
    source: &S,
    target: &mut T,
    copied: &mut HashMap<S::VertexId, T::VertexId>,
    vertex: S::VertexId,
) -> TraverserResult<T::VertexId>
where
    S: GraphStorage,
    T: GraphStorage,
{
    if let Some(existing) = copied.get(&vertex) {
        return Ok(*existing);
    }
    let id = stored_vertex_id(source, vertex)?;
    let handle = match target.get_vertex_by_id(id)? {
        Some(existing) => existing,
        None => target.insert_vertex(
            Some(CompactString::new(id)),
            source.vertex_labels(vertex)?.to_vec(),
            source
                .vertex_properties(vertex)?
                .map(crate::value::logical_element)
                .unwrap_or_default(),
        )?,
    };
    copied.insert(vertex, handle);
    Ok(handle)
}

/// The terminal form of a graph value: `{"vertices": [..], "edges": [..]}`, each element
/// materialized like a `g.V()`/`g.E()` result. Computed once at a terminal, never in the stream.
pub(crate) fn graph_to_property(graph: &crate::graph::Graph) -> TraverserResult<ElementProperty> {
    let graph = graph.read();
    let graph: &TraversalGraph = &graph;
    graph_listing(graph, None)
}

/// A graph as one value, `{"vertices": [..], "edges": [..]}` with each element materialized like a
/// `g.V()`/`g.E()` result, holding at most `keep` vertices and `keep` edges (all with `None`).
///
/// The form [`graph_to_property`] gives a graph value at a terminal, and what a script host shows
/// for a graph as a script's final value (`g.e(0).to_graph()`, `cap("sg").next()`, `g`): a
/// display passes its item limit plus one, so the visualizer's own cut marks a longer list and
/// a huge graph is never materialized whole.
pub(crate) fn graph_listing<S: GraphStorage>(
    graph: &S,
    keep: Option<usize>,
) -> TraverserResult<ElementProperty> {
    use crate::value::TraverserValue;
    let keep = keep.unwrap_or(usize::MAX);
    let mut vertices = Vec::with_capacity(graph.vertex_count()?.min(keep));
    for vertex in graph.vertices()?.take(keep) {
        vertices.push(TraverserValue::<S>::Vertex(vertex).into_node_property(graph)?);
    }
    let mut edges = Vec::with_capacity(graph.edge_count()?.min(keep));
    for edge in graph.edges()?.take(keep) {
        edges.push(TraverserValue::<S>::Edge(edge).into_node_property(graph)?);
    }
    let mut map = PropertyMap::default();
    map.insert(
        CompactString::new("vertices"),
        ElementProperty::Array(vertices),
    );
    map.insert(CompactString::new("edges"), ElementProperty::Array(edges));
    Ok(ElementProperty::Object(Box::new(map)))
}
