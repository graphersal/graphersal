//! Path requirement analysis (in the spirit of TinkerPop's `PathRetractionStrategy`).
//!
//! Every step reports what it reads from the path of its incoming traversers through
//! `path_requirement()`, and steps holding child traversals report what their children read.
//! One backward walk per traversal level then decides, for each step, what it must *write* into
//! the paths of its outputs so that every later reader finds what it needs — and nothing more:
//!
//! - [`PathMode::Full`]: the step records every output object (`path()`, `simple_path()`,
//!   `cyclic_path()` downstream);
//! - [`PathMode::Labels`]: only an `as()` with a label that is read later records its position;
//!   every other step keeps the incoming path untouched;
//! - [`PathMode::None`]: the step drops the path of its outputs.
//!
//! Labels are retracted past the `as()` that produces them, so a label nobody reads further
//! upstream is never stored, and tracking stops after the last consumer. The pass runs after all
//! optimizer rules, because rules fuse and remove steps.

use std::fmt;
use std::sync::Arc;

use compact_str::CompactString;

use crate::error::{TraverserError, TraverserResult, ValueError};
use crate::graph::GraphStorage;
use crate::traversal::step::GraphStep;

/// Step "kind names" (the `&'static str` every `GraphStep` variant carries) that can turn any
/// incoming value into a map/object shape a later `select()` might legitimately read a key from
/// (a `valueMap()`/`elementMap()`/`project()`/`group()`/... result, an `inject()`-ed map, a
/// nested JSON property, an `unfold()`-ed entry, ...). Seeing any of them anywhere in the query (top level or nested)
/// disables the plan-time "undeclared label" check for the whole query: we can prove a label is
/// declared, but we can never prove one *isn't* possibly a map/side-effect key once a step like
/// this is in play, and the spec is explicit that an unprovable case must never be reported as an
/// error (see `step_select.rs`'s module doc and the design spec, task 1).
const MAP_PRODUCING_KINDS: &[&str] = &[
    "values",
    "properties",
    "value_map",
    "element_map",
    "project",
    "gql:project",
    "constant",
    "unfold",
    "group",
    "property_json",
    "json_path",
    "inject",
];

/// A sorted, deduplicated set of step labels, built once at compile time and never per traverser.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SmallLabelSet(Arc<[CompactString]>);

impl SmallLabelSet {
    /// Builds a set from any list of labels.
    pub(crate) fn new<I, T>(labels: I) -> Self
    where
        I: IntoIterator<Item = T>,
        T: Into<CompactString>,
    {
        let mut labels: Vec<CompactString> = labels.into_iter().map(Into::into).collect();
        labels.sort_unstable();
        labels.dedup();
        Self(Arc::from(labels))
    }

    /// Returns `true` when the set holds no label.
    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The labels, sorted, shared by reference count.
    pub(crate) fn as_arc(&self) -> &Arc<[CompactString]> {
        &self.0
    }

    /// Returns `true` when `label` is in the set.
    pub(crate) fn contains(&self, label: &str) -> bool {
        self.0.binary_search_by(|l| l.as_str().cmp(label)).is_ok()
    }

    fn union(&self, other: &SmallLabelSet) -> SmallLabelSet {
        if other.0.iter().all(|l| self.contains(l)) {
            return self.clone();
        }
        SmallLabelSet::new(self.0.iter().chain(other.0.iter()).cloned())
    }

    fn without(&self, labels: &[CompactString]) -> SmallLabelSet {
        if !labels.iter().any(|l| self.contains(l)) {
            return self.clone();
        }
        SmallLabelSet::new(self.0.iter().filter(|l| !labels.contains(l)).cloned())
    }

    /// Returns `true` when at least one of `labels` is in the set.
    pub(crate) fn intersects(&self, labels: &[CompactString]) -> bool {
        labels.iter().any(|l| self.contains(l))
    }
}

