//! The storage advisor (book page "Optimize"): recommendations computed from what the graph
//! holds (the memory report), how it is used (the workload statistics) and its catalog. One
//! pure function, [`compute`]; every front end shows its result ([`Advice::to_property`]) and
//! applies a recommendation through the ordinary catalog command it names, never a privileged
//! path. Nothing is applied automatically.
//!
//! The rules (thresholds in [`AdviceRequest`]):
//!
//! | Kind | When | Effect |
//! |---|---|---|
//! | `create_index` | a `(element, label, key)` filtered by a full scan at least `min_filter_runs` times in two days, on a label of at least `min_index_elements` elements, no index serving it (a label-less index on the key serves every label), values an index holds (a key holding arrays is advised `transform: each`, one entry per element, and only an `each` index serves it); a scan whose flow names no single label (no label, or several) is advised a LABEL-LESS index (`label: None`, id `create_index:<element>:*:<key>`; the elements of the kind count) | the scans become index lookups; costs index memory (estimated) |
//! | `drop_index` | an index no query used for its key in the window observed, after at least `min_observed_days` days of statistics; or (confidence low) a per-label index whose key a label-less index of the same element kind and transform covers too (the entries are kept twice) | frees the index's memory (measured) |
//! | `add_compression` | lz4: a top-level string property of at least `min_compression_bytes` without a rule, not indexed, read fewer than `hot_reads` times in two days, whose best lz4 candidate saves at least `min_saving_bytes` and `min_saving_percent`; dedup: a top-level property of any value type without a rule (indexed or not, hot or not) whose values repeat (at most one in `dedup_distinct_ratio` distinct) and whose dedup saves at least `min_dedup_saving_bytes` and more than lz4 would | frees memory (measured on the data by `estimate_compression`); lz4 reads decompress, dedup writes look the value up |
//! | `drop_compression` | a compressed key read at least `hot_reads` times in two days | reads stop decompressing; memory grows back (measured) |
//!
//! Impact ([`Impact`], what applying it is worth): for a change of memory (`add_compression`,
//! `drop_index`) its share of the graph's memory ([`Advice::graph_bytes`]): `high` from 10 %,
//! `medium` from 2 %, `low` below; for `create_index` the elements its scans read in two days
//! (`high` from 100 million, `medium` from 10 million); for `drop_compression` the reads in two
//! days (`high` from 100,000, `medium` from 10,000). Confidence ([`Confidence`]) is another
//! axis: how much data the figures rest on, not how much they are worth.
//!
//! Order: by impact, the highest first; within one impact `create_index` by the elements its
//! scans read, then `add_compression` by bytes saved, `drop_index` by bytes freed,
//! `drop_compression` by reads.
//!
//! The instance's marks ([`AdviceMark`], kept in the workload statistics) leave a dismissed
//! recommendation out until its time ([`Advice::dismissed`]) and turn an applied one into an
//! outcome for 30 days ([`Advice::applied`]: the memory promised against the memory measured
//! since, and for a new index the lookups it served).

use compact_str::CompactString;

use crate::graph::ElementKind;
use crate::graph::compression::Codec;
use crate::graph::compression_estimate::{
    CompressionCandidate, CompressionEstimate, CompressionEstimateRequest,
};
use crate::graph::memory_report::{MemoryReport, PropertyMemory};
use crate::graph::property_index::{PropertyIndexInfo, PropertyIndexTransform};
use crate::graph::workload::{AccessKind, AdviceMark, SeriesKind, WorkloadLevel, WorkloadReport};
use crate::value::{ElementProperty, PropertyMap};

/// The thresholds of the advisor's rules (module docs); [`AdviceRequest::new`] has the
/// defaults.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct AdviceRequest {
    /// `create_index`: the full-scan filter runs in two days (default 10).
    pub min_filter_runs: u64,
    /// `create_index`: the elements of the label (default 1,000; below, a scan is cheap).
    pub min_index_elements: u64,
    /// `drop_index`: the days of statistics before an unused index is called unused (default 7).
    pub min_observed_days: u64,
    /// `add_compression`: the plain string bytes of the property (default 256 KiB).
    pub min_compression_bytes: u64,
    /// `add_compression`: the bytes the best codec must save (default 64 KiB).
    pub min_saving_bytes: u64,
    /// `add_compression`: the share of the string bytes it must save, in percent (default 20).
    pub min_saving_percent: u64,
    /// `add_compression` / `drop_compression`: the reads in two days that make a key hot
    /// (default 1,000).
    pub hot_reads: u64,
    /// `add_compression`: the most large-string properties measured (the largest first;
    /// default 10).
    pub max_estimates: usize,
    /// `add_compression` with `dedup`: the most properties of repeated values measured (the
    /// most values first; default 200). A dedup estimate is exact and cheap (one pass over the
    /// values, no trial compression).
    pub max_dedup_estimates: usize,
    /// `add_compression` with `dedup`: the bytes it must save (default 1 MiB).
    pub min_dedup_saving_bytes: u64,
    /// `add_compression` with `dedup`: at most one value in this many is distinct (default 8).
    pub dedup_distinct_ratio: u64,
}

impl Default for AdviceRequest {
    fn default() -> Self {
        Self {
            min_filter_runs: 10,
            min_index_elements: 1_000,
            min_observed_days: 7,
            min_compression_bytes: 256 * 1024,
            min_saving_bytes: 64 * 1024,
            min_saving_percent: 20,
            hot_reads: 1_000,
            max_estimates: 10,
            min_dedup_saving_bytes: 1024 * 1024,
            dedup_distinct_ratio: 8,
            max_dedup_estimates: 200,
        }
    }
}

impl AdviceRequest {
    /// The default thresholds.
    pub fn new() -> Self {
        Self::default()
    }

    /// The same request with every threshold at its smallest (every candidate is reported):
    /// for tests and for looking at small graphs.
    pub fn everything() -> Self {
        Self {
            min_filter_runs: 1,
            min_index_elements: 0,
            min_observed_days: 0,
            min_compression_bytes: 0,
            min_saving_bytes: 1,
            min_saving_percent: 0,
            hot_reads: u64::MAX,
            max_estimates: 50,
            min_dedup_saving_bytes: 1,
            dedup_distinct_ratio: 8,
            max_dedup_estimates: 500,
        }
    }
}

