use std::collections::HashMap;

use crate::graph::GraphStorage;
use crate::*;

pub(crate) mod bulk;
pub mod cancel;
pub(crate) mod capacity;
pub(crate) mod context;
/// The `execute()` terminal and its [`Execution`] result.
pub mod execution;
pub(crate) mod memory_budget;
/// Structured profiling results ([`TraversalMetrics`]).
pub mod metrics;
pub(crate) mod optimizer;
pub(crate) mod permission_check;
/// Rendering a traversal result for display (tables, JSON, tree).
pub mod render;
pub(crate) mod side_effect;
/// [`GraphSource`] and [`GraphTraversalSource`], the traversal entry points.
pub mod source;
/// [`Spelling`], the snake_case / camelCase spelling of rendered step names.
pub mod spelling;
/// Step implementations, one variant per pipeline category.
pub mod step;
/// Result-set-to-table conversion, used by `render`.
pub mod table;
pub(crate) mod traverser;
pub(crate) mod tree_render;

#[cfg(test)]
mod bulk_tests;
#[cfg(all(test, feature = "script"))]
pub(crate) mod test_utils;
#[cfg(test)]
pub(crate) mod unwind_probe;

pub use cancel::CancelToken;
pub use context::*;
pub use execution::{ExecOptions, Execution};
pub use metrics::{
    LoopMetrics, MemoryMetrics, MemorySource, Metrics, StepCounts, TimingMetrics, TraversalMetrics,
};
pub use render::{RenderOptions, RenderedOutput, Truncation};
pub use spelling::Spelling;
pub use step::flatmap_steps::step_glob_path::GlobPathOptions;
pub use step::sideeffect_steps::SideEffectFn;
pub use step::*;
pub(crate) use traverser::Traverser;

/// The name of every registered optimizer rule, in pipeline order: the names
/// `g.with("optimizer.disabled", [...])` accepts. Exposed for the crate's own differential tests,
/// which check every rule against the unoptimized plan; not part of the supported API.
#[doc(hidden)]
pub fn optimizer_rule_names() -> Vec<&'static str> {
    optimizer::TraversalOptimizer::<crate::TraversalGraph>::registry()
        .into_iter()
        .map(|(name, _, _)| name)
        .collect()
}

/// Factory for constructing shared `Graph` instances.
#[derive(Debug, Clone)]
pub struct GraphSource {}

/// Primary entry point for constructing and executing Gremlin-style graph traversals.
///
/// # Example
/// ```rust
/// use graphersal::prelude::*;
///
/// let graph = TraversalGraph::new();
/// let g = graph.traversal();
/// let count = g.v(None::<()>).count().next().unwrap();
/// assert!(count.is_some());
/// ```
#[derive(Clone)]
pub struct GraphTraversalSource<'g, S: GraphStorage + 'g> {
    pub(crate) storage: S,
    pub(crate) strategy: TraversalStrategy,
    pub(crate) steps: Vec<GraphStep<'g, S>>,
    pub(crate) side_effect: HashMap<CompactString, side_effect::SideEffectValue<S>>,
    pub(crate) options: HashMap<CompactString, ElementProperty>,
    /// Set by `.profile_with(flags)` (`ProfileType::none()` otherwise, including for every
    /// non-`.profile()` terminal); read once when the execution context is built.
    pub(crate) profile_type: ProfileType,
    /// Set by `.with_sack(initial)`: every generated traverser starts with this sack.
    pub(crate) sack: Option<crate::value::SackSpec>,
    /// Set by [`Self::with_authorizer`] (and the script DSL): the authorization check `run()`
    /// performs before anything executes. `None` (the Rust API's default): everything allowed,
    /// unchecked.
    pub(crate) restrictions: Option<std::sync::Arc<permission_check::Restrictions>>,
    /// Set by [`Self::with_now`] (and the script DSL's creating steps carry the evaluation's
    /// `now()`): the time of `defaultFunction` `now`/`today`. `None`: the unit's start.
    pub(crate) create_time: Option<crate::value::temporal::DateTime>,
    /// Set by [`Self::with_embedders`] (and the script DSL): the embedders the `embed` step
    /// calls. `None`: the default registry (`hash-ngram/1` only).
    pub(crate) embedders: Option<std::sync::Arc<crate::value::Embedders>>,
}

/// Anonymous-traversal token (`__`): starts a child traversal inside a step argument (e.g.
/// `.by(__.values("name"))`).
pub struct __;

/// A child traversal built from `__` or `g.v()`/`g.e()`, used as a step argument (e.g. inside
/// `.by(...)`, `.where_(...)`, `.repeat(...)`).
pub struct AnonymousTraversal<'g, S: GraphStorage + 'g> {
    pub(crate) steps: Vec<GraphStep<'g, S>>,
    /// This level as written by the user, kept when optimization rewrote it.
    pub(crate) level: Option<std::sync::Arc<crate::traversal::optimizer::LevelPlan<'g, S>>>,
    /// Whether the optimizer has already run on these steps.
    pub(crate) optimized: bool,
    /// Path mode of each physical step, set by the path requirement analysis of the enclosing
    /// traversal. Empty until analysed, in which case every step records its full path.
    pub(crate) path_modes: Vec<crate::traversal::optimizer::path_analysis::PathMode>,
    /// What this level and everything nested in it does (mutates, writes side effects), set by
    /// the path analysis together with `path_modes`.
    pub(crate) effects: crate::traversal::optimizer::path_analysis::PlanEffects,
    /// Position of this traversal among the children of the step that holds it (the `<child>`
    /// segment of a step id, see `GraphStep::child_traversals_mut`). Set on the optimized plan by
    /// `GraphStep::number_children` before execution; `0` until then.
    pub(crate) child_index: usize,
}

