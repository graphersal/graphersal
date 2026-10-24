//! The memory report (book page "Memory Usage", section "The memory report"): WHERE a graph's
//! memory goes, per element kind and label and per property path, computed on demand by one
//! walk over the storage's read API ([`walk::compute`]).
//!
//! It reports facts, never advice: what each `(kind, label, path)` holds (elements, value
//! types, bytes, a size histogram, distinct strings, vectors, compressed and shared values),
//! what each label costs (ids, labels, slots, degrees), the property indexes and compression
//! rules, and, from the storage itself ([`GraphStorage::memory_structure`]), the structure:
//! slots, tombstones, the id and label indexes, the shape table. Projections ("if the vectors
//! were `i8`") are derived from these figures by whoever reads them.
//!
//! The report does not assume a walk: every figure is a plain field, so a later source (per-key
//! statistics kept with stored segments) can fill the same types.
//!
//! [`GraphStorage::memory_structure`]: crate::graph::GraphStorage::memory_structure

mod hll;
mod property;
mod text;
pub(crate) mod walk;

use std::sync::Arc;

use compact_str::CompactString;

use crate::graph::compression::CompressionStats;
use crate::graph::memory::MemoryStats;
use crate::graph::property_index::PropertyIndexInfo;
use crate::graph::schema::ElementKind;
use crate::traversal::cancel::CancelToken;
use crate::value::JsonPath;
use crate::value::json_path::JsonPathSegment;
use crate::value::vector::VectorElement;

/// What a figure of the memory report (and later the workload statistics and the storage
/// advisor) is about: the property at `path` of the `element`s labelled `label`.
///
/// `label` is the PRIMARY label of a vertex (its first; a vertex with several labels is counted
/// once, under its first label) or the label of an edge; `None` for unlabeled elements. `path` is
/// a jpath of names; the memory report fills top-level keys (one segment).
///
/// ```
/// use graphersal::storage::{ElementKind, PropertyTarget};
///
/// let target = PropertyTarget::top_level(ElementKind::Vertex, Some("person"), "name");
/// assert_eq!(target.to_string(), "vertex person $.name");
/// assert_eq!(target.key(), Some("name"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct PropertyTarget {
    /// The element kind.
    pub element: ElementKind,
    /// The primary label (vertices) or the label (edges); `None`: unlabeled elements.
    pub label: Option<CompactString>,
    /// The property path, a jpath of names (one segment: a top-level key).
    pub path: JsonPath,
}

impl PropertyTarget {
    /// The target `path` of the `element`s labelled `label`.
    pub fn new(element: ElementKind, label: Option<&str>, path: JsonPath) -> Self {
        PropertyTarget {
            element,
            label: label.map(CompactString::from),
            path,
        }
    }

    /// The top-level property `key` of the `element`s labelled `label`.
    pub fn top_level(element: ElementKind, label: Option<&str>, key: &str) -> Self {
        Self::new(
            element,
            label,
            JsonPath(Arc::new(vec![JsonPathSegment::Key(key.into())])),
        )
    }

    /// The key of a one-segment path (a top-level key), `None` for another path.
    pub fn key(&self) -> Option<&str> {
        match self.path.segments() {
            [JsonPathSegment::Key(key)] => Some(key.as_str()),
            _ => None,
        }
    }
}

impl std::fmt::Display for PropertyTarget {
    /// `vertex person $.name` (an unlabeled element: `vertex - $.name`).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {} {}",
            self.element.as_str(),
            self.label.as_deref().unwrap_or("-"),
            self.path
        )
    }
}

/// How far a running report has come: elements visited of the elements to visit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct MemoryReportProgress {
    /// Elements (vertices, then edges) passed so far, sampled or not.
    pub visited: u64,
    /// All elements of the graph.
    pub total: u64,
}

/// A progress callback of a [`MemoryReportRequest`].
type ProgressFn = Arc<dyn Fn(MemoryReportProgress) + Send + Sync>;

