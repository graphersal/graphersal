//! The delta of a [`LayeredGraph`](super::LayeredGraph): what the layer stores beside the base.
//!
//! Later parts of the layer fill it (design `ai/roadmap/layered-graph.md` §3): the elements the
//! layer adds in their own slot space (handles with bit 31 set), the tombstones of removed base
//! elements, the copied-up images of modified base elements, the extra adjacency of base vertices
//! that gained edges, and the id and label indexes of all of these. Today it holds nothing, and
//! every method answers "nothing here".

use compact_str::CompactString;

use super::handle::{LayeredEdge, LayeredVertex};
use crate::graph::{EdgeHandle, VertexHandle};
use crate::value::PropertyMap;

/// The state of one vertex as a read sees it: what a delta vertex or the image of a modified
/// base vertex supplies (an untouched base vertex is read from the base directly).
#[derive(Clone, Copy)]
pub(crate) struct VertexView<'a> {
    pub(crate) id: &'a str,
    pub(crate) labels: &'a [CompactString],
    pub(crate) properties: &'a PropertyMap,
}

/// The state of one edge as a read sees it; see [`VertexView`].
#[derive(Clone, Copy)]
pub(crate) struct EdgeView<'a> {
    pub(crate) id: &'a str,
    pub(crate) label: Option<&'a str>,
    pub(crate) properties: Option<&'a PropertyMap>,
    pub(crate) endpoints: (LayeredVertex, LayeredVertex),
}

/// The layer's own data. Empty until the write tasks add to it.
#[derive(Debug, Clone, Default)]
pub(crate) struct Delta {}

impl Delta {
    /// Vertices the delta adds to the base count (added minus removed base vertices).
    #[inline]
    pub(crate) fn vertex_count_change(&self) -> isize {
        0
    }

    /// Edges the delta adds to the base count (added minus removed base edges).
    #[inline]
    pub(crate) fn edge_count_change(&self) -> isize {
        0
    }

    /// The vertices the layer added, in slot order.
    #[inline]
    pub(crate) fn vertices(&self) -> std::iter::Empty<LayeredVertex> {
        std::iter::empty()
    }

    /// The edges the layer added, in slot order.
    #[inline]
    pub(crate) fn edges(&self) -> std::iter::Empty<LayeredEdge> {
        std::iter::empty()
    }

    /// The added vertices carrying `label`.
    #[inline]
    pub(crate) fn vertices_by_label(&self, label: &str) -> std::iter::Empty<LayeredVertex> {
        let _ = label;
        std::iter::empty()
    }

    /// The added edges labelled `label`.
    #[inline]
    pub(crate) fn edges_by_label(&self, label: &str) -> std::iter::Empty<LayeredEdge> {
        let _ = label;
        std::iter::empty()
    }

    /// An added vertex by id.
    #[inline]
    pub(crate) fn vertex_by_id(&self, id: &str) -> Option<LayeredVertex> {
        let _ = id;
        None
    }

    /// An added edge by id.
    #[inline]
    pub(crate) fn edge_by_id(&self, id: &str) -> Option<LayeredEdge> {
        let _ = id;
        None
    }

    /// The added edges leaving `node` (a delta vertex, or a base vertex that gained edges).
    #[inline]
    pub(crate) fn out_edges(&self, node: LayeredVertex) -> std::iter::Empty<LayeredEdge> {
        let _ = node;
        std::iter::empty()
    }

    /// The added edges arriving at `node`; see [`Self::out_edges`].
    #[inline]
    pub(crate) fn in_edges(&self, node: LayeredVertex) -> std::iter::Empty<LayeredEdge> {
        let _ = node;
        std::iter::empty()
    }

    /// A delta vertex (`node.is_delta()`), `None` when stale.
    #[inline]
    pub(crate) fn vertex(&self, node: LayeredVertex) -> Option<VertexView<'_>> {
        let _ = node;
        None
    }

    /// A delta edge (`edge.is_delta()`), `None` when stale.
    #[inline]
    pub(crate) fn edge(&self, edge: LayeredEdge) -> Option<EdgeView<'_>> {
        let _ = edge;
        None
    }

    /// A dirty base vertex as the layer sees it: `None` when the layer removed it, else its
    /// image. No write sets a dirty bit yet, so nothing reaches this.
    #[inline]
    pub(crate) fn base_vertex(&self, node: VertexHandle) -> Option<VertexView<'_>> {
        let _ = node;
        None
    }

    /// A dirty base edge as the layer sees it; see [`Self::base_vertex`].
    #[inline]
    pub(crate) fn base_edge(&self, edge: EdgeHandle) -> Option<EdgeView<'_>> {
        let _ = edge;
        None
    }
}