/// What a recommendation does: the catalog change applying it.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum RecommendedAction {
    /// Create a property index on `(element, label, key)`.
    #[non_exhaustive]
    CreateIndex {
        /// The element kind.
        element: ElementKind,
        /// The label; `None`: a label-less index (every element of the kind), recommended for
        /// scans whose flow names no single label.
        label: Option<String>,
        /// The top-level key.
        key: String,
        /// The index transform: `Some(each)` for a key holding arrays (its scans filter on
        /// their elements, `has(key, __.unfold().is(..))`), `None` otherwise.
        transform: Option<crate::graph::PropertyIndexTransform>,
    },
    /// Drop the property index `name`.
    #[non_exhaustive]
    DropIndex {
        /// The index's name.
        name: String,
    },
    /// Define the compression rule `name` on `(element, label, key)` with `candidate`.
    #[non_exhaustive]
    AddCompression {
        /// The suggested rule name.
        name: String,
        /// The element kind.
        element: ElementKind,
        /// The label.
        label: String,
        /// The top-level key.
        key: String,
        /// The codec and its options.
        candidate: CompressionCandidate,
    },
    /// Drop the compression rule `name`.
    #[non_exhaustive]
    DropCompression {
        /// The rule's name.
        name: String,
    },
}

impl RecommendedAction {
    /// `create_index`, `drop_index`, `add_compression`, `drop_compression`.
    pub fn kind(&self) -> &'static str {
        match self {
            RecommendedAction::CreateIndex { .. } => "create_index",
            RecommendedAction::DropIndex { .. } => "drop_index",
            RecommendedAction::AddCompression { .. } => "add_compression",
            RecommendedAction::DropCompression { .. } => "drop_compression",
        }
    }

    /// The DSL call that applies it (what an agent or a user types).
    pub fn command(&self) -> String {
        let text = |value: &str| format!("{value:?}");
        match self {
            RecommendedAction::CreateIndex {
                element,
                label,
                key,
                transform,
            } => {
                let mut fields = Vec::new();
                if *element == ElementKind::Edge {
                    fields.push("element: \"edge\"".to_string());
                }
                if let Some(label) = label {
                    fields.push(format!("label: {}", text(label)));
                }
                fields.push(format!("key: {}", text(key)));
                if let Some(transform) = transform {
                    let names: Vec<String> = transform
                        .names()
                        .into_iter()
                        .map(|name| text(name))
                        .collect();
                    fields.push(match names.as_slice() {
                        [one] => format!("transform: {one}"),
                        many => format!("transform: [{}]", many.join(", ")),
                    });
                }
                format!("g.create_index(#{{{}}})", fields.join(", "))
            }
            RecommendedAction::DropIndex { name } => format!("g.drop_index({})", text(name)),
            RecommendedAction::AddCompression {
                name,
                element,
                label,
                key,
                candidate,
            } => {
                let mut fields = vec![
                    format!("name: {}", text(name)),
                    format!("label: {}", text(label)),
                    format!("path: {}", text(key)),
                ];
                if *element == ElementKind::Edge {
                    fields.push("element: \"edge\"".to_string());
                }
                fields.push(format!("codec: {}", text(candidate.codec.name())));
                fields.push(format!("min_bytes: {}", candidate.min_bytes));
                if candidate.dictionary {
                    fields.push("dictionary: true".to_string());
                }
                format!("g.define_compression(#{{{}}})", fields.join(", "))
            }
            RecommendedAction::DropCompression { name } => {
                format!("g.drop_compression({})", text(name))
            }
        }
    }
}

/// The catalog change applying a recommendation ([`RecommendedAction::catalog_change`]).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CatalogChange {
    /// Store this new definition (an index under its default name, a rule under the
    /// suggested name).
    Set(crate::graph::definition::Definition),
    /// Remove the definition of this kind and name.
    Remove(crate::graph::definition::DefinitionKind, String),
}

impl RecommendedAction {
    /// The catalog change applying it: the same definition its [`Self::command`] stores (the
    /// index under [`PropertyIndexDefinition::default_name`](crate::graph::PropertyIndexDefinition::default_name),
    /// the rule with the measured candidate's codec and options) or the one it removes.
    pub(crate) fn catalog_change(&self) -> Result<CatalogChange, crate::error::DefinitionError> {
        use crate::graph::definition::{Definition, DefinitionKind};
        Ok(match self {
            RecommendedAction::CreateIndex {
                element,
                label,
                key,
                transform,
            } => {
                let index = match label {
                    Some(label) => crate::graph::PropertyIndexDefinition::new(
                        *element,
                        label.as_str(),
                        key.as_str(),
                    ),
                    None => {
                        crate::graph::PropertyIndexDefinition::label_less(*element, key.as_str())
                    }
                }
                .with_transform(transform.clone());
                CatalogChange::Set(Definition::property_index(index.default_name(), index))
            }
            RecommendedAction::DropIndex { name } => {
                CatalogChange::Remove(DefinitionKind::PROPERTY_INDEX, name.clone())
            }
            RecommendedAction::AddCompression {
                name,
                element,
                label,
                key,
                candidate,
            } => {
                let rule = crate::graph::CompressionDefinition::new(
                    *element,
                    label.as_str(),
                    key.as_str(),
                )?
                .with_codec(candidate.codec)
                .with_min_bytes(candidate.min_bytes)
                .with_dictionary(candidate.dictionary);
                CatalogChange::Set(Definition::compression(name.as_str(), rule))
            }
            RecommendedAction::DropCompression { name } => {
                CatalogChange::Remove(DefinitionKind::COMPRESSION, name.clone())
            }
        })
    }
}

/// How sure the advisor is of a recommendation: how much data it rests on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub enum Confidence {
    /// Little data (a few runs, a short time observed, a small sample).
    Low,
    /// Enough to act on after a look.
    Medium,
    /// Plenty.
    High,
}

impl Confidence {
    /// `low`, `medium`, `high`.
    pub fn name(self) -> &'static str {
        match self {
            Confidence::Low => "low",
            Confidence::Medium => "medium",
            Confidence::High => "high",
        }
    }
}

/// What applying a recommendation is worth (module docs): its share of the graph's memory, or
/// for a change of speed the work it saves. Not to be confused with [`Confidence`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub enum Impact {
    /// A small effect (below 2 % of the graph's memory, for one).
    Low,
    /// A noticeable effect.
    Medium,
    /// A large effect (10 % of the graph's memory or more, for one).
    High,
}

impl Impact {
    /// `low`, `medium`, `high`.
    pub fn name(self) -> &'static str {
        match self {
            Impact::Low => "low",
            Impact::Medium => "medium",
            Impact::High => "high",
        }
    }

    /// The impact of freeing (or taking) `bytes` of a graph of `graph_bytes`.
    fn of_memory(bytes: u64, graph_bytes: u64) -> Impact {
        let share = u128::from(bytes) * 100;
        let graph = u128::from(graph_bytes.max(1));
        if share >= graph * 10 {
            Impact::High
        } else if share >= graph * 2 {
            Impact::Medium
        } else {
            Impact::Low
        }
    }

    /// The impact of a count against its `medium` and `high` thresholds.
    fn of_count(count: u64, medium: u64, high: u64) -> Impact {
        if count >= high {
            Impact::High
        } else if count >= medium {
            Impact::Medium
        } else {
            Impact::Low
        }
    }
}

