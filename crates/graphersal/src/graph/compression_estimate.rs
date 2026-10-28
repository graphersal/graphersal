//! Estimating a compression rule before defining it (book page "Compressed Properties",
//! section "Estimating the gain"): [`TraversalGraph::estimate_compression`] measures every
//! candidate rule ([`CompressionCandidate`]: codec and its options) over the current values at
//! one `(element, label, path)` and reports what each would hold ([`CompressionEstimate`]).
//!
//! The estimate is READ-ONLY: no unit, no commit, no catalog change, no change to any stored
//! value; it reads under the caller's read lock. It reuses the rule's own code:
//!
//! - the trial dictionary comes from [`dictionary_from`] over the elements in id order
//!   ([`TraversalGraph::compression_elements`]), the sampling of a defined rule's dictionary;
//! - a value qualifies exactly when the rule's write path would compress it
//!   ([`CompressedText::compress`]: `>= min_bytes`, at most 90 %);
//! - bytes are counted like [`TraversalGraph::compression_stats`] (plain length, compressed
//!   length, dictionary length);
//! - a `dedup` candidate shares a value exactly when the rule would (`dedup_qualifies`) and
//!   counts each distinct text once with its overhead, like the stats of a dedup rule. Dedup
//!   needs no trial encoding, so its numbers are always exact (never sampled).
//!
//! So below the caps, the numbers of a candidate EQUAL what `compression_stats` reports after
//! defining that rule on the same data. Above the caps (`max_values` strings or `max_bytes`
//! plain bytes) the estimate compresses a deterministic sample (every k-th string in id order)
//! and extrapolates per candidate from the exact counts and sizes of the strings that reach its
//! threshold; the result says `sampled`. The value counts and the size histogram are always
//! exact (they need no compression).

use std::borrow::Cow;
use std::sync::Arc;

use compact_str::CompactString;

use crate::error::DefinitionError;
use crate::graph::TraversalGraph;
use crate::graph::compression::{
    Codec, CompressionDefinition, CompressionStats, dedup_qualifies, dictionary_from, path_problem,
    value_at,
};
use crate::graph::definition::DefinitionKind;
use crate::graph::schema::ElementKind;
use crate::traversal::cancel::CancelToken;
use crate::value::compressed::{CompressedText, CompressionDictionary};
use crate::value::deduped::DEDUP_ENTRY_OVERHEAD;
use crate::value::json_path::JsonPathSegment;
use crate::value::{ElementProperty, JsonPath};

/// The `min_bytes` thresholds of the default candidate set (each with and without a
/// dictionary).
pub const DEFAULT_ESTIMATE_THRESHOLDS: [u32; 5] = [64, 128, 256, 512, 1024];
/// The `min_bytes` thresholds a `dedup` estimate measures when none is given
/// ([`CompressionCandidate::matching`] with codec `dedup`).
pub const DEFAULT_DEDUP_ESTIMATE_THRESHOLDS: [u32; 4] = [25, 64, 128, 256];
/// The default cap on the strings an estimate compresses; above it a sample is measured.
pub const DEFAULT_ESTIMATE_MAX_VALUES: usize = 100_000;
/// The default cap on the plain bytes an estimate compresses (16 MiB); above it a sample is
/// measured.
pub const DEFAULT_ESTIMATE_MAX_BYTES: usize = 16 * 1024 * 1024;
/// The lower bounds of the size histogram's buckets (bytes); the last bucket is open.
const HISTOGRAM_BOUNDS: [u64; 8] = [0, 64, 128, 256, 512, 1024, 4096, 16384];
/// How many items a loop handles between two cancellation checks.
const CANCEL_EVERY: usize = 256;

/// One rule an estimate measures: a codec and its options (`lz4` with or without a dictionary,
/// or `dedup`, at a threshold). A codec a later version adds (and its options) becomes another
/// candidate of the same list; the fields are public to read, built with the constructors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct CompressionCandidate {
    /// The codec.
    pub codec: Codec,
    /// Strings shorter than this many UTF-8 bytes stay plain.
    pub min_bytes: u32,
    /// Whether the rule builds a dictionary from its values.
    pub dictionary: bool,
}

impl CompressionCandidate {
    /// A candidate of `codec` with these options.
    pub fn new(codec: Codec, min_bytes: u32, dictionary: bool) -> Self {
        CompressionCandidate {
            codec,
            min_bytes,
            dictionary,
        }
    }

    /// An `lz4` candidate.
    pub fn lz4(min_bytes: u32, dictionary: bool) -> Self {
        Self::new(Codec::Lz4, min_bytes, dictionary)
    }

