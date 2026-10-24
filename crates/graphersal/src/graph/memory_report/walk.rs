//! The on-demand walk behind [`MemoryReport`]: one pass over the vertices and the edges through
//! the read API of any [`GraphStorage`], aggregating by shape.
//!
//! Per element the walk finds its label accumulator (a memo on the last label: one short string
//! compare) and, for a shaped map, the shape's row list (a memo on the last shape pointer, a
//! pointer-keyed map otherwise); each value is then added to its row by slot index, so no key is
//! hashed or compared per element. A new shape maps its keys to rows once. A plain (`Indexed`)
//! map, which a storage outside this crate may hand out, falls back to one key lookup per entry.
//!
//! Sampling is stratified by label: the walk still passes every element (reading its label
//! only), counts it under its label and reads the properties of every k-th element OF THAT
//! LABEL (the first one always). So every label is represented, and each label's figures are
//! scaled by its exact element count (counted by the walk itself) over its visited count. A graph
//! with many small labels visits about one element per label.

use std::collections::HashMap;
use std::sync::Arc;

use compact_str::CompactString;

use super::hll::HyperLogLog;
use super::{
    DegreeStats, LabelMemory, MemoryReport, MemoryReportRequest, PropertyMemory, PropertyTarget,
    RuleMemory, SIZE_BUCKET_BOUNDS, ShapeBucket, ShapeSummary, SizeBucket, StringMemory,
    ValueTypeCount, VectorMemory,
};
use crate::graph::GraphStorage;
use crate::graph::definition::DefinitionKind;
use crate::graph::memory::CompressedCount;
use crate::graph::schema::ElementKind;
use crate::value::ElementProperty;
use crate::value::property_map::Shape;
use crate::value::temporal::TemporalKind;
use crate::value::vector::VectorElement;

/// How many elements the walk passes between two cancellation checks.
const CANCEL_EVERY: u64 = 1024;
/// How many elements the walk passes between two progress reports.
const PROGRESS_EVERY: u64 = 16 * 1024;
/// The size of one value slot.
const SLOT: u64 = std::mem::size_of::<ElementProperty>() as u64;
/// Strings up to this many bytes are stored inline (`CompactString` on 64-bit targets).
const INLINE_ID: usize = std::mem::size_of::<CompactString>();

/// The value type buckets, in the order of [`type_index`].
const TYPE_NAMES: [&str; 17] = [
    "null",
    "boolean",
    "int64",
    "float64",
    "string",
    "uuid",
    "date",
    "localdatetime",
    "datetime",
    "time",
    "duration",
    "offsettime",
    "vector",
    "array",
    "object",
    "vertex",
    "edge",
];

/// The bucket of [`TYPE_NAMES`] a value counts in (its logical type; a compressed or shared
/// string is a `string`, a map with non-string keys an `object`).
fn type_index(value: &ElementProperty) -> usize {
    match value {
        ElementProperty::Null => 0,
        ElementProperty::Boolean(_) => 1,
        ElementProperty::Number(number) => {
            if number.is_i64() {
                2
            } else {
                3
            }
        }
        ElementProperty::String(_)
        | ElementProperty::Compressed(_)
        | ElementProperty::Deduped(_) => 4,
        ElementProperty::Uuid(_) => 5,
        ElementProperty::Temporal(value) => match value.kind() {
            TemporalKind::Date => 6,
            TemporalKind::LocalDateTime => 7,
            TemporalKind::DateTime => 8,
            TemporalKind::Time => 9,
            TemporalKind::Duration => 10,
            TemporalKind::OffsetTime => 11,
        },
        ElementProperty::Vector(_) => 12,
        ElementProperty::Array(_) => 13,
        ElementProperty::Object(_) | ElementProperty::Map(_) => 14,
        ElementProperty::Vertex(_) => 15,
        ElementProperty::Edge(_) => 16,
    }
}