/// One recommendation.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Recommendation {
    /// Stable across recomputations: the action's kind and target (`create_index:vertex:person:age`).
    pub id: String,
    /// The catalog change applying it.
    pub action: RecommendedAction,
    /// The element kind of the target.
    pub element: ElementKind,
    /// The label of the target; `None` for a label-less index (every element of the kind).
    pub label: Option<String>,
    /// The key of the target.
    pub key: String,
    /// One line: what to do.
    pub title: String,
    /// The numbers behind it, as a sentence.
    pub evidence: String,
    /// What it costs, as a sentence.
    pub cost: String,
    /// Bytes of memory it frees (negative: it takes memory).
    pub memory_bytes: i64,
    /// Whether [`Self::memory_bytes`] is measured on the data (`false`: estimated).
    pub measured: bool,
    /// What applying it is worth.
    pub impact: Impact,
    /// How sure the advisor is.
    pub confidence: Confidence,
    /// Whether it can be undone exactly (every action of this version can: drop the rule or
    /// the index, create it again).
    pub reversible: bool,
}

/// A recommendation the instance dismissed, still advised.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DismissedRecommendation {
    /// Its id.
    pub id: String,
    /// Its title.
    pub title: String,
    /// Dismissed until (microseconds since the Unix epoch; `i64::MAX` = for good).
    pub until: i64,
}

/// The outcome of a recommendation applied in the last 30 days.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct AppliedOutcome {
    /// Its id.
    pub id: String,
    /// Its kind.
    pub kind: String,
    /// Its title.
    pub title: String,
    /// When it was applied (microseconds since the Unix epoch).
    pub at: i64,
    /// The memory it promised to free (negative: to take).
    pub promised_bytes: i64,
    /// The memory freed since (negative: taken): the graph's property and property index bytes
    /// then minus now. Other changes of the data since count too.
    pub measured_bytes: i64,
    /// For a new index: the indexed lookups of its key in the last 90 days.
    pub index_lookups: Option<u64>,
}

/// How long an applied recommendation's outcome is shown.
const OUTCOME_DAYS: i64 = 30;

/// The advisor's answer.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Advice {
    /// The recommendations, in the advisor's order (module docs), without the dismissed ones.
    pub recommendations: Vec<Recommendation>,
    /// The recommendations dismissed for now.
    pub dismissed: Vec<DismissedRecommendation>,
    /// The outcomes of the recommendations applied in the last 30 days, the latest first.
    pub applied: Vec<AppliedOutcome>,
    /// The level of the workload statistics it read (`off`: only the data was looked at, so no
    /// index advice).
    pub workload_level: WorkloadLevel,
    /// The start of the oldest day of workload statistics (microseconds since the Unix epoch),
    /// `None` without any.
    pub observed_since: Option<i64>,
    /// The commit the data was read at.
    pub commit_seq: u64,
    /// The property bytes of the graph (the memory report's sum).
    pub property_bytes: u64,
    /// The property bytes plus the property index bytes ([`measured_bytes`]): the figure an
    /// applied recommendation's outcome is measured on.
    pub memory_bytes: u64,
    /// The graph's memory a recommendation's [`Impact`] is measured against: the estimated
    /// total footprint ([`MemoryReport::estimated_total_bytes`]), or [`Self::memory_bytes`]
    /// when the storage does not report its structure.
    pub graph_bytes: u64,
    /// How many properties `estimate_compression` measured.
    pub estimates: usize,
    /// The recommendations grouped by kind (and codec), for applying a group at once.
    pub groups: Vec<AdviceGroup>,
}

/// Recommendations of the same kind (and for `add_compression` the same codec), for applying
/// several at once ([`Graph::apply_recommendations`](crate::graph::Graph::apply_recommendations)):
/// "dedup: 74 keys, total -400.1 MiB". Each member keeps its own [`Recommendation::id`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct AdviceGroup {
    /// The group's key: the kind, with the codec for `add_compression`
    /// (`add_compression:dedup`, `create_index`).
    pub key: String,
    /// The kind of its recommendations ([`RecommendedAction::kind`]).
    pub kind: String,
    /// The codec of its `add_compression` recommendations, `None` for the other kinds.
    pub codec: Option<String>,
    /// One line: `dedup: 74 keys, total -400.1 MiB` (`-`: memory freed, `+`: taken).
    pub title: String,
    /// The ids of its recommendations, in the advice's order.
    pub ids: Vec<String>,
    /// The sum of their [`Recommendation::memory_bytes`] (negative: they take memory).
    pub memory_bytes: i64,
}

/// The groups of `recommendations` ([`AdviceGroup`]), in the order their first member appears.
fn groups_of(recommendations: &[Recommendation]) -> Vec<AdviceGroup> {
    let mut groups: Vec<AdviceGroup> = Vec::new();
    for r in recommendations {
        let codec = match &r.action {
            RecommendedAction::AddCompression { candidate, .. } => {
                Some(candidate.codec.name().to_string())
            }
            RecommendedAction::CreateIndex { .. }
            | RecommendedAction::DropIndex { .. }
            | RecommendedAction::DropCompression { .. } => None,
        };
        let kind = r.action.kind();
        let key = match &codec {
            Some(codec) => format!("{kind}:{codec}"),
            None => kind.to_string(),
        };
        let at = match groups.iter().position(|g| g.key == key) {
            Some(at) => at,
            None => {
                groups.push(AdviceGroup {
                    key,
                    kind: kind.to_string(),
                    codec,
                    title: String::new(),
                    ids: Vec::new(),
                    memory_bytes: 0,
                });
                groups.len() - 1
            }
        };
        let group = &mut groups[at];
        group.ids.push(r.id.clone());
        group.memory_bytes = group.memory_bytes.saturating_add(r.memory_bytes);
    }
    for group in &mut groups {
        let what = match (group.kind.as_str(), &group.codec) {
            ("add_compression", Some(codec)) => codec.clone(),
            (kind, _) => kind.replace('_', " "),
        };
        let n = group.ids.len();
        let noun = match group.kind.as_str() {
            "drop_index" => {
                if n == 1 {
                    "index"
                } else {
                    "indexes"
                }
            }
            _ => {
                if n == 1 {
                    "key"
                } else {
                    "keys"
                }
            }
        };
        // A change of memory: `-` frees, `+` takes.
        let sign = if group.memory_bytes > 0 { "-" } else { "+" };
        group.title = format!(
            "{what}: {n} {noun}, total {sign}{}",
            mib_text(group.memory_bytes.unsigned_abs())
        );
    }
    groups
}

