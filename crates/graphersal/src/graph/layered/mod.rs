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
//!   untouched element. A set bit means "consult the delta": a base vertex may have a stub with
//!   new incident edges, be removed or have a copied-up image; the delta answers which. Adjacency
//!   is decided per vertex: a removed base edge dirties both of its endpoints, so the base lists
//!   of a clean vertex are read without a per-edge test.
//! - **Delta:** added elements, tombstones, images, the stubs of base vertices that carry the
//!   new edges touching them (`delta`).
//!
//! # State of the implementation
//!
//! Reads are complete: every read method of `GraphStorage` answers from the base merged with
//! the delta (scans and label lookups list the base first, then the added elements; id lookups
//! ask the base, then the delta's id index), with the dirty-bit check where a changed base
//! element will need it.
//!
//! Writes (S1.3, S1.4): new vertices and new edges, with any endpoints (added or base vertices),
//! are stored in the delta (`delta`), each write running its `graph/write_rules.rs` function
//! first, then the id check (`write`); ids are unique across base and delta and the auto-id
//! sequences continue the base's. A new edge at a base vertex goes to that vertex's stub and
//! sets its dirty bit. Adjacency lists the delta's edges first (newest first), then the base's,
//! which is the order a `TraversalGraph` holding the same writes lists them in (it puts a new
//! edge at the front). The added elements can be changed and dropped as in a
//! `TraversalGraph`. Base vertices and edges can be dropped (S1.5): a tombstone in the delta
//! behind the element's dirty bit; dropping a base vertex drops its base edges (each dirtying
//! both endpoints) and the added edges at its stub, and the stub with them. A dropped base id
//! is free again for a new element. Property, label and path writes to a base element (S1.6)
//! copy it up into an image in the delta on the first write that changes something (a write
//! of an identical value changes nothing and copies nothing); an image identical to the base
//! element again is dropped. The layer keeps its own copy of the base's
//! statistics, updated per write.
//!
//! Units (S1.8): `GraphStorage::capabilities` claims `writable` and `atomic`. The layer keeps
//! an undo log of its own (`units`) with `TraversalGraph`'s semantics: a unit records the
//! inverse of every write to the delta, nested units are savepoints (log positions), a rollback
//! replays the log in reverse down to the mark, so a failing traversal leaves nothing; the
//! auto-id sequences are never rolled back.
//!
//! Change capture (S1.9): `capabilities` also claims `change_capture`. Commit hooks, the
//! instance commit clock, `discard_unit` (dry runs) and `set_unit_metadata` behave as on a
//! `TraversalGraph`; the [`ChangeSet`](crate::graph::ChangeSet) of a unit is derived from the
//! layer's undo log (`capture`) with the same mutations in the same order as a `TraversalGraph`
//! holding the same data reports for the same writes (also a write of an identical value to a
//! base element, which stores nothing but is recorded for the change set).
//!
//! Schema and catalog (S1.7): the layer copies the base's schema and catalog when it is created
//! and from then on reads and writes only its copies; the base's are never read again nor
//! written, so a second layer over the same base keeps the base's. `set_schema` checks the
//! layer's current state exactly as `TraversalGraph::set_schema` checks its own (the generic
//! `scan_storage_change`, the same errors), then stores the schema. The catalog takes saved
//! queries, jobs, host kinds and kinds this version does not know; the kinds that need index,
//! compression or embedding machinery (property indexes, compression rules, models) are
//! refused until the layer has index deltas (stage 4), and so are schemas with vector rules
//! and bases with unique indexes or vector rules (plan decision P5, [`LayeredGraph::over`]).
//! The property index lookups and the execution policy are the base's.
//!
//! Memory (S1.11, `memory`): `memory_usage` reports the layer's own footprint as the total
//! and the shared base as `MemoryStats::shared_bytes`, outside the total; the process memory
//! ceiling checks the outermost unit at its commit on the layer's own footprint, as
//! `TraversalGraph` does on its own.

mod capture;
mod delta;
mod dirty;
mod handle;
mod memory;
mod storage;
mod units;
mod write;

use std::sync::Arc;

pub use handle::{LayeredEdge, LayeredVertex};

