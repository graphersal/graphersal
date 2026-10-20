use core::fmt;

use crate::error::TraverserResult;
use crate::graph::{GraphNodeType, GraphStorage};
use crate::traversal::{
    AnonymousTraversal, ArgsModulation, By, ByModulation, Traverser, TraverserContext,
};

pub(crate) mod profile;
pub(crate) use profile::*;

pub mod append_steps;
pub mod barrier_steps;
pub mod filter_steps;
pub mod flatbarrier_steps;
pub mod flatmap_steps;
pub(crate) mod local_scope;
pub mod map_steps;
pub mod modulator_steps;
pub(crate) mod order_clauses;
pub(crate) mod path_window;
pub(crate) mod prop_path;
pub mod sideeffect_steps;
pub mod start_steps;

/// What the plan as written states about the traversers reaching a step, for the requests of
/// a write of the received elements ([`GraphStep::for_each_access_in`]): their element kind
/// and labels when known. The default states nothing. Labels are stated only where they are
/// complete for each element: an edge's one label, the labels `add_v(labels)` gives a new
/// vertex ([`GraphStep::access_flow_after`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct AccessFlow {
    /// The element kind, when known.
    pub(crate) element: Option<crate::graph::ElementKind>,
    /// Every label the elements can carry (an element carries no other); a write asks once per
    /// label. Empty when unknown.
    pub(crate) labels: Vec<String>,
}

