//! The reset of a layer ([`LayeredGraph::reset_layer`], [`Graph::reset_layer`]): every change
//! the layer made over its base discarded in one call, the base untouched.
//!
//! **One ordinary unit.** The reset writes the inverse of the delta through the layer's own
//! write paths (rules skipped: it restores states the rules accepted), each write recorded in
//! the undo log, so it is atomic like any traversal: a failure, a vetoing commit hook or a dry
//! run leave the layer as it was; the commit hooks get its [`ChangeSet`](crate::changes::ChangeSet)
//! (derived by `capture` as for any unit), a Store's journal writes it as one commit and
//! recovery replays it. Cost O(delta): nothing proportional to the base.
//!
//! **Order** (the change set must replay through the rule-checked write methods, on this
//! layer and on any replica, so every intermediate state passes the rules):
//! 1. when base elements are to be put back (images or tombstones exist): the layer's schema
//!    replaced by "no schema" and every unique property index definition removed (a swap of
//!    unique values between two modified elements, or a restored element holding a value an
//!    image holds, would fail a check half-way);
//! 2. the added edges dropped, then the added vertices;
//! 3. every modified base edge dropped (it comes back with its base state in 4: an edge image
//!    of a base edge without a map cannot be turned back into "no map" by property writes);
//!    every modified base vertex written back: its labels (removals first, then the base's
//!    missing labels appended in order, so a replay that appends reaches the base's order),
//!    its properties (the extra keys dropped, the values set; when the remaining keys are in
//!    another order than the base's, all dropped and set again in the base's order): the image
//!    is identical to the base vertex again and is dropped (return to clean);
//! 4. every removed base vertex, then every removed base edge made visible again
//!    ([`LayerUndo::RestoredBaseVertex`] / [`LayerUndo::RestoredBaseEdge`], reported as
//!    `AddVertex` / `AddEdge` with the base state);
//! 5. the catalog put back to the base's (removals first), then the base's schema.
//!
//! Everything it wrote is exempt from the vector-rule pass (the base's vectors come back as
//! they are).
//!
//! **A create equal to a removed base element is that element again.** A vertex created with
//! the id and the exact state (labels in order, every property by `identity_eq`, in order) of
//! a base vertex the layer removed, or an edge with the id, the base endpoints, the label and
//! the properties of a removed base edge (an empty map matching a base edge without one),
//! restores the base element instead of adding a new one (`store_vertex`, `store_edge`). That
//! is what makes a journal replay of a reset reach the same state as the reset itself (the
//! replay sees `AddVertex`/`AddEdge`), and it is the layer's minimal delta, as the return to
//! clean of an image is. The restored element keeps its base handle and its base position.
//!
//! **What a reset keeps:** the auto-id sequences (an id once handed out is never handed out
//! again, as after a rollback), the commit position and clock, the lineage id, the registered
//! hooks and the journal slot, the base's name. Not graph data, so not reset: the workload
//! statistics (kept per instance by the `Graph`, or a Store's sidecar).
//!
//! **Fresh afterwards.** When the reset committed as a unit of its own (and after a replayed
//! change set that leaves the layer without changes, `persist`), the layer drops its now empty
//! bookkeeping (`compact_if_clean`): new dirty bits, a new delta with the old slot generations
//! (so no stale handle of a removed added element names a later one), the base's statistics,
//! fresh index overlays. Reads take the clean fast path again and the layer's own memory is
//! that of a new layer.

use compact_str::CompactString;

use super::LayeredGraph;
use super::delta::{Delta, PropertyChange};
use super::dirty::SlotBits;
use super::handle::{LayeredEdge, LayeredVertex};
use super::units::LayerUndo;
use crate::error::GraphError;
use crate::graph::definition::{Definition, DefinitionKind};
use crate::graph::schema::GraphSchema;
use crate::graph::{EdgeHandle, Graph, GraphStorage, LayerCounts, LayerReset, VertexHandle};
use crate::value::identity::identity_eq;
use crate::value::{PropertyMap, logical_element};

