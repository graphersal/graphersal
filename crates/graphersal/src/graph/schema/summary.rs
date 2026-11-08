//! The schema summary: a compact text map of a graph for a reader that has to learn it first
//! (an LLM behind the MCP endpoint, a person at the REPL): the labels with their element counts,
//! each label's top-level keys with their value types and how often they occur, the endpoint
//! label pairs of every edge label, the property indexes and the schema mode, with the stored
//! schema's declarations laid over it (descriptions, enums, declared labels and keys without
//! data). The raw schema JSON of a large graph is far larger (the field store of
//! 2026-11-09: 204 KB of JSON for 124 labels) and says nothing about counts.
//!
//! One walk over the read API of any [`GraphStorage`]: every element is passed once (its label,
//! its property map's shape, an edge's endpoint labels), so the element counts, the key counts
//! and the endpoint pair counts are EXACT. Only the value types are sampled per label: the
//! first [`SchemaSummaryRequest::type_samples`] elements of a label, then every second, every
//! fourth, ... (about that many more per doubling), plus the first element of every distinct
//! key set, so every key has at least one observed type. The walk writes nothing, checks the
//! cancel token every 1024 elements and allocates per label and per distinct key set, never
//! per element.

use std::collections::HashMap;
use std::fmt::{self, Write as _};
use std::sync::Arc;

use compact_str::CompactString;
use indexmap::IndexMap;

use crate::graph::GraphStorage;
use crate::graph::schema::{ElementKind, GraphSchema, JsonType, PropertySchema, SchemaFormat};
use crate::traversal::cancel::CancelToken;
use crate::value::ElementProperty;
use crate::value::PropertyMap;
use crate::value::property_map::Shape;
use crate::value::temporal::TemporalKind;
use crate::value::vector::VectorElement;

/// How many elements the walk passes between two cancellation checks.
const CANCEL_EVERY: u64 = 1024;
/// How many list items are looked at for a list's item types.
const LIST_ITEMS: usize = 16;
/// The most endpoint pairs one edge label line lists.
const MAX_PAIRS: usize = 12;
/// The most characters of a label description in the whole-graph summary.
const LABEL_DESCRIPTION_CHARS: usize = 80;
/// The most characters of a description in a label's detail.
const DETAIL_DESCRIPTION_CHARS: usize = 400;
/// The most enum values a declared key lists.
const MAX_ENUM_VALUES: usize = 8;
/// The fewest labels of a kind whose shared keys are written once ("every vertex label has").
const COMMON_KEYS_LABELS: usize = 4;
/// The most labels an "every label except" line names.
const MAX_EXCEPTIONS: usize = 10;
/// The text of a missing label.
const NO_LABEL: &str = "(no label)";

/// What a [`SchemaSummary`] covers and how much it shows. The defaults fit a large graph (the
/// field store of 124 labels and 935 label keys) in about 20,000 characters.
///
/// ```
/// use graphersal::prelude::*;
/// use graphersal::schema::SchemaSummaryRequest;
///
/// let graph = TraversalGraph::tinkerpop_modern();
/// let summary = graph.traversal().schema_summary(&SchemaSummaryRequest::new()).unwrap();
/// let text = summary.to_string();
/// assert!(text.contains("person 4: age int, name str"), "{text}");
/// assert!(text.contains("created 4 person->software: weight float"), "{text}");
/// ```
#[derive(Debug, Clone)]
pub struct SchemaSummary {
    vertices: u64,
    edges: u64,
    vertex_labels: Vec<LabelSummary>,
    edge_labels: Vec<LabelSummary>,
    schema: Option<GraphSchema>,
    indexes: Vec<IndexLine>,
    sampled_elements: u64,
    elapsed_micros: u64,
    max_keys: usize,
    max_labels: usize,
    labels: Vec<String>,
    descriptions: bool,
}

/// The options of a [`SchemaSummary`]: caps, the labels to detail, descriptions, cancellation.
#[derive(Debug, Clone)]
pub struct SchemaSummaryRequest {
    max_keys: usize,
    max_labels: usize,
    type_samples: u64,
    labels: Vec<String>,
    descriptions: bool,
    cancel: Option<CancelToken>,
}

impl Default for SchemaSummaryRequest {
    fn default() -> Self {
        Self::new()
    }
}

impl SchemaSummaryRequest {
    /// The most keys one label line lists by default (the most frequent; the rest are counted).
    pub const DEFAULT_MAX_KEYS: usize = 30;
    /// The most labels of each kind listed by default (the largest; the rest are counted).
    pub const DEFAULT_MAX_LABELS: usize = 200;
    /// The elements per label whose values are read for their types by default, before the
    /// sampling thins out (see the module docs of the summary).
    pub const DEFAULT_TYPE_SAMPLES: u64 = 1000;

    /// The whole graph with the default caps, label descriptions on.
    pub fn new() -> Self {
        Self {
            max_keys: Self::DEFAULT_MAX_KEYS,
            max_labels: Self::DEFAULT_MAX_LABELS,
            type_samples: Self::DEFAULT_TYPE_SAMPLES,
            labels: Vec::new(),
            descriptions: true,
            cancel: None,
        }
    }