pub(crate) enum GraphStep<'g, S: GraphStorage + 'g> {
    Init(&'static str, Box<dyn GraphStepInit<'g, S> + 'g>),
    Append(&'static str, Box<dyn GraphStepAppend<'g, S> + 'g>),
    Map(&'static str, Box<dyn GraphStepMap<'g, S> + 'g>),
    FlatMap(&'static str, Box<dyn GraphStepFlatMap<'g, S> + 'g>),
    Barrier(&'static str, Box<dyn GraphStepBarrier<'g, S> + 'g>),
    FlatBarrier(&'static str, Box<dyn GraphStepFlatBarrier<'g, S> + 'g>),
    Filter(&'static str, Box<dyn GraphStepFilter<'g, S> + 'g>),
    As(&'static str, Box<dyn GraphStepAs<'g, S> + 'g>),
    SideEffect(&'static str, Box<dyn GraphStepSideEffect<'g, S> + 'g>),
    SideEffectMut(&'static str, Box<dyn GraphStepSideEffectMut<'g, S> + 'g>),
    Modulator(&'static str, Box<dyn GraphStepModulator<'g, S> + 'g>),
    Count(&'static str, Box<dyn GraphStepCount<'g, S> + 'g>),
}

impl<'g, S: GraphStorage + 'g> Clone for GraphStep<'g, S> {
    fn clone(&self) -> Self {
        match self {
            GraphStep::Init(n, s) => GraphStep::Init(n, s.clone_box()),
            GraphStep::Append(n, s) => GraphStep::Append(n, s.clone_box()),
            GraphStep::Map(n, s) => GraphStep::Map(n, s.clone_box()),
            GraphStep::FlatMap(n, s) => GraphStep::FlatMap(n, s.clone_box()),
            GraphStep::Barrier(n, s) => GraphStep::Barrier(n, s.clone_box()),
            GraphStep::FlatBarrier(n, s) => GraphStep::FlatBarrier(n, s.clone_box()),
            GraphStep::Filter(n, s) => GraphStep::Filter(n, s.clone_box()),
            GraphStep::As(n, s) => GraphStep::As(n, s.clone_box()),
            GraphStep::SideEffect(n, s) => GraphStep::SideEffect(n, s.clone_box()),
            GraphStep::SideEffectMut(n, s) => GraphStep::SideEffectMut(n, s.clone_box()),
            GraphStep::Modulator(n, s) => GraphStep::Modulator(n, s.clone_box()),
            GraphStep::Count(n, s) => GraphStep::Count(n, s.clone_box()),
        }
    }
}

impl<'g, S: GraphStorage + 'g> GraphStep<'g, S> {
    /// The canonical rendering of this step: ONE text that reads like the DSL call building it,
    /// in snake_case (`has_label("person")`, `where(__.out())`, `order().by("age", Order.desc)`),
    /// fused and optimizer-inserted steps under the name the profile documents
    /// (`v(labels: ["person"]).count()`, `lazy_barrier()`). It is what `.profile()`, error step
    /// locations and the `help()` rewrites of the failing query show; the camelCase spelling is
    /// derived from it (`traversal::spelling::respell`). Never `Debug` (rule 14).
    pub fn get_name(&self) -> String {
        match self {
            GraphStep::Init(_, step) => step.get_name(),
            GraphStep::Append(_, step) => step.get_name(),
            GraphStep::Map(_, step) => step.get_name(),
            GraphStep::FlatMap(_, step) => step.get_name(),
            GraphStep::Barrier(_, step) => step.get_name(),
            GraphStep::FlatBarrier(_, step) => step.get_name(),
            GraphStep::Filter(_, step) => step.get_name(),
            GraphStep::As(_, step) => step.get_name(),
            GraphStep::SideEffect(_, step) => step.get_name(),
            GraphStep::SideEffectMut(_, step) => step.get_name(),
            GraphStep::Modulator(_, step) => step.to_string(),
            GraphStep::Count(_, step) => step.to_string(),
        }
    }

    /// The child traversals this step holds, in their canonical order. This is the single
    /// definition of the child numbering behind step ids (`Metrics::id` of `.profile()` and the
    /// location of an execution error, `<step>.<child>.<step>...`): the step's own traversal
    /// arguments first, in argument order (`union()`/`coalesce()`/`and()`/`or()` branches, the
    /// `where()`/`not()`/`filter()`/`local()`/`optional()` body, the `repeat()` body, the
    /// `choose()` selector), then its modulator children in attachment order (`by()`,
    /// `option()` keys and branches, `until()`/`emit()`). The optimizer reaches every child
    /// through it too ([`Self::optimize_nested`]), so a child is never optimized but unnumbered.
    pub(crate) fn child_traversals_mut(&mut self) -> Vec<&mut AnonymousTraversal<'g, S>> {
        match self {
            GraphStep::Init(_, step) => step.child_traversals_mut(),
            GraphStep::Append(_, step) => step.child_traversals_mut(),
            GraphStep::Map(_, step) => step.child_traversals_mut(),
            GraphStep::FlatMap(_, step) => step.child_traversals_mut(),
            GraphStep::Barrier(_, step) => step.child_traversals_mut(),
            GraphStep::FlatBarrier(_, step) => step.child_traversals_mut(),
            GraphStep::Filter(_, step) => step.child_traversals_mut(),
            GraphStep::As(_, step) => step.child_traversals_mut(),
            GraphStep::SideEffect(_, step) => step.child_traversals_mut(),
            GraphStep::SideEffectMut(_, step) => step.child_traversals_mut(),
            GraphStep::Modulator(_, step) => step.child_traversals_mut(),
            GraphStep::Count(_, step) => step.child_traversals_mut(),
        }
    }

    /// Optimizes every child traversal of this step (see [`Self::child_traversals_mut`]).
    pub(crate) fn optimize_nested(
        &mut self,
        optimizer: &crate::traversal::optimizer::TraversalOptimizer<'g, S>,
    ) {
        for child in self.child_traversals_mut() {
            child.optimize(optimizer);
        }
    }

    /// Stores in every child traversal of every step of `steps` (recursively) its position among
    /// the children of the step holding it, the `<child>` segment of a step id. Runs once per
    /// execution on the optimized plan, before any step executes.
    pub(crate) fn number_children(steps: &mut [GraphStep<'g, S>]) {
        for step in steps {
            for (index, child) in step.child_traversals_mut().into_iter().enumerate() {
                child.child_index = index;
                Self::number_children(&mut child.steps);
            }
        }
    }
}

impl<'g, S: GraphStorage + 'g> GraphStep<'g, S> {
    /// Hands `from()`/`to()` arguments to this step; see [`ArgsModulation`]. Categories without
    /// the hook never take one.
    pub(crate) fn push_modulator_args(
        &mut self,
        args: Vec<String>,
        modulator_name: &str,
    ) -> ArgsModulation {
        match self {
            GraphStep::Init(_, step) => step.push_modulator_args(args, modulator_name),
            GraphStep::Map(_, step) => step.push_modulator_args(args, modulator_name),
            GraphStep::FlatMap(_, step) => step.push_modulator_args(args, modulator_name),
            GraphStep::Barrier(_, step) => step.push_modulator_args(args, modulator_name),
            GraphStep::FlatBarrier(_, step) => step.push_modulator_args(args, modulator_name),
            GraphStep::Filter(_, step) => step.push_modulator_args(args, modulator_name),
            GraphStep::SideEffect(_, step) => step.push_modulator_args(args, modulator_name),
            GraphStep::SideEffectMut(_, step) => step.push_modulator_args(args, modulator_name),
            GraphStep::Append(_, _)
            | GraphStep::As(_, _)
            | GraphStep::Modulator(_, _)
            | GraphStep::Count(_, _) => ArgsModulation::Unsupported,
        }
    }

    /// Hands a `from(__...)`/`to(__...)` child traversal to this step; see [`ArgsModulation`].
    /// Only mutating steps (`add_e()`) take one.
    pub(crate) fn push_modulator_traversal(
        &mut self,
        child: AnonymousTraversal<'g, S>,
        modulator_name: &str,
    ) -> ArgsModulation {
        match self {
            GraphStep::SideEffectMut(_, step) => {
                step.push_modulator_traversal(child, modulator_name)
            }
            _ => ArgsModulation::Unsupported,
        }
    }

    /// Fails when this is a rejected-modulator marker (`StepModulatorRejected`), a mutating
    /// step whose build-time problem `plan_check()` reports (an invalid constant `mergeV()` map),
    /// or a map step whose `plan_check()` reports one (a `math()` equation that does not parse);
    /// every other step is unaffected. Used by the plan-time validation pass
    /// (`optimizer::path_analysis::PathAnalysis`) to find such a problem before execution, even
    /// inside a child traversal that never runs.
    pub(crate) fn modulator_check(&self) -> TraverserResult<()> {
        match self {
            GraphStep::Modulator(_, step) => step.check(),
            GraphStep::SideEffectMut(_, step) => step.plan_check(),
            GraphStep::Map(_, step) => step.plan_check(),
            GraphStep::Filter(_, step) => step.plan_check(),
            GraphStep::Init(_, _)
            | GraphStep::Append(_, _)
            | GraphStep::FlatMap(_, _)
            | GraphStep::Barrier(_, _)
            | GraphStep::FlatBarrier(_, _)
            | GraphStep::As(_, _)
            | GraphStep::SideEffect(_, _)
            | GraphStep::Count(_, _) => Ok(()),
        }
    }

    /// Hands a `by()` modulator to this step; see [`ByModulation`]. Categories without the hook
    /// never take one.
    pub(crate) fn push_modulator_by(
        &mut self,
        by: By<'g, S>,
        modulator_name: &str,
    ) -> ByModulation {
        match self {
            GraphStep::Init(_, step) => step.push_modulator_by(by, modulator_name),
            GraphStep::Map(_, step) => step.push_modulator_by(by, modulator_name),
            GraphStep::FlatMap(_, step) => step.push_modulator_by(by, modulator_name),
            GraphStep::Barrier(_, step) => step.push_modulator_by(by, modulator_name),
            GraphStep::FlatBarrier(_, step) => step.push_modulator_by(by, modulator_name),
            GraphStep::Filter(_, step) => step.push_modulator_by(by, modulator_name),
            GraphStep::SideEffect(_, step) => step.push_modulator_by(by, modulator_name),
            GraphStep::SideEffectMut(_, step) => step.push_modulator_by(by, modulator_name),
            GraphStep::Append(_, _)
            | GraphStep::As(_, _)
            | GraphStep::Modulator(_, _)
            | GraphStep::Count(_, _) => ByModulation::Unsupported,
        }
    }

    /// Hands a step-level `with(WithOptions..)` option to this step: `None` when the step took
    /// it, otherwise the `InvalidModulator` reason. Only `valueMap()` and `index()` take one.
    pub(crate) fn push_with_option(
        &mut self,
        key: crate::value::WithOptions,
        value: Option<crate::value::WithOptions>,
    ) -> Option<String> {
        match self {
            GraphStep::Map(_, step) => step.push_with_option(key, value),
            GraphStep::Init(_, _)
            | GraphStep::FlatMap(_, _)
            | GraphStep::Barrier(_, _)
            | GraphStep::FlatBarrier(_, _)
            | GraphStep::Filter(_, _)
            | GraphStep::SideEffect(_, _)
            | GraphStep::SideEffectMut(_, _)
            | GraphStep::Append(_, _)
            | GraphStep::As(_, _)
            | GraphStep::Modulator(_, _)
            | GraphStep::Count(_, _) => {
                Some(crate::traversal::with_reason::NOT_ACCEPTED.to_string())
            }
        }
    }

    /// What this step reads from the path of its incoming traversers.
    pub(crate) fn path_requirement(
        &self,
    ) -> crate::traversal::optimizer::path_analysis::PathRequirement {
        match self {
            GraphStep::Init(_, step) => step.path_requirement(),
            GraphStep::Append(_, step) => step.path_requirement(),
            GraphStep::Map(_, step) => step.path_requirement(),
            GraphStep::FlatMap(_, step) => step.path_requirement(),
            GraphStep::Barrier(_, step) => step.path_requirement(),
            GraphStep::FlatBarrier(_, step) => step.path_requirement(),
            GraphStep::Filter(_, step) => step.path_requirement(),
            GraphStep::As(_, step) => step.path_requirement(),
            GraphStep::SideEffect(_, step) => step.path_requirement(),
            GraphStep::SideEffectMut(_, step) => step.path_requirement(),
            GraphStep::Modulator(_, step) => step.path_requirement(),
            GraphStep::Count(_, step) => step.path_requirement(),
        }
    }

    /// The step labels whose every occurrence this step reads; see
    /// [`GraphStepFilter::history_labels`]. Only internal GQL steps report any, so it exists only
    /// with the `gql` feature.
    #[cfg(feature = "gql")]
    pub(crate) fn history_labels(&self) -> &[compact_str::CompactString] {
        match self {
            GraphStep::Map(_, step) => step.history_labels(),
            GraphStep::FlatMap(_, step) => step.history_labels(),
            GraphStep::FlatBarrier(_, step) => step.history_labels(),
            GraphStep::Filter(_, step) => step.history_labels(),
            GraphStep::Init(_, _)
            | GraphStep::Append(_, _)
            | GraphStep::Barrier(_, _)
            | GraphStep::As(_, _)
            | GraphStep::SideEffect(_, _)
            | GraphStep::SideEffectMut(_, _)
            | GraphStep::Modulator(_, _)
            | GraphStep::Count(_, _) => &[],
        }
    }

    /// Labels this step requires to resolve, declared somewhere in the query, together with the
    /// step's own name for the diagnostic; see `GraphStepMap::required_labels`. Only `Map` steps
    /// (`select()`) report anything today.
    pub(crate) fn required_labels(
        &self,
    ) -> Option<(Vec<compact_str::CompactString>, &'static str)> {
        match self {
            GraphStep::Map(name, step) => step.required_labels().map(|labels| (labels, *name)),
            GraphStep::Init(_, _)
            | GraphStep::Append(_, _)
            | GraphStep::FlatMap(_, _)
            | GraphStep::Barrier(_, _)
            | GraphStep::FlatBarrier(_, _)
            | GraphStep::Filter(_, _)
            | GraphStep::As(_, _)
            | GraphStep::SideEffect(_, _)
            | GraphStep::SideEffectMut(_, _)
            | GraphStep::Modulator(_, _)
            | GraphStep::Count(_, _) => None,
        }
    }

    /// The side-effect key this step declares (`aggregate`/`store`/`tree`), if any; see
    /// `GraphStepSideEffect::declares_side_effect_key`/`GraphStepBarrier::declares_side_effect_key`.
    pub(crate) fn declares_side_effect_key(&self) -> Option<&str> {
        match self {
            GraphStep::SideEffect(_, step) => step.declares_side_effect_key(),
            GraphStep::Barrier(_, step) => step.declares_side_effect_key(),
            GraphStep::FlatBarrier(_, step) => step.declares_side_effect_key(),
            GraphStep::Init(_, _)
            | GraphStep::Append(_, _)
            | GraphStep::Map(_, _)
            | GraphStep::FlatMap(_, _)
            | GraphStep::Filter(_, _)
            | GraphStep::As(_, _)
            | GraphStep::SideEffectMut(_, _)
            | GraphStep::Modulator(_, _)
            | GraphStep::Count(_, _) => None,
        }
    }

    /// What running this step itself does besides reading the graph, its child traversals not
    /// included (see [`Self::has_effects`] for the whole subtree): it mutates the graph
    /// (`SideEffectMut`, `drop()`), or writes a side effect (`SideEffect` — `aggregate`, `store`,
    /// `subgraph`, `sideEffect(fn)`, `profile` — or a step declaring a side-effect key, such as
    /// `tree("x")`). The single classification behind both the path analysis' `PlanEffects` and
    /// the optimizer rules that must not move effectful steps.
    pub(crate) fn own_effects(&self) -> crate::traversal::optimizer::path_analysis::PlanEffects {
        let mut effects = crate::traversal::optimizer::path_analysis::PlanEffects::default();
        if self.mutates() {
            effects.mutates = true;
        } else if matches!(self, GraphStep::SideEffect(_, _))
            || self.declares_side_effect_key().is_some()
        {
            effects.writes_side_effects = true;
        }
        effects
    }

    /// Calls `visit` with every [`AccessRequest`](crate::graph::AccessRequest) running this
    /// step itself makes, its child traversals not included, and stops at the first error. The
    /// single step -> request mapping: the authorization check (`permission_check.rs`), the
    /// path analysis' `mutates` ([`Self::own_effects`], via [`Self::mutates`]) and the script
    /// DSL's `is_mutating()` all derive from it.
    ///
    /// - `add_v(labels)` / `add_e(label)`: `Create` on `Data { Vertex | Edge, label }`, one
    ///   request per statically known label (`None` when the label comes from a traversal or is
    ///   absent);
    /// - `merge_v` / `merge_e`: `Create` and `Update` on `Data { Vertex | Edge, None }`;
    /// - `property`, `property_json`, `remove_property`: `Update` on `Data` of the elements the
    ///   step receives as far as the plan states them (`flow`, [`AccessFlow`]): one request per
    ///   known label, `Data { None, None }` when nothing is known;
    /// - `add_label` / `drop_label`: `Update` on `Data { Vertex, label }`; `set_label`: `Update`
    ///   on `Data { Edge, label }`, the labels again from `flow`;
    /// - `drop()`: `Delete` on `Data` of the elements it receives, like `property`;
    /// - `schema()`: `Read` on `Schema`;
    /// - `v()` / `e()`: `Read` on `Data { Vertex | Edge, None }`; `out`/`in`/`both`: `Read` on
    ///   edges and on vertices; `out_e`/`in_e`/`both_e`: `Read` on edges;
    ///   `out_v`/`in_v`/`both_v`/`other_v`: `Read` on vertices; `subgraph()`: `Read` on
    ///   vertices (it copies the endpoints of the edges it receives);
    /// - every other step: nothing. Only steps that reach graph data (sources, adjacency) ask
    ///   for a read; a value-only step (`has`, `values`, `limit`, `count`, `inject`, `math`,
    ///   modulators, ...) works on what earlier steps produced, so `g.inject(1, 2).sum()` needs
    ///   no permission. An element handle held from an earlier, allowed run is read without a
    ///   further request (`g.inject(v).values("name")`). The internal GQL steps (`gql:filter`,
    ///   `gql:project`, `gql:value`, `gql:order`, `gql:aggregate`, `gql:distinct`, `gql:bind`,
    ///   `gql:for`, `gql:trail`, `gql:not_self_loop`, the `gql:error` marker) are value-only too:
    ///   they read the
    ///   properties and labels of the elements the lowered `v()` / adjacency steps reached (and
    ///   asked `Read` for), like the `has()` / `values()` they stand for, so a GQL query asks
    ///   exactly what its Gremlin equivalent asks. `gql:lookup` reaches vertices of the graph
    ///   (a component found by id or index): `Read` on vertices, like the `v()` it replaces.
    ///   A subquery (`EXISTS`, a pattern comprehension) a `gql:*` step evaluates is a child
    ///   traversal of that step, walked like any child (`child_traversals_mut`).
    ///
    /// Labels of reads are not reported (label-level read security is a later storage
    /// decorator, see `book/src/users_guide/permissions.md`).
    ///
    /// This form knows nothing about the step's input ([`AccessFlow::default`]); the
    /// authorization check walks a plan with [`Self::for_each_access_in`] and
    /// [`Self::access_flow_after`].
    pub(crate) fn for_each_access<E>(
        &self,
        visit: impl FnMut(crate::graph::AccessRequest<'_>) -> Result<(), E>,
    ) -> Result<(), E> {
        self.for_each_access_in(&AccessFlow::default(), visit)
    }

    /// [`Self::for_each_access`] for a step whose input the plan states as `flow` (the
    /// element kind and labels of the traversers reaching it): a write of the received
    /// elements (`property`, `drop()`, ...) names them.
    pub(crate) fn for_each_access_in<E>(
        &self,
        flow: &AccessFlow,
        mut visit: impl FnMut(crate::graph::AccessRequest<'_>) -> Result<(), E>,
    ) -> Result<(), E> {
        use crate::graph::{AccessRequest, Action, ElementKind, Resource};
        let vertex = Some(ElementKind::Vertex);
        let edge = Some(ElementKind::Edge);
        let read = |element| AccessRequest::new(Action::Read, Resource::data(element, None));
        // A write of the received elements (of the kind `element` when the step fixes it): one
        // request per label the flow knows.
        fn received<E>(
            flow: &AccessFlow,
            action: Action<'_>,
            element: Option<ElementKind>,
            visit: &mut impl FnMut(AccessRequest<'_>) -> Result<(), E>,
        ) -> Result<(), E> {
            let element = element.or(flow.element);
            if flow.labels.is_empty() || element != flow.element {
                return visit(AccessRequest::new(action, Resource::data(element, None)));
            }
            for label in &flow.labels {
                visit(AccessRequest::new(
                    action,
                    Resource::data(element, Some(label.as_str())),
                ))?;
            }
            Ok(())
        }
        match self {
            GraphStep::SideEffectMut(kind, step) => {
                let element = match *kind {
                    "add_v" | "merge_v" | "add_label" | "drop_label" => vertex,
                    "add_e" | "merge_e" | "set_label" => edge,
                    _ => None,
                };
                match *kind {
                    "add_v" | "add_e" => {
                        let labels = step.static_labels();
                        if labels.is_empty() {
                            return visit(AccessRequest::new(
                                Action::Create,
                                Resource::data(element, None),
                            ));
                        }
                        for label in labels {
                            visit(AccessRequest::new(
                                Action::Create,
                                Resource::data(element, Some(label)),
                            ))?;
                        }
                        Ok(())
                    }
                    "merge_v" | "merge_e" => {
                        visit(AccessRequest::new(
                            Action::Create,
                            Resource::data(element, None),
                        ))?;
                        visit(AccessRequest::new(
                            Action::Update,
                            Resource::data(element, None),
                        ))
                    }
                    _ => received(flow, Action::Update, element, &mut visit),
                }
            }
            GraphStep::Filter("drop", _) => received(flow, Action::Delete, None, &mut visit),
            GraphStep::Barrier("infer_schema", _) => {
                visit(AccessRequest::new(Action::Read, Resource::Schema))
            }
            // `gql:lookup` (a GQL component started at the vertices one equality finds) reads
            // vertices like the `v()` it stands for.
            GraphStep::Init("v", _)
            | GraphStep::FlatMap("out_v" | "in_v" | "both_v" | "other_v" | "gql:lookup", _) => {
                visit(read(vertex))
            }
            GraphStep::Init("e", _) | GraphStep::FlatMap("out_e" | "in_e" | "both_e", _) => {
                visit(read(edge))
            }
            GraphStep::FlatMap("out" | "in" | "both", _) => {
                visit(read(edge))?;
                visit(read(vertex))
            }
            GraphStep::SideEffect("subgraph", _) => visit(read(vertex)),
            // `embed` runs an embedder of the registry: `Execute` on it, for every embedder.
            GraphStep::Map("embed", step) => match step.embedder_name() {
                Some(name) => visit(AccessRequest::new(
                    Action::Execute,
                    Resource::Embedder { name },
                )),
                None => Ok(()),
            },
            // A text query embeds its text: `Execute` on each candidate embedder of its model.
            GraphStep::Map("similarity", step) => match step.text_query() {
                Some(query) => query.embedder_names().try_for_each(|name| {
                    visit(AccessRequest::new(
                        Action::Execute,
                        Resource::Embedder { name },
                    ))
                }),
                None => Ok(()),
            },
            GraphStep::FlatBarrier("nearest", step) => match step.text_query() {
                Some(query) => query.embedder_names().try_for_each(|name| {
                    visit(AccessRequest::new(
                        Action::Execute,
                        Resource::Embedder { name },
                    ))
                }),
                None => Ok(()),
            },
            // `inject(v)` of a vertex or edge value reaches the stored element of its id.
            GraphStep::Append(_, step) => {
                let (vertices, edges) = step.reads_elements();
                if vertices {
                    visit(read(vertex))?;
                }
                if edges {
                    visit(read(edge))?;
                }
                Ok(())
            }
            GraphStep::Init(_, _)
            | GraphStep::Map(_, _)
            | GraphStep::FlatMap(_, _)
            | GraphStep::Barrier(_, _)
            | GraphStep::FlatBarrier(_, _)
            | GraphStep::Filter(_, _)
            | GraphStep::As(_, _)
            | GraphStep::SideEffect(_, _)
            | GraphStep::Modulator(_, _)
            | GraphStep::Count(_, _) => Ok(()),
        }
    }

    /// What the plan states about the traversers leaving this step, given `flow` for the ones
    /// reaching it: the input of the next step for [`Self::for_each_access_in`].
    ///
    /// - `v()` / `merge_v()` and `out`/`in`/`both` and the endpoint steps yield vertices,
    ///   `add_v(labels)` vertices with exactly these labels; `e(labels)` / `add_e(label)` /
    ///   `merge_e()` / `out_e(labels)` / `in_e(labels)` / `both_e(labels)` yield edges (with
    ///   the labels when given);
    /// - `has_label(labels)` keeps the kind and, for edges, states the labels (an edge has
    ///   exactly one label, so a passing edge has one of them; a label set already stated
    ///   stays). A vertex has a label SET: `has_label("a")` says nothing about its other labels,
    ///   and a request naming only `a` would let a vertex labelled `{a, secret}` past a policy
    ///   that protects `secret`, so vertex labels are stated only where the plan gives all of
    ///   them (`add_v(labels)`);
    /// - every step that passes its input on unchanged keeps the flow: filters, side effects,
    ///   modulators, step labels, `identity()`, the ordering and windowing barriers
    ///   (`barrier`, `dedup`, `limit`, `order`, `range`, `sample`, `skip`, `tail`, the GQL
    ///   `gql:order` and `gql:distinct`; their
    ///   `Scope.local` forms are map steps) and the writes of the received
    ///   elements (`property`, ...); `add_label` / `drop_label` / `set_label` keep the kind but
    ///   forget the labels;
    /// - every other step yields something the plan does not state ([`AccessFlow::default`]).
    ///
    /// A child traversal starts from [`AccessFlow::default`]: the check never assumes what a
    /// parent hands its children.
    pub(crate) fn access_flow_after(&self, flow: AccessFlow) -> AccessFlow {
        use crate::graph::ElementKind;
        let of = |element, labels: Option<&[String]>| AccessFlow {
            element: Some(element),
            labels: labels.unwrap_or_default().to_vec(),
        };
        match self {
            GraphStep::Init("v", _) => of(ElementKind::Vertex, None),
            GraphStep::Init("e", step) => of(ElementKind::Edge, step.labels()),
            GraphStep::FlatMap(
                "out" | "in" | "both" | "out_v" | "in_v" | "both_v" | "other_v" | "gql:lookup",
                _,
            ) => of(ElementKind::Vertex, None),
            GraphStep::FlatMap("out_e" | "in_e" | "both_e", step) => {
                of(ElementKind::Edge, step.labels())
            }
            GraphStep::SideEffectMut("add_v", step) => AccessFlow {
                element: Some(ElementKind::Vertex),
                labels: step.static_labels().into_iter().map(String::from).collect(),
            },
            GraphStep::SideEffectMut("add_e", step) => AccessFlow {
                element: Some(ElementKind::Edge),
                labels: step.static_labels().into_iter().map(String::from).collect(),
            },
            GraphStep::SideEffectMut("merge_v", _) => of(ElementKind::Vertex, None),
            GraphStep::SideEffectMut("merge_e", _) => of(ElementKind::Edge, None),
            GraphStep::SideEffectMut("add_label" | "drop_label" | "set_label", _) => AccessFlow {
                element: flow.element,
                labels: Vec::new(),
            },
            GraphStep::Filter("has_label", step) => match step.labels() {
                Some(labels)
                    if flow.element == Some(ElementKind::Edge)
                        && flow.labels.is_empty()
                        && !labels.is_empty() =>
                {
                    AccessFlow {
                        element: flow.element,
                        labels: labels.to_vec(),
                    }
                }
                _ => flow,
            },
            GraphStep::Filter(_, _)
            | GraphStep::SideEffect(_, _)
            | GraphStep::SideEffectMut(_, _)
            | GraphStep::Modulator(_, _)
            | GraphStep::As(_, _)
            | GraphStep::Map("identity", _)
            | GraphStep::FlatBarrier(
                "barrier" | "dedup" | "limit" | "order" | "range" | "sample" | "skip" | "tail"
                | "gql:order" | "gql:distinct",
                _,
            ) => flow,
            // The GQL row of an aggregation, the element a variable holds and the items of a
            // `FOR` list: the plan states none of them (a GQL query writes nothing, so no write
            // ever reads this flow).
            GraphStep::FlatBarrier("gql:aggregate", _)
            | GraphStep::FlatMap("gql:bind" | "gql:for", _) => AccessFlow::default(),
            GraphStep::Init(_, _)
            | GraphStep::Append(_, _)
            | GraphStep::Map(_, _)
            | GraphStep::FlatMap(_, _)
            | GraphStep::Barrier(_, _)
            | GraphStep::FlatBarrier(_, _)
            | GraphStep::Count(_, _) => AccessFlow::default(),
        }
    }

    /// Whether running this step itself changes the graph: one of its requests
    /// ([`Self::for_each_access`]) writes data or the schema.
    pub(crate) fn mutates(&self) -> bool {
        self.for_each_access(|request| {
            if request.writes_graph() {
                Err(())
            } else {
                Ok(())
            }
        })
        .is_err()
    }

    /// Whether running this step can be observed other than through its output stream: the step
    /// itself or any child traversal it holds, at any depth, mutates the graph or writes a side
    /// effect ([`Self::own_effects`]). How often and in which order such a step runs is part of
    /// the query's result (`g.V().filter(__.aggregate("x")).hasLabel("software").cap("x")`), so
    /// an optimizer rule must never reorder other steps across it or move it.
    pub(crate) fn has_effects(&mut self) -> bool {
        let own = self.own_effects();
        own.mutates
            || own.writes_side_effects
            || self
                .child_traversals_mut()
                .into_iter()
                .any(|child| child.steps.iter_mut().any(GraphStep::has_effects))
    }

    /// The static, `&'static str` kind name behind this step's category constructor (e.g.
    /// `"values"`, `"select"`), without formatting (`get_name()` may `format!`). Used by the
    /// plan-time "undeclared label" check to recognize steps that can turn any incoming value
    /// into a map/object shape a later `select()` might legitimately read a key from.
    pub(crate) fn kind_name(&self) -> &'static str {
        match self {
            GraphStep::Init(n, _)
            | GraphStep::Append(n, _)
            | GraphStep::Map(n, _)
            | GraphStep::FlatMap(n, _)
            | GraphStep::Barrier(n, _)
            | GraphStep::FlatBarrier(n, _)
            | GraphStep::Filter(n, _)
            | GraphStep::SideEffect(n, _)
            | GraphStep::SideEffectMut(n, _)
            | GraphStep::Modulator(n, _)
            | GraphStep::Count(n, _) => n,
            GraphStep::As(_, _) => "as",
        }
    }

    /// Analyses the child traversals of this step; see `GraphStepMap::analyze_nested_paths`.
    pub(crate) fn analyze_nested_paths(
        &mut self,
        analysis: &crate::traversal::optimizer::path_analysis::PathAnalysis,
        downstream: &crate::traversal::optimizer::path_analysis::PathRequirement,
    ) -> crate::traversal::optimizer::path_analysis::PathRequirement {
        match self {
            GraphStep::Init(_, step) => step.analyze_nested_paths(analysis, downstream),
            GraphStep::Append(_, step) => step.analyze_nested_paths(analysis, downstream),
            GraphStep::Map(_, step) => step.analyze_nested_paths(analysis, downstream),
            GraphStep::FlatMap(_, step) => step.analyze_nested_paths(analysis, downstream),
            GraphStep::Barrier(_, step) => step.analyze_nested_paths(analysis, downstream),
            GraphStep::FlatBarrier(_, step) => step.analyze_nested_paths(analysis, downstream),
            GraphStep::Filter(_, step) => step.analyze_nested_paths(analysis, downstream),
            GraphStep::As(_, step) => step.analyze_nested_paths(analysis, downstream),
            GraphStep::SideEffect(_, step) => step.analyze_nested_paths(analysis, downstream),
            GraphStep::SideEffectMut(_, step) => step.analyze_nested_paths(analysis, downstream),
            GraphStep::Modulator(_, step) => step.analyze_nested_paths(analysis, downstream),
            GraphStep::Count(_, step) => step.analyze_nested_paths(analysis, downstream),
        }
    }

    /// Whether the executor skips this step entirely when a fused `count()` follows it.
    pub(crate) fn skipped_before_count(&self) -> bool {
        match self {
            GraphStep::Map(_, step) => !step.may_filter(),
            GraphStep::Barrier(_, _) => true,
            _ => false,
        }
    }

    /// Whether this step starts new paths, so nothing upstream of it survives into its outputs.
    pub(crate) fn resets_path(&self) -> bool {
        matches!(
            self,
            GraphStep::Barrier(_, _)
                | GraphStep::Count(_, _)
                | GraphStep::FlatBarrier("gql:aggregate", _)
        )
    }

    /// Whether this step genuinely owns new heap data at the end of its execution — see
    /// `GraphStepMap::materializes` and its siblings. `Modulator` never materializes (it doesn't
    /// execute independently); `Count`'s fused output is a scalar, so it doesn't either.
    ///
    /// Only called from the `alloc-tracking`-off (estimation) path in `source.rs`'s
    /// `step_memory_metrics` — with the feature on, every step's real allocator delta is measured
    /// directly instead, so this and every trait-level `materializes()` override go unreferenced in
    /// that build; `cfg_attr` silences the resulting dead-code warning without deleting the method,
    /// which the estimation path (any consumer built without `alloc-tracking`) still needs.
    #[cfg_attr(feature = "alloc-tracking", allow(dead_code))]
    pub(crate) fn materializes(&self) -> bool {
        match self {
            GraphStep::Init(_, step) => step.materializes(),
            GraphStep::Append(_, step) => step.materializes(),
            GraphStep::Map(_, step) => step.materializes(),
            GraphStep::FlatMap(_, step) => step.materializes(),
            GraphStep::Barrier(_, step) => step.materializes(),
            GraphStep::FlatBarrier(_, step) => step.materializes(),
            GraphStep::Filter(_, step) => step.materializes(),
            GraphStep::As(_, step) => step.materializes(),
            GraphStep::SideEffect(_, step) => step.materializes(),
            GraphStep::SideEffectMut(_, step) => step.materializes(),
            GraphStep::Modulator(_, _) => false,
            GraphStep::Count(_, _) => false,
        }
    }
}

/// The canonical rendering (see [`GraphStep::get_name`]).
impl<'g, S: GraphStorage + 'g> fmt::Display for GraphStep<'g, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.get_name())
    }
}

/// For debugging only: the canonical rendering, so a `{:?}` of a plan stays readable.
impl<'g, S: GraphStorage + 'g> fmt::Debug for GraphStep<'g, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.get_name())
    }
}

pub(crate) trait GraphStepCount<'g, S: GraphStorage + 'g>:
    Send + Sync + fmt::Display
{
    /// The child traversals of this step in their canonical order; see
    /// [`GraphStep::child_traversals_mut`]. Steps without children keep the default.
    fn child_traversals_mut(&mut self) -> Vec<&mut AnonymousTraversal<'g, S>> {
        Vec::new()
    }
    /// What this step reads from the path of its incoming traversers. Steps that read
    /// `Traverser.path` must override it, so the path requirement analysis records it for them.
    fn path_requirement(&self) -> crate::traversal::optimizer::path_analysis::PathRequirement {
        crate::traversal::optimizer::path_analysis::PathRequirement::None
    }
    /// Analyses the child traversals of this step, given what the steps after it read, and
    /// returns what the children read from the incoming path. Steps without children keep the
    /// default.
    fn analyze_nested_paths(
        &mut self,
        _analysis: &crate::traversal::optimizer::path_analysis::PathAnalysis,
        _downstream: &crate::traversal::optimizer::path_analysis::PathRequirement,
    ) -> crate::traversal::optimizer::path_analysis::PathRequirement {
        crate::traversal::optimizer::path_analysis::PathRequirement::None
    }
    fn clone_box(&self) -> Box<dyn GraphStepCount<'g, S> + 'g>;
}

pub(crate) trait GraphStepModulator<'g, S: GraphStorage + 'g>:
    Send + Sync + fmt::Display
{
    fn get_args(&mut self) -> Option<Vec<String>> {
        None
    }
    fn get_by(&mut self) -> Option<By<'g, S>> {
        None
    }
    /// The option of a step-level `with(WithOptions..)` modulator, taken once like
    /// [`get_by`](Self::get_by); `None` for every other modulator.
    fn get_with_option(
        &mut self,
    ) -> Option<(crate::value::WithOptions, Option<crate::value::WithOptions>)> {
        None
    }
    /// The child traversal of a `from(__...)`/`to(__...)` modulator, taken once like
    /// [`get_args`](Self::get_args); `None` for every other modulator.
    fn get_traversal(&mut self) -> Option<AnonymousTraversal<'g, S>> {
        None
    }
    /// Fails when the executor reaches this modulator; only the marker of a `by()` no step took
    /// (`StepByRejected`) overrides it.
    fn check(&self) -> TraverserResult<()> {
        Ok(())
    }
    /// Renames the empty traversal a rejected modulator with no step before it reports (`__`)
    /// to `root`; only that marker (`StepModulatorRejected`) overrides it. Used when the steps
    /// of a root traversal were built through the anonymous-traversal builders (the script DSL
    /// lowering, see `GraphTraversalSource::adopt_lowered`).
    fn rename_root(&mut self, _root: &'static str) {}
    /// The child traversals of this step in their canonical order; see
    /// [`GraphStep::child_traversals_mut`]. Steps without children keep the default.
    fn child_traversals_mut(&mut self) -> Vec<&mut AnonymousTraversal<'g, S>> {
        Vec::new()
    }
    /// What this step reads from the path of its incoming traversers. Steps that read
    /// `Traverser.path` must override it, so the path requirement analysis records it for them.
    fn path_requirement(&self) -> crate::traversal::optimizer::path_analysis::PathRequirement {
        crate::traversal::optimizer::path_analysis::PathRequirement::None
    }
    /// Analyses the child traversals of this step, given what the steps after it read, and
    /// returns what the children read from the incoming path. Steps without children keep the
    /// default.
    fn analyze_nested_paths(
        &mut self,
        _analysis: &crate::traversal::optimizer::path_analysis::PathAnalysis,
        _downstream: &crate::traversal::optimizer::path_analysis::PathRequirement,
    ) -> crate::traversal::optimizer::path_analysis::PathRequirement {
        crate::traversal::optimizer::path_analysis::PathRequirement::None
    }
    fn clone_box(&self) -> Box<dyn GraphStepModulator<'g, S> + 'g>;
}

pub(crate) trait GraphStepSideEffectMut<'g, S: GraphStorage + 'g>:
    Send + Sync + fmt::Display
{
    /// The labels of the element this step creates when the plan names them (`add_v("person")`),
    /// for its authorization requests ([`GraphStep::for_each_access`]); empty when there are
    /// none or they come from a traversal.
    fn static_labels(&self) -> Vec<&str> {
        Vec::new()
    }
    /// Pins the time `defaultFunction` `now`/`today` use for the elements this step creates
    /// (the script DSL's evaluation `now()`). The default ignores it: a step that creates no
    /// element.
    #[cfg_attr(not(feature = "script"), allow(dead_code))]
    fn set_create_time(&mut self, _now: crate::value::temporal::DateTime) {}
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Option<Traverser<S>>,
    ) -> TraverserResult<Vec<Traverser<S>>>;
    /// Attaches a `from()`/`to()` modulator argument list. The default rejects it: only a
    /// step that overrides this and returns [`ArgsModulation::Accepted`] takes `from()`/
    /// `to()` (TinkerPop's `FromToModulating`: `add_e()` today), so a step can never swallow
    /// one silently.
    fn push_modulator_args(&mut self, _args: Vec<String>, _modulator_name: &str) -> ArgsModulation {
        ArgsModulation::Unsupported
    }
    /// Attaches a `from(__...)`/`to(__...)` child traversal. The default rejects it; only
    /// `add_e()` takes one (its endpoint is the traversal's first result).
    fn push_modulator_traversal(
        &mut self,
        _child: AnonymousTraversal<'g, S>,
        _modulator_name: &str,
    ) -> ArgsModulation {
        ArgsModulation::Unsupported
    }
    /// Attaches a `by()` modulator. The default rejects it: only a step that overrides this and
    /// returns [`ByModulation::Accepted`] takes `by()`, so a step can never swallow one silently.
    fn push_modulator_by(&mut self, _args: By<'g, S>, _modulator_name: &str) -> ByModulation {
        ByModulation::Unsupported
    }
    fn get_name(&self) -> String {
        self.to_string()
    }
    /// The child traversals of this step in their canonical order; see
    /// [`GraphStep::child_traversals_mut`]. Steps without children keep the default.
    fn child_traversals_mut(&mut self) -> Vec<&mut AnonymousTraversal<'g, S>> {
        Vec::new()
    }
    /// What this step reads from the path of its incoming traversers. Steps that read
    /// `Traverser.path` must override it, so the path requirement analysis records it for them.
    fn path_requirement(&self) -> crate::traversal::optimizer::path_analysis::PathRequirement {
        crate::traversal::optimizer::path_analysis::PathRequirement::None
    }
    /// Analyses the child traversals of this step, given what the steps after it read, and
    /// returns what the children read from the incoming path. Steps without children keep the
    /// default.
    fn analyze_nested_paths(
        &mut self,
        _analysis: &crate::traversal::optimizer::path_analysis::PathAnalysis,
        _downstream: &crate::traversal::optimizer::path_analysis::PathRequirement,
    ) -> crate::traversal::optimizer::path_analysis::PathRequirement {
        crate::traversal::optimizer::path_analysis::PathRequirement::None
    }
    /// Whether each output object is a new path position. Steps that return their input element,
    /// such as `property()`, override it with `false`.
    fn extends_path(&self) -> bool {
        true
    }
    /// Whether this step's `process()` genuinely owns new heap data at the end of execution
    /// (e.g. a terminal materialization, `group()`/`fold()`/`sum()`/`mean()`/`median()` building
    /// an aggregation buffer, `path()` building a `Path` object, `element_map()` constructing an
    /// owned map). Default `false`: most steps only pass lazy handles through
    /// (`GraphNodeIndex`/`GraphEdgeIndex`/`VertexProperty`/`EdgeProperty`), per rule 4's
    /// lazy-evaluation discipline. Used by `.profile()` (D3 of the memory-inspection spec) to
    /// decide, with no per-item work, whether a step's `bytes_materialized` figure is worth
    /// computing at all.
    fn materializes(&self) -> bool {
        false
    }
    /// The element-creating step (`addV()`/`addE()`) behind this step, so a following
    /// `property(T.id|T.label, v)` can fold into it (TinkerPop folds `property()` calls that
    /// directly follow an add step into it). Every other step keeps the default `None`.
    fn as_add_element_mut(&mut self) -> Option<&mut dyn AddElementStep> {
        None
    }
    /// For a plain `property(name, constant)` step: the key and the constant value, so the
    /// `add_property_fold` optimizer rule can fold it into the preceding add step. Every other
    /// step, and every `property()` form whose semantics folding could not reproduce exactly
    /// (a traversal value, a `Cardinality`, a `jpath` key), keeps the default `None`.
    fn constant_property(&self) -> Option<(&compact_str::CompactString, &crate::ElementProperty)> {
        None
    }
    /// The merge step (`mergeV()`/`mergeE()`) behind this step, so a following
    /// `option(Merge.x, ..)` can attach to it. Every other step keeps the default `None`.
    fn as_merge_mut(&mut self) -> Option<&mut dyn sideeffect_steps::merge_map::MergeStep<'g, S>> {
        None
    }
    /// A problem found when the step was built (e.g. an invalid constant merge map), raised by
    /// the plan-time validation pass before execution, even when no traverser reaches the step.
    fn plan_check(&self) -> TraverserResult<()> {
        Ok(())
    }
    fn clone_box(&self) -> Box<dyn GraphStepSideEffectMut<'g, S> + 'g>;
}

/// An element-creating step that `property(T.id, v)` / `property(T.label, v)` can fold into.
pub(crate) trait AddElementStep {
    /// Sets the user-supplied id of the element this step creates.
    fn set_id(&mut self, id: compact_str::CompactString);
    /// Replaces the label of the element this step creates; `false` when the step cannot take a
    /// label this way (`addE()`, whose label comes from `addE(label)`).
    fn set_label(&mut self, label: compact_str::CompactString) -> bool;
    /// Adds a property the element is created with (the `add_property_fold` rule). A key that
    /// is already present is overwritten, so the last `property()` of a key wins, exactly as the
    /// unfused sequence would leave it.
    fn fold_property(&mut self, key: compact_str::CompactString, value: crate::ElementProperty);
}

pub(crate) trait GraphStepSideEffect<'g, S: GraphStorage + 'g>:
    Send + Sync + fmt::Display
{
    /// Mutates `context.side_effect` (or runs some other non-transforming side operation);
    /// `source` continues downstream unchanged either way. Fallible because a `.by(...)`
    /// modulator (`aggregate()`/`store()`) resolves and materializes a value, which can fail the
    /// same way `group()`/`fold()`'s `.by()` resolution can.
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: &Traverser<S>,
    ) -> TraverserResult<()>;
    /// Called once after `process` has seen every traverser of one batch (one execution of the
    /// step; a `local()` child runs it once per traverser). The default does nothing;
    /// `aggregate()` folds the batch into a reduced side effect here.
    fn finish_batch(&self, _context: &mut TraverserContext<'_, S>) -> TraverserResult<()> {
        Ok(())
    }
    /// Attaches a `from()`/`to()` modulator argument list. The default rejects it: only a
    /// step that overrides this and returns [`ArgsModulation::Accepted`] takes `from()`/
    /// `to()` (TinkerPop's `FromToModulating`: `add_e()` today), so a step can never swallow
    /// one silently.
    fn push_modulator_args(&mut self, _args: Vec<String>, _modulator_name: &str) -> ArgsModulation {
        ArgsModulation::Unsupported
    }
    /// Attaches a `by()` modulator. The default rejects it: only a step that overrides this and
    /// returns [`ByModulation::Accepted`] takes `by()`, so a step can never swallow one silently.
    fn push_modulator_by(&mut self, _args: By<'g, S>, _modulator_name: &str) -> ByModulation {
        ByModulation::Unsupported
    }
    fn get_name(&self) -> String {
        self.to_string()
    }
    /// The child traversals of this step in their canonical order; see
    /// [`GraphStep::child_traversals_mut`]. Steps without children keep the default.
    fn child_traversals_mut(&mut self) -> Vec<&mut AnonymousTraversal<'g, S>> {
        Vec::new()
    }
    /// What this step reads from the path of its incoming traversers. Steps that read
    /// `Traverser.path` must override it, so the path requirement analysis records it for them.
    fn path_requirement(&self) -> crate::traversal::optimizer::path_analysis::PathRequirement {
        crate::traversal::optimizer::path_analysis::PathRequirement::None
    }
    /// Analyses the child traversals of this step, given what the steps after it read, and
    /// returns what the children read from the incoming path. Steps without children keep the
    /// default.
    fn analyze_nested_paths(
        &mut self,
        _analysis: &crate::traversal::optimizer::path_analysis::PathAnalysis,
        _downstream: &crate::traversal::optimizer::path_analysis::PathRequirement,
    ) -> crate::traversal::optimizer::path_analysis::PathRequirement {
        crate::traversal::optimizer::path_analysis::PathRequirement::None
    }
    /// Whether this step's `process()` genuinely owns new heap data at the end of execution
    /// (e.g. a terminal materialization, `group()`/`fold()`/`sum()`/`mean()`/`median()` building
    /// an aggregation buffer, `path()` building a `Path` object, `element_map()` constructing an
    /// owned map). Default `false`: most steps only pass lazy handles through
    /// (`GraphNodeIndex`/`GraphEdgeIndex`/`VertexProperty`/`EdgeProperty`), per rule 4's
    /// lazy-evaluation discipline. Used by `.profile()` (D3 of the memory-inspection spec) to
    /// decide, with no per-item work, whether a step's `bytes_materialized` figure is worth
    /// computing at all.
    fn materializes(&self) -> bool {
        false
    }
    /// The side-effect key this step populates (`aggregate("key")`/`store("key")`), if any — a
    /// second way, besides `as()`, that a label read by `select()`/`cap()`/
    /// `where(within(...))` can be declared somewhere in the query. `None` for every step that
    /// isn't a keyed side effect.
    fn declares_side_effect_key(&self) -> Option<&str> {
        None
    }
    /// Registers the (empty) bucket of [`Self::declares_side_effect_key`] before the step
    /// processes its first traverser, so `cap(key)` of a step that ran but collected nothing
    /// (an empty upstream, or a `by()` that resolved no value) reads the empty collection, as in
    /// TinkerPop. An existing bucket (a `withSideEffect` seed, a `Set`, an earlier fill) is never
    /// touched, and the common path only does a `contains_key`.
    fn declare_side_effect(
        &self,
        side_effect: &mut std::collections::HashMap<
            compact_str::CompactString,
            crate::traversal::side_effect::SideEffectValue<S>,
        >,
    ) {
        if let Some(key) = self.declares_side_effect_key()
            && !side_effect.contains_key(key)
        {
            side_effect.insert(
                compact_str::CompactString::new(key),
                crate::traversal::side_effect::SideEffectValue::List(Vec::new()),
            );
        }
    }
    fn clone_box(&self) -> Box<dyn GraphStepSideEffect<'g, S> + 'g>;
}