/// One catalog entry that differs from the base's: its kind and name and what the base holds
/// (`None`: the base has no such definition, the layer's goes).
type CatalogChange = (DefinitionKind, CompactString, Option<Definition>);

impl LayeredGraph {
    /// Discards every change this layer made over its base: the elements it added are removed,
    /// the base elements it changed or removed are the base's again, its schema and catalog
    /// are the base's again. The base is never written. Afterwards the layer reads exactly as
    /// its base, [`GraphStorage::layer_diff`] is empty and its statistics and index overlays are
    /// those of a new layer. Returns what was discarded.
    ///
    /// The reset is ONE unit (book page "Layers", section "Discarding a layer's changes"):
    /// outside a unit it commits as a unit of its own, inside one it is part of it; a failure,
    /// a vetoing commit hook ([`GraphError::CommitRejected`]) or a dry run leave the layer
    /// unchanged. While a hook is registered it gets the reset's change set (the drops, the
    /// writes back and the base elements created again), and a Store's journal writes it as
    /// one commit that recovery replays. Kept: the auto-id sequences (new elements get new
    /// ids, never one handed out before), the commit position, the hooks. A layer without
    /// changes commits nothing (an empty [`LayerReset`]).
    ///
    /// # Errors
    ///
    /// What a commit can fail with: [`GraphError::CommitRejected`] (a hook vetoed),
    /// [`GraphError::ResourceLimitExceeded`] (the process memory ceiling at the commit),
    /// [`GraphError::SequenceOverflow`] (the commit sequence).
    ///
    /// ```
    /// use std::sync::Arc;
    /// use graphersal::prelude::*;
    /// use graphersal::storage::LayeredGraph;
    ///
    /// let base = Arc::new(TraversalGraph::tinkerpop_modern());
    /// let mut layer = LayeredGraph::over(base).unwrap();
    /// GraphTraversalSource::new(&mut layer).v("1").drop().iterate().unwrap();
    /// GraphTraversalSource::new(&mut layer).add_v("person").iterate().unwrap();
    ///
    /// let reset = layer.reset_layer().unwrap();
    /// assert_eq!((reset.added.vertices, reset.removed.vertices), (1, 1));
    /// let g = GraphTraversalSource::new(&layer);
    /// assert_eq!(g.v(None).count().next().unwrap(), Some(6_i64.into()));
    /// assert!(layer.reset_layer().unwrap().is_empty());
    /// ```
    pub fn reset_layer(&mut self) -> Result<LayerReset, GraphError> {
        let catalog = self.catalog_changes();
        let [added, modified, removed] = self
            .delta
            .change_counts()
            .map(|(vertices, edges)| LayerCounts::new(vertices, edges));
        let base_schema = self.base.schema.read().clone();
        let report = LayerReset {
            added,
            modified,
            removed,
            schema: self.schema != base_schema,
            definitions: catalog.len(),
        };
        if report.is_empty() {
            return Ok(report);
        }
        let outermost = !self.units.is_recording();
        self.in_own_unit(|this| this.reset_in_unit(base_schema))?;
        if outermost {
            self.compact_if_clean();
        }
        Ok(report)
    }

