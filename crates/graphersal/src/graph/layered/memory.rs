//! Memory figures of a [`LayeredGraph`](super::LayeredGraph) (design
//! `ai/roadmap/layered-graph.md` §7): the layer's OWN footprint, and the shared base beside it.
//!
//! - [`MemoryStats::total_bytes`] of a layer is what the layer itself holds: the delta
//!   (`Delta::memory`: added elements, stubs, images, tombstones, label structures, indexes,
//!   shapes) plus the dirty bitsets, which the layer allocates for every base slot (one bit per
//!   base vertex and base edge slot: 1/8 byte each, the only part of an untouched layer that
//!   grows with the base). Like `TraversalGraph`'s figure it leaves out the schema, the catalog,
//!   the statistics and the undo log of an open unit.
//! - [`MemoryStats::shared_bytes`] is the base's own total (its cached footprint walk: the base
//!   is frozen, so it is walked once and then answered from the cache). It is not part of
//!   the total: many layers share one base, and each must not "weigh" all of it.
//! - The memory budget of a unit (the process ceiling, `traversal/memory_budget.rs`) and the
//!   estimated start of a run measure the layer's own footprint
//!   ([`GraphStorage::memory_footprint_bytes`](crate::graph::GraphStorage::memory_footprint_bytes)),
//!   cached like `TraversalGraph`'s by the commit position and the delta's slot counts, and
//!   never walked while an open unit holds changes (`graph::memory::FootprintCache`).

use super::LayeredGraph;
use crate::graph::MemoryStats;
use crate::graph::memory::FootprintKey;

impl LayeredGraph {
    /// The layer's memory figures (`GraphStorage::memory_usage`): its own footprint, walked
    /// (O(delta), plus the dirty bitsets' size), with the shared base's total beside it. The
    /// walk is remembered as the footprint cache when no open unit holds changes.
    pub(super) fn layer_memory(&self) -> MemoryStats {
        let mut stats = self.delta.memory();
        stats.structure_bytes += self.dirty_vertices.heap_bytes() + self.dirty_edges.heap_bytes();
        stats.shared_bytes = crate::graph::memory::footprint_bytes(&self.base);
        if self.units.entries().is_empty() {
            self.footprint
                .remember(self.footprint_key(), stats.total_bytes());
        }
        stats
    }

    /// The layer's own total (`GraphStorage::memory_footprint_bytes`): the cached walk while
    /// the layer did not change since, an estimate (never a walk) while an open unit holds
    /// changes, else a walk.
    pub(super) fn layer_footprint_bytes(&self) -> u64 {
        self.footprint.bytes_or_walk(
            self.footprint_key(),
            self.units.entries().is_empty(),
            || self.layer_memory().total_bytes(),
        )
    }

    /// The cheap estimate of [`Self::layer_footprint_bytes`] (`memory_footprint_hint`).
    pub(super) fn layer_footprint_hint(&self) -> Option<u64> {
        let (vertices, edges) = self.delta.slot_counts();
        self.footprint.hint(
            vertices.saturating_add(edges),
            self.units.entries().is_empty(),
        )
    }

    /// Releases the spare capacity of the layer's own structures (`GraphStorage::
    /// shrink_to_fit`): the delta's; the dirty bitsets are exact already. The cached footprint
    /// is forgotten (outside an open unit with changes, as `TraversalGraph::shrink_to_fit`).
    pub(super) fn shrink_layer(&mut self) {
        self.delta.shrink_to_fit();
        self.dirty_vertices.shrink_to_fit();
        self.dirty_edges.shrink_to_fit();
        if self.units.entries().is_empty() {
            self.footprint.forget();
        }
    }