/// What a step reads from the path of its incoming traversers.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub(crate) enum PathRequirement {
    /// The step does not read the path.
    #[default]
    None,
    /// The step reads these step labels.
    Labels(SmallLabelSet),
    /// The step reads the whole path.
    Full,
}

/// What a step writes into the paths of its outputs; see the module documentation.
///
/// It has the same shape as a [`PathRequirement`]: the mode of a step is the requirement of every
/// step after it.
pub(crate) type PathMode = PathRequirement;

impl PathRequirement {
    /// A requirement for `labels`; `None` when there is no label.
    pub(crate) fn labels<I, T>(labels: I) -> Self
    where
        I: IntoIterator<Item = T>,
        T: Into<CompactString>,
    {
        let set = SmallLabelSet::new(labels);
        if set.is_empty() {
            PathRequirement::None
        } else {
            PathRequirement::Labels(set)
        }
    }

    /// Returns `true` for [`PathRequirement::None`].
    pub(crate) fn is_none(&self) -> bool {
        matches!(self, PathRequirement::None)
    }

    /// Returns `true` for [`PathRequirement::Full`].
    pub(crate) fn is_full(&self) -> bool {
        matches!(self, PathRequirement::Full)
    }

    /// Set union; `Full ∪ x = Full`.
    pub(crate) fn union(self, other: PathRequirement) -> PathRequirement {
        match (self, other) {
            (PathRequirement::Full, _) | (_, PathRequirement::Full) => PathRequirement::Full,
            (PathRequirement::None, x) | (x, PathRequirement::None) => x,
            (PathRequirement::Labels(a), PathRequirement::Labels(b)) => {
                PathRequirement::Labels(a.union(&b))
            }
        }
    }

    /// Removes the labels an `as()` produces: upstream of it nobody needs them anymore.
    /// `Full` is never retracted, because it covers the path from its start.
    fn retract(self, labels: &[CompactString]) -> PathRequirement {
        match self {
            PathRequirement::Labels(set) => {
                let rest = set.without(labels);
                if rest.is_empty() {
                    PathRequirement::None
                } else {
                    PathRequirement::Labels(rest)
                }
            }
            other => other,
        }
    }
}

impl fmt::Display for PathRequirement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PathRequirement::None => f.write_str("none"),
            PathRequirement::Full => f.write_str("full"),
            PathRequirement::Labels(set) => {
                f.write_str("labels(")?;
                for (i, label) in set.as_arc().iter().enumerate() {
                    if i > 0 {
                        f.write_str(",")?;
                    }
                    f.write_str(label)?;
                }
                f.write_str(")")
            }
        }
    }
}

/// What a plan (a level and everything nested in it) does besides reading the graph, found by
/// the same walk as the path requirements.
///
/// Merge points consult it: a plan that mutates never merges, and a child traversal that writes
/// side effects runs once per unit of bulk.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PlanEffects {
    /// Some step mutates the graph (`addV`, `addE`, `property`, `mergeV`, `mergeE`, `drop`).
    pub(crate) mutates: bool,
    /// Some step writes a side effect (`aggregate`, `store`, `subgraph`, `tree("x")`,
    /// `sideEffect(fn)`).
    pub(crate) writes_side_effects: bool,
}

impl PlanEffects {
    /// The effects of both plans together.
    pub(crate) fn or(self, other: PlanEffects) -> PlanEffects {
        PlanEffects {
            mutates: self.mutates || other.mutates,
            writes_side_effects: self.writes_side_effects || other.writes_side_effects,
        }
    }
}