/// The uses of one `(element, label, key)` in the last two days and the last 90 days, summed
/// over the channels.
#[derive(Debug, Default, Clone, Copy)]
struct Uses {
    scan_runs: u64,
    scan_items: u64,
    index_runs_90: u64,
    reads: u64,
}

impl Uses {
    /// `label` `None`: the uses of a label-less target, the scans whose flow names no single
    /// label (an index lookup with any label counts as a use of it).
    fn of(
        workload: Option<&WorkloadReport>,
        element: ElementKind,
        label: Option<&str>,
        key: &str,
    ) -> Uses {
        let mut uses = Uses::default();
        let Some(workload) = workload else {
            return uses;
        };
        for series in &workload.series {
            let SeriesKind::Property(property) = &series.key.kind else {
                continue;
            };
            let same_label = match label {
                Some(label) => property.label.as_deref().is_none_or(|l| l == label),
                None => true,
            };
            let same_element = property.element.is_none_or(|e| e == element);
            if property.key != key || !same_label || !same_element {
                continue;
            }
            // A use that names the label is this property's for sure; one without a label
            // might be any label's, so it counts only for "is it read" (never for a per-label
            // index). For a label-less target exactly the uses without a label are its scans.
            let exact = property.element.is_some()
                && match label {
                    Some(_) => property.label.is_some(),
                    None => property.label.is_none(),
                };
            let two_days = &series.last_two_days;
            match property.access {
                AccessKind::FilterScan if exact => {
                    uses.scan_runs += two_days.count;
                    uses.scan_items += two_days.items;
                }
                AccessKind::FilterIndex => {
                    uses.index_runs_90 += series.last_ninety_days.count;
                }
                _ => {}
            }
            if matches!(
                property.access,
                AccessKind::FilterScan
                    | AccessKind::FilterIndex
                    | AccessKind::Read
                    | AccessKind::OrderGroup
                    | AccessKind::Similarity
            ) {
                uses.reads += two_days.count;
            }
        }
        uses
    }
}

/// The value types a property index cannot hold.
const NOT_INDEXABLE: [&str; 3] = ["vector", "array", "object"];

/// About the bytes of one distinct value of an index not yet built (the key, its 8-byte
/// postings and the tree's share; `graph/postings.rs`).
const INDEX_VALUE_BYTES: u64 = 48;

/// About the bytes of each further element of a value (a `u32` slot in the value's postings).
const INDEX_POSTING_BYTES: u64 = 4;

/// The estimated memory of an index over `entries` elements holding `distinct` values (the
/// postings layout: each value once, then 4 bytes per further element).
fn index_cost_bytes(entries: u64, distinct: u64) -> u64 {
    let distinct = distinct.clamp(entries.min(1), entries);
    distinct
        .saturating_mul(INDEX_VALUE_BYTES)
        .saturating_add((entries - distinct).saturating_mul(INDEX_POSTING_BYTES))
}

/// A byte count for a sentence: `512 B`, `12.3 KiB`, `4.0 MiB`, `1.2 GiB`.
pub(crate) fn bytes_text(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

fn element_name(element: ElementKind) -> &'static str {
    element.as_str()
}

/// The suggested name of a rule or index on `(element, label, key)`: `<label>_<key>`, with
/// `_edge` for an edge (characters a name may not hold become `_`).
pub(crate) fn suggested_name(element: ElementKind, label: &str, key: &str) -> String {
    let mut name: String = format!("{label}_{key}")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if name.starts_with(|c: char| c.is_ascii_digit()) {
        name.insert(0, '_');
    }
    if element == ElementKind::Edge {
        name.push_str("_edge");
    }
    name
}

/// The key of a top-level path, `None` for a nested one.
fn key_of(path: &crate::value::JsonPath) -> Option<String> {
    match path.segments() {
        [crate::value::JsonPathSegment::Key(key)] => Some(key.to_string()),
        _ => None,
    }
}

/// `person.age`, or `age (any label)` for a label-less index.
fn target_text(index: &PropertyIndexInfo) -> String {
    match &index.label {
        Some(label) => format!("{label}.{}", index.key),
        None => format!("{} (any label)", index.key),
    }
}

/// The top-level key of a memory row, `None` for a nested path or an unlabeled row.
fn top_level(row: &PropertyMemory) -> Option<(ElementKind, &str, String)> {
    let label = row.target.label.as_deref()?;
    Some((row.target.element, label, key_of(&row.target.path)?))
}

/// A byte count in MiB for a sentence (`1.4 MiB`, `0.2 MiB`).
fn mib_text(bytes: u64) -> String {
    format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
}

/// The `add_compression` recommendation of `best`, measured by `measured` on `(element, label,
/// key)`.
#[allow(clippy::too_many_arguments)]
fn compression_recommendation(
    element: ElementKind,
    label: &str,
    key: &str,
    measured: &crate::graph::CompressionEstimate,
    best: &crate::catalog::CandidateEstimate,
    uses: &Uses,
    workload: Option<&WorkloadReport>,
    graph_bytes: u64,
) -> Recommendation {
    let saved = u64::try_from(best.saved_bytes).unwrap_or(0);
    let codec = best.candidate.codec.name();
    let reads = if workload.is_some() {
        format!("read {} times in two days", uses.reads)
    } else {
        "reads are not counted (no workload statistics)".to_string()
    };
    let sampled = if measured.sampled {
        ", measured on a sample"
    } else {
        ""
    };
    let (title, evidence, cost) = if best.candidate.codec == Codec::Dedup {
        let one_in = best
            .compressed_values
            .checked_div(best.distinct_values)
            .unwrap_or(0);
        (
            format!("Store each distinct {label}.{key} once (dedup)"),
            format!(
                "{} values, {} distinct (one in {one_in}); dedup keeps {} instead of {}: saves {}",
                best.compressed_values,
                best.distinct_values,
                bytes_text(best.stored_bytes),
                bytes_text(best.plain_bytes),
                mib_text(saved),
            ),
            format!(
                "writes look the value up in the rule's dictionary: overwriting {key} costs \
                 about 5-9 % more, adding a key that is new to an element of {label} or a jpath \
                 write into a value of {key} about 30 % more (measured at query level); reads \
                 stay plain and are mostly faster; {reads}"
            ),
        )
    } else {
        let share = saved
            .saturating_mul(100)
            .checked_div(measured.string_bytes)
            .unwrap_or(0);
        let decode = best
            .decode_bytes_per_sec
            .map(|speed| format!(", decoding at {}/s", bytes_text(speed as u64)))
            .unwrap_or_default();
        (
            format!("Compress {label}.{key} ({codec})"),
            format!(
                "{} of strings in {} values, {} distinct; {codec} keeps {} ({share} % saved){sampled}",
                bytes_text(measured.string_bytes),
                measured.string_values,
                best.distinct_values,
                bytes_text(best.stored_bytes),
            ),
            format!("every read of {key} decompresses the value{decode}; {reads}"),
        )
    };
    Recommendation {
        id: format!("add_compression:{}:{label}:{key}", element_name(element)),
        action: RecommendedAction::AddCompression {
            name: suggested_name(element, label, key),
            element,
            label: label.to_string(),
            key: key.to_string(),
            candidate: best.candidate,
        },
        element,
        label: Some(label.to_string()),
        key: key.to_string(),
        title,
        evidence,
        cost,
        memory_bytes: saved.min(i64::MAX as u64) as i64,
        measured: true,
        impact: Impact::of_memory(saved, graph_bytes),
        confidence: if measured.sampled {
            Confidence::Medium
        } else {
            Confidence::High
        },
        reversible: true,
    }
}

