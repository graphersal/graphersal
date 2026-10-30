//! Collection of the per-step profiling data while a traversal executes (`.profile()`,
//! `execute_with(ExecOptions::profile(..))`), and the memory estimators behind
//! [`MemorySource::Estimated`].
//!
//! Everything here is crate-internal: the executor records into a [`ProfileContext`] (only inside
//! its `is_profile` gates, so an unprofiled run pays nothing), and the run hands the finished tree
//! out as the public, pure-data [`TraversalMetrics`] (`traversal/metrics.rs`).

use std::time::Duration;

use crate::traversal::metrics::{
    LoopMetrics, MemoryMetrics, MemorySource, Metrics, StepCounts, TimingMetrics, TraversalMetrics,
};

/// The memory figures one step execution reports to [`ProfileContext::exit_step`]: what
/// `source.rs`'s `step_memory_metrics` measured (or estimated) for it. Only built when
/// `ProfileType::Memory` was requested; otherwise the step reports `None` and its metrics carry
/// no `memory` section.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct MemoryFigures {
    /// Gross bytes requested from the allocator (or estimated) during the step.
    pub(crate) bytes_materialized: u64,
    /// Whether the figures are an allocator measurement or a structural estimate.
    pub(crate) source: MemorySource,
    /// Net bytes retained by the step; `None` under [`MemorySource::Estimated`].
    pub(crate) bytes_retained: Option<i64>,
    /// Allocator calls made during the step; `None` under [`MemorySource::Estimated`].
    pub(crate) alloc_count: Option<u64>,
}

impl MemoryFigures {
    /// An estimated figure: `bytes_retained`/`alloc_count` stay `None`, since the `size_of`-based
    /// estimation has no allocator-level data to draw them from.
    #[cfg_attr(feature = "alloc-tracking", allow(dead_code))]
    pub(crate) fn estimated(bytes_materialized: u64) -> Self {
        Self {
            bytes_materialized,
            source: MemorySource::Estimated,
            bytes_retained: None,
            alloc_count: None,
        }
    }

    /// A tracked figure with real allocator-level `bytes_retained`/`alloc_count` data.
    #[cfg_attr(not(feature = "alloc-tracking"), allow(dead_code))]
    pub(crate) fn tracked(bytes_materialized: u64, bytes_retained: i64, alloc_count: u64) -> Self {
        Self {
            bytes_materialized,
            source: MemorySource::Tracked,
            bytes_retained: Some(bytes_retained),
            alloc_count: Some(alloc_count),
        }
    }

    /// Adds the figures of another call of the same step (same telescoping sum as the counts).
    fn accumulate(&mut self, other: MemoryFigures) {
        self.bytes_materialized = self
            .bytes_materialized
            .saturating_add(other.bytes_materialized);
        self.source = other.source;
        self.bytes_retained = add_opt_i64(self.bytes_retained, other.bytes_retained);
        self.alloc_count = add_opt_u64(self.alloc_count, other.alloc_count);
    }
}

/// What [`ProfileContext::enter_step`] records about a new step: its plain name and its path
/// recording mode (`None` when it records nothing). Built lazily, only for a step's first call.
pub(crate) trait StepDescription {
    fn describe(self) -> (String, Option<String>);
}

impl StepDescription for &str {
    fn describe(self) -> (String, Option<String>) {
        (self.to_string(), None)
    }
}

impl<F: FnOnce() -> (String, Option<String>)> StepDescription for F {
    fn describe(self) -> (String, Option<String>) {
        self()
    }
}

/// The key of a step within its level of the profile tree: the child traversal of the enclosing
/// step it belongs to (`None` at the top level) and its position in that traversal.
type NodeKey = (Option<usize>, usize);

/// One step of the profile tree while it is being recorded.
#[derive(Debug, Clone, Default)]
pub(crate) struct ProfileNode {
    child: Option<usize>,
    step: usize,
    name: String,
    path_recording: Option<String>,
    duration: Duration,
    min_duration: Duration,
    max_duration: Duration,
    calls: u64,
    count_in: u64,
    count_out: u64,
    bulk_out: u64,
    loops: Option<LoopMetrics>,
    memory: Option<MemoryFigures>,
    /// The steps of every child traversal of this step, ordered by `(child, step)`.
    nested: Vec<ProfileNode>,
}