/// The path requirement analysis pass.
#[derive(Debug)]
pub(crate) struct PathAnalysis {
    /// Effects of the levels walked so far; see [`PlanEffects`].
    effects: std::cell::Cell<PlanEffects>,
    /// Kill switch (`g.with("path.analysis", false)`): every step records its full path, which is
    /// the behaviour without analysis. Used to verify the analysis and to diagnose it.
    force_full: bool,
    /// Labels produced by an `as()`, or declared by a keyed side effect
    /// (`aggregate`/`store`/`tree`), anywhere in the query, collected while analysing.
    produced: std::cell::RefCell<Vec<CompactString>>,
    /// When set, label requirements are restricted to these labels: a label no `as()` produces
    /// can never be found on a path (a string argument of `is("marko")` is not a label).
    known: Option<SmallLabelSet>,
    /// Whether some step required labels, so that restricting them can change anything.
    saw_labels: std::cell::Cell<bool>,
    /// Every label a `select()`-like reader hard-requires (`required_labels()`), together with
    /// the requiring step's name, collected unrestricted (rule: the plan-time "undeclared label"
    /// check needs the raw request, not the path-mode-restricted one). Empty unless the first,
    /// collecting pass populates it — see `analyze_query`.
    required: std::cell::RefCell<Vec<(CompactString, &'static str)>>,
    /// Whether any step anywhere in the query (top level or nested) is one of
    /// [`MAP_PRODUCING_KINDS`], so the query cannot be proven free of "the key might come from a
    /// map/side effect after all" cases.
    map_producing_seen: std::cell::Cell<bool>,
    /// The first rejected `by()`/`from()`/`to()` modulator marker (`StepModulatorRejected`) found
    /// anywhere in the query (top level or nested), if any — collected while walking every level,
    /// so a rejected modulator inside a branch that never executes (`where(__.out().by("x"))` on
    /// a traverser that never reaches `where()`) is still reported before execution.
    rejected_modulator: std::cell::RefCell<Option<crate::error::BoxedTraverserError>>,
}

impl PathAnalysis {
    /// Creates the pass; `enabled == false` forces [`PathMode::Full`] for every step.
    pub(crate) fn new(enabled: bool) -> Self {
        Self {
            force_full: !enabled,
            effects: std::cell::Cell::new(PlanEffects::default()),
            produced: std::cell::RefCell::new(Vec::new()),
            known: None,
            saw_labels: std::cell::Cell::new(false),
            required: std::cell::RefCell::new(Vec::new()),
            map_producing_seen: std::cell::Cell::new(false),
            rejected_modulator: std::cell::RefCell::new(None),
        }
    }

