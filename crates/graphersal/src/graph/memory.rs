//! Memory footprint inspection: [`MemoryStats`] and the computation behind
//! [`crate::graph::GraphStorage::memory_usage`]'s `TraversalGraph` implementation.
//!
//! This is entirely separate from traversal execution (see `.profile()`'s `bytes_materialized`
//! in `traversal/step/profile.rs` for the other, execution-scoped question). `memory_usage()`
//! takes `&self` and never touches `TraverserContext`/`ProfileContext`/any query path. The only
//! callers on the execution path are the estimated memory checks (the process memory ceiling
//! `memory.limit` without a tracking allocator): once per run they take `footprint_hint` (the
//! last walk adjusted by the element counts, no walk) and, only near the ceiling, before the
//! first walk or (outside an open unit with changes) once the element count doubled or halved,
//! `footprint_bytes` (cached; walks again only after the graph changed). Never a walk per check,
//! and never one inside an open unit with uncommitted changes: there both are the last walk
//! extrapolated by the element counts (see [`footprint_bytes`]).

/// Snapshot of a graph's approximate in-memory footprint, in bytes, broken down by subsystem.
///
/// Obtained via [`crate::graph::GraphStorage::memory_usage`]. All figures are estimates: they sum
/// `std::mem::size_of`/`size_of_val`-style arithmetic over the structures that back the graph, not
/// a true allocator-level accounting — fragmentation, allocator bookkeeping overhead, and
/// over-allocation from `Vec`/`HashMap` growth are not captured exactly, only via each structure's
/// own `.capacity()` where available.
///
/// `#[non_exhaustive]`: these three fields are, honestly, modeled on [`crate::TraversalGraph`]'s
/// specific internals (a slot topology plus a `GraphIndex` plus inline `PropertyMap`
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
    /// Vertex and edge storage: the slot vectors of the graph's topology (their allocated
    /// capacity times the slot size, so vacant slots of removed elements and spare capacity
    /// count), plus each element's id and label heap and the handle generation arrays.
    ///
    /// A removed vertex or edge leaves its slot vacant (other indices stay stable) until a later
    /// insertion reuses it; [`crate::TraversalGraph::shrink_to_fit`] (also run at the end of
    /// every load) releases the spare capacity above the last slot, not vacant slots. The
    /// memory report splits the slots into live, vacant (`tombstoned`) and spare.
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
    /// The graph's [workload statistics](crate::workload) (book page "Workload Statistics"),
    /// held beside the storage: filled in by the graph that holds them (`g.memory_usage()`, the
    /// front ends' statistics), `0` from a storage alone and for a graph without statistics.
    /// NOT part of [`Self::total_bytes`], which is the graph's own data.
    #[cfg_attr(feature = "serde", serde(default))]
    pub workload_bytes: u64,
    /// The embedding models of the graph's catalog that are loaded (book page "Embedding
    /// Models in a Store"): what each loaded model takes in memory (counted at its load when a
    /// tracking allocator is installed, else the loader's estimate, which leaves out most of
    /// a tokenizer's in-memory form), a model shared by several positions counted once per
    /// graph that holds it. Filled in by the graph (`g.memory_usage()`,
    /// the front ends' statistics), `0` from a storage alone. NOT part of
    /// [`Self::total_bytes`], which is the graph's own data; the process ceiling counts it.
    #[cfg_attr(feature = "serde", serde(default))]
    pub model_bytes: u64,
}

impl MemoryStats {
    /// Sum of every category. This is what a caller almost always wants.
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let graph = TraversalGraph::new();
    /// let stats = graph.memory_usage().unwrap().expect("TraversalGraph always supports introspection");
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
    /// let stats = graph.memory_usage().unwrap().unwrap();
    /// assert_eq!(stats.total_mb(), stats.total_bytes() as f64 / 1_048_576.0);
    /// ```
    pub fn total_mb(&self) -> f64 {
        self.total_bytes() as f64 / 1_048_576.0
    }
}

/// What [`Graph::shrink_to_fit`](crate::storage::Graph::shrink_to_fit) released: the storage's
/// [`MemoryStats::total_bytes`] measured before and after
/// [`GraphStorage::shrink_to_fit`](crate::storage::GraphStorage::shrink_to_fit) under the same
/// write lock. Both are `None` when the storage reports no memory figures
/// ([`GraphStorage::memory_usage`](crate::storage::GraphStorage::memory_usage) answers
/// `None`): the shrink ran, but nothing was measured.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ShrinkReport {
    /// The storage's total bytes before the shrink (`None`: not measured).
    pub before_bytes: Option<u64>,
    /// The storage's total bytes after the shrink (`None`: not measured).
    pub after_bytes: Option<u64>,
}

impl ShrinkReport {
    /// `before_bytes - after_bytes` (never negative); `None` when the storage reports no memory
    /// figures.
    ///
    /// ```rust
    /// use std::sync::Arc;
    /// use graphersal::prelude::*;
    ///
    /// let graph = Graph::new(TraversalGraph::tinkerpop_modern());
    /// graph.shrink_to_fit().unwrap();
    /// // Nothing is left to release the second time.
    /// let report = graph.shrink_to_fit().unwrap();
    /// assert_eq!(report.released_bytes(), Some(0));
    /// assert_eq!(report.before_bytes, report.after_bytes);
    /// ```
    pub fn released_bytes(&self) -> Option<u64> {
        Some(self.before_bytes?.saturating_sub(self.after_bytes?))
    }
}

