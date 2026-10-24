use crate::graph::{EdgeHandle, GraphType, VertexHandle};
use compact_str::CompactString;
use fnv::FnvBuildHasher;
use hashbrown::HashTable;
use indexmap::IndexSet;
pub(crate) use petgraph::graph::{EdgeIndex, NodeIndex};
use std::collections::HashMap;
use std::hash::Hasher;

/// The 32-bit hash an id index entry caches: FNV-1a over the id's bytes, folded to 32 bits.
#[inline]
fn id_hash(id: &str) -> u32 {
    let mut hasher = fnv::FnvHasher::default();
    hasher.write(id.as_bytes());
    let hash = hasher.finish();
    (hash ^ (hash >> 32)) as u32
}

/// The `u64` hash the table probes with, derived from the cached 32-bit hash alone, so growing
/// the table re-places its entries without reading a single id from the graph (no random reads
/// into the graph while a bulk load grows the table). The two copies give the table good high
/// bits (its control byte) and good low bits (its bucket).
#[inline]
fn table_hash(hash: u32) -> u64 {
    u64::from(hash) | (u64::from(hash) << 32)
}

/// A table of handles under a cached 32-bit hash of the element id; the id itself is not stored
/// here (it lives once, in the graph weight). Matching a candidate against an id is the caller's
/// job (see [`GraphIndex::get_node`]); this type only knows hashes and handles.
#[derive(Debug, Clone)]
pub(crate) struct IdTable<H> {
    table: HashTable<(H, u32)>,
}

impl<H> Default for IdTable<H> {
    fn default() -> Self {
        Self {
            table: HashTable::new(),
        }
    }
}

impl<H: Copy + Eq> IdTable<H> {
    /// Adds `handle` under `hash`. The caller guarantees that no entry for the same id exists.
    #[inline]
    fn insert(&mut self, hash: u32, handle: H) {
        self.table
            .insert_unique(table_hash(hash), (handle, hash), |entry| {
                table_hash(entry.1)
            });
    }

    /// The first entry under `hash` that `matches` accepts.
    #[inline]
    fn find(&self, hash: u32, mut matches: impl FnMut(H) -> bool) -> Option<H> {
        self.table
            .find(table_hash(hash), |entry| {
                entry.1 == hash && matches(entry.0)
            })
            .map(|entry| entry.0)
    }