    /// Lists at most `n` keys per label in the whole-graph summary (`usize::MAX`: all; at least
    /// 1). A label's detail ([`Self::with_labels`]) lists every key.
    pub fn with_max_keys(mut self, n: usize) -> Self {
        self.max_keys = n.max(1);
        self
    }

    /// Lists at most `n` labels of each kind (`usize::MAX`: all; at least 1).
    pub fn with_max_labels(mut self, n: usize) -> Self {
        self.max_labels = n.max(1);
        self
    }

    /// Reads the values of the first `n` elements of each label for their types, then thins
    /// out (at least 1).
    pub fn with_type_samples(mut self, n: u64) -> Self {
        self.type_samples = n.max(1);
        self
    }

    /// Details these labels only (vertex or edge labels, case-sensitive): every key with its
    /// declared description, and for a vertex label the edges going out and coming in.
    pub fn with_labels<I, L>(mut self, labels: I) -> Self
    where
        I: IntoIterator<Item = L>,
        L: Into<String>,
    {
        self.labels = labels.into_iter().map(Into::into).collect();
        self
    }

    /// Shows the stored schema's descriptions (`true` by default): label descriptions, cut,
    /// in the whole-graph summary; label and key descriptions in a label's detail.
    pub fn with_descriptions(mut self, on: bool) -> Self {
        self.descriptions = on;
        self
    }

    /// Stops the walk when `token` is cancelled (the summary then fails with
    /// `TraverserError::Cancelled`).
    pub fn with_cancel(mut self, token: CancelToken) -> Self {
        self.cancel = Some(token);
        self
    }

    /// The keys listed per label.
    pub fn max_keys(&self) -> usize {
        self.max_keys
    }

    /// The labels listed per kind.
    pub fn max_labels(&self) -> usize {
        self.max_labels
    }

    /// The elements per label read for their value types before the sampling thins out.
    pub fn type_samples(&self) -> u64 {
        self.type_samples
    }

    /// The labels to detail (empty: the whole graph).
    pub fn labels(&self) -> &[String] {
        &self.labels
    }

    /// Whether descriptions are shown.
    pub fn descriptions(&self) -> bool {
        self.descriptions
    }

    /// The cancel token, if any.
    pub fn cancel_token(&self) -> Option<&CancelToken> {
        self.cancel.as_ref()
    }
}

impl SchemaSummary {
    /// The number of vertices the walk passed (every one).
    pub fn vertex_count(&self) -> u64 {
        self.vertices
    }

    /// The number of edges the walk passed (every one).
    pub fn edge_count(&self) -> u64 {
        self.edges
    }

    /// The number of elements whose values were read for their types.
    pub fn sampled_elements(&self) -> u64 {
        self.sampled_elements
    }

    /// How long the walk took, in microseconds.
    pub fn elapsed_micros(&self) -> u64 {
        self.elapsed_micros
    }
}

/// The value type buckets, in the order of [`type_bit`].
const TYPE_NAMES: [&str; 16] = [
    "null",
    "bool",
    "int",
    "float",
    "str",
    "uuid",
    "date",
    "localdatetime",
    "datetime",
    "time",
    "duration",
    "offsettime",
    "vector",
    "list",
    "map",
    "element",
];
const LIST_BIT: u16 = 1 << 13;
const VECTOR_BIT: u16 = 1 << 12;

/// The bucket of [`TYPE_NAMES`] of a value's logical type (a compressed or shared string is a
/// `str`).
fn type_bit(value: &ElementProperty) -> u16 {
    let index = match value {
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
        ElementProperty::Object(_) | ElementProperty::Map(_) | ElementProperty::Blob(_) => 14,
        ElementProperty::Vertex(_) | ElementProperty::Edge(_) => 15,
    };
    1 << index
}

/// The value types seen at one key.
#[derive(Debug, Clone, Default)]
struct Types {
    bits: u16,
    items: u16,
    /// The vector element and dimension (`None` dimension once two differ).
    vector: Option<(VectorElement, Option<u32>)>,
}

impl Types {
    fn add(&mut self, value: &ElementProperty) {
        self.bits |= type_bit(value);
        match value {
            ElementProperty::Array(items) => {
                for item in items.iter().take(LIST_ITEMS) {
                    self.items |= type_bit(item);
                }
            }
            ElementProperty::Vector(vector) => {
                let (element, dimension) = (vector.element(), vector.dimension() as u32);
                self.vector = match self.vector {
                    None => Some((element, Some(dimension))),
                    Some((seen, Some(d))) if seen == element && d == dimension => {
                        Some((seen, Some(d)))
                    }
                    Some((seen, _)) => Some((seen, None)),
                };
            }
            _ => {}
        }
    }