    /// A `dedup` candidate (no dictionary).
    pub fn dedup(min_bytes: u32) -> Self {
        Self::new(Codec::Dedup, min_bytes, false)
    }

    /// The candidate a stored rule is (its codec, threshold and dictionary flag).
    pub fn of_rule(rule: &CompressionDefinition) -> Self {
        Self::new(rule.codec(), rule.min_bytes(), rule.dictionary())
    }

    /// The default candidate set: `lz4` at every [`DEFAULT_ESTIMATE_THRESHOLDS`] threshold,
    /// first without, then with a dictionary. `dedup` is measured when asked for
    /// ([`Self::matching`] / [`Self::matching_codecs`]) and when it is the current rule's codec.
    pub fn defaults() -> Vec<CompressionCandidate> {
        let mut out = Vec::new();
        for min_bytes in DEFAULT_ESTIMATE_THRESHOLDS {
            out.push(Self::lz4(min_bytes, false));
            out.push(Self::lz4(min_bytes, true));
        }
        out
    }

    /// The candidates a front end's optional fields ask for (`g.estimate_compression(#{..})`,
    /// the session's spec): each given field narrows the default set, so `min_bytes` alone
    /// measures that threshold with and without a dictionary. Nothing given: empty (the
    /// request then takes the default set plus the current rule's parameters). Codec `dedup`
    /// without `min_bytes` measures [`DEFAULT_DEDUP_ESTIMATE_THRESHOLDS`], without a dictionary
    /// (`dictionary: true` with `dedup` is kept, so that validation refuses it).
    pub fn matching(
        codec: Option<Codec>,
        min_bytes: Option<u32>,
        dictionary: Option<bool>,
    ) -> Vec<CompressionCandidate> {
        if codec.is_none() && min_bytes.is_none() && dictionary.is_none() {
            return Vec::new();
        }
        let codec = codec.unwrap_or_default();
        let thresholds: Vec<u32> = match (min_bytes, codec) {
            (Some(min_bytes), _) => vec![min_bytes],
            (None, Codec::Lz4) => DEFAULT_ESTIMATE_THRESHOLDS.to_vec(),
            (None, Codec::Dedup) => DEFAULT_DEDUP_ESTIMATE_THRESHOLDS.to_vec(),
        };
        let dictionaries: Vec<bool> = match (dictionary, codec.supports_dictionary()) {
            (Some(flag), _) => vec![flag],
            (None, true) => vec![false, true],
            (None, false) => vec![false],
        };
        let mut out = Vec::new();
        for min_bytes in thresholds {
            for &dictionary in &dictionaries {
                out.push(Self::new(codec, min_bytes, dictionary));
            }
        }
        out
    }

    /// [`Self::matching`] for several codecs, concatenated in the given order (empty `codecs`:
    /// [`Self::matching`] without a codec). `[Codec::Lz4, Codec::Dedup]` compares both.
    pub fn matching_codecs(
        codecs: &[Codec],
        min_bytes: Option<u32>,
        dictionary: Option<bool>,
    ) -> Vec<CompressionCandidate> {
        if codecs.is_empty() {
            return Self::matching(None, min_bytes, dictionary);
        }
        let mut out: Vec<CompressionCandidate> = Vec::new();
        for &codec in codecs {
            for candidate in Self::matching(Some(codec), min_bytes, dictionary) {
                if !out.contains(&candidate) {
                    out.push(candidate);
                }
            }
        }
        out
    }

    /// `rule` with this candidate's codec and options (what "Use" in the playground does).
    pub fn apply_to(self, rule: CompressionDefinition) -> CompressionDefinition {
        rule.with_codec(self.codec)
            .with_min_bytes(self.min_bytes)
            .with_dictionary(self.dictionary)
    }
}

/// What to estimate: the `(element, label, path)` of a would-be rule, the candidates (empty:
/// [`CompressionCandidate::defaults`] plus the current rule's own parameters), the caps and a
/// cancellation token. Built with [`Self::new`] and the `with_*` methods.
///
/// ```
/// use graphersal::catalog::{CompressionCandidate, CompressionEstimateRequest};
/// use graphersal::storage::ElementKind;
///
/// let request = CompressionEstimateRequest::new(ElementKind::Vertex, "customer", "cv")
///     .unwrap()
///     .with_candidates([CompressionCandidate::lz4(512, true)]);
/// assert_eq!(request.path().to_string(), "$.cv");
/// ```
#[derive(Debug, Clone)]
pub struct CompressionEstimateRequest {
    element: ElementKind,
    label: String,
    path: JsonPath,
    candidates: Vec<CompressionCandidate>,
    max_values: usize,
    max_bytes: usize,
    cancel: Option<CancelToken>,
}

