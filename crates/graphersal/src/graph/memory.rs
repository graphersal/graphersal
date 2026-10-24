//! Memory footprint inspection: [`MemoryStats`] and the computation behind
//! [`crate::graph::GraphStorage::memory_usage`]'s `TraversalGraph` implementation.
//!
//! This is entirely separate from traversal execution (see `.profile()`'s `bytes_materialized`
//! in `traversal/step/profile.rs` for the other, execution-scoped question). `memory_usage()`
//! takes `&self` and never touches `TraverserContext`/`ProfileContext`/any query path. The only
//! caller on the execution path is the estimated memory budget (`memory.limit` without a
//! tracking allocator), once per run and through the cache of `footprint_bytes`, which walks again
//! only after the graph changed.

/// Snapshot of a graph's approximate in-memory footprint, in bytes, broken down by subsystem.
///
/// Obtained via [`crate::graph::GraphStorage::memory_usage`]. All figures are estimates: they sum
/// `std::mem::size_of`/`size_of_val`-style arithmetic over the structures that back the graph, not
/// a true allocator-level accounting — fragmentation, allocator bookkeeping overhead, and
/// over-allocation from `Vec`/`HashMap` growth are not captured exactly, only via each structure's
/// own `.capacity()` where available.
///
/// `#[non_exhaustive]`: these three fields are, honestly, modeled on [`crate::TraversalGraph`]'s
/// specific internals (a petgraph `StableGraph` plus a `GraphIndex` plus inline `PropertyMap`
/// properties). A genuinely different [`crate::graph::GraphStorage`] backend's memory model might
/// not map onto exactly these three buckets — it may have no separate "index" at all, or an
/// entirely different structure. This is not a settled, backend-agnostic taxonomy; `#[non_exhaustive]`
/// leaves room for a future backend author to propose new fields, or a variant shape, as a
/// semver-minor addition instead of a breaking redesign, once a real second backend exists to
/// design against.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct MemoryStats {
    /// Vertex and edge storage: an estimate of the underlying petgraph `StableGraph`'s retained
    /// slot capacity (node/edge count times each slot's approximate size), **not** just
    /// `vertex_count() * size_of::<VertexData>()`.
    ///
    /// **Known, documented undercount risk:** `StableGraph` never releases a removed vertex/edge's
    /// slot — it tombstones it to keep other indices stable — so a live-count-based estimate could
    /// be arbitrarily smaller than the graph's actual retained slot count on a graph with heavy
    /// add/remove churn. This field instead uses `StableGraph::capacity()`, which reports the
    /// underlying `Vec`s' raw capacity (including tombstoned slots), specifically to avoid that
    /// undercount. It is still an approximation: petgraph's internal `Node`/`Edge` wrapper types
    /// (`petgraph::graph::Node<N>`/`Edge<E>`) carry bookkeeping beyond the payload (edge-list
    /// links, endpoint indices) which this field's per-slot size accounts for via
    /// `size_of::<petgraph::graph::Node<Option<VertexData>>>()`/`Edge<..>`, since those wrapper
    /// types are public even though their private fields aren't. [`crate::TraversalGraph::shrink_to_fit`]
    /// today only shrinks the index (see [`Self::index_bytes`]); it does nothing to reclaim
    /// `StableGraph`'s tombstoned slots, so a heavily-churned graph should not treat this field as
    /// tight — no operation currently exists to reclaim that space (a future
    /// `TraversalGraph::compact()` rebuilding the underlying graph is a plausible follow-up, not
    /// built here).
    pub structure_bytes: u64,
    /// The label index (`node_labels`/`edge_labels`) and the ID index (`nodes`/`edges`) in
    /// `GraphIndex`. The ID index stores no ids (they live once, in the element): each of its two
    /// hash tables counts its exact allocation, per bucket a handle, a cached 32-bit id hash and
    /// one control byte (13 bytes; buckets a power of two at most 7/8 full). The label index is
    /// sized via each collection's `.capacity()`, including every label bucket set, not just the
    /// outer maps; each of its maps counts both of its allocations: the entries (cached hash,
    /// key, value) and the hash index (one `usize` slot and one control byte per bucket).
    pub index_bytes: u64,
    /// Estimated heap footprint of vertex/edge properties: each element's value slice (one
    /// [`crate::ElementProperty`] per property, 32 bytes; a dropped key leaves its slot for the
    /// next added one), every stored value's own owned heap data, walked recursively via [`crate::ElementProperty::estimated_heap_bytes`] (a
    /// nested object counts its own map: entries, hash index and keys, sized from its
    /// `.capacity()`), and the key names, which live once per distinct key sequence
    /// ([`Self::shape_bytes`]).
    ///
    /// The shared texts of the `dedup` rules are included once ([`Self::dedup_table_bytes`]).
    ///
    /// **This is the O(n) part of `memory_usage()`** — it visits every vertex, every edge, and
    /// every property. Expected cost is roughly proportional to `vertex_count + edge_count` times
    /// the average property count per element; see the benchmark in `benches/graph.rs` for a
    /// measured number on the `large` (~110k element) fixture.
    pub property_bytes: u64,
    /// Strings stored compressed by compression rules (book page "Compressed Properties").
    /// `property_bytes` counts their stored (compressed) size.
    pub compressed_values: u64,
    /// The plain size of the compressed strings, in bytes.
    pub compressed_plain_bytes: u64,
    /// The stored size of the compressed strings, in bytes (without dictionaries).
    pub compressed_stored_bytes: u64,
    /// The dictionaries of the compression rules, in bytes (each shared dictionary once); part
    /// of [`Self::total_bytes`].
    pub compression_dictionary_bytes: u64,
    /// The entries of the property indexes (book page "Property Indexes"), an estimate; part
    /// of [`Self::total_bytes`]. Not part of [`Self::index_bytes`] (the id and label indexes).
    pub property_index_bytes: u64,
    /// Strings shared through the tables of `dedup` compression rules (book page "Compressed
    /// Properties", section "Deduplicating repeated values"): the references, one per stored
    /// value. A reference owns no heap; the shared text is counted once, in
    /// [`Self::dedup_table_bytes`].
    #[cfg_attr(feature = "serde", serde(default))]
    pub deduped_values: u64,
    /// The plain size of the shared strings (every reference with its text's length), in bytes.
    #[cfg_attr(feature = "serde", serde(default))]
    pub deduped_plain_bytes: u64,
    /// The tables of the `dedup` rules, in bytes: every shared text once (its bytes and the
    /// reference counts of its allocation, also an entry not swept yet) plus the tables' slots.
    /// Part of [`Self::property_bytes`].
    #[cfg_attr(feature = "serde", serde(default))]
    pub dedup_table_bytes: u64,
    /// The shared property key lists ("shapes"): every distinct key sequence of the stored
    /// property maps once (its key names, a wide shape's key index, the shared allocation) plus
    /// the shape table's own hash tables (`0` while no element has a property). Part of [`Self::property_bytes`] (like
    /// [`Self::dedup_table_bytes`]), so part of [`Self::total_bytes`]. Shapes no element uses any more are counted until the next
    /// sweep (`shrink_to_fit`, a load, or the table doubling).
    #[cfg_attr(feature = "serde", serde(default))]
    pub shape_bytes: u64,
}