    fn text(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        for (index, name) in TYPE_NAMES.iter().enumerate() {
            let bit = 1u16 << index;
            if self.bits & bit == 0 {
                continue;
            }
            parts.push(match bit {
                LIST_BIT => {
                    let items = Types {
                        bits: self.items,
                        ..Types::default()
                    };
                    format!("[{}]", items.text())
                }
                VECTOR_BIT => match self.vector {
                    Some((element, Some(dimension))) => {
                        format!("vector<{},{dimension}>", element.name())
                    }
                    Some((element, None)) => format!("vector<{}>", element.name()),
                    None => "vector".to_string(),
                },
                _ => (*name).to_string(),
            });
        }
        parts.join("|")
    }
}

/// One label while walking.
struct LabelAcc {
    label: Option<CompactString>,
    count: u64,
    sampled: u64,
    shapes: Vec<(Arc<Shape>, u64)>,
    shape_index: HashMap<usize, usize>,
    last_shape: Option<(usize, usize)>,
    /// Key counts of maps without a shape.
    loose: IndexMap<CompactString, u64>,
    types: HashMap<CompactString, Types>,
    /// Vertices: the other labels of the vertices of this primary label, with counts.
    extra_labels: Vec<(CompactString, u64)>,
    extra_index: HashMap<CompactString, usize>,
    /// Edges: the endpoint primary label pairs, with counts.
    pairs: Vec<(Option<CompactString>, Option<CompactString>, u64)>,
    pair_index: HashMap<(Option<CompactString>, Option<CompactString>), usize>,
    last_pair: usize,
}

impl LabelAcc {
    fn new(label: Option<&str>) -> Self {
        LabelAcc {
            label: label.map(CompactString::from),
            count: 0,
            sampled: 0,
            shapes: Vec::new(),
            shape_index: HashMap::new(),
            last_shape: None,
            loose: IndexMap::new(),
            types: HashMap::new(),
            extra_labels: Vec::new(),
            extra_index: HashMap::new(),
            pairs: Vec::new(),
            pair_index: HashMap::new(),
            last_pair: 0,
        }
    }

    /// Counts one more element; whether its values are read for their types.
    fn pass(&mut self, samples: u64) -> bool {
        self.count += 1;
        if self.count <= samples {
            return true;
        }
        let stride = (self.count / samples).next_power_of_two();
        self.count.is_multiple_of(stride)
    }

    fn observe(&mut self, map: &PropertyMap) {
        self.sampled += 1;
        for (key, value) in map.iter() {
            match self.types.get_mut(key.as_str()) {
                Some(types) => types.add(value),
                None => {
                    let mut types = Types::default();
                    types.add(value);
                    self.types.insert(key.clone(), types);
                }
            }
        }
    }

    fn add_map(&mut self, map: &PropertyMap, sample: bool) {
        if let Some(shape) = map.stored_shape() {
            let pointer = Arc::as_ptr(shape) as usize;
            let known = match self.last_shape {
                Some((last, index)) if last == pointer => Some(index),
                _ => self.shape_index.get(&pointer).copied(),
            };
            let new = match known {
                Some(index) => {
                    if let Some(entry) = self.shapes.get_mut(index) {
                        entry.1 += 1;
                    }
                    self.last_shape = Some((pointer, index));
                    false
                }
                None => {
                    let index = self.shapes.len();
                    self.shapes.push((Arc::clone(shape), 1));
                    self.shape_index.insert(pointer, index);
                    self.last_shape = Some((pointer, index));
                    true
                }
            };
            if new || sample {
                self.observe(map);
            }
        } else {
            let mut new = false;
            for key in map.keys() {
                match self.loose.get_mut(key.as_str()) {
                    Some(count) => *count += 1,
                    None => {
                        self.loose.insert(key.clone(), 1);
                        new = true;
                    }
                }
            }
            if new || sample {
                self.observe(map);
            }
        }
    }

    fn add_extra_label(&mut self, label: &CompactString) {
        match self.extra_index.get(label.as_str()) {
            Some(&index) => {
                if let Some(entry) = self.extra_labels.get_mut(index) {
                    entry.1 += 1;
                }
            }
            None => {
                self.extra_index
                    .insert(label.clone(), self.extra_labels.len());
                self.extra_labels.push((label.clone(), 1));
            }
        }
    }

    fn add_pair(&mut self, from: Option<&CompactString>, to: Option<&CompactString>) {
        if let Some(entry) = self.pairs.get_mut(self.last_pair)
            && entry.0.as_ref() == from
            && entry.1.as_ref() == to
        {
            entry.2 += 1;
            return;
        }
        // A new pair allocates its key once; a known one is found by a hash lookup.
        let key = (from.cloned(), to.cloned());
        match self.pair_index.get(&key) {
            Some(&index) => {
                if let Some(entry) = self.pairs.get_mut(index) {
                    entry.2 += 1;
                }
                self.last_pair = index;
            }
            None => {
                self.last_pair = self.pairs.len();
                self.pair_index.insert(key, self.last_pair);
                self.pairs.push((from.cloned(), to.cloned(), 1));
            }
        }
    }

