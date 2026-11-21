//! Property-index overlays of a [`LayeredGraph`] (S4.1, design `ai/roadmap/layered-graph.md`
//! §8 stage 4, survey `S3-S5-survey.md` §4.1): what the layer keeps so that each property
//! index of its base can answer for the layer's data exactly, without copying the base index.
//!
//! For every index the layer serves (today: the indexes installed in the base, inherited with
//! the catalog copy) there is one overlay of three parts over ONE sorted `u32` slot space, the
//! slots of the layer's handles ([`LayeredVertex::slot`], [`LayeredEdge::slot`]):
//!
//! - **base** ([`BasePart::Shared`]): the base's own [`IndexState`], read in place through the
//!   shared `Arc`, never copied. Its slots are base slots (bit 31 clear), which are the layer's
//!   handles of base elements unchanged.
//! - **masked**: the BASE entries of the base slots the layer EXCLUDES: a base element the
//!   layer removed (its tombstone) or holds an image of. A lookup skips such a slot in the base
//!   part by the read path's own bit tests (dirty bit, then the tombstone or image bit; never a
//!   hash lookup); the masked part only keeps the counts O(log n):
//!   `count = base - masked + delta`, each from postings lengths.
//! - **delta**: the entries of the elements whose current state the delta holds, by their layer
//!   slot: an image under its base slot, an added element under bit 31 + its delta slot.
//!
//! So the visible entries are exactly `(base minus the excluded slots) plus delta`, and the
//! masked part equals the base part restricted to the excluded slots (checked by
//! [`LayeredGraph::check_layer_indexes`]). A base slot that is dirty for another reason (a stub
//! with added edges, lost base edges) is not excluded: its values are the base's.
//!
//! **Maintenance** happens in the `Delta` choke points (`delta.rs`), the functions every write
//! AND every undo goes through, so a rollback restores the overlays exactly: an added element's
//! entries go in with its insert and out with its removal; a property write takes the key's
//! entries of the element out before and puts them back after (only the indexes on that key);
//! a label change takes out the entries of the labels it lost and puts in those of the labels
//! it gained (label-less indexes untouched); a copy-up puts the base element's entries into
//! `masked` and the image's into `delta`; dropping an image (an undo of the copy-up, a return
//! to clean) takes both out again; removing a base element takes its image's entries out of
//! `delta` or, without an image, puts its base entries into `masked`. Each costs O(indexes on
//! the touched labels and key x log n); with no index at all one branch.
//!
//! The parts of all overlays are kept as two [`PropertyIndexes`] (masked, delta) with the base's
//! positions and label lookups ([`PropertyIndexes::empty_like`]), so the maintenance is the
//! base's own (`PropertyIndexes::slot_entries`, `IndexState::account`). Lookups and counts from
//! the overlays, `property_indexes()` stats and the removal of the scan fallbacks are S4.2;
//! unique checks S4.3; layer-defined indexes ([`BasePart`] owned by the layer) S4.4.

use super::LayeredGraph;
use super::handle::{LayeredEdge, LayeredVertex};
use crate::graph::property_index::{IndexKey, IndexState, PropertyIndexes};
use crate::graph::schema::ElementKind;
use crate::graph::topology::{EdgeIndex, NodeIndex};
use crate::graph::{GraphStorage, TraversalGraph};
use crate::value::PropertyMap;

/// Where the base part of an overlay comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BasePart {
    /// The base's installed index at this position (`TraversalGraph::property_indexes`): the
    /// layer serves an index inherited from the base's catalog, read in place.
    Shared(usize),
}

/// The index overlays of a layer (module docs): one per index it serves, at the same position
/// in `bases`, `masked` and `delta`.
#[derive(Debug, Default)]
pub(crate) struct LayerIndexes {
    /// The base part of each overlay.
    bases: Vec<BasePart>,
    /// The base entries of the excluded base slots, one state per overlay.
    masked: PropertyIndexes,
    /// The entries of the added elements and the images, by layer slot, one state per overlay.
    delta: PropertyIndexes,
}

/// One overlay as a reader sees it: its three parts (module docs).
#[derive(Debug, Clone, Copy)]
pub(crate) struct IndexOverlay<'a> {
    /// The base part (read in place).
    pub(crate) base: &'a IndexState,
    /// The base entries of the excluded base slots.
    pub(crate) masked: &'a IndexState,
    /// The entries of the added elements and the images, by layer slot.
    pub(crate) delta: &'a IndexState,
}

