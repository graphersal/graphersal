pub(crate) mod path_analysis;
pub(crate) mod plan;
pub(crate) mod rule;
pub(crate) mod rules;

use std::cell::RefCell;

use compact_str::CompactString;

use crate::graph::GraphStorage;
use crate::traversal::context::ExecutionOptions;
use crate::traversal::step::GraphStep;
pub(crate) use plan::{LevelPlan, Plan};
pub(crate) use rule::OptimizationRule;
pub(crate) use rules::add_property_fold::AddPropertyFoldRule;
pub(crate) use rules::barrier_pushdown::BarrierPushdownRule;
pub(crate) use rules::count_pushdown::CountPushdownRule;
pub(crate) use rules::filter_pushdown::SourceFilterPushdownRule;
pub(crate) use rules::filter_reorder::FilterReorderRule;
pub(crate) use rules::group_count_pushdown::GroupCountPushdownRule;
pub(crate) use rules::has_id_pushdown::HasIdPushdownRule;
pub(crate) use rules::lazy_barrier::LazyBarrierRule;
pub(crate) use rules::property_index::PropertyIndexRule;
pub(crate) use rules::where_unnest::WhereUnnestRule;

/// The stable, user-facing name of every optimizer rule this crate knows, and whether it runs by
/// default. These are exactly the strings `.profile()` renders in its "Optimizer rules applied"
/// line, and the strings `g.with("optimizer.disabled", [...])`/`g.with("optimizer.enabled", [...])`
/// accept — `ExecutionOptions::parse` (`traversal/context.rs`) validates against this list.
/// Independent of the storage type parameter `S`, since a rule's name never depends on it;
/// [`TraversalOptimizer::registry`] pairs each of these names with the per-`S` constructor. All 10
/// rules that exist today default to on: nothing in this crate ships off-by-default yet, but the
/// mechanism is ready for a future rule that should (per the spec motivating this list, a
/// cardinality-aware hop-direction rewrite is expected to be the first).
pub(crate) const KNOWN_RULES: &[(&str, bool)] = &[
    ("where_unnest", true),
    ("add_property_fold", true),
    ("filter_reorder", true),
    ("has_id_pushdown", true),
    ("source_filter_pushdown", true),
    ("property_index", true),
    ("barrier_pushdown", true),
    ("group_count_pushdown", true),
    ("count_pushdown", true),
    ("lazy_barrier", true),
];

fn new_where_unnest<'g, S: GraphStorage + 'g>() -> Box<dyn OptimizationRule<'g, S> + 'g> {
    Box::new(WhereUnnestRule::new())
}
fn new_add_property_fold<'g, S: GraphStorage + 'g>() -> Box<dyn OptimizationRule<'g, S> + 'g> {
    Box::new(AddPropertyFoldRule::new())
}
fn new_filter_reorder<'g, S: GraphStorage + 'g>() -> Box<dyn OptimizationRule<'g, S> + 'g> {
    Box::new(FilterReorderRule::new())
}
fn new_has_id_pushdown<'g, S: GraphStorage + 'g>() -> Box<dyn OptimizationRule<'g, S> + 'g> {
    Box::new(HasIdPushdownRule::new())
}
fn new_source_filter_pushdown<'g, S: GraphStorage + 'g>() -> Box<dyn OptimizationRule<'g, S> + 'g> {
    Box::new(SourceFilterPushdownRule::new())
}
fn new_property_index<'g, S: GraphStorage + 'g>() -> Box<dyn OptimizationRule<'g, S> + 'g> {
    Box::new(PropertyIndexRule::new())
}
fn new_barrier_pushdown<'g, S: GraphStorage + 'g>() -> Box<dyn OptimizationRule<'g, S> + 'g> {
    Box::new(BarrierPushdownRule::new())
}
fn new_group_count_pushdown<'g, S: GraphStorage + 'g>() -> Box<dyn OptimizationRule<'g, S> + 'g> {
    Box::new(GroupCountPushdownRule::new())
}
fn new_lazy_barrier<'g, S: GraphStorage + 'g>() -> Box<dyn OptimizationRule<'g, S> + 'g> {
    Box::new(LazyBarrierRule::new())
}
fn new_count_pushdown<'g, S: GraphStorage + 'g>() -> Box<dyn OptimizationRule<'g, S> + 'g> {
    Box::new(CountPushdownRule::new())
}

