//! `impl PersistentStorage for LayeredGraph` (feature `persist`, S3.1): what the persistence
//! layer drives a layer through (design `ai/roadmap/layered-graph.md` §5, survey
//! `S3-S5-survey.md` §3.1).
//!
//! - **Position:** the layer's own lineage id (created on first use, not the base's), commit
//!   position and auto-id sequences (they start at the base's, so an auto id never names a base
//!   element; restored with `raise_id_sequences`, raise only).
//! - **Load sink:** a layer is loaded over its base, which it never writes: a fresh
//!   [`LayeredGraph::over`] the same base takes [`BasePatch`] entries (tombstones, images and the
//!   base definitions the layer removed, all by id: slots are not stable across loads of the
//!   same base, ids are), then its schema, catalog and added elements through the ordinary
//!   sink. The patch and the elements go through the layer's own write paths with the write
//!   rules skipped and nothing recorded, so the statistics, the label lookups and the tombstone
//!   and image bookkeeping are what the live layer had; [`PersistentStorage::finish_load`]
//!   rebuilds the property-index overlays from the catalog and the data.
//! - **Encoder side:** [`PersistentStorage::layer_parts`] lists the patch (removed base edges,
//!   removed base vertices, vertex images, edge images, removed base definitions) and the added
//!   elements in delta slot order.
//! - **Replay:** a journal's change sets (identical to `TraversalGraph`'s, S1.9) go through the
//!   layer's write methods outside units: no hook, no rule pass (a change set carries its derived
//!   vectors), the schema and catalog changes stored without opening a unit of their own.
//! - **`clear`:** an empty layer over the same base (the base's schema and catalog copies),
//!   keeping the registered hooks and the journal slot.
//!
//! Order classes (plan P4, as a `TraversalGraph` reload compacts its slots): after a load, a
//! base vertex that gained a label is listed in that label's bucket before the added vertices
//! (live: in write order), and the adjacency lists of added edges follow their load order.

use std::sync::Arc;

use compact_str::CompactString;

use super::LayeredGraph;
use super::delta::PropertyChange;
use super::handle::{LayeredEdge, LayeredVertex};
use crate::error::{DefinitionError, GraphError};
use crate::graph::change::{ChangeSet, Mutation};
use crate::graph::definition::Definition;
use crate::graph::hooks::JournalSlot;
use crate::graph::persistent::{BasePatch, LayerParts, PersistentStorage};
use crate::graph::replay;
use crate::graph::schema::GraphSchema;
use crate::graph::{EdgeHandle, GraphStorage, VertexHandle};
use crate::value::PropertyMap;
use crate::value::identity::identity_eq;

/// A key no property map holds: what [`LayeredGraph::patch_edge_properties`] sets and drops
/// again to give an edge image an empty map (the one state the property writes cannot
/// otherwise reach from a base edge without a map).
const EMPTY_MAP_KEY: &str = "\u{0}graphersal-empty-map";

/// Whether two maps hold the same keys in the same order, each value `identity_eq`.
fn same_map(a: &PropertyMap, b: &PropertyMap) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b.iter())
            .all(|((ka, va), (kb, vb))| ka == kb && identity_eq(va, vb))
}

impl PersistentStorage for LayeredGraph {
    fn graph_id(&self) -> u128 {
        *self.graph_id.get_or_init(crate::value::uuid::new_v7)
    }

    fn set_graph_id(&mut self, graph_id: u128) -> Result<(), GraphError> {
        self.graph_id = Box::new(std::sync::OnceLock::from(graph_id));
        Ok(())
    }

    fn last_committed_at(&self) -> i64 {
        self.units.last_committed_at()
    }

    /// The layer's own sequences (they started at the base's when the layer was created).
    fn id_sequences(&self) -> (u64, u64) {
        (self.node_sequence, self.edge_sequence)
    }

    fn raise_position(&mut self, commit_seq: u64, committed_at: i64) -> Result<(), GraphError> {
        self.units.raise_position(commit_seq, committed_at);
        Ok(())
    }

    /// Stores an added vertex as it is (no write rule, nothing recorded, no sequence change);
    /// [`GraphError::VertexIdUniqueConstraint`] when a live base or added vertex has `id`.
    fn load_vertex(
        &mut self,
        id: CompactString,
        labels: Vec<CompactString>,
        properties: PropertyMap,
    ) -> Result<LayeredVertex, GraphError> {
        if GraphStorage::get_vertex_by_id(&*self, &id)?.is_some() {
            return Err(GraphError::duplicate_vertex_id(id.as_str()));
        }
        self.delta
            .insert_vertex(id, labels, properties, &mut self.statistics)
    }