impl LayerIndexes {
    /// The overlays of a new layer over `base`: one per index the base has installed (a
    /// disabled unique index is not installed, so it stays unserved as in the base), every
    /// layer-owned part empty.
    pub(crate) fn inherit(base: &TraversalGraph) -> Self {
        let installed = &base.property_indexes;
        Self {
            bases: (0..installed.len()).map(BasePart::Shared).collect(),
            masked: installed.empty_like(),
            delta: installed.empty_like(),
        }
    }

    /// Whether the layer serves no index (every maintenance call is then one branch).
    #[inline]
    pub(crate) fn is_empty(&self) -> bool {
        self.bases.is_empty()
    }

    /// The number of overlays.
    pub(crate) fn len(&self) -> usize {
        self.bases.len()
    }

    /// The overlay at `position`, its base part read from `base` (the layer's base).
    pub(crate) fn overlay<'a>(
        &'a self,
        base: &'a TraversalGraph,
        position: usize,
    ) -> Option<IndexOverlay<'a>> {
        let base_part = match self.bases.get(position)? {
            BasePart::Shared(at) => base.property_indexes.state(*at)?,
        };
        Some(IndexOverlay {
            base: base_part,
            masked: self.masked.state(position)?,
            delta: self.delta.state(position)?,
        })
    }

    /// Puts in (`insert`) or takes out the delta entries of the element at layer `slot` with
    /// `labels` and `properties` (its current state); `only_key` limits it to one top-level
    /// key, `label_less` false leaves the label-less indexes out (a label change).
    #[inline]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn delta_entries<'l>(
        &mut self,
        element: ElementKind,
        insert: bool,
        slot: u32,
        labels: impl Iterator<Item = &'l str>,
        properties: Option<&PropertyMap>,
        only_key: Option<&str>,
        label_less: bool,
    ) {
        if self.is_empty() {
            return;
        }
        self.delta.slot_entries(
            element, insert, slot, labels, properties, only_key, label_less,
        );
    }

    /// The masked entries of the base vertex at `node` (its base state), in or out.
    pub(crate) fn masked_vertex(
        &mut self,
        base: &TraversalGraph,
        node: crate::graph::VertexHandle,
        insert: bool,
    ) {
        if self.is_empty() {
            return;
        }
        if let Some(vertex) = base.vertex_weight(node) {
            self.masked.slot_entries(
                ElementKind::Vertex,
                insert,
                node.index().slot(),
                base.labels.set(vertex.1).iter().map(|label| label.as_str()),
                Some(&vertex.2),
                None,
                true,
            );
        }
    }

    /// The masked entries of the base edge `edge` (its base state), in or out.
    pub(crate) fn masked_edge(
        &mut self,
        base: &TraversalGraph,
        edge: crate::graph::EdgeHandle,
        insert: bool,
    ) {
        if self.is_empty() {
            return;
        }
        if let Some(weight) = base.edge_weight(edge) {
            self.masked.slot_entries(
                ElementKind::Edge,
                insert,
                edge.index().slot(),
                base.labels.name_str(weight.1).into_iter(),
                weight.2.as_ref(),
                None,
                true,
            );
        }
    }

    /// The heap bytes of the layer-owned parts (masked and delta): the overlays' share of the
    /// layer's own footprint. The shared base parts belong to the base's.
    pub(crate) fn bytes(&self) -> u64 {
        self.masked.bytes() + self.delta.bytes()
    }
}

/// `true` iff the base slot `slot` of kind `element` is excluded from the base part of every
/// overlay: dirty, and removed or imaged (the read path's bit tests, in its order).
fn excluded(layer: &LayeredGraph, element: ElementKind, slot: u32) -> bool {
    match element {
        ElementKind::Vertex => {
            layer.dirty_vertices.contains(slot)
                && (layer.delta.is_removed_vertex(slot)
                    || layer.delta.imaged_vertices().contains(slot))
        }
        ElementKind::Edge => {
            layer.dirty_edges.contains(slot)
                && (layer.delta.is_removed_edge(slot) || layer.delta.is_imaged_edge(slot))
        }
    }
}

/// Every `(key, slot)` entry of `state`, in key-then-slot order.
fn pairs(state: &IndexState) -> Vec<(IndexKey, u32)> {
    state
        .postings()
        .flat_map(|(key, slots)| slots.map(move |slot| (key.clone(), slot)))
        .collect()
}

/// Whether an element with `labels` belongs to the index `state` (every element of the kind
/// for a label-less one).
fn in_index<'l>(state: &IndexState, mut labels: impl Iterator<Item = &'l str>) -> bool {
    match state.label() {
        None => true,
        Some(label) => labels.any(|held| held == label),
    }
}

