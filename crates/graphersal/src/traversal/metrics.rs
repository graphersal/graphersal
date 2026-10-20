// Structured profiling results; the public documentation is the module docs of `crate::profile`.

use floating_duration::TimeFormat;
use std::time::Duration;

use compact_str::CompactString;

use crate::value::{ElementProperty, Number, PropertyMap};

const TOTAL: &str = "TOTAL:";

/// The profile of one traversal run: its total duration, one [`Metrics`] per top-level step of
/// the optimized plan (child traversals nest under the step holding them), and the optimizer
/// rules that rewrote the plan.
///
/// Follows the [extensibility rule](crate::profile#extensibility-rule): a new
/// [`ProfileType`](crate::ProfileType) only adds an optional section to [`Metrics`]; existing
/// fields never change. `#[non_exhaustive]` and built only by the library.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct TraversalMetrics {
    pub(crate) duration: Duration,
    pub(crate) metrics: Vec<Metrics>,
    pub(crate) optimizer_rules_applied: Vec<String>,
}

/// The figures of one step of the optimized plan.
///
/// Steps are named as they run, fused steps under their real names (e.g.
/// `v(labels: ["person"]).count()`); the name is plain step text, what the step records into paths
/// is the separate [`path_recording`](Self::path_recording).
/// The [`id`](Self::id) is the step's static position in the optimized plan (see the book page
/// "Profiling and execute()"); a failing step of an [`Execution`](crate::Execution) carries the
/// same id in its error location, so a partial profile and the error join by id.
///
/// Follows the [extensibility rule](crate::profile#extensibility-rule). `#[non_exhaustive]`.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Metrics {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) path_recording: Option<String>,
    pub(crate) duration: Duration,
    pub(crate) counts: StepCounts,
    pub(crate) percent_duration: f64,
    pub(crate) nested: Vec<Metrics>,
    pub(crate) calls: u64,
    pub(crate) count_in: u64,
    pub(crate) timing: Option<TimingMetrics>,
    pub(crate) loops: Option<LoopMetrics>,
    pub(crate) memory: Option<MemoryMetrics>,
}

/// How many traversers a step emitted. `#[non_exhaustive]`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct StepCounts {
    pub(crate) traverser_count: u64,
    pub(crate) element_count: u64,
}

/// Fastest and slowest call of a step that ran more than once (per-traverser child steps, for
/// example). Present once the step completed a call. `#[non_exhaustive]`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct TimingMetrics {
    pub(crate) min_duration: Duration,
    pub(crate) max_duration: Duration,
}

/// Loop statistics of a `repeat()` step, accumulated over all its executions. Present only on
/// `repeat()` steps. `#[non_exhaustive]`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
#[non_exhaustive]
pub struct LoopMetrics {
    pub(crate) total_loops: u64,
    pub(crate) max_depth: u64,
    pub(crate) avg_loops_per_traverser: f64,
}

/// The memory section of a step, present only when the run was profiled with
/// `ProfileType::Memory` (absent otherwise: memory was not measured, which is not "zero
/// allocation"). `#[non_exhaustive]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct MemoryMetrics {
    pub(crate) bytes_materialized: u64,
    pub(crate) bytes_retained: Option<i64>,
    pub(crate) alloc_count: Option<u64>,
    pub(crate) source: MemorySource,
}

/// Provenance of a [`MemoryMetrics`] section.
///
/// `#[non_exhaustive]`: a future partially-tracked/sampled mode is plausible, so every external
/// `match` needs a wildcard arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MemorySource {
    /// `size_of`/capacity-based estimate: always available, no allocator hook. Expect roughly
    /// 10-30% divergence from true bytes for many-small-allocations cases, and no visibility into
    /// temporary churn freed within the step. Only steps that materialize data (`group()`,
    /// `fold()`, `path()`, ...) report non-zero bytes.
    Estimated,
    /// A real allocator-level delta, captured via the `alloc-tracking` feature's
    /// `TrackingAllocator`. Only meaningful when that feature is compiled in **and** the binary
    /// installed the allocator as its `#[global_allocator]`: the library cannot verify the
    /// latter, so a consumer that enables the feature but forgets the allocator sees `0` for
    /// every step. `graphersal` (the CLI) and `graphersal-playground` are wired correctly.
    Tracked,
}