impl MemoryStats {
    /// Sum of every category. This is what a caller almost always wants.
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let graph = TraversalGraph::new();
    /// let stats = graph.memory_usage().expect("TraversalGraph always supports introspection");
    /// assert_eq!(
    ///     stats.total_bytes(),
    ///     stats.structure_bytes
    ///         + stats.index_bytes
    ///         + stats.property_bytes
    ///         + stats.compression_dictionary_bytes
    ///         + stats.property_index_bytes
    /// );
    /// ```
    pub fn total_bytes(&self) -> u64 {
        self.structure_bytes
            + self.index_bytes
            + self.property_bytes
            + self.compression_dictionary_bytes
            + self.property_index_bytes
    }

    /// [`Self::total_bytes`] as mebibytes (bytes / 1_048_576.0), for human-readable reporting.
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let graph = TraversalGraph::new();
    /// let stats = graph.memory_usage().unwrap();
    /// assert_eq!(stats.total_mb(), stats.total_bytes() as f64 / 1_048_576.0);
    /// ```
    pub fn total_mb(&self) -> f64 {
        self.total_bytes() as f64 / 1_048_576.0
    }
}

/// Computes [`MemoryStats`] for a [`crate::TraversalGraph`]. Lives here, rather than inline in
/// `graph/storage.rs`'s `impl GraphStorage for TraversalGraph`, purely to keep that file's trait
/// impl list uncluttered.
///
/// `structure_bytes` uses `StableGraph::capacity()` — the underlying node/edge `Vec`s' raw
/// capacity, which includes tombstoned (removed-but-not-reclaimed) slots — rather than
/// `node_count()`/`edge_count()`, specifically to avoid undercounting a graph with heavy
/// add/remove churn; see [`MemoryStats::structure_bytes`]'s doc comment for the full reasoning.
/// Each vertex's primary label and multi-label set are counted here too (structural data, not a
/// property), while `property_bytes` walks only each element's `PropertyMap`.
pub(crate) fn compute(graph: &super::TraversalGraph) -> MemoryStats {
    use super::{EdgeData, VertexData};
    use crate::value::compact_string_heap_bytes as string_heap;

    let (node_cap, edge_cap) = graph.graph.capacity();
    let node_slot_size = std::mem::size_of::<petgraph::graph::Node<Option<VertexData>>>() as u64;
    let edge_slot_size = std::mem::size_of::<petgraph::graph::Edge<Option<EdgeData>>>() as u64;
    let mut structure_bytes = node_cap as u64 * node_slot_size + edge_cap as u64 * edge_slot_size;

    let mut property_bytes = 0u64;
    let mut compressed = CompressedCount::default();

    for (id, labels, properties, _) in graph.graph.node_weights() {
        structure_bytes += string_heap(id);
        structure_bytes +=
            labels.capacity() as u64 * std::mem::size_of::<compact_str::CompactString>() as u64;
        structure_bytes += labels.iter().map(string_heap).sum::<u64>();
        property_bytes += crate::value::estimated_element_heap_bytes(properties);
        if !graph.compression.is_empty() {
            properties.values().for_each(|value| compressed.add(value));
        }
    }

    for (id, edge_label, properties, _) in graph.graph.edge_weights() {
        structure_bytes += string_heap(id);
        structure_bytes += edge_label.as_ref().map_or(0, string_heap);
        if let Some(properties) = properties {
            property_bytes += crate::value::estimated_element_heap_bytes(properties);
            if !graph.compression.is_empty() {
                properties.values().for_each(|value| compressed.add(value));
            }
        }
    }

    // The slot generation arrays of the generational handles are structure, like the slots.
    structure_bytes += graph.node_generations.heap_bytes() + graph.edge_generations.heap_bytes();

    let index_bytes = graph.index.estimated_heap_bytes();
    // The shared texts: once, in their tables (a reference counts 0 in the walk above).
    let dedup_table_bytes = graph.compression.dedup_table_bytes();
    property_bytes += dedup_table_bytes;
    // The shared key lists: once, in the shape table (a stored map counts its values only).
    let shape_bytes = graph.shapes.heap_bytes();
    property_bytes += shape_bytes;

    MemoryStats {
        structure_bytes,
        index_bytes,
        property_bytes,
        compressed_values: compressed.values,
        compressed_plain_bytes: compressed.plain,
        compressed_stored_bytes: compressed.stored,
        compression_dictionary_bytes: graph.compression.dictionary_bytes(),
        property_index_bytes: graph.property_indexes.bytes(),
        deduped_values: compressed.deduped,
        deduped_plain_bytes: compressed.deduped_plain,
        dedup_table_bytes,
        shape_bytes,
    }
}

