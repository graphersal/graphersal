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
//! | `create_index` | a `(element, label, key)` filtered by a full scan at least `min_filter_runs` times in two days, on a label of at least `min_index_elements` elements, no index serving it, values an index holds | the scans become index lookups; costs index memory (estimated) |
//! | `drop_index` | an index no query used for its key in the window observed, after at least `min_observed_days` days of statistics | frees the index's memory (measured) |
//! | `add_compression` | a top-level string property of at least `min_compression_bytes` without a rule, not indexed, read fewer than `hot_reads` times in two days, whose best measured codec saves at least `min_saving_bytes` and `min_saving_percent` | frees memory (measured on the data by `estimate_compression`); reads decompress |
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
    /// `add_compression`: the most properties measured (the largest first; default 10).
    pub max_estimates: usize,
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
        }
    }
}

/// What a recommendation does: the catalog change applying it.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum RecommendedAction {
    /// Create a property index on `(element, label, key)`.
    CreateIndex {
        /// The element kind.
        element: ElementKind,
        /// The label.
        label: String,
        /// The top-level key.
        key: String,
    },
    /// Drop the property index `name`.
    DropIndex {
        /// The index's name.
        name: String,
    },
    /// Define the compression rule `name` on `(element, label, key)` with `candidate`.
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
            } => match element {
                ElementKind::Vertex => {
                    format!(
                        "g.create_index(#{{label: {}, key: {}}})",
                        text(label),
                        text(key)
                    )
                }
                ElementKind::Edge => format!(
                    "g.create_index(#{{element: \"edge\", label: {}, key: {}}})",
                    text(label),
                    text(key)
                ),
            },
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
    /// The label of the target.
    pub label: String,
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
    fn of(workload: Option<&WorkloadReport>, element: ElementKind, label: &str, key: &str) -> Uses {
        let mut uses = Uses::default();
        let Some(workload) = workload else {
            return uses;
        };
        for series in &workload.series {
            let SeriesKind::Property(property) = &series.key.kind else {
                continue;
            };
            let same_label = property.label.as_deref().is_none_or(|l| l == label);
            let same_element = property.element.is_none_or(|e| e == element);
            if property.key != key || !same_label || !same_element {
                continue;
            }
            // A use that names the label is this property's for sure; one without a label
            // might be any label's, so it counts only for "is it read" (never for an index).
            let exact = property.label.is_some() && property.element.is_some();
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

/// About the bytes of one index entry (the value, the element and the tree's share), for the
/// cost of an index not yet built.
const INDEX_ENTRY_BYTES: u64 = 48;

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
fn suggested_name(element: ElementKind, label: &str, key: &str) -> String {
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

/// The top-level key of a memory row, `None` for a nested path or an unlabeled row.
fn top_level(row: &PropertyMemory) -> Option<(ElementKind, &str, String)> {
    let label = row.target.label.as_deref()?;
    Some((row.target.element, label, key_of(&row.target.path)?))
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

    // create_index: full scans of a labelled filter.
    if workload.is_some() {
        let mut seen: Vec<(ElementKind, CompactString, CompactString)> = Vec::new();
        for series in workload.map_or(&[][..], |w| &w.series[..]) {
            let SeriesKind::Property(property) = &series.key.kind else {
                continue;
            };
            let (Some(element), Some(label)) = (property.element, property.label.clone()) else {
                continue;
            };
            if property.access != AccessKind::FilterScan || property.key == "*" {
                continue;
            }
            let target = (element, label.clone(), property.key.clone());
            if seen.contains(&target) {
                continue;
            }
            seen.push(target);
            let key = property.key.as_str();
            let uses = Uses::of(workload, element, &label, key);
            if uses.scan_runs < request.min_filter_runs {
                continue;
            }
            let indexed = memory
                .indexes
                .iter()
                .any(|index| index.element == element && index.label == label && index.key == key);
            if indexed {
                continue;
            }
            let row = memory.properties.iter().find(|row| {
                top_level(row).is_some_and(|(e, l, k)| e == element && l == label && k == key)
            });
            let label_elements = memory
                .labels
                .iter()
                .find(|l| l.element == element && l.label.as_deref() == Some(label.as_str()))
                .map_or_else(|| row.map_or(0, |row| row.label_elements), |l| l.elements);
            if label_elements < request.min_index_elements {
                continue;
            }
            let Some(row) = row else {
                // The key holds no values: nothing to index.
                continue;
            };
            if row
                .value_types
                .iter()
                .any(|t| NOT_INDEXABLE.contains(&t.name.as_str()))
            {
                continue;
            }
            let cost_bytes = row.elements.saturating_mul(INDEX_ENTRY_BYTES);
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
            index_advice.push((
                scanned,
                Recommendation {
                    id: format!("create_index:{}:{label}:{key}", element_name(element)),
                    action: RecommendedAction::CreateIndex {
                        element,
                        label: label.to_string(),
                        key: key.to_string(),
                    },
                    element,
                    label: label.to_string(),
                    key: key.to_string(),
                    title: format!("Create an index on {label}.{key}"),
                    evidence: format!(
                        "filtered {} times in two days by a full scan of {} {} elements \
                         ({about}{} elements read in all)",
                        uses.scan_runs, label_elements, label, scanned
                    ),
                    cost: format!(
                        "about {} of memory for {} entries; every write of {key} updates the index",
                        bytes_text(cost_bytes),
                        row.elements
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
            let uses = Uses::of(workload, index.element, &index.label, &index.key);
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
                        "filters on {}.{} scan again; it can be created again at any time",
                        index.label, index.key
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

    // drop_compression: a compressed key that became hot.
    for rule in &memory.compression_rules {
        let Some(label) = rule.target.label.as_deref() else {
            continue;
        };
        let Some(key) = key_of(&rule.target.path) else {
            continue;
        };
        let uses = Uses::of(workload, rule.target.element, label, &key);
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
                label: label.to_string(),
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

    // add_compression: large strings without a rule, measured.
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
    for (element, label, key, _) in candidates {
        let uses = Uses::of(workload, element, label, &key);
        if workload.is_some() && uses.reads >= request.hot_reads {
            continue;
        }
        let Ok(estimate_request) = CompressionEstimateRequest::new(element, label, key.as_str())
        else {
            continue;
        };
        let estimate_request = estimate_request.with_candidates(
            CompressionCandidate::matching_codecs(&[Codec::Lz4, Codec::Dedup], None, None),
        );
        let Some(measured) = estimate(&estimate_request) else {
            continue;
        };
        estimates += 1;
        let Some(best) = measured.best.and_then(|i| measured.candidates.get(i)) else {
            continue;
        };
        let saved = u64::try_from(best.saved_bytes).unwrap_or(0);
        let share = saved
            .saturating_mul(100)
            .checked_div(measured.string_bytes)
            .unwrap_or(0);
        if saved < request.min_saving_bytes || share < request.min_saving_percent {
            continue;
        }
        let codec = best.candidate.codec.name();
        let reads = if workload.is_some() {
            format!("read {} times in two days", uses.reads)
        } else {
            "reads are not counted (no workload statistics)".to_string()
        };
        let decode = best
            .decode_bytes_per_sec
            .map(|speed| format!(", decoding at {}/s", bytes_text(speed as u64)))
            .unwrap_or_default();
        compression_advice.push((
            saved,
            Recommendation {
                id: format!("add_compression:{}:{label}:{key}", element_name(element)),
                action: RecommendedAction::AddCompression {
                    name: suggested_name(element, label, &key),
                    element,
                    label: label.to_string(),
                    key: key.clone(),
                    candidate: best.candidate,
                },
                element,
                label: label.to_string(),
                key: key.clone(),
                title: if best.candidate.codec == Codec::Dedup {
                    format!("Store each distinct {label}.{key} once (dedup)")
                } else {
                    format!("Compress {label}.{key} ({codec})")
                },
                evidence: format!(
                    "{} of strings in {} values, {} distinct; {codec} keeps {} ({share} % saved){}",
                    bytes_text(measured.string_bytes),
                    measured.string_values,
                    best.distinct_values,
                    bytes_text(best.stored_bytes),
                    if measured.sampled {
                        ", measured on a sample"
                    } else {
                        ""
                    }
                ),
                cost: if best.candidate.codec == Codec::Dedup {
                    format!("reads of {key} stay plain (a shared copy, no decompression); {reads}")
                } else {
                    format!("every read of {key} decompresses the value{decode}; {reads}")
                },
                memory_bytes: saved.min(i64::MAX as u64) as i64,
                measured: true,
                impact: Impact::of_memory(saved, graph_bytes),
                confidence: if measured.sampled {
                    Confidence::Medium
                } else {
                    Confidence::High
                },
                reversible: true,
            },
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
                    // create_index:<element>:<label>:<key>
                    let mut parts = id.splitn(4, ':').skip(1);
                    let element = match parts.next()? {
                        "edge" => ElementKind::Edge,
                        _ => ElementKind::Vertex,
                    };
                    let (label, key) = (parts.next()?, parts.next()?);
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
        recommendations,
        dismissed,
        applied,
        workload_level: level,
        observed_since,
        commit_seq: memory.commit_seq,
        property_bytes: memory.property_bytes(),
        memory_bytes: bytes_now,
        graph_bytes,
        estimates,
    }
}

impl Advice {
    /// The advice as a value: `{workload_level, observed_since, commit_seq, property_bytes,
    /// memory_bytes, graph_bytes, estimates, recommendations: [{id, kind, element, label, key,
    /// title, evidence, cost, memory_bytes, measured, impact, confidence, reversible, command,
    /// action: {..}}], dismissed: [{id, title, until (null: for good)}], applied: [{id, kind,
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
                    } => {
                        action.insert("element", text(element.as_str()));
                        action.insert("label", text(label));
                        action.insert("key", text(key));
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
                map.insert("label", text(&r.label));
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
        map.insert("dismissed", ElementProperty::Array(dismissed));
        map.insert("applied", ElementProperty::Array(applied));
        object(map)
    }

    /// [`Self::to_property`] as JSON text.
    pub fn to_json(&self) -> String {
        crate::value::physical::to_json_text(&self.to_property()).unwrap_or_default()
    }
}