impl MemorySource {
    /// The snake_case name used in the Rhai map and the JSON form: `estimated` or `tracked`.
    pub fn as_str(&self) -> &'static str {
        match self {
            MemorySource::Estimated => "estimated",
            MemorySource::Tracked => "tracked",
        }
    }
}

impl TraversalMetrics {
    /// Duration of the whole run of the step pipeline; every step's
    /// [`percent_duration`](Metrics::percent_duration) is relative to it.
    pub fn duration(&self) -> Duration {
        self.duration
    }

    /// The top-level steps of the optimized plan, in plan order.
    pub fn metrics(&self) -> &[Metrics] {
        &self.metrics
    }

    /// Names of the optimizer rules that modified the plan (top-level and nested, deduplicated,
    /// in pipeline order); empty when none fired.
    pub fn optimizer_rules_applied(&self) -> &[String] {
        &self.optimizer_rules_applied
    }

    /// The step with the given id (`"3"`, `"3.1.0"`), at any nesting depth.
    pub fn find(&self, id: &str) -> Option<&Metrics> {
        fn find_in<'a>(list: &'a [Metrics], id: &str) -> Option<&'a Metrics> {
            list.iter().find_map(|m| {
                if m.id == id {
                    Some(m)
                } else {
                    find_in(&m.nested, id)
                }
            })
        }
        find_in(&self.metrics, id)
    }

    /// The metrics as a plain value tree with snake_case keys, the shape of the Rhai map
    /// (`to_map()`) and of [`Self::to_json`]: `{duration_ns, metrics: [..],
    /// optimizer_rules_applied: [..]}`, each step `{id, name, duration_ns, percent_duration,
    /// path_recording?, counts: {traverser_count, element_count}, calls, count_in, timing?, loops?, memory?,
    /// nested: [..]}`. Durations are integer nanoseconds; an optional section is absent unless
    /// present in the data.
    pub fn to_property(&self) -> ElementProperty {
        let mut map = PropertyMap::default();
        map.insert("duration_ns", nanos(self.duration));
        map.insert(
            "metrics",
            ElementProperty::Array(self.metrics.iter().map(Metrics::to_property).collect()),
        );
        map.insert(
            "optimizer_rules_applied",
            ElementProperty::Array(
                self.optimizer_rules_applied
                    .iter()
                    .map(|rule| ElementProperty::String(CompactString::new(rule)))
                    .collect(),
            ),
        );
        ElementProperty::Object(Box::new(map))
    }

    /// The metrics as compact JSON text: [`Self::to_property`] written by the physical codec.
    pub fn to_json(&self) -> String {
        crate::value::physical::to_json_text(&self.to_property()).unwrap_or_default()
    }
}

impl Metrics {
    /// Static position of the step in the optimized plan: the top-level index (`"3"`), or for a
    /// step inside a child traversal `<step>.<child>.<step>...` (`"3.1.0"` = top-level step 3,
    /// its child traversal 1, step 0 in it).
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The step as it runs in the optimized plan, in its canonical rendering: the DSL call that
    /// builds it (`has_label("person")`, `where(__.out())`, fused steps such as
    /// `v(labels: ["person"]).count()`), in snake_case or, with `g.with("render.spelling",
    /// "camel")`, in the Gremlin spelling. Not TinkerPop's step class name
    /// (`HasStep([~label.eq(person)])`), a deliberate deviation (`tinkerpop_deviations.md`).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// What the step records into traverser paths for later steps, set by the path requirement
    /// analysis: `full` or `labels(a,b)`; `None` when it records nothing. The text table shows it
    /// after the name (`out() [path: full]`).
    pub fn path_recording(&self) -> Option<&str> {
        self.path_recording.as_deref()
    }

    /// Total time spent in the step over all its calls (child steps included).
    pub fn duration(&self) -> Duration {
        self.duration
    }

