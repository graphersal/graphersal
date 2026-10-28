//! `mergeE()`: TinkerPop's edge upsert. Resolves the edge's vertices (`Direction.OUT` /
//! `Direction.IN` ids, or the `Merge.outV` / `Merge.inV` placeholders through
//! `option(Merge.outV|inV, ..)`), finds the edges matching the merge map (id, label, vertices
//! and properties) and emits them, applying `option(Merge.onMatch, ..)` to each; when nothing
//! matches, creates one edge from the merge map plus `option(Merge.onCreate, ..)`.

use super::merge_map::{
    Endpoint, EndpointRef, Lookup, MergeArg, MergeInput, MergeKind, MergeOption, MergeProblem,
    MergeRole, MergeStep, apply_entries, check_override, create_edge, find_edges, find_vertices,
    first_problem, id_text, merge_map, option_map,
};
use super::step_property_map::WriteTarget;
use crate::graph::GraphStorage;
use crate::traversal::optimizer::path_analysis::{PathAnalysis, PathRequirement};
use crate::value::Direction;
use crate::*;

const KIND: MergeKind = MergeKind::Edge;

struct StepMergeE<'g, S: GraphStorage + 'g> {
    merge: MergeArg<'g, S>,
    on_create: Option<MergeOption<'g, S>>,
    on_match: Option<MergeOption<'g, S>>,
    /// `option(Merge.outV, ..)`: a vertex search map or a traversal yielding the vertex.
    out_v: Option<MergeArg<'g, S>>,
    /// `option(Merge.inV, ..)`.
    in_v: Option<MergeArg<'g, S>>,
    /// Whether this is the first step of a `g` traversal: with no incoming traverser it then
    /// runs once, as TinkerPop's start step does. Mid-traversal an empty stream merges nothing.
    start: bool,
    /// Problems found when the step was built, keyed by what they belong to, so a replaced
    /// option drops its own problem. Raised before execution by `plan_check()`.
    merge_problem: Option<MergeProblem>,
    on_create_problem: Option<MergeProblem>,
    on_match_problem: Option<MergeProblem>,
    out_v_problem: Option<MergeProblem>,
    in_v_problem: Option<MergeProblem>,
    /// The time of `defaultFunction` `now`/`today` the script DSL pinned for this step (the
    /// evaluation's `now()`); `None`: the execution's ([`TraverserContext::create_context`]).
    create_time: Option<crate::value::temporal::DateTime>,
}

impl<'g, S: GraphStorage + 'g> Clone for StepMergeE<'g, S> {
    fn clone(&self) -> Self {
        Self {
            create_time: self.create_time,
            merge: self.merge.clone(),
            on_create: self.on_create.clone(),
            on_match: self.on_match.clone(),
            out_v: self.out_v.clone(),
            in_v: self.in_v.clone(),
            start: self.start,
            merge_problem: self.merge_problem.clone(),
            on_create_problem: self.on_create_problem.clone(),
            on_match_problem: self.on_match_problem.clone(),
            out_v_problem: self.out_v_problem.clone(),
            in_v_problem: self.in_v_problem.clone(),
        }
    }
}

impl<'g, S: GraphStorage + 'g> fmt::Display for StepMergeE<'g, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "merge_e({})", self.merge)?;
        for (token, option) in [("onCreate", &self.on_create), ("onMatch", &self.on_match)] {
            if let Some(option) = option {
                write!(f, ".option(Merge.{token}, {}", option.arg)?;
                if let Some(cardinality) = option.cardinality {
                    write!(f, ", Cardinality.{cardinality}")?;
                }
                f.write_str(")")?;
            }
        }
        for (token, option) in [("outV", &self.out_v), ("inV", &self.in_v)] {
            if let Some(arg) = option {
                write!(f, ".option(Merge.{token}, {arg})")?;
            }
        }
        match &self.merge {
            MergeArg::Constant(map) => write!(f, " [lookup: {}]", Lookup::of_edge(map)),
            MergeArg::Traversal(_) => f.write_str(" [lookup: dynamic]"),
        }
    }
}

/// A [`TraverserError::MergeVertexUnresolved`] for the `direction` end.
fn unresolved(direction: Direction, reason: impl Into<String>) -> BoxedTraverserError {
    Box::new(TraverserError::MergeVertexUnresolved {
        direction,
        reason: reason.into(),
    })
}

/// `Out` / `In`, as TinkerPop's messages spell the edge's ends.
fn end_name(direction: Direction) -> &'static str {
    match direction {
        Direction::In => "In",
        Direction::Out | Direction::Both => "Out",
    }
}