    /// The steps of [`Self::reset_layer`] inside its unit (module docs, "Order").
    fn reset_in_unit(&mut self, base_schema: GraphSchema) -> Result<(), GraphError> {
        let start = self.units.entries().len();
        let [
            _,
            (imaged_vertices, imaged_edges),
            (removed_vertices, removed_edges),
        ] = self.delta.change_counts();
        if imaged_vertices + imaged_edges + removed_vertices + removed_edges > 0 {
            if self.schema != GraphSchema::default() {
                self.put_schema(GraphSchema::default());
            }
            let unique: Vec<(DefinitionKind, CompactString)> = self
                .catalog
                .iter()
                .filter(|d| d.as_property_index().is_some_and(|i| i.is_unique()))
                .map(|d| (d.kind(), CompactString::from(d.name())))
                .collect();
            for (kind, name) in unique {
                self.put_definition(kind, &name, None);
            }
        }
        // 2. The added elements: edges, then vertices (their edges are gone by then).
        let edges: Vec<LayeredEdge> = self.delta.edges().collect();
        for edge in edges {
            self.drop_added_edge(edge);
        }
        let vertices: Vec<LayeredVertex> = self.delta.vertices().collect();
        for node in vertices {
            self.drop_added_vertex(node);
        }
        // 3. The modified base elements.
        let slots: Vec<u32> = self.delta.imaged_edges().ones().collect();
        for slot in slots {
            if let Some(edge) = self.base_edge_at(slot) {
                self.drop_base_edge(LayeredEdge::from_base(edge));
            }
        }
        let slots: Vec<u32> = self.delta.imaged_vertices().ones().collect();
        for slot in slots {
            if let Some(node) = self.base_vertex_at(slot) {
                self.revert_vertex_image(node)?;
            }
        }
        // 4. The removed base elements: vertices first (the edges' endpoints).
        let slots: Vec<u32> = self
            .delta
            .removed_vertices()
            .map(|removed| removed.ones().collect())
            .unwrap_or_default();
        for slot in slots {
            if let Some(node) = self.base_vertex_at(slot) {
                self.restore_removed_base_vertex(node)?;
            }
        }
        let slots: Vec<u32> = self
            .delta
            .removed_edges()
            .map(|removed| removed.ones().collect())
            .unwrap_or_default();
        for slot in slots {
            if let Some(edge) = self.base_edge_at(slot) {
                self.restore_removed_base_edge(edge);
            }
        }
        // 5. The catalog (read again: step 1 may have removed definitions), removals first,
        //    then the schema.
        let mut catalog = self.catalog_changes();
        catalog.sort_by_key(|(_, _, target)| target.is_some());
        for (kind, name, target) in catalog {
            self.put_definition(kind, &name, target);
        }
        if self.schema != base_schema {
            self.put_schema(base_schema);
        }
        self.units.exempt_from(start);
        Ok(())
    }

    /// Writes the image of the live base vertex `node` back to the base's state (module docs,
    /// step 3): the image is dropped by the last write (return to clean).
    fn revert_vertex_image(&mut self, node: VertexHandle) -> Result<(), GraphError> {
        let Some(vertex) = self.base.vertex_weight(node) else {
            return Ok(());
        };
        let base_labels: Vec<CompactString> = self.base.labels.set(vertex.1).to_vec();
        let base_map = logical_element(&vertex.2);
        let layered = LayeredVertex::from_base(node);
        let current: Vec<CompactString> = GraphStorage::vertex_labels(&*self, layered)?.to_vec();
        if current != base_labels {
            let kept: Vec<CompactString> = current
                .iter()
                .filter(|label| base_labels.contains(label))
                .cloned()
                .collect();
            let prefix = kept
                .iter()
                .zip(&base_labels)
                .take_while(|(a, b)| a == b)
                .count();
            let trimmed = kept[..prefix].to_vec();
            if trimmed != current {
                self.store_vertex_labels(layered, trimmed.clone())?;
            }
            if trimmed != base_labels {
                self.store_vertex_labels(layered, base_labels)?;
            }
        }
        let current = GraphStorage::vertex_properties(&*self, layered)?
            .cloned()
            .unwrap_or_default();
        if same_map(&current, &base_map) {
            return Ok(());
        }
        for key in current.keys() {
            if !base_map.contains_key(key) {
                self.change_vertex_property(layered, PropertyChange::Drop(key.clone()))?;
            }
        }
        let common: Vec<&CompactString> = current
            .keys()
            .filter(|key| base_map.contains_key(key))
            .collect();
        let in_order = common
            .iter()
            .zip(base_map.keys())
            .all(|(kept, base)| *kept == base);
        if !in_order {
            for key in common {
                self.change_vertex_property(layered, PropertyChange::Drop(key.clone()))?;
            }
        }
        for (key, value) in base_map.iter() {
            let unchanged = in_order
                && current
                    .get(key.as_str())
                    .is_some_and(|old| identity_eq(old, value));
            if !unchanged {
                self.change_vertex_property(
                    layered,
                    PropertyChange::Set(key.clone(), value.clone()),
                )?;
            }
        }
        Ok(())
    }