/// Computes [`MemoryStats`] for a [`crate::TraversalGraph`]. Lives here, rather than inline in
/// `graph/storage.rs`'s `impl GraphStorage for TraversalGraph`, purely to keep that file's trait
/// impl list uncluttered.
///
/// `structure_bytes` uses the topology's capacity — the underlying node/edge `Vec`s' raw
/// capacity, which includes tombstoned (removed-but-not-reclaimed) slots — rather than
/// `node_count()`/`edge_count()`, specifically to avoid undercounting a graph with heavy
/// add/remove churn; see [`MemoryStats::structure_bytes`]'s doc comment for the full reasoning.
/// Each vertex's primary label and multi-label set are counted here too (structural data, not a
/// property), while `property_bytes` walks only each element's `PropertyMap`.
pub(crate) fn compute(graph: &super::TraversalGraph) -> MemoryStats {
    use crate::value::compact_string_heap_bytes as string_heap;

    let (node_cap, edge_cap) = graph.graph.capacity();
    let node_slot_size = super::GraphType::node_slot_bytes() as u64;
    let edge_slot_size = super::GraphType::edge_slot_bytes() as u64;
    let mut structure_bytes = node_cap as u64 * node_slot_size + edge_cap as u64 * edge_slot_size;

    let mut property_bytes = 0u64;
    let mut compressed = CompressedCount::default();

    // The labels live once, in the label table; a slot holds a number.
    structure_bytes += graph.labels.heap_bytes();
    for (id, _, properties, _) in graph.graph.node_weights() {
        structure_bytes += string_heap(id);
        property_bytes += crate::value::estimated_element_heap_bytes(properties);
        if !graph.compression.is_empty() {
            properties.values().for_each(|value| compressed.add(value));
        }
    }

    for (id, _, properties, _) in graph.graph.edge_weights() {
        structure_bytes += string_heap(id);
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
        workload_bytes: 0,
        model_bytes: 0,
    }
}

/// The structure part of the memory report for a [`crate::TraversalGraph`]: the totals of
/// [`compute`], the slots (live; tombstoned: vacant slots of removed elements; spare: allocated,
/// never used), the id and label indexes and the shape table.
pub(crate) fn structure(graph: &super::TraversalGraph) -> super::StructureMemory {
    use super::memory_report::SlotUsage;
    let (node_cap, edge_cap) = graph.graph.capacity();
    let (nodes, edges) = (graph.graph.node_count(), graph.graph.edge_count());
    let (vacant_nodes, vacant_edges) = (graph.graph.vacant_nodes(), graph.graph.vacant_edges());
    let vertices = SlotUsage::new(
        nodes as u64,
        vacant_nodes as u64,
        node_cap.saturating_sub(nodes + vacant_nodes) as u64,
        super::GraphType::node_slot_bytes() as u64,
    );
    let edges = SlotUsage::new(
        edges as u64,
        vacant_edges as u64,
        edge_cap.saturating_sub(edges + vacant_edges) as u64,
        super::GraphType::edge_slot_bytes() as u64,
    );
    let id_index = graph.index.id_index_heap_bytes();
    let label_index = graph.index.estimated_heap_bytes().saturating_sub(id_index);
    super::StructureMemory::new(compute(graph))
        .with_slots(vertices, edges)
        .with_index_bytes(id_index, label_index)
        .with_shape_table_shapes(graph.shapes.len() as u64)
}

/// The compressed strings of a walk (only objects can hold one: rule paths name keys).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct CompressedCount {
    pub(crate) values: u64,
    pub(crate) plain: u64,
    pub(crate) stored: u64,
    pub(crate) deduped: u64,
    pub(crate) deduped_plain: u64,
}

