//! Delta visibility of a [`LayeredGraph`] (design `ai/roadmap/layered-graph.md` §4): the origin
//! of an element ([`LayerOrigin`], `GraphStorage::vertex_layer` / `edge_layer`) and the diff of
//! the layer against its base ([`LayerDiff`], `GraphStorage::layer_diff`).
//!
//! - **Origin:** an untouched base element is [`LayerOrigin::Base`] after the read path's one
//!   compare (`is_clean_base_vertex`), with the delta half out of line as for every read. A
//!   delta element is `Added`; a dirty base element is `Modified` exactly while it has an image
//!   (its own labels or properties differ from the base: a return to clean drops the image), so
//!   a base vertex that only has a stub (new incident edges) or lost base edges stays `Base`.
//! - **Diff:** walks only what the layer holds: the delta's slots (added elements, stubs
//!   skipped), and the set bits of the tombstones and of the image bits (removed and modified
//!   base elements, in base slot order), never every base element. Removed elements report the
//!   base's labels and properties (logical values: what a read of the base returns).

use compact_str::CompactString;

use super::LayeredGraph;
use super::delta::logical;
use super::handle::{LayeredEdge, LayeredVertex};
use crate::error::GraphError;
use crate::graph::layer_diff::{
    LayerDiff, LayerDiffChange, LayerDiffEntry, LayerDiffRequest, LayerOrigin, LayerPropertyChange,
};
use crate::graph::schema::ElementKind;
use crate::graph::topology::{EdgeIndex, NodeIndex};
use crate::graph::{EdgeHandle, GraphStorage, VertexHandle};
use crate::value::identity::identity_eq;
use crate::value::{PropertyMap, logical_element};

/// The diff being filled: the request's filters and what is left of its limit.
struct Collector<'r> {
    request: &'r LayerDiffRequest,
    /// Entries the limit still allows.
    left: usize,
    /// Set once the limit refused an entry: nothing more is collected.
    stopped: bool,
    diff: LayerDiff,
}

impl<'r> Collector<'r> {
    fn new(request: &'r LayerDiffRequest) -> Self {
        Self {
            request,
            left: request.limit.unwrap_or(usize::MAX),
            stopped: false,
            diff: LayerDiff::default(),
        }
    }

    /// Room for one more matching entry: `true` takes it; `false` (the limit is used up)
    /// records the truncation and stops the walk.
    fn admit(&mut self) -> bool {
        if self.left == 0 {
            self.diff.truncated = true;
            self.stopped = true;
            return false;
        }
        self.left -= 1;
        true
    }
}

/// The property changes from `base` to `layer`: the keys whose values differ (`identity_eq`,
/// which reads a compressed base string as its text) or that only the layer has, in the layer's
/// order, then the keys the layer removed, in the base's order. Values are logical.
fn property_changes(
    base: Option<&PropertyMap>,
    layer: Option<&PropertyMap>,
) -> Vec<LayerPropertyChange> {
    let mut changes = Vec::new();
    if let Some(layer) = layer {
        for (key, value) in layer.iter() {
            let old = base.and_then(|base| base.get(key));
            if old.is_some_and(|old| identity_eq(old, value)) {
                continue;
            }
            changes.push(LayerPropertyChange::new(
                key.clone(),
                old.map(|old| logical(old.clone())),
                Some(value.clone()),
            ));
        }
    }
    if let Some(base) = base {
        for (key, value) in base.iter() {
            if layer.is_none_or(|layer| !layer.contains_key(key)) {
                changes.push(LayerPropertyChange::new(
                    key.clone(),
                    Some(logical(value.clone())),
                    None,
                ));
            }
        }
    }
    changes
}

impl LayeredGraph {
    /// `GraphStorage::vertex_layer`: `None` for a removed vertex or a stale handle.
    #[inline(always)]
    pub(super) fn vertex_origin(&self, node: LayeredVertex) -> Option<LayerOrigin> {
        if self.is_clean_base_vertex(node) {
            return self
                .base
                .vertex_weight(node.to_base())
                .map(|_| LayerOrigin::Base);
        }
        self.vertex_origin_delta(node)
    }