pub(crate) trait GraphStepAs<'g, S: GraphStorage + 'g>: Send + Sync + fmt::Display {
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: &mut Traverser<S>,
    ) -> TraverserResult<()>;
    fn get_name(&self) -> String {
        self.to_string()
    }
    /// The child traversals of this step in their canonical order; see
    /// [`GraphStep::child_traversals_mut`]. Steps without children keep the default.
    fn child_traversals_mut(&mut self) -> Vec<&mut AnonymousTraversal<'g, S>> {
        Vec::new()
    }
    /// What this step reads from the path of its incoming traversers. Steps that read
    /// `Traverser.path` must override it, so the path requirement analysis records it for them.
    fn path_requirement(&self) -> crate::traversal::optimizer::path_analysis::PathRequirement {
        crate::traversal::optimizer::path_analysis::PathRequirement::None
    }
    /// Analyses the child traversals of this step, given what the steps after it read, and
    /// returns what the children read from the incoming path. Steps without children keep the
    /// default.
    fn analyze_nested_paths(
        &mut self,
        _analysis: &crate::traversal::optimizer::path_analysis::PathAnalysis,
        _downstream: &crate::traversal::optimizer::path_analysis::PathRequirement,
    ) -> crate::traversal::optimizer::path_analysis::PathRequirement {
        crate::traversal::optimizer::path_analysis::PathRequirement::None
    }
    /// The labels this step attaches to the current path position.
    fn step_labels(&self) -> Option<&std::sync::Arc<[compact_str::CompactString]>> {
        None
    }
    /// Whether this step's `process()` genuinely owns new heap data at the end of execution
    /// (e.g. a terminal materialization, `group()`/`fold()`/`sum()`/`mean()`/`median()` building
    /// an aggregation buffer, `path()` building a `Path` object, `element_map()` constructing an
    /// owned map). Default `false`: most steps only pass lazy handles through
    /// (`GraphNodeIndex`/`GraphEdgeIndex`/`VertexProperty`/`EdgeProperty`), per rule 4's
    /// lazy-evaluation discipline. Used by `.profile()` (D3 of the memory-inspection spec) to
    /// decide, with no per-item work, whether a step's `bytes_materialized` figure is worth
    /// computing at all.
    fn materializes(&self) -> bool {
        false
    }
    fn clone_box(&self) -> Box<dyn GraphStepAs<'g, S> + 'g>;
}

