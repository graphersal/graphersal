//! [`LayeredGraph`]: a frozen, shared base graph plus a layer of its own (the delta).
//!
//! **EXPERIMENTAL**, like [`GraphStorage`]. Many layers can share one base
//! ([`Arc<TraversalGraph>`]): each stores only what it changes, and from outside each is an
//! ordinary [`GraphStorage`] that every front end (the Rhai DSL, GQL, the session) runs on, the
//! way a `Graph<TraversalGraph>` does. Design: `ai/roadmap/layered-graph.md`.
//!
//! [`GraphStorage`]: crate::graph::GraphStorage
//!
//! # Shape
//!
//! - **Base:** an `Arc<TraversalGraph>` nobody can change any more (the `Arc` is shared, so no
//!   `&mut` exists). Reads of an untouched base element go straight to it, through the base's
//!   own inlined accessors.
//! - **Handles:** [`LayeredVertex`] and [`LayeredEdge`] are 8 bytes: a `u32` slot and a
//!   generation. Bit 31 of the slot clear names a base slot (the base's handle, unchanged, no
//!   lookup); set, it names a slot of the layer's own store. So a base must have fewer than 2^31
//!   slots of each kind ([`LayeredGraph::over`] refuses a larger one), and a
//!   `Traverser<LayeredGraph>` stays 80 bytes (rule 20).
//! - **Dirty bits:** one bit per base vertex slot and one per base edge slot, allocated when the
//!   layer is created, all clear. A clear bit means "read the base"; never a hash lookup for an
//!   untouched element. A set bit means the delta holds the element's state (removed, or a
//!   copied-up image). Adjacency is decided per vertex: a removed base edge dirties both of its
//!   endpoints, so the base lists of a clean vertex are read without a per-edge test.
//! - **Delta:** added elements, tombstones, images, extra adjacency of base vertices (`delta`).
//!
//! # State of the implementation
//!
//! Reads are complete: every read method of `GraphStorage` answers from the base, with the
//! dirty-bit check where a changed element will need it (scans, lookups, adjacency, element
//! reads). Writes are not implemented yet: every mutating method returns
//! [`GraphError::Unsupported`], and `GraphStorage::capabilities` claims neither `writable`
//! nor `atomic`. The schema, the catalog, the statistics, the property indexes and the execution
//! policy are the base's.

mod delta;
mod dirty;
mod handle;
mod storage;

use std::sync::Arc;

pub use handle::{LayeredEdge, LayeredVertex};

use crate::error::GraphError;
use crate::graph::{EdgeHandle, TraversalGraph, VertexHandle};
use delta::{Delta, EdgeView, VertexView};
use dirty::SlotBits;
use handle::MAX_BASE_SLOTS;

/// A [`GraphStorage`](crate::graph::GraphStorage) made of a frozen base graph shared by any
/// number of layers and a layer of its own: added elements, removed and modified base
/// elements. Handles ([`LayeredVertex`], [`LayeredEdge`]) are 8 bytes; a base element untouched
/// by the layer is read straight from the base, with one bit test and no lookup.
///
/// **EXPERIMENTAL.** Today it is a read-only view of its base with its own handle types: reads
/// answer exactly what the base answers, writes fail with [`GraphError::Unsupported`].
///
/// ```
/// use std::sync::Arc;
/// use graphersal::prelude::*;
/// use graphersal::storage::LayeredGraph;
///
/// let base = Arc::new(TraversalGraph::tinkerpop_modern());
/// let layer = LayeredGraph::over(Arc::clone(&base)).unwrap();
/// let g = GraphTraversalSource::new(&layer);
/// let names = g.v(None).has_label("person").values("name").to_list().unwrap();
/// assert_eq!(names.len(), 4);
/// ```
pub struct LayeredGraph {
    /// The frozen base, shared with every other layer over it.
    base: Arc<TraversalGraph>,
    /// One bit per base vertex slot: set when the delta holds the vertex's state.
    dirty_vertices: SlotBits,
    /// One bit per base edge slot: set when the delta holds the edge's state.
    dirty_edges: SlotBits,
    /// What the layer stores beside the base.
    delta: Delta,
}

