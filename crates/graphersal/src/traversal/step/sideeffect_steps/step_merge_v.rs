//! `mergeV()`: TinkerPop's vertex upsert. Finds the vertices matching the merge map (id, label
//! and properties) and emits them, applying `option(Merge.onMatch, ..)` to each; when nothing
//! matches, creates one vertex from the merge map plus `option(Merge.onCreate, ..)`.

use super::merge_map::{
    Lookup, MergeArg, MergeInput, MergeKind, MergeOption, MergeProblem, MergeRole, MergeStep,
    apply_entries, attach_merge_option, check_override, create_vertex, find_vertices,
    first_problem, merge_map, option_map,
};
use super::step_property_map::{PropertyEntry, WriteTarget};
use crate::graph::GraphStorage;
use crate::traversal::optimizer::path_analysis::{PathAnalysis, PathRequirement};
use crate::*;

const KIND: MergeKind = MergeKind::Vertex;

struct StepMergeV<'g, S: GraphStorage + 'g> {
    merge: MergeArg<'g, S>,
    on_create: Option<MergeOption<'g, S>>,
    on_match: Option<MergeOption<'g, S>>,
    /// Whether this is the first step of a `g` traversal: with no incoming traverser it then
    /// runs once, as TinkerPop's start step does. Mid-traversal an empty stream merges nothing.
    start: bool,
    /// Problems found when the step was built, keyed by what they belong to, so a replaced
    /// option drops its own problem. Raised before execution by `plan_check()`.
    merge_problem: Option<MergeProblem>,
    on_create_problem: Option<MergeProblem>,
    on_match_problem: Option<MergeProblem>,
    /// `option(Merge.outV|inV, ..)`, which `mergeV()` does not take.
    option_problem: Option<MergeProblem>,
    /// The time of `defaultFunction` `now`/`today` the script DSL pinned for this step (the
    /// evaluation's `now()`); `None`: the execution's ([`TraverserContext::create_context`]).
    create_time: Option<crate::value::temporal::DateTime>,
}

impl<'g, S: GraphStorage + 'g> Clone for StepMergeV<'g, S> {
    fn clone(&self) -> Self {
        Self {
            create_time: self.create_time,
            merge: self.merge.clone(),
            on_create: self.on_create.clone(),
            on_match: self.on_match.clone(),
            start: self.start,
            merge_problem: self.merge_problem.clone(),
            on_create_problem: self.on_create_problem.clone(),
            on_match_problem: self.on_match_problem.clone(),
            option_problem: self.option_problem.clone(),
        }
    }
}

impl<'g, S: GraphStorage + 'g> fmt::Display for StepMergeV<'g, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "merge_v({})", self.merge)?;
        for (token, option) in [("onCreate", &self.on_create), ("onMatch", &self.on_match)] {
            if let Some(option) = option {
                write!(f, ".option(Merge.{token}, {}", option.arg)?;
                if let Some(cardinality) = option.cardinality {
                    write!(f, ", Cardinality.{cardinality}")?;
                }
                f.write_str(")")?;
            }
        }
        match &self.merge {
            MergeArg::Constant(map) => write!(f, " [lookup: {}]", Lookup::of(map)),
            MergeArg::Traversal(_) => f.write_str(" [lookup: dynamic]"),
        }
    }
}

impl<'g, S: GraphStorage + 'g> StepMergeV<'g, S> {
    fn new(merge: MergeInput<'g, S>, start: bool) -> Self {
        let (merge, merge_problem) = merge.into_arg(KIND, MergeRole::Merge, None);
        Self {
            create_time: None,
            merge,
            on_create: None,
            on_match: None,
            start,
            merge_problem,
            on_create_problem: None,
            on_match_problem: None,
            option_problem: None,
        }
    }

    /// The first build-time problem, in the order the query was written.
    fn problem(&self) -> Option<MergeProblem> {
        first_problem(
            KIND,
            &[
                &self.merge_problem,
                &self.on_create_problem,
                &self.on_match_problem,
                &self.option_problem,
            ],
            &self.merge,
            &self.on_create,
        )
    }
}