    /// Traversers the step emitted ([`StepCounts`]).
    pub fn counts(&self) -> StepCounts {
        self.counts
    }

    /// Share of [`TraversalMetrics::duration`] spent in this step, in percent.
    pub fn percent_duration(&self) -> f64 {
        self.percent_duration
    }

    /// The steps of this step's child traversals (`where()`, `union()` branches, `by()`,
    /// `repeat()` bodies, ...), ordered by id.
    pub fn nested(&self) -> &[Metrics] {
        &self.nested
    }

    /// How many times the step completed: `1` for a top-level step, once per traverser (or per
    /// batch) for a step inside a child traversal; `0` for a step that failed on its first call.
    pub fn calls(&self) -> u64 {
        self.calls
    }

    /// Traversers handed to the step, over all calls.
    pub fn count_in(&self) -> u64 {
        self.count_in
    }

    /// Fastest and slowest call; `None` before the first completed call.
    pub fn timing(&self) -> Option<TimingMetrics> {
        self.timing
    }

    /// Loop statistics; present only on `repeat()` steps.
    pub fn loops(&self) -> Option<LoopMetrics> {
        self.loops
    }

    /// Memory figures; present only when profiled with `ProfileType::Memory`.
    pub fn memory(&self) -> Option<MemoryMetrics> {
        self.memory
    }

    fn to_property(&self) -> ElementProperty {
        let mut map = PropertyMap::default();
        map.insert("id", ElementProperty::String(CompactString::new(&self.id)));
        map.insert(
            "name",
            ElementProperty::String(CompactString::new(&self.name)),
        );
        if let Some(recording) = &self.path_recording {
            map.insert("path_recording", text_value(recording));
        }
        map.insert("duration_ns", nanos(self.duration));
        map.insert(
            "percent_duration",
            ElementProperty::Number(Number::from_f64(self.percent_duration)),
        );
        let mut counts = PropertyMap::default();
        counts.insert("traverser_count", int(self.counts.traverser_count));
        counts.insert("element_count", int(self.counts.element_count));
        map.insert("counts", ElementProperty::Object(Box::new(counts)));
        map.insert("calls", int(self.calls));
        map.insert("count_in", int(self.count_in));
        if let Some(timing) = &self.timing {
            let mut section = PropertyMap::default();
            section.insert("min_duration_ns", nanos(timing.min_duration));
            section.insert("max_duration_ns", nanos(timing.max_duration));
            map.insert("timing", ElementProperty::Object(Box::new(section)));
        }
        if let Some(loops) = &self.loops {
            let mut section = PropertyMap::default();
            section.insert("total_loops", int(loops.total_loops));
            section.insert("max_depth", int(loops.max_depth));
            section.insert(
                "avg_loops_per_traverser",
                ElementProperty::Number(Number::from_f64(loops.avg_loops_per_traverser)),
            );
            map.insert("loops", ElementProperty::Object(Box::new(section)));
        }
        if let Some(memory) = &self.memory {
            let mut section = PropertyMap::default();
            section.insert("bytes_materialized", int(memory.bytes_materialized));
            section.insert(
                "bytes_retained",
                memory
                    .bytes_retained
                    .map_or(ElementProperty::Null, |bytes| {
                        ElementProperty::Number(Number::from_i64(bytes))
                    }),
            );
            section.insert(
                "alloc_count",
                memory.alloc_count.map_or(ElementProperty::Null, int),
            );
            section.insert(
                "source",
                ElementProperty::String(CompactString::new(memory.source.as_str())),
            );
            map.insert("memory", ElementProperty::Object(Box::new(section)));
        }
        map.insert(
            "nested",
            ElementProperty::Array(self.nested.iter().map(Metrics::to_property).collect()),
        );
        ElementProperty::Object(Box::new(map))
    }
}

impl StepCounts {
    /// Traverser objects the step emitted (TinkerPop `traverserCount`).
    pub fn traverser_count(&self) -> u64 {
        self.traverser_count
    }

    /// Logical traversers they stand for, the sum of their bulks (TinkerPop `elementCount`).
    /// Equal to [`Self::traverser_count`] unless merged traversers carry a bulk.
    pub fn element_count(&self) -> u64 {
        self.element_count
    }
}