    /// The catalog entries that differ from the base's, each with the base's definition.
    fn catalog_changes(&self) -> Vec<CatalogChange> {
        let mut changes: Vec<CatalogChange> = Vec::new();
        for definition in self.catalog.iter() {
            let base = self
                .base
                .definitions
                .get(definition.kind(), definition.name());
            if base != Some(definition) {
                changes.push((
                    definition.kind(),
                    CompactString::from(definition.name()),
                    base.cloned(),
                ));
            }
        }
        for definition in self.base.definitions.iter() {
            if self
                .catalog
                .get(definition.kind(), definition.name())
                .is_none()
            {
                changes.push((
                    definition.kind(),
                    CompactString::from(definition.name()),
                    Some(definition.clone()),
                ));
            }
        }
        changes
    }

    /// Stores `schema` as the layer's, recorded, without the checks of `set_schema` (it is the
    /// base's, which the base's data meets, or "no schema").
    fn put_schema(&mut self, schema: GraphSchema) {
        let old = std::mem::replace(&mut self.schema, schema);
        self.refresh_vector_rules();
        self.units.record(LayerUndo::Schema(Box::new(old)));
    }

    /// Puts the definition of `kind` named `name` to `target` (`None`: removed), recorded,
    /// without the checks of a catalog write (the definitions are the base's); a property
    /// index's overlay follows.
    fn put_definition(&mut self, kind: DefinitionKind, name: &str, target: Option<Definition>) {
        let old = match target {
            Some(definition) => self.catalog.insert(definition),
            None => self.catalog.remove(kind, name),
        };
        if kind.is_property_index() {
            self.layer_index_after_change(name);
        }
        self.units.record(LayerUndo::Definition {
            kind,
            name: CompactString::from(name),
            old: old.map(Box::new),
        });
    }

    /// The live added edge `edge` dropped and recorded (step 2).
    fn drop_added_edge(&mut self, edge: LayeredEdge) {
        let recording = self.units.is_recording();
        if let Some(removed) = self
            .delta
            .remove_edge(edge, &mut self.statistics, recording)
        {
            self.units.record(LayerUndo::RemovedEdge(Box::new(removed)));
        }
    }

    /// The live added vertex `node` dropped and recorded (step 2; its edges are gone).
    fn drop_added_vertex(&mut self, node: LayeredVertex) {
        let mut edges = Vec::new();
        let removed = self
            .delta
            .remove_vertex(node, &mut self.statistics, Some(&mut edges));
        for edge in edges {
            self.units.record(LayerUndo::RemovedEdge(Box::new(edge)));
        }
        if let Some(removed) = removed {
            self.units
                .record(LayerUndo::RemovedVertex(Box::new(removed)));
        }
    }

    /// The base vertex the layer removed whose id is `id` and whose base state is exactly
    /// `labels` and `properties` (module docs), `None` otherwise (one counter test while the
    /// layer removed no base vertex).
    pub(super) fn removed_base_vertex_like(
        &self,
        id: &str,
        labels: &[CompactString],
        properties: &PropertyMap,
    ) -> Option<VertexHandle> {
        self.delta.removed_vertices()?;
        let node = GraphStorage::get_vertex_by_id(&*self.base, id).ok()??;
        (self.delta.is_removed_vertex(node.index().slot())
            && Delta::base_vertex_state_is(&self.base, node, labels, properties))
        .then_some(node)
    }