    /// The delta half of [`Self::vertex_origin`].
    #[inline(never)]
    fn vertex_origin_delta(&self, node: LayeredVertex) -> Option<LayerOrigin> {
        if node.is_delta() {
            return self.delta.vertex(node).map(|_| LayerOrigin::Added);
        }
        let slot = node.slot();
        self.base.vertex_weight(node.to_base())?;
        if self.delta.is_removed_vertex(slot) {
            None
        } else if self.delta.imaged_vertices.contains(slot) {
            Some(LayerOrigin::Modified)
        } else {
            Some(LayerOrigin::Base)
        }
    }

    /// `GraphStorage::edge_layer`: `None` for a removed edge or a stale handle.
    #[inline(always)]
    pub(super) fn edge_origin(&self, edge: LayeredEdge) -> Option<LayerOrigin> {
        if self.is_clean_base_edge(edge) {
            return self
                .base
                .edge_weight(edge.to_base())
                .map(|_| LayerOrigin::Base);
        }
        self.edge_origin_delta(edge)
    }

    /// The delta half of [`Self::edge_origin`].
    #[inline(never)]
    fn edge_origin_delta(&self, edge: LayeredEdge) -> Option<LayerOrigin> {
        if edge.is_delta() {
            return self.delta.edge(edge).map(|_| LayerOrigin::Added);
        }
        let slot = edge.slot();
        self.base.edge_weight(edge.to_base())?;
        if self.delta.is_removed_edge(slot) {
            None
        } else if self.delta.is_imaged_edge(slot) {
            Some(LayerOrigin::Modified)
        } else {
            Some(LayerOrigin::Base)
        }
    }

    /// `GraphStorage::layer_diff` (module docs).
    pub(super) fn diff(&self, request: &LayerDiffRequest) -> Result<LayerDiff, GraphError> {
        let mut out = Collector::new(request);
        if request.wants_kind(ElementKind::Vertex) {
            self.diff_added_vertices(&mut out);
            self.diff_removed_vertices(&mut out);
            self.diff_modified_vertices(&mut out);
        }
        if request.wants_kind(ElementKind::Edge) {
            self.diff_added_edges(&mut out)?;
            self.diff_removed_edges(&mut out)?;
            self.diff_modified_edges(&mut out)?;
        }
        Ok(out.diff)
    }

    /// The live base vertex in base slot `slot` (a set bit of a layer bitset names one).
    pub(super) fn base_vertex_at(&self, slot: u32) -> Option<VertexHandle> {
        let index = NodeIndex::new(slot as usize);
        let vertex = self.base.graph.node_weight(index)?;
        Some(VertexHandle::new(index, vertex.3))
    }

    /// The live base edge in base slot `slot`.
    pub(super) fn base_edge_at(&self, slot: u32) -> Option<EdgeHandle> {
        let index = EdgeIndex::new(slot as usize);
        let edge = self.base.graph.edge_weight(index)?;
        Some(EdgeHandle::new(index, edge.3.own))
    }

    /// The base's ids of the endpoints of the base edge `edge`.
    fn base_endpoint_ids(
        &self,
        edge: EdgeHandle,
    ) -> Result<(CompactString, CompactString), GraphError> {
        let (from, to) = GraphStorage::edge_endpoints(&*self.base, edge)?
            .ok_or_else(GraphError::edge_removed)?;
        let id = |node: VertexHandle| {
            self.base
                .vertex_weight(node)
                .map(|vertex| vertex.0.clone())
                .ok_or_else(GraphError::vertex_removed)
        };
        Ok((id(from)?, id(to)?))
    }

    /// The id of a live vertex of the layer (an endpoint of a live edge).
    fn live_vertex_id(&self, node: LayeredVertex) -> Result<CompactString, GraphError> {
        GraphStorage::vertex_id(self, node)?
            .map(CompactString::from)
            .ok_or_else(GraphError::vertex_removed)
    }

    fn diff_added_vertices(&self, out: &mut Collector<'_>) {
        if out.stopped {
            return;
        }
        for node in self.delta.vertices() {
            let Some(view) = self.delta.vertex(node) else {
                continue;
            };
            if !out.request.matches_labels(view.labels) {
                continue;
            }
            if !out.admit() {
                return;
            }
            out.diff.added.push(LayerDiffEntry::vertex(
                view.id,
                view.labels.to_vec(),
                view.properties.clone(),
            ));
        }
    }

