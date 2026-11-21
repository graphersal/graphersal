//! The delta of a [`LayeredGraph`](super::LayeredGraph): what the layer stores beside the base.
//!
//! Design `ai/roadmap/layered-graph.md` §3. It holds the elements the layer ADDS, in a
//! slot store of their own built from `TraversalGraph`'s parts (plan P2: the delta is not a
//! small `TraversalGraph`): a [`GraphType`] topology (stable slots, LIFO free lists, linked
//! adjacency), the slot [`Generations`], an own [`Labels`] table and [`ShapeTable`], and a
//! [`GraphIndex`] (id index and label indexes) over the delta-local handles. A layer handle of a
//! delta element is the delta-local handle with bit 31 of its slot set
//! ([`LayeredVertex::from_delta`]).
//!
//! # Stubs: new edges at base vertices (S1.4)
//!
//! Every edge the layer adds lives in the delta topology, whatever its endpoints (delta-delta,
//! delta-base, base-delta, base-base, a self-loop at a base vertex): one edge slot space. A base
//! vertex that gets its first new incident edge gets a hidden **stub** node in the delta
//! topology, which carries the delta part of its adjacency lists. The map base slot -> stub
//! (`stubs`) is consulted only behind the base vertex's dirty bit, so a clean vertex never
//! touches a hash map (plan P8). A stub is invisible: it is not in [`Delta::vertices`], the
//! counts, the statistics or the id index, it is in a label bucket only for a label its base
//! vertex gained (images, below), and no layer handle names it
//! (`stub_bases` turns a stub endpoint back into the base vertex's handle; [`Delta::vertex`]
//! and the adjacency of a delta handle refuse a stub slot). A stub stays when its last edge is
//! dropped (it is empty then, and still invisible); its base vertex stays dirty.
//!
//! # Tombstones: removed base elements (S1.5)
//!
//! A removed base vertex or edge is a set bit in `removed_vertices` / `removed_edges`, bitsets
//! over base slots that start empty and grow on demand (a layer that never removes a base
//! element allocates nothing for them). They are read only behind the element's dirty bit
//! ([`Delta::base_vertex`], [`Delta::base_edge`]), so a clean element never looks at them, and a
//! dirty one pays a bit test, never a hash lookup (plan P8). A bitset beats a hash set here:
//! one bit per base slot up to the highest removed one, an O(1) test without hashing, and the
//! base never reuses a slot, so a bit never has to be told apart from a later occupant. The
//! base slot is never reused (the base is frozen), so a tombstone stays valid for every handle
//! of that slot: a stale handle of a removed base element never aliases anything.
//!
//! Removing a base vertex removes its stub too (after the caller removed every incident edge):
//! the slot is freed, its generation bumped, `stubs` and `stub_bases` cleared together, so
//! [`Delta::stub_index`] never hands out the stub of a removed vertex.
//!
//! # Images: modified base elements (S1.6)
//!
//! The first write that changes a base vertex or edge copies it up whole: its labels and its
//! property map with LOGICAL values (a base map may be coded or hold compressed or deduplicated
//! strings; the image holds the plain values the base's readers return, so a read answers
//! identically). Later writes change the image. The image is found only behind the element's
//! dirty bit, and then only when its bit in `imaged_vertices` / `imaged_edges` is set, so a
//! dirty element without an image (a stub, a lost base edge) pays a bit test, not a lookup:
//!
//! - **A vertex image lives in the vertex's stub** (S1.4): the stub's label set and property map
//!   ARE the image (empty, `LabelSetId::EMPTY`, while there is none). One hash lookup
//!   (`stubs`), the same one the vertex's added edges need.
//! - **An edge image** is an entry of `edge_images`, keyed by base slot.
//!
//! [`Delta::base_vertex`] / [`Delta::base_edge`] check the tombstone first, then the image, and
//! otherwise answer with the base's own state.
//!
//! **Return to clean:** when a write leaves an image identical to the base element (labels in
//! order, and every property, in order, by `value/identity.rs` `identity_eq`), the image is
//! dropped and the element reads the base again. The check runs only after a write to an
//! element that already HAD an image (a fresh copy-up differs from the base by construction:
//! the write that made it changed something), and costs one pass over the labels and the map.
//! The dirty bit stays set (plan invariant S1.5): the element may still have a stub or lost
//! edges, and a set bit alone never means "changed".
//!
//! **Label lookups of modified base elements**, kept as a function of the current state (so a
//! return to clean needs no extra step):
//! - a base vertex that LOST a base label has its bit set in `lost_vertex_labels[label]`: a
//!   by-label scan of the base bucket fetches that set once per scan and tests one bit per
//!   dirty member (never a hash lookup per element); members keep the base bucket's order;
//! - a base vertex that GAINED a label its base element lacks is a member of the delta's label
//!   bucket through its STUB handle, in the order of the writes, interleaved with the added
//!   vertices exactly as a `TraversalGraph` appends both ([`Delta::vertices_by_label`] turns
//!   the stub back into the base handle);
//! - a base edge whose label differs from its base label has its bit set in `relabeled_edges`
//!   (skipped in its base bucket) and is listed in `gained_edge_labels[label]`, which a
//!   by-label scan lists after the base bucket and before the added edges. A `TraversalGraph`
//!   appends a relabeled edge to the bucket, interleaved with new edges of that label: the
//!   layer lists the same members, in another order only when a bucket holds both relabeled
//!   base edges and added edges.
//!
//! The delta stores what the write rules (`graph/write_rules.rs`) returned: no check runs here.
//! Values are stored uncompressed (plan P5).

use compact_str::CompactString;
use fnv::FnvBuildHasher;

use super::dirty::SlotBits;
use super::handle::{LayeredEdge, LayeredVertex, MAX_BASE_SLOTS};
use crate::error::GraphError;
use crate::graph::handle::{EdgeGenerations, FIRST_GENERATION, Generations};
use crate::graph::labels::{LabelSetId, Labels};
use crate::graph::shape::ShapeTable;
use crate::graph::topology::{Direction, NodeIndex};
use crate::graph::undo::restore_stored_property;
use crate::graph::{
    EdgeHandle, GraphIndex, GraphStatistics, GraphStorage, GraphType, MemoryStats, TraversalGraph,
    VertexHandle,
};
use crate::value::identity::identity_eq;
use crate::value::{ElementProperty, PropertyMap, logical_element};

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
    /// One entry, or empty for an unlabeled edge (the form the schema validator takes).
    pub(crate) label: &'a [CompactString],
    pub(crate) properties: Option<&'a PropertyMap>,
    pub(crate) endpoints: (LayeredVertex, LayeredVertex),
}

/// The layer's own data: added elements, stubs, tombstones and images (module docs).
#[derive(Debug)]
pub(crate) struct Delta {
    /// The added vertices and edges, in the delta's own slot space (bit 31 clear here).
    graph: GraphType,
    /// The id index and the label indexes of the added elements, over delta-local handles.
    index: GraphIndex,
    /// Slot generations of the added vertices: bumped when a slot is freed.
    node_generations: Generations,
    /// Slot generations of the added edges.
    edge_generations: Generations,
    /// The interned labels and label sets the delta slots refer to.
    labels: Labels,
    /// The interned key lists of the delta's property maps.
    shapes: ShapeTable,
    /// Base vertex slot -> its stub in `graph` (module docs). Asked only for a dirty base vertex.
    stubs: hashbrown::HashMap<u32, NodeIndex, FnvBuildHasher>,
    /// Per delta node slot: `Some(base handle)` when the slot holds the stub of that base
    /// vertex, `None` (or beyond the end) for an added vertex or a vacant slot.
    stub_bases: Vec<Option<VertexHandle>>,
    /// The tombstones of removed base vertices, by base slot (module docs). Read only behind
    /// the vertex's dirty bit.
    removed_vertices: SlotBits,
    /// The tombstones of removed base edges, by base slot.
    removed_edges: SlotBits,
    /// The number of set bits in `removed_vertices` (the base vertices the layer removed).
    removed_vertex_count: usize,
    /// The number of set bits in `removed_edges`.
    removed_edge_count: usize,
    /// Base vertex slots whose stub holds an image (module docs), read behind the dirty bit.
    pub(super) imaged_vertices: SlotBits,
    /// Base edge slots with an entry in `edge_images`.
    imaged_edges: SlotBits,
    /// The number of set bits in `imaged_vertices`.
    vertex_image_count: usize,
    /// The images of modified base edges, by base slot.
    edge_images: hashbrown::HashMap<u32, EdgeImage, FnvBuildHasher>,
    /// Per base label: the base vertex slots whose image lacks it (module docs).
    lost_vertex_labels: hashbrown::HashMap<CompactString, SlotBits, FnvBuildHasher>,
    /// Base edge slots whose image's label differs from the base edge's.
    relabeled_edges: SlotBits,
    /// Per label: the base edges relabeled to it, in the order they gained it (a removal moves
    /// the last one into its place, as a label bucket does).
    gained_edge_labels: hashbrown::HashMap<CompactString, Vec<EdgeHandle>, FnvBuildHasher>,
}