impl ProfileNode {
    fn key(&self) -> NodeKey {
        (self.child, self.step)
    }

    /// The public metrics of this node, whose id is `id`; `total` is the duration of the whole
    /// run (`percent_duration` is relative to it).
    fn into_metrics(self, id: String, total: Duration) -> Metrics {
        let nested = self
            .nested
            .into_iter()
            .map(|node| {
                let child_id = format!("{id}.{}.{}", node.child.unwrap_or(0), node.step);
                node.into_metrics(child_id, total)
            })
            .collect();
        let percent_duration = if total.as_nanos() > 0 {
            self.duration.as_nanos() as f64 * 100.0 / total.as_nanos() as f64
        } else {
            0.0
        };
        let timing = (self.calls > 0).then_some(TimingMetrics {
            min_duration: self.min_duration,
            max_duration: self.max_duration,
        });
        let memory = self.memory.map(|figures| MemoryMetrics {
            bytes_materialized: figures.bytes_materialized,
            bytes_retained: figures.bytes_retained,
            alloc_count: figures.alloc_count,
            source: figures.source,
        });
        Metrics {
            id,
            name: self.name,
            path_recording: self.path_recording,
            duration: self.duration,
            counts: StepCounts {
                traverser_count: self.count_out,
                element_count: self.bulk_out,
            },
            percent_duration,
            nested,
            calls: self.calls,
            count_in: self.count_in,
            timing,
            loops: self.loops,
            memory,
        }
    }
}

/// The profile tree of one execution, recorded by the executor inside its `is_profile` gates.
///
/// Steps are addressed by their static position in the optimized plan, never by execution
/// order: a step inside a child traversal is keyed by the child's index among the children of
/// the enclosing step (`GraphStep::child_traversals_mut`) and its own position in that child, so
/// a branch that never runs leaves no gap and repeated calls of a step accumulate in one node.
#[derive(Debug, Clone, Default)]
pub(crate) struct ProfileContext {
    pub(crate) nodes: Vec<ProfileNode>,
    /// Names of every optimizer rule that modified the plan for this execution, top-level and
    /// nested, deduplicated, in pipeline order. Set by `execute_steps` right after the optimizer
    /// runs.
    pub(crate) applied_optimizer_rules: Vec<String>,
    /// Indices (into the node list of each level) of the steps currently executing, outermost
    /// first. Left as is when a step fails: the partial tree is still handed out.
    scope_stack: Vec<usize>,
}

impl ProfileContext {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Respells every recorded step name (`render.spelling`); the names are recorded in the
    /// canonical snake_case rendering.
    pub(crate) fn respell(&mut self, spelling: crate::traversal::Spelling) {
        fn walk(nodes: &mut [ProfileNode], spelling: crate::traversal::Spelling) {
            for node in nodes {
                node.name = crate::traversal::spelling::respell(&node.name, spelling).into_owned();
                walk(&mut node.nested, spelling);
            }
        }
        if spelling != crate::traversal::Spelling::Snake {
            walk(&mut self.nodes, spelling);
        }
    }

    /// The node list of the innermost executing step (the top-level list when none executes).
    fn current_nodes_mut(&mut self) -> &mut Vec<ProfileNode> {
        let mut list = &mut self.nodes;
        for &index in &self.scope_stack {
            // The indices always point at existing nodes: they are pushed by `enter_step`.
            if index < list.len() {
                list = &mut list[index].nested;
            }
        }
        list
    }

    /// Starts one call of the step at `step` of child traversal `child` (`None` at the top level)
    /// of the innermost executing step. The node is created on its first call, in plan order.
    pub(crate) fn enter_step(
        &mut self,
        child: Option<usize>,
        step: usize,
        describe: impl StepDescription,
    ) {
        let list = self.current_nodes_mut();
        let key = (child, step);
        let index = match list.binary_search_by_key(&key, ProfileNode::key) {
            Ok(index) => index,
            Err(index) => {
                let (name, path_recording) = describe.describe();
                list.insert(
                    index,
                    ProfileNode {
                        child,
                        step,
                        name,
                        path_recording,
                        ..ProfileNode::default()
                    },
                );
                index
            }
        };
        self.scope_stack.push(index);
    }