impl TimingMetrics {
    /// The fastest completed call.
    pub fn min_duration(&self) -> Duration {
        self.min_duration
    }

    /// The slowest completed call.
    pub fn max_duration(&self) -> Duration {
        self.max_duration
    }
}

impl LoopMetrics {
    /// Body executions: iterations in breadth-first order, expanded traversers in depth-first.
    pub fn total_loops(&self) -> u64 {
        self.total_loops
    }

    /// Highest iteration count any traverser reached.
    pub fn max_depth(&self) -> u64 {
        self.max_depth
    }

    /// Mean loop counter of the traversers the loop emitted.
    pub fn avg_loops_per_traverser(&self) -> f64 {
        self.avg_loops_per_traverser
    }
}

impl MemoryMetrics {
    /// Gross bytes the step requested from the allocator ([`MemorySource::Tracked`]) or the
    /// estimated heap bytes its output newly owns ([`MemorySource::Estimated`]).
    pub fn bytes_materialized(&self) -> u64 {
        self.bytes_materialized
    }

    /// **Net** bytes the step retained (live bytes at exit minus at entry; negative when it freed
    /// more than it kept). `None` unless the source is [`MemorySource::Tracked`].
    pub fn bytes_retained(&self) -> Option<i64> {
        self.bytes_retained
    }

    /// Allocator calls made during the step. `None` unless the source is
    /// [`MemorySource::Tracked`].
    pub fn alloc_count(&self) -> Option<u64> {
        self.alloc_count
    }

    /// Whether the figures are measured or estimated.
    pub fn source(&self) -> MemorySource {
        self.source
    }
}

/// A string value.
fn text_value(value: &str) -> ElementProperty {
    ElementProperty::String(CompactString::new(value))
}

/// A count as an integer value (saturating at `i64::MAX`).
fn int(value: u64) -> ElementProperty {
    ElementProperty::Number(Number::from_i64(i64::try_from(value).unwrap_or(i64::MAX)))
}

/// A duration as integer nanoseconds (saturating at `i64::MAX`, about 292 years).
fn nanos(duration: Duration) -> ElementProperty {
    ElementProperty::Number(Number::from_i64(
        i64::try_from(duration.as_nanos()).unwrap_or(i64::MAX),
    ))
}

/// Formats a byte count as `B`/`KB`/`MB`. `~` prefixes an estimated figure so a reader can tell
/// exact numbers from estimates.
fn format_bytes(bytes: u64, source: MemorySource) -> String {
    let prefix = match source {
        MemorySource::Estimated => "~",
        MemorySource::Tracked => "",
    };
    let formatted = if bytes >= 1024 * 1024 {
        format!("{:.2}MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.2}KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes}B")
    };
    format!("{prefix}{formatted}")
}

/// Formats a signed byte delta (`bytes_retained`) with an explicit `-` for a net-freeing delta.
fn format_signed_bytes(bytes: i64) -> String {
    let sign = if bytes < 0 { "-" } else { "" };
    format!(
        "{sign}{}",
        format_bytes(bytes.unsigned_abs(), MemorySource::Tracked)
    )
}

/// Largest char boundary of `s` at or below `index` (`str::floor_char_boundary` is stable only
/// from Rust 1.91; this keeps the crate's MSRV lower).
fn floor_char_boundary(s: &str, index: usize) -> usize {
    if index >= s.len() {
        return s.len();
    }
    (0..=index)
        .rev()
        .find(|&i| s.is_char_boundary(i))
        .unwrap_or(0)
}

fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        return s.to_string();
    }
    if max_len <= 3 {
        let idx = floor_char_boundary(s, max_len);
        return s[..idx].to_string();
    }
    let idx = floor_char_boundary(s, max_len - 3);
    format!("{}...", &s[..idx])
}

/// `Mem` column text of one row.
fn mem_column(node: &Metrics) -> String {
    node.memory
        .map(|m| format_bytes(m.bytes_materialized, m.source))
        .unwrap_or_else(|| "-".to_string())
}