    /// Stores an added edge as it is; its endpoints are live base or added vertices, named by
    /// id (a base endpoint gets its stub and its dirty bit, as a live write does).
    fn load_edge(
        &mut self,
        id: CompactString,
        label: Option<CompactString>,
        from_id: &str,
        to_id: &str,
        properties: PropertyMap,
    ) -> Result<LayeredEdge, GraphError> {
        if GraphStorage::get_edge_by_id(&*self, &id)?.is_some() {
            return Err(GraphError::duplicate_edge_id(id.as_str()));
        }
        let endpoint = |layer: &Self, id: &str| {
            GraphStorage::get_vertex_by_id(layer, id)?
                .ok_or_else(|| GraphError::VertexNotFound { id: id.to_string() })
        };
        let from = endpoint(self, from_id)?;
        let to = endpoint(self, to_id)?;
        let from = self.edge_endpoint(from)?;
        let to = self.edge_endpoint(to)?;
        self.delta
            .insert_edge(id, label, properties, from, to, &mut self.statistics)
    }

    /// Replaces the layer's schema without checking the data (the vector rules follow it).
    fn load_schema(&mut self, schema: GraphSchema) -> Result<(), GraphError> {
        self.schema = schema;
        self.refresh_vector_rules();
        Ok(())
    }

    /// Stores a definition in the layer's catalog as it is (any kind: the catalog copied from
    /// the base may hold kinds the layer refuses to write); the index overlays are built by
    /// `finish_load`.
    fn load_definition(&mut self, definition: Definition) -> Result<(), GraphError> {
        self.catalog.insert(definition);
        Ok(())
    }

    /// Rebuilds the property-index overlays from the catalog and the data (the overlays are
    /// never persisted, like `TraversalGraph`'s index entries), then releases spare capacity.
    fn finish_load(&mut self) -> Result<(), GraphError> {
        self.rebuild_layer_indexes();
        self.shrink_layer();
        Ok(())
    }

    /// An empty layer over the same base (the base's schema and catalog copies, statistics and
    /// sequences), with a new lineage id, keeping the registered hooks and the journal slot.
    fn clear(&mut self) -> Result<(), GraphError> {
        let hooks = std::mem::take(&mut self.units.hooks);
        *self = Self::new_unchecked(Arc::clone(&self.base));
        self.units.hooks = hooks;
        Ok(())
    }

    fn attach_journal(&mut self, journal: JournalSlot) -> Result<(), GraphError> {
        if self.units.is_recording() {
            return Err(GraphError::Unsupported {
                operation: "attaching a journal while a unit is open",
            });
        }
        self.units.hooks.set_journal(journal)
    }

    /// Every mutation through the layer's write methods outside units (nothing recorded, no
    /// hook, no vector-rule pass: the change set carries its derived vectors); a schema change
    /// and a stored definition as the load sink stores them, a removed definition taken out of
    /// the catalog (neither opens a unit of its own); then the sequences and the position
    /// raised. With `verify` every mutation's before image is compared first.
    fn replay(&mut self, changes: &ChangeSet, verify: bool) -> Result<(), String> {
        let applied = changes.mutations().iter().try_for_each(|mutation| {
            if verify && let Some(reason) = replay::before_mismatch(&*self, mutation, true) {
                return Err(format!("{}: {reason}", replay::mutation_subject(mutation)));
            }
            self.replay_mutation(mutation)
                .map_err(|err| format!("{}: {err}", replay::mutation_subject(mutation)))
        });
        // The writes changed the delta without moving the commit position.
        self.footprint.forget();
        applied?;
        GraphStorage::raise_id_sequences(self, changes.node_sequence(), changes.edge_sequence())
            .map_err(|err| err.to_string())?;
        self.units
            .raise_position(changes.commit_seq(), changes.committed_at());
        Ok(())
    }