/// The option token that resolves the `direction` placeholder.
fn token_name(direction: Direction) -> &'static str {
    match direction {
        Direction::In => "inV",
        Direction::Out | Direction::Both => "outV",
    }
}

impl<'g, S: GraphStorage + 'g> StepMergeE<'g, S> {
    fn new(merge: MergeInput<'g, S>, start: bool) -> Self {
        let (merge, merge_problem) = merge.into_arg(KIND, MergeRole::Merge, None);
        Self {
            create_time: None,
            merge,
            on_create: None,
            on_match: None,
            out_v: None,
            in_v: None,
            start,
            merge_problem,
            on_create_problem: None,
            on_match_problem: None,
            out_v_problem: None,
            in_v_problem: None,
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
                &self.out_v_problem,
                &self.in_v_problem,
            ],
            &self.merge,
            &self.on_create,
        )
    }

    /// Resolves one endpoint of a merge or onCreate map: an id to its vertex (or
    /// [`Endpoint::Unknown`]), a placeholder through `option(Merge.outV|inV, ..)` evaluated on
    /// `source`.
    fn resolve(
        &self,
        endpoint: &Option<EndpointRef>,
        direction: Direction,
        context: &mut TraverserContext<'_, S>,
        source: &Traverser<S>,
    ) -> TraverserResult<Option<Endpoint<S::VertexId>>> {
        let token = token_name(direction);
        match endpoint {
            None => Ok(None),
            Some(EndpointRef::Id(id)) => Ok(Some(Self::vertex_by_id(context, id)?)),
            Some(EndpointRef::Placeholder) => {
                let option = match direction {
                    Direction::In => &self.in_v,
                    Direction::Out | Direction::Both => &self.out_v,
                };
                match option {
                    None => Err(unresolved(
                        direction,
                        format!(
                            "{} Vertex not specified: the map names Merge.{token} but there is no option(Merge.{token}, ..)",
                            end_name(direction)
                        ),
                    )),
                    Some(MergeArg::Constant(map)) => {
                        let found = find_vertices(&*context.storage, map)?;
                        match found.as_slice() {
                            [node] => Ok(Some(Endpoint::Vertex(*node))),
                            [] => Err(unresolved(
                                direction,
                                format!(
                                    "Vertex id could not be resolved from mergeE: option(Merge.{token}, {map}) matched no vertex"
                                ),
                            )),
                            many => Err(unresolved(
                                direction,
                                format!(
                                    "option(Merge.{token}, {map}) matched {} vertices; it must identify exactly one vertex",
                                    many.len()
                                ),
                            )),
                        }
                    }
                    Some(MergeArg::Traversal(traversal)) => {
                        let Some(first) = traversal.first(context, source)? else {
                            return Err(unresolved(
                                direction,
                                format!(
                                    "Vertex id could not be resolved from mergeE: option(Merge.{token}, {traversal}) produced no vertex"
                                ),
                            ));
                        };
                        Self::vertex_of(context, first.value, direction)
                    }
                }
            }
        }
    }

    /// The vertex with id `id`, or [`Endpoint::Unknown`].
    fn vertex_by_id(
        context: &TraverserContext<'_, S>,
        id: &str,
    ) -> Result<Endpoint<S::VertexId>, GraphError> {
        Ok(match context.storage.get_vertex_by_id(id)? {
            Some(node) => Endpoint::Vertex(node),
            None => Endpoint::Unknown(CompactString::from(id)),
        })
    }

    /// The vertex an `option(Merge.outV|inV, traversal)` result names: a vertex or a vertex id.
    fn vertex_of(
        context: &mut TraverserContext<'_, S>,
        value: TraverserValue<S>,
        direction: Direction,
    ) -> TraverserResult<Option<Endpoint<S::VertexId>>> {
        if let TraverserValue::Vertex(node) = value {
            return Ok(Some(Endpoint::Vertex(node)));
        }
        let value = value.into_node_property(context.storage)?;
        let id = match &value {
            ElementProperty::Vertex(vertex) => Some(CompactString::new(vertex.id())),
            other => id_text(other),
        };
        match id {
            Some(id) => Ok(Some(Self::vertex_by_id(context, &id)?)),
            None => Err(unresolved(
                direction,
                format!(
                    "option(Merge.{}, ..) must yield a vertex or a vertex id, got {}",
                    token_name(direction),
                    crate::graph::schema::value_type_name(&value)
                ),
            )),
        }
    }
}

