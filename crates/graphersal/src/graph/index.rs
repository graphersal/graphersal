use crate::graph::handle::Generations;
use crate::graph::topology::{EdgeIndex, NodeIndex};
use crate::graph::{EdgeHandle, GraphType, VertexHandle};
use compact_str::CompactString;
use fnv::FnvBuildHasher;
use hashbrown::HashTable;
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

/// A table of entries `H` (the graph's id index keeps slots, `u32`) under a cached 32-bit hash
/// of the element id; the id itself is not stored here (it lives once, in the graph weight).
/// Matching a candidate against an id is the caller's job (see [`GraphIndex::get_node`]); this
/// type only knows hashes and entries.
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

/// What one id index entry `H` adds, amortized: its bucket `(H, cached hash)` (8 bytes for the
/// slot the graph keeps) plus one control byte. Spare capacity (the table is at most 7/8 full)
/// is not apportioned.
pub(crate) const fn id_entry_bytes<H>() -> u64 {
    (std::mem::size_of::<(H, u32)>() + 1) as u64
}

/// No position: the slot has no membership recorded in [`LabelIndex::positions`].
const NO_POSITION: u32 = u32::MAX;

/// The members of one label: element slots in bucket order (insertion order, a removal moves the
/// last member into the removed one's place, as `IndexSet::swap_remove` did), plus the label's
/// number, which keys its memberships in [`LabelIndex::extra`].
#[derive(Debug, Clone)]
pub(crate) struct LabelMembers {
    number: u32,
    slots: Vec<u32>,
}

impl LabelMembers {
    /// The allocated member entries.
    #[cfg(test)]
    pub(crate) fn capacity(&self) -> usize {
        self.slots.capacity()
    }
}

/// The label index of one element kind: per label the member slots ([`LabelMembers`]), and where
/// each membership sits in its label's list, so a removal is O(1) (`swap_remove`) without a hash
/// set per label.
///
/// A slot's FIRST recorded membership keeps its position in `positions` (one `u32` per slot);
/// any further membership of the same slot (a vertex with several labels) keeps its position in
/// `extra`, keyed by `(slot, label number)`. A membership is recorded in exactly one of the two,
/// and `extra` is always asked first: a slot's `positions` entry is then known to belong to the
/// label asked about whenever `extra` has no entry for it. Membership tests confirm a
/// `positions` entry against the list itself (`slots[position] == slot`), since the entry may
/// belong to another label of the slot.
///
/// The index stores slots, not handles: every member is a live element (the raw insert/remove
/// helpers of `graph/undo.rs` keep the index in step with the topology), and a scan builds each
/// handle from the slot's current generation ([`Generations::get`]). Removal is by slot and reads
/// nothing from the graph.
#[derive(Debug, Clone)]
pub(crate) struct LabelIndex {
    labels: hashbrown::HashMap<CompactString, LabelMembers, FnvBuildHasher>,
    /// Label numbers of removed (emptied) labels, reused first.
    free_numbers: Vec<u32>,
    /// The next never-used label number.
    next_number: u32,
    /// Per slot: the position of its first recorded membership, or [`NO_POSITION`].
    positions: Vec<u32>,
    /// `(slot, label number) -> position` of every further membership.
    extra: hashbrown::HashMap<(u32, u32), u32, FnvBuildHasher>,
}

impl Default for LabelIndex {
    fn default() -> Self {
        Self {
            labels: hashbrown::HashMap::with_hasher(FnvBuildHasher::default()),
            free_numbers: Vec::new(),
            next_number: 0,
            positions: Vec::new(),
            extra: hashbrown::HashMap::with_hasher(FnvBuildHasher::default()),
        }
    }
}

/// Where a membership's position is recorded.
#[derive(Clone, Copy)]
enum Record {
    Position,
    Extra,
}

impl LabelIndex {
    /// The members of `label`, `None` when no element carries it.
    #[inline]
    pub(crate) fn get(&self, label: &str) -> Option<&LabelMembers> {
        self.labels.get(label)
    }