    fn load_base_patch(&mut self, patch: BasePatch) -> Result<(), GraphError> {
        match patch {
            BasePatch::RemovedVertex { id } => {
                if let Some(node) = self.patch_base_vertex(&id)? {
                    self.drop_base_vertex(node)?;
                }
            }
            BasePatch::RemovedEdge { id } => {
                if let Some(edge) = self.patch_base_edge(&id)? {
                    self.drop_base_edge(edge);
                }
            }
            BasePatch::VertexImage {
                id,
                labels,
                properties,
            } => {
                let node = self
                    .patch_base_vertex(&id)?
                    .ok_or_else(GraphError::vertex_removed)?;
                if GraphStorage::vertex_labels(&*self, node)? != labels.as_slice() {
                    self.store_vertex_labels(node, labels)?;
                }
                self.patch_vertex_properties(node, properties)?;
            }
            BasePatch::EdgeImage {
                id,
                label,
                properties,
            } => {
                let edge = self
                    .patch_base_edge(&id)?
                    .ok_or_else(GraphError::edge_removed)?;
                self.patch_edge_label(edge, label)?;
                if let Some(properties) = properties {
                    self.patch_edge_properties(edge, properties)?;
                }
            }
            BasePatch::RemovedDefinition { kind, name } => {
                self.catalog.remove(kind, &name);
            }
        }
        Ok(())
    }

    /// The patch (removed base edges, removed base vertices, vertex images, edge images in the
    /// order of the relabeled-edge lists then by slot, removed base definitions) and the added
    /// elements in delta slot order (stubs are not vertices).
    fn layer_parts(
        &self,
    ) -> Result<Option<LayerParts<'_, LayeredVertex, LayeredEdge>>, GraphError> {
        let mut patch = Vec::new();
        if let Some(removed) = self.delta.removed_edges() {
            for slot in removed.ones() {
                if let Some(edge) = self.base_edge_at(slot)
                    && let Some(weight) = self.base.edge_weight(edge)
                {
                    patch.push(BasePatch::removed_edge(weight.0.clone()));
                }
            }
        }
        if let Some(removed) = self.delta.removed_vertices() {
            for slot in removed.ones() {
                if let Some(node) = self.base_vertex_at(slot)
                    && let Some(vertex) = self.base.vertex_weight(node)
                {
                    patch.push(BasePatch::removed_vertex(vertex.0.clone()));
                }
            }
        }
        for slot in self.delta.imaged_vertices().ones() {
            let Some(node) = self.base_vertex_at(slot).map(LayeredVertex::from_base) else {
                continue;
            };
            let (Some(id), Some(properties)) = (
                GraphStorage::vertex_id(self, node)?,
                GraphStorage::vertex_properties(self, node)?,
            ) else {
                continue;
            };
            patch.push(BasePatch::vertex_image(
                id,
                GraphStorage::vertex_labels(self, node)?.to_vec(),
                properties.clone(),
            ));
        }
        // A base edge relabeled to an unlabeled one is in no list: it comes by slot.
        let mut edges: Vec<EdgeHandle> = self.delta.gained_edges_in_order().collect();
        let gained: std::collections::HashSet<u32> =
            edges.iter().map(|edge| edge.index().slot()).collect();
        for slot in self.delta.imaged_edges().ones() {
            if !gained.contains(&slot)
                && let Some(edge) = self.base_edge_at(slot)
            {
                edges.push(edge);
            }
        }
        for edge in edges {
            let edge = LayeredEdge::from_base(edge);
            let Some(id) = GraphStorage::edge_id(self, edge)? else {
                continue;
            };
            patch.push(BasePatch::edge_image(
                id,
                GraphStorage::edge_label(self, edge)?.map(CompactString::from),
                GraphStorage::edge_properties(self, edge)?.cloned(),
            ));
        }
        for definition in self.base.definitions.iter() {
            if self
                .catalog
                .get(definition.kind(), definition.name())
                .is_none()
            {
                patch.push(BasePatch::removed_definition(
                    definition.kind(),
                    definition.name(),
                ));
            }
        }
        Ok(Some(LayerParts::new(
            patch,
            Box::new(self.delta.vertices()),
            Box::new(self.delta.edges()),
        )))
    }
}