/// The copied-up state of a modified base edge: what a write may change (its endpoints and id
/// never change).
#[derive(Debug)]
pub(crate) struct EdgeImage {
    /// `None` for an unlabeled edge.
    pub(super) label: Option<CompactString>,
    /// As the base stores it: `None` when the base edge has no map and no write gave it one.
    pub(super) properties: Option<PropertyMap>,
}

/// The copied-up state of a modified base vertex taken out of the layer by its drop: what an
/// undo of the drop puts back into a new stub.
#[derive(Debug)]
pub(crate) struct VertexCopy {
    pub(super) labels: Vec<CompactString>,
    pub(super) properties: PropertyMap,
}

/// An endpoint of a removed added edge, named so that an undo finds it again: a base vertex by
/// its handle (base handles never change), an added vertex by its id (an undone drop of it
/// gives it a new handle).
#[derive(Debug)]
pub(crate) enum End {
    /// A base vertex (the edge was linked to its stub).
    Base(VertexHandle),
    /// An added vertex.
    Delta(CompactString),
}

/// An added edge taken out of the delta: everything an undo needs to store it again.
#[derive(Debug)]
pub(crate) struct RemovedEdge {
    pub(super) id: CompactString,
    pub(super) label: Option<CompactString>,
    pub(super) properties: Option<PropertyMap>,
    pub(super) from: End,
    pub(super) to: End,
}

/// An added vertex taken out of the delta (its edges are [`RemovedEdge`]s of their own).
#[derive(Debug)]
pub(crate) struct RemovedVertex {
    pub(super) id: CompactString,
    pub(super) labels: Vec<CompactString>,
    pub(super) properties: PropertyMap,
}

/// The old state of one top-level property: its position and value, or `None` when it was
/// absent (what an undo restores, `graph::undo::restore_stored_property`).
pub(crate) type OldProperty = Option<(usize, ElementProperty)>;

/// One property change of a base element, as the write rules returned it.
pub(crate) enum PropertyChange {
    /// Set `key` to the value.
    Set(CompactString, ElementProperty),
    /// Remove `key`.
    Drop(CompactString),
}

impl PropertyChange {
    /// The key the change writes or removes.
    pub(crate) fn key(&self) -> &CompactString {
        match self {
            PropertyChange::Set(key, _) | PropertyChange::Drop(key) => key,
        }
    }

    /// `true` when applying the change to `current` (the element's map as the layer reads it)
    /// would change nothing: the key holds an identical value (`identity_eq`, so `1` and `1.0`
    /// differ) or, for a drop, is absent. Such a write creates no image (plan P6).
    pub(crate) fn is_noop(&self, current: Option<&PropertyMap>) -> bool {
        match self {
            PropertyChange::Set(key, value) => current
                .and_then(|map| map.get(key))
                .is_some_and(|old| identity_eq(old, value)),
            PropertyChange::Drop(key) => current.is_none_or(|map| map.get(key).is_none()),
        }
    }

    /// Applies the change to an image map, the value made logical first; returns the key's
    /// old state.
    fn apply(self, shapes: &mut ShapeTable, map: &mut PropertyMap) -> OldProperty {
        match self {
            PropertyChange::Set(key, value) => {
                let (position, old) = shapes.insert_full(map, key, logical(value));
                old.map(|old| (position, old))
            }
            PropertyChange::Drop(key) => shapes
                .shift_remove_full(map, &key)
                .map(|(position, _, old)| (position, old)),
        }
    }
}

/// `value` with every storage-only string (compressed, deduplicated) made plain: what the
/// delta stores (plan P5).
#[inline]
pub(super) fn logical(value: ElementProperty) -> ElementProperty {
    if value.holds_compressed() {
        value.into_logical()
    } else {
        value
    }
}

/// Whether two property maps are identical: the same keys in the same order, each value
/// `identity_eq` (the return-to-clean test, module docs).
fn maps_identical(a: &PropertyMap, b: &PropertyMap) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b.iter())
            .all(|((ka, va), (kb, vb))| ka == kb && identity_eq(va, vb))
}

impl Default for Delta {
    fn default() -> Self {
        Self {
            graph: GraphType::new(),
            index: GraphIndex::new(),
            node_generations: Generations::default(),
            edge_generations: Generations::default(),
            labels: Labels::default(),
            shapes: ShapeTable::new(),
            stubs: hashbrown::HashMap::with_hasher(FnvBuildHasher::default()),
            stub_bases: Vec::new(),
            removed_vertices: SlotBits::default(),
            removed_edges: SlotBits::default(),
            removed_vertex_count: 0,
            removed_edge_count: 0,
            imaged_vertices: SlotBits::default(),
            imaged_edges: SlotBits::default(),
            vertex_image_count: 0,
            edge_images: hashbrown::HashMap::with_hasher(FnvBuildHasher::default()),
            lost_vertex_labels: hashbrown::HashMap::with_hasher(FnvBuildHasher::default()),
            relabeled_edges: SlotBits::default(),
            gained_edge_labels: hashbrown::HashMap::with_hasher(FnvBuildHasher::default()),
        }
    }
}

/// The weight of the live added vertex `node` in `graph` (`None` when stale).
fn live_vertex_mut(
    graph: &mut GraphType,
    node: LayeredVertex,
) -> Option<&mut crate::graph::VertexData> {
    let local = node.to_delta();
    graph
        .node_weight_mut(local.index())
        .filter(|vertex| vertex.3 == local.generation())
}

/// The delta slot store is full: the next slot would carry the tag bit itself.
fn delta_slots_exhausted() -> GraphError {
    GraphError::unsupported("more than 2^31 added vertex or edge slots in a layered graph")
}

impl Delta {
    /// Vertices the delta adds to the base count (added minus removed base vertices; stubs are
    /// not vertices).
    #[inline]
    pub(crate) fn vertex_count_change(&self) -> isize {
        (self.graph.node_count() - self.stubs.len()) as isize - self.removed_vertex_count as isize
    }

    /// The base vertex whose stub sits in delta node slot `index`, `None` for any other slot.
    #[inline]
    fn stub_base(&self, index: NodeIndex) -> Option<VertexHandle> {
        self.stub_bases.get(index.index()).copied().flatten()
    }

    /// The layer handle of the endpoint in delta node slot `index`: the base vertex's own
    /// handle for a stub, else the delta handle with `generation`.
    #[inline]
    fn endpoint(&self, index: NodeIndex, generation: std::num::NonZeroU32) -> LayeredVertex {
        match self.stub_base(index) {
            Some(base) => LayeredVertex::from_base(base),
            None => LayeredVertex::from_delta(VertexHandle::new(index, generation)),
        }
    }

    /// Edges the delta adds to the base count (added minus removed base edges).
    #[inline]
    pub(crate) fn edge_count_change(&self) -> isize {
        self.graph.edge_count() as isize - self.removed_edge_count as isize
    }