/// The bucket of [`SIZE_BUCKET_BOUNDS`] (the last index: the open bucket) of `bytes`.
fn size_index(bytes: u64) -> usize {
    SIZE_BUCKET_BOUNDS.partition_point(|&bound| bound < bytes)
}

/// The string figures of one row while walking.
#[derive(Default)]
struct StringAcc {
    values: u64,
    inline: u64,
    heap: u64,
    plain: u64,
    distinct: HyperLogLog,
}

/// One `(kind, label, key)` row while walking (raw, unscaled figures).
struct RowAcc {
    key: CompactString,
    elements: u64,
    nested: u64,
    types: [u64; TYPE_NAMES.len()],
    heap: u64,
    sizes: [(u64, u64); SIZE_BUCKET_BOUNDS.len() + 1],
    max: u64,
    strings: Option<Box<StringAcc>>,
    vectors: Vec<(VectorElement, u64, u64, u64)>,
    compressed: CompressedCount,
}

impl RowAcc {
    fn new(key: CompactString) -> Self {
        RowAcc {
            key,
            elements: 0,
            nested: 0,
            types: [0; TYPE_NAMES.len()],
            heap: 0,
            sizes: [(0, 0); SIZE_BUCKET_BOUNDS.len() + 1],
            max: 0,
            strings: None,
            vectors: Vec::new(),
            compressed: CompressedCount::default(),
        }
    }

    fn add(&mut self, value: &ElementProperty) {
        self.elements += 1;
        let ty = type_index(value);
        if let Some(count) = self.types.get_mut(ty) {
            *count += 1;
        }
        let heap = value.estimated_heap_bytes();
        self.heap += heap;
        self.max = self.max.max(heap);
        if let Some(bucket) = self.sizes.get_mut(size_index(heap)) {
            bucket.0 += 1;
            bucket.1 += heap;
        }
        match value {
            ElementProperty::String(text) => {
                let strings = self.strings.get_or_insert_with(Box::default);
                strings.values += 1;
                if text.is_heap_allocated() {
                    strings.heap += heap;
                } else {
                    strings.inline += 1;
                }
                strings.plain += text.len() as u64;
                strings.distinct.add(text.as_bytes());
            }
            ElementProperty::Compressed(text) => {
                let strings = self.strings.get_or_insert_with(Box::default);
                strings.values += 1;
                strings.heap += heap;
                strings.plain += text.plain_len() as u64;
                text.with_text(|plain| strings.distinct.add(plain.as_bytes()));
            }
            ElementProperty::Deduped(text) => {
                let strings = self.strings.get_or_insert_with(Box::default);
                strings.values += 1;
                strings.plain += text.len() as u64;
                strings.distinct.add(text.as_str().as_bytes());
            }
            ElementProperty::Vector(vector) => {
                let (element, dimension) = (vector.element(), vector.dimension() as u64);
                match self
                    .vectors
                    .iter_mut()
                    .find(|entry| entry.0 == element && entry.1 == dimension)
                {
                    Some(entry) => {
                        entry.2 += 1;
                        entry.3 += heap;
                    }
                    None => self.vectors.push((element, dimension, 1, heap)),
                }
            }
            ElementProperty::Object(_) | ElementProperty::Map(_) => self.nested += 1,
            _ => {}
        }
        self.compressed.add(value);
    }
}

/// The rows of one shape within a label: the row index of each slot, and the elements met.
struct ShapeRows {
    shape: Arc<Shape>,
    rows: Vec<usize>,
    elements: u64,
}