impl<'g, S: GraphStorage + 'g> Clone for AnonymousTraversal<'g, S> {
    fn clone(&self) -> Self {
        Self {
            steps: self.steps.clone(),
            level: self.level.clone(),
            optimized: self.optimized,
            path_modes: self.path_modes.clone(),
            effects: self.effects,
            child_index: self.child_index,
        }
    }
}

impl<'g, S: GraphStorage + 'g> Default for AnonymousTraversal<'g, S> {
    fn default() -> Self {
        Self {
            steps: Vec::new(),
            level: None,
            optimized: false,
            path_modes: Vec::new(),
            effects: Default::default(),
            child_index: 0,
        }
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// A new, empty anonymous traversal.
    pub fn new() -> Self {
        Self {
            steps: Vec::new(),
            level: None,
            optimized: false,
            path_modes: Vec::new(),
            effects: Default::default(),
            child_index: 0,
        }
    }

    pub(crate) fn push_step(mut self, step: GraphStep<'g, S>) -> Self {
        push_step_into(&mut self.steps, step, "__");
        self
    }

    /// Pins the time of `defaultFunction` `now`/`today` on the creating step just pushed
    /// (`add_v`, `add_e`, `merge_v`, `merge_e`): the script DSL's evaluation `now()`. `None`
    /// changes nothing.
    #[cfg(feature = "script")]
    pub(crate) fn stamp_create_time(
        mut self,
        now: Option<crate::value::temporal::DateTime>,
    ) -> Self {
        if let (Some(now), Some(GraphStep::SideEffectMut(_, step))) = (now, self.steps.last_mut()) {
            step.set_create_time(now);
        }
        self
    }

    /// The number of steps in this traversal's pipeline.
    pub fn len(&self) -> usize {
        self.steps.len()
    }

    /// Whether the pipeline holds global state at its top level: a barrier (`count`, `sum`,
    /// `fold`, `group`, ...) or a stream-wide flat barrier (`order`, `dedup`, `limit`, `range`,
    /// `tail`, ...). Such a child must see every traverser routed to it as one stream
    /// (`union()`/`choose()`/`branch()` batch mode). `repeat()` (a streaming loop, unless its body holds a `dedup()`, whose seen-set lives for the whole loop), `barrier()`
    /// (a pure merge point) and `discard()` do not count. Call it after optimization, so fused
    /// steps are judged as they run.
    pub(crate) fn has_barrier(&self) -> bool {
        self.steps.iter().any(|step| match step {
            GraphStep::Barrier(..) | GraphStep::Count(..) => true,
            GraphStep::FlatBarrier(name, step) => {
                !matches!(*name, "repeat" | "barrier" | "discard") || step.holds_loop_state()
            }
            GraphStep::Init(_, init) => init.is_count_only(),
            _ => false,
        })
    }

    /// True if this traversal's pipeline has no steps.
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    pub(crate) fn optimize(
        &mut self,
        optimizer: &crate::traversal::optimizer::TraversalOptimizer<'g, S>,
    ) {
        if self.optimized {
            // The rules have already run on these steps, and the logical snapshot of the level is
            // kept. Optimizing again would record the physical plan as if the user had written it.
            return;
        }
        self.optimized = true;
        self.level = optimizer.apply(&mut self.steps).map(std::sync::Arc::new);
    }

    /// Runs the path requirement analysis on this level and stores its modes.
    ///
    /// `downstream` is what the enclosing traversal reads after this level's output when that
    /// output continues the enclosing path (`union()`), and `None` otherwise. Returns the
    /// requirement at the entry of this level, which the step holding it reports upward.
    pub(crate) fn analyze_paths(
        &mut self,
        analysis: &crate::traversal::optimizer::path_analysis::PathAnalysis,
        downstream: crate::traversal::optimizer::path_analysis::PathRequirement,
    ) -> crate::traversal::optimizer::path_analysis::PathRequirement {
        let saved = analysis.take_effects();
        let (modes, entry) = analysis.analyze_level(&mut self.steps, downstream);
        self.path_modes = modes;
        self.effects = analysis.take_effects();
        analysis.restore_effects(saved, self.effects);
        entry
    }

    /// Runs this traversal on `source` as one batch (the bulks of the input are kept).
    pub(crate) fn to_list(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Vec<Traverser<S>>,
    ) -> TraverserResult<Vec<Traverser<S>>> {
        GraphTraversalSource::<'g, S>::execute_optimized_steps(
            &self.steps,
            source,
            context,
            crate::traversal::source::LevelAt::child(self.child_index),
            self.level.as_deref(),
            &self.path_modes,
        )
    }

    /// Runs this child for one parent traverser, like TinkerPop's `TraversalUtil.apply`: the
    /// child sees the parent with bulk 1 (unit in), and every output's bulk is multiplied by
    /// the parent's bulk (multiply out), so the result is the same multiset whether or not
    /// equal parents were merged.
    ///
    /// Use it for every per-traverser child call whose outputs flow on. Batch calls
    /// ([`Self::to_list`] with many traversers: `repeat()` bodies, `group()`'s
    /// barrier value traversal) keep the bulks of their input instead.
    ///
    /// When the child writes side effects (`effects.writes_side_effects`) and the parent has
    /// bulk above 1, the child instead runs `bulk` times with a bulk-1 parent and the outputs
    /// are concatenated (each with its own bulk). This is exact unmerged semantics; TinkerPop
    /// runs such a child once per merged traverser, we deliberately do not. `first` and
    /// `exists` do the same (see their docs).
    pub(crate) fn apply_to(
        &self,
        context: &mut TraverserContext<'_, S>,
        parent: &Traverser<S>,
    ) -> TraverserResult<Vec<Traverser<S>>> {
        // A per-traverser child never shares the loop's persistent state (`dedup()` seen-sets).
        context.with_persist(false, |context| self.apply_to_inner(context, parent))
    }

    fn apply_to_inner(
        &self,
        context: &mut TraverserContext<'_, S>,
        parent: &Traverser<S>,
    ) -> TraverserResult<Vec<Traverser<S>>> {
        if parent.bulk > 1 && self.effects.writes_side_effects {
            // Exact unmerged semantics: every unit of the parent runs the child once, so the
            // side effects happen `bulk` times and the outputs (bulk 1 each) concatenate.
            let mut all = Vec::new();
            for _ in 0..parent.bulk {
                context.tick()?;
                let unit = Traverser::with_parts(parent.value.clone(), parent.path, 1, parent.sack);
                all.extend(self.to_list(context, vec![unit])?);
            }
            return Ok(all);
        }
        let unit = Traverser::with_parts(parent.value.clone(), parent.path, 1, parent.sack);
        let mut produced = self.to_list(context, vec![unit])?;
        if parent.bulk != 1 {
            for item in &mut produced {
                item.bulk =
                    crate::traversal::bulk::mul_bulk(item.bulk, parent.bulk, "child traversal")?;
            }
        }
        Ok(produced)
    }

    /// The first result of this traversal applied to `source`, or `None` when it produces
    /// nothing: TinkerPop's `TraversalUtil.produce()`, the rule every `by(__...)` modulator
    /// follows. Stops at the first result unless the traversal holds a barrier. Errors of the
    /// child propagate.
    ///
    /// A child that writes side effects runs once per unit of a bulk-n parent (all runs happen,
    /// the first run's result is returned), like [`Self::apply_to`].
    pub(crate) fn first(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: &Traverser<S>,
    ) -> TraverserResult<Option<Traverser<S>>> {
        context.with_persist(false, |context| self.first_inner(context, source))
    }

    fn first_inner(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: &Traverser<S>,
    ) -> TraverserResult<Option<Traverser<S>>> {
        if source.bulk > 1 && self.effects.writes_side_effects {
            let mut result = None;
            for run in 0..source.bulk {
                context.tick()?;
                let unit = Traverser::with_parts(source.value.clone(), source.path, 1, source.sack);
                let found = self.first(context, &unit)?;
                if run == 0 {
                    result = found;
                }
            }
            return Ok(result);
        }
        // The child sees one unit of the parent, whatever its bulk.
        let unit;
        let source = if source.bulk == 1 {
            source
        } else {
            unit = Traverser::with_parts(source.value.clone(), source.path, 1, source.sack);
            &unit
        };
        GraphTraversalSource::<'g, S>::execute_optimized_steps_first(
            &self.steps,
            source,
            context,
            self.child_index,
            self.level.as_deref(),
            &self.path_modes,
        )
    }

    /// Whether this traversal produces anything for `source` (short-circuiting).
    pub(crate) fn exists(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: &Traverser<S>,
    ) -> TraverserResult<bool> {
        context.with_persist(false, |context| self.exists_inner(context, source))
    }

    fn exists_inner(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: &Traverser<S>,
    ) -> TraverserResult<bool> {
        if source.bulk > 1 && self.effects.writes_side_effects {
            // Every run must happen (its side effects are part of the unmerged semantics) even
            // though the boolean is known after the first one.
            let mut found = false;
            for _ in 0..source.bulk {
                context.tick()?;
                let unit = Traverser::with_parts(source.value.clone(), source.path, 1, source.sack);
                found |= self.exists(context, &unit)?;
            }
            return Ok(found);
        }
        // The child sees one unit of the parent, whatever its bulk.
        let unit;
        let source = if source.bulk == 1 {
            source
        } else {
            unit = Traverser::with_parts(source.value.clone(), source.path, 1, source.sack);
            &unit
        };
        GraphTraversalSource::<'g, S>::execute_optimized_steps_exists(
            &self.steps,
            source,
            context,
            self.child_index,
            self.level.as_deref(),
            &self.path_modes,
        )
    }
}