    fn diff_removed_vertices(&self, out: &mut Collector<'_>) {
        let Some(removed) = self.delta.removed_vertices() else {
            return;
        };
        if out.stopped {
            return;
        }
        for slot in removed.ones() {
            let Some(vertex) = self
                .base_vertex_at(slot)
                .and_then(|h| self.base.vertex_weight(h))
            else {
                continue;
            };
            let labels = self.base.labels.set(vertex.1);
            if !out.request.matches_labels(labels) {
                continue;
            }
            if !out.admit() {
                return;
            }
            out.diff.removed.push(LayerDiffEntry::vertex(
                vertex.0.clone(),
                labels.to_vec(),
                logical_element(&vertex.2),
            ));
        }
    }

    fn diff_modified_vertices(&self, out: &mut Collector<'_>) {
        if out.stopped {
            return;
        }
        for slot in self.delta.imaged_vertices().ones() {
            let Some(node) = self.base_vertex_at(slot) else {
                continue;
            };
            let (Some(base), Some(layer)) = (
                self.base.vertex_weight(node),
                self.delta.base_vertex(&self.base, node),
            ) else {
                continue;
            };
            let base_labels = self.base.labels.set(base.1);
            if !out.request.matches_labels(base_labels) && !out.request.matches_labels(layer.labels)
            {
                continue;
            }
            if !out.admit() {
                return;
            }
            out.diff.modified.push(LayerDiffChange::vertex(
                layer.id,
                base_labels.to_vec(),
                layer.labels.to_vec(),
                property_changes(Some(&base.2), Some(layer.properties)),
            ));
        }
    }

    fn diff_added_edges(&self, out: &mut Collector<'_>) -> Result<(), GraphError> {
        if out.stopped {
            return Ok(());
        }
        for edge in self.delta.edges() {
            let Some(view) = self.delta.edge(edge) else {
                continue;
            };
            if !out.request.matches_labels(view.label) {
                continue;
            }
            if !out.admit() {
                return Ok(());
            }
            let (from, to) = view.endpoints;
            out.diff.added.push(LayerDiffEntry::edge(
                view.id,
                view.label.first().cloned(),
                self.live_vertex_id(from)?,
                self.live_vertex_id(to)?,
                view.properties.cloned().unwrap_or_default(),
            ));
        }
        Ok(())
    }

    fn diff_removed_edges(&self, out: &mut Collector<'_>) -> Result<(), GraphError> {
        let Some(removed) = self.delta.removed_edges() else {
            return Ok(());
        };
        if out.stopped {
            return Ok(());
        }
        for slot in removed.ones() {
            let Some(edge) = self.base_edge_at(slot) else {
                continue;
            };
            let Some(weight) = self.base.edge_weight(edge) else {
                continue;
            };
            let label = self.base.labels.name_slice(weight.1);
            if !out.request.matches_labels(label) {
                continue;
            }
            if !out.admit() {
                return Ok(());
            }
            let (from, to) = self.base_endpoint_ids(edge)?;
            out.diff.removed.push(LayerDiffEntry::edge(
                weight.0.clone(),
                label.first().cloned(),
                from,
                to,
                weight.2.as_ref().map(logical_element).unwrap_or_default(),
            ));
        }
        Ok(())
    }

    fn diff_modified_edges(&self, out: &mut Collector<'_>) -> Result<(), GraphError> {
        if out.stopped {
            return Ok(());
        }
        for slot in self.delta.imaged_edges().ones() {
            let Some(edge) = self.base_edge_at(slot) else {
                continue;
            };
            let (Some(base), Some(layer)) = (
                self.base.edge_weight(edge),
                self.delta.base_edge(&self.base, edge),
            ) else {
                continue;
            };
            let base_label = self.base.labels.name_slice(base.1);
            if !out.request.matches_labels(base_label) && !out.request.matches_labels(layer.label) {
                continue;
            }
            if !out.admit() {
                return Ok(());
            }
            let (from, to) = self.base_endpoint_ids(edge)?;
            out.diff.modified.push(LayerDiffChange::edge(
                layer.id,
                base_label.first().cloned(),
                layer.label.first().cloned(),
                from,
                to,
                property_changes(base.2.as_ref(), layer.properties),
            ));
        }
        Ok(())
    }
}