/// Compares one maintained part with its rebuild: the same entries, lengths, skipped counts,
/// and the heap counted with every write equal to the walked one.
fn same_part(name: &str, part: &str, kept: &IndexState, fresh: &IndexState) -> Result<(), String> {
    if !kept.same_entries(fresh) || kept.len() != fresh.len() {
        return Err(format!(
            "index '{name}': the {part} part holds {} entries, {} after a rebuild (or other \
             values)",
            kept.len(),
            fresh.len()
        ));
    }
    if kept.skipped() != fresh.skipped() {
        return Err(format!(
            "index '{name}': the {part} part counts {} skipped values, {} after a rebuild",
            kept.skipped(),
            fresh.skipped()
        ));
    }
    if kept.heap() != kept.walked_heap() {
        return Err(format!(
            "index '{name}': the {part} part counts {} heap bytes, holds {}",
            kept.heap(),
            kept.walked_heap()
        ));
    }
    Ok(())
}

impl LayeredGraph {
    /// Compares every index overlay with a rebuild from scratch: `Err` names the first overlay
    /// that differs. A test helper (it walks the base slots the layer excludes, the delta and
    /// every visible element of each overlay's kind); the counterpart of
    /// [`TraversalGraph::check_property_indexes`].
    ///
    /// Checked per overlay:
    /// - its definition is the base index's at the same position;
    /// - `masked` equals the base entries of the excluded base slots (removed or imaged),
    ///   rebuilt from the base's data;
    /// - `delta` equals the entries of the added elements and the images under their layer
    ///   slots, rebuilt from the layer's data;
    /// - the visible entries, the base part minus the excluded slots plus `delta`, equal an
    ///   index built over every element the layer lists through its read API (what a
    ///   `TraversalGraph` with the same data holds, keyed by the layer's slots), and so do the
    ///   counts `base - masked + delta` (entries and skipped values).
    #[doc(hidden)]
    pub fn check_layer_indexes(&self) -> Result<(), String> {
        let overlays = self.delta.indexes();
        let installed = &self.base.property_indexes;
        if overlays.len() != installed.len() {
            return Err(format!(
                "{} overlays kept, the base has {} indexes installed",
                overlays.len(),
                installed.len()
            ));
        }
        for position in 0..overlays.len() {
            let Some(overlay) = overlays.overlay(&self.base, position) else {
                return Err(format!("overlay {position}: a part is missing"));
            };
            let Some(base_state) = installed.state(position) else {
                return Err(format!("overlay {position}: no base index at its position"));
            };
            let name = base_state.name();
            for (part, state) in [("masked", overlay.masked), ("delta", overlay.delta)] {
                if state.name() != name
                    || state.element() != base_state.element()
                    || state.label() != base_state.label()
                    || state.key() != base_state.key()
                    || state.transform() != base_state.transform()
                {
                    return Err(format!(
                        "index '{name}': the {part} part's definition differs"
                    ));
                }
            }
            let (fresh_masked, fresh_delta) = self.rebuild_parts(base_state);
            same_part(name, "masked", overlay.masked, &fresh_masked)?;
            same_part(name, "delta", overlay.delta, &fresh_delta)?;
            self.check_visible(name, overlay)?;
        }
        Ok(())
    }