    /// Analyses a top-level traversal: a first walk collects the labels the query produces (and,
    /// along the way, every label a reader hard-requires), the second one computes the modes with
    /// requirements restricted to those labels.
    ///
    /// Fails with [`crate::error::TraverserError::InvalidModulator`] when the first walk finds a
    /// rejected `by()`/`from()`/`to()` marker (`StepModulatorRejected`) anywhere in the query, top
    /// level or nested — before either walk's output is used to execute anything, so a rejected
    /// modulator inside a branch the data never reaches still fails (see
    /// `check_rejected_modulator`).
    ///
    /// Fails with [`ValueError::UndeclaredLabel`] when some
    /// step's `required_labels()` names a label that no `as()` and no keyed side effect
    /// (`aggregate`/`store`/`tree`) declares anywhere in the query (any nesting level), *and* the
    /// query contains no step that could plausibly produce a map/side-effect value the label
    /// might legitimately come from instead (see [`MAP_PRODUCING_KINDS`]) — a plan-time,
    /// before-execution check: query-authoring mistakes like `g.V().select("nope")` must not
    /// silently return nothing (design spec, task 1).
    pub(crate) fn analyze_query<'g, S: GraphStorage + 'g>(
        enabled: bool,
        steps: &mut [GraphStep<'g, S>],
        predeclared_side_effects: &std::collections::HashMap<
            CompactString,
            crate::traversal::side_effect::SideEffectValue<S>,
        >,
    ) -> TraverserResult<(Vec<PathMode>, PlanEffects)> {
        let collect = PathAnalysis::new(enabled);
        // `with_side_effect("x", ...)` populates `context.side_effect` directly at build time,
        // outside the step pipeline this pass otherwise walks, so it declares a label the same
        // way `aggregate`/`store`/`tree` do (see `error.rs`'s "side_effect" help text) but this
        // pass would never see it on its own.
        collect
            .produced
            .borrow_mut()
            .extend(predeclared_side_effects.keys().cloned());
        let (modes, _) = collect.analyze_level(steps, PathRequirement::None);
        collect.check_rejected_modulator()?;
        collect.check_undeclared_labels()?;
        if collect.force_full || !collect.saw_labels.get() {
            // Nothing to restrict: the first walk already is the result.
            return Ok((modes, collect.effects.get()));
        }
        let effects = collect.effects.get();
        let known = SmallLabelSet::new(collect.produced.into_inner());
        let analysis = PathAnalysis {
            force_full: false,
            effects: std::cell::Cell::new(PlanEffects::default()),
            produced: std::cell::RefCell::new(Vec::new()),
            known: Some(known),
            saw_labels: std::cell::Cell::new(false),
            required: std::cell::RefCell::new(Vec::new()),
            map_producing_seen: std::cell::Cell::new(false),
            rejected_modulator: std::cell::RefCell::new(None),
        };
        Ok((
            analysis.analyze_level(steps, PathRequirement::None).0,
            effects,
        ))
    }

    /// The plan-time rejected-modulator check: fails with the first `by()`/`from()`/`to()` no
    /// step took, found anywhere in the query (top level or nested), before execution starts.
    /// Without this, a rejected modulator inside a branch the data never reaches (e.g.
    /// `g.v().has("name", "nobody").where(__.out().by("x"))`) would only fail when some other
    /// input happens to reach it, unlike TinkerPop, which rejects such queries at build time.
    fn check_rejected_modulator(&self) -> TraverserResult<()> {
        match self.rejected_modulator.borrow_mut().take() {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }

    /// The plan-time "undeclared label" check itself; see `analyze_query`'s doc comment.
    fn check_undeclared_labels(&self) -> TraverserResult<()> {
        if self.map_producing_seen.get() {
            // A key this query reads might legitimately come from a map or a side effect we
            // cannot rule out; never report a plausible case as a static error.
            return Ok(());
        }
        let produced = self.produced.borrow();
        let required = self.required.borrow();
        for (label, _step_name) in required.iter() {
            if !produced.iter().any(|p| p == label) {
                return Err(Box::new(TraverserError::Value(
                    ValueError::UndeclaredLabel {
                        label: label.to_string(),
                    },
                )));
            }
        }
        Ok(())
    }

    /// Drops the labels of `requirement` that no `as()` in the query produces.
    fn restrict(&self, requirement: PathRequirement) -> PathRequirement {
        if matches!(requirement, PathRequirement::Labels(_)) {
            self.saw_labels.set(true);
        }
        match (requirement, &self.known) {
            (PathRequirement::Labels(set), Some(known)) => {
                PathRequirement::labels(set.as_arc().iter().filter(|l| known.contains(l)).cloned())
            }
            (requirement, _) => requirement,
        }
    }

    /// Records what `step` does to the graph and to side effects.
    fn note_effects<'g, S: GraphStorage + 'g>(&self, step: &GraphStep<'g, S>) {
        self.effects.set(self.effects.get().or(step.own_effects()));
    }

    /// Takes the effects found so far, leaving none; a child traversal brackets its own level
    /// with it.
    pub(crate) fn take_effects(&self) -> PlanEffects {
        self.effects.replace(PlanEffects::default())
    }

    /// Restores `saved` ORed with `own` after a child level.
    pub(crate) fn restore_effects(&self, saved: PlanEffects, own: PlanEffects) {
        self.effects.set(saved.or(own));
    }

    /// Analyses one traversal level with a backward walk.
    ///
    /// `downstream` is what the steps after this level read: `None` for the top level and for a
    /// child whose output does not continue the parent path, the parent's own downstream need for
    /// a child that does (`union()`, `coalesce()`, `optional()`). Returns the mode of every
    /// physical step (modulators included, as `None`) and the requirement left at the entry of
    /// the level, which is what the step holding the level reports to its parent.
    pub(crate) fn analyze_level<'g, S: GraphStorage + 'g>(
        &self,
        steps: &mut [GraphStep<'g, S>],
        downstream: PathRequirement,
    ) -> (Vec<PathMode>, PathRequirement) {
        let mut modes = vec![PathMode::None; steps.len()];
        let mut need = if self.force_full {
            PathRequirement::Full
        } else {
            downstream
        };
        // Whether the next executed step is a fused `count()`, which skips pure maps.
        let mut next_is_count = false;
        // The rejected-modulator marker directly at this level with the smallest step index (the
        // one written first, as the user wrote the query), found while walking this level
        // backward. Tracked locally so several rejections at the same level are resolved by
        // position, not by backward-walk order; see `check_rejected_modulator`. A rejection found
        // in a nested child (through `analyze_nested_paths` below) is written straight into
        // `self.rejected_modulator` by that child's own call to this method, and takes priority
        // over this level's own markers when it happens first.
        let mut own_rejected_modulator: Option<(usize, crate::error::BoxedTraverserError)> = None;

        for index in (0..steps.len()).rev() {
            let step = &mut steps[index];
            if matches!(step, GraphStep::Modulator(_, _)) {
                if let Err(err) = step.modulator_check() {
                    let replace = match &own_rejected_modulator {
                        None => true,
                        Some((existing_index, _)) => index < *existing_index,
                    };
                    if replace {
                        own_rejected_modulator = Some((index, err));
                    }
                }
                continue;
            }
            // A build-time problem of a mutating step (an invalid constant `mergeV()` map) is
            // reported the same way as a rejected modulator: before execution, by position.
            if let Err(err) = step.modulator_check() {
                let replace = match &own_rejected_modulator {
                    None => true,
                    Some((existing_index, _)) => index < *existing_index,
                };
                if replace {
                    own_rejected_modulator = Some((index, err));
                }
            }
            self.note_effects(step);
            let children = step.analyze_nested_paths(self, &need);
            let is_count = matches!(step, GraphStep::Count(_, _));

            // Plan-time bookkeeping for the "undeclared label" check (`check_undeclared_labels`):
            // unconditional, independent of `force_full`/the As-branch below (an `as()`'s labels
            // must be recorded as produced even with the `path.analysis` kill switch on, which
            // otherwise skips the branch that used to be the only place collecting them), and run
            // on every level this pass ever walks (top level and every nested child, since
            // children are reached recursively through `analyze_nested_paths` above) — see
            // `analyze_query`.
            if let GraphStep::As(_, as_step) = step {
                let labels: &[CompactString] = as_step.step_labels().map_or(&[], |l| &l[..]);
                self.produced.borrow_mut().extend(labels.iter().cloned());
            }
            if let Some((labels, step_name)) = step.required_labels() {
                let mut required = self.required.borrow_mut();
                for label in labels {
                    required.push((label, step_name));
                }
            }
            if let Some(key) = step.declares_side_effect_key() {
                self.produced.borrow_mut().push(CompactString::from(key));
            }
            if MAP_PRODUCING_KINDS.contains(&step.kind_name()) {
                self.map_producing_seen.set(true);
            }

            if self.force_full {
                modes[index] = PathMode::Full;
            } else if let GraphStep::As(_, as_step) = step {
                // `as()` produces labels rather than reading them: it records its position when
                // one of its labels is read later, and upstream of it that label is not needed
                // (the labels themselves were already recorded above, unconditionally).
                let labels: &[CompactString] = as_step.step_labels().map_or(&[], |l| &l[..]);
                modes[index] = need.clone();
                need = need.retract(labels).union(children);
            } else {
                modes[index] = need.clone();
                let skipped = next_is_count && step.skipped_before_count();
                let own = if skipped {
                    PathRequirement::None
                } else {
                    self.restrict(step.path_requirement()).union(children)
                };
                need = if step.resets_path() {
                    own
                } else {
                    need.union(own)
                };
            }
            next_is_count = is_count;
        }
        if let Some((_, err)) = own_rejected_modulator {
            let mut cell = self.rejected_modulator.borrow_mut();
            if cell.is_none() {
                *cell = Some(err);
            }
        }
        (modes, need)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_sets_are_sorted_and_deduplicated() {
        let set = SmallLabelSet::new(["b", "a", "b"]);
        assert_eq!(
            &set.as_arc()[..],
            &[CompactString::new("a"), CompactString::new("b")]
        );
        assert!(set.contains("a"));
        assert!(!set.contains("c"));
    }

    #[test]
    fn union_and_retraction() {
        let a = PathRequirement::labels(["a"]);
        let b = PathRequirement::labels(["b"]);
        let ab = a.clone().union(b);
        assert_eq!(ab, PathRequirement::labels(["a", "b"]));
        assert_eq!(
            ab.clone().union(PathRequirement::Full),
            PathRequirement::Full
        );
        assert_eq!(PathRequirement::None.union(a.clone()), a);
        assert_eq!(ab.retract(&[CompactString::new("b")]), a);
        assert_eq!(a.retract(&[CompactString::new("a")]), PathRequirement::None);
        assert_eq!(
            PathRequirement::Full.retract(&[CompactString::new("a")]),
            PathRequirement::Full
        );
        assert_eq!(
            PathRequirement::labels(Vec::<String>::new()),
            PathRequirement::None
        );
    }

    #[test]
    fn display() {
        assert_eq!(PathRequirement::Full.to_string(), "full");
        assert_eq!(
            PathRequirement::labels(["b", "a"]).to_string(),
            "labels(a,b)"
        );
        assert_eq!(PathRequirement::None.to_string(), "none");
    }

    fn effects_of(
        build: impl for<'a> FnOnce(
            crate::GraphTraversalSource<'a, &'a crate::TraversalGraph>,
        )
            -> crate::GraphTraversalSource<'a, &'a crate::TraversalGraph>,
    ) -> PlanEffects {
        let graph = crate::TraversalGraph::tinkerpop_modern();
        let g = crate::GraphTraversalSource::new(&graph);
        let mut steps = build(g).steps;
        PathAnalysis::analyze_query(true, &mut steps, &std::collections::HashMap::new())
            .unwrap()
            .1
    }

    #[test]
    fn plan_effects_are_collected_from_every_level() {
        let none = PlanEffects::default();
        assert_eq!(effects_of(|g| g.v(None).out(None).count()), none);
        let writes = PlanEffects {
            mutates: false,
            writes_side_effects: true,
        };
        assert_eq!(effects_of(|g| g.v(None).aggregate("x")), writes);
        assert_eq!(effects_of(|g| g.v(None).store("x")), writes);
        assert_eq!(effects_of(|g| g.v(None).subgraph("s")), writes);
        assert_eq!(
            effects_of(|g| g.v(None).side_effect(Box::new(|_, _| {}))),
            writes
        );
        let mutates = PlanEffects {
            mutates: true,
            writes_side_effects: false,
        };
        assert_eq!(effects_of(|g| g.add_v("x")), mutates);
        assert_eq!(effects_of(|g| g.v(None).drop()), mutates);
        // Nested levels count too, and a child's own flags are kept on the child.
        assert_eq!(
            effects_of(|g| g.v(None).union(vec![crate::__::add_v("y")])),
            mutates
        );
        assert_eq!(
            effects_of(|g| g.v(None).where_t(crate::__::out(None).aggregate("x"))),
            writes
        );
    }
}