    fn finish(self) -> LabelSummary {
        let mut counts: IndexMap<CompactString, u64> = self.loose;
        for (shape, elements) in &self.shapes {
            for key in shape.keys.iter() {
                *counts.entry(key.clone()).or_insert(0) += elements;
            }
        }
        let mut types = self.types;
        let mut keys: Vec<KeySummary> = counts
            .into_iter()
            .filter(|(_, count)| *count > 0)
            .map(|(key, count)| {
                let types = types.remove(key.as_str()).unwrap_or_default();
                KeySummary { key, count, types }
            })
            .collect();
        keys.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.key.cmp(&b.key)));
        let mut extra_labels = self.extra_labels;
        extra_labels.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let mut pairs = self.pairs;
        pairs.sort_by(|a, b| {
            b.2.cmp(&a.2)
                .then_with(|| a.0.cmp(&b.0))
                .then_with(|| a.1.cmp(&b.1))
        });
        LabelSummary {
            label: self.label,
            count: self.count,
            keys,
            extra_labels,
            pairs,
        }
    }
}

/// The labels of one kind while walking.
#[derive(Default)]
struct Labels {
    accs: Vec<LabelAcc>,
    index: HashMap<CompactString, usize>,
    unlabeled: Option<usize>,
    last: Option<usize>,
}

impl Labels {
    fn get(&mut self, label: Option<&str>) -> usize {
        if let Some(last) = self.last
            && let Some(acc) = self.accs.get(last)
            && acc.label.as_deref() == label
        {
            return last;
        }
        let index = match label {
            None => match self.unlabeled {
                Some(index) => index,
                None => {
                    let index = self.accs.len();
                    self.accs.push(LabelAcc::new(None));
                    self.unlabeled = Some(index);
                    index
                }
            },
            Some(label) => match self.index.get(label) {
                Some(&index) => index,
                None => {
                    let index = self.accs.len();
                    self.accs.push(LabelAcc::new(Some(label)));
                    self.index.insert(CompactString::from(label), index);
                    index
                }
            },
        };
        self.last = Some(index);
        index
    }

    fn finish(self) -> Vec<LabelSummary> {
        let mut labels: Vec<LabelSummary> = self.accs.into_iter().map(LabelAcc::finish).collect();
        labels.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.label.cmp(&b.label)));
        labels
    }
}

/// One label of the finished summary.
#[derive(Debug, Clone)]
struct LabelSummary {
    label: Option<CompactString>,
    count: u64,
    keys: Vec<KeySummary>,
    extra_labels: Vec<(CompactString, u64)>,
    pairs: Vec<(Option<CompactString>, Option<CompactString>, u64)>,
}

/// One key of a label.
#[derive(Debug, Clone)]
struct KeySummary {
    key: CompactString,
    count: u64,
    types: Types,
}

/// One property index, for the header.
#[derive(Debug, Clone)]
struct IndexLine {
    label: Option<String>,
    text: String,
}

/// Walks `storage` (see the module docs); `Ok(None)` when the request's token was cancelled.
pub(crate) fn compute<S: GraphStorage>(
    storage: &S,
    request: &SchemaSummaryRequest,
) -> Result<Option<SchemaSummary>, crate::error::GraphError> {
    let started = web_time::Instant::now();
    let cancel = request.cancel.as_ref();
    let samples = request.type_samples;
    let cancelled = |passed: u64| {
        passed.is_multiple_of(CANCEL_EVERY) && cancel.is_some_and(CancelToken::is_cancelled)
    };
    let mut vertex_labels = Labels::default();
    let mut edge_labels = Labels::default();
    let (mut vertices, mut edges, mut passed) = (0u64, 0u64, 0u64);
    for vertex in storage.vertices()? {
        passed += 1;
        if cancelled(passed) {
            return Ok(None);
        }
        vertices += 1;
        let labels = storage.vertex_labels(vertex)?;
        let index = vertex_labels.get(labels.first().map(CompactString::as_str));
        let Some(acc) = vertex_labels.accs.get_mut(index) else {
            continue;
        };
        for extra in labels.iter().skip(1) {
            acc.add_extra_label(extra);
        }
        let sample = acc.pass(samples);
        if let Some(map) = storage.vertex_properties(vertex)? {
            acc.add_map(map, sample);
        }
    }
    for edge in storage.edges()? {
        passed += 1;
        if cancelled(passed) {
            return Ok(None);
        }
        edges += 1;
        let index = edge_labels.get(storage.edge_label(edge)?.filter(|l| !l.is_empty()));
        let endpoints = match storage.edge_endpoints(edge)? {
            Some((from, to)) => Some((
                storage.vertex_labels(from)?.first(),
                storage.vertex_labels(to)?.first(),
            )),
            None => None,
        };
        let Some(acc) = edge_labels.accs.get_mut(index) else {
            continue;
        };
        let sample = acc.pass(samples);
        if let Some((from, to)) = endpoints {
            acc.add_pair(from, to);
        }
        if let Some(map) = storage.edge_properties(edge)? {
            acc.add_map(map, sample);
        }
    }
    let sampled_elements = vertex_labels
        .accs
        .iter()
        .chain(edge_labels.accs.iter())
        .map(|acc| acc.sampled)
        .sum();
    let indexes = storage
        .property_indexes()?
        .into_iter()
        .map(|info| {
            let mut text = match (&info.element, &info.label) {
                (ElementKind::Vertex, Some(label)) => {
                    format!("{}.{}", name(label), name(&info.key))
                }
                (ElementKind::Edge, Some(label)) => {
                    format!("edge {}.{}", name(label), name(&info.key))
                }
                (ElementKind::Vertex, None) => format!("any vertex.{}", name(&info.key)),
                (ElementKind::Edge, None) => format!("any edge.{}", name(&info.key)),
            };
            if let Some(transform) = &info.transform {
                let _ = write!(text, " {transform}");
            }
            if info.unique {
                text.push_str(" unique");
            }
            IndexLine {
                label: info.label,
                text,
            }
        })
        .collect();
    Ok(Some(SchemaSummary {
        vertices,
        edges,
        vertex_labels: vertex_labels.finish(),
        edge_labels: edge_labels.finish(),
        schema: storage.get_schema()?,
        indexes,
        sampled_elements,
        elapsed_micros: started.elapsed().as_micros() as u64,
        max_keys: request.max_keys,
        max_labels: request.max_labels,
        labels: request.labels.clone(),
        descriptions: request.descriptions,
    }))
}