/// The memory the advisor's outcomes measure: property bytes plus property index bytes.
pub fn measured_bytes(memory: &MemoryReport) -> u64 {
    memory.property_bytes()
        + memory
            .indexes
            .iter()
            .map(|index| index.stats.bytes)
            .sum::<u64>()
}

/// The recommendations for the data in `memory`, the uses in `workload` (read with the day
/// tier's history for the time observed), the instance's `marks` and the measurements of
/// `estimate` (module docs). `now` is microseconds since the Unix epoch.
pub fn compute(
    memory: &MemoryReport,
    workload: Option<&WorkloadReport>,
    marks: &[(String, AdviceMark)],
    estimate: &mut dyn FnMut(&CompressionEstimateRequest) -> Option<CompressionEstimate>,
    request: &AdviceRequest,
    now: i64,
) -> Advice {
    const DAY: i64 = 86_400_000_000;
    let observed_since = workload.and_then(|workload| {
        workload
            .series
            .iter()
            .flat_map(|series| series.history.iter().map(|bucket| bucket.start))
            .min()
    });
    let observed_days = observed_since.map_or(0, |since| ((now - since).max(0) / DAY) as u64);
    let level = workload.map_or(WorkloadLevel::Off, |w| w.level);
    let bytes_now = measured_bytes(memory);
    let graph_bytes = memory.estimated_total_bytes().unwrap_or(bytes_now);
    let mut index_advice: Vec<(u64, Recommendation)> = Vec::new();
    let mut compression_advice: Vec<(u64, Recommendation)> = Vec::new();
    let mut drop_index_advice: Vec<(u64, Recommendation)> = Vec::new();
    let mut drop_compression_advice: Vec<(u64, Recommendation)> = Vec::new();

    // create_index: full scans of a filter, per label, or label-less when the flow names no
    // single label (design D1).
    if workload.is_some() {
        let mut seen: Vec<(ElementKind, Option<CompactString>, CompactString)> = Vec::new();
        for series in workload.map_or(&[][..], |w| &w.series[..]) {
            let SeriesKind::Property(property) = &series.key.kind else {
                continue;
            };
            let Some(element) = property.element else {
                continue;
            };
            let label = property.label.clone();
            if property.access != AccessKind::FilterScan || property.key == "*" {
                continue;
            }
            let target = (element, label.clone(), property.key.clone());
            if seen.contains(&target) {
                continue;
            }
            seen.push(target);
            let key = property.key.as_str();
            let uses = Uses::of(workload, element, label.as_deref(), key);
            if uses.scan_runs < request.min_filter_runs {
                continue;
            }
            let rows: Vec<&PropertyMemory> = memory
                .properties
                .iter()
                .filter(|row| {
                    top_level(row).is_some_and(|(e, l, k)| {
                        e == element && k == key && label.as_deref().is_none_or(|label| l == label)
                    })
                })
                .collect();
            // A key holding arrays is filtered on its elements (`has(key, __.unfold().is(..))`,
            // or a whole-value has() an `each` index serves as a superset): an `each` index.
            let arrays = rows
                .iter()
                .any(|row| row.value_types.iter().any(|t| t.name == "array"));
            // A label-less index serves a labelled source too (D2); a key holding arrays only
            // an `each` one.
            let indexed = memory.indexes.iter().any(|index| {
                index.element == element
                    && index.key == key
                    && (index.label.is_none() || index.label.as_deref() == label.as_deref())
                    && (!arrays
                        || index
                            .transform
                            .as_ref()
                            .is_some_and(|transform| *transform == PropertyIndexTransform::Each))
            });
            if indexed {
                continue;
            }
            let label_elements: u64 = match &label {
                Some(label) => memory
                    .labels
                    .iter()
                    .find(|l| l.element == element && l.label.as_deref() == Some(label.as_str()))
                    .map_or_else(
                        || rows.first().map_or(0, |row| row.label_elements),
                        |l| l.elements,
                    ),
                None => memory
                    .labels
                    .iter()
                    .filter(|l| l.element == element)
                    .map(|l| l.elements)
                    .sum(),
            };
            if label_elements < request.min_index_elements {
                continue;
            }
            if rows.is_empty() {
                // The key holds no values: nothing to index.
                continue;
            }
            // Without arrays a vector or an object value is never indexed; with arrays an
            // `each` index holds their scalar elements (objects and vectors are skipped).
            if !arrays
                && rows.iter().any(|row| {
                    row.value_types
                        .iter()
                        .any(|t| NOT_INDEXABLE.contains(&t.name.as_str()))
                })
            {
                continue;
            }
            let transform = arrays.then_some(PropertyIndexTransform::Each);
            let row_elements: u64 = rows.iter().map(|row| row.elements).sum();
            // Distinct values: the string estimate of each row (a label-less index sums the
            // labels' estimates, an upper bound); a key without one counts every element as
            // its own value (the near-unique worst case).
            let distinct: u64 = rows
                .iter()
                .map(|row| match row.strings {
                    Some(strings) if strings.values == row.elements => strings.distinct_estimate,
                    _ => row.elements,
                })
                .sum();
            let cost_bytes = index_cost_bytes(row_elements, distinct);
            let confidence = if uses.scan_runs >= 100 {
                Confidence::High
            } else {
                Confidence::Medium
            };
            // A filter folded into the source step counts no items of its own: every run then
            // read the whole label.
            let (scanned, about) = if uses.scan_items > 0 {
                (uses.scan_items, "")
            } else {
                (uses.scan_runs.saturating_mul(label_elements), "about ")
            };
            let (id_label, title, scope) = match &label {
                Some(label) => (
                    label.to_string(),
                    format!("Create an index on {label}.{key}"),
                    format!("{label_elements} {label} elements"),
                ),
                None => (
                    "*".to_string(),
                    format!(
                        "Create a label-less index on {key} (every {})",
                        element.as_str()
                    ),
                    format!(
                        "{label_elements} {} elements (no single label)",
                        element.as_str()
                    ),
                ),
            };
            index_advice.push((
                scanned,
                Recommendation {
                    id: format!("create_index:{}:{id_label}:{key}", element_name(element)),
                    action: RecommendedAction::CreateIndex {
                        element,
                        label: label.as_ref().map(CompactString::to_string),
                        key: key.to_string(),
                        transform,
                    },
                    element,
                    label: label.as_ref().map(CompactString::to_string),
                    key: key.to_string(),
                    title,
                    evidence: format!(
                        "filtered {} times in two days by a full scan of {scope} ({about}{} \
                         elements read in all)",
                        uses.scan_runs, scanned
                    ),
                    cost: format!(
                        "about {} of memory for {} entries; every write of {key} updates the index",
                        bytes_text(cost_bytes),
                        row_elements
                    ),
                    memory_bytes: -(cost_bytes.min(i64::MAX as u64) as i64),
                    measured: false,
                    impact: Impact::of_count(scanned, 10_000_000, 100_000_000),
                    confidence,
                    reversible: true,
                },
            ));
        }
    }

    // drop_index: no indexed lookup in the window observed.
    if workload.is_some() && observed_days >= request.min_observed_days {
        for index in &memory.indexes {
            let uses = Uses::of(workload, index.element, index.label.as_deref(), &index.key);
            if uses.index_runs_90 > 0 {
                continue;
            }
            let confidence = if observed_days >= 30 {
                Confidence::High
            } else if observed_days >= 7 {
                Confidence::Medium
            } else {
                Confidence::Low
            };
            drop_index_advice.push((
                index.stats.bytes,
                Recommendation {
                    id: format!("drop_index:{}", index.name),
                    action: RecommendedAction::DropIndex {
                        name: index.name.clone(),
                    },
                    element: index.element,
                    label: index.label.clone(),
                    key: index.key.clone(),
                    title: format!("Drop the unused index {}", index.name),
                    evidence: format!("no query used it in {observed_days} days of statistics"),
                    cost: format!(
                        "filters on {} scan again; it can be created again at any time",
                        target_text(index)
                    ),
                    memory_bytes: index.stats.bytes.min(i64::MAX as u64) as i64,
                    measured: true,
                    impact: Impact::of_memory(index.stats.bytes, graph_bytes),
                    confidence,
                    reversible: true,
                },
            ));
        }
    }

    // drop_index: a per-label index whose key a label-less index of the same element kind and
    // transform covers too (design section 8: the entries are kept twice). The label-less one
    // serves that label's queries as well (D2), so dropping the per-label one frees its memory;
    // it may still be the faster read for its label, so the confidence is low.
    for index in &memory.indexes {
        let Some(label) = &index.label else {
            continue;
        };
        let Some(any) = memory.indexes.iter().find(|other| {
            other.label.is_none()
                && other.element == index.element
                && other.key == index.key
                && other.transform == index.transform
        }) else {
            continue;
        };
        let id = format!("drop_index:{}", index.name);
        if drop_index_advice.iter().any(|(_, r)| r.id == id) {
            continue;
        }
        drop_index_advice.push((
            index.stats.bytes,
            Recommendation {
                id,
                action: RecommendedAction::DropIndex {
                    name: index.name.clone(),
                },
                element: index.element,
                label: index.label.clone(),
                key: index.key.clone(),
                title: format!(
                    "Drop the index {}: the label-less index {} covers it",
                    index.name, any.name
                ),
                evidence: format!(
                    "the label-less index {} on {} holds every {}, {label} ones included, so the \
                     entries of {label}.{} are kept twice",
                    any.name,
                    index.key,
                    index.element.as_str(),
                    index.key
                ),
                cost: format!(
                    "filters on {label}.{} read the label-less index and check the label of each \
                     candidate; it can be created again at any time",
                    index.key
                ),
                memory_bytes: index.stats.bytes.min(i64::MAX as u64) as i64,
                measured: true,
                impact: Impact::of_memory(index.stats.bytes, graph_bytes),
                confidence: Confidence::Low,
                reversible: true,
            },
        ));
    }

    // drop_compression: a compressed key that became hot.
    for rule in &memory.compression_rules {
        let Some(label) = rule.target.label.as_deref() else {
            continue;
        };
        let Some(key) = key_of(&rule.target.path) else {
            continue;
        };
        let uses = Uses::of(workload, rule.target.element, Some(label), &key);
        if workload.is_none() || uses.reads < request.hot_reads {
            continue;
        }
        let grows = rule
            .stats
            .as_ref()
            .map_or(0, |s| s.plain_bytes.saturating_sub(s.stored_bytes));
        drop_compression_advice.push((
            uses.reads,
            Recommendation {
                id: format!("drop_compression:{}", rule.name),
                action: RecommendedAction::DropCompression {
                    name: rule.name.clone(),
                },
                element: rule.target.element,
                label: Some(label.to_string()),
                key: key.clone(),
                title: format!("Stop compressing {label}.{key} (rule {})", rule.name),
                evidence: format!(
                    "read {} times in two days: every read decompresses the value",
                    uses.reads
                ),
                cost: format!("{} more memory", bytes_text(grows)),
                memory_bytes: -(grows.min(i64::MAX as u64) as i64),
                measured: rule.stats.is_some(),
                impact: Impact::of_count(uses.reads, 10_000, 100_000),
                confidence: Confidence::Medium,
                reversible: true,
            },
        ));
    }

    // add_compression: large strings without a rule (lz4, or dedup when it wins), then values
    // of any type that repeat (dedup), measured.
    let dedup_ok = |c: &crate::catalog::CandidateEstimate| {
        c.candidate.codec == Codec::Dedup
            && c.compressed_values > 0
            && c.distinct_values
                .saturating_mul(request.dedup_distinct_ratio.max(1))
                <= c.compressed_values
            && u64::try_from(c.saved_bytes).unwrap_or(0) >= request.min_dedup_saving_bytes.max(1)
    };
    let mut candidates: Vec<(ElementKind, &str, String, u64)> = memory
        .properties
        .iter()
        .filter(|row| row.rules.is_empty() && row.indexes.is_empty())
        .filter_map(|row| {
            let (element, label, key) = top_level(row)?;
            let plain = row.strings.as_ref()?.plain_bytes;
            (plain >= request.min_compression_bytes.max(1)).then_some((element, label, key, plain))
        })
        .collect();
    candidates.sort_by_key(|c| std::cmp::Reverse(c.3));
    candidates.truncate(request.max_estimates);
    let mut estimates = 0;
    let mut measured_keys: Vec<(ElementKind, String, String)> = Vec::new();
    for (element, label, key, _) in candidates {
        let uses = Uses::of(workload, element, Some(label), &key);
        // A hot key keeps lz4 away (every read would decompress); dedup reads stay plain.
        let hot = workload.is_some() && uses.reads >= request.hot_reads;
        let Ok(estimate_request) = CompressionEstimateRequest::new(element, label, key.as_str())
        else {
            continue;
        };
        let mut wanted = if hot {
            Vec::new()
        } else {
            CompressionCandidate::matching_codecs(&[Codec::Lz4], None, None)
        };
        wanted.push(CompressionCandidate::dedup(0));
        let estimate_request = estimate_request.with_candidates(wanted);
        // A target no rule could have (an invalid label, path or threshold) gets no advice; it
        // is not measured, so an estimate error is always a real failure (`Graph::advise`).
        if estimate_request.validate().is_err() {
            continue;
        }
        let Some(measured) = estimate(&estimate_request) else {
            continue;
        };
        estimates += 1;
        measured_keys.push((element, label.to_string(), key.clone()));
        let best_lz4 = measured
            .candidates
            .iter()
            .filter(|c| c.candidate.codec == Codec::Lz4 && c.saved_bytes > 0)
            .max_by_key(|c| c.saved_bytes);
        let dedup = measured
            .candidates
            .iter()
            .find(|c| c.candidate.codec == Codec::Dedup)
            .filter(|c| dedup_ok(c));
        // Dedup only where it saves more than lz4 would.
        let best = match (dedup, best_lz4) {
            (Some(dedup), Some(lz4)) if dedup.saved_bytes > lz4.saved_bytes => dedup,
            (Some(dedup), None) => dedup,
            (_, Some(lz4)) => lz4,
            (None, None) => continue,
        };
        let saved = u64::try_from(best.saved_bytes).unwrap_or(0);
        if best.candidate.codec == Codec::Lz4 {
            let share = saved
                .saturating_mul(100)
                .checked_div(measured.string_bytes)
                .unwrap_or(0);
            if saved < request.min_saving_bytes || share < request.min_saving_percent {
                continue;
            }
        }
        compression_advice.push((
            saved,
            compression_recommendation(
                element,
                label,
                &key,
                &measured,
                best,
                &uses,
                workload,
                graph_bytes,
            ),
        ));
    }

    // dedup: values of any type at a top-level key (indexed or not) that repeat: at most one in
    // `dedup_distinct_ratio` distinct, saving at least `min_dedup_saving_bytes`.
    let code_saving = (std::mem::size_of::<ElementProperty>() as u64)
        .saturating_sub(crate::value::dictionary::CODE_BYTES);
    let mut repeated: Vec<(ElementKind, &str, String, u64)> = memory
        .properties
        .iter()
        .filter(|row| row.rules.is_empty())
        .filter_map(|row| {
            let (element, label, key) = top_level(row)?;
            if measured_keys
                .iter()
                .any(|(e, l, k)| *e == element && l == label && *k == key)
            {
                return None;
            }
            // At most every value's slot minus its code is saved; too few values save too
            // little.
            if row.elements.saturating_mul(code_saving) < request.min_dedup_saving_bytes.max(1) {
                return None;
            }
            // Strings that are mostly distinct (the walk's distinct estimate, with slack for its
            // error) are not measured.
            if let Some(strings) = row.strings.as_ref()
                && strings.values == row.elements
                && strings
                    .distinct_estimate
                    .saturating_mul(request.dedup_distinct_ratio.max(1))
                    > row.elements.saturating_mul(2)
            {
                return None;
            }
            Some((element, label, key, row.elements))
        })
        .collect();
    repeated.sort_by_key(|c| std::cmp::Reverse(c.3));
    repeated.truncate(request.max_dedup_estimates);
    for (element, label, key, _) in repeated {
        let Ok(estimate_request) = CompressionEstimateRequest::new(element, label, key.as_str())
        else {
            continue;
        };
        let estimate_request = estimate_request.with_candidates([CompressionCandidate::dedup(0)]);
        if estimate_request.validate().is_err() {
            continue;
        }
        let Some(measured) = estimate(&estimate_request) else {
            continue;
        };
        estimates += 1;
        let Some(best) = measured.candidates.iter().find(|c| dedup_ok(c)) else {
            continue;
        };
        let uses = Uses::of(workload, element, Some(label), &key);
        compression_advice.push((
            u64::try_from(best.saved_bytes).unwrap_or(0),
            compression_recommendation(
                element,
                label,
                &key,
                &measured,
                best,
                &uses,
                workload,
                graph_bytes,
            ),
        ));
    }

    let mut all = Vec::new();
    for mut group in [
        index_advice,
        compression_advice,
        drop_index_advice,
        drop_compression_advice,
    ] {
        group.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.id.cmp(&b.1.id)));
        all.extend(group.into_iter().map(|(_, r)| r));
    }
    // Stable: within one impact the order of the groups above stays.
    all.sort_by_key(|r| std::cmp::Reverse(r.impact));
    let mark_of = |id: &str| marks.iter().find(|(m, _)| m == id).map(|(_, mark)| mark);
    let mut recommendations = Vec::new();
    let mut dismissed = Vec::new();
    for r in all {
        match mark_of(&r.id).and_then(|m| m.dismissed_until) {
            Some(until) if until > now => dismissed.push(DismissedRecommendation {
                id: r.id.clone(),
                title: r.title.clone(),
                until,
            }),
            _ => recommendations.push(r),
        }
    }
    let mut applied: Vec<AppliedOutcome> = marks
        .iter()
        .filter_map(|(id, mark)| {
            let record = mark.applied.as_ref()?;
            if now.saturating_sub(record.at) > OUTCOME_DAYS * DAY {
                return None;
            }
            let index_lookups = (record.kind == "create_index")
                .then(|| {
                    // create_index:<element>:<label>:<key> (`*`: a label-less index)
                    let mut parts = id.splitn(4, ':').skip(1);
                    let element = match parts.next()? {
                        "edge" => ElementKind::Edge,
                        _ => ElementKind::Vertex,
                    };
                    let (label, key) = (parts.next()?, parts.next()?);
                    let label = (label != "*").then_some(label);
                    Some(Uses::of(workload, element, label, key).index_runs_90)
                })
                .flatten();
            Some(AppliedOutcome {
                id: id.clone(),
                kind: record.kind.clone(),
                title: record.title.clone(),
                at: record.at,
                promised_bytes: record.promised_bytes,
                measured_bytes: i64::try_from(record.bytes_before).unwrap_or(i64::MAX)
                    - i64::try_from(bytes_now).unwrap_or(i64::MAX),
                index_lookups,
            })
        })
        .collect();
    applied.sort_by(|a, b| b.at.cmp(&a.at).then_with(|| a.id.cmp(&b.id)));
    Advice {
        dismissed,
        applied,
        workload_level: level,
        observed_since,
        commit_seq: memory.commit_seq,
        property_bytes: memory.property_bytes(),
        memory_bytes: bytes_now,
        graph_bytes,
        estimates,
        groups: groups_of(&recommendations),
        recommendations,
    }
}