use crate::error::GraphError;
use crate::graph::definition::Catalog;
use crate::graph::schema::GraphSchema;
use crate::graph::topology::NodeIndex;
use crate::graph::{EdgeHandle, GraphStatistics, TraversalGraph, VertexHandle};
use delta::{Delta, EdgeView, VertexView};
use dirty::SlotBits;
use handle::MAX_BASE_SLOTS;
use units::UnitLog;

/// A [`GraphStorage`](crate::graph::GraphStorage) made of a frozen base graph shared by any
/// number of layers and a layer of its own: added elements, removed and modified base
/// elements. Handles ([`LayeredVertex`], [`LayeredEdge`]) are 8 bytes; a base element untouched
/// by the layer is read straight from the base, with one bit test and no lookup. Book page
/// "Layered Graphs".
///
/// **EXPERIMENTAL**, like [`GraphStorage`](crate::graph::GraphStorage).
///
/// **Supported:** every read; new vertices and edges (also at base vertices); changes and
/// drops of added and of base elements (a changed base element is copied into the layer, the
/// base is never written); its own copies of the base's schema and catalog (saved queries,
/// jobs, host kinds); units with an undo log of its own (a traversal that fails leaves
/// nothing); change capture (commit hooks, dry runs) with the change sets a `TraversalGraph`
/// reports for the same writes; the process memory ceiling at commit.
///
/// **Capabilities:** `writable`, `atomic`, `change_capture`, and
/// `exact_property_index_lookups` as the base states it. Not `property_indexes` (creating an
/// index is refused) nor `durability`.
///
/// **Memory:** [`memory_usage`](crate::graph::GraphStorage::memory_usage) reports the layer's
/// own footprint (its delta and one dirty bit per base slot) as the total and the shared base
/// as [`MemoryStats::shared_bytes`](crate::graph::MemoryStats::shared_bytes), outside the
/// total. [`shrink_to_fit`](crate::graph::GraphStorage::shrink_to_fit) releases the delta's
/// spare capacity.
///
/// **Not supported yet** ([`GraphError::Unsupported`]):
/// - persistence: a layer cannot be saved, opened from a Store or journaled;
/// - a base with a unique property index or vector rules ([`Self::over`]); vector rules,
///   property index, compression rule and embedding model definitions on the layer. The
///   base's plain indexes serve lookups only while the layer has not touched that element
///   kind; then the engine scans.
///
/// Values the layer stores are uncompressed. After a drop followed by a create, or a drop or
/// label removal followed by a label lookup, unordered traversals may list elements in
/// another order than a `TraversalGraph` with the same data.
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
///
/// // New elements live in the layer; the shared base is unchanged.
/// let mut layer = layer;
/// GraphTraversalSource::new(&mut layer).add_v("person").property("name", "ada").iterate().unwrap();
/// assert_eq!(GraphTraversalSource::new(&layer).v(None).count().next().unwrap(), Some(7_i64.into()));
/// assert_eq!(TraversalGraph::vertex_count(&base), 6);
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
    /// The statistics of the layer as a whole: the base's, cloned when the layer is created,
    /// then updated by every write exactly as `TraversalGraph`'s raw helpers update its own.
    statistics: GraphStatistics,
    /// The last vertex auto id handed out (or raised to): starts at the base's sequence, so an
    /// auto id never names a base vertex nor an id the base handed out before.
    node_sequence: u64,
    /// The edge counterpart of `node_sequence`.
    edge_sequence: u64,
    /// The layer's own schema: the base's, copied when the layer is created, then changed only
    /// by the layer's `set_schema`. Every read and write rule of the layer uses it; the base's
    /// schema is never read again nor written.
    schema: GraphSchema,
    /// The layer's own catalog: the base's definitions, copied when the layer is created, then
    /// changed only by the layer's catalog writes (the kinds without storage machinery, see
    /// `storage::check_layer_kind`).
    catalog: Catalog,
    /// The undo log of the open units (`units`): what makes a unit of the layer atomic.
    units: UnitLog,
    /// The cached walk of the layer's own footprint (`memory`): what the memory budget starts
    /// from without walking the delta on every run.
    footprint: crate::graph::memory::FootprintCache,
}