impl<'g, S: GraphStorage + 'g> From<GraphTraversalSource<'g, S>> for AnonymousTraversal<'g, S> {
    fn from(gts: GraphTraversalSource<'g, S>) -> Self {
        Self {
            steps: gts.steps,
            level: None,
            optimized: false,
            path_modes: Vec::new(),
            effects: Default::default(),
            child_index: 0,
        }
    }
}

impl<'g, S: GraphStorage + 'g> From<Vec<GraphStep<'g, S>>> for AnonymousTraversal<'g, S> {
    fn from(steps: Vec<GraphStep<'g, S>>) -> Self {
        Self {
            steps,
            level: None,
            optimized: false,
            path_modes: Vec::new(),
            effects: Default::default(),
            child_index: 0,
        }
    }
}

impl<'g, S: GraphStorage + 'g> std::fmt::Debug for AnonymousTraversal<'g, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnonymousTraversal")
            .field("steps", &self.steps)
            .finish()
    }
}

/// The canonical rendering of a child traversal: `__.out().has("x")`, modulators left out like
/// at the top level (they are part of the step they modulate); an empty child is
/// `__.identity()`.
impl<'g, S: GraphStorage + 'g> std::fmt::Display for AnonymousTraversal<'g, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("__")?;
        let mut any = false;
        for step in &self.steps {
            if matches!(step, GraphStep::Modulator(_, _)) {
                continue;
            }
            any = true;
            write!(f, ".{}", step.get_name())?;
        }
        if !any {
            f.write_str(".identity()")?;
        }
        Ok(())
    }
}