    /// Closes the innermost call started by [`Self::enter_step`], adding its timing, counts and
    /// (when memory was requested) memory figures. `bulk_extra` is how many more logical
    /// traversers the output stands for than it has objects (merged traversers carry a bulk).
    pub(crate) fn exit_step(
        &mut self,
        duration: Duration,
        count_in: usize,
        count_out: usize,
        bulk_extra: u64,
        memory: Option<MemoryFigures>,
    ) {
        let Some(index) = self.scope_stack.pop() else {
            return;
        };
        let Some(node) = self.current_nodes_mut().get_mut(index) else {
            return;
        };
        if node.calls == 0 {
            node.min_duration = duration;
            node.max_duration = duration;
        } else {
            node.min_duration = node.min_duration.min(duration);
            node.max_duration = node.max_duration.max(duration);
        }
        node.calls += 1;
        node.duration += duration;
        node.count_in += count_in as u64;
        node.count_out += count_out as u64;
        node.bulk_out = node
            .bulk_out
            .saturating_add(count_out as u64)
            .saturating_add(bulk_extra);
        if let Some(figures) = memory {
            match &mut node.memory {
                Some(total) => total.accumulate(figures),
                None => node.memory = Some(figures),
            }
        }
    }

    /// Merges the loop statistics of one `repeat()` execution into the node of the step being
    /// executed. `outputs` is the number of traversers this execution produced; the average
    /// loop count is weighted by it across calls.
    pub(crate) fn merge_loop_summary(&mut self, summary: LoopMetrics, outputs: usize) {
        let Some((&current, parents)) = self.scope_stack.split_last() else {
            return;
        };
        let mut list = &mut self.nodes;
        for &index in parents {
            match list.get_mut(index) {
                Some(node) => list = &mut node.nested,
                None => return,
            }
        }
        let Some(node) = list.get_mut(current) else {
            return;
        };
        let previous_outputs = node.count_out;
        match &mut node.loops {
            None => node.loops = Some(summary),
            Some(merged) => {
                let total = previous_outputs + outputs as u64;
                if total > 0 {
                    merged.avg_loops_per_traverser = (merged.avg_loops_per_traverser
                        * previous_outputs as f64
                        + summary.avg_loops_per_traverser * outputs as f64)
                        / total as f64;
                }
                merged.total_loops += summary.total_loops;
                merged.max_depth = merged.max_depth.max(summary.max_depth);
            }
        }
    }

    /// The finished (or, after a failure, partial) tree as public metrics. `duration` is the
    /// duration of the whole run.
    pub(crate) fn into_metrics(self, duration: Duration) -> TraversalMetrics {
        let metrics = self
            .nodes
            .into_iter()
            .map(|node| {
                let id = node.step.to_string();
                node.into_metrics(id, duration)
            })
            .collect();
        TraversalMetrics {
            duration,
            metrics,
            optimizer_rules_applied: self.applied_optimizer_rules,
            memory: None,
        }
    }
}