impl LayeredGraph {
    /// A new, empty layer over `base`: until it is written to it reads exactly as `base`.
    ///
    /// # Errors
    ///
    /// [`GraphError::Unsupported`] when `base` cannot carry a layer: it has 2^31 or more vertex
    /// or edge slots (live or vacant), which the layer's handles cannot address; or, not
    /// supported on a layered graph yet, it has a unique property index or its schema has
    /// vector rules (properties an embedder derives from another property).
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
        let schema = base.schema.read().clone();
        Self {
            dirty_vertices: SlotBits::new(base.graph.node_slots()),
            dirty_edges: SlotBits::new(base.graph.edge_slots()),
            delta: Delta::default(),
            statistics: base.statistics.clone(),
            node_sequence: base.node_sequence,
            edge_sequence: base.edge_sequence,
            schema,
            catalog: base.definitions.clone(),
            units: UnitLog::after(base.undo.last_committed_at),
            footprint: crate::graph::memory::FootprintCache::default(),
            base,
        }
    }

    /// `true` iff the base vertex `node`, a CURRENT handle (from the base's own scans, indexes
    /// and adjacency), is visible in the layer: untouched (one bit test), or dirty and not
    /// removed (one more bit test; never the image lookup).
    #[inline]
    fn base_vertex_live(&self, node: VertexHandle) -> bool {
        let slot = node.index().slot();
        !self.dirty_vertices.contains(slot) || !self.delta.is_removed_vertex(slot)
    }

    /// The edge counterpart of [`Self::base_vertex_live`].
    #[inline]
    fn base_edge_live(&self, edge: EdgeHandle) -> bool {
        let slot = edge.index().slot();
        !self.dirty_edges.contains(slot) || !self.delta.is_removed_edge(slot)
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

    /// The delta node slot holding the delta part of `node`'s adjacency: the added vertex
    /// itself, the stub of a dirty base vertex (one hash lookup, only behind the bit), and
    /// [`NodeIndex::end`] (lists nothing) for a clean base vertex, a vertex without a stub or a
    /// stale handle.
    #[inline]
    fn delta_adjacency_index(&self, node: LayeredVertex) -> NodeIndex {
        if node.is_delta() {
            self.delta.delta_index(node)
        } else if self.dirty_vertices.contains(node.slot()) {
            self.delta.stub_index(node.to_base())
        } else {
            NodeIndex::end()
        }
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
            view(self.delta.base_vertex(&self.base, node.to_base()))
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
            view(self.delta.base_edge(&self.base, edge.to_base()))
        } else {
            base(&self.base, edge.to_base())
        }
    }
}

/// The `operation` of the refusal of a base with a unique property index (plan decision P5).
const UNIQUE_INDEX_BASE: &str =
    "a layer over a base with unique property indexes: not supported on a layered graph yet";

/// The `operation` of the refusal of a base whose schema has vector rules (P5).
const VECTOR_RULES_BASE: &str = "a layer over a base whose schema has vector rules \
     (properties derived by an embedder): not supported on a layered graph yet";