/// How to compute a [`MemoryReport`]: full or sampled, the path depth, the row cap, a
/// cancellation token and a progress callback. Built with [`Self::new`] and the `with_*`
/// methods; the default is a full walk.
///
/// ```
/// use graphersal::storage::MemoryReportRequest;
///
/// let request = MemoryReportRequest::new().with_max_elements(50_000).with_max_rows(20);
/// assert_eq!(request.max_elements(), Some(50_000));
/// assert_eq!(request.path_depth(), 1);
/// ```
#[derive(Clone)]
pub struct MemoryReportRequest {
    sample_every: u64,
    max_elements: Option<u64>,
    path_depth: u32,
    max_rows: Option<usize>,
    max_labels: Option<usize>,
    cancel: Option<CancelToken>,
    progress: Option<ProgressFn>,
}

impl Default for MemoryReportRequest {
    fn default() -> Self {
        MemoryReportRequest {
            sample_every: 1,
            max_elements: None,
            path_depth: 1,
            max_rows: None,
            max_labels: None,
            cancel: None,
            progress: None,
        }
    }
}

impl std::fmt::Debug for MemoryReportRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryReportRequest")
            .field("sample_every", &self.sample_every)
            .field("max_elements", &self.max_elements)
            .field("path_depth", &self.path_depth)
            .field("max_rows", &self.max_rows)
            .field("max_labels", &self.max_labels)
            .field("cancel", &self.cancel.is_some())
            .field("progress", &self.progress.is_some())
            .finish()
    }
}

impl MemoryReportRequest {
    /// A full report (every element visited), top-level keys, every row.
    pub fn new() -> Self {
        Self::default()
    }

    /// Reads the properties of every `k`-th element of each label only (`1`: all, the default;
    /// `0` is refused by [`Self::validate`]). The sample is stratified by label: the walk still
    /// passes every element and counts it under its label, reads the first element of every
    /// label and every k-th after it, and scales each label's figures by its exact count over
    /// the elements read. Distinct counts from a sample are flagged as unreliable.
    pub fn with_sample_every(mut self, k: u64) -> Self {
        self.sample_every = k;
        self
    }

    /// Samples so that about `n` elements are read (the step is the element count over `n`,
    /// computed when the report runs; a larger [`Self::with_sample_every`] wins). Every label is
    /// read at least once, so a graph with more labels than `n` reads more.
    pub fn with_max_elements(mut self, n: u64) -> Self {
        self.max_elements = Some(n);
        self
    }

    /// The depth of the property paths reported (default `1`: top-level keys, a nested value
    /// aggregated under its key). Only `1` is supported yet; a deeper breakdown is a later,
    /// additive change.
    pub fn with_path_depth(mut self, depth: u32) -> Self {
        self.path_depth = depth;
        self
    }

    /// Keeps the `n` property rows with the most bytes (the rest counted in
    /// [`MemoryReport::rows_omitted`]).
    pub fn with_max_rows(mut self, n: usize) -> Self {
        self.max_rows = Some(n);
        self
    }

    /// Keeps the `n` label rows with the most bytes (property bytes plus slots; the rest counted
    /// in [`MemoryReport::labels_omitted`]).
    pub fn with_max_labels(mut self, n: usize) -> Self {
        self.max_labels = Some(n);
        self
    }

    /// Stops the walk when `token` is cancelled (the report then fails with
    /// [`TraverserError::Cancelled`](crate::TraverserError::Cancelled)). Without a token the
    /// running thread's [`CancelToken::scope`] applies.
    pub fn with_cancel(mut self, token: CancelToken) -> Self {
        self.cancel = Some(token);
        self
    }

    /// Calls `progress` every few thousand elements and once at the end.
    pub fn with_progress(
        mut self,
        progress: impl Fn(MemoryReportProgress) + Send + Sync + 'static,
    ) -> Self {
        self.progress = Some(Arc::new(progress));
        self
    }

    /// The sampling step asked for (`1`: every element).
    pub fn sample_every(&self) -> u64 {
        self.sample_every
    }

    /// The cap on visited elements, if any.
    pub fn max_elements(&self) -> Option<u64> {
        self.max_elements
    }

    /// The path depth.
    pub fn path_depth(&self) -> u32 {
        self.path_depth
    }

    /// The row cap, if any.
    pub fn max_rows(&self) -> Option<usize> {
        self.max_rows
    }

    /// The label row cap, if any.
    pub fn max_labels(&self) -> Option<usize> {
        self.max_labels
    }

    /// The cancellation token, if any.
    pub fn cancel_token(&self) -> Option<&CancelToken> {
        self.cancel.as_ref()
    }