/// `n` with thousands separators.
fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A label or key as written in the summary: as it is, or in backticks when it holds a
/// character the notation uses (space, comma, colon, ...), or is empty.
fn name(text: &str) -> String {
    let plain = !text.is_empty()
        && text.chars().all(|c| {
            !c.is_whitespace()
                && !matches!(
                    c,
                    ',' | ':' | ';' | '`' | '?' | '>' | '<' | '|' | '(' | ')' | '[' | ']'
                )
        });
    if plain {
        text.to_string()
    } else {
        format!("`{}`", text.replace('`', "\\`"))
    }
}

fn label_name(label: Option<&str>) -> String {
    label.map_or_else(|| NO_LABEL.to_string(), name)
}

/// `text` on one line, cut to `max` characters (with an ellipsis).
fn cut(text: &str, max: usize) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        let mut out: String = flat.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

/// The first sentence of `text` (up to the first ". "), or all of it.
fn first_sentence(text: &str) -> &str {
    match text.find(". ") {
        Some(end) => &text[..=end],
        None => text,
    }
}

/// The type of a declared property, in the summary's notation.
fn declared_type(node: &PropertySchema) -> String {
    if let Some(members) = &node.any_of {
        let parts: Vec<String> = members.iter().map(declared_type).collect();
        return parts.join("|");
    }
    if let Some(vector) = &node.vector {
        return match vector.dimension {
            Some(dimension) => format!("vector<{},{dimension}>", vector.element.name()),
            None => format!("vector<{}>", vector.element.name()),
        };
    }
    let Some(types) = &node.types else {
        return "any".to_string();
    };
    let parts: Vec<String> = types
        .iter()
        .map(|ty| match ty {
            JsonType::Null => "null".to_string(),
            JsonType::Boolean => "bool".to_string(),
            JsonType::Integer => "int".to_string(),
            JsonType::Number => "float".to_string(),
            JsonType::String => match node.format {
                Some(SchemaFormat::Uuid) => "uuid".to_string(),
                Some(SchemaFormat::DateTime) => "datetime".to_string(),
                Some(SchemaFormat::Date) => "date".to_string(),
                Some(SchemaFormat::LocalDateTime) => "localdatetime".to_string(),
                Some(SchemaFormat::Time) => "time".to_string(),
                Some(SchemaFormat::Duration) => "duration".to_string(),
                Some(SchemaFormat::OffsetTime) => "offsettime".to_string(),
                None => "str".to_string(),
            },
            JsonType::Array => match &node.items {
                Some(items) => format!("[{}]", declared_type(items)),
                None => "[]".to_string(),
            },
            JsonType::Object => "map".to_string(),
        })
        .collect();
    parts.join("|")
}

/// The declared enum of a property as ` in(a|b)`, or empty.
fn declared_enum(node: &PropertySchema) -> String {
    let Some(values) = node
        .constraints
        .as_ref()
        .and_then(|c| c.enum_values.as_ref())
    else {
        return String::new();
    };
    let shown: Vec<String> = values
        .iter()
        .take(MAX_ENUM_VALUES)
        .map(|value| match value {
            ElementProperty::String(text) => cut(text, 40),
            other => {
                crate::value::physical::to_json_text(other).unwrap_or_else(|_| "?".to_string())
            }
        })
        .collect();
    let more = values.len().saturating_sub(MAX_ENUM_VALUES);
    if more > 0 {
        format!(" in({}|+{more} more)", shown.join("|"))
    } else {
        format!(" in({})", shown.join("|"))
    }
}

fn description(node: &PropertySchema) -> Option<&str> {
    node.annotations
        .as_ref()
        .and_then(|a| a.description.as_deref())
        .filter(|d| !d.trim().is_empty())
}

/// What the summary knows of one label of one kind: the data and the declaration.
struct LabelView<'a> {
    data: Option<&'a LabelSummary>,
    declared: Option<&'a PropertySchema>,
    label: Option<&'a str>,
}