    /// The masked and delta parts of the overlay of `base_state`, rebuilt from scratch.
    fn rebuild_parts(&self, base_state: &IndexState) -> (IndexState, IndexState) {
        let mut masked = base_state.empty_like();
        let mut delta = base_state.empty_like();
        let key = base_state.key();
        match base_state.element() {
            ElementKind::Vertex => {
                let excluded_slots = self
                    .delta
                    .removed_vertices()
                    .into_iter()
                    .flat_map(|removed| removed.ones())
                    .chain(self.delta.imaged_vertices().ones());
                for slot in excluded_slots {
                    if let Some(vertex) = self.base.graph.node_weight(NodeIndex::from_slot(slot))
                        && in_index(
                            base_state,
                            self.base.labels.set(vertex.1).iter().map(|l| l.as_str()),
                        )
                        && let Some(value) = vertex.2.get(key)
                    {
                        masked.account(true, value, slot);
                    }
                }
                let images = self.delta.imaged_vertices().ones().filter_map(|slot| {
                    let node = self.base.graph.node_weight(NodeIndex::from_slot(slot))?;
                    Some(LayeredVertex::from_base(crate::graph::VertexHandle::new(
                        NodeIndex::from_slot(slot),
                        node.3,
                    )))
                });
                for node in self.delta.vertices().chain(images) {
                    let labels = GraphStorage::vertex_labels(self, node).unwrap_or(&[]);
                    if in_index(base_state, labels.iter().map(|l| l.as_str()))
                        && let Ok(Some(map)) = GraphStorage::vertex_properties(self, node)
                        && let Some(value) = map.get(key)
                    {
                        delta.account(true, value, node.slot());
                    }
                }
            }
            ElementKind::Edge => {
                let excluded_slots = self
                    .delta
                    .removed_edges()
                    .into_iter()
                    .flat_map(|removed| removed.ones())
                    .chain(self.delta.imaged_edges().ones());
                for slot in excluded_slots {
                    if let Some(weight) = self.base.graph.edge_weight(EdgeIndex::from_slot(slot))
                        && in_index(base_state, self.base.labels.name_str(weight.1).into_iter())
                        && let Some(value) = weight.2.as_ref().and_then(|map| map.get(key))
                    {
                        masked.account(true, value, slot);
                    }
                }
                let images = self.delta.imaged_edges().ones().filter_map(|slot| {
                    let weight = self.base.graph.edge_weight(EdgeIndex::from_slot(slot))?;
                    Some(LayeredEdge::from_base(crate::graph::EdgeHandle::new(
                        EdgeIndex::from_slot(slot),
                        weight.3.own,
                    )))
                });
                for edge in self.delta.edges().chain(images) {
                    let label = GraphStorage::edge_label(self, edge).ok().flatten();
                    if in_index(base_state, label.into_iter())
                        && let Ok(Some(map)) = GraphStorage::edge_properties(self, edge)
                        && let Some(value) = map.get(key)
                    {
                        delta.account(true, value, edge.slot());
                    }
                }
            }
        }
        (masked, delta)
    }