impl CompressedCount {
    pub(crate) fn add(&mut self, value: &crate::value::ElementProperty) {
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
    let slot = super::GraphType::node_slot_bytes() as u64;
    let mut bytes = slot
        + super::index::id_entry_bytes::<super::VertexHandle>()
        + compact_string_heap_bytes(id);
    // The entry in each label's bucket set (the label itself lives once, in the label table).
    bytes += labels.len() as u64 * index_entry_bytes::<super::VertexHandle, ()>();
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
    let slot = super::GraphType::edge_slot_bytes() as u64;
    let heap = |text: &str| -> u64 {
        // A `CompactString` keeps up to its own size inline (24 bytes on 64-bit targets).
        if text.len() > size_of::<CompactString>() {
            text.len() as u64
        } else {
            0
        }
    };
    let mut bytes = slot + super::index::id_entry_bytes::<super::EdgeHandle>() + heap(id);
    if label.is_some() {
        // The label lives once, in the label table; the edge adds its bucket entry.
        bytes += index_entry_bytes::<super::EdgeHandle, ()>();
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

impl FootprintCache {
    /// Forgets the cached walk, as if the graph had never been walked (`shrink_to_fit`, tests).
    pub(crate) fn forget(&self) {
        *self.entry.lock() = None;
    }
}

/// [`compute`]'s total for `graph`, walked again only when the graph changed since the cached
/// walk (the commit position or the element counts differ). A rolled-back unit restores the
/// data and leaves the commit position, so the cached figure stays valid.
///
/// While an open unit holds uncommitted changes the graph is NEVER walked (user decision
/// 2026-11-01): a walk there could not be cached (a rollback may restore other data under the
/// same commit position), so every run of the unit would walk again, O(graph) per traversal.
/// The figure is then [`footprint_estimate`]: the last walk extrapolated by the element counts.
/// A documented edge case of the estimated memory measure: after large changes inside the
/// unit (property rewrites, elements far from the walked average) it can be off until the unit
/// ends; the tracking allocator gives exact figures. `memory_usage()` still walks on request.
pub(crate) fn footprint_bytes(graph: &super::TraversalGraph) -> u64 {
    if !graph.undo.entries().is_empty() {
        return footprint_estimate(graph);
    }
    let key = footprint_key(graph);
    if let Some((cached, bytes)) = *graph.footprint.entry.lock()
        && cached == key
    {
        return bytes;
    }
    walk(graph).total_bytes()
}

/// The footprint without a walk: the last walk extrapolated to the current element counts at
/// its average element size ([`extrapolate`], whatever the drift), or the element count times
/// the minimum element size (`MIN_ELEMENT_BYTES`) when the graph has never been walked.
fn footprint_estimate(graph: &super::TraversalGraph) -> u64 {
    let now = element_count(graph);
    match *graph.footprint.entry.lock() {
        Some((key, bytes)) => extrapolate(key, bytes, now),
        None => now.saturating_mul(crate::traversal::memory_budget::MIN_ELEMENT_BYTES),
    }
}

/// Vertices plus edges of `graph`.
fn element_count(graph: &super::TraversalGraph) -> u64 {
    (graph.graph.node_count() as u64).saturating_add(graph.graph.edge_count() as u64)
}

/// The walked total `bytes` (taken at `key`) moved to `now` elements at the walk's average
/// element size (at least `MIN_ELEMENT_BYTES`).
fn extrapolate(key: FootprintKey, bytes: u64, now: u64) -> u64 {
    let walked = key.vertices.saturating_add(key.edges);
    let per_element = bytes
        .checked_div(walked)
        .unwrap_or(0)
        .max(crate::traversal::memory_budget::MIN_ELEMENT_BYTES);
    if now >= walked {
        bytes.saturating_add((now - walked).saturating_mul(per_element))
    } else {
        bytes.saturating_sub((walked - now).saturating_mul(per_element))
    }
}

/// [`compute`] for `graph`, remembered as the footprint cache when no open unit holds
/// uncommitted changes: `memory_usage()` goes through it, so asking for the footprint also
/// refreshes [`footprint_hint`] (a manual full walk, e.g. after many property rewrites).
pub(crate) fn walk(graph: &super::TraversalGraph) -> MemoryStats {
    #[cfg(test)]
    graph
        .footprint
        .walks
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let stats = compute(graph);
    if graph.undo.entries().is_empty() {
        *graph.footprint.entry.lock() = Some((footprint_key(graph), stats.total_bytes()));
    }
    stats
}

fn footprint_key(graph: &super::TraversalGraph) -> FootprintKey {
    FootprintKey {
        commit_seq: graph.commit_seq(),
        vertices: graph.graph.node_count() as u64,
        edges: graph.graph.edge_count() as u64,
    }
}

/// A cheap estimate of [`footprint_bytes`] without a walk: the total of the last walk plus the
/// elements added since (minus those removed) at that walk's average element size (at least 128
/// bytes). `None` before the first walk.
///
/// Outside an open unit with uncommitted changes it is also `None` once the element count has
/// more than doubled (plus 1024) or halved since the walk: then the caller walks again and the
/// walk is cached, so a growing graph is walked O(log n) times, amortized a constant per added
/// element. Inside an open unit with changes a walk would NOT be cached (a rollback could
/// restore other data under the same commit position), so every later run of the unit would
/// walk again: O(graph) per traversal (the 0.5.0 regression: a script dropping most of a graph
/// and then running many small writes). There the hint stays an estimate from the last walk,
/// whatever the drift; the next run outside the unit walks again if the band is exceeded.
/// Property values rewritten since the walk are not seen; outside an open unit with changes the
/// memory checks also walk again when the estimate reaches half of the ceiling (inside one
/// [`footprint_bytes`] does not walk either).
pub(crate) fn footprint_hint(graph: &super::TraversalGraph) -> Option<u64> {
    let (key, bytes) = (*graph.footprint.entry.lock())?;
    let walked = key.vertices.saturating_add(key.edges);
    let now = element_count(graph);
    let refreshable = graph.undo.entries().is_empty();
    if refreshable && (now > walked.saturating_mul(2).saturating_add(1024) || now < walked / 2) {
        return None;
    }
    Some(extrapolate(key, bytes, now))
}