    /// The position of `slot` in `members` and where it is recorded; `None` when `slot` is not
    /// a member.
    #[inline]
    fn locate(
        extra: &hashbrown::HashMap<(u32, u32), u32, FnvBuildHasher>,
        positions: &[u32],
        members: &LabelMembers,
        slot: u32,
    ) -> Option<(u32, Record)> {
        if !extra.is_empty()
            && let Some(&position) = extra.get(&(slot, members.number))
        {
            return Some((position, Record::Extra));
        }
        let position = *positions.get(slot as usize)?;
        (position != NO_POSITION && members.slots.get(position as usize) == Some(&slot))
            .then_some((position, Record::Position))
    }

    /// Adds `slot` to `label`; nothing when it is a member already (a label's members are a
    /// set).
    pub(crate) fn insert(&mut self, label: impl Into<CompactString>, slot: u32) {
        let label = label.into();
        let members = match self.labels.get_mut(label.as_str()) {
            Some(members) => members,
            None => {
                let number = match self.free_numbers.pop() {
                    Some(number) => number,
                    None => {
                        let number = self.next_number;
                        self.next_number = self.next_number.wrapping_add(1);
                        number
                    }
                };
                self.labels.entry(label).or_insert(LabelMembers {
                    number,
                    slots: Vec::new(),
                })
            }
        };
        if Self::locate(&self.extra, &self.positions, members, slot).is_some() {
            return;
        }
        let position = members.slots.len() as u32;
        crate::graph::topology::reserve_one(&mut members.slots);
        members.slots.push(slot);
        let index = slot as usize;
        if index >= self.positions.len() {
            if index == self.positions.len() {
                crate::graph::topology::reserve_one(&mut self.positions);
            }
            self.positions.resize(index + 1, NO_POSITION);
        }
        match self.positions.get_mut(index) {
            Some(recorded) if *recorded == NO_POSITION => *recorded = position,
            _ => {
                self.extra.insert((slot, members.number), position);
            }
        }
    }

    /// Removes `slot` from `label`; nothing when it is not a member. The last member takes the
    /// removed one's place. An emptied label is dropped.
    pub(crate) fn remove(&mut self, label: &str, slot: u32) {
        let Some(members) = self.labels.get_mut(label) else {
            return;
        };
        let Some((position, record)) = Self::locate(&self.extra, &self.positions, members, slot)
        else {
            return;
        };
        match record {
            Record::Extra => {
                self.extra.remove(&(slot, members.number));
            }
            Record::Position => {
                if let Some(recorded) = self.positions.get_mut(slot as usize) {
                    *recorded = NO_POSITION;
                }
            }
        }
        members.slots.swap_remove(position as usize);
        if let Some(&moved) = members.slots.get(position as usize) {
            // The moved member's record is found the way `locate` finds it: `extra` first.
            match self.extra.get_mut(&(moved, members.number)) {
                Some(recorded) => *recorded = position,
                None => {
                    if let Some(recorded) = self.positions.get_mut(moved as usize) {
                        *recorded = position;
                    }
                }
            }
        }
        if members.slots.is_empty() {
            let number = members.number;
            self.labels.remove(label);
            self.free_numbers.push(number);
        }
    }

    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }

    pub(crate) fn shrink_to_fit(&mut self) {
        self.labels.shrink_to_fit();
        for members in self.labels.values_mut() {
            members.slots.shrink_to_fit();
        }
        self.free_numbers.shrink_to_fit();
        self.positions.shrink_to_fit();
        self.extra.shrink_to_fit();
    }

    /// The exact heap bytes: the label table, every label's text (when not inline) and member
    /// list, the free label numbers, the positions and the further memberships.
    pub(crate) fn heap_bytes(&self) -> u64 {
        use crate::value::compact_string_heap_bytes;
        let u32_bytes = std::mem::size_of::<u32>() as u64;
        let mut total = self.labels.allocation_size() as u64;
        for (label, members) in &self.labels {
            total += compact_string_heap_bytes(label);
            total += members.slots.capacity() as u64 * u32_bytes;
        }
        total += self.free_numbers.capacity() as u64 * u32_bytes;
        total += self.positions.capacity() as u64 * u32_bytes;
        total + self.extra.allocation_size() as u64
    }
}

/// What an element with `labels` labels adds to the label index, amortized: one member entry
/// per label, its slot's position (with any label), and an `extra` entry (key, position, control
/// byte) for each label beyond the first. Spare capacity is not apportioned.
pub(crate) const fn label_memberships_bytes(labels: usize) -> u64 {
    if labels == 0 {
        return 0;
    }
    let word = std::mem::size_of::<u32>();
    let extra = std::mem::size_of::<((u32, u32), u32)>() + 1;
    (labels * word + word + (labels - 1) * extra) as u64
}