    /// The visible entries of `overlay` (base minus the excluded slots, plus delta) against an
    /// index over every element the layer lists, and the counts `base - masked + delta`.
    fn check_visible(&self, name: &str, overlay: IndexOverlay<'_>) -> Result<(), String> {
        let element = overlay.base.element();
        let mut expected = overlay.base.empty_like();
        let key = overlay.base.key();
        match element {
            ElementKind::Vertex => {
                for node in GraphStorage::vertices(self).map_err(|err| err.to_string())? {
                    let labels = GraphStorage::vertex_labels(self, node).unwrap_or(&[]);
                    if in_index(overlay.base, labels.iter().map(|l| l.as_str()))
                        && let Ok(Some(map)) = GraphStorage::vertex_properties(self, node)
                        && let Some(value) = map.get(key)
                    {
                        expected.account(true, value, node.slot());
                    }
                }
            }
            ElementKind::Edge => {
                for edge in GraphStorage::edges(self).map_err(|err| err.to_string())? {
                    let label = GraphStorage::edge_label(self, edge).ok().flatten();
                    if in_index(overlay.base, label.into_iter())
                        && let Ok(Some(map)) = GraphStorage::edge_properties(self, edge)
                        && let Some(value) = map.get(key)
                    {
                        expected.account(true, value, edge.slot());
                    }
                }
            }
        }
        let mut visible: Vec<(IndexKey, u32)> = pairs(overlay.base)
            .into_iter()
            .filter(|(_, slot)| !excluded(self, element, *slot))
            .chain(pairs(overlay.delta))
            .collect();
        visible.sort();
        let wanted = pairs(&expected);
        if visible != wanted {
            return Err(format!(
                "index '{name}': {} visible entries (base minus excluded plus delta), {} in an \
                 index over the layer's elements (or other values)",
                visible.len(),
                wanted.len()
            ));
        }
        let base_pairs = pairs(overlay.base);
        let masked_in_base = pairs(overlay.masked).into_iter().all(|(key, slot)| {
            excluded(self, element, slot) && base_pairs.binary_search(&(key, slot)).is_ok()
        });
        if !masked_in_base {
            return Err(format!(
                "index '{name}': the masked part holds an entry the base part does not exclude"
            ));
        }
        let entries = (overlay.base.len() + overlay.delta.len()).checked_sub(overlay.masked.len());
        let skipped = (overlay.base.skipped() + overlay.delta.skipped())
            .checked_sub(overlay.masked.skipped());
        if entries != Some(expected.len()) || skipped != Some(expected.skipped()) {
            return Err(format!(
                "index '{name}': base - masked + delta counts {entries:?} entries and \
                 {skipped:?} skipped values, the layer's elements {} and {}",
                expected.len(),
                expected.skipped()
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::graph::definition::Definition;
    use crate::graph::property_index::PropertyIndexDefinition;
    use crate::graph::{GraphStorage, RollbackReason};
    use crate::value::ElementProperty;

    /// The modern graph with a vertex index on `person.age`, a label-less one on `name` and an
    /// edge index on `knows.weight`.
    fn indexed_base() -> Arc<TraversalGraph> {
        let mut base = TraversalGraph::tinkerpop_modern();
        for (name, definition) in [
            (
                "age",
                PropertyIndexDefinition::new(ElementKind::Vertex, "person", "age"),
            ),
            (
                "name",
                PropertyIndexDefinition::label_less(ElementKind::Vertex, "name"),
            ),
            (
                "weight",
                PropertyIndexDefinition::new(ElementKind::Edge, "knows", "weight"),
            ),
        ] {
            base.set_definition(Definition::property_index(name, definition))
                .unwrap();
        }
        Arc::new(base)
    }

    #[test]
    fn a_new_layer_inherits_one_empty_overlay_per_installed_index() {
        let layer = LayeredGraph::over(indexed_base()).unwrap();
        let overlays = layer.delta.indexes();
        assert_eq!(overlays.len(), 3);
        assert_eq!(overlays.bytes(), 0);
        for position in 0..3 {
            let overlay = overlays.overlay(&layer.base, position).unwrap();
            assert!(overlay.masked.is_empty() && overlay.delta.is_empty());
            assert!(!overlay.base.is_empty());
        }
        layer.check_layer_indexes().unwrap();
        assert!(
            LayeredGraph::over(Arc::new(TraversalGraph::tinkerpop_modern()))
                .unwrap()
                .delta
                .indexes()
                .is_empty()
        );
    }

    /// A copy-up masks the base entries and puts the image's into the delta part; a return to
    /// clean takes both out; a drop of a base element masks it; a rollback restores it all.
    #[test]
    fn copy_up_return_to_clean_drop_and_rollback() {
        let mut layer = LayeredGraph::over(indexed_base()).unwrap();
        let marko = layer.get_vertex_by_id("1").unwrap().unwrap();
        layer
            .set_vertex_property(marko, "age".into(), 30.into())
            .unwrap();
        layer.check_layer_indexes().unwrap();
        let age = layer.delta.indexes().overlay(&layer.base, 0).unwrap();
        assert_eq!((age.masked.len(), age.delta.len()), (1, 1));
        // The label-less `name` index masks marko's base name and holds the image's.
        let name = layer.delta.indexes().overlay(&layer.base, 1).unwrap();
        assert_eq!((name.masked.len(), name.delta.len()), (1, 1));
        layer
            .set_vertex_property(marko, "age".into(), 29.into())
            .unwrap();
        layer.check_layer_indexes().unwrap();
        assert_eq!(layer.delta.indexes().bytes(), 0, "back to clean");

        let mark = layer.begin_unit().unwrap();
        layer
            .set_vertex_property(marko, "age".into(), ElementProperty::from("old"))
            .unwrap();
        layer.remove_vertex_label(marko, "person").unwrap();
        layer.check_layer_indexes().unwrap();
        let knows = layer.get_edge_by_id("2").unwrap().unwrap();
        layer
            .set_edge_property(knows, "weight".into(), 0.25.into())
            .unwrap();
        layer.drop_vertex(marko).unwrap();
        layer.check_layer_indexes().unwrap();
        let added = layer
            .insert_vertex(None, vec!["person".into()], PropertyMap::default())
            .unwrap();
        layer
            .set_vertex_property(added, "age".into(), 40.into())
            .unwrap();
        layer.check_layer_indexes().unwrap();
        layer
            .rollback_unit(mark, &RollbackReason::failed(&"test"))
            .unwrap();
        layer.check_layer_indexes().unwrap();
        assert_eq!(layer.delta.indexes().bytes(), 0);
    }

    /// The overlays' bytes are the layer's own (in its total, never in the shared figure).
    #[test]
    fn overlay_bytes_count_in_the_layers_own_footprint() {
        let mut layer = LayeredGraph::over(indexed_base()).unwrap();
        let before = layer.memory_usage().unwrap().unwrap();
        assert_eq!(before.property_index_bytes, 0);
        let vadas = layer.get_vertex_by_id("2").unwrap().unwrap();
        layer
            .set_vertex_property(vadas, "age".into(), 28.into())
            .unwrap();
        let after = layer.memory_usage().unwrap().unwrap();
        assert_eq!(
            after.property_index_bytes,
            layer.delta.indexes().bytes(),
            "{after:?}"
        );
        assert!(after.property_index_bytes > 0);
        assert_eq!(after.shared_bytes, before.shared_bytes);
    }
}