/// One `(kind, label)` while walking (raw, unscaled figures).
struct LabelAcc {
    element: ElementKind,
    label: Option<CompactString>,
    /// Elements of the label passed (every one, sampled or not).
    seen: u64,
    /// Elements whose properties were read.
    visited: u64,
    id_heap: u64,
    label_bytes: u64,
    overhead: u64,
    values: u64,
    unshaped: u64,
    /// Row index by key.
    keys: HashMap<CompactString, usize>,
    /// The shapes met, by shape pointer (index into `shape_rows`).
    shapes: HashMap<usize, usize>,
    shape_rows: Vec<ShapeRows>,
    /// The last shape met (pointer, index into `shape_rows`).
    last_shape: Option<(usize, usize)>,
    /// The degree (out + in) of every visited vertex.
    degrees: Vec<u32>,
    max_out: u64,
    max_in: u64,
}

impl LabelAcc {
    fn new(element: ElementKind, label: Option<CompactString>) -> Self {
        LabelAcc {
            element,
            label,
            seen: 0,
            visited: 0,
            id_heap: 0,
            label_bytes: 0,
            overhead: 0,
            values: 0,
            unshaped: 0,
            keys: HashMap::new(),
            shapes: HashMap::new(),
            shape_rows: Vec::new(),
            last_shape: None,
            degrees: Vec::new(),
            max_out: 0,
            max_in: 0,
        }
    }
}

/// The accumulators of a walk.
#[derive(Default)]
struct Walk {
    labels: Vec<LabelAcc>,
    rows: Vec<(usize, RowAcc)>,
    vertex_labels: HashMap<CompactString, usize>,
    edge_labels: HashMap<CompactString, usize>,
    unlabeled: [Option<usize>; 2],
    /// The last label met per element kind.
    last: [Option<usize>; 2],
}

fn kind_slot(element: ElementKind) -> usize {
    match element {
        ElementKind::Vertex => 0,
        ElementKind::Edge => 1,
    }
}

impl Walk {
    /// The accumulator of `(element, label)`.
    fn label_index(&mut self, element: ElementKind, label: Option<&str>) -> usize {
        let slot = kind_slot(element);
        if let Some(last) = self.last[slot]
            && let Some(acc) = self.labels.get(last)
            && acc.label.as_deref() == label
        {
            return last;
        }
        let index = match label {
            None => match self.unlabeled[slot] {
                Some(index) => index,
                None => {
                    let index = self.labels.len();
                    self.labels.push(LabelAcc::new(element, None));
                    self.unlabeled[slot] = Some(index);
                    index
                }
            },
            Some(label) => {
                let map = match element {
                    ElementKind::Vertex => &mut self.vertex_labels,
                    ElementKind::Edge => &mut self.edge_labels,
                };
                match map.get(label) {
                    Some(&index) => index,
                    None => {
                        let index = self.labels.len();
                        self.labels
                            .push(LabelAcc::new(element, Some(CompactString::from(label))));
                        map.insert(CompactString::from(label), index);
                        index
                    }
                }
            }
        };
        self.last[slot] = Some(index);
        index
    }

    /// Counts one element of label `label` and whether its properties are read (every
    /// `step`-th element of the label, the first one always).
    fn take(&mut self, label: usize, step: u64) -> bool {
        let Some(acc) = self.labels.get_mut(label) else {
            return false;
        };
        acc.seen += 1;
        (acc.seen - 1).is_multiple_of(step)
    }

    /// The row of `key` in label `label`, created on first use.
    fn row_index(&mut self, label: usize, key: &CompactString) -> usize {
        let Some(acc) = self.labels.get_mut(label) else {
            return usize::MAX;
        };
        if let Some(&row) = acc.keys.get(key.as_str()) {
            return row;
        }
        let row = self.rows.len();
        self.rows.push((label, RowAcc::new(key.clone())));
        acc.keys.insert(key.clone(), row);
        row
    }