impl LayeredGraph {
    /// A new, empty layer over `base`: until it is written to it reads exactly as `base`.
    ///
    /// # Errors
    ///
    /// [`GraphError::Unsupported`] when `base` cannot carry a layer: it has 2^31 or more vertex
    /// or edge slots (live or vacant), which the layer's handles cannot address.
    pub fn over(base: Arc<TraversalGraph>) -> Result<Self, GraphError> {
        check_base(&base)?;
        Ok(Self::new_unchecked(base))
    }

    /// The shared base.
    pub fn base(&self) -> &Arc<TraversalGraph> {
        &self.base
    }

    /// The layer over a base [`check_base`] accepted.
    fn new_unchecked(base: Arc<TraversalGraph>) -> Self {
        Self {
            dirty_vertices: SlotBits::new(base.graph.node_slots()),
            dirty_edges: SlotBits::new(base.graph.edge_slots()),
            base,
            delta: Delta::default(),
        }
    }

    /// `true` iff the base vertex `node` is visible in the layer: untouched (one bit test), or
    /// dirty and not removed.
    #[inline]
    fn base_vertex_live(&self, node: VertexHandle) -> bool {
        !self.dirty_vertices.contains(node.index().slot()) || self.delta.base_vertex(node).is_some()
    }

    /// The edge counterpart of [`Self::base_vertex_live`].
    #[inline]
    fn base_edge_live(&self, edge: EdgeHandle) -> bool {
        !self.dirty_edges.contains(edge.index().slot()) || self.delta.base_edge(edge).is_some()
    }