impl Advice {
    /// The advice as a value: `{workload_level, observed_since, commit_seq, property_bytes,
    /// memory_bytes, graph_bytes, estimates, recommendations: [{id, kind, element, label, key,
    /// title, evidence, cost, memory_bytes, measured, impact, confidence, reversible, command,
    /// action: {..}}], groups: [{key, kind, codec, title, ids, memory_bytes}], dismissed: [{id,
    /// title, until (null: for good)}], applied: [{id, kind,
    /// title, at, promised_bytes, measured_bytes, index_lookups}]}` (the one form every front
    /// end shows; snake_case keys like the workload report).
    pub fn to_property(&self) -> ElementProperty {
        let text = |value: &str| ElementProperty::from(value);
        let int = |value: i64| ElementProperty::from(value);
        let object = |map: PropertyMap| ElementProperty::Object(Box::new(map));
        let recommendations = self
            .recommendations
            .iter()
            .map(|r| {
                let mut action = PropertyMap::default();
                action.insert("kind", text(r.action.kind()));
                match &r.action {
                    RecommendedAction::CreateIndex {
                        element,
                        label,
                        key,
                        transform,
                    } => {
                        action.insert("element", text(element.as_str()));
                        action.insert(
                            "label",
                            label.as_deref().map_or(ElementProperty::Null, text),
                        );
                        action.insert("key", text(key));
                        if let Some(transform) = transform {
                            action.insert(
                                "transform",
                                crate::graph::property_index::transform_payload(transform),
                            );
                        }
                    }
                    RecommendedAction::DropIndex { name }
                    | RecommendedAction::DropCompression { name } => {
                        action.insert("name", text(name));
                    }
                    RecommendedAction::AddCompression {
                        name,
                        element,
                        label,
                        key,
                        candidate,
                    } => {
                        action.insert("name", text(name));
                        action.insert("element", text(element.as_str()));
                        action.insert("label", text(label));
                        action.insert("path", text(key));
                        action.insert("codec", text(candidate.codec.name()));
                        action.insert("min_bytes", int(i64::from(candidate.min_bytes)));
                        action.insert("dictionary", ElementProperty::from(candidate.dictionary));
                    }
                }
                let mut map = PropertyMap::default();
                map.insert("id", text(&r.id));
                map.insert("kind", text(r.action.kind()));
                map.insert("element", text(r.element.as_str()));
                map.insert(
                    "label",
                    r.label.as_deref().map_or(ElementProperty::Null, text),
                );
                map.insert("key", text(&r.key));
                map.insert("title", text(&r.title));
                map.insert("evidence", text(&r.evidence));
                map.insert("cost", text(&r.cost));
                map.insert("memory_bytes", int(r.memory_bytes));
                map.insert("measured", ElementProperty::from(r.measured));
                map.insert("impact", text(r.impact.name()));
                map.insert("confidence", text(r.confidence.name()));
                map.insert("reversible", ElementProperty::from(r.reversible));
                map.insert("command", text(&r.action.command()));
                map.insert("action", object(action));
                object(map)
            })
            .collect();
        let dismissed = self
            .dismissed
            .iter()
            .map(|d| {
                let mut map = PropertyMap::default();
                map.insert("id", text(&d.id));
                map.insert("title", text(&d.title));
                map.insert(
                    "until",
                    if d.until == i64::MAX {
                        ElementProperty::Null
                    } else {
                        int(d.until)
                    },
                );
                object(map)
            })
            .collect();
        let applied = self
            .applied
            .iter()
            .map(|a| {
                let mut map = PropertyMap::default();
                map.insert("id", text(&a.id));
                map.insert("kind", text(&a.kind));
                map.insert("title", text(&a.title));
                map.insert("at", int(a.at));
                map.insert("promised_bytes", int(a.promised_bytes));
                map.insert("measured_bytes", int(a.measured_bytes));
                map.insert(
                    "index_lookups",
                    a.index_lookups.map_or(ElementProperty::Null, |n| {
                        int(i64::try_from(n).unwrap_or(i64::MAX))
                    }),
                );
                object(map)
            })
            .collect();
        let mut map = PropertyMap::default();
        map.insert("workload_level", text(self.workload_level.name()));
        map.insert(
            "observed_since",
            self.observed_since.map_or(ElementProperty::Null, int),
        );
        map.insert(
            "commit_seq",
            int(i64::try_from(self.commit_seq).unwrap_or(i64::MAX)),
        );
        map.insert(
            "property_bytes",
            int(i64::try_from(self.property_bytes).unwrap_or(i64::MAX)),
        );
        map.insert(
            "memory_bytes",
            int(i64::try_from(self.memory_bytes).unwrap_or(i64::MAX)),
        );
        map.insert(
            "graph_bytes",
            int(i64::try_from(self.graph_bytes).unwrap_or(i64::MAX)),
        );
        map.insert("estimates", int(self.estimates as i64));
        map.insert("recommendations", ElementProperty::Array(recommendations));
        let groups = self
            .groups
            .iter()
            .map(|g| {
                let mut map = PropertyMap::default();
                map.insert("key", text(&g.key));
                map.insert("kind", text(&g.kind));
                map.insert(
                    "codec",
                    g.codec.as_deref().map_or(ElementProperty::Null, text),
                );
                map.insert("title", text(&g.title));
                map.insert(
                    "ids",
                    ElementProperty::Array(g.ids.iter().map(|id| text(id)).collect()),
                );
                map.insert("memory_bytes", int(g.memory_bytes));
                object(map)
            })
            .collect();
        map.insert("groups", ElementProperty::Array(groups));
        map.insert("dismissed", ElementProperty::Array(dismissed));
        map.insert("applied", ElementProperty::Array(applied));
        object(map)
    }

    /// [`Self::to_property`] as JSON text.
    pub fn to_json(&self) -> String {
        crate::value::physical::to_json_text(&self.to_property()).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_index_cost_follows_the_postings_layout() {
        // A near-unique key: one map entry per element.
        assert_eq!(index_cost_bytes(1000, 1000), 48_000);
        // A low-cardinality key: each value once, 4 bytes per further element.
        assert_eq!(index_cost_bytes(1000, 10), 10 * 48 + 990 * 4);
        // An estimate above the elements (HLL error) is clamped; no elements cost nothing.
        assert_eq!(index_cost_bytes(10, 12), 480);
        assert_eq!(index_cost_bytes(0, 0), 0);
        assert_eq!(index_cost_bytes(5, 0), 48 + 4 * 4);
    }
}