/// A `has(key, value)` condition folded into a `v()`/`e()` source step: the element must carry
/// the property `key` (a literal name, never a jpath) equal to `value`, with exactly the equality
/// of the `has` step ([`filter_steps::step_has::vertex_has`]/`edge_has`), so folding never changes
/// a result. Checked while the source scans, before a traverser exists.
#[derive(Debug, Clone)]
pub(crate) struct PropertyFilter {
    pub(crate) key: String,
    pub(crate) value: crate::value::ElementProperty,
}

impl PropertyFilter {
    /// Whether vertex `node` satisfies the condition.
    #[inline]
    pub(crate) fn vertex_matches<S: GraphStorage>(&self, storage: &S, node: S::VertexId) -> bool {
        filter_steps::step_has::vertex_has(storage, node, &self.key, &self.value)
    }

    /// Whether edge `edge` satisfies the condition.
    #[inline]
    pub(crate) fn edge_matches<S: GraphStorage>(&self, storage: &S, edge: S::EdgeId) -> bool {
        filter_steps::step_has::edge_has(storage, edge, &self.key, &self.value)
    }
}

/// The property index a `v()`/`e()` source reads instead of its label buckets (set by the
/// `property_index` rule, book page "Property Indexes"): the candidates are the elements of
/// each source label whose `key` lies in one of `ranges` (a superset); the source still checks
/// its folded conditions, and the condition the index serves stays in the plan when it is a
/// `has(key, P)` step, so the result never differs from the label scan.
#[derive(Debug, Clone)]
pub(crate) struct IndexAccess {
    /// The top-level property key of the index.
    pub(crate) key: String,
    /// The transform of the index (`None`: the plain index on the value).
    pub(crate) transform: Option<crate::graph::PropertyIndexTransform>,
    /// The slices of the index to read (several for `P.within` and `P.outside`).
    pub(crate) ranges: Vec<crate::graph::PropertyIndexRange>,
}