impl<'g, S: GraphStorage + 'g> MergeStep<'g, S> for StepMergeV<'g, S> {
    fn set_option(
        &mut self,
        token: Merge,
        input: MergeInput<'g, S>,
        cardinality: Option<Cardinality>,
    ) {
        match token {
            Merge::OnCreate => {
                let (arg, problem) = input.into_arg(KIND, MergeRole::OnCreate, cardinality);
                self.on_create = Some(MergeOption { arg, cardinality });
                self.on_create_problem = problem;
            }
            Merge::OnMatch => {
                let (arg, problem) = input.into_arg(KIND, MergeRole::OnMatch, cardinality);
                self.on_match = Some(MergeOption { arg, cardinality });
                self.on_match_problem = problem;
            }
            Merge::OutV | Merge::InV => {
                self.option_problem = Some(MergeProblem::invalid_at(
                    KIND,
                    if token == Merge::OutV {
                        "option(outV)"
                    } else {
                        "option(inV)"
                    },
                    format!(
                        "merge_v() takes only option(Merge.onCreate, ..) and option(Merge.onMatch, ..); \
                         option(Merge.{token}, ..) resolves an edge vertex of merge_e()"
                    ),
                ));
            }
        }
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepSideEffectMut<'g, S> for StepMergeV<'g, S> {
    fn set_create_time(&mut self, now: crate::value::temporal::DateTime) {
        self.create_time = Some(now);
    }

    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Option<Traverser<S>>,
    ) -> TraverserResult<Vec<Traverser<S>>> {
        if let Some(problem) = self.problem() {
            return Err(problem.into());
        }
        let source = match source {
            Some(source) => source,
            // The start step runs once, on a seed traverser holding null, so a map traversal
            // still sees the side effects (`g.withSideEffect("c", ..).mergeV(__.select("c"))`).
            None if self.start => Traverser::start(
                context,
                TraverserValue::Computed(ElementProperty::Null, None),
            ),
            None => return Ok(Vec::new()),
        };

        let merge = merge_map(&self.merge, context, &source, KIND)?;

        let matches = find_vertices(&*context.storage, &merge)?;
        if !matches.is_empty() {
            let mut output = Vec::with_capacity(matches.len());
            for node in matches {
                let matched = source.split(TraverserValue::Vertex(node));
                // The onMatch traversal receives the matched vertex.
                if let Some(on_match) =
                    option_map(&self.on_match, context, &matched, KIND, MergeRole::OnMatch)?
                {
                    let default = self.on_match.as_ref().and_then(|option| option.cardinality);
                    apply_entries(
                        context.storage,
                        WriteTarget::Vertex(node),
                        &on_match.entries,
                        default,
                        KIND,
                    )?;
                }
                output.push(matched);
            }
            return Ok(output);
        }

        // Nothing matched: create from the merge map plus the onCreate map (the onCreate
        // traversal receives the incoming traverser).
        let created =
            match option_map(&self.on_create, context, &source, KIND, MergeRole::OnCreate)? {
                Some(on_create) => {
                    check_override(KIND, &merge, &on_create)?;
                    merge.inherit(&on_create)
                }
                None => merge.into_owned(),
            };
        let create = context.create_context_or(self.create_time)?;
        let node = create_vertex(context.storage, &created, &create)?;
        Ok(vec![Traverser::with_parts(
            TraverserValue::Vertex(node),
            source.path,
            source.bulk,
            source.sack,
        )])
    }

    fn plan_check(&self) -> TraverserResult<()> {
        match self.problem() {
            Some(problem) => Err(problem.into()),
            None => Ok(()),
        }
    }

    fn child_traversals_mut(&mut self) -> Vec<&mut crate::traversal::AnonymousTraversal<'g, S>> {
        let options = [&mut self.on_create, &mut self.on_match]
            .into_iter()
            .flatten()
            .map(|option| &mut option.arg);
        std::iter::once(&mut self.merge)
            .chain(options)
            .filter_map(|arg| match arg {
                MergeArg::Traversal(traversal) => Some(traversal),
                _ => None,
            })
            .collect()
    }

    fn analyze_nested_paths(
        &mut self,
        analysis: &PathAnalysis,
        _downstream: &PathRequirement,
    ) -> PathRequirement {
        // Every child runs on the incoming traverser (onMatch on the matched vertex, which
        // keeps the incoming path) and only its first result is used.
        let mut need = PathRequirement::None;
        if let MergeArg::Traversal(traversal) = &mut self.merge {
            need = need.union(traversal.analyze_paths(analysis, PathRequirement::None));
        }
        for option in [&mut self.on_create, &mut self.on_match]
            .into_iter()
            .flatten()
        {
            if let MergeArg::Traversal(traversal) = &mut option.arg {
                need = need.union(traversal.analyze_paths(analysis, PathRequirement::None));
            }
        }
        need
    }