/// One [`TraversalOptimizer::registry`] entry: a rule's name, whether it defaults to on, and its
/// constructor.
type RegistryEntry<'g, S> = (
    &'static str,
    bool,
    fn() -> Box<dyn OptimizationRule<'g, S> + 'g>,
);

/// Rule-based optimization pipeline that transforms logical traversal steps
/// into an optimized physical execution plan.
pub(crate) struct TraversalOptimizer<'g, S: GraphStorage + 'g> {
    rules: Vec<Box<dyn OptimizationRule<'g, S> + 'g>>,
    /// Whether rewrites record where each executed step came from, for diagnostics.
    diagnostics: bool,
    /// Names of every rule that modified the plan anywhere in this query so far — top-level and
    /// any nested level, since `apply()` recurses into nested traversals through
    /// `step.optimize_nested(self)` on a shared `&self` (D1). `RefCell` because `apply()` only
    /// borrows `self` immutably (nested recursion needs to keep sharing the same optimizer
    /// instance and its rule set), so this is the one field that needs interior mutability to
    /// record a fired rule from inside that shared borrow.
    fired: RefCell<Vec<&'static str>>,
    /// The property indexes the storage serves (read once per run, see
    /// [`Self::for_run`]); empty: no index is used.
    indexes: Vec<crate::graph::PropertyIndexInfo>,
}

/// What a rule may know about the run beyond the plan ([`OptimizationRule::apply_in`]).
pub(crate) struct OptimizerContext<'a> {
    /// The property indexes the storage serves, with their stats.
    pub(crate) indexes: &'a [crate::graph::PropertyIndexInfo],
}

impl<'g, S: GraphStorage + 'g> Default for TraversalOptimizer<'g, S> {
    fn default() -> Self {
        Self {
            rules: Vec::new(),
            diagnostics: true,
            fired: RefCell::new(Vec::new()),
            indexes: Vec::new(),
        }
    }
}