/// The argument of a `by()` modulator: what to read from each traverser.
#[non_exhaustive]
pub enum By<'g, S: GraphStorage + 'g> {
    /// Reads a named property.
    Property(CompactString),
    /// Runs a child traversal and reads its result.
    Traversal(AnonymousTraversal<'g, S>),
    /// Sorts by a fixed ordering direction, no projection.
    Order(Order),
    /// Reads the element's label.
    Label,
    /// Reads the element's ID.
    Id,
    /// Reads the value of a property element (TinkerPop `T.value`): the stored value of a
    /// `properties()` value, the leaf of a `properties(jpath(..))` value, or the value of a map
    /// entry. Any other value (a vertex, a number, a `values()` result) is a cast error, as in
    /// TinkerPop.
    Value,
    /// The incoming value itself, unchanged: what an absent `by()` projects (the default key and
    /// value of `group()`).
    Identity,
    /// Reads a property's key (TinkerPop `T.key`): the key of a `properties()` value or of a
    /// map entry.
    Key,
    /// Reads a group's/bucket's size.
    Count,
    /// Reads a value at a JSON Path.
    JsonPath(crate::value::JsonPath),
    /// A list of vertex labels, such as `by(["directory", "file"])`: not a projection, but a
    /// filter for steps that walk vertices and accept it at a specific `by()` position. Every
    /// other step rejects it with [`crate::error::TraverserError::InvalidModulator`] when it
    /// executes. An empty list is accepted here; the consuming step validates it.
    Labels(Vec<CompactString>),
    /// `by(Column::Keys)` / `by(Column::Values)`: the key or value of a map entry, or the list of
    /// keys/values of a whole map (the same projection as `select(Column)`). Only `order()` (both
    /// scopes) takes it as a sort key; every other step rejects it with
    /// [`crate::error::TraverserError::InvalidModulator`] when it executes.
    Column(crate::value::Column),
}

impl<'g, S: GraphStorage + 'g> std::fmt::Debug for By<'g, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            By::Property(prop) => f.debug_tuple("Property").field(prop).finish(),
            By::Traversal(t) => f.debug_tuple("Traversal").field(t).finish(),
            By::Order(o) => f.debug_tuple("Order").field(o).finish(),
            By::Label => write!(f, "Label"),
            By::Id => write!(f, "Id"),
            By::Value => write!(f, "Value"),
            By::Identity => write!(f, "Identity"),
            By::Key => write!(f, "Key"),
            By::Count => write!(f, "Count"),
            By::JsonPath(path) => write!(f, "jpath(\"{path}\")"),
            By::Labels(labels) => f.debug_tuple("Labels").field(labels).finish(),
            By::Column(column) => write!(f, "Column.{column}"),
        }
    }
}

/// The canonical rendering of a `by()` argument, as the DSL writes it: `"age"`, `__.label()`,
/// `Order.desc`, `T.label`, `T.id`, `T.value`, `T.key`, `By.Count`, `jpath("a.b")`,
/// `["dir", "file"]`, `Column.keys`.
impl<'g, S: GraphStorage + 'g> std::fmt::Display for By<'g, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use crate::value::literal::{rhai_string, rhai_string_array};
        match self {
            By::Property(prop) => f.write_str(&rhai_string(prop)),
            By::Traversal(t) => write!(f, "{t}"),
            By::Order(o) => write!(f, "{o}"),
            By::Label => f.write_str("T.label"),
            By::Id => f.write_str("T.id"),
            By::Value => f.write_str("T.value"),
            By::Identity => f.write_str("__.identity()"),
            By::Key => f.write_str("T.key"),
            By::Count => f.write_str("By.Count"),
            By::JsonPath(path) => write!(f, "jpath({})", rhai_string(&path.to_string())),
            By::Labels(labels) => f.write_str(&rhai_string_array(labels)),
            By::Column(column) => write!(f, "Column.{column}"),
        }
    }
}