    /// The filter of a scan over base vertices: [`Self::base_vertex_live`], with "no bit set at
    /// all" captured once by value, so a clean layer pays one predictable branch per item and
    /// no memory load.
    #[inline]
    fn base_vertex_filter(&self) -> impl Fn(&VertexHandle) -> bool + '_ {
        let clean = !self.dirty_vertices.any();
        move |node| clean || self.base_vertex_live(*node)
    }

    /// The edge counterpart of [`Self::base_vertex_filter`].
    #[inline]
    fn base_edge_filter(&self) -> impl Fn(&EdgeHandle) -> bool + '_ {
        let clean = !self.dirty_edges.any();
        move |edge| clean || self.base_edge_live(*edge)
    }

    /// The filter of the base adjacency lists of `node`, decided once per vertex: a clean base
    /// vertex's base lists are exact, so its edges pass without a test. **Invariant the write
    /// path keeps:** removing (or otherwise dirtying) a base edge sets the dirty bit of both of
    /// its endpoints, so only the lists of a dirty vertex can hold a removed base edge. (A delta
    /// vertex has no base list: its slot is beyond the base's.)
    #[inline]
    fn base_adjacency_filter(&self, node: LayeredVertex) -> impl Fn(&EdgeHandle) -> bool + '_ {
        let exact = !self.dirty_vertices.contains(node.slot());
        move |edge| exact || self.base_edge_live(*edge)
    }

    /// Runs one vertex read: `base` for an untouched base vertex (the hot path: a tag test and
    /// one bit test), `view` with what the delta holds for a delta vertex or a dirty base vertex.
    #[inline]
    fn vertex_read<'a, T>(
        &'a self,
        node: LayeredVertex,
        base: impl FnOnce(&'a TraversalGraph, VertexHandle) -> T,
        view: impl FnOnce(Option<VertexView<'a>>) -> T,
    ) -> T {
        if node.is_delta() {
            view(self.delta.vertex(node))
        } else if self.dirty_vertices.contains(node.slot()) {
            view(self.delta.base_vertex(node.to_base()))
        } else {
            base(&self.base, node.to_base())
        }
    }

    /// The edge counterpart of [`Self::vertex_read`].
    #[inline]
    fn edge_read<'a, T>(
        &'a self,
        edge: LayeredEdge,
        base: impl FnOnce(&'a TraversalGraph, EdgeHandle) -> T,
        view: impl FnOnce(Option<EdgeView<'a>>) -> T,
    ) -> T {
        if edge.is_delta() {
            view(self.delta.edge(edge))
        } else if self.dirty_edges.contains(edge.slot()) {
            view(self.delta.base_edge(edge.to_base()))
        } else {
            base(&self.base, edge.to_base())
        }
    }
}

/// The one place that decides whether a base can carry a layer: `Err` names what the layer
/// cannot handle. Today: a slot count the handles cannot address.
fn check_base(base: &TraversalGraph) -> Result<(), GraphError> {
    if base.graph.node_slots() >= MAX_BASE_SLOTS || base.graph.edge_slots() >= MAX_BASE_SLOTS {
        return Err(GraphError::unsupported(
            "a layer over a base with 2^31 or more vertex or edge slots",
        ));
    }
    Ok(())
}

/// A layer over an empty base.
impl Default for LayeredGraph {
    fn default() -> Self {
        Self::new_unchecked(Arc::new(TraversalGraph::new()))
    }
}

impl std::fmt::Debug for LayeredGraph {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LayeredGraph")
            .field("base_vertices", &self.base.graph.node_count())
            .field("base_edges", &self.base.graph.edge_count())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::GraphStorage;
    use crate::value::PropertyMap;

    /// A handle of the delta that names nothing, and a base handle whose slot was reused before
    /// the base was frozen, read as removed everywhere (the stale-handle contract).
    #[test]
    fn unknown_and_stale_handles_read_as_removed() {
        let mut base = TraversalGraph::new();
        let old = base
            .insert_vertex(Some("a".into()), vec!["n".into()], PropertyMap::default())
            .unwrap();
        base.drop_vertex(old).unwrap();
        let new = base
            .insert_vertex(Some("b".into()), vec!["n".into()], PropertyMap::default())
            .unwrap();
        base.insert_edge(Some("e".into()), None, new, new, PropertyMap::default())
            .unwrap();
        assert_eq!(old.index(), new.index(), "the slot is reused");
        let layer = LayeredGraph::over(Arc::new(base)).unwrap();
        for node in [
            LayeredVertex::from_base(old),
            LayeredVertex::delta_for_test(0),
        ] {
            assert!(!layer.contains_vertex(node).unwrap());
            assert_eq!(layer.vertex_id(node).unwrap(), None);
            assert_eq!(layer.vertex_label(node).unwrap(), None);
            assert!(layer.vertex_labels(node).unwrap().is_empty());
            assert!(layer.vertex_properties(node).unwrap().is_none());
            assert_eq!(layer.out_edges(node).unwrap().count(), 0);
            assert_eq!(layer.in_edges(node).unwrap().count(), 0);
        }
        let edge = LayeredEdge::delta_for_test(0);
        assert!(!layer.contains_edge(edge).unwrap());
        assert_eq!(layer.edge_id(edge).unwrap(), None);
        assert_eq!(layer.edge_label(edge).unwrap(), None);
        assert!(layer.edge_properties(edge).unwrap().is_none());
        assert_eq!(layer.edge_endpoints(edge).unwrap(), None);
        // The live vertex in the reused slot still reads, with its self-loop in both lists.
        let live = LayeredVertex::from_base(new);
        assert_eq!(layer.vertex_id(live).unwrap(), Some("b"));
        assert_eq!(layer.out_edges(live).unwrap().count(), 1);
        assert_eq!(layer.in_edges(live).unwrap().count(), 1);
    }

    #[test]
    fn the_dirty_bits_cover_every_base_slot() {
        let layer = LayeredGraph::over(Arc::new(TraversalGraph::tinkerpop_modern())).unwrap();
        assert!((0..6).all(|slot| !layer.dirty_vertices.contains(slot)));
        assert!((0..6).all(|slot| !layer.dirty_edges.contains(slot)));
        assert_eq!(
            format!("{layer:?}"),
            "LayeredGraph { base_vertices: 6, base_edges: 6, .. }"
        );
    }
}