    /// The position the layer's own footprint is cached at: its commit position and the
    /// delta's slot counts (added elements and stubs: what the footprint grows with).
    fn footprint_key(&self) -> FootprintKey {
        let (vertices, edges) = self.delta.slot_counts();
        FootprintKey::new(self.units.last_commit_seq(), vertices, edges)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::TraversalGraph;
    use crate::graph::GraphStorage;
    use crate::value::PropertyMap;

    /// An untouched layer holds only its dirty bitsets; the base is its shared figure, outside
    /// the total.
    #[test]
    fn an_untouched_layer_weighs_its_bitsets_and_shares_the_base() {
        let base = Arc::new(TraversalGraph::tinkerpop_modern());
        let base_total = base.memory_usage().unwrap().unwrap().total_bytes();
        let layer = LayeredGraph::over(Arc::clone(&base)).unwrap();
        let stats = layer.memory_usage().unwrap().unwrap();
        assert_eq!(stats.shared_bytes, base_total);
        // One 64-bit word of dirty bits per kind (6 slots each) and the empty delta (its label
        // table's constant empty set).
        assert_eq!(stats.total_bytes(), 16 + layer.delta.memory().total_bytes());
        assert!(stats.total_bytes() < 64, "{stats:?}");
        assert_eq!(stats.property_bytes, 0);
        assert_eq!(base.memory_usage().unwrap().unwrap().shared_bytes, 0);
    }

    /// Writes grow the layer's own figure, never the base's; the footprint cache follows the
    /// walk, and a shrink releases the delta's spare capacity.
    #[test]
    fn writes_grow_the_layer_and_a_shrink_releases_spare_capacity() {
        let base = Arc::new(TraversalGraph::tinkerpop_modern());
        let mut layer = LayeredGraph::over(Arc::clone(&base)).unwrap();
        let untouched = layer.memory_usage().unwrap().unwrap();
        let marko = layer.get_vertex_by_id("1").unwrap().unwrap();
        for i in 0..100 {
            let node = layer
                .insert_vertex(None, vec!["p".into()], PropertyMap::default())
                .unwrap();
            layer
                .set_vertex_property(node, "i".into(), (i as i64).into())
                .unwrap();
            layer
                .insert_edge(None, Some("e".into()), marko, node, PropertyMap::default())
                .unwrap();
        }
        layer
            .set_vertex_property(marko, "bio".into(), "x".repeat(1000).into())
            .unwrap();
        let grown = layer.memory_usage().unwrap().unwrap();
        assert_eq!(grown.shared_bytes, untouched.shared_bytes);
        assert!(
            grown.total_bytes() > untouched.total_bytes() + 100 * 64,
            "{grown:?}"
        );
        assert!(grown.property_bytes >= 1000, "the image holds the bio");
        assert!(grown.index_bytes > 0);
        assert_eq!(
            layer.memory_footprint_bytes().unwrap(),
            Some(grown.total_bytes())
        );
        assert_eq!(
            layer.memory_footprint_hint().unwrap(),
            Some(grown.total_bytes())
        );
        layer.shrink_to_fit().unwrap();
        let shrunk = layer.memory_usage().unwrap().unwrap();
        assert!(shrunk.total_bytes() < grown.total_bytes(), "{shrunk:?}");
        layer.shrink_to_fit().unwrap();
        assert_eq!(layer.memory_usage().unwrap().unwrap(), shrunk);
        assert_eq!(layer.vertex_count().unwrap(), 106);
        assert_eq!(layer.edge_count().unwrap(), 106);
    }

    /// Tombstones, edge images and relabels count in the layer's figure.
    #[test]
    fn tombstones_and_edge_images_count() {
        let base = Arc::new(TraversalGraph::tinkerpop_modern());
        let mut layer = LayeredGraph::over(Arc::clone(&base)).unwrap();
        let before = layer.memory_usage().unwrap().unwrap().total_bytes();
        let knows = layer.get_edge_by_id("0").unwrap().unwrap();
        layer
            .set_edge_property(knows, "note".into(), "y".repeat(500).into())
            .unwrap();
        let imaged = layer.memory_usage().unwrap().unwrap().total_bytes();
        assert!(imaged >= before + 500, "{imaged} vs {before}");
        let created = layer.get_edge_by_id("1").unwrap().unwrap();
        layer.set_edge_label(created, "made".into()).unwrap();
        let relabeled = layer.memory_usage().unwrap().unwrap().total_bytes();
        assert!(relabeled > imaged);
        let ripple = layer.get_vertex_by_id("5").unwrap().unwrap();
        layer.drop_vertex(ripple).unwrap();
        let dropped = layer.memory_usage().unwrap().unwrap().total_bytes();
        assert!(dropped > relabeled, "the tombstone bitsets");
    }
}