/// Comma-separated child traversals, as the arguments of `union()`/`coalesce()`:
/// `__.out(), __.in()`.
pub(crate) fn render_traversals<'g, S: GraphStorage + 'g>(
    traversals: &[AnonymousTraversal<'g, S>],
) -> String {
    traversals
        .iter()
        .map(|traversal| traversal.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The DSL rendering of a `by()` modulator: `.by("age")`, `.by(__.label())`.
pub(crate) fn render_by<'g, S: GraphStorage + 'g>(by: &By<'g, S>) -> String {
    format!(".by({by})")
}

impl<'g, S: GraphStorage + 'g> Clone for By<'g, S> {
    fn clone(&self) -> Self {
        match self {
            By::Property(prop) => By::Property(prop.clone()),
            By::Traversal(t) => By::Traversal(t.clone()),
            By::Order(o) => By::Order(o.clone()),
            By::Label => By::Label,
            By::Id => By::Id,
            By::Value => By::Value,
            By::Identity => By::Identity,
            By::Key => By::Key,
            By::Count => By::Count,
            By::JsonPath(path) => By::JsonPath(path.clone()),
            By::Labels(labels) => By::Labels(labels.clone()),
            By::Column(column) => By::Column(*column),
        }
    }
}

impl<'g, S: GraphStorage + 'g> By<'g, S> {
    /// The child traversal of a `by(__...)` modulator, if it is one.
    pub(crate) fn traversal_mut(&mut self) -> Option<&mut AnonymousTraversal<'g, S>> {
        match self {
            By::Traversal(traversal) => Some(traversal),
            _ => None,
        }
    }

    /// Analyses a `by(__...)` child traversal, which is evaluated on the traverser it modulates
    /// and does not continue its path. Returns what the child reads from that traverser's path.
    pub(crate) fn analyze_paths(
        &mut self,
        analysis: &crate::traversal::optimizer::path_analysis::PathAnalysis,
    ) -> crate::traversal::optimizer::path_analysis::PathRequirement {
        match self {
            By::Traversal(traversal) => traversal.analyze_paths(
                analysis,
                crate::traversal::optimizer::path_analysis::PathRequirement::None,
            ),
            _ => crate::traversal::optimizer::path_analysis::PathRequirement::None,
        }
    }

    /// Analyses a list of `by()` modulators; see [`By::analyze_paths`].
    pub(crate) fn analyze_all_paths(
        list: &mut [By<'g, S>],
        analysis: &crate::traversal::optimizer::path_analysis::PathAnalysis,
    ) -> crate::traversal::optimizer::path_analysis::PathRequirement {
        list.iter_mut().fold(
            crate::traversal::optimizer::path_analysis::PathRequirement::None,
            |need, by| need.union(by.analyze_paths(analysis)),
        )
    }
}

/// Whether a step took a `by()` modulator; returned by every `push_modulator_by`.
///
/// The trait default is [`ByModulation::Unsupported`], so only a step that overrides
/// `push_modulator_by` and answers [`ByModulation::Accepted`] takes `by()` (TinkerPop's
/// `ByModulating`). A step that forgets the override rejects `by()` instead of dropping it.
#[must_use]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ByModulation {
    /// The step took the `by()`; it validates the argument itself (see [`ByModulatorCheck`]).
    Accepted,
    /// The step does not take `by()` modulators at all.
    Unsupported,
}

/// Whether a step took a `from()`/`to()` modulator; returned by every `push_modulator_args`.
///
/// The trait default is [`ArgsModulation::Unsupported`], so only a step that overrides
/// `push_modulator_args` and answers [`ArgsModulation::Accepted`] takes `from()`/`to()`
/// (TinkerPop's `FromToModulating`: `add_e()`, `path()`, `simple_path()` and `cyclic_path()`).
/// A step that forgets the override rejects `from()`/`to()` instead of silently dropping it.
/// A `from(traversal)`/`to(traversal)` goes through `push_modulator_traversal` instead and is
/// answered the same way; only `add_e()` takes it. `from()`/`to()` select their step with
/// [`from_to_target`]: the `add_e()` they follow, `property()` calls in between included.
#[must_use]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ArgsModulation {
    /// The step took the `from()`/`to()` argument.
    Accepted,
    /// The step does not take `from()`/`to()` modulators at all.
    Unsupported,
}