impl CompressionEstimateRequest {
    /// An estimate of `element`s labelled `label` at `path` (a jpath of names, like a rule's:
    /// `"cv"`, `"details.career.cv"`). A path that does not parse, has an index segment or is
    /// empty is an error.
    pub fn new(
        element: ElementKind,
        label: impl Into<String>,
        path: &str,
    ) -> Result<Self, DefinitionError> {
        let path: JsonPath =
            path.parse()
                .map_err(|err: crate::TraverserError| DefinitionError::InvalidField {
                    name: String::new(),
                    field: "path".to_string(),
                    reason: err.to_string(),
                })?;
        let request = Self::with_path(element, label, path);
        if let Some(reason) = path_problem(&request.path) {
            return Err(DefinitionError::InvalidField {
                name: String::new(),
                field: "path".to_string(),
                reason,
            });
        }
        Ok(request)
    }

    /// An estimate with an already parsed path (checked by [`Self::validate`]).
    pub fn with_path(element: ElementKind, label: impl Into<String>, path: JsonPath) -> Self {
        CompressionEstimateRequest {
            element,
            label: label.into(),
            path,
            candidates: Vec::new(),
            max_values: DEFAULT_ESTIMATE_MAX_VALUES,
            max_bytes: DEFAULT_ESTIMATE_MAX_BYTES,
            cancel: None,
        }
    }

    /// The same request measuring only these candidates (in this order, duplicates dropped);
    /// none: the default set.
    pub fn with_candidates(
        mut self,
        candidates: impl IntoIterator<Item = CompressionCandidate>,
    ) -> Self {
        self.candidates.clear();
        for candidate in candidates {
            if !self.candidates.contains(&candidate) {
                self.candidates.push(candidate);
            }
        }
        self
    }

    /// The same request with these caps: at most `max_values` strings and `max_bytes` plain
    /// bytes are compressed; above either a deterministic sample is (0 counts as 1).
    pub fn with_limits(mut self, max_values: usize, max_bytes: usize) -> Self {
        self.max_values = max_values.max(1);
        self.max_bytes = max_bytes.max(1);
        self
    }

    /// The same request stopping when `token` is cancelled (without one, the token of the
    /// running thread's [`CancelToken::scope`] applies).
    pub fn with_cancel(mut self, token: CancelToken) -> Self {
        self.cancel = Some(token);
        self
    }

    /// The element kind.
    pub fn element(&self) -> ElementKind {
        self.element
    }

    /// The label.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// The path.
    pub fn path(&self) -> &JsonPath {
        &self.path
    }

    /// The candidates asked for (empty: the default set).
    pub fn candidates(&self) -> &[CompressionCandidate] {
        &self.candidates
    }

    /// The caps: (most strings, most plain bytes) compressed before sampling.
    pub fn limits(&self) -> (usize, usize) {
        (self.max_values, self.max_bytes)
    }

    /// The cancellation token, if one was given.
    pub fn cancel_token(&self) -> Option<&CancelToken> {
        self.cancel.as_ref()
    }

    /// Checks the request like [`Definition::validate`](crate::graph::Definition::validate)
    /// checks a rule: the label, the path and every candidate's threshold (an error names the
    /// field; the rule name in it is empty).
    pub fn validate(&self) -> Result<(), DefinitionError> {
        let base = CompressionDefinition::with_path(self.element, &*self.label, self.path.clone());
        if self.candidates.is_empty() {
            return base.validate("");
        }
        for candidate in &self.candidates {
            candidate.apply_to(base.clone()).validate("")?;
        }
        Ok(())
    }

    /// The path as names (validated: no index segment).
    fn names(&self) -> Vec<CompactString> {
        self.path
            .segments()
            .iter()
            .filter_map(|segment| match segment {
                JsonPathSegment::Key(name) => Some(name.clone()),
                JsonPathSegment::Index(_) => None,
            })
            .collect()
    }
}

/// One bucket of the size histogram of an estimate: the strings at the path whose UTF-8 length
/// is in `[min_bytes, max_bytes)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct SizeBucket {
    /// The smallest length in the bucket.
    pub min_bytes: u64,
    /// The first length above the bucket; `None` for the last, open bucket.
    pub max_bytes: Option<u64>,
    /// How many strings fall in it.
    pub values: u64,
    /// Their summed length.
    pub bytes: u64,
}