    /// The vertices the layer added, in slot order (stubs skipped).
    #[inline]
    pub(crate) fn vertices(&self) -> impl Iterator<Item = LayeredVertex> + '_ {
        self.graph
            .node_references()
            .filter(|(index, _)| self.stub_base(*index).is_none())
            .map(|(index, vertex)| LayeredVertex::from_delta(VertexHandle::new(index, vertex.3)))
    }

    /// The edges the layer added, in slot order.
    #[inline]
    pub(crate) fn edges(&self) -> impl Iterator<Item = LayeredEdge> + '_ {
        self.graph
            .edge_references()
            .map(|edge| LayeredEdge::from_delta(EdgeHandle::new(edge.id(), edge.weight().3.own)))
    }

    /// The added vertices carrying `label` and the base vertices that gained it (members
    /// through their stub, turned back into the base handle), in the label bucket's order.
    #[inline]
    pub(crate) fn vertices_by_label<'a>(
        &'a self,
        label: &str,
    ) -> impl Iterator<Item = LayeredVertex> + 'a {
        self.index
            .get_nodes_by_label(label)
            .into_iter()
            .flatten()
            .map(|member| match self.stub_base(member.index()) {
                Some(base) => LayeredVertex::from_base(base),
                None => LayeredVertex::from_delta(member),
            })
    }

    /// The added edges labelled `label`.
    #[inline]
    pub(crate) fn edges_by_label<'a>(
        &'a self,
        label: &str,
    ) -> impl Iterator<Item = LayeredEdge> + 'a {
        self.index
            .get_edges_by_label(label)
            .into_iter()
            .flatten()
            .map(LayeredEdge::from_delta)
    }

    /// An added vertex by id.
    #[inline]
    pub(crate) fn vertex_by_id(&self, id: &str) -> Option<LayeredVertex> {
        self.index
            .get_node(&self.graph, id)
            .map(LayeredVertex::from_delta)
    }

    /// An added edge by id.
    #[inline]
    pub(crate) fn edge_by_id(&self, id: &str) -> Option<LayeredEdge> {
        self.index
            .get_edge(&self.graph, id)
            .map(LayeredEdge::from_delta)
    }

    /// The delta node slot of a live added vertex (`node.is_delta()`), [`NodeIndex::end`]
    /// (never occupied, so it lists nothing) for a stale handle or one naming a stub.
    #[inline]
    pub(crate) fn delta_index(&self, node: LayeredVertex) -> NodeIndex {
        let local = node.to_delta();
        match self.graph.node_weight(local.index()) {
            Some(vertex)
                if vertex.3 == local.generation() && self.stub_base(local.index()).is_none() =>
            {
                local.index()
            }
            _ => NodeIndex::end(),
        }
    }

    /// The stub of the base vertex `node` (the caller asks only for a dirty one: a hash lookup),
    /// [`NodeIndex::end`] when it has none or `node` is stale (another generation).
    #[inline]
    pub(crate) fn stub_index(&self, node: VertexHandle) -> NodeIndex {
        match self.stubs.get(&node.index().slot()) {
            Some(&index) if self.stub_base(index) == Some(node) => index,
            _ => NodeIndex::end(),
        }
    }

    /// The added edges in list `direction` of delta node slot `index` (an added vertex or a
    /// stub, from [`Self::delta_index`] / [`Self::stub_index`]), newest first.
    #[inline]
    pub(crate) fn edges_at(
        &self,
        index: NodeIndex,
        direction: Direction,
    ) -> impl Iterator<Item = LayeredEdge> + '_ {
        self.graph
            .edges_directed(index, direction)
            .map(|e| LayeredEdge::from_delta(EdgeHandle::new(e.id(), e.weight().3.own)))
    }

    /// A delta vertex (`node.is_delta()`), `None` when stale or naming a stub.
    #[inline]
    pub(crate) fn vertex(&self, node: LayeredVertex) -> Option<VertexView<'_>> {
        let local = node.to_delta();
        let vertex = self
            .graph
            .node_weight(local.index())
            .filter(|vertex| vertex.3 == local.generation())?;
        if self.stub_base(local.index()).is_some() {
            return None;
        }
        Some(VertexView {
            id: &vertex.0,
            labels: self.labels.set(vertex.1),
            properties: &vertex.2,
        })
    }

    /// A delta edge (`edge.is_delta()`), `None` when stale.
    #[inline]
    pub(crate) fn edge(&self, edge: LayeredEdge) -> Option<EdgeView<'_>> {
        let local = edge.to_delta();
        let weight = self
            .graph
            .edge_weight(local.index())
            .filter(|weight| weight.3.own == local.generation())?;
        let (source, target) = self.graph.edge_endpoints(local.index())?;
        Some(EdgeView {
            id: &weight.0,
            label: self.labels.name_slice(weight.1),
            properties: weight.2.as_ref(),
            endpoints: (
                self.endpoint(source, weight.3.source),
                self.endpoint(target, weight.3.target),
            ),
        })
    }

    /// `true` iff the layer removed the base vertex in `slot` (its tombstone): one bit test.
    /// The liveness of a dirty base vertex whose handle is current (from the base's own scans
    /// and indexes).
    #[inline]
    pub(crate) fn is_removed_vertex(&self, slot: u32) -> bool {
        self.removed_vertices.contains(slot)
    }

    /// The edge counterpart of [`Self::is_removed_vertex`].
    #[inline]
    pub(crate) fn is_removed_edge(&self, slot: u32) -> bool {
        self.removed_edges.contains(slot)
    }

    /// A dirty base vertex as the layer sees it: `None` when the layer removed it (its
    /// tombstone, checked first) or `node` is stale, else its image when it has one (module
    /// docs), else the base's own state (a stub or a lost base edge changes its adjacency, not
    /// the vertex). The id is always the base's.
    #[inline]
    pub(crate) fn base_vertex<'a>(
        &'a self,
        base: &'a TraversalGraph,
        node: VertexHandle,
    ) -> Option<VertexView<'a>> {
        let slot = node.index().slot();
        if self.removed_vertices.contains(slot) {
            return None;
        }
        let vertex = base.vertex_weight(node)?;
        if self.imaged_vertices.contains(slot)
            && let Some(stub) = self.graph.node_weight(self.stub_index(node))
        {
            return Some(VertexView {
                id: &vertex.0,
                labels: self.labels.set(stub.1),
                properties: &stub.2,
            });
        }
        Some(VertexView {
            id: &vertex.0,
            labels: base.labels.set(vertex.1),
            properties: &vertex.2,
        })
    }

    /// A dirty base edge as the layer sees it: `None` when the layer removed it (checked
    /// first) or `edge` is stale, else its image's label and properties when it has one, else
    /// the base's own state. A live base edge never has a removed endpoint: removing a base
    /// vertex removes its edges first.
    #[inline]
    pub(crate) fn base_edge<'a>(
        &'a self,
        base: &'a TraversalGraph,
        edge: EdgeHandle,
    ) -> Option<EdgeView<'a>> {
        let slot = edge.index().slot();
        if self.removed_edges.contains(slot) {
            return None;
        }
        let weight = base.edge_weight(edge)?;
        let (from, to) = GraphStorage::edge_endpoints(base, edge).ok().flatten()?;
        let endpoints = (LayeredVertex::from_base(from), LayeredVertex::from_base(to));
        if self.imaged_edges.contains(slot)
            && let Some(image) = self.edge_images.get(&slot)
        {
            return Some(EdgeView {
                id: &weight.0,
                label: image.label.as_slice(),
                properties: image.properties.as_ref(),
                endpoints,
            });
        }
        Some(EdgeView {
            id: &weight.0,
            label: base.labels.name_slice(weight.1),
            properties: weight.2.as_ref(),
            endpoints,
        })
    }

    /// The base vertex slots that lost the base label `label` (their image lacks it): fetched
    /// once per by-label scan, then one bit test per dirty member.
    #[inline]
    pub(crate) fn lost_vertex_label(&self, label: &str) -> Option<&SlotBits> {
        if self.lost_vertex_labels.is_empty() {
            return None;
        }
        self.lost_vertex_labels.get(label)
    }

    /// The base vertex slots with an image (modified base vertices; the layer diff walks them).
    #[inline]
    pub(crate) fn imaged_vertices(&self) -> &SlotBits {
        &self.imaged_vertices
    }

    /// The base edge slots with an image (modified base edges).
    #[inline]
    pub(crate) fn imaged_edges(&self) -> &SlotBits {
        &self.imaged_edges
    }

    /// `true` iff the base edge in `slot` has an image: one bit test (read behind the dirty
    /// bit).
    #[inline]
    pub(crate) fn is_imaged_edge(&self, slot: u32) -> bool {
        self.imaged_edges.contains(slot)
    }

    /// The base edge slots whose label is no longer their base label.
    #[inline]
    pub(crate) fn relabeled_edges(&self) -> &SlotBits {
        &self.relabeled_edges
    }

    /// The base edges relabeled to `label`, in the order they gained it.
    #[inline]
    pub(crate) fn gained_edges<'a>(
        &'a self,
        label: &str,
    ) -> impl Iterator<Item = LayeredEdge> + 'a {
        let gained = if self.gained_edge_labels.is_empty() {
            None
        } else {
            self.gained_edge_labels.get(label)
        };
        gained
            .into_iter()
            .flatten()
            .map(|edge| LayeredEdge::from_base(*edge))
    }

    /// `true` while the layer holds an added vertex (stubs are not vertices).
    #[inline]
    pub(crate) fn has_added_vertices(&self) -> bool {
        self.graph.node_count() > self.stubs.len()
    }

    /// `true` while the layer holds an added edge.
    #[inline]
    pub(crate) fn has_added_edges(&self) -> bool {
        self.graph.edge_count() > 0
    }

    /// The tombstones of removed base vertices when there is at least one, else `None`: a scan
    /// fetches this once and tests one bit per base vertex only when it is `Some`. A removed
    /// base vertex is dirty too, so this alone decides the liveness of a current base handle.
    #[inline]
    pub(crate) fn removed_vertices(&self) -> Option<&SlotBits> {
        (self.removed_vertex_count > 0).then_some(&self.removed_vertices)
    }

    /// The edge counterpart of [`Self::removed_vertices`].
    #[inline]
    pub(crate) fn removed_edges(&self) -> Option<&SlotBits> {
        (self.removed_edge_count > 0).then_some(&self.removed_edges)
    }

    /// `true` once any base edge was relabeled (stays `true` after it returns to its base
    /// label: a conservative answer, it only sends a by-label scan down the filtered path).
    #[inline]
    pub(crate) fn may_have_relabeled_edges(&self) -> bool {
        self.relabeled_edges.any()
    }

    /// `true` while the layer has added, removed or modified (imaged) any vertex: a property
    /// index of the base may then miss or wrongly hold vertices (index deltas: stage 4).
    #[inline]
    pub(crate) fn vertices_touched(&self) -> bool {
        self.graph.node_count() > self.stubs.len()
            || self.removed_vertex_count > 0
            || self.vertex_image_count > 0
    }

    /// The edge counterpart of [`Self::vertices_touched`].
    #[inline]
    pub(crate) fn edges_touched(&self) -> bool {
        self.graph.edge_count() > 0 || self.removed_edge_count > 0 || !self.edge_images.is_empty()
    }

    // --------------------------------------------------------------------------------------
    // Writes: store what the write rules returned (no checks here), keep the indexes and the
    // layer's statistics in step, exactly as `TraversalGraph`'s raw helpers (`graph/undo.rs`).
    // --------------------------------------------------------------------------------------

    /// Stores a new vertex and indexes it. The id must be free in the whole layer.
    pub(crate) fn insert_vertex(
        &mut self,
        id: CompactString,
        labels: Vec<CompactString>,
        mut properties: PropertyMap,
        statistics: &mut GraphStatistics,
    ) -> Result<LayeredVertex, GraphError> {
        if self.graph.vacant_nodes() == 0 && self.graph.node_slots() >= MAX_BASE_SLOTS {
            return Err(delta_slots_exhausted());
        }
        self.shapes.adopt(&mut properties);
        let label_set = self.labels.intern_set(&labels);
        // The slot is known only after `add_node`: a placeholder generation, patched right
        // after (as `TraversalGraph::insert_vertex_raw` does).
        let index = self
            .graph
            .add_node((id, label_set, properties, FIRST_GENERATION));
        let generation = self.node_generations.current(index.index());
        let handle = VertexHandle::new(index, generation);
        if let Some(vertex) = self.graph.node_weight_mut(index) {
            vertex.3 = generation;
            self.index.insert_node(&vertex.0, handle);
        }
        statistics.vertex_added(&labels);
        for label in labels {
            self.index.insert_node_label(label, handle);
        }
        Ok(LayeredVertex::from_delta(handle))
    }

    /// The stub of the live base vertex `node`, created (empty, invisible) when it has none.
    /// The caller sets the vertex's dirty bit.
    pub(crate) fn stub_for(&mut self, node: VertexHandle) -> Result<NodeIndex, GraphError> {
        if let Some(&index) = self.stubs.get(&node.index().slot()) {
            return Ok(index);
        }
        if self.graph.vacant_nodes() == 0 && self.graph.node_slots() >= MAX_BASE_SLOTS {
            return Err(delta_slots_exhausted());
        }
        let index = self.graph.add_node((
            CompactString::default(),
            LabelSetId::EMPTY,
            PropertyMap::default(),
            FIRST_GENERATION,
        ));
        let generation = self.node_generations.current(index.index());
        if let Some(stub) = self.graph.node_weight_mut(index) {
            stub.3 = generation;
        }
        if self.stub_bases.len() <= index.index() {
            self.stub_bases.resize(index.index() + 1, None);
        }
        self.stub_bases[index.index()] = Some(node);
        self.stubs.insert(node.index().slot(), index);
        Ok(index)
    }

    /// Stores a new edge and indexes it. `from` and `to` are delta node slots with the
    /// generation the endpoint's handle carries: a live added vertex ([`Self::delta_index`])
    /// or the stub of a live base vertex ([`Self::stub_for`]) with the base vertex's generation.
    /// The id must be free in the whole layer.
    pub(crate) fn insert_edge(
        &mut self,
        id: CompactString,
        label: Option<CompactString>,
        properties: PropertyMap,
        from: VertexHandle,
        to: VertexHandle,
        statistics: &mut GraphStatistics,
    ) -> Result<LayeredEdge, GraphError> {
        if self.graph.vacant_edges() == 0 && self.graph.edge_slots() >= MAX_BASE_SLOTS {
            return Err(delta_slots_exhausted());
        }
        let mut properties = Some(properties);
        self.shapes.adopt_optional(&mut properties);
        let label_id = self.labels.intern_name(label.as_deref());
        let mut generations = EdgeGenerations {
            own: FIRST_GENERATION,
            source: from.generation(),
            target: to.generation(),
        };
        let Some(index) = self.graph.add_edge(
            from.index(),
            to.index(),
            (id, label_id, properties, generations),
        ) else {
            // The caller resolved both endpoints through the write rules.
            return Err(GraphError::vertex_removed());
        };
        generations.own = self.edge_generations.current(index.index());
        let handle = EdgeHandle::new(index, generations.own);
        if let Some(edge) = self.graph.edge_weight_mut(index) {
            edge.3 = generations;
            self.index.insert_edge(&edge.0, handle);
        }
        statistics.edge_added(label.as_deref());
        if let Some(label) = label {
            self.index.insert_edge_label(label, handle);
        }
        Ok(LayeredEdge::from_delta(handle))
    }

    /// Removes a live delta edge with its index entries and retires its slot generation;
    /// nothing for a stale handle. With `capture` (inside a unit) it returns what an undo needs
    /// to store the edge again.
    pub(crate) fn remove_edge(
        &mut self,
        edge: LayeredEdge,
        statistics: &mut GraphStatistics,
        capture: bool,
    ) -> Option<RemovedEdge> {
        let local = edge.to_delta();
        if self
            .graph
            .edge_weight(local.index())
            .is_none_or(|weight| weight.3.own != local.generation())
        {
            return None;
        }
        let ends = match (capture, self.graph.edge_endpoints(local.index())) {
            (true, Some((source, target))) => Some((self.end_of(source), self.end_of(target))),
            _ => None,
        };
        let (id, label, properties, _) = self.graph.remove_edge(local.index())?;
        let label = self.labels.name(label).cloned();
        statistics.edge_removed(label.as_deref());
        if let Some(label) = &label {
            self.index.remove_edge_label(label, local);
        }
        self.index.remove_edge(&id, local);
        self.edge_generations.bump(local.index().index());
        let (from, to) = ends?;
        Some(RemovedEdge {
            id,
            label,
            properties,
            from,
            to,
        })
    }

    /// How an undo names the endpoint in delta node slot `index` (see [`End`]).
    fn end_of(&self, index: NodeIndex) -> End {
        match self.stub_base(index) {
            Some(base) => End::Base(base),
            None => End::Delta(
                self.graph
                    .node_weight(index)
                    .map(|vertex| vertex.0.clone())
                    .unwrap_or_default(),
            ),
        }
    }

    /// The delta node slot (with the endpoint handle's generation) an undo links a restored
    /// edge to: the stub of a base vertex (created again when its drop took it along), or the
    /// added vertex with that id.
    fn resolve_end(&mut self, end: &End) -> Result<VertexHandle, GraphError> {
        match end {
            End::Base(base) => {
                let stub = self.stub_for(*base)?;
                Ok(VertexHandle::new(stub, base.generation()))
            }
            End::Delta(id) => self
                .vertex_by_id(id)
                .map(|node| node.to_delta())
                .ok_or_else(GraphError::vertex_removed),
        }
    }

    /// Stores an added edge an undone drop took out again (a new slot and generation, at the
    /// front of its endpoints' lists). The caller keeps the dirty bits of base endpoints set.
    pub(crate) fn restore_edge(
        &mut self,
        removed: RemovedEdge,
        statistics: &mut GraphStatistics,
    ) -> Result<(), GraphError> {
        let from = self.resolve_end(&removed.from)?;
        let to = self.resolve_end(&removed.to)?;
        self.insert_edge(
            removed.id,
            removed.label,
            removed.properties.unwrap_or_default(),
            from,
            to,
            statistics,
        )?;
        Ok(())
    }

    /// Stores an added vertex an undone drop took out again (a new slot and generation; its
    /// edges come back through [`Self::restore_edge`]).
    pub(crate) fn restore_vertex(
        &mut self,
        removed: RemovedVertex,
        statistics: &mut GraphStatistics,
    ) -> Result<(), GraphError> {
        self.insert_vertex(removed.id, removed.labels, removed.properties, statistics)?;
        Ok(())
    }

    /// Removes a live delta vertex, its incident edges (also those to base vertices: they leave
    /// the stubs' lists, the stubs stay) and every index entry, and retires the slot
    /// generations; nothing for a stale handle. Returns the removed vertex; with `removed_edges`
    /// (inside a unit) the removed edges are pushed there, in removal order.
    pub(crate) fn remove_vertex(
        &mut self,
        node: LayeredVertex,
        statistics: &mut GraphStatistics,
        mut removed_edges: Option<&mut Vec<RemovedEdge>>,
    ) -> Option<RemovedVertex> {
        let index = self.delta_index(node);
        if index == NodeIndex::end() {
            return None;
        }
        // Each incident edge once: a self-loop is taken from the outgoing side only.
        let mut incident: Vec<LayeredEdge> = Vec::new();
        for direction in [Direction::Outgoing, Direction::Incoming] {
            for edge in self.graph.edges_directed(index, direction) {
                if direction == Direction::Incoming && edge.source() == index {
                    continue;
                }
                incident.push(LayeredEdge::from_delta(EdgeHandle::new(
                    edge.id(),
                    edge.weight().3.own,
                )));
            }
        }
        for edge in incident {
            let removed = self.remove_edge(edge, statistics, removed_edges.is_some());
            if let (Some(sink), Some(removed)) = (removed_edges.as_deref_mut(), removed) {
                sink.push(removed);
            }
        }
        let local = node.to_delta();
        let (id, labels, properties, _) = self.graph.remove_node(index)?;
        let labels = self.labels.set(labels).to_vec();
        statistics.vertex_removed(&labels);
        self.index.remove_node(&id, local);
        for label in &labels {
            self.index.remove_node_label(label, local);
        }
        self.node_generations.bump(index.index());
        Some(RemovedVertex {
            id,
            labels,
            properties,
        })
    }

    /// Tombstones the live base edge `edge` (the caller checked it through the write rules and
    /// sets the dirty bits of the edge and of both endpoints) and counts it out of the
    /// statistics as `TraversalGraph`'s raw remove does. Nothing (`None`) for an edge already
    /// removed or stale. Returns what an undo puts back: the image the edge had and its
    /// position among the base edges relabeled to the image's label.
    pub(crate) fn remove_base_edge(
        &mut self,
        base: &TraversalGraph,
        edge: EdgeHandle,
        statistics: &mut GraphStatistics,
    ) -> Option<(Option<EdgeImage>, Option<usize>)> {
        let slot = edge.index().slot();
        if self.removed_edges.contains(slot) {
            return None;
        }
        let weight = base.edge_weight(edge)?;
        // The label it has now: its image's, when a write changed it.
        let image = if self.imaged_edges.contains(slot) {
            self.imaged_edges.clear(slot);
            self.edge_images.remove(&slot)
        } else {
            None
        };
        let mut position = None;
        match &image {
            Some(image) => {
                statistics.edge_removed(image.label.as_deref());
                if self.relabeled_edges.contains(slot) {
                    self.relabeled_edges.clear(slot);
                    if let Some(label) = &image.label {
                        position = self.remove_gained_edge(label, edge);
                    }
                }
            }
            None => statistics.edge_removed(base.labels.name(weight.1).map(|l| l.as_str())),
        }
        self.removed_edges.set_growing(slot);
        self.removed_edge_count += 1;
        Some((image, position))
    }

    /// Undoes [`Self::remove_base_edge`]: clears the tombstone, counts the edge back into the
    /// statistics and puts its image back (with its relabeled bit and its old position among
    /// the base edges relabeled to its label). The dirty bits stay set.
    pub(crate) fn restore_base_edge(
        &mut self,
        base: &TraversalGraph,
        edge: EdgeHandle,
        image: Option<EdgeImage>,
        position: Option<usize>,
        statistics: &mut GraphStatistics,
    ) {
        let slot = edge.index().slot();
        if !self.removed_edges.contains(slot) {
            return;
        }
        self.removed_edges.clear(slot);
        self.removed_edge_count = self.removed_edge_count.saturating_sub(1);
        let base_label = base
            .edge_weight(edge)
            .and_then(|weight| base.labels.name(weight.1))
            .cloned();
        match image {
            None => statistics.edge_added(base_label.as_deref()),
            Some(mut image) => {
                statistics.edge_added(image.label.as_deref());
                if image.label != base_label {
                    self.relabeled_edges.set_growing(slot);
                    if let Some(label) = &image.label {
                        self.insert_gained_edge(label.clone(), edge, position);
                    }
                }
                self.shapes.adopt_optional(&mut image.properties);
                self.edge_images.insert(slot, image);
                self.imaged_edges.set_growing(slot);
            }
        }
    }

    /// Tombstones the live base vertex `node` and removes its stub, if it has one (the caller
    /// removed every incident edge first, base and added, and sets the vertex's dirty bit),
    /// with its image and the label memberships the image had; counts it out of the
    /// statistics, with the labels it has now, as `TraversalGraph`'s raw remove does. Nothing
    /// (`None`) for a vertex already removed or stale; else `Some` of the image it had, which
    /// an undo puts back.
    pub(crate) fn remove_base_vertex(
        &mut self,
        base: &TraversalGraph,
        node: VertexHandle,
        statistics: &mut GraphStatistics,
    ) -> Option<Option<VertexCopy>> {
        let slot = node.index().slot();
        if self.removed_vertices.contains(slot) {
            return None;
        }
        let vertex = base.vertex_weight(node)?;
        let base_labels = base.labels.set(vertex.1);
        let stub = self.stub_index(node);
        let imaged = self.imaged_vertices.contains(slot);
        let mut image_labels = None;
        match self.graph.node_weight(stub) {
            Some(weight) if imaged => {
                let current = self.labels.set(weight.1).to_vec();
                let member = VertexHandle::new(stub, weight.3);
                statistics.vertex_removed(&current);
                for label in &current {
                    if !base_labels.contains(label) {
                        self.index.remove_node_label(label, member);
                    }
                }
                for label in base_labels.iter().filter(|l| !current.contains(l)) {
                    if let Some(lost) = self.lost_vertex_labels.get_mut(label.as_str()) {
                        lost.clear(slot);
                    }
                }
                self.imaged_vertices.clear(slot);
                self.vertex_image_count -= 1;
                image_labels = Some(current);
            }
            _ => statistics.vertex_removed(base_labels),
        }
        self.removed_vertices.set_growing(slot);
        self.removed_vertex_count += 1;
        let mut copy = None;
        if stub != NodeIndex::end() {
            // Empty by now; `remove_node` would unlink a leftover edge without its index
            // entries, so the caller's order (edges first) is what keeps the indexes exact.
            let removed = self.graph.remove_node(stub);
            self.node_generations.bump(stub.index());
            self.stubs.remove(&slot);
            if let Some(entry) = self.stub_bases.get_mut(stub.index()) {
                *entry = None;
            }
            if let (Some(labels), Some((_, _, properties, _))) = (image_labels, removed) {
                copy = Some(VertexCopy { labels, properties });
            }
        }
        Some(copy)
    }

    /// Undoes [`Self::remove_base_vertex`]: clears the tombstone, counts the vertex back into
    /// the statistics and puts its image back into a new stub, with its label memberships and
    /// lost-label bits. The dirty bit stays set; the added edges the drop took along come back
    /// through [`Self::restore_edge`].
    pub(crate) fn restore_base_vertex(
        &mut self,
        base: &TraversalGraph,
        node: VertexHandle,
        image: Option<VertexCopy>,
        statistics: &mut GraphStatistics,
    ) -> Result<(), GraphError> {
        let slot = node.index().slot();
        if !self.removed_vertices.contains(slot) {
            return Ok(());
        }
        let Some(vertex) = base.vertex_weight(node) else {
            return Err(GraphError::vertex_removed());
        };
        self.removed_vertices.clear(slot);
        self.removed_vertex_count = self.removed_vertex_count.saturating_sub(1);
        let base_labels = base.labels.set(vertex.1);
        let Some(VertexCopy {
            labels,
            mut properties,
        }) = image
        else {
            statistics.vertex_added(base_labels);
            return Ok(());
        };
        statistics.vertex_added(&labels);
        let stub = self.stub_for(node)?;
        self.shapes.adopt(&mut properties);
        let label_set = self.labels.intern_set(&labels);
        let Some(weight) = self.graph.node_weight_mut(stub) else {
            return Err(GraphError::vertex_removed());
        };
        weight.1 = label_set;
        weight.2 = properties;
        let member = VertexHandle::new(stub, weight.3);
        for label in labels.iter().filter(|l| !base_labels.contains(l)) {
            self.index.insert_node_label(label.clone(), member);
        }
        for label in base_labels.iter().filter(|l| !labels.contains(l)) {
            self.lost_vertex_labels
                .entry(label.clone())
                .or_default()
                .set_growing(slot);
        }
        self.imaged_vertices.set_growing(slot);
        self.vertex_image_count += 1;
        Ok(())
    }

    // --------------------------------------------------------------------------------------
    // Images of base elements (module docs): the caller ran the write rule, found that the
    // write changes something (`PropertyChange::is_noop`, a label rule's `Some`) and set the
    // element's dirty bit; each writer copies the element up first when it has no image and
    // returns whether it HAD one (then the caller asks `settle_*` for a return to clean).
    // --------------------------------------------------------------------------------------

    /// The stub of the live base vertex `node` holding its image, copied up now when it has
    /// none (labels and logical property values), and whether the image existed before.
    fn vertex_image(
        &mut self,
        base: &TraversalGraph,
        node: VertexHandle,
    ) -> Result<(NodeIndex, bool), GraphError> {
        let slot = node.index().slot();
        let stub = self.stub_for(node)?;
        if self.imaged_vertices.contains(slot) {
            return Ok((stub, true));
        }
        let Some(vertex) = base.vertex_weight(node) else {
            return Err(GraphError::vertex_removed());
        };
        let label_set = self.labels.intern_set(base.labels.set(vertex.1));
        let mut properties = logical_element(&vertex.2);
        self.shapes.adopt(&mut properties);
        if let Some(weight) = self.graph.node_weight_mut(stub) {
            weight.1 = label_set;
            weight.2 = properties;
        }
        self.imaged_vertices.set_growing(slot);
        self.vertex_image_count += 1;
        Ok((stub, false))
    }

    /// Applies one property change to the image of the live base vertex `node`; returns
    /// whether the image existed before and the key's old state.
    pub(crate) fn change_base_vertex_property(
        &mut self,
        base: &TraversalGraph,
        node: VertexHandle,
        change: PropertyChange,
    ) -> Result<(bool, OldProperty), GraphError> {
        let (stub, existed) = self.vertex_image(base, node)?;
        let old = match self.graph.node_weight_mut(stub) {
            Some(weight) => change.apply(&mut self.shapes, &mut weight.2),
            None => None,
        };
        Ok((existed, old))
    }

    /// Undoes one property change of the image of the base vertex `node`: puts `key` back to
    /// `old` (copying the vertex up again when a return to clean dropped the image since: the
    /// image then equalled the base vertex, which the write left with the same labels).
    pub(crate) fn restore_base_vertex_property(
        &mut self,
        base: &TraversalGraph,
        node: VertexHandle,
        key: &str,
        old: OldProperty,
    ) -> Result<(), GraphError> {
        let (stub, _) = self.vertex_image(base, node)?;
        if let Some(weight) = self.graph.node_weight_mut(stub) {
            restore_stored_property(&mut self.shapes, &mut weight.2, key, old);
        }
        Ok(())
    }

    /// Drops the image of the base vertex `node` whatever it holds (an undo of the write that
    /// copied it up: its labels are the base's again, so no label lookup changes). The stub
    /// and the dirty bit stay.
    pub(crate) fn clear_vertex_image(&mut self, node: VertexHandle) {
        let slot = node.index().slot();
        if !self.imaged_vertices.contains(slot) {
            return;
        }
        let stub = self.stub_index(node);
        if let Some(image) = self.graph.node_weight_mut(stub) {
            image.1 = LabelSetId::EMPTY;
            image.2 = PropertyMap::default();
        }
        self.imaged_vertices.clear(slot);
        self.vertex_image_count -= 1;
    }

    /// Gives the image of the live base vertex `node` the label list `new_labels` (from the
    /// write rules) and keeps the label lookups (module docs) and the statistics in step;
    /// returns whether the image existed before and the old label list.
    pub(crate) fn set_base_vertex_labels(
        &mut self,
        base: &TraversalGraph,
        node: VertexHandle,
        new_labels: Vec<CompactString>,
        statistics: &mut GraphStatistics,
    ) -> Result<(bool, Vec<CompactString>), GraphError> {
        let (stub, existed) = self.vertex_image(base, node)?;
        let Some(vertex) = base.vertex_weight(node) else {
            return Err(GraphError::vertex_removed());
        };
        let base_labels = base.labels.set(vertex.1);
        let slot = node.index().slot();
        let new_set = self.labels.intern_set(&new_labels);
        let Some(weight) = self.graph.node_weight_mut(stub) else {
            return Err(GraphError::vertex_removed());
        };
        let member = VertexHandle::new(stub, weight.3);
        let old = self
            .labels
            .set(std::mem::replace(&mut weight.1, new_set))
            .to_vec();
        statistics.vertex_relabeled(&old, &new_labels);
        for label in old.iter().filter(|l| !new_labels.contains(l)) {
            if base_labels.contains(label) {
                self.lost_vertex_labels
                    .entry(label.clone())
                    .or_default()
                    .set_growing(slot);
            } else {
                self.index.remove_node_label(label, member);
            }
        }
        for label in new_labels.into_iter().filter(|l| !old.contains(l)) {
            if base_labels.contains(&label) {
                if let Some(lost) = self.lost_vertex_labels.get_mut(label.as_str()) {
                    lost.clear(slot);
                }
            } else {
                self.index.insert_node_label(label, member);
            }
        }
        Ok((existed, old))
    }

    /// Drops the image of the base vertex `node` when it is identical to the base vertex again
    /// (return to clean, module docs). Its label memberships need no change: with the base's
    /// labels it lost and gained none. The dirty bit stays.
    pub(crate) fn settle_base_vertex(&mut self, base: &TraversalGraph, node: VertexHandle) {
        let slot = node.index().slot();
        if !self.imaged_vertices.contains(slot) {
            return;
        }
        let (Some(vertex), stub) = (base.vertex_weight(node), self.stub_index(node)) else {
            return;
        };
        let Some(image) = self.graph.node_weight(stub) else {
            return;
        };
        if self.labels.set(image.1) != base.labels.set(vertex.1)
            || !maps_identical(&image.2, &vertex.2)
        {
            return;
        }
        self.clear_vertex_image(node);
    }

    /// Copies the live base edge `edge` up when it has no image yet (label and logical
    /// property values); `true` when it had one.
    fn edge_image(&mut self, base: &TraversalGraph, edge: EdgeHandle) -> Result<bool, GraphError> {
        let slot = edge.index().slot();
        if self.imaged_edges.contains(slot) {
            return Ok(true);
        }
        let Some(weight) = base.edge_weight(edge) else {
            return Err(GraphError::edge_removed());
        };
        let mut properties = weight.2.as_ref().map(logical_element);
        self.shapes.adopt_optional(&mut properties);
        self.edge_images.insert(
            slot,
            EdgeImage {
                label: base.labels.name(weight.1).cloned(),
                properties,
            },
        );
        self.imaged_edges.set_growing(slot);
        Ok(false)
    }

    /// Applies one property change to the image of the live base edge `edge` (a set gives an
    /// edge without a map one, as `TraversalGraph` does); returns whether the image existed
    /// before, the key's old state and whether the image had no map before.
    pub(crate) fn change_base_edge_property(
        &mut self,
        base: &TraversalGraph,
        edge: EdgeHandle,
        change: PropertyChange,
    ) -> Result<(bool, OldProperty, bool), GraphError> {
        let existed = self.edge_image(base, edge)?;
        let Some(image) = self.edge_images.get_mut(&edge.index().slot()) else {
            return Ok((existed, None, false));
        };
        let map_was_none = image.properties.is_none();
        let old = match change {
            PropertyChange::Set(..) => {
                let map = image.properties.get_or_insert_with(PropertyMap::default);
                change.apply(&mut self.shapes, map)
            }
            PropertyChange::Drop(_) => match image.properties.as_mut() {
                Some(map) => change.apply(&mut self.shapes, map),
                None => None,
            },
        };
        Ok((existed, old, map_was_none))
    }

    /// The edge counterpart of [`Self::restore_base_vertex_property`]; `map_was_none` puts
    /// back an image without a map.
    pub(crate) fn restore_base_edge_property(
        &mut self,
        base: &TraversalGraph,
        edge: EdgeHandle,
        key: &str,
        old: OldProperty,
        map_was_none: bool,
    ) -> Result<(), GraphError> {
        self.edge_image(base, edge)?;
        if let Some(image) = self.edge_images.get_mut(&edge.index().slot()) {
            if map_was_none {
                image.properties = None;
            } else {
                let map = image.properties.get_or_insert_with(PropertyMap::default);
                restore_stored_property(&mut self.shapes, map, key, old);
            }
        }
        Ok(())
    }

    /// The edge counterpart of [`Self::clear_vertex_image`]: its label is the base's again.
    pub(crate) fn clear_edge_image(&mut self, edge: EdgeHandle) {
        let slot = edge.index().slot();
        if self.imaged_edges.contains(slot) {
            self.edge_images.remove(&slot);
            self.imaged_edges.clear(slot);
            self.relabeled_edges.clear(slot);
        }
    }

    /// Relabels the image of the live base edge `edge` to `label` (`None`: unlabeled; the
    /// write rules passed and the label differs, or an undo puts the old label back), keeping
    /// the label lookups (module docs) and the statistics in step. A relabeled edge joins the
    /// list of its new label at its end, or at `position` (an undo: the place it had there).
    /// Returns whether the image existed before, the old label and the place the edge had in
    /// the list of the old label (what an undo hands back as `position`).
    pub(crate) fn set_base_edge_label(
        &mut self,
        base: &TraversalGraph,
        edge: EdgeHandle,
        label: Option<CompactString>,
        position: Option<usize>,
        statistics: &mut GraphStatistics,
    ) -> Result<(bool, Option<CompactString>, Option<usize>), GraphError> {
        let existed = self.edge_image(base, edge)?;
        let slot = edge.index().slot();
        let base_label = base
            .edge_weight(edge)
            .and_then(|weight| base.labels.name(weight.1))
            .cloned();
        let Some(image) = self.edge_images.get_mut(&slot) else {
            return Err(GraphError::edge_removed());
        };
        let old = std::mem::replace(&mut image.label, label.clone());
        statistics.edge_relabeled(old.as_deref(), label.as_deref());
        let mut old_position = None;
        if old != base_label
            && let Some(old) = &old
        {
            old_position = self.remove_gained_edge(old, edge);
        }
        if label == base_label {
            self.relabeled_edges.clear(slot);
        } else {
            self.relabeled_edges.set_growing(slot);
            if let Some(label) = label {
                self.insert_gained_edge(label, edge, position);
            }
        }
        Ok((existed, old, old_position))
    }

    /// Removes `edge` from the base edges relabeled to `label` (the last one takes its place,
    /// as in a label bucket); returns the place it had.
    fn remove_gained_edge(&mut self, label: &str, edge: EdgeHandle) -> Option<usize> {
        let members = self.gained_edge_labels.get_mut(label)?;
        let position = members.iter().position(|member| *member == edge);
        if let Some(position) = position {
            members.swap_remove(position);
        }
        if members.is_empty() {
            self.gained_edge_labels.remove(label);
        }
        position
    }

    /// Adds `edge` to the base edges relabeled to `label`: at the end, or back at `position`
    /// (the exact inverse of [`Self::remove_gained_edge`]: the member that took its place moves
    /// back to the end).
    fn insert_gained_edge(
        &mut self,
        label: CompactString,
        edge: EdgeHandle,
        position: Option<usize>,
    ) {
        let members = self.gained_edge_labels.entry(label).or_default();
        members.push(edge);
        let last = members.len() - 1;
        if let Some(position) = position
            && position < last
        {
            members.swap(position, last);
        }
    }

    /// The edge counterpart of [`Self::settle_base_vertex`]: the label and the map identical
    /// to the base edge's (a map the base edge lacks counts as different from an empty one, as
    /// the two read differently through `edge_properties`).
    pub(crate) fn settle_base_edge(&mut self, base: &TraversalGraph, edge: EdgeHandle) {
        let slot = edge.index().slot();
        if !self.imaged_edges.contains(slot) {
            return;
        }
        let (Some(weight), Some(image)) = (base.edge_weight(edge), self.edge_images.get(&slot))
        else {
            return;
        };
        let same_properties = match (&image.properties, &weight.2) {
            (None, None) => true,
            (Some(image), Some(base)) => maps_identical(image, base),
            _ => false,
        };
        if same_properties && image.label.as_ref() == base.labels.name(weight.1) {
            self.clear_edge_image(edge);
        }
    }

    /// Replaces the label set of a live delta vertex with `new_labels` (from the write rules);
    /// returns the old label list (empty for a stale handle).
    pub(crate) fn set_vertex_labels(
        &mut self,
        node: LayeredVertex,
        new_labels: Vec<CompactString>,
        statistics: &mut GraphStatistics,
    ) -> Vec<CompactString> {
        let local = node.to_delta();
        let Some(vertex) = self
            .graph
            .node_weight_mut(local.index())
            .filter(|vertex| vertex.3 == local.generation())
        else {
            return Vec::new();
        };
        let new_set = self.labels.intern_set(&new_labels);
        let old = self
            .labels
            .set(std::mem::replace(&mut vertex.1, new_set))
            .to_vec();
        statistics.vertex_relabeled(&old, &new_labels);
        for label in old.iter().filter(|l| !new_labels.contains(l)) {
            self.index.remove_node_label(label, local);
        }
        for label in new_labels.into_iter().filter(|l| !old.contains(l)) {
            self.index.insert_node_label(label, local);
        }
        old
    }

    /// Relabels a live delta edge to `label` (`None`: unlabeled, an undo only; the write rules
    /// passed); returns the old label (`None` also for a stale handle).
    pub(crate) fn set_edge_label(
        &mut self,
        edge: LayeredEdge,
        label: Option<CompactString>,
        statistics: &mut GraphStatistics,
    ) -> Option<CompactString> {
        let local = edge.to_delta();
        let weight = self
            .graph
            .edge_weight_mut(local.index())
            .filter(|weight| weight.3.own == local.generation())?;
        let new_id = self.labels.intern_name(label.as_deref());
        let old = self
            .labels
            .name(std::mem::replace(&mut weight.1, new_id))
            .cloned();
        statistics.edge_relabeled(old.as_deref(), label.as_deref());
        if let Some(old) = &old {
            self.index.remove_edge_label(old, local);
        }
        if let Some(label) = label {
            self.index.insert_edge_label(label, local);
        }
        old
    }

    /// Sets one property of a live delta vertex to the value the write rules returned; returns
    /// the key's old state.
    pub(crate) fn set_vertex_property(
        &mut self,
        node: LayeredVertex,
        key: CompactString,
        value: ElementProperty,
    ) -> OldProperty {
        let vertex = live_vertex_mut(&mut self.graph, node)?;
        let (position, old) = self.shapes.insert_full(&mut vertex.2, key, value);
        old.map(|old| (position, old))
    }

    /// Sets one property of a live delta edge to the value the write rules returned; returns
    /// the key's old state and whether the edge had no map before.
    pub(crate) fn set_edge_property(
        &mut self,
        edge: LayeredEdge,
        key: CompactString,
        value: ElementProperty,
    ) -> (OldProperty, bool) {
        let local = edge.to_delta();
        let Some(weight) = self
            .graph
            .edge_weight_mut(local.index())
            .filter(|weight| weight.3.own == local.generation())
        else {
            return (None, false);
        };
        let map_was_none = weight.2.is_none();
        let map = weight.2.get_or_insert_with(PropertyMap::default);
        let (position, old) = self.shapes.insert_full(map, key, value);
        (old.map(|old| (position, old)), map_was_none)
    }

    /// Removes one property of a live delta vertex (absent: nothing); returns its old state.
    pub(crate) fn drop_vertex_property(&mut self, node: LayeredVertex, key: &str) -> OldProperty {
        let vertex = live_vertex_mut(&mut self.graph, node)?;
        self.shapes
            .shift_remove_full(&mut vertex.2, key)
            .map(|(position, _, old)| (position, old))
    }

    /// Removes one property of a live delta edge (absent: nothing); returns its old state.
    pub(crate) fn drop_edge_property(&mut self, edge: LayeredEdge, key: &str) -> OldProperty {
        let local = edge.to_delta();
        let weight = self
            .graph
            .edge_weight_mut(local.index())
            .filter(|weight| weight.3.own == local.generation())?;
        let map = weight.2.as_mut()?;
        self.shapes
            .shift_remove_full(map, key)
            .map(|(position, _, old)| (position, old))
    }

    /// Undoes one property write of the added vertex `id`: `key` back to `old`.
    pub(crate) fn restore_vertex_property(&mut self, id: &str, key: &str, old: OldProperty) {
        let Some(node) = self.vertex_by_id(id) else {
            tracing::error!(%id, "layer undo: added vertex of a property write not found");
            return;
        };
        if let Some(vertex) = live_vertex_mut(&mut self.graph, node) {
            restore_stored_property(&mut self.shapes, &mut vertex.2, key, old);
        }
    }

    /// Undoes one property write of the added edge `id`; `map_was_none` puts back an edge
    /// without a map.
    pub(crate) fn restore_edge_property(
        &mut self,
        id: &str,
        key: &str,
        old: OldProperty,
        map_was_none: bool,
    ) {
        let Some(edge) = self.edge_by_id(id) else {
            tracing::error!(%id, "layer undo: added edge of a property write not found");
            return;
        };
        let local = edge.to_delta();
        if let Some(weight) = self
            .graph
            .edge_weight_mut(local.index())
            .filter(|weight| weight.3.own == local.generation())
        {
            if map_was_none {
                weight.2 = None;
            } else {
                let map = weight.2.get_or_insert_with(PropertyMap::default);
                restore_stored_property(&mut self.shapes, map, key, old);
            }
        }
    }

    /// What the delta holds in memory, sorted into [`MemoryStats`]' buckets
    /// (`LayeredGraph::memory_usage`, the same arithmetic as `TraversalGraph`'s
    /// `graph::memory::compute`):
    /// - `structure_bytes`: the delta topology's slots (capacity, so vacant and spare slots
    ///   count; stubs are slots too), the added elements' ids, the label table, the slot
    ///   generations, the stub maps (`stubs`, `stub_bases`) and the bitsets read behind the
    ///   dirty bits (tombstones, imaged and relabeled slots);
    /// - `index_bytes`: the delta's id and label indexes plus the label structures of modified
    ///   base elements (`lost_vertex_labels`, `gained_edge_labels`);
    /// - `property_bytes`: the property maps of the added elements and of the vertex images
    ///   (in their stubs), the edge images (with their map's table entry), and the shape table
    ///   (`shape_bytes`).
    pub(crate) fn memory(&self) -> MemoryStats {
        use crate::value::compact_string_heap_bytes as string_heap;
        use crate::value::estimated_element_heap_bytes as map_heap;
        use crate::value::property_map::hash_table_heap_bytes as table;
        use std::mem::size_of;

        let (node_cap, edge_cap) = self.graph.capacity();
        let mut structure_bytes = node_cap as u64 * GraphType::node_slot_bytes() as u64
            + edge_cap as u64 * GraphType::edge_slot_bytes() as u64
            + self.labels.heap_bytes()
            + self.node_generations.heap_bytes()
            + self.edge_generations.heap_bytes()
            + table(self.stubs.capacity(), size_of::<(u32, NodeIndex)>())
            + self.stub_bases.capacity() as u64 * size_of::<Option<VertexHandle>>() as u64
            + self.removed_vertices.heap_bytes()
            + self.removed_edges.heap_bytes()
            + self.imaged_vertices.heap_bytes()
            + self.imaged_edges.heap_bytes()
            + self.relabeled_edges.heap_bytes();
        let mut property_bytes = 0u64;
        for (id, _, properties, _) in self.graph.node_weights() {
            structure_bytes += string_heap(id);
            property_bytes += map_heap(properties);
        }
        for (id, _, properties, _) in self.graph.edge_weights() {
            structure_bytes += string_heap(id);
            property_bytes += properties.as_ref().map_or(0, map_heap);
        }
        property_bytes += table(self.edge_images.capacity(), size_of::<(u32, EdgeImage)>());
        for image in self.edge_images.values() {
            property_bytes += image.label.as_ref().map_or(0, string_heap);
            property_bytes += image.properties.as_ref().map_or(0, map_heap);
        }
        let shape_bytes = self.shapes.heap_bytes();
        property_bytes += shape_bytes;

        let mut index_bytes = self.index.estimated_heap_bytes()
            + table(
                self.lost_vertex_labels.capacity(),
                size_of::<(CompactString, SlotBits)>(),
            )
            + table(
                self.gained_edge_labels.capacity(),
                size_of::<(CompactString, Vec<EdgeHandle>)>(),
            );
        for (label, slots) in &self.lost_vertex_labels {
            index_bytes += string_heap(label) + slots.heap_bytes();
        }
        for (label, edges) in &self.gained_edge_labels {
            index_bytes +=
                string_heap(label) + edges.capacity() as u64 * size_of::<EdgeHandle>() as u64;
        }
        MemoryStats {
            structure_bytes,
            index_bytes,
            property_bytes,
            shape_bytes,
            ..MemoryStats::default()
        }
    }

    /// Releases the spare capacity of the delta (`LayeredGraph::shrink_to_fit`): the slot
    /// vectors above the last used slot (vacant slots stay, handles are stable), the indexes,
    /// the generation arrays, the shared key lists no map uses any more, and the stub, image
    /// and label maps and bitsets. Changes nothing a read sees.
    pub(crate) fn shrink_to_fit(&mut self) {
        self.graph.shrink_to_fit();
        self.index.shrink_to_fit();
        self.shapes.shrink_to_fit();
        self.node_generations.shrink_to_fit();
        self.edge_generations.shrink_to_fit();
        self.stubs.shrink_to_fit();
        self.stub_bases.shrink_to_fit();
        self.removed_vertices.shrink_to_fit();
        self.removed_edges.shrink_to_fit();
        self.imaged_vertices.shrink_to_fit();
        self.imaged_edges.shrink_to_fit();
        self.relabeled_edges.shrink_to_fit();
        self.edge_images.shrink_to_fit();
        self.lost_vertex_labels.shrink_to_fit();
        for slots in self.lost_vertex_labels.values_mut() {
            slots.shrink_to_fit();
        }
        self.gained_edge_labels.shrink_to_fit();
        for edges in self.gained_edge_labels.values_mut() {
            edges.shrink_to_fit();
        }
    }

    /// The delta topology's vertex and edge counts (added elements and stubs): what the
    /// layer's own footprint grows with (`LayeredGraph`'s footprint cache key).
    pub(crate) fn slot_counts(&self) -> (u64, u64) {
        (
            self.graph.node_count() as u64,
            self.graph.edge_count() as u64,
        )
    }
}