impl IndexAccess {
    /// `[index: vertex person.title, vertex employee.title]`: what `.profile()` shows; an index
    /// with a transform names it (`[index: vertex person.name to_lower]`).
    pub(crate) fn render(&self, element: &str, labels: &[String]) -> String {
        let suffix = match &self.transform {
            Some(transform) => format!(" {transform}"),
            None => String::new(),
        };
        let names: Vec<String> = labels
            .iter()
            .map(|label| format!("{element} {label}.{}{suffix}", self.key))
            .collect();
        format!(" [index: {}]", names.join(", "))
    }
}

/// A `has(key, P)` condition as slices of an ordered property index
/// ([`GraphStepFilter::index_condition`]).
#[derive(Debug, Clone)]
pub(crate) struct IndexCondition<'a> {
    /// The top-level property key.
    pub(crate) key: &'a str,
    /// The transform the condition applies to the value before the predicate (`has(key,
    /// __.to_lower().is(P))`: `to_lower`), so only an index of that transform serves it.
    pub(crate) transform: Option<crate::graph::PropertyIndexTransform>,
    /// The slices holding every value the predicate accepts.
    pub(crate) ranges: Vec<crate::graph::PropertyIndexRange>,
}

pub(crate) trait GraphStepInit<'g, S: GraphStorage + 'g>:
    Send + Sync + fmt::Display
{
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
    ) -> (GraphNodeType, &Option<Vec<String>>);
    fn labels(&self) -> Option<&[String]> {
        None
    }
    fn set_labels(&mut self, _labels: Vec<String>) {}
    fn ids(&self) -> Option<&[String]> {
        None
    }
    fn set_ids(&mut self, _ids: Vec<String>) {}
    /// The `has(key, value)` conditions folded into this source step; each element it finds
    /// must satisfy all of them. Default: none.
    fn property_filters(&self) -> &[PropertyFilter] {
        &[]
    }
    /// Folds one more `has(key, value)` condition into this source step. Returns `false` (and
    /// keeps nothing) for a step that cannot check properties while it scans (the default).
    fn push_property_filter(&mut self, _filter: PropertyFilter) -> bool {
        false
    }
    /// The property index this source reads (see [`IndexAccess`]). Default: none.
    fn index_access(&self) -> Option<&IndexAccess> {
        None
    }
    /// Sets (or with `None` clears) the property index this source reads. Returns `false` for
    /// a step that cannot read an index (the default).
    fn set_index_access(&mut self, _access: Option<IndexAccess>) -> bool {
        false
    }
    fn is_count_only(&self) -> bool {
        false
    }
    fn set_count_only(&mut self, _count_only: bool) {}
    fn is_group_count_by_label(&self) -> bool {
        false
    }
    fn set_group_count_by_label(&mut self, _group_count_by_label: bool) {}
    /// Attaches a `from()`/`to()` modulator argument list. The default rejects it: only a
    /// step that overrides this and returns [`ArgsModulation::Accepted`] takes `from()`/
    /// `to()` (TinkerPop's `FromToModulating`: `add_e()` today), so a step can never swallow
    /// one silently.
    fn push_modulator_args(&mut self, _args: Vec<String>, _modulator_name: &str) -> ArgsModulation {
        ArgsModulation::Unsupported
    }
    /// Attaches a `by()` modulator. The default rejects it: only a step that overrides this and
    /// returns [`ByModulation::Accepted`] takes `by()`, so a step can never swallow one silently.
    fn push_modulator_by(&mut self, _args: By<'g, S>, _modulator_name: &str) -> ByModulation {
        ByModulation::Unsupported
    }
    fn get_name(&self) -> String {
        self.to_string()
    }
    /// The child traversals of this step in their canonical order; see
    /// [`GraphStep::child_traversals_mut`]. Steps without children keep the default.
    fn child_traversals_mut(&mut self) -> Vec<&mut AnonymousTraversal<'g, S>> {
        Vec::new()
    }
    /// What this step reads from the path of its incoming traversers. Steps that read
    /// `Traverser.path` must override it, so the path requirement analysis records it for them.
    fn path_requirement(&self) -> crate::traversal::optimizer::path_analysis::PathRequirement {
        crate::traversal::optimizer::path_analysis::PathRequirement::None
    }
    /// Analyses the child traversals of this step, given what the steps after it read, and
    /// returns what the children read from the incoming path. Steps without children keep the
    /// default.
    fn analyze_nested_paths(
        &mut self,
        _analysis: &crate::traversal::optimizer::path_analysis::PathAnalysis,
        _downstream: &crate::traversal::optimizer::path_analysis::PathRequirement,
    ) -> crate::traversal::optimizer::path_analysis::PathRequirement {
        crate::traversal::optimizer::path_analysis::PathRequirement::None
    }
    /// Whether this step's `process()` genuinely owns new heap data at the end of execution
    /// (e.g. a terminal materialization, `group()`/`fold()`/`sum()`/`mean()`/`median()` building
    /// an aggregation buffer, `path()` building a `Path` object, `element_map()` constructing an
    /// owned map). Default `false`: most steps only pass lazy handles through
    /// (`GraphNodeIndex`/`GraphEdgeIndex`/`VertexProperty`/`EdgeProperty`), per rule 4's
    /// lazy-evaluation discipline. Used by `.profile()` (D3 of the memory-inspection spec) to
    /// decide, with no per-item work, whether a step's `bytes_materialized` figure is worth
    /// computing at all.
    fn materializes(&self) -> bool {
        false
    }
    fn clone_box(&self) -> Box<dyn GraphStepInit<'g, S> + 'g>;
}