/// The one place that decides whether a base can carry a layer: `Err` names what the layer
/// cannot handle. Today:
/// - a slot count the handles cannot address;
/// - (P5, until index deltas) a unique property index: the layer cannot check uniqueness
///   across base and delta;
/// - (P5, until index deltas) vector rules in the schema: the rule pass needs units and change
///   capture, and the layer cannot keep the derived vectors of base elements in step.
fn check_base(base: &TraversalGraph) -> Result<(), GraphError> {
    if base.graph.node_slots() >= MAX_BASE_SLOTS || base.graph.edge_slots() >= MAX_BASE_SLOTS {
        return Err(GraphError::unsupported(
            "a layer over a base with 2^31 or more vertex or edge slots",
        ));
    }
    if base.definitions.iter().any(|definition| {
        definition
            .as_property_index()
            .is_some_and(|index| index.is_unique())
    }) {
        return Err(GraphError::unsupported(UNIQUE_INDEX_BASE));
    }
    if crate::graph::vector_rules::has_rules(&base.schema.read()) {
        return Err(GraphError::unsupported(VECTOR_RULES_BASE));
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

    /// A stub (delta slot 0 here) is never a vertex: a handle forged to name it reads as
    /// removed, lists nothing and is refused as an edge endpoint; the base vertex it serves is
    /// dirty and still reads as the base holds it.
    #[test]
    fn a_stub_is_invisible_even_to_a_forged_handle() {
        let mut layer = LayeredGraph::over(Arc::new(TraversalGraph::tinkerpop_modern())).unwrap();
        let marko = layer.get_vertex_by_id("1").unwrap().unwrap();
        layer
            .insert_edge(
                None,
                Some("self".into()),
                marko,
                marko,
                PropertyMap::default(),
            )
            .unwrap();
        assert!(layer.dirty_vertices.contains(marko.slot()));
        assert!(!layer.dirty_edges.any());
        let stub = LayeredVertex::delta_for_test(0);
        assert!(!layer.contains_vertex(stub).unwrap());
        assert_eq!(layer.vertex_id(stub).unwrap(), None);
        assert_eq!(layer.out_edges(stub).unwrap().count(), 0);
        assert_eq!(layer.in_edges(stub).unwrap().count(), 0);
        assert!(
            layer
                .insert_edge(None, None, stub, marko, PropertyMap::default())
                .is_err()
        );
        assert_eq!(layer.vertex_count().unwrap(), 6);
        assert_eq!(layer.vertices().unwrap().count(), 6);
        assert_eq!(layer.vertex_id(marko).unwrap(), Some("1"));
        assert_eq!(layer.out_edges(marko).unwrap().count(), 4);
        assert_eq!(layer.in_edges(marko).unwrap().count(), 1);
    }

    /// Dropping a base vertex with added edges at its stub removes the stub with them (slot
    /// freed, generation bumped, both maps cleared), tombstones the vertex and its base edges
    /// and dirties the other endpoints of those edges; a new added vertex reusing the stub's
    /// slot is an ordinary vertex.
    #[test]
    fn a_dropped_base_vertex_takes_its_stub_along() {
        let mut layer = LayeredGraph::over(Arc::new(TraversalGraph::tinkerpop_modern())).unwrap();
        let marko = layer.get_vertex_by_id("1").unwrap().unwrap();
        let peter = layer.get_vertex_by_id("6").unwrap().unwrap();
        layer
            .insert_edge(None, None, peter, marko, PropertyMap::default())
            .unwrap();
        assert_ne!(layer.delta.stub_index(marko.to_base()), NodeIndex::end());
        layer.drop_vertex(marko).unwrap();
        assert_eq!(layer.delta.stub_index(marko.to_base()), NodeIndex::end());
        assert_ne!(
            layer.delta.stub_index(peter.to_base()),
            NodeIndex::end(),
            "peter keeps his stub"
        );
        assert!(layer.dirty_vertices.contains(marko.slot()));
        assert!(
            layer
                .delta
                .base_vertex(&layer.base, marko.to_base())
                .is_none()
        );
        // marko's base edges reached vadas (2), josh (4) and lop (3): all dirty now.
        for id in ["2", "3", "4"] {
            let node = layer.get_vertex_by_id(id).unwrap().unwrap();
            assert!(layer.dirty_vertices.contains(node.slot()), "{id}");
        }
        let ripple = layer.get_vertex_by_id("5").unwrap().unwrap();
        assert!(
            !layer.dirty_vertices.contains(ripple.slot()),
            "ripple untouched"
        );
        assert_eq!(layer.vertex_count().unwrap(), 5);
        assert_eq!(layer.edge_count().unwrap(), 3);
        let new = layer
            .insert_vertex(None, vec!["n".into()], PropertyMap::default())
            .unwrap();
        assert!(layer.contains_vertex(new).unwrap());
        assert_eq!(layer.vertices().unwrap().count(), 6);
        assert_eq!(layer.out_edges(new).unwrap().count(), 0);
    }

    /// A vertex image lives in the vertex's (invisible) stub; an equal-value write makes no
    /// dirty bit; a return to clean drops the image but keeps the dirty bit and the stub, and
    /// the kind counts as untouched again for the property-index lookups. A gained label is a
    /// stub membership in the delta's label bucket, a lost one a bit in its lost set.
    #[test]
    fn a_vertex_image_lives_in_its_stub_and_returns_to_clean() {
        let mut layer = LayeredGraph::over(Arc::new(TraversalGraph::tinkerpop_modern())).unwrap();
        let marko = layer.get_vertex_by_id("1").unwrap().unwrap();
        layer
            .set_vertex_property(marko, "age".into(), 29.into())
            .unwrap();
        assert!(
            !layer.dirty_vertices.any(),
            "an equal value dirties nothing"
        );
        layer
            .set_vertex_property(marko, "age".into(), 30.into())
            .unwrap();
        assert!(layer.dirty_vertices.contains(marko.slot()));
        assert!(layer.delta.imaged_vertices.contains(marko.slot()));
        assert_ne!(layer.delta.stub_index(marko.to_base()), NodeIndex::end());
        assert!(layer.delta.vertices_touched());
        assert_eq!(layer.vertex_count().unwrap(), 6);
        assert_eq!(layer.vertices().unwrap().count(), 6);
        layer.add_vertex_label(marko, "x".into()).unwrap();
        layer.remove_vertex_label(marko, "person").unwrap();
        assert_eq!(
            layer.delta.vertices_by_label("x").collect::<Vec<_>>(),
            [marko],
            "the stub's membership names the base vertex"
        );
        assert!(
            layer
                .delta
                .lost_vertex_label("person")
                .is_some_and(|lost| lost.contains(marko.slot()))
        );
        layer.add_vertex_label(marko, "person".into()).unwrap();
        layer.remove_vertex_label(marko, "x").unwrap();
        layer
            .set_vertex_property(marko, "age".into(), 29.into())
            .unwrap();
        assert!(!layer.delta.imaged_vertices.contains(marko.slot()));
        assert!(layer.dirty_vertices.contains(marko.slot()), "kept");
        assert!(!layer.delta.vertices_touched());
        assert_eq!(layer.delta.vertices_by_label("x").count(), 0);
        assert!(
            !layer
                .delta
                .lost_vertex_label("person")
                .is_some_and(|lost| lost.contains(marko.slot()))
        );
        assert_eq!(layer.vertex_count().unwrap(), 6);
        assert_eq!(
            layer.statistics,
            GraphStatistics::from_storage(&layer).unwrap()
        );
    }

    /// A rolled-back unit leaves the delta's own bookkeeping as before: no image, no
    /// tombstone, no relabeled or gained edge, the kinds untouched again for the property-index
    /// lookups; the dirty bits and the stubs the unit made may stay (invisible).
    #[test]
    fn a_rollback_restores_the_delta_bookkeeping() {
        use crate::graph::hooks::RollbackReason;
        let mut layer = LayeredGraph::over(Arc::new(TraversalGraph::tinkerpop_modern())).unwrap();
        let marko = layer.get_vertex_by_id("1").unwrap().unwrap();
        let lop = layer.get_vertex_by_id("3").unwrap().unwrap();
        let knows = layer.get_edge_by_id("0").unwrap().unwrap();
        let mark = layer.begin_unit().unwrap();
        layer
            .set_vertex_property(marko, "age".into(), 30.into())
            .unwrap();
        layer.remove_vertex_label(marko, "person").unwrap();
        layer.add_vertex_label(lop, "x".into()).unwrap();
        layer.set_edge_label(knows, "met".into()).unwrap();
        layer
            .insert_edge(None, None, lop, marko, PropertyMap::default())
            .unwrap();
        layer.drop_vertex(marko).unwrap();
        assert!(layer.delta.vertices_touched() && layer.delta.edges_touched());
        layer
            .rollback_unit(mark, &RollbackReason::failed(&"test"))
            .unwrap();
        assert!(!layer.delta.vertices_touched(), "no image, no tombstone");
        assert!(!layer.delta.edges_touched());
        assert!(!layer.delta.imaged_vertices.contains(marko.slot()));
        assert!(!layer.delta.is_removed_vertex(marko.slot()));
        assert!(
            layer
                .delta
                .lost_vertex_label("person")
                .is_none_or(|lost| !lost.contains(marko.slot()))
        );
        assert_eq!(layer.delta.vertices_by_label("x").count(), 0);
        assert_eq!(layer.delta.gained_edges("met").count(), 0);
        assert!(!layer.delta.relabeled_edges().contains(knows.slot()));
        assert_eq!(layer.vertex_count().unwrap(), 6);
        assert_eq!(layer.edge_count().unwrap(), 6);
        assert_eq!(
            layer.statistics,
            GraphStatistics::from_storage(&layer).unwrap()
        );
        assert_eq!(layer.statistics, layer.base.statistics);
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