/// Appends `step` to `steps`, the one place every traversal builder attaches modulators.
///
/// A modulator (`by()`, `from()`, `to()`) is handed to the step it modulates: the last step that
/// is neither a modulator nor an `as()` label, since `as()` labels the step before it, like
/// TinkerPop. When that step does not take the modulator ([`ByModulation::Unsupported`] /
/// [`ArgsModulation::Unsupported`]), or no step precedes it at all, the modulator is replaced by a
/// [`StepModulatorRejected`] marker that fails with
/// [`crate::error::TraverserError::InvalidModulator`] when the traversal executes (builders
/// cannot fail; the plan-time validation pass in `optimizer::path_analysis` also finds this marker
/// before execution, whether or not the level holding it ever runs). `source` names the empty
/// traversal (`g` or `__`) for the no-step case.
pub(crate) fn push_step_into<'g, S: GraphStorage + 'g>(
    steps: &mut Vec<GraphStep<'g, S>>,
    mut step: GraphStep<'g, S>,
    source: &'static str,
) {
    if let GraphStep::Modulator(modulator_name, mod_step) = &mut step {
        let args = mod_step.get_args();
        let by = mod_step.get_by();
        let child = mod_step.get_traversal();
        let with_option = mod_step.get_with_option();
        let target = if args.is_some() || child.is_some() {
            from_to_target(steps)
        } else {
            steps
                .iter()
                .rposition(|s| !matches!(s, GraphStep::Modulator(_, _) | GraphStep::As(_, _)))
        };
        let rejected = match target {
            Some(index) => {
                // The 1-based position of this modulator among the target's modulators of the
                // same kind (`by()`s counted separately from `from()`/`to()`s).
                let position = steps[index + 1..]
                    .iter()
                    .filter(
                        |s| matches!(s, GraphStep::Modulator(name, _) if *name == *modulator_name),
                    )
                    .count()
                    + 1;
                let target = &mut steps[index];
                let args_rejected =
                    args.and_then(
                        |args| match target.push_modulator_args(args, modulator_name) {
                            ArgsModulation::Accepted => None,
                            ArgsModulation::Unsupported => {
                                let rendered = target.get_name();
                                let name = user_step_name(&rendered);
                                Some(StepModulatorRejected {
                                    step: name.clone(),
                                    position,
                                    modulator: args_reason::MODULATOR,
                                    reason: format!("{name}(){}", args_reason::NOT_ACCEPTED),
                                })
                            }
                        },
                    );
                let by_rejected =
                    by.and_then(|by| match target.push_modulator_by(by, modulator_name) {
                        ByModulation::Accepted => None,
                        ByModulation::Unsupported => {
                            let rendered = target.get_name();
                            let name = user_step_name(&rendered);
                            // `select(Column.keys)` shares its name with the `select()` that does
                            // take `by()`, so it is named in full.
                            let shown = if name == "select" {
                                rendered
                            } else {
                                format!("{name}()")
                            };
                            Some(StepModulatorRejected {
                                step: name,
                                position,
                                modulator: by_reason::MODULATOR,
                                reason: format!("{shown}{}", by_reason::NOT_ACCEPTED),
                            })
                        }
                    });
                let child_rejected = child.and_then(|child| {
                    match target.push_modulator_traversal(child, modulator_name) {
                        ArgsModulation::Accepted => None,
                        ArgsModulation::Unsupported => {
                            let rendered = target.get_name();
                            let name = user_step_name(&rendered);
                            Some(StepModulatorRejected {
                                step: name.clone(),
                                position,
                                modulator: args_reason::MODULATOR,
                                reason: format!("{name}(){}", args_reason::NOT_ACCEPTED),
                            })
                        }
                    }
                });
                let with_rejected = with_option.and_then(|(key, value)| {
                    target.push_with_option(key, value).map(|reason| {
                        let name = user_step_name(&target.get_name());
                        StepModulatorRejected {
                            step: name.clone(),
                            position,
                            modulator: with_reason::MODULATOR,
                            reason: format!("{name}(): {reason}"),
                        }
                    })
                });
                args_rejected
                    .or(child_rejected)
                    .or(by_rejected)
                    .or(with_rejected)
            }
            None => {
                if with_option.is_some() {
                    Some(StepModulatorRejected {
                        step: source.to_string(),
                        position: 1,
                        modulator: with_reason::MODULATOR,
                        reason: with_reason::NO_STEP.to_string(),
                    })
                } else if args.is_some() || child.is_some() {
                    Some(StepModulatorRejected {
                        step: source.to_string(),
                        position: 1,
                        modulator: args_reason::MODULATOR,
                        reason: args_reason::NO_STEP.to_string(),
                    })
                } else {
                    by.map(|_| StepModulatorRejected {
                        step: source.to_string(),
                        position: 1,
                        modulator: by_reason::MODULATOR,
                        reason: by_reason::NO_STEP.to_string(),
                    })
                }
            }
        };
        if let Some(rejected) = rejected {
            step = GraphStep::Modulator(modulator_name, Box::new(rejected));
        }
    }
    steps.push(step);
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Takes the steps of `body` as the steps of this root traversal, replacing its own: the
    /// script DSL lowers `g` and `__` through the same anonymous-traversal builders. A
    /// `by()`/`from()`/`to()` with no step before it then names `g`, as when it is pushed onto
    /// `g` directly; rejections inside nested child traversals keep naming `__`.
    #[cfg_attr(not(feature = "script"), allow(dead_code))]
    pub(crate) fn adopt_lowered(mut self, body: AnonymousTraversal<'g, S>) -> Self {
        self.steps = body.steps;
        for step in &mut self.steps {
            if let GraphStep::Modulator(_, modulator) = step {
                modulator.rename_root("g");
            }
        }
        self
    }
}