    fn as_merge_mut(&mut self) -> Option<&mut dyn MergeStep<'g, S>> {
        Some(self)
    }

    fn clone_box(&self) -> Box<dyn GraphStepSideEffectMut<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

fn merge_v_step<'g, S: GraphStorage + 'g>(
    input: MergeInput<'g, S>,
    start: bool,
) -> GraphStep<'g, S> {
    GraphStep::SideEffectMut("merge_v", Box::new(StepMergeV::new(input, start)))
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// `mergeV(map)`: TinkerPop's vertex upsert. `map` is an [`ElementProperty::Object`] (or
    /// [`ElementProperty::Null`], the same as an empty map) of property keys plus the reserved
    /// keys `"T.id"` and `"T.label"`. Every vertex with that id, label and properties is emitted
    /// (an empty map matches every vertex); when none exists, one vertex is created from the map
    /// plus [`Self::option_merge`]`(Merge::OnCreate, ..)`. Hidden (`~`) keys and other `T.` /
    /// `Direction.` keys fail with [`TraverserError::InvalidMergeMap`] before execution.
    ///
    /// As the first step it runs once; mid-traversal it runs once per incoming traverser.
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let mut graph = TraversalGraph::new();
    /// let mut map = PropertyMap::default();
    /// map.insert("T.label", "person".into());
    /// map.insert("name", "stephen".into());
    /// let map = ElementProperty::Object(Box::new(map));
    /// graph.traversal_mut().merge_v(map.clone()).next().unwrap();
    /// graph.traversal_mut().merge_v(map).next().unwrap(); // matches, creates nothing
    /// assert_eq!(graph.traversal().v(None).count().next().unwrap(), Some(ElementProperty::from(1)));
    /// ```
    pub fn merge_v(self, map: ElementProperty) -> Self {
        let start = self.steps.is_empty();
        self.push_step(merge_v_step(MergeInput::Value(map), start))
    }

    /// `mergeV(traversal)`: [`Self::merge_v`] with the map taken from the first result of
    /// `traversal`, evaluated per incoming traverser (`mergeV()` with no argument is
    /// `merge_v_by(__::identity())`: the incoming traverser is the map).
    pub fn merge_v_by(self, traversal: AnonymousTraversal<'g, S>) -> Self {
        let start = self.steps.is_empty();
        self.push_step(merge_v_step(MergeInput::Traversal(traversal), start))
    }

    /// `option(Merge.onCreate|onMatch, map)` on the preceding [`Self::merge_v`]: the properties
    /// a created vertex gets in addition to the merge map (`OnCreate`; a key in both must have
    /// the same value, else [`TraverserError::MergeOverride`]), or the properties written to
    /// every matched vertex (`OnMatch`; no `T.id`/`T.label`). `Null` or an empty map adds
    /// nothing. After [`Self::merge_e`], `OutV`/`InV` take a vertex search map (`"T.id"`,
    /// `"T.label"`, properties) that resolves the `"Merge.outV"`/`"Merge.inV"` placeholder and
    /// must find exactly one vertex. A second option with the same token replaces the first, as
    /// in TinkerPop. Without a preceding merge step the traversal fails with
    /// [`TraverserError::OptionWithoutBranch`].
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let mut graph = TraversalGraph::tinkerpop_modern();
    /// let mut name = PropertyMap::default();
    /// name.insert("name", "marko".into());
    /// let mut age = PropertyMap::default();
    /// age.insert("age", 30.into());
    /// graph
    ///     .traversal_mut()
    ///     .merge_v(ElementProperty::Object(Box::new(name)))
    ///     .option_merge(Merge::OnMatch, ElementProperty::Object(Box::new(age)))
    ///     .next()
    ///     .unwrap();
    /// assert_eq!(graph.traversal().v("1").values("age").next().unwrap(), Some(ElementProperty::from(30)));
    /// ```
    pub fn option_merge(mut self, token: Merge, map: ElementProperty) -> Self {
        attach_merge_option(&mut self.steps, token, MergeInput::Value(map), None);
        self
    }

    /// `option(Merge.x, traversal)`: [`Self::option_merge`] with the map taken from the first
    /// result of `traversal` — evaluated on the incoming traverser for `OnCreate` and on the
    /// matched element for `OnMatch`; no result adds nothing. For `OutV`/`InV` (after
    /// [`Self::merge_e`]) the traversal runs on the incoming traverser and its first result must
    /// be a vertex or a vertex id, for example `option_merge_by(Merge::OutV, __::select("a"))`.
    pub fn option_merge_by(mut self, token: Merge, traversal: AnonymousTraversal<'g, S>) -> Self {
        attach_merge_option(
            &mut self.steps,
            token,
            MergeInput::Traversal(traversal),
            None,
        );
        self
    }

    /// `option(Merge.x, map, Cardinality)`: [`Self::option_merge`] with a default cardinality
    /// for the map's values. `Cardinality::List`/`Set` fail with
    /// [`TraverserError::UnsupportedCardinality`] when a value would be written.
    pub fn option_merge_with(
        mut self,
        token: Merge,
        map: ElementProperty,
        cardinality: Cardinality,
    ) -> Self {
        attach_merge_option(
            &mut self.steps,
            token,
            MergeInput::Value(map),
            Some(cardinality),
        );
        self
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// `mergeV(map)`; see [`GraphTraversalSource::merge_v`]. Inside a child traversal it runs
    /// once per traverser the child receives.
    pub fn merge_v(self, map: ElementProperty) -> Self {
        self.push_step(merge_v_step(MergeInput::Value(map), false))
    }

    /// `mergeV(traversal)`; see [`GraphTraversalSource::merge_v_by`].
    pub fn merge_v_by(self, traversal: AnonymousTraversal<'g, S>) -> Self {
        self.push_step(merge_v_step(MergeInput::Traversal(traversal), false))
    }

    /// `mergeV(..)` as the script DSL lowers it, for `g` and `__` alike: `start` is
    /// whether it is the first step of the root traversal `g` (it then runs once), which
    /// [`GraphTraversalSource::merge_v`] decides from its own position.
    #[cfg_attr(not(feature = "script"), allow(dead_code))]
    pub(crate) fn merge_v_lowered(self, input: MergeInput<'g, S>, start: bool) -> Self {
        self.push_step(merge_v_step(input, start))
    }

    /// `option(Merge.x, map)`; see [`GraphTraversalSource::option_merge`].
    pub fn option_merge(mut self, token: Merge, map: ElementProperty) -> Self {
        attach_merge_option(&mut self.steps, token, MergeInput::Value(map), None);
        self
    }

    /// `option(Merge.x, traversal)`; see [`GraphTraversalSource::option_merge_by`].
    pub fn option_merge_by(mut self, token: Merge, traversal: AnonymousTraversal<'g, S>) -> Self {
        attach_merge_option(
            &mut self.steps,
            token,
            MergeInput::Traversal(traversal),
            None,
        );
        self
    }

    /// `option(Merge.x, map, Cardinality)`; see [`GraphTraversalSource::option_merge_with`].
    pub fn option_merge_with(
        mut self,
        token: Merge,
        map: ElementProperty,
        cardinality: Cardinality,
    ) -> Self {
        attach_merge_option(
            &mut self.steps,
            token,
            MergeInput::Value(map),
            Some(cardinality),
        );
        self
    }

    /// `option(Merge.x, entries[, Cardinality])` (the DSL).
    #[cfg_attr(not(feature = "script"), allow(dead_code))]
    pub(crate) fn option_merge_entries(
        mut self,
        token: Merge,
        entries: Vec<PropertyEntry>,
        cardinality: Option<Cardinality>,
    ) -> Self {
        attach_merge_option(
            &mut self.steps,
            token,
            MergeInput::Entries(entries),
            cardinality,
        );
        self
    }

    /// `option(Merge.x, traversal, Cardinality)` (the DSL).
    #[cfg_attr(not(feature = "script"), allow(dead_code))]
    pub(crate) fn option_merge_by_with(
        mut self,
        token: Merge,
        traversal: AnonymousTraversal<'g, S>,
        cardinality: Option<Cardinality>,
    ) -> Self {
        attach_merge_option(
            &mut self.steps,
            token,
            MergeInput::Traversal(traversal),
            cardinality,
        );
        self
    }
}

impl __ {
    /// `__.mergeV(map)`; see [`GraphTraversalSource::merge_v`].
    pub fn merge_v<'g, S: GraphStorage + 'g>(map: ElementProperty) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().merge_v(map)
    }

    /// `__.mergeV(traversal)`; see [`GraphTraversalSource::merge_v_by`].
    pub fn merge_v_by<'g, S: GraphStorage + 'g>(
        traversal: AnonymousTraversal<'g, S>,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().merge_v_by(traversal)
    }
}