/// `Retained` column text of one row: the tracked figure, `-` without one.
fn retained_column(node: &Metrics) -> String {
    node.memory
        .and_then(|m| m.bytes_retained)
        .map(format_signed_bytes)
        .unwrap_or_else(|| "-".to_string())
}

/// `Allocs` column text of one row: the call count when tracked, `-` otherwise.
fn allocs_column(node: &Metrics) -> String {
    node.memory
        .and_then(|m| m.alloc_count)
        .map(|n| n.to_string())
        .unwrap_or_else(|| "-".to_string())
}

/// Whether `node` or a descendant stands for more logical traversers than it has objects, which
/// is when the table gets a `Bulk` column.
fn has_bulk(node: &Metrics) -> bool {
    node.counts.element_count != node.counts.traverser_count || node.nested.iter().any(has_bulk)
}

/// Whether `node` or a descendant carries a memory section.
fn has_memory(node: &Metrics) -> bool {
    node.memory.is_some() || node.nested.iter().any(has_memory)
}

/// Visits `node` and every descendant, depth first.
fn walk<'a>(node: &'a Metrics, visit: &mut dyn FnMut(&'a Metrics)) {
    visit(node);
    for child in &node.nested {
        walk(child, visit);
    }
}

/// Column widths of the text table, gathered over every row before any is rendered.
#[derive(Clone, Copy)]
struct ColumnWidths {
    step_len: usize,
    call_len: usize,
    in_len: usize,
    out_len: usize,
    time_len: usize,
    mem_len: usize,
    retained_len: usize,
    allocs_len: usize,
    dur_len: usize,
    show_mem: bool,
    show_bulk: bool,
    bulk_len: usize,
}

/// Indentation of a nested row's step text (`\\> ` after spaces), empty at the top level.
fn step_prefix(depth: usize) -> String {
    if depth == 0 {
        String::new()
    } else {
        format!("{}\\> ", " ".repeat(6 * depth - 4))
    }
}

/// Step column text of one row: the step name plus its `[path: ..]` recording.
fn step_text(node: &Metrics) -> String {
    match &node.path_recording {
        Some(recording) => format!("{} [path: {recording}]", node.name),
        None => node.name.clone(),
    }
}

/// Width (in bytes) the step column needs to show `node` and its descendants uncut.
fn step_width(node: &Metrics, depth: usize) -> usize {
    let own = step_prefix(depth).len() + step_text(node).len();
    node.nested
        .iter()
        .map(|child| step_width(child, depth + 1))
        .fold(own, usize::max)
}

/// Narrowest step column of a table without nested rows.
const STEP_MIN_WIDTH: usize = 60;
/// Narrowest step column of a table with nested rows.
const STEP_MIN_NESTED_WIDTH: usize = 70;
/// Widest step column: a longer step text is cut with `...`.
const STEP_MAX_WIDTH: usize = 160;