    /// The base edge the layer removed whose id is `id`, whose endpoints are the live base
    /// vertices `from` and `to` and whose base state is `label` and `properties` (module
    /// docs), `None` otherwise.
    pub(super) fn removed_base_edge_like(
        &self,
        id: &str,
        label: Option<&str>,
        from: LayeredVertex,
        to: LayeredVertex,
        properties: &PropertyMap,
    ) -> Option<EdgeHandle> {
        self.delta.removed_edges()?;
        if from.is_delta() || to.is_delta() {
            return None;
        }
        let edge = GraphStorage::get_edge_by_id(&*self.base, id).ok()??;
        let (source, target) = GraphStorage::edge_endpoints(&*self.base, edge).ok()??;
        (self.delta.is_removed_edge(edge.index().slot())
            && source == from.to_base()
            && target == to.to_base()
            && Delta::base_edge_state_is(&self.base, edge, label, properties))
        .then_some(edge)
    }

    /// Makes the removed base vertex `node` visible again with its base state (no stub, no
    /// image; its base edges stay removed) and records it. Nothing for a vertex not removed.
    pub(super) fn restore_removed_base_vertex(
        &mut self,
        node: VertexHandle,
    ) -> Result<(), GraphError> {
        if !self.delta.is_removed_vertex(node.index().slot()) {
            return Ok(());
        }
        self.delta
            .restore_base_vertex(&self.base, node, None, &mut self.statistics)?;
        self.units.record(LayerUndo::RestoredBaseVertex { node });
        Ok(())
    }

    /// Makes the removed base edge `edge` (both endpoints live) visible again with its base
    /// state and records it. Nothing for an edge not removed.
    pub(super) fn restore_removed_base_edge(&mut self, edge: EdgeHandle) {
        if !self.delta.is_removed_edge(edge.index().slot()) {
            return;
        }
        self.delta
            .restore_base_edge(&self.base, edge, None, None, &mut self.statistics);
        self.units.record(LayerUndo::RestoredBaseEdge { edge });
    }

    /// Drops the empty bookkeeping of a layer that holds no change any more (module docs,
    /// "Fresh afterwards"): outside a unit, when some base element is dirty, no element is
    /// added, modified or removed and the schema and the catalog equal the base's. Cheap when
    /// it does nothing (counters first).
    pub(super) fn compact_if_clean(&mut self) {
        if self.units.is_recording()
            || !(self.dirty_vertices.any() || self.dirty_edges.any())
            || self.delta.change_counts() != [(0, 0); 3]
            || self.schema != *self.base.schema.read()
            || !self.catalog_changes().is_empty()
        {
            return;
        }
        let old = std::mem::take(&mut self.delta);
        self.delta = Delta::fresh_after(&self.base, old);
        self.dirty_vertices = SlotBits::new(self.base.graph.node_slots());
        self.dirty_edges = SlotBits::new(self.base.graph.edge_slots());
        self.statistics = self.base.statistics.clone();
        self.footprint.forget();
    }
}

/// Whether two maps hold the same keys in the same order, each value `identity_eq`.
fn same_map(a: &PropertyMap, b: &PropertyMap) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b.iter())
            .all(|((ka, va), (kb, vb))| ka == kb && identity_eq(va, vb))
}

impl Graph<LayeredGraph> {
    /// [`LayeredGraph::reset_layer`] under the graph's write lock: discards every change the
    /// layer made over its base (data, schema, catalog) in one unit, the base untouched. On a
    /// Store's live graph (`Store<LayeredGraph>`, a layer store) the reset is one journaled
    /// commit: a reopen recovers the reset layer. Inside a unit this thread holds on the graph
    /// it joins that unit.
    ///
    /// # Errors
    ///
    /// [`GraphError::Poisoned`] or [`GraphError::GraphViewHeld`] (the lock, as
    /// [`Graph::write_checked`]), and the commit errors of [`LayeredGraph::reset_layer`].
    pub fn reset_layer(&self) -> Result<LayerReset, GraphError> {
        self.write_checked()?.reset_layer()
    }
}