/// The step a `from()`/`to()` modulator modulates: the `add_e()` it follows, with `property()`
/// calls, `as()` labels and other modulators in between (TinkerPop's `FromToModulating` modulates
/// the add step, whatever `property()`s come first); otherwise the last step that is neither a
/// modulator nor an `as()` label, so `g.V().property("a", 1).from("x")` still names `property()`.
pub(crate) fn from_to_target<'g, S: GraphStorage + 'g>(
    steps: &[GraphStep<'g, S>],
) -> Option<usize> {
    let mut index = steps.len();
    while index > 0 {
        match &steps[index - 1] {
            GraphStep::Modulator(_, _)
            | GraphStep::As(_, _)
            | GraphStep::SideEffectMut("property", _) => {
                index -= 1;
            }
            GraphStep::SideEffectMut("add_e", _) => return Some(index - 1),
            _ => break,
        }
    }
    steps
        .iter()
        .rposition(|s| !matches!(s, GraphStep::Modulator(_, _) | GraphStep::As(_, _)))
}

/// The name a user writes for a step, from its rendered form: `out(["knows"])` is `out`, and the
/// internal `has_p`/`where_p`/`where_t` variants are `has`/`where`.
fn user_step_name(rendered: &str) -> String {
    let base = rendered.split('(').next().unwrap_or(rendered);
    match base {
        "has_p" => "has".to_string(),
        "where_p" | "where_t" => "where".to_string(),
        other => other.to_string(),
    }
}

/// `InvalidModulator` reasons of a `by()` no step took; `help()` keys on them.
pub(crate) mod by_reason {
    /// The `modulator` shown in [`super::StepModulatorRejected`]'s `InvalidModulator`.
    pub(crate) const MODULATOR: &str = "by()";
    /// Appended to the rendered step (`out()` + this) that does not take `by()` modulators.
    pub(crate) const NOT_ACCEPTED: &str = " does not take by() modulators";
    /// A `by()` with no step before it.
    pub(crate) const NO_STEP: &str = "by() modulates the step before it, and there is none";
    /// A `by(Column)` on a step other than `order()`.
    pub(crate) const COLUMN: &str = "a Column (keys/values) is a sort key only order() takes";
}

/// `InvalidModulator` reasons of a `from()`/`to()` no step took; `help()` keys on them. Only
/// `add_e()`, `path()`, `simple_path()` and `cyclic_path()` accept `from()`/`to()`.
pub(crate) mod args_reason {
    /// The `modulator` shown in [`super::StepModulatorRejected`]'s `InvalidModulator`.
    pub(crate) const MODULATOR: &str = "from()/to()";
    /// Appended to the rendered step (`out()` + this) that does not take `from()`/`to()`
    /// modulators.
    pub(crate) const NOT_ACCEPTED: &str = " does not take from()/to() modulators";
    /// A `from()`/`to()` with no step before it.
    pub(crate) const NO_STEP: &str = "from()/to() modulates the step before it, and there is none";
    /// A `from()`/`to()` of `path()`/`simple_path()`/`cyclic_path()` that is not exactly one label.
    pub(crate) const ONE_LABEL: &str = "from()/to() of a path step takes exactly one step label";
}

/// `InvalidModulator` reasons of a step-level `with(WithOptions..)` no step took; `help()` keys
/// on them. Only `value_map()` (`WithOptions.tokens`) and `index()` (`WithOptions.indexer`) take
/// one.
pub(crate) mod with_reason {
    /// The `modulator` shown in [`super::StepModulatorRejected`]'s `InvalidModulator`.
    pub(crate) const MODULATOR: &str = "with()";
    /// The step does not take `with(WithOptions..)` options at all.
    pub(crate) const NOT_ACCEPTED: &str = "this step does not take with(WithOptions..) options";
    /// A `with()` with no step before it.
    pub(crate) const NO_STEP: &str = "with() configures the step before it, and there is none";
}

/// A `by()` or `from()`/`to()` modulator that no step took (see [`push_step_into`]): it stays in
/// the pipeline in place of the modulator and fails with
/// [`crate::error::TraverserError::InvalidModulator`] when the executor reaches it, whatever the
/// stream holds — and, before that, is found by the plan-time validation pass
/// (`optimizer::path_analysis::PathAnalysis`) that walks the whole plan, nested child traversals
/// included, so a rejected modulator inside a branch that never executes (e.g.
/// `where(__.out().by("x"))` on a traverser that never reaches `where()`) still fails instead of
/// silently succeeding.
#[derive(Debug, Clone)]
pub(crate) struct StepModulatorRejected {
    step: String,
    position: usize,
    /// The modulator kind shown in the error (`"by()"` or `"from()/to()"`).
    modulator: &'static str,
    reason: String,
}

/// The rejected modulator's kind (`by()`, `from()/to()`, `with()`); it never shows in a plan,
/// which leaves modulators out.
impl std::fmt::Display for StepModulatorRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.modulator)
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepModulator<'g, S> for StepModulatorRejected {
    fn rename_root(&mut self, root: &'static str) {
        if self.reason == by_reason::NO_STEP
            || self.reason == args_reason::NO_STEP
            || self.reason == with_reason::NO_STEP
        {
            self.step = root.to_string();
        }
    }

    fn check(&self) -> crate::error::TraverserResult<()> {
        Err(Box::new(crate::error::TraverserError::InvalidModulator {
            step: self.step.clone(),
            position: self.position,
            modulator: self.modulator,
            reason: self.reason.clone(),
        }))
    }

    fn clone_box(&self) -> Box<dyn GraphStepModulator<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

/// `InvalidModulator` reasons shared by the steps whose `by()` modulators form TinkerPop's
/// traversal ring (see [`ByModulatorCheck::observe_ring`]).
pub(crate) mod ring_reason {
    /// A `by()` kind that projects nothing the ring could apply to a key or path object.
    pub(crate) const KIND: &str =
        "an order or Count is not a projection; this step applies each by() to a value";
}