    /// Checks the options: a sampling step and an element cap of at least 1, path depth 1.
    pub fn validate(&self) -> Result<(), crate::error::GraphError> {
        if self.sample_every == 0 {
            return Err(crate::error::GraphError::unsupported(
                "a memory report sampling every 0th element (use 1 for a full report)",
            ));
        }
        if self.max_elements == Some(0) {
            return Err(crate::error::GraphError::unsupported(
                "a memory report visiting at most 0 elements",
            ));
        }
        if self.path_depth != 1 {
            return Err(crate::error::GraphError::unsupported(
                "a memory report path depth other than 1 (top-level keys)",
            ));
        }
        Ok(())
    }

    /// The step the walk uses for a graph of `total` elements.
    pub(crate) fn step_for(&self, total: u64) -> u64 {
        let by_cap = match self.max_elements {
            Some(cap) if cap > 0 && total > cap => total.div_ceil(cap),
            _ => 1,
        };
        by_cap.max(self.sample_every).max(1)
    }

    pub(crate) fn report_progress(&self, visited: u64, total: u64) {
        if let Some(progress) = &self.progress {
            progress(MemoryReportProgress { visited, total });
        }
    }
}

/// The memory report: where a graph's memory goes. Obtained with
/// [`GraphTraversalSource::memory_report`](crate::GraphTraversalSource::memory_report).
///
/// The property rows ([`Self::properties`]), the label rows ([`Self::labels`]), the shapes,
/// indexes and compression rules are computed for ANY storage through its read API; the
/// structure ([`Self::structure`]: slots, the id and label indexes, totals) only when the
/// storage reports it. All byte figures are estimates of the same kind as
/// [`MemoryStats`]; for a `TraversalGraph` the property figures add up exactly to
/// [`MemoryStats::property_bytes`] (see [`Self::property_bytes`]).
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct MemoryReport {
    /// One row per `(kind, label, path)`, most bytes first.
    pub properties: Vec<PropertyMemory>,
    /// Rows left out by [`MemoryReportRequest::with_max_rows`].
    pub rows_omitted: u64,
    /// One row per `(kind, label)`, vertices first, most bytes first.
    pub labels: Vec<LabelMemory>,
    /// Label rows left out by [`MemoryReportRequest::with_max_labels`].
    pub labels_omitted: u64,
    /// The [`LabelMemory::property_bytes`] of those rows summed (so that
    /// [`Self::property_bytes`] stays complete).
    pub labels_omitted_property_bytes: u64,
    /// The shared property key lists ("shapes") the walk met.
    pub shapes: ShapeSummary,
    /// The property indexes and what each holds (each index separately).
    pub indexes: Vec<PropertyIndexInfo>,
    /// The compression rules and what each holds.
    pub compression_rules: Vec<RuleMemory>,
    /// What only the storage can tell: slots, tombstones, the id and label indexes and the
    /// totals of [`MemoryStats`]; `None` when the storage does not report its structure
    /// ([`GraphStorage::memory_structure`](crate::graph::GraphStorage::memory_structure)).
    pub structure: Option<StructureMemory>,
    /// The sampling step used (`1`: every element visited).
    pub sample_every: u64,
    /// Whether some figures are extrapolated from a sample (some label had elements not read).
    pub sampled: bool,
    /// Elements whose properties were read (with sampling at least one per label).
    pub visited_elements: u64,
    /// The path depth of the rows.
    pub path_depth: u32,
    /// The commit position of the graph the report describes
    /// ([`GraphStorage::last_commit_seq`](crate::graph::GraphStorage::last_commit_seq)).
    pub commit_seq: u64,
    /// The process's live heap as counted by an installed tracking allocator
    /// (`alloc_tracking::global_live_bytes`, feature `alloc-tracking`), `None` without one. It covers the whole
    /// process (other graphs, the script engine, buffers), not this graph alone.
    pub tracked_live_bytes: Option<u64>,
    /// The time the report took, in microseconds.
    pub elapsed_micros: u64,
}