/// What one candidate would hold ([`CompressionEstimate::candidates`]), counted like
/// [`CompressionStats`].
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct CandidateEstimate {
    /// The candidate.
    pub candidate: CompressionCandidate,
    /// The values the rule would store compressed (`lz4`: `>= min_bytes` and at most 90 %) or
    /// share (`dedup`: `>= min_bytes` and longer than 24 bytes).
    pub compressed_values: u64,
    /// Their plain size in bytes.
    pub plain_bytes: u64,
    /// Their stored size in bytes: compressed without the dictionary (`lz4`), each distinct
    /// value once with its 32 bytes of overhead (`dedup`, like [`CompressionStats`]).
    pub stored_bytes: u64,
    /// The distinct values among `compressed_values` (`dedup`; 0 for `lz4`).
    pub distinct_values: u64,
    /// The size of the dictionary the rule would build (0: none).
    pub dictionary_bytes: u64,
    /// `plain_bytes - stored_bytes - dictionary_bytes`: what the rule would save (negative: it
    /// would cost).
    pub saved_bytes: i64,
    /// Whether this candidate has the parameters of the rule that covers the path today.
    pub current: bool,
    /// Compression speed of the measured strings in plain bytes per second (writes and the
    /// rule's pass pay it); `None` when nothing was compressed or the clock did not move.
    pub encode_bytes_per_sec: Option<f64>,
    /// Decompression speed in plain bytes per second (every read of a compressed value pays
    /// it); `None` like `encode_bytes_per_sec`.
    pub decode_bytes_per_sec: Option<f64>,
}

impl CandidateEstimate {
    /// `stored_bytes / plain_bytes` (`None` without compressed values).
    pub fn ratio(&self) -> Option<f64> {
        (self.plain_bytes > 0).then(|| self.stored_bytes as f64 / self.plain_bytes as f64)
    }
}

/// The rule that covers the estimated path today (the baseline): its name, parameters and what
/// it holds now ([`TraversalGraph::compression_stats`]).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct CurrentCompression {
    /// The rule's name.
    pub name: String,
    /// The rule.
    pub rule: CompressionDefinition,
    /// What it holds now.
    pub stats: CompressionStats,
}

/// Something an estimate found worth saying about the path (a typo silently makes a useless
/// rule otherwise).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum EstimateWarning {
    /// No element of the kind carries the label.
    UnknownLabel,
    /// Elements carry the label, but none has a value at the path.
    NoValues,
    /// Some values at the path are not strings: a rule never compresses them.
    NotStrings {
        /// How many.
        values: u64,
    },
    /// Strings were found, but no candidate would compress any of them.
    NothingQualifies,
    /// Values would be compressed, but no candidate saves memory (the dictionary costs more).
    NoSaving,
    /// A `dedup` candidate would share values, but none saves memory: the values are mostly
    /// distinct (dedup pays for long values repeated across many elements).
    HighCardinality {
        /// The distinct values among them (of the dedup candidate sharing the most).
        distinct_values: u64,
        /// The values it would share.
        values: u64,
    },
}

impl EstimateWarning {
    /// A stable short code: `unknown_label`, `no_values`, `not_strings`, `nothing_qualifies`,
    /// `no_saving`, `high_cardinality`.
    pub fn code(&self) -> &'static str {
        match self {
            EstimateWarning::UnknownLabel => "unknown_label",
            EstimateWarning::NoValues => "no_values",
            EstimateWarning::NotStrings { .. } => "not_strings",
            EstimateWarning::NothingQualifies => "nothing_qualifies",
            EstimateWarning::NoSaving => "no_saving",
            EstimateWarning::HighCardinality { .. } => "high_cardinality",
        }
    }

    /// The warning as a sentence for `estimate` of `(element, label, path)`.
    pub fn message(&self, estimate: &CompressionEstimate) -> String {
        let element = estimate.element.as_str();
        match self {
            EstimateWarning::UnknownLabel => format!(
                "no {element} has the label '{}' (a typo? a rule for it would compress nothing \
                 until such elements exist)",
                estimate.label
            ),
            EstimateWarning::NoValues => format!(
                "no {element} labelled '{}' has a value at {} (check the path)",
                estimate.label, estimate.path
            ),
            EstimateWarning::NotStrings { values } => format!(
                "{values} value(s) at {} are not strings; a rule compresses strings only",
                estimate.path
            ),
            EstimateWarning::NothingQualifies => {
                "no candidate would compress any value: the strings are shorter than every \
                 threshold or do not compress to 90 % (lower min_bytes, or no rule)"
                    .to_string()
            }
            EstimateWarning::NoSaving => {
                "no candidate saves memory: the dictionary costs more than it gains (try \
                 dictionary false)"
                    .to_string()
            }
            EstimateWarning::HighCardinality {
                distinct_values,
                values,
            } => format!(
                "{distinct_values} of the {values} long strings at {} are distinct: dedup \
                 stores each distinct value once and saves nothing here (it pays for long values \
                 repeated across many elements; lz4 may suit long text better)",
                estimate.path
            ),
        }
    }
}