impl SchemaSummary {
    fn declared(&self, kind: ElementKind, label: Option<&str>) -> Option<&PropertySchema> {
        let (schema, label) = (self.schema.as_ref()?, label?);
        match kind {
            ElementKind::Vertex => schema.vertices.get(label).map(|v| &v.schema),
            ElementKind::Edge => schema.edges.get(label).map(|e| &e.schema),
        }
    }

    fn labels_of(&self, kind: ElementKind) -> &[LabelSummary] {
        match kind {
            ElementKind::Vertex => &self.vertex_labels,
            ElementKind::Edge => &self.edge_labels,
        }
    }

    /// The labels of `kind` in their order: the data's (largest first), then the declared
    /// labels without data.
    fn views(&self, kind: ElementKind) -> Vec<LabelView<'_>> {
        let mut views: Vec<LabelView<'_>> = self
            .labels_of(kind)
            .iter()
            .map(|data| LabelView {
                data: Some(data),
                declared: self.declared(kind, data.label.as_deref()),
                label: data.label.as_deref(),
            })
            .collect();
        if let Some(schema) = &self.schema {
            let declared: Vec<(&String, &PropertySchema)> = match kind {
                ElementKind::Vertex => schema
                    .vertices
                    .iter()
                    .map(|(l, v)| (l, &v.schema))
                    .collect(),
                ElementKind::Edge => schema.edges.iter().map(|(l, e)| (l, &e.schema)).collect(),
            };
            let seen: std::collections::HashSet<&str> = self
                .labels_of(kind)
                .iter()
                .filter_map(|data| data.label.as_deref())
                .collect();
            for (label, node) in declared {
                if !seen.contains(label.as_str()) {
                    views.push(LabelView {
                        data: None,
                        declared: Some(node),
                        label: Some(label.as_str()),
                    });
                }
            }
        }
        views
    }

    /// The key entries of a label (`key type` with its markers), the data's first, then the
    /// declared keys without data; each with its description when `describe`.
    fn key_entries(&self, view: &LabelView<'_>, describe: bool) -> Vec<String> {
        let total = view.data.map_or(0, |data| data.count);
        let mut entries = Vec::new();
        let declared_key = |key: &str| view.declared.and_then(|node| node.properties.get(key));
        if let Some(data) = view.data {
            for key in &data.keys {
                let mut entry = format!("{} {}", name(&key.key), key.types.text());
                if key.count < total {
                    entry.push('?');
                    if key.count * 100 < total {
                        let _ = write!(entry, " n={}", thousands(key.count));
                    } else if key.count * 10 < total {
                        let _ = write!(entry, " {}%", key.count * 100 / total.max(1));
                    }
                }
                if let Some(node) = declared_key(&key.key) {
                    entry.push_str(&declared_enum(node));
                    if describe && let Some(text) = description(node) {
                        let _ = write!(entry, " \"{}\"", cut(text, DETAIL_DESCRIPTION_CHARS));
                    }
                }
                entries.push(entry);
            }
        }
        if let Some(node) = view.declared {
            for (key, property) in &node.properties {
                if view
                    .data
                    .is_some_and(|data| data.keys.iter().any(|k| k.key == key.as_str()))
                {
                    continue;
                }
                let mut entry = format!(
                    "{} {}{} (declared, no data)",
                    name(key),
                    declared_type(property),
                    declared_enum(property)
                );
                if describe && let Some(text) = description(property) {
                    let _ = write!(entry, " \"{}\"", cut(text, DETAIL_DESCRIPTION_CHARS));
                }
                entries.push(entry);
            }
        }
        entries
    }

    /// The endpoint pairs of an edge label: the data's, then the declared ones without data.
    fn pair_entries(&self, view: &LabelView<'_>) -> Vec<String> {
        let mut entries: Vec<String> = Vec::new();
        let pairs = view.data.map_or(&[][..], |data| data.pairs.as_slice());
        let single = pairs.len() == 1;
        for (from, to, count) in pairs {
            let mut entry = format!(
                "{}->{}",
                label_name(from.as_deref()),
                label_name(to.as_deref())
            );
            if !single {
                let _ = write!(entry, " {}", thousands(*count));
            }
            entries.push(entry);
        }
        if let (Some(schema), Some(label)) = (&self.schema, view.label)
            && let Some(edge) = schema.edges.get(label)
        {
            for connection in &edge.connections {
                let seen = pairs.iter().any(|(from, to, _)| {
                    from.as_deref() == Some(connection.from.as_str())
                        && to.as_deref() == Some(connection.to.as_str())
                });
                if !seen {
                    entries.push(format!(
                        "{}->{} 0",
                        name(&connection.from),
                        name(&connection.to)
                    ));
                }
            }
        }
        entries
    }

    /// The key entries most labels of `kind` share (each in at least 80 % of the labels with
    /// data, at least [`COMMON_KEYS_LABELS`] labels, no detailed labels), written once instead
    /// of on every line, and the labels with data that lack one of them (their lines keep every
    /// key).
    fn common_keys(&self, kind: ElementKind) -> (Vec<String>, Vec<String>) {
        let labels = self.labels_of(kind);
        if labels.len() < COMMON_KEYS_LABELS || !self.labels.is_empty() {
            return (Vec::new(), Vec::new());
        }
        let views = self.views(kind);
        let with_data: Vec<(&LabelView<'_>, Vec<String>)> = views
            .iter()
            .filter(|view| view.data.is_some())
            .map(|view| (view, self.key_entries(view, false)))
            .collect();
        let mut counts: IndexMap<&str, usize> = IndexMap::new();
        for (_, entries) in &with_data {
            for entry in entries {
                *counts.entry(entry.as_str()).or_insert(0) += 1;
            }
        }
        let common: Vec<String> = counts
            .into_iter()
            .filter(|(_, count)| count * 5 >= with_data.len() * 4)
            .map(|(entry, _)| entry.to_string())
            .collect();
        if common.is_empty() {
            return (Vec::new(), Vec::new());
        }
        let without: Vec<String> = with_data
            .iter()
            .filter(|(_, entries)| !common.iter().all(|entry| entries.contains(entry)))
            .map(|(view, _)| label_name(view.label))
            .collect();
        (common, without)
    }

    fn write_label(
        &self,
        out: &mut String,
        kind: ElementKind,
        view: &LabelView<'_>,
        common: &[String],
        detail: bool,
    ) {
        let count = view.data.map_or(0, |data| data.count);
        let _ = write!(out, "{} {}", label_name(view.label), thousands(count));
        if view.data.is_none() {
            out.push_str(" (declared, no data)");
        }
        if let Some(data) = view.data
            && !data.extra_labels.is_empty()
        {
            let extra: Vec<String> = data
                .extra_labels
                .iter()
                .map(|(label, count)| format!("{} {}", name(label), thousands(*count)))
                .collect();
            let _ = write!(out, " (also labelled: {})", extra.join(", "));
        }
        if kind == ElementKind::Edge {
            let pairs = self.pair_entries(view);
            if !pairs.is_empty() {
                out.push(' ');
                if pairs.len() > MAX_PAIRS && !detail {
                    let _ = write!(
                        out,
                        "{}, +{} more pairs",
                        pairs[..MAX_PAIRS].join(", "),
                        pairs.len() - MAX_PAIRS
                    );
                } else {
                    out.push_str(&pairs.join(", "));
                }
            }
        }
        let entries: Vec<String> = self
            .key_entries(view, detail && self.descriptions)
            .into_iter()
            .filter(|entry| !common.contains(entry))
            .collect();
        if !entries.is_empty() {
            out.push_str(": ");
            let shown = if detail {
                entries.len()
            } else {
                entries.len().min(self.max_keys)
            };
            out.push_str(&entries[..shown].join(", "));
            if shown < entries.len() {
                let _ = write!(out, ", ... {} more keys", entries.len() - shown);
            }
        }
        out.push('\n');
        if self.descriptions
            && let Some(text) = view.declared.and_then(description)
        {
            let text = if detail {
                cut(text, DETAIL_DESCRIPTION_CHARS)
            } else {
                cut(first_sentence(text), LABEL_DESCRIPTION_CHARS)
            };
            let _ = writeln!(out, "  {text}");
        }
    }

    /// For a detailed vertex label: the edges going out and coming in, with counts.
    fn write_edges_of(&self, out: &mut String, label: Option<&str>) {
        let mut outgoing = Vec::new();
        let mut incoming = Vec::new();
        for edge in &self.edge_labels {
            for (from, to, count) in &edge.pairs {
                let edge_label = label_name(edge.label.as_deref());
                if from.as_deref() == label {
                    outgoing.push((
                        *count,
                        format!(
                            "{edge_label}->{} {}",
                            label_name(to.as_deref()),
                            thousands(*count)
                        ),
                    ));
                }
                if to.as_deref() == label {
                    incoming.push((
                        *count,
                        format!(
                            "{edge_label}<-{} {}",
                            label_name(from.as_deref()),
                            thousands(*count)
                        ),
                    ));
                }
            }
        }
        for (title, mut list) in [("out", outgoing), ("in", incoming)] {
            if list.is_empty() {
                continue;
            }
            list.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
            let texts: Vec<String> = list.into_iter().map(|(_, text)| text).collect();
            let _ = writeln!(out, "  edges {title}: {}", texts.join(", "));
        }
    }

    fn write_header(&self, out: &mut String) {
        let _ = write!(
            out,
            "graph: {} vertices, {} edges; {} vertex labels, {} edge labels; ",
            thousands(self.vertices),
            thousands(self.edges),
            thousands(self.vertex_labels.len() as u64),
            thousands(self.edge_labels.len() as u64)
        );
        match &self.schema {
            Some(schema) => {
                let _ = writeln!(
                    out,
                    "schema mode {} (stored schema declares {} vertex labels, {} edge labels)",
                    schema.mode.as_str(),
                    schema.vertices.len(),
                    schema.edges.len()
                );
            }
            None => out.push_str("no stored schema (mode none)\n"),
        }
        let indexes: Vec<&str> = self
            .indexes
            .iter()
            .filter(|index| {
                self.labels.is_empty()
                    || index
                        .label
                        .as_ref()
                        .is_none_or(|label| self.labels.contains(label))
            })
            .map(|index| index.text.as_str())
            .collect();
        if !indexes.is_empty() {
            let _ = writeln!(out, "indexes: {}", indexes.join("; "));
        }
        out.push_str(
            "notation: label count: key type, ...; \"?\" = not on every element (\"? 3%\" under \
             10 %, \"? n=12\" under 1 %); types str int float bool uuid date datetime \
             localdatetime time offsettime duration null, [T] list, map, vector<f32,384>, a|b \
             mixed; an edge label line names its from->to vertex label pairs (with counts when \
             several)",
        );
        if self.sampled_elements < self.vertices + self.edges {
            let _ = write!(
                out,
                "; counts are exact, value types sampled from {} of {} elements",
                thousands(self.sampled_elements),
                thousands(self.vertices + self.edges)
            );
        }
        out.push('\n');
    }

    fn write_kind(&self, out: &mut String, kind: ElementKind) {
        let views = self.views(kind);
        let (title, noun) = match kind {
            ElementKind::Vertex => ("vertex labels", "vertices"),
            ElementKind::Edge => ("edge labels", "edges"),
        };
        let _ = writeln!(out, "\n{title}");
        if views.is_empty() {
            out.push_str("(none)\n");
            return;
        }
        let (common, without) = self.common_keys(kind);
        if !common.is_empty() {
            let _ = write!(out, "every {} label", kind_word(kind));
            if !without.is_empty() {
                let shown = without.len().min(MAX_EXCEPTIONS);
                let _ = write!(out, " except {}", without[..shown].join(", "));
                if shown < without.len() {
                    let _ = write!(out, " and {} more", without.len() - shown);
                }
            }
            let _ = writeln!(out, " has: {}", common.join(", "));
        }
        for view in views.iter().take(self.max_labels) {
            let lacks = view.data.is_none() || without.contains(&label_name(view.label));
            let strip: &[String] = if lacks { &[] } else { &common };
            self.write_label(out, kind, view, strip, false);
        }
        if views.len() > self.max_labels {
            let rest = &views[self.max_labels..];
            let elements: u64 = rest.iter().filter_map(|v| v.data).map(|d| d.count).sum();
            let _ = writeln!(
                out,
                "... {} more {title} ({} {noun})",
                rest.len(),
                thousands(elements)
            );
        }
    }

    fn write_details(&self, out: &mut String) {
        for wanted in &self.labels {
            let mut found = false;
            for kind in [ElementKind::Vertex, ElementKind::Edge] {
                let views = self.views(kind);
                let Some(view) = views
                    .iter()
                    .find(|view| view.label == Some(wanted.as_str()))
                else {
                    continue;
                };
                found = true;
                let _ = write!(out, "\n{} label ", kind_word(kind));
                self.write_label(out, kind, view, &[], true);
                if kind == ElementKind::Vertex {
                    self.write_edges_of(out, view.label);
                }
            }
            if !found {
                let _ = writeln!(
                    out,
                    "\nno vertex or edge label {} (labels are case-sensitive)",
                    name(wanted)
                );
            }
        }
    }
}