/// The id and label indexes of a [`crate::TraversalGraph`].
///
/// The id index (`nodes`/`edges`) stores no ids and no generations: an entry is a slot plus a
/// cached 32-bit hash of its element's id, and a lookup confirms a candidate by comparing the id
/// in the graph weight at the slot, whose weight also gives the handle's generation. This relies
/// on an invariant kept by the raw insert/remove helpers of `graph/undo.rs`, the only writers of
/// the id index:
///
/// - every slot in the id index is a live slot of the graph whose weight carries the id the
///   entry was hashed from (so a slot is in the index at most once);
/// - an element's id never changes after insert (nothing writes `VertexData.0` / `EdgeData.0`
///   of a stored element).
///
/// An entry is inserted after its slot's generation is patched, and removed by slot (no graph
/// read) right after its element left the graph, before the slot can be reused. Cloning the index together with the graph keeps
/// it valid: a topology clone keeps every slot. Nothing iterates the id index; it serves
/// point lookups only, so it keeps no insertion order.
///
/// The label indexes (`node_labels`/`edge_labels`, [`LabelIndex`]) keep slots; their scans take
/// the graph's slot generations to build handles.
///
/// Design and invariants: the book page "In-Memory Storage Internals"
/// (`book/src/for_developers/storage_internals.md`).
#[derive(Default, Debug, Clone)]
pub(crate) struct GraphIndex {
    nodes: IdTable<u32>,
    edges: IdTable<u32>,
    pub(crate) node_labels: LabelIndex,
    pub(crate) edge_labels: LabelIndex,
}