impl MemoryReport {
    /// The bytes of the property values the rows and label rows account for: every row's
    /// [`PropertyMemory::bytes`], every label's [`LabelMemory::map_overhead_bytes`], plus the
    /// shape table and the `dedup` tables when the storage reports them. For a full report of a
    /// `TraversalGraph` this equals [`MemoryStats::property_bytes`] (also with
    /// [`MemoryReportRequest::with_max_rows`] and [`MemoryReportRequest::with_max_labels`]:
    /// omitted rows still count, in their label rows and in
    /// [`Self::labels_omitted_property_bytes`]).
    pub fn property_bytes(&self) -> u64 {
        let labels: u64 = self
            .labels
            .iter()
            .map(|label| label.property_bytes)
            .sum::<u64>()
            + self.labels_omitted_property_bytes;
        let shared = self.structure.as_ref().map_or(0, |structure| {
            structure.totals.shape_bytes + structure.totals.dedup_table_bytes
        });
        labels + shared
    }

    /// The estimated total footprint ([`MemoryStats::total_bytes`] of [`Self::structure`]), or
    /// `None` when the storage does not report its structure.
    pub fn estimated_total_bytes(&self) -> Option<u64> {
        self.structure
            .as_ref()
            .map(|structure| structure.totals.total_bytes())
    }

    /// The tracked live heap minus the estimated total: what the estimate does not explain
    /// (allocator overhead and fragmentation, the rest of the process). `None` without a tracking
    /// allocator or without a structure; negative when the estimate is higher.
    pub fn unattributed_bytes(&self) -> Option<i64> {
        let live = self.tracked_live_bytes?;
        let estimate = self.estimated_total_bytes()?;
        Some(live as i64 - estimate as i64)
    }

    /// The row of `target`, if the report has one.
    pub fn property(&self, target: &PropertyTarget) -> Option<&PropertyMemory> {
        self.properties.iter().find(|row| &row.target == target)
    }

    /// The row of the `element`s labelled `label`, if the report has one.
    pub fn label(&self, element: ElementKind, label: Option<&str>) -> Option<&LabelMemory> {
        self.labels
            .iter()
            .find(|row| row.element == element && row.label.as_deref() == label)
    }
}

/// What one `(kind, label, path)` holds.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct PropertyMemory {
    /// The property.
    pub target: PropertyTarget,
    /// Whether every value here is a leaf: `false` when at least one value is an object (or a
    /// map), whose nested keys this row aggregates. Arrays count as leaves.
    pub leaf: bool,
    /// Values that are objects (or maps).
    pub nested_values: u64,
    /// Elements of the label that have the key (sparsity: compare with
    /// [`Self::label_elements`]).
    pub elements: u64,
    /// All elements of the label.
    pub label_elements: u64,
    /// How many values have each logical type (`string`, `int64`, `float64`, `boolean`, `uuid`,
    /// the temporal type names, `vector`, `array`, `object`, `null`, ...), most first.
    pub value_types: Vec<ValueTypeCount>,
    /// The value slots: one [`crate::ElementProperty`] (32 bytes) per value.
    pub slot_bytes: u64,
    /// The values' own heap (strings above 24 bytes, compressed blocks, vectors, arrays,
    /// objects with everything nested in them). A `dedup`-shared text counts 0 here: it is
    /// counted once, in its rule's table.
    pub heap_bytes: u64,
    /// The size distribution of the values by their own heap bytes ([`SIZE_BUCKET_BOUNDS`]),
    /// each bucket with its value count and bytes.
    pub sizes: Vec<SizeBucket>,
    /// The largest value's own heap bytes.
    pub max_value_bytes: u64,
    /// The string values (also compressed and shared ones), `None` when there are none.
    pub strings: Option<StringMemory>,
    /// The vector values by element kind and dimension.
    pub vectors: Vec<VectorMemory>,
    /// Strings stored compressed (at any depth below the key).
    pub compressed_values: u64,
    /// Their plain size, in bytes.
    pub compressed_plain_bytes: u64,
    /// Their stored size, in bytes.
    pub compressed_stored_bytes: u64,
    /// Strings shared through a `dedup` rule (at any depth below the key).
    pub deduped_values: u64,
    /// Their plain size, in bytes.
    pub deduped_plain_bytes: u64,
    /// The compression rules whose path lies at or below this one (their figures:
    /// [`MemoryReport::compression_rules`]).
    pub rules: Vec<String>,
    /// The property indexes on this key (their figures: [`MemoryReport::indexes`]).
    pub indexes: Vec<String>,
}

impl PropertyMemory {
    /// The slots plus the heap.
    pub fn bytes(&self) -> u64 {
        self.slot_bytes + self.heap_bytes
    }