/// The result of [`TraversalGraph::estimate_compression`]: the values at the path, a size
/// histogram, one [`CandidateEstimate`] per candidate, the best one, the current rule and
/// warnings. The graph is unchanged.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct CompressionEstimate {
    /// The element kind estimated.
    pub element: ElementKind,
    /// The label estimated.
    pub label: String,
    /// The canonical text of the path (`$.details.career.cv`).
    pub path: String,
    /// The elements of the kind carrying the label.
    pub elements: u64,
    /// How many of them have a value at the path.
    pub values_found: u64,
    /// How many of those values are strings.
    pub string_values: u64,
    /// Their summed length in bytes.
    pub string_bytes: u64,
    /// Whether the candidates were measured on a sample (above the caps) and extrapolated.
    pub sampled: bool,
    /// How many strings were compressed per dictionary setting (all of them unless `sampled`).
    pub measured_values: u64,
    /// The strings by length (exact, never sampled).
    pub histogram: Vec<SizeBucket>,
    /// One entry per candidate, in the request's order.
    pub candidates: Vec<CandidateEstimate>,
    /// The index (into `candidates`) of the candidate that saves the most; `None` when none
    /// saves anything. A tie goes to the earlier candidate.
    pub best: Option<usize>,
    /// The rule that covers the path today, if any.
    pub current: Option<CurrentCompression>,
    /// What is worth saying about the path.
    pub warnings: Vec<EstimateWarning>,
    /// Whether the estimate stopped because its cancellation token was cancelled (the numbers
    /// are then incomplete; the authorized entry points turn it into
    /// [`TraverserError::Cancelled`](crate::TraverserError::Cancelled)).
    pub cancelled: bool,
    /// How long the estimate took, in microseconds.
    pub elapsed_micros: u64,
}

/// A trial encoding of the measured strings with one dictionary setting.
struct Trial {
    /// Per measured string: its compressed length, `None` when it stays plain.
    stored: Vec<Option<usize>>,
    encode: Option<f64>,
    decode: Option<f64>,
}

/// Plain bytes per second, `None` when no time was measured.
fn speed(bytes: u64, elapsed: web_time::Duration) -> Option<f64> {
    let secs = elapsed.as_secs_f64();
    (bytes > 0 && secs > 0.0).then(|| bytes as f64 / secs)
}

fn cancelled(token: &Option<CancelToken>, at: usize) -> bool {
    at.is_multiple_of(CANCEL_EVERY) && token.as_ref().is_some_and(CancelToken::is_cancelled)
}