impl GraphIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Indexes the vertex `index` under `id`, the id its weight carries (see the invariant on
    /// [`GraphIndex`]). The id must be free.
    #[inline]
    pub fn insert_node(&mut self, id: impl AsRef<str>, index: VertexHandle) {
        self.nodes
            .insert(id_hash(id.as_ref()), index.index().index() as u32);
    }

    /// Removes the entry of the vertex `index` indexed under `id`. Reads nothing from the graph,
    /// so it may run after the vertex left it. Returns whether the entry was there.
    #[inline]
    pub fn remove_node(&mut self, id: &str, index: VertexHandle) -> bool {
        self.nodes.remove(id_hash(id), index.index().index() as u32)
    }

    /// The live vertex of `graph` (the graph this index belongs to) with the id `id`.
    #[inline]
    pub fn get_node(&self, graph: &GraphType, id: &str) -> Option<VertexHandle> {
        let mut generation = None;
        let slot = self.nodes.find(id_hash(id), |slot| {
            match graph.node_weight(NodeIndex::new(slot as usize)) {
                Some(vertex) if vertex.0 == id => {
                    generation = Some(vertex.3);
                    true
                }
                _ => false,
            }
        })?;
        Some(VertexHandle::new(
            NodeIndex::new(slot as usize),
            generation?,
        ))
    }

    /// Whether `graph` holds a live vertex with the id `id`.
    #[inline]
    pub fn contains_node(&self, graph: &GraphType, id: &str) -> bool {
        self.get_node(graph, id).is_some()
    }

    /// Indexes the edge `index` under `id`, the id its weight carries. The id must be free.
    #[inline]
    pub fn insert_edge(&mut self, id: impl AsRef<str>, index: EdgeHandle) {
        self.edges
            .insert(id_hash(id.as_ref()), index.index().index() as u32);
    }

    /// The edge counterpart of [`Self::remove_node`].
    #[inline]
    pub fn remove_edge(&mut self, id: &str, index: EdgeHandle) -> bool {
        self.edges.remove(id_hash(id), index.index().index() as u32)
    }

    /// The live edge of `graph` (the graph this index belongs to) with the id `id`.
    #[inline]
    pub fn get_edge(&self, graph: &GraphType, id: &str) -> Option<EdgeHandle> {
        let mut generation = None;
        let slot = self.edges.find(id_hash(id), |slot| {
            match graph.edge_weight(EdgeIndex::new(slot as usize)) {
                Some(edge) if edge.0 == id => {
                    generation = Some(edge.3.own);
                    true
                }
                _ => false,
            }
        })?;
        Some(EdgeHandle::new(EdgeIndex::new(slot as usize), generation?))
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

    /// Adds the live vertex `index` to `label`'s members (nothing when it is one already).
    #[inline]
    pub fn insert_node_label(&mut self, label: impl Into<CompactString>, index: VertexHandle) {
        self.node_labels.insert(label, index.index().index() as u32);
    }

    /// Removes the vertex `index` from `label`'s members, by slot (no graph read): the caller
    /// passes a live handle or one whose element just left the graph.
    #[inline]
    pub fn remove_node_label(&mut self, label: &str, index: VertexHandle) {
        self.node_labels.remove(label, index.index().index() as u32);
    }

    /// The members of `label`, in the bucket's order, as handles of the generations in
    /// `generations` (the graph's vertex slot generations), or `None` if no vertex carries
    /// `label`.
    #[inline]
    pub fn nodes_by_label<'a>(
        &'a self,
        generations: &'a Generations,
        label: &str,
    ) -> Option<impl Iterator<Item = VertexHandle> + 'a + use<'a>> {
        self.node_labels.get(label).map(move |members| {
            members.slots.iter().map(move |&slot| {
                VertexHandle::new(
                    NodeIndex::new(slot as usize),
                    generations.get(slot as usize),
                )
            })
        })
    }

    /// [`Self::nodes_by_label`] for handles of the first generation (unit tests' handles).
    #[cfg(test)]
    pub fn get_nodes_by_label(
        &self,
        label: &str,
    ) -> Option<impl Iterator<Item = VertexHandle> + '_ + use<'_>> {
        static NONE: std::sync::LazyLock<Generations> =
            std::sync::LazyLock::new(Generations::default);
        self.nodes_by_label(&NONE, label)
    }

    /// The edge counterpart of [`Self::insert_node_label`].
    #[inline]
    pub fn insert_edge_label(&mut self, label: impl Into<CompactString>, index: EdgeHandle) {
        self.edge_labels.insert(label, index.index().index() as u32);
    }

    /// The edge counterpart of [`Self::remove_node_label`].
    #[inline]
    pub fn remove_edge_label(&mut self, label: &str, index: EdgeHandle) {
        self.edge_labels.remove(label, index.index().index() as u32);
    }

    /// The edge counterpart of [`Self::nodes_by_label`].
    #[inline]
    pub fn edges_by_label<'a>(
        &'a self,
        generations: &'a Generations,
        label: &str,
    ) -> Option<impl Iterator<Item = EdgeHandle> + 'a + use<'a>> {
        self.edge_labels.get(label).map(move |members| {
            members.slots.iter().map(move |&slot| {
                EdgeHandle::new(
                    EdgeIndex::new(slot as usize),
                    generations.get(slot as usize),
                )
            })
        })
    }

    /// [`Self::edges_by_label`] for handles of the first generation (unit tests' handles).
    #[cfg(test)]
    pub fn get_edges_by_label(
        &self,
        label: &str,
    ) -> Option<impl Iterator<Item = EdgeHandle> + '_ + use<'_>> {
        static NONE: std::sync::LazyLock<Generations> =
            std::sync::LazyLock::new(Generations::default);
        self.edges_by_label(&NONE, label)
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

    /// The label indexes alone ([`LabelIndex::heap_bytes`], exact).
    pub(crate) fn label_index_heap_bytes(&self) -> u64 {
        self.node_labels.heap_bytes() + self.edge_labels.heap_bytes()
    }

    /// Heap footprint of the id and label indexes, in bytes: [`Self::id_index_heap_bytes`] plus
    /// [`Self::label_index_heap_bytes`]. Used by [`crate::graph::MemoryStats::index_bytes`].
    pub(crate) fn estimated_heap_bytes(&self) -> u64 {
        self.id_index_heap_bytes() + self.label_index_heap_bytes()
    }

    #[inline]
    pub fn shrink_to_fit(&mut self) {
        self.nodes.shrink_to_fit();
        self.edges.shrink_to_fit();
        self.node_labels.shrink_to_fit();
        self.edge_labels.shrink_to_fit();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_label_bucket_grows_by_an_eighth() {
        let mut index = GraphIndex::new();
        for i in 0..8_000 {
            index.insert_node_label("person", VertexHandle::for_test(i));
        }
        index.shrink_to_fit();
        let bucket = |index: &GraphIndex| index.node_labels.get("person").unwrap().capacity();
        let before = bucket(&index);
        index.insert_node_label("person", VertexHandle::for_test(8_000));
        let after = bucket(&index);
        // Entries grow by 1/8 (the hash table keeps its own spare room), never to the table's
        // capacity.
        assert!(after <= before + before / 8 + 1, "{before} -> {after}");
        assert!(after > before);
    }

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
    fn graph_entry_is_a_slot_and_a_hash() {
        assert_eq!(std::mem::size_of::<(u32, u32)>(), 8);
        assert_eq!(id_entry_bytes::<u32>(), 9);
    }

    #[test]
    fn lookups_take_the_generation_of_the_slot() {
        let mut graph = TraversalGraph::new();
        let first = add_vertex(&mut graph, "x");
        let a = add_vertex(&mut graph, "a");
        let b = add_vertex(&mut graph, "b");
        let e = graph
            .insert_edge(Some("e".into()), None, a, b, PropertyMap::default())
            .expect("edge");
        graph.drop_edge(e).expect("drop edge");
        graph.drop_vertex(first).expect("drop vertex");
        // Both slots are reused, under new generations.
        let again = add_vertex(&mut graph, "y");
        let f = graph
            .insert_edge(Some("f".into()), None, b, a, PropertyMap::default())
            .expect("edge");
        assert_eq!((again.index(), f.index()), (first.index(), e.index()));
        assert_eq!(graph.index.get_node(&graph.graph, "y"), Some(again));
        assert_ne!(graph.index.get_node(&graph.graph, "y"), Some(first));
        assert_eq!(graph.index.get_edge(&graph.graph, "f"), Some(f));
        assert_eq!(graph.index.get_node(&graph.graph, "x"), None);
        assert_eq!(graph.index.get_edge(&graph.graph, "e"), None);
        assert_eq!(graph.resolve_vertex(first), None);
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
        // Exact allocation: 65 536 buckets of 9 bytes (slot, cached hash, control byte) plus one
        // control group at most.
        let bytes = graph.index.nodes.heap_bytes();
        assert!(bytes >= 50_000 * 9, "{bytes}");
        assert!(bytes <= 65_536 * 9 + 64, "{bytes}");
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

#[cfg(test)]
mod label_index_tests {
    use super::*;
    use indexmap::{IndexMap, IndexSet};

    /// The reference: what the label index was before it kept slots, one `IndexSet` per label
    /// (insert = no-op for a member, removal = `swap_remove`).
    #[derive(Default)]
    struct Model(IndexMap<String, IndexSet<u32>>);

    impl Model {
        fn insert(&mut self, label: &str, slot: u32) {
            self.0.entry(label.to_string()).or_default().insert(slot);
        }
        fn remove(&mut self, label: &str, slot: u32) {
            if let Some(set) = self.0.get_mut(label) {
                set.swap_remove(&slot);
                if set.is_empty() {
                    self.0.shift_remove(label);
                }
            }
        }
    }

    fn members(index: &LabelIndex, label: &str) -> Option<Vec<u32>> {
        index.get(label).map(|members| members.slots.clone())
    }

    /// Every membership is recorded exactly once and points at its slot.
    fn check_records(index: &LabelIndex) {
        let mut recorded = 0usize;
        for (slot, &position) in index.positions.iter().enumerate() {
            if position != NO_POSITION {
                recorded += 1;
                assert!(
                    index
                        .labels
                        .values()
                        .any(|m| m.slots.get(position as usize) == Some(&(slot as u32))),
                    "slot {slot} position {position}"
                );
            }
        }
        for (&(slot, number), &position) in &index.extra {
            recorded += 1;
            let members = index
                .labels
                .values()
                .find(|m| m.number == number)
                .expect("a live label number");
            assert_eq!(members.slots.get(position as usize), Some(&slot));
        }
        let total: usize = index.labels.values().map(|m| m.slots.len()).sum();
        assert_eq!(recorded, total);
    }

    /// A small deterministic generator (no dependency).
    struct Rng(u64);
    impl Rng {
        fn next(&mut self, bound: u64) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0 % bound
        }
    }

    #[test]
    fn random_operations_keep_the_order_of_index_sets() {
        const LABELS: [&str; 5] = ["a", "b", "c", "a_label_longer_than_twenty_four_bytes", "e"];
        for seed in 1..=40u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
            let mut index = LabelIndex::default();
            let mut model = Model::default();
            for step in 0..2_000 {
                let label = LABELS[rng.next(LABELS.len() as u64) as usize];
                let slot = rng.next(64) as u32;
                if rng.next(3) == 0 {
                    index.remove(label, slot);
                    model.remove(label, slot);
                } else {
                    index.insert(label, slot);
                    model.insert(label, slot);
                }
                if step % 97 == 0 {
                    index.shrink_to_fit();
                }
                for label in LABELS {
                    let expected = model.0.get(label).map(|s| s.iter().copied().collect());
                    assert_eq!(members(&index, label), expected, "seed {seed} step {step}");
                }
            }
            check_records(&index);
            let clone = index.clone();
            for label in LABELS {
                assert_eq!(members(&clone, label), members(&index, label));
            }
        }
    }

    #[test]
    fn a_multi_label_slot_is_found_under_each_label() {
        let mut index = LabelIndex::default();
        // Slot 3 sits at position 0 of both labels: its `positions` entry belongs to "a", its
        // "b" membership is in `extra`.
        index.insert("a", 3);
        index.insert("b", 3);
        index.insert("b", 7);
        index.insert("a", 7);
        assert_eq!(index.positions[3], 0);
        // 3's "b" and 7's "a" memberships.
        assert_eq!(index.extra.len(), 2);
        // A repeated insert changes nothing.
        index.insert("b", 3);
        assert_eq!(members(&index, "b"), Some(vec![3, 7]));
        // Removing "b" moves 7 into position 0 of "b": its record there is in `extra`.
        index.remove("b", 3);
        assert_eq!(members(&index, "b"), Some(vec![7]));
        assert_eq!(members(&index, "a"), Some(vec![3, 7]));
        check_records(&index);
        index.remove("a", 3);
        assert_eq!(members(&index, "a"), Some(vec![7]));
        check_records(&index);
        // 3 left every label; it can come back.
        index.insert("b", 3);
        assert_eq!(members(&index, "b"), Some(vec![7, 3]));
        check_records(&index);
        index.remove("a", 7);
        index.remove("b", 7);
        index.remove("b", 3);
        assert!(index.get("a").is_none() && index.get("b").is_none());
        assert!(index.extra.is_empty());
        // Emptied labels give their numbers back.
        assert_eq!(index.free_numbers.len(), 2);
        index.insert("c", 1);
        assert_eq!(index.free_numbers.len(), 1);
    }

    #[test]
    fn heap_bytes_follow_the_allocations() {
        let mut index = LabelIndex::default();
        assert_eq!(index.heap_bytes(), 0);
        for slot in 0..1_000 {
            index.insert("person", slot);
        }
        index.shrink_to_fit();
        let bytes = index.heap_bytes();
        // 1000 members and 1000 positions of 4 bytes, plus the one-label table.
        assert!(bytes >= 8_000, "{bytes}");
        assert!(bytes <= 8_000 + 256, "{bytes}");
        index.insert("a_label_longer_than_twenty_four_bytes", 5);
        assert!(index.heap_bytes() > bytes + 37);
    }

    #[test]
    fn scans_take_the_generation_of_the_slot() {
        let mut graph = crate::TraversalGraph::new();
        let add = |graph: &mut crate::TraversalGraph, id: &str| {
            graph
                .insert_vertex(
                    Some(id.into()),
                    vec!["v".into(), "w".into()],
                    crate::value::PropertyMap::default(),
                )
                .expect("free id")
        };
        let old = add(&mut graph, "old");
        let keep = add(&mut graph, "keep");
        graph.drop_vertex(old).expect("drop");
        let new = add(&mut graph, "new");
        assert_eq!(new.index(), old.index());
        assert_ne!(new, old);
        for label in ["v", "w"] {
            let scanned: Vec<VertexHandle> = graph
                .index
                .nodes_by_label(&graph.node_generations, label)
                .expect("members")
                .collect();
            // `keep` moved into the dropped vertex's place; `new` was appended.
            assert_eq!(scanned, vec![keep, new], "{label}");
        }
    }
}