    /// Removes the entry `handle` under `hash`, by handle equality alone (no id comparison, so
    /// it works after the element left the graph). Returns whether it was there.
    #[inline]
    fn remove(&mut self, hash: u32, handle: H) -> bool {
        match self.table.find_entry(table_hash(hash), |entry| {
            entry.1 == hash && entry.0 == handle
        }) {
            Ok(entry) => {
                entry.remove();
                true
            }
            Err(_) => false,
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.table.len()
    }

    #[inline]
    fn clear(&mut self) {
        self.table.clear();
    }

    #[inline]
    fn shrink_to_fit(&mut self) {
        self.table.shrink_to_fit(|entry| table_hash(entry.1));
    }

    /// The table's one allocation: the buckets of `(handle, hash)`, one control byte per bucket
    /// and the trailing group of control bytes.
    #[inline]
    fn heap_bytes(&self) -> u64 {
        self.table.allocation_size() as u64
    }
}

/// What one id index entry adds, amortized: its bucket `(handle, cached hash)` (12 bytes) plus
/// one control byte. Spare capacity (the table is at most 7/8 full) is not apportioned.
pub(crate) const fn id_entry_bytes<H>() -> u64 {
    (std::mem::size_of::<(H, u32)>() + 1) as u64
}

/// The id and label indexes of a [`crate::TraversalGraph`].
///
/// The id index (`nodes`/`edges`) stores no ids: an entry is a handle plus a cached 32-bit hash
/// of its element's id, and a lookup confirms a candidate by comparing the id in the graph
/// weight at the handle's slot. This relies on an invariant kept by the raw insert/remove
/// helpers of `graph/undo.rs`, the only writers of the id index:
///
/// - every handle in the id index points to a live slot of the graph whose weight carries the
///   handle's generation and the id the entry was hashed from;
/// - an element's id never changes after insert (nothing writes `VertexData.0` / `EdgeData.0`
///   of a stored element).
///
/// An entry is inserted after its slot's generation is patched, and removed by handle (no graph
/// read) right after its element left the graph. Cloning the index together with the graph keeps
/// it valid: a `StableGraph` clone keeps every slot. Nothing iterates the id index; it serves
/// point lookups only, so it keeps no insertion order.
///
/// Design and invariants: the book page "In-Memory Storage Internals"
/// (`book/src/for_developers/storage_internals.md`).
#[derive(Default, Debug, Clone)]
pub(crate) struct GraphIndex {
    nodes: IdTable<VertexHandle>,
    edges: IdTable<EdgeHandle>,
    // Each label's members are a set, not a multiset: a vertex/edge appears in a label's bucket
    // at most once. `IndexSet` gives O(1) average insert/`swap_remove`/contains while preserving
    // insertion order, so a large bucket's per-element removal on `drop()` no longer costs a full
    // linear scan of the bucket.
    pub(crate) node_labels:
        HashMap<CompactString, IndexSet<VertexHandle, FnvBuildHasher>, FnvBuildHasher>,
    pub(crate) edge_labels:
        HashMap<CompactString, IndexSet<EdgeHandle, FnvBuildHasher>, FnvBuildHasher>,
}

impl GraphIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Indexes the vertex `index` under `id`, the id its weight carries (see the invariant on
    /// [`GraphIndex`]). The id must be free.
    #[inline]
    pub fn insert_node(&mut self, id: impl AsRef<str>, index: VertexHandle) {
        self.nodes.insert(id_hash(id.as_ref()), index);
    }

    /// Removes the entry of the vertex `index` indexed under `id`. Reads nothing from the graph,
    /// so it may run after the vertex left it. Returns whether the entry was there.
    #[inline]
    pub fn remove_node(&mut self, id: &str, index: VertexHandle) -> bool {
        self.nodes.remove(id_hash(id), index)
    }

    /// The live vertex of `graph` (the graph this index belongs to) with the id `id`.
    #[inline]
    pub fn get_node(&self, graph: &GraphType, id: &str) -> Option<VertexHandle> {
        self.nodes.find(id_hash(id), |handle| {
            graph
                .node_weight(handle.index())
                .is_some_and(|vertex| vertex.3 == handle.generation() && vertex.0 == id)
        })
    }

    /// Whether `graph` holds a live vertex with the id `id`.
    #[inline]
    pub fn contains_node(&self, graph: &GraphType, id: &str) -> bool {
        self.get_node(graph, id).is_some()
    }

    /// Indexes the edge `index` under `id`, the id its weight carries. The id must be free.
    #[inline]
    pub fn insert_edge(&mut self, id: impl AsRef<str>, index: EdgeHandle) {
        self.edges.insert(id_hash(id.as_ref()), index);
    }

    /// The edge counterpart of [`Self::remove_node`].
    #[inline]
    pub fn remove_edge(&mut self, id: &str, index: EdgeHandle) -> bool {
        self.edges.remove(id_hash(id), index)
    }

    /// The live edge of `graph` (the graph this index belongs to) with the id `id`.
    #[inline]
    pub fn get_edge(&self, graph: &GraphType, id: &str) -> Option<EdgeHandle> {
        self.edges.find(id_hash(id), |handle| {
            graph
                .edge_weight(handle.index())
                .is_some_and(|edge| edge.3.own == handle.generation() && edge.0 == id)
        })
    }

    /// The number of indexed vertex ids and edge ids.
    #[cfg(test)]
    pub fn id_counts(&self) -> (usize, usize) {
        (self.nodes.len(), self.edges.len())
    }

    // Only exercised by the crate's own unit tests. The id lives in the graph weight; this
    // checks that the id index resolves it back to `index`.
    #[cfg(test)]
    pub fn get_edge_id_by_index<'g>(
        &self,
        graph: &'g GraphType,
        index: EdgeHandle,
    ) -> Option<&'g str> {
        let id = graph
            .edge_weight(index.index())
            .filter(|edge| edge.3.own == index.generation())?
            .0
            .as_str();
        (self.get_edge(graph, id) == Some(index)).then_some(id)
    }

    #[allow(dead_code)]
    #[inline]
    pub fn insert_node_label(&mut self, label: impl Into<CompactString>, index: VertexHandle) {
        // `IndexSet::insert` is a no-op when `index` is already a member of this label's bucket:
        // labels are a set, not a multiset, so this is the correct behavior, not a regression
        // relative to the old `Vec::push` (which could accumulate duplicate entries).
        self.node_labels
            .entry(label.into())
            .or_default()
            .insert(index);
    }

    #[allow(dead_code)]
    #[inline]
    pub fn remove_node_label(&mut self, label: &str, index: VertexHandle) {
        if let Some(set) = self.node_labels.get_mut(label) {
            // O(1) average, versus the old `Vec::retain`'s O(bucket size) full scan/shift.
            set.swap_remove(&index);
            if set.is_empty() {
                self.node_labels.remove(label);
            }
        }
    }

    /// Returns the members of `label`'s bucket, in the bucket's insertion order, or `None` if no
    /// vertex currently carries `label`. Iterator-based rather than slice-based because
    /// `IndexSet` does not expose a contiguous slice view the way `Vec` did; every caller in this
    /// crate only ever iterates the result, never needs random access.
    #[allow(dead_code)]
    #[inline]
    pub fn get_nodes_by_label(
        &self,
        label: &str,
    ) -> Option<impl Iterator<Item = VertexHandle> + '_ + use<'_>> {
        self.node_labels.get(label).map(|s| s.iter().copied())
    }

    #[allow(dead_code)]
    #[inline]
    pub fn insert_edge_label(&mut self, label: impl Into<CompactString>, index: EdgeHandle) {
        self.edge_labels
            .entry(label.into())
            .or_default()
            .insert(index);
    }

    #[allow(dead_code)]
    #[inline]
    pub fn remove_edge_label(&mut self, label: &str, index: EdgeHandle) {
        if let Some(set) = self.edge_labels.get_mut(label) {
            set.swap_remove(&index);
            if set.is_empty() {
                self.edge_labels.remove(label);
            }
        }
    }

    /// Returns the members of `label`'s bucket, in the bucket's insertion order, or `None` if no
    /// edge currently carries `label`. See [`Self::get_nodes_by_label`] for why this is
    /// iterator-based rather than slice-based.
    #[allow(dead_code)]
    #[inline]
    pub fn get_edges_by_label(
        &self,
        label: &str,
    ) -> Option<impl Iterator<Item = EdgeHandle> + '_ + use<'_>> {
        self.edge_labels.get(label).map(|s| s.iter().copied())
    }

    #[allow(dead_code)]
    #[inline]
    pub fn clear(&mut self) {
        self.nodes.clear();
        self.edges.clear();
        self.node_labels.clear();
        self.edge_labels.clear();
    }

    /// The id index alone (the two id tables' exact allocations).
    pub(crate) fn id_index_heap_bytes(&self) -> u64 {
        self.nodes.heap_bytes() + self.edges.heap_bytes()
    }

    /// Estimated heap footprint of this index's four collections, in bytes: the two id tables'
    /// exact allocations (buckets of `(handle, cached hash)` plus control bytes, see
    /// [`Self::id_index_heap_bytes`]), the label maps and each label bucket `IndexSet` sized
    /// from its own `.capacity()` ([`crate::value::property_map::index_map_heap_bytes`]), plus
    /// heap-allocated label strings.
    /// Used by [`crate::graph::MemoryStats::index_bytes`] (D1 of the memory-inspection spec);
    /// walking every bucket, not just the outer maps, is deliberate — the bucket sets are where a
    /// heavily-labeled graph's memory actually lives.
    pub(crate) fn estimated_heap_bytes(&self) -> u64 {
        use crate::value::compact_string_heap_bytes;
        use crate::value::property_map::index_map_heap_bytes;

        let mut total = self.nodes.heap_bytes();
        total += self.edges.heap_bytes();

        total += index_map_heap_bytes::<CompactString, IndexSet<VertexHandle, FnvBuildHasher>>(
            self.node_labels.capacity(),
        );
        for (label, bucket) in self.node_labels.iter() {
            total += compact_string_heap_bytes(label);
            total += index_map_heap_bytes::<VertexHandle, ()>(bucket.capacity());
        }

        total += index_map_heap_bytes::<CompactString, IndexSet<EdgeHandle, FnvBuildHasher>>(
            self.edge_labels.capacity(),
        );
        for (label, bucket) in self.edge_labels.iter() {
            total += compact_string_heap_bytes(label);
            total += index_map_heap_bytes::<EdgeHandle, ()>(bucket.capacity());
        }

        total
    }

    #[inline]
    pub fn shrink_to_fit(&mut self) {
        self.nodes.shrink_to_fit();
        self.edges.shrink_to_fit();
        self.node_labels.shrink_to_fit();
        for set in self.node_labels.values_mut() {
            set.shrink_to_fit();
        }
        self.edge_labels.shrink_to_fit();
        for set in self.edge_labels.values_mut() {
            set.shrink_to_fit();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_node_label_indexing() {
        let mut index = GraphIndex::new();
        let n0 = VertexHandle::for_test(0);
        let n1 = VertexHandle::for_test(1);
        let n2 = VertexHandle::for_test(2);

        assert!(index.get_nodes_by_label("person").is_none());

        // Insert labels
        index.insert_node_label("person", n0);
        index.insert_node_label("person", n1);
        index.insert_node_label("software", n2);

        let person_nodes: Vec<_> = index.get_nodes_by_label("person").unwrap().collect();
        assert_eq!(person_nodes, vec![n0, n1]);

        let software_nodes: Vec<_> = index.get_nodes_by_label("software").unwrap().collect();
        assert_eq!(software_nodes, vec![n2]);

        // Remove a node from label
        index.remove_node_label("person", n0);
        let person_nodes: Vec<_> = index.get_nodes_by_label("person").unwrap().collect();
        assert_eq!(person_nodes, vec![n1]);

        // Remove non-existent node doesn't alter state
        index.remove_node_label("person", n0);
        let person_nodes: Vec<_> = index.get_nodes_by_label("person").unwrap().collect();
        assert_eq!(person_nodes, vec![n1]);

        // Remove last node cleans up label entry
        index.remove_node_label("person", n1);
        assert!(index.get_nodes_by_label("person").is_none());

        // Clear cleans up all labels
        index.clear();
        assert!(index.get_nodes_by_label("software").is_none());
    }

    #[test]
    fn test_edge_label_indexing() {
        let mut index = GraphIndex::new();
        let e0 = EdgeHandle::for_test(0);
        let e1 = EdgeHandle::for_test(1);
        let e2 = EdgeHandle::for_test(2);

        assert!(index.get_edges_by_label("knows").is_none());

        // Insert labels
        index.insert_edge_label("knows", e0);
        index.insert_edge_label("knows", e1);
        index.insert_edge_label("created", e2);

        let knows_edges: Vec<_> = index.get_edges_by_label("knows").unwrap().collect();
        assert_eq!(knows_edges, vec![e0, e1]);

        let created_edges: Vec<_> = index.get_edges_by_label("created").unwrap().collect();
        assert_eq!(created_edges, vec![e2]);

        // Remove an edge from label
        index.remove_edge_label("knows", e0);
        let knows_edges: Vec<_> = index.get_edges_by_label("knows").unwrap().collect();
        assert_eq!(knows_edges, vec![e1]);

        // Remove last edge cleans up label entry
        index.remove_edge_label("knows", e1);
        assert!(index.get_edges_by_label("knows").is_none());

        // Clear cleans up all labels
        index.clear();
        assert!(index.get_edges_by_label("created").is_none());
    }

    #[test]
    fn test_shrink_to_fit() {
        let mut index = GraphIndex::new();
        for i in 0..100 {
            let n = VertexHandle::for_test(i);
            let e = EdgeHandle::for_test(i);
            index.insert_node(format!("node_{}", i), n);
            index.insert_edge(format!("edge_{}", i), e);
            index.insert_node_label("person", n);
            index.insert_edge_label("knows", e);
        }
        index.shrink_to_fit();
        assert_eq!(index.get_nodes_by_label("person").unwrap().count(), 100);
        assert_eq!(index.get_edges_by_label("knows").unwrap().count(), 100);
        // The id lookups need the graph the handles live in (the index stores no ids); these
        // `for_test` handles have none, so the entries are counted instead.
        assert_eq!(index.id_counts(), (100, 100));
    }
}

#[cfg(test)]
mod id_index_tests {
    use super::*;
    use crate::TraversalGraph;
    use crate::value::PropertyMap;
    use std::collections::HashMap;

    fn add_vertex(graph: &mut TraversalGraph, id: &str) -> VertexHandle {
        graph
            .insert_vertex(Some(id.into()), vec!["v".into()], PropertyMap::default())
            .expect("free id")
    }

    /// Two distinct ids whose cached 32-bit hashes are equal (found by search; deterministic).
    fn colliding_ids() -> (String, String) {
        let mut seen: HashMap<u32, String> = HashMap::new();
        for i in 0u64.. {
            let id = format!("c{i}");
            if let Some(other) = seen.insert(id_hash(&id), id.clone()) {
                return (other, id);
            }
        }
        unreachable!("the u32 hash space is finite")
    }

    #[test]
    fn table_hash_depends_only_on_the_cached_hash() {
        let h = id_hash("some id");
        assert_eq!(table_hash(h), table_hash(id_hash("some id")));
        assert_eq!(table_hash(h) as u32, h);
        assert_eq!((table_hash(h) >> 32) as u32, h);
    }

    #[test]
    fn entry_is_twelve_bytes() {
        assert_eq!(std::mem::size_of::<(VertexHandle, u32)>(), 12);
        assert_eq!(std::mem::size_of::<(EdgeHandle, u32)>(), 12);
        assert_eq!(id_entry_bytes::<VertexHandle>(), 13);
    }

    #[test]
    fn table_with_forced_equal_hashes_matches_and_removes_by_handle() {
        let mut table = IdTable::<VertexHandle>::default();
        let a = VertexHandle::for_test(1);
        let b = VertexHandle::for_test(2);
        table.insert(7, a);
        table.insert(7, b);
        assert_eq!(table.len(), 2);
        assert_eq!(table.find(7, |h| h == b), Some(b));
        assert_eq!(table.find(7, |h| h == a), Some(a));
        assert_eq!(table.find(8, |_| true), None);
        // Removing under another hash or an absent handle changes nothing.
        assert!(!table.remove(8, a));
        assert!(!table.remove(7, VertexHandle::for_test(3)));
        assert!(table.remove(7, a));
        assert!(!table.remove(7, a));
        assert_eq!(table.find(7, |_| true), Some(b));
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn lookup_hit_and_miss() {
        let mut graph = TraversalGraph::new();
        let a = add_vertex(&mut graph, "a");
        let b = add_vertex(&mut graph, "b");
        let e = graph
            .insert_edge(
                Some("e".into()),
                Some("knows".into()),
                a,
                b,
                PropertyMap::default(),
            )
            .expect("edge");
        assert_eq!(graph.index.get_node(&graph.graph, "a"), Some(a));
        assert_eq!(graph.index.get_node(&graph.graph, "b"), Some(b));
        assert_eq!(graph.index.get_node(&graph.graph, "c"), None);
        assert_eq!(graph.index.get_node(&graph.graph, "e"), None);
        assert_eq!(graph.index.get_edge(&graph.graph, "e"), Some(e));
        assert_eq!(graph.index.get_edge(&graph.graph, "a"), None);
        assert!(graph.index.contains_node(&graph.graph, "a"));
        assert!(!graph.index.contains_node(&graph.graph, ""));
        assert_eq!(graph.index.id_counts(), (2, 1));
    }

    #[test]
    fn colliding_ids_resolve_to_their_own_elements() {
        let (first, second) = colliding_ids();
        assert_ne!(first, second);
        assert_eq!(id_hash(&first), id_hash(&second));

        let mut graph = TraversalGraph::new();
        let v1 = add_vertex(&mut graph, &first);
        let v2 = add_vertex(&mut graph, &second);
        assert_eq!(graph.index.get_node(&graph.graph, &first), Some(v1));
        assert_eq!(graph.index.get_node(&graph.graph, &second), Some(v2));
        // A duplicate of either is refused.
        assert!(
            graph
                .insert_vertex(Some(second.as_str().into()), vec![], PropertyMap::default())
                .is_err()
        );

        let e1 = graph
            .insert_edge(
                Some(first.as_str().into()),
                None,
                v1,
                v2,
                PropertyMap::default(),
            )
            .expect("edge");
        let e2 = graph
            .insert_edge(
                Some(second.as_str().into()),
                None,
                v2,
                v1,
                PropertyMap::default(),
            )
            .expect("edge");
        assert_eq!(graph.index.get_edge(&graph.graph, &first), Some(e1));
        assert_eq!(graph.index.get_edge(&graph.graph, &second), Some(e2));

        // Removing one of the colliding pair leaves the other resolvable.
        graph.drop_edge(e1).expect("drop edge");
        assert_eq!(graph.index.get_edge(&graph.graph, &first), None);
        assert_eq!(graph.index.get_edge(&graph.graph, &second), Some(e2));
        graph.drop_vertex(v1).expect("drop vertex");
        assert_eq!(graph.index.get_node(&graph.graph, &first), None);
        assert_eq!(graph.index.get_node(&graph.graph, &second), Some(v2));
        // `e2` touched `v1`, so it went with it.
        assert_eq!(graph.index.get_edge(&graph.graph, &second), None);
        assert_eq!(graph.index.id_counts(), (1, 0));
    }

    #[test]
    fn grows_with_many_entries() {
        let mut graph = TraversalGraph::new();
        let handles: Vec<VertexHandle> = (0..50_000)
            .map(|i| add_vertex(&mut graph, &format!("v{i}")))
            .collect();
        for (i, handle) in handles.iter().enumerate() {
            assert_eq!(
                graph.index.get_node(&graph.graph, &format!("v{i}")),
                Some(*handle)
            );
        }
        assert_eq!(graph.index.get_node(&graph.graph, "v50000"), None);
        assert_eq!(graph.index.id_counts(), (50_000, 0));
        // Exact allocation: 65 536 buckets of 13 bytes plus one control group at most.
        let bytes = graph.index.nodes.heap_bytes();
        assert!(bytes >= 50_000 * 13, "{bytes}");
        assert!(bytes <= 65_536 * 13 + 64, "{bytes}");
        graph.shrink_to_fit();
        for i in (0..50_000).step_by(997) {
            assert!(graph.index.contains_node(&graph.graph, &format!("v{i}")));
        }
    }

    #[test]
    fn stale_entry_never_resolves_another_id_after_slot_reuse() {
        let mut graph = TraversalGraph::new();
        let old = add_vertex(&mut graph, "old");
        graph.drop_vertex(old).expect("drop");
        assert_eq!(graph.index.get_node(&graph.graph, "old"), None);
        assert_eq!(graph.index.id_counts(), (0, 0));
        // The freed slot is reused by the next vertex, with a new generation.
        let new = add_vertex(&mut graph, "new");
        assert_eq!(new.index(), old.index());
        assert_ne!(new, old);
        assert_eq!(graph.index.get_node(&graph.graph, "old"), None);
        assert_eq!(graph.index.get_node(&graph.graph, "new"), Some(new));
        // Re-adding the dropped id gets its own entry.
        let again = add_vertex(&mut graph, "old");
        assert_eq!(graph.index.get_node(&graph.graph, "old"), Some(again));
        assert_eq!(graph.index.id_counts(), (2, 0));
    }

    #[test]
    fn long_ids_and_clones_resolve() {
        let mut graph = TraversalGraph::new();
        let ids: Vec<String> = (0..100)
            .map(|i| format!("0192f0c4-7d2e-7a1b-9c3d-{i:012x}"))
            .collect();
        let handles: Vec<VertexHandle> = ids.iter().map(|id| add_vertex(&mut graph, id)).collect();
        let index = graph.index.clone();
        let structure = graph.graph.clone();
        for (id, handle) in ids.iter().zip(&handles) {
            assert_eq!(index.get_node(&structure, id), Some(*handle));
            assert_eq!(graph.index.get_node(&graph.graph, id), Some(*handle));
        }
    }
}