impl<'g, S: GraphStorage + 'g> MergeStep<'g, S> for StepMergeE<'g, S> {
    fn set_option(
        &mut self,
        token: Merge,
        input: MergeInput<'g, S>,
        cardinality: Option<Cardinality>,
    ) {
        let role = match token {
            Merge::OnCreate => MergeRole::OnCreate,
            Merge::OnMatch => MergeRole::OnMatch,
            Merge::OutV | Merge::InV => MergeRole::VertexLookup,
        };
        let (arg, mut problem) = input.into_arg(KIND, role, cardinality);
        // Edge properties have no cardinality, whatever form the option takes.
        if let (None, Some(cardinality)) = (&problem, cardinality) {
            problem = Some(MergeProblem::EdgeCardinality {
                kind: KIND,
                cardinality,
            });
        }
        match token {
            Merge::OnCreate => {
                self.on_create = Some(MergeOption { arg, cardinality });
                self.on_create_problem = problem;
            }
            Merge::OnMatch => {
                self.on_match = Some(MergeOption { arg, cardinality });
                self.on_match_problem = problem;
            }
            Merge::OutV => {
                self.out_v = Some(arg);
                self.out_v_problem = problem;
            }
            Merge::InV => {
                self.in_v = Some(arg);
                self.in_v_problem = problem;
            }
        }
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepSideEffectMut<'g, S> for StepMergeE<'g, S> {
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
            // still sees the side effects (`g.withSideEffect("a", ..).mergeE(__.select("a"))`).
            None if self.start => Traverser::start(
                context,
                TraverserValue::Computed(ElementProperty::Null, None),
            ),
            None => return Ok(Vec::new()),
        };

        let merge = merge_map(&self.merge, context, &source, KIND)?;
        let out_v = self.resolve(&merge.out_v, Direction::Out, context, &source)?;
        let in_v = self.resolve(&merge.in_v, Direction::In, context, &source)?;

        let matches = find_edges(&*context.storage, &merge, out_v.as_ref(), in_v.as_ref())?;
        if !matches.is_empty() {
            let mut output = Vec::with_capacity(matches.len());
            for edge in matches {
                let matched = Traverser::with_parts(
                    TraverserValue::Edge(edge),
                    source.path,
                    source.bulk,
                    source.sack,
                );
                // The onMatch traversal receives the matched edge.
                if let Some(on_match) =
                    option_map(&self.on_match, context, &matched, KIND, MergeRole::OnMatch)?
                {
                    apply_entries(
                        context.storage,
                        WriteTarget::Edge(edge),
                        &on_match.entries,
                        None,
                        KIND,
                    )?;
                }
                output.push(matched);
            }
            return Ok(output);
        }

        // Nothing matched: create from the merge map plus the onCreate map (the onCreate
        // traversal receives the incoming traverser). Endpoints the merge map names are
        // already resolved; inherited ones are resolved now.
        let on_create = option_map(&self.on_create, context, &source, KIND, MergeRole::OnCreate)?;
        let created = match &on_create {
            Some(on_create) => {
                check_override(KIND, &merge, on_create)?;
                merge.inherit(on_create)
            }
            None => merge.into_owned(),
        };
        let out_v = match out_v {
            Some(endpoint) => Some(endpoint),
            None => self.resolve(&created.out_v, Direction::Out, context, &source)?,
        };
        let in_v = match in_v {
            Some(endpoint) => Some(endpoint),
            None => self.resolve(&created.in_v, Direction::In, context, &source)?,
        };
        let out_v = Self::require(out_v, Direction::Out)?;
        let in_v = Self::require(in_v, Direction::In)?;
        let create = context.create_context_or(self.create_time)?;
        let edge = create_edge(context.storage, &created, out_v, in_v, &create)?;
        Ok(vec![Traverser::with_parts(
            TraverserValue::Edge(edge),
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
        self.child_args()
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
        // Every child runs on the incoming traverser (onMatch on the matched edge, which keeps
        // the incoming path) and only its first result is used.
        let mut need = PathRequirement::None;
        for arg in self.child_args() {
            if let MergeArg::Traversal(traversal) = arg {
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

impl<'g, S: GraphStorage + 'g> StepMergeE<'g, S> {
    /// The argument and every option, for the child-traversal hooks.
    fn child_args(&mut self) -> impl Iterator<Item = &mut MergeArg<'g, S>> {
        let options = [&mut self.on_create, &mut self.on_match]
            .into_iter()
            .flatten()
            .map(|option| &mut option.arg);
        std::iter::once(&mut self.merge)
            .chain(options)
            .chain(self.out_v.as_mut())
            .chain(self.in_v.as_mut())
    }

    /// The vertex an edge about to be created needs at its `direction` end.
    fn require(
        endpoint: Option<Endpoint<S::VertexId>>,
        direction: Direction,
    ) -> TraverserResult<S::VertexId> {
        match endpoint {
            Some(Endpoint::Vertex(node)) => Ok(node),
            Some(Endpoint::Unknown(id)) => Err(unresolved(
                direction,
                format!("Vertex id could not be resolved from mergeE: {id}"),
            )),
            None => Err(unresolved(
                direction,
                format!("{} Vertex not specified", end_name(direction)),
            )),
        }
    }
}

fn merge_e_step<'g, S: GraphStorage + 'g>(
    input: MergeInput<'g, S>,
    start: bool,
) -> GraphStep<'g, S> {
    GraphStep::SideEffectMut("merge_e", Box::new(StepMergeE::new(input, start)))
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// `mergeE(map)`: TinkerPop's edge upsert. `map` is an [`ElementProperty::Object`] (or
    /// [`ElementProperty::Null`], the same as an empty map) of property keys plus the reserved
    /// keys `"T.id"`, `"T.label"`, `"Direction.OUT"` and `"Direction.IN"`. A `Direction` key takes
    /// a vertex id or the placeholder `"Merge.outV"` / `"Merge.inV"`, which
    /// [`Self::option_merge`]`(Merge::OutV|InV, ..)` resolves (a vertex search map) or
    /// [`Self::option_merge_by`] (a traversal yielding the vertex). Every edge with that id,
    /// label, vertices and properties is emitted (an empty map matches every edge); when none
    /// exists, one edge is created from the map plus `option_merge(Merge::OnCreate, ..)`, which
    /// needs both vertices ([`TraverserError::MergeVertexUnresolved`] otherwise).
    ///
    /// As the first step it runs once; mid-traversal it runs once per incoming traverser, which
    /// is not an implicit endpoint.
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let mut graph = TraversalGraph::tinkerpop_modern();
    /// let mut map = PropertyMap::default();
    /// map.insert("T.label", "knows".into());
    /// map.insert("Direction.OUT", "1".into());
    /// map.insert("Direction.IN", "3".into());
    /// let map = ElementProperty::Object(Box::new(map));
    /// graph.traversal_mut().merge_e(map.clone()).next().unwrap(); // creates 1 -knows-> 3
    /// graph.traversal_mut().merge_e(map).next().unwrap(); // matches, creates nothing
    /// assert_eq!(graph.traversal().e(None).count().next().unwrap(), Some(ElementProperty::from(7)));
    /// ```
    pub fn merge_e(self, map: ElementProperty) -> Self {
        let start = self.steps.is_empty();
        self.push_step(merge_e_step(MergeInput::Value(map), start))
    }

    /// `mergeE(traversal)`: [`Self::merge_e`] with the map taken from the first result of
    /// `traversal`, evaluated per incoming traverser (`mergeE()` with no argument is
    /// `merge_e_by(__::identity())`: the incoming traverser is the map).
    pub fn merge_e_by(self, traversal: AnonymousTraversal<'g, S>) -> Self {
        let start = self.steps.is_empty();
        self.push_step(merge_e_step(MergeInput::Traversal(traversal), start))
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// `mergeE(map)`; see [`GraphTraversalSource::merge_e`]. Inside a child traversal it runs
    /// once per traverser the child receives.
    pub fn merge_e(self, map: ElementProperty) -> Self {
        self.push_step(merge_e_step(MergeInput::Value(map), false))
    }

    /// `mergeE(traversal)`; see [`GraphTraversalSource::merge_e_by`].
    pub fn merge_e_by(self, traversal: AnonymousTraversal<'g, S>) -> Self {
        self.push_step(merge_e_step(MergeInput::Traversal(traversal), false))
    }

    /// `mergeE(..)` as the script DSL lowers it, for `g` and `__` alike: `start` is
    /// whether it is the first step of the root traversal `g` (it then runs once), which
    /// [`GraphTraversalSource::merge_e`] decides from its own position.
    #[cfg_attr(not(feature = "script"), allow(dead_code))]
    pub(crate) fn merge_e_lowered(self, input: MergeInput<'g, S>, start: bool) -> Self {
        self.push_step(merge_e_step(input, start))
    }
}

impl __ {
    /// `__.mergeE(map)`; see [`GraphTraversalSource::merge_e`].
    pub fn merge_e<'g, S: GraphStorage + 'g>(map: ElementProperty) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().merge_e(map)
    }

    /// `__.mergeE(traversal)`; see [`GraphTraversalSource::merge_e_by`].
    pub fn merge_e_by<'g, S: GraphStorage + 'g>(
        traversal: AnonymousTraversal<'g, S>,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().merge_e_by(traversal)
    }
}