    /// Adds one element's property map to label `label`.
    fn add_map(&mut self, label: usize, map: &crate::value::PropertyMap) {
        let own = map.own_heap_bytes();
        let len = map.len() as u64;
        if let Some((shape, values)) = map.shaped_parts().filter(|(_, values)| !values.is_empty()) {
            let pointer = Arc::as_ptr(shape) as usize;
            let entry = self.shape_entry(label, pointer, shape);
            for (slot, value) in values.iter().enumerate() {
                let row = self
                    .labels
                    .get(label)
                    .and_then(|acc| acc.shape_rows.get(entry))
                    .and_then(|rows| rows.rows.get(slot))
                    .copied();
                if let Some((_, acc)) = row.and_then(|row| self.rows.get_mut(row)) {
                    acc.add(value);
                }
            }
        } else if !map.is_empty() {
            if let Some(acc) = self.labels.get_mut(label) {
                acc.unshaped += 1;
            }
            for (key, value) in map.iter() {
                let row = self.row_index(label, key);
                if let Some((_, acc)) = self.rows.get_mut(row) {
                    acc.add(value);
                }
            }
        }
        if let Some(acc) = self.labels.get_mut(label) {
            acc.values += len;
            acc.overhead += own.saturating_sub(len * SLOT);
        }
    }

    /// The index of `shape`'s rows within label `label` (counting one more element), mapping its
    /// keys to rows when it is met for the first time.
    fn shape_entry(&mut self, label: usize, pointer: usize, shape: &Arc<Shape>) -> usize {
        let known = self.labels.get(label).and_then(|acc| match acc.last_shape {
            Some((last, entry)) if last == pointer => Some(entry),
            _ => acc.shapes.get(&pointer).copied(),
        });
        let entry = match known {
            Some(entry) => entry,
            None => {
                let rows: Vec<usize> = shape
                    .keys
                    .iter()
                    .map(|key| self.row_index(label, key))
                    .collect();
                let Some(acc) = self.labels.get_mut(label) else {
                    return usize::MAX;
                };
                let entry = acc.shape_rows.len();
                acc.shape_rows.push(ShapeRows {
                    shape: Arc::clone(shape),
                    rows,
                    elements: 0,
                });
                acc.shapes.insert(pointer, entry);
                entry
            }
        };
        if let Some(acc) = self.labels.get_mut(label) {
            acc.last_shape = Some((pointer, entry));
            if let Some(rows) = acc.shape_rows.get_mut(entry) {
                rows.elements += 1;
            }
        }
        entry
    }
}

/// The heap bytes of an id of `len` bytes (kept inline up to 24 bytes).
fn id_heap(len: usize) -> u64 {
    if len > INLINE_ID { len as u64 } else { 0 }
}