    /// The values of all buckets whose sizes are above `bytes` (exact when `bytes` is a bucket
    /// bound of [`SIZE_BUCKET_BOUNDS`], else the buckets wholly above it): `(values, bytes)`.
    pub fn above(&self, bytes: u64) -> (u64, u64) {
        let mut previous: Option<u64> = None;
        let mut out = (0, 0);
        for bucket in &self.sizes {
            // Every value of the bucket is above the previous bucket's bound.
            if previous.is_some_and(|bound| bound >= bytes) {
                out.0 += bucket.values;
                out.1 += bucket.bytes;
            }
            previous = bucket.max_bytes.or(previous);
        }
        out
    }
}

/// The upper bounds (inclusive, bytes) of the size histogram's buckets: `0` (no heap: a scalar,
/// a string of at most 24 bytes, a shared text), then powers of two up to 1 MiB; a last open
/// bucket holds the values above 1 MiB.
pub const SIZE_BUCKET_BOUNDS: [u64; 17] = [
    0,
    32,
    64,
    128,
    256,
    512,
    1 << 10,
    2 << 10,
    4 << 10,
    8 << 10,
    16 << 10,
    32 << 10,
    64 << 10,
    128 << 10,
    256 << 10,
    512 << 10,
    1 << 20,
];

/// One bucket of a size histogram.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct SizeBucket {
    /// The inclusive upper bound in bytes (`None`: the open last bucket).
    pub max_bytes: Option<u64>,
    /// Values in the bucket.
    pub values: u64,
    /// Their bytes summed.
    pub bytes: u64,
}

/// How many values have one logical type.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ValueTypeCount {
    /// The type name (`string`, `int64`, `date`, `vector`, `object`, ...).
    pub name: String,
    /// The values of that type.
    pub count: u64,
}

/// The string values of a row (plain, compressed and shared alike).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct StringMemory {
    /// String values.
    pub values: u64,
    /// Those stored inline (at most 24 bytes, no heap).
    pub inline_values: u64,
    /// Their heap bytes (plain strings above 24 bytes and compressed blocks; shared texts are
    /// in their tables).
    pub heap_bytes: u64,
    /// Their plain (logical) length, in bytes.
    pub plain_bytes: u64,
    /// The estimated number of distinct texts (HyperLogLog, 256 registers: about 6.5 %
    /// standard error).
    pub distinct_estimate: u64,
    /// `false` when the estimate comes from a sample: distinct counts do not extrapolate.
    pub distinct_reliable: bool,
}

/// The vectors of one element kind and dimension in a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct VectorMemory {
    /// The element kind.
    pub element: VectorElement,
    /// The number of components.
    pub dimension: u64,
    /// Vectors of this kind and dimension.
    pub count: u64,
    /// Their heap bytes (components and the shared allocation).
    pub bytes: u64,
}

/// What one `(kind, label)` costs.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct LabelMemory {
    /// The element kind.
    pub element: ElementKind,
    /// The primary label (vertices) or label (edges); `None`: unlabeled.
    pub label: Option<CompactString>,
    /// Elements (a vertex counts under its primary label only).
    pub elements: u64,
    /// Ids longer than 24 bytes (kept on the heap), in bytes.
    pub id_heap_bytes: u64,
    /// The label lists of the vertices (a slot per label plus long labels' heap); `0` for edges,
    /// whose single label is part of the slot.
    pub label_bytes: u64,
    /// The elements' slots ([`SlotUsage::slot_bytes`] each), when the storage reports its
    /// structure; for edges this IS the adjacency (endpoints and list links), for vertices the
    /// slot holds the heads of the edge lists.
    pub slot_bytes: Option<u64>,
    /// The property bytes of these elements: their rows' [`PropertyMemory::bytes`] plus
    /// [`Self::map_overhead_bytes`].
    pub property_bytes: u64,
    /// What the property maps take beyond one slot per value: spare value slots (a dropped
    /// key's slot kept for reuse) and, for a map that is not shaped, its own hash table and
    /// keys.
    pub map_overhead_bytes: u64,
    /// Property values of these elements.
    pub values: u64,
    /// The degrees of these vertices (`None` for edges).
    pub degree: Option<DegreeStats>,
}