fn format_row(buf: &mut String, node: &Metrics, depth: usize, widths: ColumnWidths) {
    let ColumnWidths {
        step_len,
        call_len,
        in_len,
        out_len,
        time_len,
        mem_len,
        retained_len,
        allocs_len,
        dur_len,
        show_mem,
        show_bulk,
        bulk_len,
    } = widths;
    let bulk_cell = if show_bulk {
        format!(" {:>bulk_len$}", node.counts.element_count)
    } else {
        String::new()
    };
    let prefix = step_prefix(depth);
    let available_len = step_len.saturating_sub(prefix.len());
    let shown = step_text(node);
    let name_part = truncate_str(&shown, available_len);
    let full_step = format!("{prefix}{name_part:<available_len$}");
    let time = TimeFormat(node.duration).to_string();
    let pers = node.percent_duration;
    let (calls, count_in, count_out) = (node.calls, node.count_in, node.counts.traverser_count);
    if show_mem {
        let mem = mem_column(node);
        let retained = retained_column(node);
        let allocs = allocs_column(node);
        buf.push_str(&format!(
            "{full_step:<step_len$} {calls:>call_len$} {count_in:>in_len$} {count_out:>out_len$}{bulk_cell} {time:>time_len$} {mem:>mem_len$} {retained:>retained_len$} {allocs:>allocs_len$} {pers: >dur_len$.2}\n"
        ));
    } else {
        buf.push_str(&format!(
            "{full_step:<step_len$} {calls:>call_len$} {count_in:>in_len$} {count_out:>out_len$}{bulk_cell} {time:>time_len$} {pers: >dur_len$.2}\n"
        ));
    }

    let indent = " ".repeat(if depth == 0 { 0 } else { 6 * depth - 1 });
    if node.calls > 1
        && let Some(timing) = &node.timing
    {
        let avg = node.duration / u32::try_from(node.calls).unwrap_or(u32::MAX);
        buf.push_str(&format!(
            "{indent}[min: {}, avg: {}, max: {}]\n",
            TimeFormat(timing.min_duration),
            TimeFormat(avg),
            TimeFormat(timing.max_duration)
        ));
    }
    if let Some(loops) = &node.loops {
        buf.push_str(&format!(
            "{indent}[loops: {}, max_depth: {}, avg_loops: {:.1}]\n",
            loops.total_loops, loops.max_depth, loops.avg_loops_per_traverser
        ));
    }
    for child in &node.nested {
        format_row(buf, child, depth + 1, widths);
    }
}

impl std::fmt::Display for TraversalMetrics {
    /// The text table `profile()` shows in the CLI and the REPL: one row per step (nested steps
    /// indented under the step holding them), a `Bulk` column when merged traversers carry a
    /// bulk, `Mem`/`Retained`/`Allocs` columns when the run was profiled with
    /// `ProfileType::Memory`, the `TOTAL:` rows, and the optimizer rules that fired.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let has_nested = self.metrics.iter().any(|m| !m.nested.is_empty());
        // The step column is as wide as the longest step text (indentation included), so
        // annotations such as `[index: ..]` or `[path: ..]` show in full, between a floor that
        // keeps short plans compact and a cap beyond which the text is cut with `...`.
        let min_len = if has_nested {
            STEP_MIN_NESTED_WIDTH
        } else {
            STEP_MIN_WIDTH
        };
        let step_len = self
            .metrics
            .iter()
            .map(|m| step_width(m, 0))
            .fold(min_len, usize::max)
            .min(STEP_MAX_WIDTH);
        let has_rows = !self.metrics.is_empty();
        let show_mem = has_rows && self.metrics.iter().any(has_memory);
        let show_bulk = has_rows && self.metrics.iter().any(has_bulk);

        let mut all: Vec<&Metrics> = Vec::new();
        for node in &self.metrics {
            walk(node, &mut |m| all.push(m));
        }
        let total_materialized: u64 = all
            .iter()
            .filter_map(|m| m.memory)
            .map(|m| m.bytes_materialized)
            .sum();
        let total_retained: Option<i64> = all
            .iter()
            .filter_map(|m| m.memory.and_then(|m| m.bytes_retained))
            .reduce(|a, b| a.saturating_add(b));
        let total_source = all
            .iter()
            .find_map(|m| m.memory.map(|m| m.source))
            .unwrap_or(MemorySource::Estimated);

        let mut widths = ColumnWidths {
            step_len,
            call_len: 4,
            in_len: 7,
            out_len: 7,
            time_len: 10usize.max(TimeFormat(self.duration).to_string().len()),
            mem_len: 9usize.max(format_bytes(total_materialized, total_source).len()),
            retained_len: 8,
            allocs_len: 6,
            dur_len: 8,
            show_mem,
            show_bulk,
            bulk_len: 4,
        };
        for node in &all {
            widths.call_len = widths.call_len.max(node.calls.to_string().len());
            widths.in_len = widths.in_len.max(node.count_in.to_string().len());
            widths.out_len = widths
                .out_len
                .max(node.counts.traverser_count.to_string().len());
            widths.time_len = widths
                .time_len
                .max(TimeFormat(node.duration).to_string().len());
            widths.mem_len = widths.mem_len.max(mem_column(node).len());
            widths.retained_len = widths.retained_len.max(retained_column(node).len());
            widths.allocs_len = widths.allocs_len.max(allocs_column(node).len());
            if show_bulk {
                widths.bulk_len = widths
                    .bulk_len
                    .max(node.counts.element_count.to_string().len());
            }
        }
        let ColumnWidths {
            call_len,
            in_len,
            out_len,
            time_len,
            mem_len,
            retained_len,
            allocs_len,
            dur_len,
            bulk_len,
            ..
        } = widths;