/// Records, for a step that consumes `by()` modulators as projections, the first `by()` it cannot
/// use, so the step can fail with [`crate::error::TraverserError::InvalidModulator`] when it
/// executes (step builders and `push_modulator_by` are infallible).
///
/// Counts every `by()` call the step receives, so the reported position is the 1-based index of
/// the offending call even when the step merges calls internally (e.g. `order()` folds a
/// direction-only `by(Order::Desc)` into the previous clause).
#[derive(Debug, Clone, Default)]
pub(crate) struct ByModulatorCheck {
    /// Number of `by()` calls observed so far.
    calls: usize,
    /// Position and reason of the first rejected `by()`, if any.
    rejected: Option<(usize, &'static str)>,
}

impl ByModulatorCheck {
    /// Observes one `by()` call, in the order the step receives them. A `by(Column)` is rejected
    /// with [`by_reason::COLUMN`]: only `order()` takes it (see
    /// [`observe_sort_key`](Self::observe_sort_key)).
    pub(crate) fn observe<'g, S: GraphStorage + 'g>(&mut self, by: &By<'g, S>) {
        self.observe_inner(by, false);
    }

    /// Observes one `by()` of `order()` (either scope), which also takes `by(Column::Keys)` /
    /// `by(Column::Values)` as a sort key.
    pub(crate) fn observe_sort_key<'g, S: GraphStorage + 'g>(&mut self, by: &By<'g, S>) {
        self.observe_inner(by, true);
    }

    fn observe_inner<'g, S: GraphStorage + 'g>(&mut self, by: &By<'g, S>, accepts_column: bool) {
        self.calls += 1;
        if self.rejected.is_some() {
            return;
        }
        match by {
            By::Labels(_) => {
                self.rejected = Some((
                    self.calls,
                    "a vertex-label list is not a projection this step can use",
                ));
            }
            By::Column(_) => {
                if !accepts_column {
                    self.rejected = Some((self.calls, by_reason::COLUMN));
                }
            }
            By::Property(_)
            | By::Traversal(_)
            | By::Order(_)
            | By::Label
            | By::Id
            | By::Value
            | By::Identity
            | By::Key
            | By::Count
            | By::JsonPath(_) => {}
        }
    }

    /// Observes one `by()` of a step whose `by()` modulators form TinkerPop's traversal ring
    /// (`select()`, `project()`, `path()`, `tree()`, `math()`): each one is a projection applied
    /// round-robin, so an order or `Count`, which projects nothing, is rejected with
    /// [`ring_reason::KIND`].
    pub(crate) fn observe_ring<'g, S: GraphStorage + 'g>(&mut self, by: &By<'g, S>) {
        self.observe(by);
        match by {
            By::Order(_) | By::Count => self.reject_last(ring_reason::KIND),
            By::Property(_)
            | By::Traversal(_)
            | By::Label
            | By::Id
            | By::Value
            | By::Identity
            | By::Key
            | By::JsonPath(_)
            | By::Labels(_)
            | By::Column(_) => {}
        }
    }

    /// Rejects the most recently observed `by()` for a step-specific `reason` (such as a kind the
    /// step cannot use, or one `by()` too many), unless an earlier one was rejected already. Call
    /// it right after [`observe`](Self::observe) for the same `by()`.
    pub(crate) fn reject_last(&mut self, reason: &'static str) {
        if self.rejected.is_none() {
            self.rejected = Some((self.calls, reason));
        }
    }

    /// Whether an observed `by()` was rejected; fast paths that would bypass `check()` (such as
    /// an optimizer pushdown) must not fire then.
    pub(crate) fn has_rejected(&self) -> bool {
        self.rejected.is_some()
    }

    /// Fails with `InvalidModulator` for `step` when an observed `by()` was rejected.
    #[inline]
    pub(crate) fn check(&self, step: &str) -> crate::error::TraverserResult<()> {
        match self.rejected {
            Some((position, reason)) => {
                Err(Box::new(crate::error::TraverserError::InvalidModulator {
                    step: step.to_string(),
                    position,
                    modulator: by_reason::MODULATOR,
                    reason: reason.to_string(),
                }))
            }
            None => Ok(()),
        }
    }
}

/// Sort direction of `order().by(...)`.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Order {
    /// Ascending.
    Asc,
    /// Descending.
    Desc,
    /// Random order (TinkerPop `Order.shuffle`): the items are shuffled with the execution's
    /// random number generator (`g.with("random.seed", n)` makes it reproducible), then sorted
    /// by any clause that follows.
    Shuffle,
}

/// The DSL token: `Order.asc`, `Order.desc`, `Order.shuffle`.
impl std::fmt::Display for Order {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Order::Asc => "Order.asc",
            Order::Desc => "Order.desc",
            Order::Shuffle => "Order.shuffle",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn test_anonymous_traversal_send_sync() {
        assert_send_sync::<AnonymousTraversal<crate::graph::TraversalGraph>>();
    }

    #[test]
    fn test_anonymous_traversal_step_chain() {
        let anon: AnonymousTraversal<crate::graph::TraversalGraph> =
            __::out("knows").has("name", "Marko");
        let display = format!("{}", anon);
        assert_eq!(display, r#"__.out("knows").has("name", "Marko")"#);
        assert!(display.contains("has"));
    }
}