/// The degree distribution of the vertices of one label (supernodes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct DegreeStats {
    /// The largest number of outgoing edges.
    pub max_out: u64,
    /// The largest number of incoming edges.
    pub max_in: u64,
    /// The largest degree (out + in).
    pub max: u64,
    /// The 95th percentile of the degree (out + in) over the visited vertices.
    pub p95: u64,
    /// All edge ends of these vertices (out + in), extrapolated when sampled.
    pub edge_ends: u64,
}

/// The property shapes the walk met: a shape is one key sequence shared by every element with
/// exactly those keys in that order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct ShapeSummary {
    /// Distinct shapes met.
    pub shapes: u64,
    /// Elements whose map has a shape and at least one key (extrapolated when sampled); elements
    /// without properties count in neither figure.
    pub shaped_elements: u64,
    /// Elements whose map has none (a storage keeping plain maps).
    pub unshaped_elements: u64,
    /// Elements per shape: bucket `max_elements` 1, 3, 7, 15, ... (powers of two minus one),
    /// each with its shape and element counts; only non-empty buckets.
    pub histogram: Vec<ShapeBucket>,
}

/// One bucket of [`ShapeSummary::histogram`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct ShapeBucket {
    /// The inclusive upper bound of elements per shape.
    pub max_elements: u64,
    /// Shapes in the bucket.
    pub shapes: u64,
    /// Their elements summed.
    pub elements: u64,
}

/// A compression rule and what it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RuleMemory {
    /// The rule's name.
    pub name: String,
    /// What it applies to.
    pub target: PropertyTarget,
    /// Its codec (`lz4`, `dedup`).
    pub codec: String,
    /// What it holds ([`GraphStorage::compression_stats`](crate::graph::GraphStorage::compression_stats));
    /// `None` when the storage serves no compression.
    pub stats: Option<CompressionStats>,
}

/// The slots of one element kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct SlotUsage {
    /// Slots holding an element.
    pub live: u64,
    /// Vacant slots below the highest live slot: removed elements whose slots are not reused
    /// yet (a stable graph keeps them so that handles stay valid).
    pub tombstoned: u64,
    /// Allocated slots above the highest live slot (spare capacity, also slots of removed
    /// elements that were the last ones).
    pub spare: u64,
    /// The size of one slot, in bytes.
    pub slot_bytes: u64,
}

impl SlotUsage {
    /// The slot figures of a storage.
    pub fn new(live: u64, tombstoned: u64, spare: u64, slot_bytes: u64) -> Self {
        SlotUsage {
            live,
            tombstoned,
            spare,
            slot_bytes,
        }
    }

    /// All allocated slots times the slot size.
    pub fn bytes(&self) -> u64 {
        (self.live + self.tombstoned + self.spare) * self.slot_bytes
    }
}

/// What only a storage can tell about its memory: the totals of [`MemoryStats`], the slots,
/// the id and label indexes and the shape table. Returned by
/// [`GraphStorage::memory_structure`](crate::graph::GraphStorage::memory_structure); a storage
/// outside this crate builds one with [`Self::new`] and the `with_*` methods.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct StructureMemory {
    /// The totals ([`GraphStorage::memory_usage`](crate::graph::GraphStorage::memory_usage)).
    pub totals: MemoryStats,
    /// The vertex slots.
    pub vertex_slots: SlotUsage,
    /// The edge slots.
    pub edge_slots: SlotUsage,
    /// The id index (id -> element), in bytes.
    pub id_index_bytes: u64,
    /// The label index (label -> elements), in bytes.
    pub label_index_bytes: u64,
    /// Shapes held by the shape table (also ones no element uses any more until the next sweep).
    pub shape_table_shapes: u64,
}

impl StructureMemory {
    /// A structure with these totals and nothing else known yet.
    pub fn new(totals: MemoryStats) -> Self {
        StructureMemory {
            totals,
            ..Self::default()
        }
    }

    /// With the vertex and edge slots.
    pub fn with_slots(mut self, vertices: SlotUsage, edges: SlotUsage) -> Self {
        self.vertex_slots = vertices;
        self.edge_slots = edges;
        self
    }

    /// With the id and label index bytes.
    pub fn with_index_bytes(mut self, id_index: u64, label_index: u64) -> Self {
        self.id_index_bytes = id_index;
        self.label_index_bytes = label_index;
        self
    }

    /// With the shape table's shape count.
    pub fn with_shape_table_shapes(mut self, shapes: u64) -> Self {
        self.shape_table_shapes = shapes;
        self
    }
}