/// Computes the memory report of `storage` (see the module docs) and whether the walk was
/// cancelled (the report then covers what the walk passed). The request is validated by
/// the caller.
pub(crate) fn compute<S: GraphStorage>(
    storage: &S,
    request: &MemoryReportRequest,
) -> (MemoryReport, bool) {
    let started = web_time::Instant::now();
    let vertex_total = storage.vertex_count() as u64;
    let total = vertex_total + storage.edge_count() as u64;
    let step = request.step_for(total);
    let cancel = request.cancel_token();
    let mut walk = Walk::default();
    let mut passed = 0u64;
    let mut visited = 0u64;
    let mut cancelled = false;

    let tick = |passed: u64| -> bool {
        if passed.is_multiple_of(CANCEL_EVERY) && cancel.is_some_and(|token| token.is_cancelled()) {
            return false;
        }
        if passed.is_multiple_of(PROGRESS_EVERY) {
            request.report_progress(passed, total);
        }
        true
    };

    for vertex in storage.vertices() {
        passed += 1;
        if !tick(passed) {
            cancelled = true;
            break;
        }
        let labels = storage.vertex_labels(vertex);
        let label = walk.label_index(ElementKind::Vertex, labels.first().map(|l| l.as_str()));
        if !walk.take(label, step) {
            continue;
        }
        visited += 1;
        let out = storage.out_edges(vertex).count() as u64;
        let incoming = storage.in_edges(vertex).count() as u64;
        if let Some(acc) = walk.labels.get_mut(label) {
            acc.visited += 1;
            acc.id_heap += storage.vertex_id(vertex).map_or(0, |id| id_heap(id.len()));
            acc.label_bytes += labels.len() as u64 * INLINE_ID as u64
                + labels
                    .iter()
                    .map(crate::value::compact_string_heap_bytes)
                    .sum::<u64>();
            acc.max_out = acc.max_out.max(out);
            acc.max_in = acc.max_in.max(incoming);
            acc.degrees
                .push(u32::try_from(out + incoming).unwrap_or(u32::MAX));
        }
        if let Some(map) = storage.vertex_properties(vertex) {
            walk.add_map(label, map);
        }
    }
    if !cancelled {
        for edge in storage.edges() {
            passed += 1;
            if !tick(passed) {
                cancelled = true;
                break;
            }
            let label = walk.label_index(ElementKind::Edge, storage.edge_label(edge));
            if !walk.take(label, step) {
                continue;
            }
            visited += 1;
            if let Some(acc) = walk.labels.get_mut(label) {
                acc.visited += 1;
                acc.id_heap += storage.edge_id(edge).map_or(0, |id| id_heap(id.len()));
            }
            if let Some(map) = storage.edge_properties(edge) {
                walk.add_map(label, map);
            }
        }
    }
    request.report_progress(passed, total);

    let structure = storage.memory_structure();
    let mut report = fold(storage, walk, structure.as_ref());
    report.structure = structure;
    report.sample_every = step;
    report.sampled = visited < passed;
    report.visited_elements = visited;
    report.path_depth = request.path_depth();
    report.commit_seq = storage.last_commit_seq();
    report.tracked_live_bytes = crate::traversal::memory_budget::tracked_live_bytes();
    if let Some(cap) = request.max_rows()
        && report.properties.len() > cap
    {
        report.rows_omitted = (report.properties.len() - cap) as u64;
        report.properties.truncate(cap);
    }
    if let Some(cap) = request.max_labels()
        && report.labels.len() > cap
    {
        // The labels with the most bytes, of both kinds.
        report
            .labels
            .sort_by(|a, b| label_bytes(b).cmp(&label_bytes(a)));
        report.labels_omitted = (report.labels.len() - cap) as u64;
        report.labels_omitted_property_bytes = report.labels[cap..]
            .iter()
            .map(|label| label.property_bytes)
            .sum();
        report.labels.truncate(cap);
        report.labels.sort_by(label_order);
    }
    report.elapsed_micros = started.elapsed().as_micros() as u64;
    (report, cancelled)
}

/// What a label row costs, for its order: its property bytes plus its slots.
fn label_bytes(label: &LabelMemory) -> u64 {
    label.property_bytes + label.slot_bytes.unwrap_or(0)
}

/// The order of the label rows: vertices first, then most bytes, then by label.
fn label_order(a: &LabelMemory, b: &LabelMemory) -> std::cmp::Ordering {
    kind_slot(a.element)
        .cmp(&kind_slot(b.element))
        .then_with(|| label_bytes(b).cmp(&label_bytes(a)))
        .then_with(|| a.label.cmp(&b.label))
}