/// Adds two `Option<i64>` values, treating `None` as "no data" rather than `0`: `None` only when
/// both operands are `None`.
fn add_opt_i64(a: Option<i64>, b: Option<i64>) -> Option<i64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.saturating_add(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

/// Adds two `Option<u64>` values, treating `None` as "no data" rather than `0`: `None` only when
/// both operands are `None`.
fn add_opt_u64(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.saturating_add(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

/// Estimated heap footprint of one traverser's value (D3 of the memory-inspection spec): `0` for
/// every lazy handle variant (`Vertex`/`Edge`/`VertexProperty`/`EdgeProperty`'s key is the only
/// possibly-heap-allocated part), and a recursive `size_of`/capacity walk for every variant that
/// owns indirect data. Reuses [`crate::ElementProperty::estimated_heap_bytes`] for `Computed`, the
/// same estimator [`crate::graph::MemoryStats::property_bytes`] uses — one estimator, two callers.
pub(crate) fn estimate_traverser_value_bytes<S: crate::graph::GraphStorage>(
    value: &crate::value::TraverserValue<S>,
) -> u64 {
    use crate::value::TraverserValue;
    match value {
        TraverserValue::Computed(ep, _) => ep.estimated_heap_bytes(),
        TraverserValue::VertexProperty(key, _, _) | TraverserValue::EdgeProperty(key, _, _) => {
            if key.is_heap_allocated() {
                key.capacity() as u64
            } else {
                0
            }
        }
        TraverserValue::Vertex(_) | TraverserValue::Edge(_) => 0,
        // A path value owns its entries (copies of the values the path arena holds once per
        // execution): the `Vec` of entries plus every entry's own value, so a path of paths
        // (`repeat(__.path())`, which doubles in size on every iteration) is measured whole. The
        // arena itself is shared per execution and not counted here.
        TraverserValue::Path(path) => {
            path.steps.capacity() as u64
                * std::mem::size_of::<crate::value::path::TraverserPathStep<S>>() as u64
                + path
                    .steps
                    .iter()
                    .map(|step| estimate_traverser_value_bytes(&step.value))
                    .sum::<u64>()
        }
        TraverserValue::Record { values, .. } => {
            let container =
                values.capacity() as u64 * std::mem::size_of::<TraverserValue<S>>() as u64;
            container
                + values
                    .iter()
                    .map(estimate_traverser_value_bytes)
                    .sum::<u64>()
        }
        TraverserValue::Object(map) => {
            let entry = std::mem::size_of::<(TraverserValue<S>, TraverserValue<S>)>() as u64;
            let mut total = map.0.capacity() as u64 * entry;
            for (k, v) in map.iter() {
                total += estimate_traverser_value_bytes(k) + estimate_traverser_value_bytes(v);
            }
            total
        }
        TraverserValue::PathProperty(prop) => {
            std::mem::size_of::<(compact_str::CompactString, crate::value::ElementProperty)>()
                as u64
                + if prop.0.is_heap_allocated() {
                    prop.0.capacity() as u64
                } else {
                    0
                }
                + prop.1.estimated_heap_bytes()
        }
        TraverserValue::Entry(entry) => {
            std::mem::size_of::<(TraverserValue<S>, TraverserValue<S>)>() as u64
                + estimate_traverser_value_bytes(&entry.0)
                + estimate_traverser_value_bytes(&entry.1)
        }
        TraverserValue::Array(values) => {
            let container =
                values.capacity() as u64 * std::mem::size_of::<TraverserValue<S>>() as u64;
            container
                + values
                    .iter()
                    .map(estimate_traverser_value_bytes)
                    .sum::<u64>()
        }
        TraverserValue::Schema(_) => std::mem::size_of::<crate::graph::GraphSchema>() as u64,
        // A graph value is a shared handle; its content is not counted as this step's output.
        TraverserValue::Graph(_) => {
            std::mem::size_of::<std::sync::Arc<crate::graph::Graph>>() as u64
        }
    }
}

/// Estimated heap footprint of a step's full traverser output (D3): each item's own value bytes,
/// plus the output buffer's contribution at `size_of::<Traverser<S>>()` per item. Used by
/// `.profile()`'s per-step `bytes_materialized` figure whenever the `alloc-tracking` feature is
/// not compiled in (or a step's own tracked delta is unavailable).
#[cfg_attr(feature = "alloc-tracking", allow(dead_code))]
pub(crate) fn estimate_traversers_bytes<S: crate::graph::GraphStorage>(
    items: &[crate::traversal::traverser::Traverser<S>],
) -> u64 {
    let container = items.len() as u64
        * std::mem::size_of::<crate::traversal::traverser::Traverser<S>>() as u64;
    container
        + items
            .iter()
            .map(|t| estimate_traverser_value_bytes(&t.value))
            .sum::<u64>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traversal::step::GraphStep;

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn test_step_send_sync() {
        assert_send_sync::<GraphStep<'static, crate::graph::TraversalGraph>>();
        assert_send_sync::<ProfileContext>();
        assert_send_sync::<TraversalMetrics>();
        assert_send_sync::<Metrics>();
    }

    #[test]
    fn scoping_aggregation_and_ids() {
        let mut ctx = ProfileContext::new();
        ctx.enter_step(None, 0, "v()");
        ctx.exit_step(Duration::from_millis(5), 0, 10, 0, None);

        ctx.enter_step(None, 1, "union()");
        // Child 1 runs before child 0: the ids still follow the plan.
        ctx.enter_step(Some(1), 0, "in()");
        ctx.exit_step(Duration::from_micros(5), 1, 1, 0, None);
        ctx.enter_step(Some(0), 0, "out()");
        ctx.exit_step(Duration::from_micros(10), 1, 2, 0, None);
        ctx.enter_step(Some(0), 0, "out()");
        ctx.exit_step(Duration::from_micros(20), 1, 3, 2, None);
        ctx.exit_step(Duration::from_millis(15), 10, 5, 0, None);

        let metrics = ctx.into_metrics(Duration::from_millis(20));
        let top = metrics.metrics();
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].id(), "0");
        assert_eq!(top[1].id(), "1");
        assert_eq!(top[1].name(), "union()");
        let nested = top[1].nested();
        assert_eq!(nested.len(), 2);
        assert_eq!(nested[0].id(), "1.0.0");
        assert_eq!(nested[0].name(), "out()");
        assert_eq!(nested[0].calls(), 2);
        assert_eq!(nested[0].count_in(), 2);
        assert_eq!(nested[0].counts().traverser_count(), 5);
        assert_eq!(nested[0].counts().element_count(), 7);
        assert_eq!(nested[0].duration(), Duration::from_micros(30));
        let timing = nested[0].timing().unwrap();
        assert_eq!(timing.min_duration(), Duration::from_micros(10));
        assert_eq!(timing.max_duration(), Duration::from_micros(20));
        assert_eq!(nested[1].id(), "1.1.0");
        assert!((top[1].percent_duration() - 75.0).abs() < 1e-9);
        assert!(metrics.find("1.1.0").is_some());
        assert!(metrics.find("1.2.0").is_none());
    }

    #[test]
    fn a_failed_step_stays_in_the_partial_tree() {
        let mut ctx = ProfileContext::new();
        ctx.enter_step(None, 0, "v()");
        ctx.exit_step(Duration::from_millis(1), 0, 2, 0, None);
        ctx.enter_step(None, 1, "map()");
        // No exit: the step failed.
        let metrics = ctx.into_metrics(Duration::from_millis(1));
        assert_eq!(metrics.metrics().len(), 2);
        assert_eq!(metrics.metrics()[1].calls(), 0);
        assert!(metrics.metrics()[1].timing().is_none());
    }

    #[test]
    fn loop_summary_and_loop_table_line() {
        let mut ctx = ProfileContext::new();
        ctx.enter_step(None, 0, "repeat()");
        ctx.merge_loop_summary(
            LoopMetrics {
                total_loops: 5,
                max_depth: 3,
                avg_loops_per_traverser: 2.1,
            },
            20,
        );
        ctx.exit_step(Duration::from_millis(50), 10, 20, 0, None);
        let metrics = ctx.into_metrics(Duration::from_millis(50));
        assert_eq!(metrics.metrics()[0].loops().unwrap().total_loops(), 5);
        let display = metrics.to_string();
        assert!(display.contains("[loops: 5, max_depth: 3, avg_loops: 2.1]"));
    }

    /// Every row of a nested table (header, separators, step rows, timing lines aside, TOTAL
    /// row) has the same width, also with counts far wider than the headers.
    #[test]
    fn large_count_table_alignment() {
        let mut ctx = ProfileContext::new();
        ctx.enter_step(None, 0, "v()");
        ctx.exit_step(Duration::from_micros(3809), 0, 111_110, 0, None);
        ctx.enter_step(
            None,
            1,
            "where_t(__::out(None).where_t(__::has_label(\"person\")))",
        );
        for _ in 0..3 {
            ctx.enter_step(Some(0), 0, "out(None)");
            ctx.exit_step(Duration::from_nanos(172), 1, 1, 0, None);
            ctx.enter_step(Some(0), 1, "where_t(__::has_label(\"person\"))");
            ctx.enter_step(Some(0), 0, "has_label(\"person\")");
            ctx.exit_step(Duration::from_nanos(184), 1, 0, 0, None);
            ctx.exit_step(Duration::from_nanos(1061), 1, 0, 0, None);
        }
        ctx.exit_step(Duration::from_micros(276_678), 111_110, 0, 0, None);
        ctx.enter_step(None, 2, "project(\"name\").by(\"name\")");
        ctx.exit_step(Duration::from_nanos(375), 0, 0, 0, None);
        let display = ctx.into_metrics(Duration::from_micros(280_595)).to_string();
        let lines: Vec<&str> = display.lines().collect();
        let expected = lines[1].chars().count();
        for line in &lines[1..] {
            if line.trim_start().starts_with('[') {
                continue;
            }
            assert_eq!(line.chars().count(), expected, "{line:?} in\n{display}");
        }
        assert!(display.contains(r"  \> out(None)"), "{display}");
        assert!(display.contains(r"        \> has_label"), "{display}");
    }
}