impl LayeredGraph {
    /// One mutation of [`PersistentStorage::replay`].
    fn replay_mutation(&mut self, mutation: &Mutation) -> Result<(), GraphError> {
        match mutation {
            Mutation::SetSchema { after, .. } => self.load_schema((**after).clone()),
            Mutation::SetDefinition {
                after: Some(after), ..
            } => {
                let kind = after.kind();
                let name = CompactString::from(after.name());
                self.catalog.insert((**after).clone());
                if kind.is_property_index() {
                    self.layer_index_after_change(&name);
                }
                Ok(())
            }
            Mutation::SetDefinition {
                kind,
                name,
                after: None,
                ..
            } => {
                if self.catalog.remove(*kind, name).is_none() {
                    return Err(DefinitionError::NotFound {
                        kind: kind.to_string(),
                        name: name.clone(),
                    }
                    .into());
                }
                if kind.is_property_index() {
                    self.layer_index_after_change(name);
                }
                Ok(())
            }
            Mutation::AddVertex { .. }
            | Mutation::DropVertex { .. }
            | Mutation::AddEdge { .. }
            | Mutation::DropEdge { .. }
            | Mutation::SetProperty { .. }
            | Mutation::RemoveProperty { .. }
            | Mutation::AddLabel { .. }
            | Mutation::RemoveLabel { .. }
            | Mutation::SetEdgeLabel { .. } => replay::apply_mutation(self, mutation),
        }
    }

    /// The base vertex `id` as the layer's handle: `None` when the layer removed it;
    /// [`GraphError::VertexNotFound`] when the base holds no vertex `id`.
    fn patch_base_vertex(&self, id: &str) -> Result<Option<LayeredVertex>, GraphError> {
        let node: VertexHandle = GraphStorage::get_vertex_by_id(&*self.base, id)?
            .ok_or_else(|| GraphError::VertexNotFound { id: id.to_string() })?;
        Ok(self
            .base_vertex_live(node)
            .then(|| LayeredVertex::from_base(node)))
    }

    /// The edge counterpart of [`Self::patch_base_vertex`].
    fn patch_base_edge(&self, id: &str) -> Result<Option<LayeredEdge>, GraphError> {
        let edge: EdgeHandle = GraphStorage::get_edge_by_id(&*self.base, id)?
            .ok_or_else(|| GraphError::EdgeNotFound { id: id.to_string() })?;
        Ok(self
            .base_edge_live(edge)
            .then(|| LayeredEdge::from_base(edge)))
    }

    /// Gives the live base vertex `node` exactly `properties`, in order, through the layer's
    /// property writes (copy-up on the first change, return to clean when it equals the base
    /// again): every current key dropped, then every key of `properties` set (so the image's
    /// order is the patch's).
    fn patch_vertex_properties(
        &mut self,
        node: LayeredVertex,
        properties: PropertyMap,
    ) -> Result<(), GraphError> {
        let keys: Vec<CompactString> = match GraphStorage::vertex_properties(&*self, node)? {
            Some(current) if same_map(current, &properties) => return Ok(()),
            Some(current) => current.keys().cloned().collect(),
            None => Vec::new(),
        };
        for key in keys {
            self.change_vertex_property(node, PropertyChange::Drop(key))?;
        }
        for (key, value) in properties {
            self.change_vertex_property(node, PropertyChange::Set(key, value))?;
        }
        Ok(())
    }

    /// The edge counterpart of [`Self::patch_vertex_properties`]; an empty map on an edge
    /// whose base edge has none is reached by setting and dropping a key no map holds.
    fn patch_edge_properties(
        &mut self,
        edge: LayeredEdge,
        properties: PropertyMap,
    ) -> Result<(), GraphError> {
        let keys: Vec<CompactString> = match GraphStorage::edge_properties(&*self, edge)? {
            Some(current) if same_map(current, &properties) => return Ok(()),
            Some(current) => current.keys().cloned().collect(),
            None if properties.is_empty() => {
                let key = CompactString::from(EMPTY_MAP_KEY);
                self.change_edge_property(edge, PropertyChange::Set(key.clone(), false.into()))?;
                self.change_edge_property(edge, PropertyChange::Drop(key))?;
                return Ok(());
            }
            None => Vec::new(),
        };
        for key in keys {
            self.change_edge_property(edge, PropertyChange::Drop(key))?;
        }
        for (key, value) in properties {
            self.change_edge_property(edge, PropertyChange::Set(key, value))?;
        }
        Ok(())
    }