/// Compresses every text of `texts` with `dictionary` (the rule's write path), then
/// decompresses what was compressed, timing both. `None` when cancelled.
fn trial(
    texts: &[Cow<'_, str>],
    floor: usize,
    dictionary: Option<&Arc<CompressionDictionary>>,
    token: &Option<CancelToken>,
) -> Option<Trial> {
    let started = web_time::Instant::now();
    let mut encoded_bytes = 0u64;
    let mut made: Vec<Option<CompressedText>> = Vec::with_capacity(texts.len());
    for (at, text) in texts.iter().enumerate() {
        if cancelled(token, at) {
            return None;
        }
        if text.len() < floor {
            made.push(None);
            continue;
        }
        encoded_bytes += text.len() as u64;
        made.push(CompressedText::compress(text, dictionary));
    }
    let encode = speed(encoded_bytes, started.elapsed());
    let started = web_time::Instant::now();
    let mut buffer = Vec::new();
    let mut decoded_bytes = 0u64;
    for (at, compressed) in made.iter().enumerate() {
        if cancelled(token, at) {
            return None;
        }
        if let Some(compressed) = compressed
            && compressed.decompress_into(&mut buffer)
        {
            decoded_bytes += compressed.plain_len() as u64;
        }
    }
    let decode = speed(decoded_bytes, started.elapsed());
    Some(Trial {
        stored: made
            .iter()
            .map(|c| c.as_ref().map(CompressedText::stored_len))
            .collect(),
        encode,
        decode,
    })
}

/// `part * total / of`, rounded (`part` when `of` is 0).
fn scale(part: u64, total: u64, of: u64) -> u64 {
    if of == 0 || total == of {
        return part;
    }
    ((part as f64) * (total as f64) / (of as f64)).round() as u64
}

impl TraversalGraph {
    /// Estimates compression rules for `(element, label, path)` without changing anything
    /// (module docs of `compression_estimate.rs`; book page "Compressed Properties"): measures
    /// every candidate of `request` over the current values and reports what each rule would
    /// hold. Reads only; never fails (an invalid request is the caller's to
    /// [validate](CompressionEstimateRequest::validate); a cancelled one says `cancelled`).
    ///
    /// ```
    /// use graphersal::prelude::*;
    /// use graphersal::catalog::CompressionEstimateRequest;
    /// use graphersal::storage::ElementKind;
    ///
    /// let mut graph = TraversalGraph::new();
    /// let cv = "Senior engineer, led the platform team. ".repeat(20);
    /// graph.traversal_mut().add_v("customer").property("cv", cv.as_str()).to_list().unwrap();
    /// let request = CompressionEstimateRequest::new(ElementKind::Vertex, "customer", "cv").unwrap();
    /// let estimate = graph.estimate_compression(&request);
    /// assert_eq!(estimate.string_values, 1);
    /// let best = &estimate.candidates[estimate.best.unwrap()];
    /// assert!(best.saved_bytes > 0);
    /// ```
    pub fn estimate_compression(
        &self,
        request: &CompressionEstimateRequest,
    ) -> CompressionEstimate {
        let started = web_time::Instant::now();
        let token = request.cancel.clone().or_else(CancelToken::current);
        let names = request.names();
        let mut estimate = CompressionEstimate {
            element: request.element,
            label: request.label.clone(),
            path: request.path.to_string(),
            elements: 0,
            values_found: 0,
            string_values: 0,
            string_bytes: 0,
            sampled: false,
            measured_values: 0,
            histogram: HISTOGRAM_BOUNDS
                .iter()
                .enumerate()
                .map(|(at, &min_bytes)| SizeBucket {
                    min_bytes,
                    max_bytes: HISTOGRAM_BOUNDS.get(at + 1).copied(),
                    values: 0,
                    bytes: 0,
                })
                .collect(),
            candidates: Vec::new(),
            best: None,
            current: None,
            warnings: Vec::new(),
            cancelled: false,
            elapsed_micros: 0,
        };
        let finish = |mut estimate: CompressionEstimate, cancelled: bool| {
            estimate.cancelled = cancelled;
            estimate.elapsed_micros =
                u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
            estimate
        };

        // The baseline: the rule that covers the path today.
        estimate.current = self
            .definitions
            .iter()
            .filter(|d| d.kind() == DefinitionKind::COMPRESSION)
            .find_map(|definition| {
                let rule = definition
                    .as_compression()
                    .filter(|rule| rule.is_served())?;
                (rule.element() == request.element
                    && rule.label() == request.label
                    && rule.path() == &request.path)
                    .then(|| CurrentCompression {
                        name: definition.name().to_string(),
                        rule: rule.clone(),
                        stats: self
                            .compression_stats(definition.name())
                            .unwrap_or_default(),
                    })
            });
        let mut candidates = request.candidates.clone();
        if candidates.is_empty() {
            candidates = CompressionCandidate::defaults();
            if let Some(current) = &estimate.current {
                let own = CompressionCandidate::of_rule(&current.rule);
                if !candidates.contains(&own) {
                    candidates.push(own);
                }
            }
        }

        // Pass 1 (exact): the values at the path, in id order.
        let elements = self.compression_elements(request.element, &request.label);
        // An edge without properties is not among `elements`, but it carries the label.
        estimate.elements = match request.element {
            ElementKind::Vertex => elements.len() as u64,
            ElementKind::Edge => self
                .index
                .get_edges_by_label(&request.label)
                .map_or(0, Iterator::count) as u64,
        };
        let mut strings: Vec<&ElementProperty> = Vec::new();
        let mut lengths: Vec<usize> = Vec::new();
        for (at, (_, properties)) in elements.iter().enumerate() {
            if cancelled(&token, at) {
                return finish(estimate, true);
            }
            let Some(value) = value_at(properties, &names) else {
                continue;
            };
            estimate.values_found += 1;
            let len = match value {
                ElementProperty::String(text) => text.len(),
                ElementProperty::Compressed(text) => text.plain_len(),
                ElementProperty::Deduped(text) => text.len(),
                _ => continue,
            };
            estimate.string_values += 1;
            estimate.string_bytes += len as u64;
            let bucket = HISTOGRAM_BOUNDS
                .iter()
                .rposition(|&bound| len as u64 >= bound)
                .unwrap_or(0);
            if let Some(bucket) = estimate.histogram.get_mut(bucket) {
                bucket.values += 1;
                bucket.bytes += len as u64;
            }
            strings.push(value);
            lengths.push(len);
        }

        // The sample: every string below the caps, else every `step`-th in id order (the lz4
        // trials read it; dedup reads every string, below).
        let floor = candidates
            .iter()
            .filter(|c| c.codec == Codec::Lz4)
            .map(|c| c.min_bytes as usize)
            .min()
            .unwrap_or(usize::MAX);
        let (max_values, max_bytes) = request.limits();
        let step = (strings.len().div_ceil(max_values))
            .max((estimate.string_bytes as usize).div_ceil(max_bytes))
            .max(1);
        estimate.sampled = step > 1;
        let mut texts: Vec<Cow<'_, str>> = Vec::new();
        let mut sample_lengths: Vec<usize> = Vec::new();
        for (at, (value, &len)) in strings.iter().zip(&lengths).enumerate() {
            if cancelled(&token, at) {
                return finish(estimate, true);
            }
            if at % step != 0 {
                continue;
            }
            estimate.measured_values += 1;
            if len < floor {
                continue;
            }
            if let Some(text) = value.text() {
                texts.push(text);
                sample_lengths.push(len);
            }
        }

        // One trial per dictionary setting the candidates use (the rule's own dictionary).
        let lz4_floor = |dictionary: bool| {
            candidates
                .iter()
                .filter(|c| c.codec == Codec::Lz4 && c.dictionary == dictionary)
                .map(|c| c.min_bytes as usize)
                .min()
        };
        let dictionary = if lz4_floor(true).is_some() {
            dictionary_from(&elements, &names)
        } else {
            None
        };
        let mut plain_trial = None;
        let mut dictionary_trial = None;
        if let Some(floor) = lz4_floor(false) {
            match trial(&texts, floor, None, &token) {
                Some(found) => plain_trial = Some(found),
                None => return finish(estimate, true),
            }
        }
        if let Some(floor) = lz4_floor(true) {
            match trial(&texts, floor, dictionary.as_ref(), &token) {
                Some(found) => dictionary_trial = Some(found),
                None => return finish(estimate, true),
            }
        }

        // Dedup (exact, every string): the distinct texts that reach the lowest dedup
        // threshold, with their length and how often each occurs.
        let dedup_floor = candidates
            .iter()
            .filter(|c| c.codec == Codec::Dedup)
            .map(|c| c.min_bytes as usize)
            .min();
        let mut distinct: Vec<(usize, u64)> = Vec::new();
        if let Some(dedup_floor) = dedup_floor {
            let mut counts: std::collections::HashMap<Cow<'_, str>, u64> =
                std::collections::HashMap::new();
            for (at, (value, &len)) in strings.iter().zip(&lengths).enumerate() {
                if cancelled(&token, at) {
                    return finish(estimate, true);
                }
                if !dedup_qualifies(len, dedup_floor) {
                    continue;
                }
                if let Some(text) = value.text() {
                    *counts.entry(text).or_insert(0) += 1;
                }
            }
            distinct = counts
                .iter()
                .map(|(text, &count)| (text.len(), count))
                .collect();
        }

        let current = estimate
            .current
            .as_ref()
            .map(|c| CompressionCandidate::of_rule(&c.rule));
        for candidate in &candidates {
            let (trial, dictionary_bytes) = match candidate.codec {
                Codec::Lz4 if candidate.dictionary => (
                    dictionary_trial.as_ref(),
                    dictionary.as_ref().map_or(0, |d| d.len() as u64),
                ),
                Codec::Lz4 => (plain_trial.as_ref(), 0),
                Codec::Dedup => {
                    estimate
                        .candidates
                        .push(dedup_estimate(*candidate, &distinct, current));
                    continue;
                }
            };
            let threshold = candidate.min_bytes as usize;
            let (mut values, mut plain, mut stored) = (0u64, 0u64, 0u64);
            let (mut sample_count, mut sample_bytes) = (0u64, 0u64);
            if let Some(trial) = trial {
                for (&len, stored_len) in sample_lengths.iter().zip(&trial.stored) {
                    if len < threshold {
                        continue;
                    }
                    sample_count += 1;
                    sample_bytes += len as u64;
                    if let Some(stored_len) = stored_len {
                        values += 1;
                        plain += len as u64;
                        stored += *stored_len as u64;
                    }
                }
            }
            if estimate.sampled {
                // Extrapolate from the exact count and size of the strings at the threshold.
                let (total_count, total_bytes) = lengths
                    .iter()
                    .filter(|&&len| len >= threshold)
                    .fold((0u64, 0u64), |(n, b), &len| (n + 1, b + len as u64));
                values = scale(values, total_count, sample_count);
                plain = scale(plain, total_bytes, sample_bytes);
                stored = scale(stored, total_bytes, sample_bytes);
            }
            let saved = i64::try_from(plain).unwrap_or(i64::MAX)
                - i64::try_from(stored).unwrap_or(i64::MAX)
                - i64::try_from(dictionary_bytes).unwrap_or(i64::MAX);
            estimate.candidates.push(CandidateEstimate {
                candidate: *candidate,
                compressed_values: values,
                plain_bytes: plain,
                stored_bytes: stored,
                dictionary_bytes,
                distinct_values: 0,
                saved_bytes: saved,
                current: current == Some(*candidate),
                encode_bytes_per_sec: trial.and_then(|t| t.encode),
                decode_bytes_per_sec: trial.and_then(|t| t.decode),
            });
        }
        let mut best: Option<usize> = None;
        for (at, candidate) in estimate.candidates.iter().enumerate() {
            if candidate.saved_bytes > 0
                && best.is_none_or(|b| candidate.saved_bytes > estimate.candidates[b].saved_bytes)
            {
                best = Some(at);
            }
        }
        estimate.best = best;

        if estimate.elements == 0 {
            estimate.warnings.push(EstimateWarning::UnknownLabel);
        } else if estimate.values_found == 0 {
            estimate.warnings.push(EstimateWarning::NoValues);
        }
        let not_strings = estimate.values_found - estimate.string_values;
        if not_strings > 0 {
            estimate.warnings.push(EstimateWarning::NotStrings {
                values: not_strings,
            });
        }
        let compressed_any = estimate.candidates.iter().any(|c| c.compressed_values > 0);
        if estimate.string_values > 0 && !compressed_any {
            estimate.warnings.push(EstimateWarning::NothingQualifies);
        } else if compressed_any && best.is_none() {
            estimate.warnings.push(EstimateWarning::NoSaving);
        }
        let dedup: Vec<&CandidateEstimate> = estimate
            .candidates
            .iter()
            .filter(|c| c.candidate.codec == Codec::Dedup && c.compressed_values > 0)
            .collect();
        if !dedup.is_empty()
            && dedup.iter().all(|c| c.saved_bytes <= 0)
            && let Some(widest) = dedup.iter().max_by_key(|c| c.compressed_values)
        {
            let warning = EstimateWarning::HighCardinality {
                distinct_values: widest.distinct_values,
                values: widest.compressed_values,
            };
            estimate.warnings.push(warning);
        }
        finish(estimate, false)
    }
}

/// What a `dedup` candidate would hold, from the distinct texts (length, occurrences) that
/// reach the lowest dedup threshold: the occurrences of those at its threshold are shared,
/// each distinct one stored once with its overhead (like `compression_stats`).
fn dedup_estimate(
    candidate: CompressionCandidate,
    distinct: &[(usize, u64)],
    current: Option<CompressionCandidate>,
) -> CandidateEstimate {
    let threshold = candidate.min_bytes as usize;
    let (mut values, mut plain, mut stored, mut kinds) = (0u64, 0u64, 0u64, 0u64);
    for &(len, count) in distinct {
        if !dedup_qualifies(len, threshold) {
            continue;
        }
        values += count;
        plain += count * len as u64;
        stored += (len + DEDUP_ENTRY_OVERHEAD) as u64;
        kinds += 1;
    }
    let saved =
        i64::try_from(plain).unwrap_or(i64::MAX) - i64::try_from(stored).unwrap_or(i64::MAX);
    CandidateEstimate {
        candidate,
        compressed_values: values,
        plain_bytes: plain,
        stored_bytes: stored,
        dictionary_bytes: 0,
        distinct_values: kinds,
        saved_bytes: saved,
        current: current == Some(candidate),
        encode_bytes_per_sec: None,
        decode_bytes_per_sec: None,
    }
}