/// The compressed strings of a walk (only objects can hold one: rule paths name keys).
#[derive(Default)]
struct CompressedCount {
    values: u64,
    plain: u64,
    stored: u64,
    deduped: u64,
    deduped_plain: u64,
}

impl CompressedCount {
    fn add(&mut self, value: &crate::value::ElementProperty) {
        use crate::value::ElementProperty;
        match value {
            ElementProperty::Compressed(text) => {
                self.values += 1;
                self.plain += text.plain_len() as u64;
                self.stored += text.stored_len() as u64;
            }
            ElementProperty::Deduped(text) => {
                self.deduped += 1;
                self.deduped_plain += text.len() as u64;
            }
            ElementProperty::Object(map) => map.values().for_each(|value| self.add(value)),
            _ => {}
        }
    }
}

/// What one entry adds to an `IndexMap<K, V>` of the indexes, amortized: its bucket in the
/// entries (`hash + key + value`) plus its index slot (a `usize` and one control byte). Spare
/// capacity is not apportioned, so a full walk ([`compute`]) reports somewhat more.
fn index_entry_bytes<K, V>() -> u64 {
    use std::mem::size_of;
    (size_of::<(usize, K, V)>() + size_of::<usize>() + 1) as u64
}

/// What one vertex adds to [`compute`]'s figure for a freshly loaded graph (its slot, id,
/// labels, properties, id-index entry and label-bucket entries), without building the graph: a
/// merge checkpoint keeps the manifest's estimate (format spec 6.1, offset 128) current by
/// subtracting the replaced elements and adding the new ones. Like [`compute`], an estimate.
#[cfg_attr(not(feature = "persist"), allow(dead_code))]
pub(crate) fn loaded_vertex_bytes(
    id: &compact_str::CompactString,
    labels: &[compact_str::CompactString],
    properties: &crate::value::PropertyMap,
) -> u64 {
    use crate::value::compact_string_heap_bytes;
    use compact_str::CompactString;
    use std::mem::size_of;
    let slot = size_of::<petgraph::graph::Node<Option<super::VertexData>>>() as u64;
    let mut bytes = slot
        + super::index::id_entry_bytes::<super::VertexHandle>()
        + compact_string_heap_bytes(id);
    // The label `Vec` slot and the entry in the label's bucket set.
    bytes += labels.len() as u64
        * (size_of::<CompactString>() as u64 + index_entry_bytes::<super::VertexHandle, ()>());
    bytes += labels.iter().map(compact_string_heap_bytes).sum::<u64>();
    bytes + stored_properties_bytes(properties)
}