impl<'g, S: GraphStorage + 'g> TraversalOptimizer<'g, S> {
    /// Creates an empty optimizer with no rules.
    pub fn new() -> Self {
        Self::default()
    }

    /// Turns provenance tracking on or off for this pipeline and every nested level it optimizes.
    pub fn with_diagnostics(mut self, diagnostics: bool) -> Self {
        self.diagnostics = diagnostics;
        self
    }

    /// Every rule this crate knows, in [`KNOWN_RULES`] order: its name, whether it is part of
    /// `default_pipeline()`'s on-by-default set, and a constructor for it. This is the registry
    /// D3 needs: `optimizer.enabled` names a rule that may not be in the on-by-default `Vec` at
    /// all, so resolving it to an actual rule instance needs a name -> constructor mapping that
    /// covers every known rule, not just the ones `default_pipeline()` builds today. A unit test
    /// in this module checks this list's names against [`KNOWN_RULES`], so the two can't drift.
    pub(crate) fn registry() -> [RegistryEntry<'g, S>; KNOWN_RULES.len()] {
        [
            ("where_unnest", true, new_where_unnest::<'g, S>),
            ("add_property_fold", true, new_add_property_fold::<'g, S>),
            ("filter_reorder", true, new_filter_reorder::<'g, S>),
            ("has_id_pushdown", true, new_has_id_pushdown::<'g, S>),
            (
                "source_filter_pushdown",
                true,
                new_source_filter_pushdown::<'g, S>,
            ),
            ("property_index", true, new_property_index::<'g, S>),
            ("barrier_pushdown", true, new_barrier_pushdown::<'g, S>),
            (
                "group_count_pushdown",
                true,
                new_group_count_pushdown::<'g, S>,
            ),
            ("count_pushdown", true, new_count_pushdown::<'g, S>),
            ("lazy_barrier", true, new_lazy_barrier::<'g, S>),
        ]
    }

    /// Creates an optimizer with the default optimization pipeline: every rule in [`KNOWN_RULES`]
    /// that defaults to on, with none of `optimizer.disabled`/`optimizer.enabled` applied.
    /// Equivalent to [`Self::default_pipeline_with`]`(&ExecutionOptions::default())`; kept as a
    /// no-argument convenience for tests that don't exercise per-query rule toggles.
    #[allow(dead_code)]
    pub fn default_pipeline() -> Self {
        Self::default_pipeline_with(&ExecutionOptions::default())
    }

    /// Creates an optimizer with the default optimization pipeline, honoring this execution's
    /// `optimizer.disabled`/`optimizer.enabled` options (D3).
    ///
    /// Filters the registry at construction time rather than skipping a rule's `apply()` call at
    /// pipeline-run time: a disabled rule is then simply not in `self.rules`, so it can never be
    /// recorded as fired (D1's list can't spuriously include a rule that was actually skipped),
    /// and `default_pipeline_with` is called once per query
    /// (`GraphTraversalSource::execute_steps`), so there is no reused-pipeline lifecycle this
    /// would fight with. `ExecutionOptions::parse` has already validated every name in both sets
    /// against [`KNOWN_RULES`] and rejected a name present in both, so this only has to consult
    /// the sets, not re-check them.
    pub fn default_pipeline_with(options: &ExecutionOptions) -> Self {
        Self::pipeline_with(options, true)
    }

    /// The pipeline of one run: [`Self::default_pipeline_with`] with the storage's property
    /// `indexes`. Without any index the `property_index` rule is left out entirely (it could not
    /// pick anything, and a source only carries an index access that rule set in this same run),
    /// so a graph without indexes pays neither its construction nor its walk of the plan; it
    /// never fires there, so `.profile()`'s list of applied rules is the same either way.
    pub fn for_run(
        options: &ExecutionOptions,
        indexes: Vec<crate::graph::PropertyIndexInfo>,
    ) -> Self {
        let mut optimizer = Self::pipeline_with(options, !indexes.is_empty());
        optimizer.indexes = indexes;
        optimizer
    }

    /// The default-on rules honouring `options`; `property_index` only with `with_indexes`.
    fn pipeline_with(options: &ExecutionOptions, with_indexes: bool) -> Self {
        let registry = Self::registry();
        let mut optimizer = Self::new();
        optimizer.rules.reserve_exact(registry.len());
        for (name, default_on, ctor) in registry {
            if !with_indexes && name == "property_index" {
                continue;
            }
            let disabled = options.optimizer_disabled.contains(name);
            let enabled = options.optimizer_enabled.contains(name);
            if (default_on && !disabled) || (!default_on && enabled) {
                optimizer.add_rule(ctor());
            }
        }
        optimizer
    }

    /// Adds a rule to the optimizer pipeline.
    #[allow(dead_code)]
    pub fn add_rule(&mut self, rule: Box<dyn OptimizationRule<'g, S> + 'g>) {
        self.rules.push(rule);
    }

    /// Recursively optimizes nested child traversals within steps (bottom-up),
    /// then applies all registered rules sequentially to the current traversal steps.
    ///
    /// Returns the rewritten level, so a failure can be reported against the query as the user
    /// wrote it. `None` when no rule changed anything and the executed plan still is that query.
    pub fn apply(&self, steps: &mut Vec<GraphStep<'g, S>>) -> Option<LevelPlan<'g, S>> {
        let mut plan = Plan::from_steps(std::mem::take(steps), self.diagnostics);
        for step in plan.steps_mut() {
            step.optimize_nested(self);
        }
        let context = OptimizerContext {
            indexes: &self.indexes,
        };
        for rule in &self.rules {
            if rule.apply_in(&mut plan, &context) {
                self.fired.borrow_mut().push(rule.name());
            }
        }
        let (optimized, level) = plan.into_parts();
        *steps = optimized;
        level
    }

    /// Deduplicated names of every rule that fired (modified the plan) anywhere in this query —
    /// the union of the top level and every nested level optimized through this same instance
    /// (D1), in pipeline order. Collected unconditionally during `apply()`, regardless of whether
    /// `.profile()`/`.profile_with()` was requested for this run: tracking a handful of
    /// `&'static str`s behind a rule call that already ran is near-free, so there is no separate
    /// opt-in gate here (D2) — only `.profile()`'s `Display` impl decides whether anything
    /// renders, and a plain `.to_list()`/`.next()` run simply never reads this.
    pub fn fired_rules(&self) -> Vec<&'static str> {
        let fired = self.fired.borrow();
        let mut names: Vec<&'static str> = Vec::with_capacity(fired.len());
        for name in fired.iter() {
            if !names.contains(name) {
                names.push(*name);
            }
        }
        names
    }

    /// Runs the path requirement analysis on an optimized top-level traversal, after all rules,
    /// and returns the path mode of each of its physical steps. Nested levels store their own
    /// modes. `enabled == false` is the `path.analysis` kill switch: every step records its full
    /// path.
    ///
    /// Fails before any traverser executes when a `select()`-like reader names a label nothing in
    /// the query could ever declare; see `path_analysis::PathAnalysis::analyze_query`.
    pub fn analyze_paths(
        &self,
        steps: &mut [GraphStep<'g, S>],
        enabled: bool,
        predeclared_side_effects: &std::collections::HashMap<
            CompactString,
            crate::traversal::side_effect::SideEffectValue<S>,
        >,
    ) -> crate::error::TraverserResult<(Vec<path_analysis::PathMode>, path_analysis::PlanEffects)>
    {
        path_analysis::PathAnalysis::analyze_query(enabled, steps, predeclared_side_effects)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::TraversalGraph;

    struct DummyRule {
        applied: std::sync::atomic::AtomicBool,
    }

    impl DummyRule {
        fn new() -> Self {
            Self {
                applied: std::sync::atomic::AtomicBool::new(false),
            }
        }
    }

    impl<'g> OptimizationRule<'g, TraversalGraph> for DummyRule {
        fn name(&self) -> &'static str {
            "dummy_rule"
        }

        fn apply(&self, _plan: &mut Plan<'g, TraversalGraph>) -> bool {
            self.applied
                .store(true, std::sync::atomic::Ordering::SeqCst);
            true
        }
    }

    #[test]
    fn test_empty_optimizer_leaves_steps_untouched() {
        let optimizer = TraversalOptimizer::<TraversalGraph>::default_pipeline();
        let mut steps: Vec<GraphStep<TraversalGraph>> = Vec::new();
        optimizer.apply(&mut steps);
        assert!(steps.is_empty());
    }

    #[test]
    fn test_optimizer_executes_rules() {
        let mut optimizer = TraversalOptimizer::<TraversalGraph>::new();
        let rule = Box::new(DummyRule::new());
        assert_eq!(rule.name(), "dummy_rule");
        optimizer.add_rule(rule);

        let mut steps: Vec<GraphStep<TraversalGraph>> = Vec::new();
        optimizer.apply(&mut steps);
    }

    #[test]
    fn test_optimizer_executes_rules_records_the_fired_rule() {
        let mut optimizer = TraversalOptimizer::<TraversalGraph>::new();
        optimizer.add_rule(Box::new(DummyRule::new()));

        let mut steps: Vec<GraphStep<TraversalGraph>> = Vec::new();
        optimizer.apply(&mut steps);
        assert_eq!(optimizer.fired_rules(), vec!["dummy_rule"]);
    }

    /// D3's registry is the one genuinely new piece of infrastructure this crate needs for
    /// `optimizer.enabled` to ever resolve a name to a rule instance. Nothing off-by-default
    /// exists yet to exercise the "enable a normally-off rule" path end to end, so this checks
    /// the registry's own correctness instead: every name in [`KNOWN_RULES`] resolves to a
    /// constructible rule, in the same order, with the same default-on flag, and constructing it
    /// actually returns a rule reporting that same name.
    #[test]
    fn test_registry_matches_known_rules() {
        let registry = TraversalOptimizer::<TraversalGraph>::registry();
        assert_eq!(registry.len(), KNOWN_RULES.len());
        for ((reg_name, reg_default_on, ctor), (known_name, known_default_on)) in
            registry.iter().zip(KNOWN_RULES.iter())
        {
            assert_eq!(reg_name, known_name);
            assert_eq!(reg_default_on, known_default_on);
            let rule = ctor();
            assert_eq!(rule.name(), *known_name);
        }
    }

    /// `default_pipeline_with` filters at construction time (D3): a disabled rule is not in
    /// `self.rules` at all, so it is never a candidate to fire, and an unrelated rule is
    /// unaffected.
    #[test]
    fn test_default_pipeline_with_honors_disabled_set() {
        let mut options = ExecutionOptions::default();
        options
            .optimizer_disabled
            .insert("filter_reorder".to_string());
        let optimizer = TraversalOptimizer::<TraversalGraph>::default_pipeline_with(&options);
        assert!(!optimizer.rules.iter().any(|r| r.name() == "filter_reorder"));
        assert!(optimizer.rules.iter().any(|r| r.name() == "count_pushdown"));
    }
}