/// Turns the accumulators into the report: scales sampled figures per label, folds shapes into
/// rows, attaches rule and index names, sorts by bytes.
fn fold<S: GraphStorage>(
    storage: &S,
    walk: Walk,
    structure: Option<&super::StructureMemory>,
) -> MemoryReport {
    let indexes = storage.property_indexes();
    let rules: Vec<RuleMemory> = storage
        .definitions()
        .filter(|definition| definition.kind() == DefinitionKind::COMPRESSION)
        .filter_map(|definition| {
            let rule = definition.as_compression()?;
            Some(RuleMemory {
                name: definition.name().to_string(),
                target: PropertyTarget::new(
                    rule.element(),
                    Some(rule.label()),
                    rule.path().clone(),
                ),
                codec: rule.codec_name().to_string(),
                stats: storage.compression_stats(definition.name()),
            })
        })
        .collect();

    // The scale of each label: elements passed over elements read (1 for a full walk; a
    // cancelled walk scales what it passed).
    let scales: Vec<(u64, f64)> = walk
        .labels
        .iter()
        .map(|acc| {
            if acc.visited == 0 || acc.seen == acc.visited {
                (acc.seen, 1.0)
            } else {
                (acc.seen, acc.seen as f64 / acc.visited as f64)
            }
        })
        .collect();
    let scale = |label: usize, raw: u64| -> u64 {
        match scales.get(label) {
            Some((_, factor)) if *factor != 1.0 => (raw as f64 * factor).round() as u64,
            _ => raw,
        }
    };

    let mut properties: Vec<PropertyMemory> = Vec::with_capacity(walk.rows.len());
    let mut label_property_bytes = vec![0u64; walk.labels.len()];
    for (label, row) in &walk.rows {
        let Some(acc) = walk.labels.get(*label) else {
            continue;
        };
        let target = PropertyTarget::top_level(acc.element, acc.label.as_deref(), &row.key);
        let mut value_types: Vec<ValueTypeCount> = row
            .types
            .iter()
            .zip(TYPE_NAMES)
            .filter(|(count, _)| **count > 0)
            .map(|(count, name)| ValueTypeCount {
                name: name.to_string(),
                count: scale(*label, *count),
            })
            .collect();
        value_types.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.name.cmp(&b.name)));
        let sizes: Vec<SizeBucket> = row
            .sizes
            .iter()
            .enumerate()
            .map(|(index, (values, bytes))| SizeBucket {
                max_bytes: SIZE_BUCKET_BOUNDS.get(index).copied(),
                values: scale(*label, *values),
                bytes: scale(*label, *bytes),
            })
            .collect();
        let strings = row.strings.as_ref().map(|strings| StringMemory {
            values: scale(*label, strings.values),
            inline_values: scale(*label, strings.inline),
            heap_bytes: scale(*label, strings.heap),
            plain_bytes: scale(*label, strings.plain),
            distinct_estimate: strings.distinct.estimate(),
            distinct_reliable: acc.seen == acc.visited,
        });
        let vectors = row
            .vectors
            .iter()
            .map(|&(element, dimension, count, bytes)| VectorMemory {
                element,
                dimension,
                count: scale(*label, count),
                bytes: scale(*label, bytes),
            })
            .collect();
        let key = row.key.as_str();
        let label_text = acc.label.as_deref();
        let row_rules = rules
            .iter()
            .filter(|rule| {
                rule.target.element == acc.element
                    && rule.target.label.as_deref() == label_text
                    && matches!(
                        rule.target.path.segments().first(),
                        Some(crate::value::json_path::JsonPathSegment::Key(first)) if first == key
                    )
            })
            .map(|rule| rule.name.clone())
            .collect();
        let row_indexes = indexes
            .iter()
            .filter(|index| {
                index.element == acc.element
                    && Some(index.label.as_str()) == label_text
                    && index.key == key
            })
            .map(|index| index.name.clone())
            .collect();
        let memory = PropertyMemory {
            target,
            leaf: row.nested == 0,
            nested_values: scale(*label, row.nested),
            elements: scale(*label, row.elements),
            label_elements: scales.get(*label).map_or(0, |(count, _)| *count),
            value_types,
            slot_bytes: scale(*label, row.elements * SLOT),
            heap_bytes: scale(*label, row.heap),
            sizes,
            max_value_bytes: row.max,
            strings,
            vectors,
            compressed_values: scale(*label, row.compressed.values),
            compressed_plain_bytes: scale(*label, row.compressed.plain),
            compressed_stored_bytes: scale(*label, row.compressed.stored),
            deduped_values: scale(*label, row.compressed.deduped),
            deduped_plain_bytes: scale(*label, row.compressed.deduped_plain),
            rules: row_rules,
            indexes: row_indexes,
        };
        if let Some(sum) = label_property_bytes.get_mut(*label) {
            *sum += memory.bytes();
        }
        properties.push(memory);
    }
    properties.sort_by(|a, b| {
        b.bytes()
            .cmp(&a.bytes())
            .then_with(|| a.target.to_string().cmp(&b.target.to_string()))
    });

    // Shapes: elements per distinct shape over every label.
    let mut per_shape: HashMap<usize, u64> = HashMap::new();
    let mut shaped_elements = 0u64;
    let mut unshaped_elements = 0u64;
    for (index, acc) in walk.labels.iter().enumerate() {
        for rows in &acc.shape_rows {
            let elements = scale(index, rows.elements);
            *per_shape
                .entry(Arc::as_ptr(&rows.shape) as usize)
                .or_default() += elements;
            shaped_elements += elements;
        }
        unshaped_elements += scale(index, acc.unshaped);
    }
    let mut histogram: Vec<ShapeBucket> = Vec::new();
    for &elements in per_shape.values() {
        // Buckets 1, 3, 7, 15, ...: the smallest 2^n - 1 >= elements.
        let max_elements = (elements.max(1) + 1).next_power_of_two() - 1;
        match histogram
            .iter_mut()
            .find(|bucket| bucket.max_elements == max_elements)
        {
            Some(bucket) => {
                bucket.shapes += 1;
                bucket.elements += elements;
            }
            None => histogram.push(ShapeBucket {
                max_elements,
                shapes: 1,
                elements,
            }),
        }
    }
    histogram.sort_by_key(|bucket| bucket.max_elements);

    let slot_size = |element: ElementKind| {
        structure.map(|structure| match element {
            ElementKind::Vertex => structure.vertex_slots.slot_bytes,
            ElementKind::Edge => structure.edge_slots.slot_bytes,
        })
    };
    let mut labels: Vec<LabelMemory> = walk
        .labels
        .into_iter()
        .enumerate()
        .map(|(index, mut acc)| {
            let elements = scales.get(index).map_or(acc.visited, |(count, _)| *count);
            let overhead = scale(index, acc.overhead);
            let degree = (acc.element == ElementKind::Vertex).then(|| {
                let edge_ends: u64 = acc.degrees.iter().map(|&d| u64::from(d)).sum();
                let p95 = if acc.degrees.is_empty() {
                    0
                } else {
                    let rank = ((acc.degrees.len() as f64) * 0.95).ceil() as usize;
                    let rank = rank.clamp(1, acc.degrees.len()) - 1;
                    let (_, value, _) = acc.degrees.select_nth_unstable(rank);
                    u64::from(*value)
                };
                DegreeStats {
                    max_out: acc.max_out,
                    max_in: acc.max_in,
                    max: acc.degrees.iter().copied().max().map_or(0, u64::from),
                    p95,
                    edge_ends: scale(index, edge_ends),
                }
            });
            LabelMemory {
                element: acc.element,
                label: acc.label,
                elements,
                id_heap_bytes: scale(index, acc.id_heap),
                label_bytes: scale(index, acc.label_bytes),
                slot_bytes: slot_size(acc.element).map(|size| size * elements),
                property_bytes: label_property_bytes.get(index).copied().unwrap_or(0) + overhead,
                map_overhead_bytes: overhead,
                values: scale(index, acc.values),
                degree,
            }
        })
        .collect();
    labels.sort_by(label_order);

    MemoryReport {
        properties,
        labels,
        shapes: ShapeSummary {
            shapes: per_shape.len() as u64,
            shaped_elements,
            unshaped_elements,
            histogram,
        },
        indexes,
        compression_rules: rules,
        ..MemoryReport::default()
    }
}