/// What a property map adds to [`compute`]'s `property_bytes` once stored: its exact value slice
/// and its values' heap (the map arrives free-standing; the graph keeps it `Shaped`, the shared
/// key list counted in `shape_bytes`, which a per-element estimate leaves out).
fn stored_properties_bytes(properties: &crate::value::PropertyMap) -> u64 {
    crate::value::property_map::shaped_values_bytes(properties.len())
        + properties
            .values()
            .map(crate::value::ElementProperty::estimated_heap_bytes)
            .sum::<u64>()
}

/// The edge counterpart of [`loaded_vertex_bytes`].
#[cfg_attr(not(feature = "persist"), allow(dead_code))]
pub(crate) fn loaded_edge_bytes(
    id: &str,
    label: Option<&str>,
    properties: Option<&crate::value::PropertyMap>,
) -> u64 {
    use compact_str::CompactString;
    use std::mem::size_of;
    let slot = size_of::<petgraph::graph::Edge<Option<super::EdgeData>>>() as u64;
    let heap = |text: &str| -> u64 {
        // A `CompactString` keeps up to its own size inline (24 bytes on 64-bit targets).
        if text.len() > size_of::<CompactString>() {
            text.len() as u64
        } else {
            0
        }
    };
    let mut bytes = slot + super::index::id_entry_bytes::<super::EdgeHandle>() + heap(id);
    if let Some(label) = label {
        bytes += heap(label) + index_entry_bytes::<super::EdgeHandle, ()>();
    }
    bytes + properties.map_or(0, stored_properties_bytes)
}

/// What a cached footprint was taken at: the commit position and the element counts. A
/// committed change advances the commit position; a direct call outside any unit (not counted)
/// that adds or removes elements changes the counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FootprintKey {
    commit_seq: u64,
    vertices: u64,
    edges: u64,
}

/// The cache of [`footprint_bytes`]: the total of the last walk and the position it was taken
/// at. Behind a mutex so that readers under a shared lock can fill it (`TraversalGraph` stays
/// `Sync`); it is locked once per traversal run, never per element.
#[derive(Debug, Default)]
pub(crate) struct FootprintCache {
    entry: parking_lot::Mutex<Option<(FootprintKey, u64)>>,
    /// How many walks [`footprint_bytes`] made (tests count them).
    #[cfg(test)]
    pub(crate) walks: std::sync::atomic::AtomicU64,
}

/// [`compute`]'s total for `graph`, walked again only when the graph changed since the cached
/// walk (the commit position or the element counts differ). A rolled-back unit restores the
/// data and leaves the commit position, so the cached figure stays valid. While an open unit
/// holds uncommitted changes the figure is walked and not cached.
pub(crate) fn footprint_bytes(graph: &super::TraversalGraph) -> u64 {
    let key = FootprintKey {
        commit_seq: graph.commit_seq(),
        vertices: graph.graph.node_count() as u64,
        edges: graph.graph.edge_count() as u64,
    };
    let cacheable = graph.undo.entries().is_empty();
    if cacheable
        && let Some((cached, bytes)) = *graph.footprint.entry.lock()
        && cached == key
    {
        return bytes;
    }
    #[cfg(test)]
    graph
        .footprint
        .walks
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let bytes = compute(graph).total_bytes();
    if cacheable {
        *graph.footprint.entry.lock() = Some((key, bytes));
    }
    bytes
}