pub(crate) trait GraphStepAppend<'g, S: GraphStorage + 'g>:
    Send + Sync + fmt::Display
{
    fn process(&self, context: &mut TraverserContext<'_, S>) -> TraverserResult<Vec<Traverser<S>>>;
    fn get_name(&self) -> String {
        self.to_string()
    }
    /// Which stored elements this step reaches by id (`(vertices, edges)`): `inject(v)` of a
    /// vertex or edge value emits the stored element of its id, so it reads graph data like
    /// `V()`/`E()` and asks the same Read request. Steps that emit only plain values keep the
    /// default.
    fn reads_elements(&self) -> (bool, bool) {
        (false, false)
    }
    /// The child traversals of this step in their canonical order; see
    /// [`GraphStep::child_traversals_mut`]. Steps without children keep the default.
    fn child_traversals_mut(&mut self) -> Vec<&mut AnonymousTraversal<'g, S>> {
        Vec::new()
    }
    /// What this step reads from the path of its incoming traversers. Steps that read
    /// `Traverser.path` must override it, so the path requirement analysis records it for them.
    fn path_requirement(&self) -> crate::traversal::optimizer::path_analysis::PathRequirement {
        crate::traversal::optimizer::path_analysis::PathRequirement::None
    }
    /// Analyses the child traversals of this step, given what the steps after it read, and
    /// returns what the children read from the incoming path. Steps without children keep the
    /// default.
    fn analyze_nested_paths(
        &mut self,
        _analysis: &crate::traversal::optimizer::path_analysis::PathAnalysis,
        _downstream: &crate::traversal::optimizer::path_analysis::PathRequirement,
    ) -> crate::traversal::optimizer::path_analysis::PathRequirement {
        crate::traversal::optimizer::path_analysis::PathRequirement::None
    }
    /// Whether this step's `process()` genuinely owns new heap data at the end of execution
    /// (e.g. a terminal materialization, `group()`/`fold()`/`sum()`/`mean()`/`median()` building
    /// an aggregation buffer, `path()` building a `Path` object, `element_map()` constructing an
    /// owned map). Default `false`: most steps only pass lazy handles through
    /// (`GraphNodeIndex`/`GraphEdgeIndex`/`VertexProperty`/`EdgeProperty`), per rule 4's
    /// lazy-evaluation discipline. Used by `.profile()` (D3 of the memory-inspection spec) to
    /// decide, with no per-item work, whether a step's `bytes_materialized` figure is worth
    /// computing at all.
    fn materializes(&self) -> bool {
        false
    }
    fn clone_box(&self) -> Box<dyn GraphStepAppend<'g, S> + 'g>;
}

pub(crate) trait GraphStepMap<'g, S: GraphStorage + 'g>: Send + Sync + fmt::Display {
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<Traverser<S>>;
    /// The property index transform this step computes on a string, when it is exactly one
    /// (`to_lower()`: [`PropertyIndexTransform::ToLower`](crate::graph::PropertyIndexTransform)),
    /// so the `property_index` rule can serve `has(key, __.to_lower().is(P))` from an index of
    /// that transform. Default `None`.
    fn index_transform(&self) -> Option<crate::graph::PropertyIndexTransform> {
        None
    }
    /// Attaches a `from()`/`to()` modulator argument list. The default rejects it: only a
    /// step that overrides this and returns [`ArgsModulation::Accepted`] takes `from()`/
    /// `to()` (TinkerPop's `FromToModulating`: `add_e()` today), so a step can never swallow
    /// one silently.
    fn push_modulator_args(&mut self, _args: Vec<String>, _modulator_name: &str) -> ArgsModulation {
        ArgsModulation::Unsupported
    }
    /// Takes a step-level `with(WithOptions..)` option: `None` when accepted, otherwise the
    /// `InvalidModulator` reason. The default rejects every option.
    fn push_with_option(
        &mut self,
        _key: crate::value::WithOptions,
        _value: Option<crate::value::WithOptions>,
    ) -> Option<String> {
        Some(crate::traversal::with_reason::NOT_ACCEPTED.to_string())
    }
    /// Attaches a `by()` modulator. The default rejects it: only a step that overrides this and
    /// returns [`ByModulation::Accepted`] takes `by()`, so a step can never swallow one silently.
    fn push_modulator_by(&mut self, _args: By<'g, S>, _modulator_name: &str) -> ByModulation {
        ByModulation::Unsupported
    }
    fn get_name(&self) -> String {
        self.to_string()
    }
    /// The child traversals of this step in their canonical order; see
    /// [`GraphStep::child_traversals_mut`]. Steps without children keep the default.
    fn child_traversals_mut(&mut self) -> Vec<&mut AnonymousTraversal<'g, S>> {
        Vec::new()
    }
    /// What this step reads from the path of its incoming traversers. Steps that read
    /// `Traverser.path` must override it, so the path requirement analysis records it for them.
    fn path_requirement(&self) -> crate::traversal::optimizer::path_analysis::PathRequirement {
        crate::traversal::optimizer::path_analysis::PathRequirement::None
    }
    /// The step labels whose every occurrence on the path this step reads (the edges of a GQL
    /// quantified edge for its trail check or its group variable), besides the latest one a
    /// `select()` reads. The path analysis keeps such a label upstream of the `as()` that sets it
    /// again, so earlier occurrences survive (`PathAnalysis`'s history labels). Empty by default;
    /// only with the `gql` feature (no other step declares any).
    #[cfg(feature = "gql")]
    fn history_labels(&self) -> &[compact_str::CompactString] {
        &[]
    }
    /// Analyses the child traversals of this step, given what the steps after it read, and
    /// returns what the children read from the incoming path. Steps without children keep the
    /// default.
    fn analyze_nested_paths(
        &mut self,
        _analysis: &crate::traversal::optimizer::path_analysis::PathAnalysis,
        _downstream: &crate::traversal::optimizer::path_analysis::PathRequirement,
    ) -> crate::traversal::optimizer::path_analysis::PathRequirement {
        crate::traversal::optimizer::path_analysis::PathRequirement::None
    }
    /// Whether the output object is a new path position (TinkerPop map semantics). A step that
    /// passes its input through unchanged, such as `identity()`, overrides it with `false`.
    fn extends_path(&self) -> bool {
        true
    }
    /// Whether [`process_filter`](Self::process_filter) can drop a traverser. A fused `count()`
    /// skips a map step only when it cannot.
    fn may_filter(&self) -> bool {
        false
    }
    /// How many traversers the executor hands to [`Self::process_batch`] at once; `1` (the
    /// default): the executor calls [`Self::process_filter`] per traverser. A step that calls
    /// an embedder returns the embedder's batch size, so one call covers many traversers.
    fn batch_size(&self) -> usize {
        1
    }
    /// Maps a batch of at most [`Self::batch_size`] traversers (each kept or dropped, in input
    /// order). Only called when `batch_size() > 1`; the default maps one by one.
    fn process_batch(
        &self,
        context: &mut TraverserContext<'_, S>,
        sources: Vec<Traverser<S>>,
    ) -> TraverserResult<Vec<Traverser<S>>> {
        let mut out = Vec::with_capacity(sources.len());
        for source in sources {
            if let Some(mapped) = self.process_filter(context, source)? {
                out.push(mapped);
            }
        }
        Ok(out)
    }
    /// `Some(name)` for a step that calls an embedder: the name it asks for (`None` inside:
    /// the registry's default). `run()` resolves it and calls [`Self::bind_embedder`] before the
    /// permission check.
    fn embedder_request(&self) -> Option<Option<compact_str::CompactString>> {
        None
    }
    /// Binds the embedder `run()` resolved for [`Self::embedder_request`].
    fn bind_embedder(&mut self, _embedder: std::sync::Arc<dyn crate::value::Embedder>) {}
    /// The registry name of the bound embedder, for the permission request.
    fn embedder_name(&self) -> Option<&str> {
        None
    }
    /// The text query of a step searching vectors by a text (`similarity("emb", "text")`)
    /// with the step's property key: `run()` binds its candidate embedders before the
    /// permission check.
    fn text_query_mut(
        &mut self,
    ) -> Option<(
        Option<&crate::PropKey>,
        &mut crate::traversal::step::map_steps::step_similarity::TextQuery,
    )> {
        None
    }
    /// The bound text query (the permission check asks `Execute` on its embedders).
    fn text_query(&self) -> Option<&crate::traversal::step::map_steps::step_similarity::TextQuery> {
        None
    }
    /// Maps one traverser, or drops it; the executor calls this rather than `process`.
    fn process_filter(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<Option<Traverser<S>>> {
        self.process(context, source).map(Some)
    }
    /// Whether this step's `process()` genuinely owns new heap data at the end of execution
    /// (e.g. a terminal materialization, `group()`/`fold()`/`sum()`/`mean()`/`median()` building
    /// an aggregation buffer, `path()` building a `Path` object, `element_map()` constructing an
    /// owned map). Default `false`: most steps only pass lazy handles through
    /// (`GraphNodeIndex`/`GraphEdgeIndex`/`VertexProperty`/`EdgeProperty`), per rule 4's
    /// lazy-evaluation discipline. Used by `.profile()` (D3 of the memory-inspection spec) to
    /// decide, with no per-item work, whether a step's `bytes_materialized` figure is worth
    /// computing at all.
    fn materializes(&self) -> bool {
        false
    }
    /// Labels this step needs to resolve, declared **somewhere** in the query (an `as()`, or a
    /// side-effect key from `aggregate()`/`store()`/`tree()`), for the plan-time "undeclared
    /// label" check. Narrower than [`path_requirement`](Self::path_requirement): a step overrides
    /// this only when failing to resolve one of its labels is itself a query-authoring mistake
    /// that this crate reports before execution (`select()`), never for a step that treats an
    /// unresolved label as a best-effort match and falls back to something else (`is()`/
    /// `where_p()` fall back to a literal-value comparison; see `value/predicate.rs`).
    fn required_labels(&self) -> Option<Vec<compact_str::CompactString>> {
        None
    }
    /// A problem found when the step was built (e.g. a `math()` equation that does not parse),
    /// raised by the plan-time validation pass before execution, even when no traverser reaches
    /// the step.
    fn plan_check(&self) -> TraverserResult<()> {
        Ok(())
    }
    fn clone_box(&self) -> Box<dyn GraphStepMap<'g, S> + 'g>;
}

