//! The delta of a [`LayeredGraph`](super::LayeredGraph): what the layer stores beside the base.
//!
//! Design `ai/roadmap/layered-graph.md` §3. Today it holds the elements the layer ADDS, in a
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
//! counts, the statistics, the id index or the label indexes, and no layer handle names it
//! (`stub_bases` turns a stub endpoint back into the base vertex's handle; [`Delta::vertex`]
//! and the adjacency of a delta handle refuse a stub slot). A stub stays when its last edge is
//! dropped (it is empty then, and still invisible); its base vertex stays dirty.
//!
//! The tombstones of removed base elements and the images of modified base elements are later
//! tasks (S1.5, S1.6); until then [`Delta::base_vertex`] answers with the base's own state (a
//! base vertex is dirty only for its stub) and [`Delta::base_edge`] answers "removed" (no base
//! edge is dirty yet).
//!
//! The delta stores what the write rules (`graph/write_rules.rs`) returned: no check runs here.
//! Values are stored uncompressed (plan P5).

use compact_str::CompactString;
use fnv::FnvBuildHasher;

use super::handle::{LayeredEdge, LayeredVertex, MAX_BASE_SLOTS};
use crate::error::GraphError;
use crate::graph::handle::{EdgeGenerations, FIRST_GENERATION, Generations};
use crate::graph::labels::{LabelSetId, Labels};
use crate::graph::shape::ShapeTable;
use crate::graph::topology::{Direction, NodeIndex};
use crate::graph::{
    EdgeHandle, GraphIndex, GraphStatistics, GraphType, TraversalGraph, VertexHandle,
};
use crate::value::{ElementProperty, PropertyMap};

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

/// The layer's own data: today the added elements.
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
        (self.graph.node_count() - self.stubs.len()) as isize
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
        self.graph.edge_count() as isize
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

    /// The added vertices carrying `label`, in the label bucket's order.
    #[inline]
    pub(crate) fn vertices_by_label<'a>(
        &'a self,
        label: &str,
    ) -> impl Iterator<Item = LayeredVertex> + 'a {
        self.index
            .get_nodes_by_label(label)
            .into_iter()
            .flatten()
            .map(LayeredVertex::from_delta)
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

    /// A dirty base vertex as the layer sees it: `None` when the layer removed it (or `node` is
    /// stale), else its state. No tombstones or images yet (S1.5, S1.6): a base vertex is dirty
    /// only because it has a stub, which changes its adjacency, not the vertex, so it answers
    /// with the base's own id, labels and properties.
    #[inline]
    pub(crate) fn base_vertex<'a>(
        &'a self,
        base: &'a TraversalGraph,
        node: VertexHandle,
    ) -> Option<VertexView<'a>> {
        let vertex = base.vertex_weight(node)?;
        Some(VertexView {
            id: &vertex.0,
            labels: base.labels.set(vertex.1),
            properties: &vertex.2,
        })
    }

    /// A dirty base edge as the layer sees it; see [`Self::base_vertex`].
    #[inline]
    pub(crate) fn base_edge(&self, edge: EdgeHandle) -> Option<EdgeView<'_>> {
        let _ = edge;
        None
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