        let (bulk_head, bulk_sep) = if show_bulk {
            (
                format!(" {:>bulk_len$}", "Bulk"),
                format!("={:=>bulk_len$}", ""),
            )
        } else {
            (String::new(), String::new())
        };
        let mid_len =
            call_len + 1 + in_len + 1 + out_len + if show_bulk { 1 + bulk_len } else { 0 };

        let mut buf = "Traversal Metrics\n".to_string();
        let separator = if show_mem {
            format!(
                "{:=<step_len$}={:=>call_len$}={:=>in_len$}={:=>out_len$}{bulk_sep}={:=>time_len$}={:=>mem_len$}={:=>retained_len$}={:=>allocs_len$}={:=>dur_len$}\n",
                "", "", "", "", "", "", "", "", ""
            )
        } else {
            format!(
                "{:=<step_len$}={:=>call_len$}={:=>in_len$}={:=>out_len$}{bulk_sep}={:=>time_len$}={:=>dur_len$}\n",
                "", "", "", "", "", ""
            )
        };
        if show_mem {
            buf.push_str(&format!(
                "{:<step_len$} {:>call_len$} {:>in_len$} {:>out_len$}{bulk_head} {:>time_len$} {:>mem_len$} {:>retained_len$} {:>allocs_len$} {:>dur_len$}\n",
                "Step", "Call", "In", "Out", "Time", "Mem", "Retained", "Allocs", "% Dur"
            ));
        } else {
            buf.push_str(&format!(
                "{:<step_len$} {:>call_len$} {:>in_len$} {:>out_len$}{bulk_head} {:>time_len$} {:>dur_len$}\n",
                "Step", "Call", "In", "Out", "Time", "% Dur"
            ));
        }
        buf.push_str(&separator);

        for node in &self.metrics {
            format_row(&mut buf, node, 0, widths);
        }
        let total_pers: f64 = self.metrics.iter().map(|m| m.percent_duration).sum();
        let total_time = TimeFormat(self.duration).to_string();

        if show_mem {
            buf.push_str(&format!(
                "{TOTAL:>step_len$} {:>mid_len$} {total_time:>time_len$} {:>mem_len$} {:>retained_len$} {:>allocs_len$} {total_pers: >dur_len$.2}\n",
                "execute:", "", "", ""
            ));
            buf.push_str(&format!(
                "{TOTAL:>step_len$} {:>mid_len$} {:>time_len$} {:>mem_len$} {:>retained_len$} {:>allocs_len$} {:>dur_len$}\n",
                "memory materialized:",
                "",
                format_bytes(total_materialized, total_source),
                "",
                "",
                ""
            ));
            if let Some(retained) = total_retained {
                buf.push_str(&format!(
                    "{TOTAL:>step_len$} {:>mid_len$} {:>time_len$} {:>mem_len$} {:>retained_len$} {:>allocs_len$} {:>dur_len$}\n",
                    "net retained:",
                    "",
                    "",
                    format_signed_bytes(retained),
                    "",
                    ""
                ));
            }
        } else {
            buf.push_str(&format!(
                "{TOTAL:>step_len$} {:>mid_len$} {total_time:>time_len$} {total_pers: >dur_len$.2}\n",
                "execute:"
            ));
        }
        buf.push_str(&separator);

        // The optimizer rules that fired, omitted entirely (not `(none)`) when none did.
        if !self.optimizer_rules_applied.is_empty() {
            buf.push_str(&format!(
                "Optimizer rules applied: {}\n",
                self.optimizer_rules_applied.join(", ")
            ));
        }
        write!(f, "{buf}")
    }
}