    /// Gives the live base edge `edge` the label `label` (no write rule, nothing recorded), as
    /// `GraphStorage::set_edge_label` stores it: in its image, joining the end of the list of
    /// base edges relabeled to it; a return to clean checked when it had an image.
    fn patch_edge_label(
        &mut self,
        edge: LayeredEdge,
        label: Option<CompactString>,
    ) -> Result<(), GraphError> {
        if GraphStorage::edge_label(&*self, edge)? == label.as_deref() {
            return Ok(());
        }
        self.dirty_edges.set(edge.slot());
        let base = edge.to_base();
        let (had_image, _, _) =
            self.delta
                .set_base_edge_label(&self.base, base, label, None, &mut self.statistics)?;
        if had_image {
            self.delta.settle_base_edge(&self.base, base);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;
    use crate::graph::TraversalGraph;
    use crate::persist::{Durability, Journal, JournalRecord, SharedBuffer, read_journal};

    /// A real journal in the layer's journal slot (what a layer Store will attach, S3.3):
    /// every commit of the layer is written as a WAL record; read back and replayed into a
    /// fresh layer over the same base, it gives the same layer. A second journal is refused.
    #[test]
    fn a_journal_in_the_slot_records_what_a_replay_restores() {
        let base = Arc::new(TraversalGraph::tinkerpop_modern());
        let mut layer = LayeredGraph::over(Arc::clone(&base)).unwrap();
        let mut buffer = SharedBuffer::new();
        let header = crate::persist::WalHeader {
            graph_id: layer.graph_id(),
            first_commit_seq: 1,
            created_at: 0,
        }
        .encode();
        buffer.write_all(&header).unwrap();
        let (journal, _) =
            Journal::new(buffer.clone(), Durability::Os).continuing(0, 0, header.len() as u64);
        layer
            .attach_journal(JournalSlot::new(Box::new(journal)))
            .unwrap();
        let (second, _) = Journal::new(SharedBuffer::new(), Durability::Os).continuing(0, 0, 0);
        assert!(
            layer
                .attach_journal(JournalSlot::new(Box::new(second)))
                .is_err()
        );
        let marko = GraphStorage::get_vertex_by_id(&layer, "1")
            .unwrap()
            .unwrap();
        let josh = GraphStorage::get_vertex_by_id(&layer, "4")
            .unwrap()
            .unwrap();
        let mark = GraphStorage::begin_unit(&mut layer).unwrap();
        GraphStorage::set_vertex_property(&mut layer, marko, "age".into(), 30.into()).unwrap();
        let new = GraphStorage::insert_vertex(&mut layer, None, vec!["person".into()], {
            let mut map = PropertyMap::default();
            map.insert("name", "ada".into());
            map
        })
        .unwrap();
        GraphStorage::insert_edge(
            &mut layer,
            None,
            Some("knows".into()),
            new,
            marko,
            PropertyMap::default(),
        )
        .unwrap();
        GraphStorage::commit_unit(&mut layer, mark).unwrap();
        let mark = GraphStorage::begin_unit(&mut layer).unwrap();
        GraphStorage::drop_vertex(&mut layer, josh).unwrap();
        GraphStorage::commit_unit(&mut layer, mark).unwrap();
        layer.mark("after-drop").unwrap();

        let bytes = buffer.contents();
        let reader = read_journal(&bytes[..]).unwrap();
        assert_eq!(reader.graph_id(), layer.graph_id());
        let mut replayed = LayeredGraph::over(base).unwrap();
        let mut commits = 0;
        for record in reader {
            if let JournalRecord::Commit(changes) = record.unwrap() {
                PersistentStorage::replay(&mut replayed, &changes, true).unwrap();
                commits += 1;
            }
        }
        assert_eq!(commits, 2);
        let diff = |l: &LayeredGraph| {
            format!(
                "{:?}",
                GraphStorage::layer_diff(l, &crate::graph::LayerDiffRequest::new()).unwrap()
            )
        };
        assert_eq!(diff(&replayed), diff(&layer));
        assert_eq!(replayed.statistics, layer.statistics);
        assert_eq!(replayed.id_sequences(), layer.id_sequences());
        assert_eq!(
            GraphStorage::last_commit_seq(&replayed).unwrap(),
            GraphStorage::last_commit_seq(&layer).unwrap()
        );
        assert_eq!(
            PersistentStorage::last_committed_at(&replayed),
            PersistentStorage::last_committed_at(&layer)
        );
        // The journal is kept by `clear` (the slot stays taken).
        layer.clear().unwrap();
        let (third, _) = Journal::new(SharedBuffer::new(), Durability::Os).continuing(0, 0, 0);
        assert!(
            layer
                .attach_journal(JournalSlot::new(Box::new(third)))
                .is_err()
        );
    }
}