pub(crate) trait GraphStepFlatMap<'g, S: GraphStorage + 'g>:
    Send + Sync + fmt::Display
{
    /// Expands one input traverser into zero or more output traversers, writing them into
    /// `output` rather than returning a freshly allocated `Vec`.
    ///
    /// Buffer-ownership contract: `output` is owned by the caller and may already contain items
    /// from earlier calls (e.g. earlier input traversers in the same step's execution). This
    /// method must only `push`/`extend` onto `output` — it must never call `output.clear()` or
    /// otherwise assume anything about its prior contents beyond "it's where this call's new
    /// items go." Callers rely on this to grow one shared, amortized-growth buffer across many
    /// `process()` calls instead of allocating a small `Vec` per call.
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
        output: &mut Vec<Traverser<S>>,
    ) -> TraverserResult<()>;
    /// Expands the whole input stream at once. Only called by the executor for a step whose
    /// [`is_batch`](Self::is_batch) is true; the default loops over [`process`](Self::process).
    /// `union()`/`choose()`/`branch()` override it to feed every traverser routed to a stateful
    /// child (`limit`, `count`, `order`, `dedup`, ...) into one run of that child, as
    /// TinkerPop's `BranchStep` does.
    fn process_batch(
        &self,
        context: &mut TraverserContext<'_, S>,
        input: Vec<Traverser<S>>,
        output: &mut Vec<Traverser<S>>,
    ) -> TraverserResult<()> {
        for source in input {
            self.process(context, source, output)?;
        }
        Ok(())
    }
    /// Whether the executor must hand this step the whole input stream through
    /// [`process_batch`](Self::process_batch) instead of calling `process` per traverser. True
    /// for a branching step with a stateful child traversal. Default `false`.
    fn is_batch(&self) -> bool {
        false
    }
    fn count(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<u64> {
        let mut scratch = Vec::new();
        self.process(context, source, &mut scratch)?;
        // Outputs inherit the parent's bulk, so the logical count is the sum of their bulks.
        crate::traversal::bulk::total_bulk(context, &scratch, "count")
    }
    /// Whether this step walks the graph structure (`out`, `in`, `both`, their edge and
    /// endpoint forms). Two such steps in a row are where equal traversers pile up, so the
    /// `lazy_barrier` optimizer rule puts a merge point between them. Default `false`.
    fn is_adjacency(&self) -> bool {
        false
    }
    fn labels(&self) -> Option<&[String]> {
        None
    }
    fn set_labels(&mut self, _labels: Vec<String>) {}
    /// Attaches a `from()`/`to()` modulator argument list. The default rejects it: only a
    /// step that overrides this and returns [`ArgsModulation::Accepted`] takes `from()`/
    /// `to()` (TinkerPop's `FromToModulating`: `add_e()` today), so a step can never swallow
    /// one silently.
    fn push_modulator_args(&mut self, _args: Vec<String>, _modulator_name: &str) -> ArgsModulation {
        ArgsModulation::Unsupported
    }
    /// Attaches a `by()` modulator. The default rejects it: only a step that overrides this and
    /// returns [`ByModulation::Accepted`] takes `by()`, so a step can never swallow one silently.
    fn push_modulator_by(&mut self, _args: By<'g, S>, _modulator_name: &str) -> ByModulation {
        ByModulation::Unsupported
    }
    fn get_name(&self) -> String {
        self.to_string()
    }
    /// The child traversals of this step in their canonical order; see
    /// [`GraphStep::child_traversals_mut`]. Steps without children keep the default.
    fn child_traversals_mut(&mut self) -> Vec<&mut AnonymousTraversal<'g, S>> {
        Vec::new()
    }
    /// What this step reads from the path of its incoming traversers. Steps that read
    /// `Traverser.path` must override it, so the path requirement analysis records it for them.
    fn path_requirement(&self) -> crate::traversal::optimizer::path_analysis::PathRequirement {
        crate::traversal::optimizer::path_analysis::PathRequirement::None
    }
    /// The step labels whose every occurrence on the path this step reads (the edges of a GQL
    /// quantified edge for its trail check or its group variable), besides the latest one a
    /// `select()` reads. The path analysis keeps such a label upstream of the `as()` that sets it
    /// again, so earlier occurrences survive (`PathAnalysis`'s history labels). Empty by default;
    /// only with the `gql` feature (no other step declares any).
    #[cfg(feature = "gql")]
    fn history_labels(&self) -> &[compact_str::CompactString] {
        &[]
    }
    /// Analyses the child traversals of this step, given what the steps after it read, and
    /// returns what the children read from the incoming path. Steps without children keep the
    /// default.
    fn analyze_nested_paths(
        &mut self,
        _analysis: &crate::traversal::optimizer::path_analysis::PathAnalysis,
        _downstream: &crate::traversal::optimizer::path_analysis::PathRequirement,
    ) -> crate::traversal::optimizer::path_analysis::PathRequirement {
        crate::traversal::optimizer::path_analysis::PathRequirement::None
    }
    /// Whether each output object is a new path position. Branching steps (`union()`,
    /// `coalesce()`, `optional()`) override it with `false`: their children already extended it.
    fn extends_path(&self) -> bool {
        true
    }
    /// Whether this step may start a traversal (`g.union(...)`). As the first step of the root
    /// traversal it receives one seed traverser holding `null`, like TinkerPop's
    /// `GraphTraversalSource.union`. Default `false`; only `union()` overrides it.
    fn is_start(&self) -> bool {
        false
    }
    /// Called by the executor on a start step ([`is_start`](Self::is_start)) before it runs, so
    /// the step knows that its input is the seed, not a real traverser. Default: nothing.
    fn mark_start(&mut self) {}
    /// The step as a `choose()`/`branch()`, so a chained `.option(pick, traversal)` can attach to
    /// it in place (TinkerPop `GraphTraversal.option`), mirroring `GraphStepFlatBarrier::
    /// as_repeat_mut`'s postfix-attachment hook for `repeat()`'s `times()`/`until()`/`emit()`.
    /// Only `StepBranch` (shared by `choose()` and `branch()`) overrides it.
    fn as_branch_mut(
        &mut self,
    ) -> Option<&mut crate::traversal::step::flatmap_steps::step_choose::StepBranch<'g, S>> {
        None
    }
    /// Whether this step's `process()` genuinely owns new heap data at the end of execution
    /// (e.g. a terminal materialization, `group()`/`fold()`/`sum()`/`mean()`/`median()` building
    /// an aggregation buffer, `path()` building a `Path` object, `element_map()` constructing an
    /// owned map). Default `false`: most steps only pass lazy handles through
    /// (`GraphNodeIndex`/`GraphEdgeIndex`/`VertexProperty`/`EdgeProperty`), per rule 4's
    /// lazy-evaluation discipline. Used by `.profile()` (D3 of the memory-inspection spec) to
    /// decide, with no per-item work, whether a step's `bytes_materialized` figure is worth
    /// computing at all.
    fn materializes(&self) -> bool {
        false
    }
    fn clone_box(&self) -> Box<dyn GraphStepFlatMap<'g, S> + 'g>;
}

pub(crate) trait GraphStepBarrier<'g, S: GraphStorage + 'g>:
    Send + Sync + fmt::Display
{
    /// Reduces the whole batch to at most one output traverser. `None` means the barrier emits
    /// nothing for this batch — TinkerPop's un-seeded reducing barriers (`sum()`/`mean()`/
    /// `min()`/`max()`/`median()`) on an empty (or all-null) numeric stream. A seeded barrier
    /// (`fold()`/`group()`/`groupCount()`/`tree()`/`cap()`, see [`always_emits`](Self::always_emits))
    /// never returns `None`.
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Vec<Traverser<S>>,
    ) -> TraverserResult<Option<Traverser<S>>>;
    fn is_group_by_label_count(&self) -> bool {
        false
    }
    /// Whether this barrier always produces exactly one output traverser, independent of how
    /// many traversers reach it. TinkerPop's seeded reducing barriers (`fold()` → `[]`,
    /// `group()`/`groupCount()`/`tree()` → `{}`) and `cap()` (whose output depends only on side
    /// effects collected so far, never on the incoming stream's size) all set this to `true`.
    /// Default `false`: `sum()`/`mean()`/`min()`/`max()`/`median()` emit nothing on an empty
    /// stream, matching TinkerPop's un-seeded reducing barriers. Lets a fused `count()` learn the
    /// output count (always `1`) without materializing the barrier's actual value.
    fn always_emits(&self) -> bool {
        false
    }
    /// Attaches a `from()`/`to()` modulator argument list. The default rejects it: only a
    /// step that overrides this and returns [`ArgsModulation::Accepted`] takes `from()`/
    /// `to()` (TinkerPop's `FromToModulating`: `add_e()` today), so a step can never swallow
    /// one silently.
    fn push_modulator_args(&mut self, _args: Vec<String>, _modulator_name: &str) -> ArgsModulation {
        ArgsModulation::Unsupported
    }
    /// Attaches a `by()` modulator. The default rejects it: only a step that overrides this and
    /// returns [`ByModulation::Accepted`] takes `by()`, so a step can never swallow one silently.
    fn push_modulator_by(&mut self, _args: By<'g, S>, _modulator_name: &str) -> ByModulation {
        ByModulation::Unsupported
    }
    fn get_name(&self) -> String {
        self.to_string()
    }
    /// The child traversals of this step in their canonical order; see
    /// [`GraphStep::child_traversals_mut`]. Steps without children keep the default.
    fn child_traversals_mut(&mut self) -> Vec<&mut AnonymousTraversal<'g, S>> {
        Vec::new()
    }
    /// What this step reads from the path of its incoming traversers. Steps that read
    /// `Traverser.path` must override it, so the path requirement analysis records it for them.
    fn path_requirement(&self) -> crate::traversal::optimizer::path_analysis::PathRequirement {
        crate::traversal::optimizer::path_analysis::PathRequirement::None
    }
    /// Analyses the child traversals of this step, given what the steps after it read, and
    /// returns what the children read from the incoming path. Steps without children keep the
    /// default.
    fn analyze_nested_paths(
        &mut self,
        _analysis: &crate::traversal::optimizer::path_analysis::PathAnalysis,
        _downstream: &crate::traversal::optimizer::path_analysis::PathRequirement,
    ) -> crate::traversal::optimizer::path_analysis::PathRequirement {
        crate::traversal::optimizer::path_analysis::PathRequirement::None
    }
    /// Whether this step's `process()` genuinely owns new heap data at the end of execution
    /// (e.g. a terminal materialization, `group()`/`fold()`/`sum()`/`mean()`/`median()` building
    /// an aggregation buffer, `path()` building a `Path` object, `element_map()` constructing an
    /// owned map). Default `false`: most steps only pass lazy handles through
    /// (`GraphNodeIndex`/`GraphEdgeIndex`/`VertexProperty`/`EdgeProperty`), per rule 4's
    /// lazy-evaluation discipline. Used by `.profile()` (D3 of the memory-inspection spec) to
    /// decide, with no per-item work, whether a step's `bytes_materialized` figure is worth
    /// computing at all.
    fn materializes(&self) -> bool {
        false
    }
    /// The side-effect key this step populates (`tree("key")`), if any — a second way, besides
    /// `as()`, that a label read by `select()`/`cap()`/`where(within(...))` can be declared
    /// somewhere in the query. `None` for every step that isn't a keyed side effect.
    fn declares_side_effect_key(&self) -> Option<&str> {
        None
    }
    fn clone_box(&self) -> Box<dyn GraphStepBarrier<'g, S> + 'g>;
}

