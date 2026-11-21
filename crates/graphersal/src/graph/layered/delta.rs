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
use crate::graph::{
    EdgeHandle, GraphIndex, GraphStatistics, GraphStorage, GraphType, TraversalGraph, VertexHandle,
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
struct EdgeImage {
    /// `None` for an unlabeled edge.
    label: Option<CompactString>,
    /// As the base stores it: `None` when the base edge has no map and no write gave it one.
    properties: Option<PropertyMap>,
}

/// One property change of a base element, as the write rules returned it.
pub(crate) enum PropertyChange {
    /// Set `key` to the value.
    Set(CompactString, ElementProperty),
    /// Remove `key`.
    Drop(CompactString),
}

impl PropertyChange {
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

    /// Applies the change to an image map, the value made logical first.
    fn apply(self, shapes: &mut ShapeTable, map: &mut PropertyMap) {
        match self {
            PropertyChange::Set(key, value) => {
                shapes.insert_full(map, key, logical(value));
            }
            PropertyChange::Drop(key) => {
                shapes.shift_remove_full(map, &key);
            }
        }
    }
}

/// `value` with every storage-only string (compressed, deduplicated) made plain: what the
/// delta stores (plan P5).
#[inline]
fn logical(value: ElementProperty) -> ElementProperty {
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
    /// nothing for a stale handle.
    pub(crate) fn remove_edge(&mut self, edge: LayeredEdge, statistics: &mut GraphStatistics) {
        let local = edge.to_delta();
        if self
            .graph
            .edge_weight(local.index())
            .is_none_or(|weight| weight.3.own != local.generation())
        {
            return;
        }
        let Some((id, label, _, _)) = self.graph.remove_edge(local.index()) else {
            return;
        };
        let label = self.labels.name(label).cloned();
        statistics.edge_removed(label.as_deref());
        if let Some(label) = &label {
            self.index.remove_edge_label(label, local);
        }
        self.index.remove_edge(&id, local);
        self.edge_generations.bump(local.index().index());
    }

    /// Removes a live delta vertex, its incident edges (also those to base vertices: they leave
    /// the stubs' lists, the stubs stay) and every index entry, and retires the slot
    /// generations; nothing for a stale handle.
    pub(crate) fn remove_vertex(&mut self, node: LayeredVertex, statistics: &mut GraphStatistics) {
        let index = self.delta_index(node);
        if index == NodeIndex::end() {
            return;
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
            self.remove_edge(edge, statistics);
        }
        let local = node.to_delta();
        let Some((id, labels, _, _)) = self.graph.remove_node(index) else {
            return;
        };
        let labels = self.labels.set(labels).to_vec();
        statistics.vertex_removed(&labels);
        self.index.remove_node(&id, local);
        for label in &labels {
            self.index.remove_node_label(label, local);
        }
        self.node_generations.bump(index.index());
    }

    /// Tombstones the live base edge `edge` (the caller checked it through the write rules and
    /// sets the dirty bits of the edge and of both endpoints) and counts it out of the
    /// statistics as `TraversalGraph`'s raw remove does. Nothing for an edge already removed or
    /// stale.
    pub(crate) fn remove_base_edge(
        &mut self,
        base: &TraversalGraph,
        edge: EdgeHandle,
        statistics: &mut GraphStatistics,
    ) {
        let slot = edge.index().slot();
        if self.removed_edges.contains(slot) {
            return;
        }
        let Some(weight) = base.edge_weight(edge) else {
            return;
        };
        // The label it has now: its image's, when a write changed it.
        let image = if self.imaged_edges.contains(slot) {
            self.imaged_edges.clear(slot);
            self.edge_images.remove(&slot)
        } else {
            None
        };
        match image {
            Some(image) => {
                statistics.edge_removed(image.label.as_deref());
                if self.relabeled_edges.contains(slot) {
                    self.relabeled_edges.clear(slot);
                    if let Some(label) = &image.label {
                        self.remove_gained_edge(label, edge);
                    }
                }
            }
            None => statistics.edge_removed(base.labels.name(weight.1).map(|l| l.as_str())),
        }
        self.removed_edges.set_growing(slot);
        self.removed_edge_count += 1;
    }

    /// Tombstones the live base vertex `node` and removes its stub, if it has one (the caller
    /// removed every incident edge first, base and added, and sets the vertex's dirty bit),
    /// with its image and the label memberships the image had; counts it out of the
    /// statistics, with the labels it has now, as `TraversalGraph`'s raw remove does. Nothing
    /// for a vertex already removed or stale.
    pub(crate) fn remove_base_vertex(
        &mut self,
        base: &TraversalGraph,
        node: VertexHandle,
        statistics: &mut GraphStatistics,
    ) {
        let slot = node.index().slot();
        if self.removed_vertices.contains(slot) {
            return;
        }
        let Some(vertex) = base.vertex_weight(node) else {
            return;
        };
        let base_labels = base.labels.set(vertex.1);
        let stub = self.stub_index(node);
        let imaged = self.imaged_vertices.contains(slot);
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
            }
            _ => statistics.vertex_removed(base_labels),
        }
        self.removed_vertices.set_growing(slot);
        self.removed_vertex_count += 1;
        if stub != NodeIndex::end() {
            // Empty by now; `remove_node` would unlink a leftover edge without its index
            // entries, so the caller's order (edges first) is what keeps the indexes exact.
            self.graph.remove_node(stub);
            self.node_generations.bump(stub.index());
            self.stubs.remove(&slot);
            if let Some(entry) = self.stub_bases.get_mut(stub.index()) {
                *entry = None;
            }
        }
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

    /// Applies one property change to the image of the live base vertex `node`.
    pub(crate) fn change_base_vertex_property(
        &mut self,
        base: &TraversalGraph,
        node: VertexHandle,
        change: PropertyChange,
    ) -> Result<bool, GraphError> {
        let (stub, existed) = self.vertex_image(base, node)?;
        if let Some(weight) = self.graph.node_weight_mut(stub) {
            change.apply(&mut self.shapes, &mut weight.2);
        }
        Ok(existed)
    }

    /// Gives the image of the live base vertex `node` the label list `new_labels` (from the
    /// write rules) and keeps the label lookups (module docs) and the statistics in step.
    pub(crate) fn set_base_vertex_labels(
        &mut self,
        base: &TraversalGraph,
        node: VertexHandle,
        new_labels: Vec<CompactString>,
        statistics: &mut GraphStatistics,
    ) -> Result<bool, GraphError> {
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
        Ok(existed)
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
        if let Some(image) = self.graph.node_weight_mut(stub) {
            image.1 = LabelSetId::EMPTY;
            image.2 = PropertyMap::default();
        }
        self.imaged_vertices.clear(slot);
        self.vertex_image_count -= 1;
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
    /// edge without a map one, as `TraversalGraph` does).
    pub(crate) fn change_base_edge_property(
        &mut self,
        base: &TraversalGraph,
        edge: EdgeHandle,
        change: PropertyChange,
    ) -> Result<bool, GraphError> {
        let existed = self.edge_image(base, edge)?;
        if let Some(image) = self.edge_images.get_mut(&edge.index().slot()) {
            match change {
                PropertyChange::Set(..) => {
                    let map = image.properties.get_or_insert_with(PropertyMap::default);
                    change.apply(&mut self.shapes, map);
                }
                PropertyChange::Drop(_) => {
                    if let Some(map) = image.properties.as_mut() {
                        change.apply(&mut self.shapes, map);
                    }
                }
            }
        }
        Ok(existed)
    }

    /// Relabels the image of the live base edge `edge` (the write rules passed and the label
    /// differs), keeping the label lookups (module docs) and the statistics in step.
    pub(crate) fn set_base_edge_label(
        &mut self,
        base: &TraversalGraph,
        edge: EdgeHandle,
        label: CompactString,
        statistics: &mut GraphStatistics,
    ) -> Result<bool, GraphError> {
        let existed = self.edge_image(base, edge)?;
        let slot = edge.index().slot();
        let base_label = base
            .edge_weight(edge)
            .and_then(|weight| base.labels.name(weight.1))
            .cloned();
        let Some(image) = self.edge_images.get_mut(&slot) else {
            return Err(GraphError::edge_removed());
        };
        let old = std::mem::replace(&mut image.label, Some(label.clone()));
        statistics.edge_relabeled(old.as_deref(), Some(label.as_str()));
        if old != base_label
            && let Some(old) = &old
        {
            self.remove_gained_edge(old, edge);
        }
        if Some(&label) == base_label.as_ref() {
            self.relabeled_edges.clear(slot);
        } else {
            self.relabeled_edges.set_growing(slot);
            self.gained_edge_labels.entry(label).or_default().push(edge);
        }
        Ok(existed)
    }

    /// Removes `edge` from the base edges relabeled to `label` (the last one takes its place).
    fn remove_gained_edge(&mut self, label: &str, edge: EdgeHandle) {
        let Some(members) = self.gained_edge_labels.get_mut(label) else {
            return;
        };
        if let Some(position) = members.iter().position(|member| *member == edge) {
            members.swap_remove(position);
        }
        if members.is_empty() {
            self.gained_edge_labels.remove(label);
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
            self.edge_images.remove(&slot);
            self.imaged_edges.clear(slot);
        }
    }

    /// Replaces the label set of a live delta vertex with `new_labels` (from the write rules).
    pub(crate) fn set_vertex_labels(
        &mut self,
        node: LayeredVertex,
        new_labels: Vec<CompactString>,
        statistics: &mut GraphStatistics,
    ) {
        let local = node.to_delta();
        let Some(vertex) = self
            .graph
            .node_weight_mut(local.index())
            .filter(|vertex| vertex.3 == local.generation())
        else {
            return;
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
    }

    /// Relabels a live delta edge (the write rules passed).
    pub(crate) fn set_edge_label(
        &mut self,
        edge: LayeredEdge,
        label: CompactString,
        statistics: &mut GraphStatistics,
    ) {
        let local = edge.to_delta();
        let Some(weight) = self
            .graph
            .edge_weight_mut(local.index())
            .filter(|weight| weight.3.own == local.generation())
        else {
            return;
        };
        let new_id = self.labels.intern_name(Some(label.as_str()));
        let old = self
            .labels
            .name(std::mem::replace(&mut weight.1, new_id))
            .cloned();
        statistics.edge_relabeled(old.as_deref(), Some(label.as_str()));
        if let Some(old) = &old {
            self.index.remove_edge_label(old, local);
        }
        self.index.insert_edge_label(label, local);
    }

    /// Sets one property of a live delta vertex to the value the write rules returned.
    pub(crate) fn set_vertex_property(
        &mut self,
        node: LayeredVertex,
        key: CompactString,
        value: ElementProperty,
    ) {
        let local = node.to_delta();
        if let Some(vertex) = self
            .graph
            .node_weight_mut(local.index())
            .filter(|vertex| vertex.3 == local.generation())
        {
            self.shapes.insert_full(&mut vertex.2, key, value);
        }
    }

    /// Sets one property of a live delta edge to the value the write rules returned.
    pub(crate) fn set_edge_property(
        &mut self,
        edge: LayeredEdge,
        key: CompactString,
        value: ElementProperty,
    ) {
        let local = edge.to_delta();
        if let Some(weight) = self
            .graph
            .edge_weight_mut(local.index())
            .filter(|weight| weight.3.own == local.generation())
        {
            let map = weight.2.get_or_insert_with(PropertyMap::default);
            self.shapes.insert_full(map, key, value);
        }
    }

    /// Removes one property of a live delta vertex (absent: nothing).
    pub(crate) fn drop_vertex_property(&mut self, node: LayeredVertex, key: &str) {
        let local = node.to_delta();
        if let Some(vertex) = self
            .graph
            .node_weight_mut(local.index())
            .filter(|vertex| vertex.3 == local.generation())
        {
            self.shapes.shift_remove_full(&mut vertex.2, key);
        }
    }

    /// Removes one property of a live delta edge (absent: nothing).
    pub(crate) fn drop_edge_property(&mut self, edge: LayeredEdge, key: &str) {
        let local = edge.to_delta();
        if let Some(weight) = self
            .graph
            .edge_weight_mut(local.index())
            .filter(|weight| weight.3.own == local.generation())
            && let Some(map) = weight.2.as_mut()
        {
            self.shapes.shift_remove_full(map, key);
        }
    }
}