fn kind_word(kind: ElementKind) -> &'static str {
    match kind {
        ElementKind::Vertex => "vertex",
        ElementKind::Edge => "edge",
    }
}

impl fmt::Display for SchemaSummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut out = String::new();
        self.write_header(&mut out);
        if self.labels.is_empty() {
            self.write_kind(&mut out, ElementKind::Vertex);
            self.write_kind(&mut out, ElementKind::Edge);
        } else {
            self.write_details(&mut out);
        }
        f.write_str(out.trim_end())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_names_and_cuts() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1000), "1,000");
        assert_eq!(thousands(2_298_943), "2,298,943");
        assert_eq!(name("fqn"), "fqn");
        assert_eq!(name("first name"), "`first name`");
        assert_eq!(name("a,b"), "`a,b`");
        assert_eq!(name(""), "``");
        assert_eq!(cut("one  two\nthree", 100), "one two three");
        assert_eq!(cut("abcdef", 4), "abc…");
    }

    #[test]
    fn types_render_in_the_notation() {
        let mut types = Types::default();
        types.add(&ElementProperty::from("x"));
        assert_eq!(types.text(), "str");
        types.add(&ElementProperty::from(3i64));
        assert_eq!(types.text(), "int|str");
        let mut list = Types::default();
        list.add(&ElementProperty::Array(vec![ElementProperty::from("a")]));
        assert_eq!(list.text(), "[str]");
        let mut empty = Types::default();
        empty.add(&ElementProperty::Array(Vec::new()));
        assert_eq!(empty.text(), "[]");
    }

    #[test]
    fn sampling_thins_out_per_doubling() {
        let mut acc = LabelAcc::new(Some("x"));
        let taken = (0..100_000).filter(|_| acc.pass(1000)).count();
        // 1000 first, then about 500 per doubling up to 100k.
        assert!((1000..6000).contains(&taken), "{taken}");
    }
}