pub(crate) trait GraphStepFlatBarrier<'g, S: GraphStorage + 'g>:
    Send + Sync + fmt::Display
{
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Vec<Traverser<S>>,
    ) -> TraverserResult<Option<Vec<Traverser<S>>>>;
    /// The text query of `nearest("emb", "text", k)` with its property key; see
    /// [`GraphStepMap::text_query_mut`].
    fn text_query_mut(
        &mut self,
    ) -> Option<(
        Option<&crate::PropKey>,
        &mut crate::traversal::step::map_steps::step_similarity::TextQuery,
    )> {
        None
    }
    /// The bound text query; see [`GraphStepMap::text_query`].
    fn text_query(&self) -> Option<&crate::traversal::step::map_steps::step_similarity::TextQuery> {
        None
    }
    /// Attaches a `from()`/`to()` modulator argument list. The default rejects it: only a
    /// step that overrides this and returns [`ArgsModulation::Accepted`] takes `from()`/
    /// `to()` (TinkerPop's `FromToModulating`: `add_e()` today), so a step can never swallow
    /// one silently.
    fn push_modulator_args(&mut self, _args: Vec<String>, _modulator_name: &str) -> ArgsModulation {
        ArgsModulation::Unsupported
    }
    /// Attaches a `by()` modulator. The default rejects it: only a step that overrides this and
    /// returns [`ByModulation::Accepted`] takes `by()`, so a step can never swallow one silently.
    fn push_modulator_by(&mut self, _args: By<'g, S>, _modulator_name: &str) -> ByModulation {
        ByModulation::Unsupported
    }
    fn get_name(&self) -> String {
        self.to_string()
    }
    /// The child traversals of this step in their canonical order; see
    /// [`GraphStep::child_traversals_mut`]. Steps without children keep the default.
    fn child_traversals_mut(&mut self) -> Vec<&mut AnonymousTraversal<'g, S>> {
        Vec::new()
    }
    /// What this step reads from the path of its incoming traversers. Steps that read
    /// `Traverser.path` must override it, so the path requirement analysis records it for them.
    fn path_requirement(&self) -> crate::traversal::optimizer::path_analysis::PathRequirement {
        crate::traversal::optimizer::path_analysis::PathRequirement::None
    }
    /// The step labels whose every occurrence on the path this step reads (the edges of a GQL
    /// quantified edge for its trail check or its group variable), besides the latest one a
    /// `select()` reads. The path analysis keeps such a label upstream of the `as()` that sets it
    /// again, so earlier occurrences survive (`PathAnalysis`'s history labels). Empty by default;
    /// only with the `gql` feature (no other step declares any).
    #[cfg(feature = "gql")]
    fn history_labels(&self) -> &[compact_str::CompactString] {
        &[]
    }
    /// Analyses the child traversals of this step, given what the steps after it read, and
    /// returns what the children read from the incoming path. Steps without children keep the
    /// default.
    fn analyze_nested_paths(
        &mut self,
        _analysis: &crate::traversal::optimizer::path_analysis::PathAnalysis,
        _downstream: &crate::traversal::optimizer::path_analysis::PathRequirement,
    ) -> crate::traversal::optimizer::path_analysis::PathRequirement {
        crate::traversal::optimizer::path_analysis::PathRequirement::None
    }
    /// Whether this step keeps state across the traversers it sees that a per-traverser run of
    /// its enclosing child would lose: a `repeat()` whose body holds a `dedup()` (the seen-set
    /// lives for the whole loop). `union()`/`choose()`/`branch()` then run such a child as one
    /// batch. Only `repeat()` overrides it.
    fn holds_loop_state(&self) -> bool {
        false
    }
    /// The step as a `repeat()`, so `times()`/`until()`/`emit()` can attach to it. Only
    /// `repeat()` overrides it.
    fn as_repeat_mut(
        &mut self,
    ) -> Option<&mut crate::traversal::step::flatbarrier_steps::step_repeat::StepRepeat<'g, S>>
    {
        None
    }
    /// Whether this step's `process()` genuinely owns new heap data at the end of execution
    /// (e.g. a terminal materialization, `group()`/`fold()`/`sum()`/`mean()`/`median()` building
    /// an aggregation buffer, `path()` building a `Path` object, `element_map()` constructing an
    /// owned map). Default `false`: most steps only pass lazy handles through
    /// (`GraphNodeIndex`/`GraphEdgeIndex`/`VertexProperty`/`EdgeProperty`), per rule 4's
    /// lazy-evaluation discipline. Used by `.profile()` (D3 of the memory-inspection spec) to
    /// decide, with no per-item work, whether a step's `bytes_materialized` figure is worth
    /// computing at all.
    fn materializes(&self) -> bool {
        false
    }
    /// The side-effect key this step populates (`tree("key")`), if any; see
    /// [`GraphStepBarrier::declares_side_effect_key`].
    fn declares_side_effect_key(&self) -> Option<&str> {
        None
    }
    fn clone_box(&self) -> Box<dyn GraphStepFlatBarrier<'g, S> + 'g>;
}

pub(crate) trait GraphStepFilter<'g, S: GraphStorage + 'g>:
    Send + Sync + fmt::Display
{
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: &Traverser<S>,
    ) -> TraverserResult<bool>;
    /// How much of `source`'s bulk survives this filter: all of it or none for a deterministic
    /// filter (the default), possibly a part of it for a filter that draws per unit of bulk
    /// (`coin()`). Only the executor calls it; a child traversal asks `process` with a bulk of 1.
    fn surviving_bulk(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: &Traverser<S>,
    ) -> TraverserResult<u64> {
        Ok(if self.process(context, source)? {
            source.bulk
        } else {
            0
        })
    }
    fn labels(&self) -> Option<&[String]> {
        None
    }
    fn ids(&self) -> Option<&[String]> {
        None
    }
    #[allow(dead_code)]
    fn keys(&self) -> Option<&[String]> {
        None
    }
    /// The condition of a `has(key, value)` on a literal property name, which the
    /// `source_filter_pushdown` rule may fold into a `v()`/`e()` source step (see
    /// [`PropertyFilter`]). Default `None`: the step cannot be folded.
    fn property_equality(&self) -> Option<PropertyFilter> {
        None
    }
    /// The condition of a `has(key, P)` on a literal property name as the slices of an ordered
    /// property index that hold every value it can accept (book page "Property Indexes"):
    /// `(key, ranges)`. Default `None`: no index can serve it.
    fn index_condition(&self) -> Option<IndexCondition<'_>> {
        None
    }
    /// The predicate of an `is(P)` on the incoming value as the slices of an ordered property
    /// index that hold every value it can accept: read by the `property_index` rule for the
    /// child of `has(key, __.to_lower().is(P))`. Default `None`.
    fn value_index_ranges(&self) -> Option<Vec<crate::graph::PropertyIndexRange>> {
        None
    }
    fn as_where_traversal(&self) -> Option<&AnonymousTraversal<'g, S>> {
        None
    }
    /// Called once on the first step of a `where(traversal)` child: `and()`/`or()`/`not()` turn
    /// a leading/trailing `as()` of their own children into start/end labels (see
    /// `filter_steps/where_scope.rs`); every other step keeps the default no-op.
    fn configure_where_scope(&mut self) {}
    /// Whether this `where(traversal)` has a start or end label, so the optimizer must not
    /// flatten its child into the surrounding steps.
    fn has_scope_labels(&self) -> bool {
        false
    }
    /// A problem found when the step was built (an invalid start/end label of a `where()`
    /// child), raised by the plan-time validation pass before execution.
    fn plan_check(&self) -> TraverserResult<()> {
        Ok(())
    }
    fn into_inner_steps(self: Box<Self>) -> Option<Vec<GraphStep<'g, S>>> {
        None
    }
    /// Attaches a `from()`/`to()` modulator argument list. The default rejects it: only a
    /// step that overrides this and returns [`ArgsModulation::Accepted`] takes `from()`/
    /// `to()` (TinkerPop's `FromToModulating`: `add_e()` today), so a step can never swallow
    /// one silently.
    fn push_modulator_args(&mut self, _args: Vec<String>, _modulator_name: &str) -> ArgsModulation {
        ArgsModulation::Unsupported
    }
    /// Attaches a `by()` modulator. The default rejects it: only a step that overrides this and
    /// returns [`ByModulation::Accepted`] takes `by()`, so a step can never swallow one silently.
    fn push_modulator_by(&mut self, _args: By<'g, S>, _modulator_name: &str) -> ByModulation {
        ByModulation::Unsupported
    }
    fn get_name(&self) -> String {
        self.to_string()
    }
    /// The child traversals of this step in their canonical order; see
    /// [`GraphStep::child_traversals_mut`]. Steps without children keep the default.
    fn child_traversals_mut(&mut self) -> Vec<&mut AnonymousTraversal<'g, S>> {
        Vec::new()
    }
    /// What this step reads from the path of its incoming traversers. Steps that read
    /// `Traverser.path` must override it, so the path requirement analysis records it for them.
    fn path_requirement(&self) -> crate::traversal::optimizer::path_analysis::PathRequirement {
        crate::traversal::optimizer::path_analysis::PathRequirement::None
    }
    /// The step labels whose every occurrence on the path this step reads (the edges of a GQL
    /// quantified edge for its trail check or its group variable), besides the latest one a
    /// `select()` reads. The path analysis keeps such a label upstream of the `as()` that sets it
    /// again, so earlier occurrences survive (`PathAnalysis`'s history labels). Empty by default;
    /// only with the `gql` feature (no other step declares any).
    #[cfg(feature = "gql")]
    fn history_labels(&self) -> &[compact_str::CompactString] {
        &[]
    }
    /// Analyses the child traversals of this step, given what the steps after it read, and
    /// returns what the children read from the incoming path. Steps without children keep the
    /// default.
    fn analyze_nested_paths(
        &mut self,
        _analysis: &crate::traversal::optimizer::path_analysis::PathAnalysis,
        _downstream: &crate::traversal::optimizer::path_analysis::PathRequirement,
    ) -> crate::traversal::optimizer::path_analysis::PathRequirement {
        crate::traversal::optimizer::path_analysis::PathRequirement::None
    }
    /// Whether this step's `process()` genuinely owns new heap data at the end of execution
    /// (e.g. a terminal materialization, `group()`/`fold()`/`sum()`/`mean()`/`median()` building
    /// an aggregation buffer, `path()` building a `Path` object, `element_map()` constructing an
    /// owned map). Default `false`: most steps only pass lazy handles through
    /// (`GraphNodeIndex`/`GraphEdgeIndex`/`VertexProperty`/`EdgeProperty`), per rule 4's
    /// lazy-evaluation discipline. Used by `.profile()` (D3 of the memory-inspection spec) to
    /// decide, with no per-item work, whether a step's `bytes_materialized` figure is worth
    /// computing at all.
    fn materializes(&self) -> bool {
        false
    }
    fn clone_box(&self) -> Box<dyn GraphStepFilter<'g, S> + 'g>;
}

macro_rules! impl_box_clone {
    ($trait:ident) => {
        impl<'g, S: GraphStorage + 'g> Clone for Box<dyn $trait<'g, S> + 'g> {
            fn clone(&self) -> Self {
                self.clone_box()
            }
        }
    };
}

impl_box_clone!(GraphStepCount);
impl_box_clone!(GraphStepModulator);
impl_box_clone!(GraphStepSideEffectMut);
impl_box_clone!(GraphStepSideEffect);
impl_box_clone!(GraphStepAs);
impl_box_clone!(GraphStepInit);
impl_box_clone!(GraphStepAppend);
impl_box_clone!(GraphStepMap);
impl_box_clone!(GraphStepFlatMap);
impl_box_clone!(GraphStepBarrier);
impl_box_clone!(GraphStepFlatBarrier);
impl_box_clone!(GraphStepFilter);
