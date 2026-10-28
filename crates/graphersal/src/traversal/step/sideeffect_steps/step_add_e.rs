use crate::graph::GraphStorage;
use crate::side_effect::SideEffectValue;
use crate::traversal::optimizer::path_analysis::{PathAnalysis, PathRequirement};
use crate::*;

/// One endpoint argument of `from()`/`to()` on `add_e()`.
enum Endpoint<'g, S: GraphStorage + 'g> {
    /// A step label, a side-effect key or a vertex id (see [`StepAddE::process`] for the order).
    Name(String),
    /// A child traversal; its first result is the endpoint (TinkerPop's `TraversalUtil.produce`).
    Traversal(AnonymousTraversal<'g, S>),
}

impl<'g, S: GraphStorage + 'g> Clone for Endpoint<'g, S> {
    fn clone(&self) -> Self {
        match self {
            Endpoint::Name(name) => Endpoint::Name(name.clone()),
            Endpoint::Traversal(traversal) => Endpoint::Traversal(traversal.clone()),
        }
    }
}

impl<'g, S: GraphStorage + 'g> fmt::Display for Endpoint<'g, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Endpoint::Name(name) => f.write_str(&crate::value::literal::rhai_string(name)),
            Endpoint::Traversal(traversal) => write!(f, "{traversal}"),
        }
    }
}

/// The label argument of `add_e()`.
enum AddLabel<'g, S: GraphStorage + 'g> {
    /// `add_e(None)`: an unlabeled edge.
    None,
    Name(String),
    /// `add_e(__.select("b").label())`: the first result of the traversal, a string.
    Traversal(AnonymousTraversal<'g, S>),
    /// The caller passed a list where `add_e()` takes a single label. Validation is deferred to
    /// `process()` instead of panicking at construction time, since `add_e()` returns `Self` (not
    /// `Result`) to stay fluent.
    Invalid,
}

impl<'g, S: GraphStorage + 'g> Clone for AddLabel<'g, S> {
    fn clone(&self) -> Self {
        match self {
            AddLabel::None => AddLabel::None,
            AddLabel::Name(name) => AddLabel::Name(name.clone()),
            AddLabel::Traversal(traversal) => AddLabel::Traversal(traversal.clone()),
            AddLabel::Invalid => AddLabel::Invalid,
        }
    }
}

impl<'g, S: GraphStorage + 'g> fmt::Display for AddLabel<'g, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AddLabel::None => Ok(()),
            AddLabel::Name(name) => f.write_str(&crate::value::literal::rhai_string(name)),
            AddLabel::Traversal(traversal) => write!(f, "{traversal}"),
            AddLabel::Invalid => write!(f, "<invalid>"),
        }
    }
}

struct StepAddE<'g, S: GraphStorage + 'g> {
    label: AddLabel<'g, S>,
    from: Option<Vec<Endpoint<'g, S>>>,
    to: Option<Vec<Endpoint<'g, S>>>,
    /// The user-supplied id from a folded `property(T.id, v)`; `None` lets storage assign one.
    id: Option<CompactString>,
    /// The properties every created edge gets, folded in by the `add_property_fold` rule; empty
    /// when the rule did not fire. Built once per step, cloned per created edge.
    properties: PropertyMap,
    /// The time of `defaultFunction` `now`/`today` the script DSL pinned for this step (the
    /// evaluation's `now()`); `None`: the execution's ([`TraverserContext::create_context`]).
    create_time: Option<crate::value::temporal::DateTime>,
}

impl<'g, S: GraphStorage + 'g> Clone for StepAddE<'g, S> {
    fn clone(&self) -> Self {
        Self {
            label: self.label.clone(),
            from: self.from.clone(),
            to: self.to.clone(),
            id: self.id.clone(),
            properties: self.properties.clone(),
            create_time: self.create_time,
        }
    }
}

/// `.from(..)` / `.to(..)` as written, nothing when the endpoint is not set.
fn render_endpoints<S: GraphStorage>(
    modulator: &str,
    endpoints: &Option<Vec<Endpoint<'_, S>>>,
) -> String {
    match endpoints {
        None => String::new(),
        Some(list) => {
            let args: Vec<String> = list.iter().map(|endpoint| endpoint.to_string()).collect();
            format!(".{modulator}({})", args.join(", "))
        }
    }
}

impl<'g, S: GraphStorage + 'g> fmt::Display for StepAddE<'g, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "add_e({}", self.label)?;
        if !self.properties.is_empty() {
            let sep = if matches!(self.label, AddLabel::None) {
                ""
            } else {
                ", "
            };
            let keys: Vec<&str> = self.properties.keys().map(|k| k.as_str()).collect();
            write!(
                f,
                "{sep}properties: {}",
                crate::value::literal::rhai_string_array(&keys)
            )?;
        }
        write!(
            f,
            "){}{}",
            render_endpoints("from", &self.from),
            render_endpoints("to", &self.to)
        )?;
        if let Some(id) = &self.id {
            write!(
                f,
                ".property(T.id, {})",
                crate::value::literal::rhai_string(id)
            )?;
        }
        Ok(())
    }
}

impl<'g, S: GraphStorage + 'g> AddElementStep for StepAddE<'g, S> {
    fn set_id(&mut self, id: CompactString) {
        self.id = Some(id);
    }

    fn set_label(&mut self, _label: CompactString) -> bool {
        false
    }

    fn fold_property(&mut self, key: CompactString, value: ElementProperty) {
        self.properties.insert(key, value);
    }
}

fn add_e_step<'g, S: GraphStorage + 'g>(label: AddLabel<'g, S>) -> GraphStep<'g, S> {
    GraphStep::SideEffectMut(
        "add_e",
        Box::new(StepAddE {
            label,
            from: None,
            to: None,
            id: None,
            properties: PropertyMap::default(),
            create_time: None,
        }),
    )
}

fn plain_label<'g, S: GraphStorage + 'g>(label: ArgStrings) -> AddLabel<'g, S> {
    match label {
        ArgStrings::None => AddLabel::None,
        ArgStrings::String(v) => AddLabel::Name(v),
        ArgStrings::List(_) => AddLabel::Invalid,
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Adds an edge from the incoming vertex (or, with `from()`/`to()`, between the named
    /// vertices). `from()`/`to()` come right after it, with any number of `property()` calls in
    /// between (`add_e("knows").property("weight", 1).from("a").to("b")`); an endpoint is a step
    /// label, a side-effect key, a vertex id or a traversal (see [`Self::from_by`]). A traversal
    /// label is given with [`Self::add_e_by`].
    ///
    /// Deviations from TinkerPop (also on the "addE() endpoints" page of the user's guide): a
    /// name that is neither a path label nor a side-effect key is treated as a vertex id
    /// (`to("2")`; TinkerPop fails); a list of names creates one edge per name (TinkerPop takes
    /// one endpoint); endpoint ids compare as strings (`2` and `"2"` are the same id).
    pub fn add_e<T: Into<ArgStrings>>(self, label: T) -> Self {
        self.push_step(add_e_step(plain_label(label.into())))
    }

    /// `add_e(traversal)`: the label is the first result of `traversal`, run for each incoming
    /// traverser; it must be a string.
    pub fn add_e_by(self, label: AnonymousTraversal<'g, S>) -> Self {
        self.push_step(add_e_step(AddLabel::Traversal(label)))
    }
}

impl __ {
    /// Adds an edge with the given label; its endpoints come from from() and to() (see the book
    /// page addE() Endpoints). Starts an anonymous traversal (`__.add_e()`).
    pub fn add_e<'g, S: GraphStorage + 'g, T: Into<ArgStrings>>(
        labels: T,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().add_e(labels)
    }

    /// `__.add_e(traversal)`; see [`GraphTraversalSource::add_e_by`].
    pub fn add_e_by<'g, S: GraphStorage + 'g>(
        label: AnonymousTraversal<'g, S>,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().add_e_by(label)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// `add_e()` inside a child traversal; see [`GraphTraversalSource::add_e`].
    pub fn add_e<T: Into<ArgStrings>>(self, label: T) -> Self {
        self.push_step(add_e_step(plain_label(label.into())))
    }

    /// `add_e(traversal)` inside a child traversal; see [`GraphTraversalSource::add_e_by`].
    pub fn add_e_by(self, label: AnonymousTraversal<'g, S>) -> Self {
        self.push_step(add_e_step(AddLabel::Traversal(label)))
    }
}

fn kind_of<S: GraphStorage>(value: &TraverserValue<S>) -> &'static str {
    match value {
        TraverserValue::Vertex(_) => "vertex",
        TraverserValue::VertexProperty(..) => "vertexProperty",
        TraverserValue::Edge(_) => "edge",
        TraverserValue::EdgeProperty(..) => "edgeProperty",
        TraverserValue::Path(_) => "path",
        TraverserValue::PathProperty(_) => "pathProperty",
        TraverserValue::Entry(_) => "entry",
        TraverserValue::Object(_) | TraverserValue::Record { .. } => "object",
        TraverserValue::Array(_) => "array",
        TraverserValue::Schema(_) => "schema",
        TraverserValue::Graph(_) => "graph",
        TraverserValue::Computed(..) => "computed",
    }
}

fn not_a_vertex(actual: impl Into<String>) -> Box<TraverserError> {
    Box::new(TraverserError::cast_exception_str(
        &["vertex", "vertex id"],
        actual.into(),
    ))
}

/// The vertex an endpoint value stands for: a vertex, or the id of a vertex present in the graph
/// (a string, or an integer as its decimal text, as `property(T.id, ..)` converts ids).
fn resolve_vertex<S: GraphStorage>(
    context: &TraverserContext<'_, S>,
    value: TraverserValue<S>,
) -> TraverserResult<S::VertexId> {
    match &value {
        TraverserValue::Vertex(index) => return Ok(*index),
        // A lazily read value (`values("name")`, `id()`), as opposed to a property element.
        TraverserValue::Computed(..) => {}
        TraverserValue::VertexProperty(_, _, kind) | TraverserValue::EdgeProperty(_, _, kind)
            if kind.is_value() => {}
        other => return Err(not_a_vertex(kind_of(other))),
    }
    let id: CompactString = match value.into_node_property(context.storage)? {
        ElementProperty::String(text) => text,
        ElementProperty::Number(number) => match number.as_i64() {
            Some(int) => compact_str::format_compact!("{int}"),
            None => return Err(not_a_vertex("number")),
        },
        ElementProperty::Null => return Err(not_a_vertex("null")),
        _ => return Err(not_a_vertex("computed")),
    };
    context
        .storage
        .get_vertex_by_id(&id)?
        .ok_or_else(|| not_a_vertex("null"))
}

/// The vertex a name stands for: the object last labelled so on the path (`as()`), else the side
/// effect of that key (`withSideEffect`, a single value only), else a vertex of that id. The
/// first two are what `select(name)` reads; the vertex id is a Graphersal extension.
fn resolve_name<S: GraphStorage>(
    context: &TraverserContext<'_, S>,
    path: PathRef,
    name: &str,
) -> TraverserResult<S::VertexId> {
    if let Some(value) = context.paths.by_label(path, name) {
        return resolve_vertex(context, value);
    }
    match context.side_effect.get(name) {
        Some(SideEffectValue::Value(value)) => return resolve_vertex(context, value.clone()),
        Some(SideEffectValue::Reduced { .. }) => return Err(not_a_vertex("reduced value")),
        Some(SideEffectValue::List(_)) | Some(SideEffectValue::Set(_)) => {
            return Err(not_a_vertex("array"));
        }
        Some(SideEffectValue::Subgraph { .. }) => return Err(not_a_vertex("graph")),
        Some(SideEffectValue::Group(_)) => return Err(not_a_vertex("map")),
        None => {}
    }
    context
        .storage
        .get_vertex_by_id(name)?
        .ok_or_else(|| not_a_vertex("null"))
}

/// The incoming traverser as the implicit endpoint: a vertex, a vertex property or a computed
/// value linked to its vertex.
fn incoming_vertex<S: GraphStorage>(
    context: &TraverserContext<'_, S>,
    value: &TraverserValue<S>,
) -> TraverserResult<S::VertexId> {
    let index = match value {
        TraverserValue::VertexProperty(_, index, _) | TraverserValue::Vertex(index) => *index,
        TraverserValue::Computed(_, Some(index)) => *index,
        other => {
            return Err(Box::new(TraverserError::cast_exception_str(
                &["vertex", "vertexProperty (linked)", "computed (linked)"],
                kind_of(other).to_string(),
            )));
        }
    };
    // A vertex dropped earlier in the query (its handle is stale) cannot be an endpoint.
    if !context.storage.contains_vertex(index)? {
        return Err(Box::new(GraphError::vertex_removed().into()));
    }
    Ok(index)
}

impl<'g, S: GraphStorage + 'g> StepAddE<'g, S> {
    fn resolve_endpoints(
        context: &mut TraverserContext<'_, S>,
        endpoints: &[Endpoint<'g, S>],
        parent: &Traverser<S>,
    ) -> TraverserResult<Vec<S::VertexId>> {
        let mut out = Vec::with_capacity(endpoints.len());
        for endpoint in endpoints {
            let id = match endpoint {
                Endpoint::Name(name) => resolve_name(context, parent.path, name)?,
                Endpoint::Traversal(child) => match child.first(context, parent)? {
                    Some(found) => resolve_vertex(context, found.value)?,
                    None => return Err(not_a_vertex("null")),
                },
            };
            out.push(id);
        }
        Ok(out)
    }

    fn resolve_label(
        &self,
        context: &mut TraverserContext<'_, S>,
        parent: &Traverser<S>,
    ) -> TraverserResult<Option<String>> {
        match &self.label {
            AddLabel::None => Ok(None),
            AddLabel::Name(name) => Ok(Some(name.clone())),
            AddLabel::Invalid => Err(Box::new(TraverserError::QueryExecution {
                query: "add_e".to_string(),
                reason: "an array is not allowed as the 'label' argument for add_e(); pass a single string".to_string(),
            })),
            AddLabel::Traversal(child) => match child.first(context, parent)? {
                Some(found) => match found.value.into_node_property(context.storage)? {
                    ElementProperty::String(text) => Ok(Some(text.to_string())),
                    other => Err(Box::new(TraverserError::cast_exception_str(
                        &["string"],
                        crate::graph::schema::value_type_name(&other).to_string(),
                    ))),
                },
                None => Err(Box::new(TraverserError::cast_exception_str(&["string"], "null".to_string()))),
            },
        }
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepSideEffectMut<'g, S> for StepAddE<'g, S> {
    fn property_access(
        &self,
        visit: &mut dyn FnMut(Option<&str>, crate::graph::workload::AccessKind),
    ) {
        for key in self.properties.keys() {
            visit(Some(key), crate::graph::workload::AccessKind::Write);
        }
    }
    fn set_create_time(&mut self, now: crate::value::temporal::DateTime) {
        self.create_time = Some(now);
    }

    fn static_labels(&self) -> Vec<&str> {
        match &self.label {
            AddLabel::Name(name) => vec![name.as_str()],
            AddLabel::None | AddLabel::Traversal(_) | AddLabel::Invalid => Vec::new(),
        }
    }

    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Option<Traverser<S>>,
    ) -> TraverserResult<Vec<Traverser<S>>> {
        if let AddLabel::Invalid = self.label {
            return Err(Box::new(TraverserError::QueryExecution {
                query: "add_e".to_string(),
                reason: "an array is not allowed as the 'label' argument for add_e(); pass a single string".to_string(),
            }));
        }
        // The parent of the label and endpoint traversals and the carrier of the path: the
        // incoming traverser, or a seed holding `null` for the start form (`g.add_e(..)`).
        let seed;
        let parent: &Traverser<S> = match &source {
            Some(traverser) => traverser,
            None => {
                seed = Traverser::start(
                    context,
                    TraverserValue::Computed(ElementProperty::Null, None),
                );
                &seed
            }
        };
        let (path, bulk, sack) = (parent.path, parent.bulk, parent.sack);

        let label = self.resolve_label(context, parent)?;
        let mut result: Vec<Traverser<S>> = vec![];

        let (from_ids, to_ids): (Vec<S::VertexId>, Vec<S::VertexId>) = match (&self.from, &self.to)
        {
            (Some(from), Some(to)) => (
                Self::resolve_endpoints(context, from, parent)?,
                Self::resolve_endpoints(context, to, parent)?,
            ),
            (from, to) => match &source {
                Some(traverser) => {
                    let incoming = incoming_vertex(context, &traverser.value)?;
                    match (from, to) {
                        (None, Some(to)) => (
                            vec![incoming],
                            Self::resolve_endpoints(context, to, parent)?,
                        ),
                        (Some(from), None) => (
                            Self::resolve_endpoints(context, from, parent)?,
                            vec![incoming],
                        ),
                        // Neither from() nor to(): TinkerPop's AddEdgeStep uses the incoming
                        // vertex for both ends, a self-loop.
                        _ => (vec![incoming], vec![incoming]),
                    }
                }
                None => {
                    let missing = if self.from.is_none() {
                        "from() (and to())"
                    } else {
                        "to() (and from())"
                    };
                    return Err(Box::new(TraverserError::QueryExecution {
                        query: "add_e".to_string(),
                        reason: format!(
                            "add_e() as a start step has no incoming vertex, so it needs {missing}"
                        ),
                    }));
                }
            },
        };

        let create = context.create_context_or(self.create_time)?;
        for index_from in &from_ids {
            for index_to in &to_ids {
                let index = context.storage.add_edge_with(
                    self.id.clone(),
                    label.clone().map(Into::into),
                    *index_from,
                    *index_to,
                    self.properties.clone(),
                    &create,
                )?;
                result.push(Traverser::with_parts(
                    TraverserValue::Edge(index),
                    path,
                    bulk,
                    sack,
                ));
            }
        }
        Ok(result)
    }

    fn path_requirement(&self) -> PathRequirement {
        // `from()`/`to()` name either a step label or a vertex id; the label is read from the path.
        PathRequirement::labels(self.from.iter().chain(self.to.iter()).flatten().filter_map(
            |endpoint| match endpoint {
                Endpoint::Name(name) => Some(name.as_str()),
                Endpoint::Traversal(_) => None,
            },
        ))
    }

    fn child_traversals_mut(&mut self) -> Vec<&mut crate::traversal::AnonymousTraversal<'g, S>> {
        self.children_mut()
    }

    fn analyze_nested_paths(
        &mut self,
        analysis: &PathAnalysis,
        _downstream: &PathRequirement,
    ) -> PathRequirement {
        // Every child runs on the incoming traverser and only its first result is used, so none
        // continues the path.
        self.children_mut()
            .into_iter()
            .fold(PathRequirement::None, |need, child| {
                need.union(child.analyze_paths(analysis, PathRequirement::None))
            })
    }

    fn as_add_element_mut(&mut self) -> Option<&mut dyn AddElementStep> {
        Some(self)
    }

    fn push_modulator_args(&mut self, args: Vec<String>, modulator_name: &str) -> ArgsModulation {
        let names = Some(args.into_iter().map(Endpoint::Name).collect());
        if modulator_name.eq("from") {
            self.from = names;
        } else if modulator_name.eq("to") {
            self.to = names;
        }
        ArgsModulation::Accepted
    }

    fn push_modulator_traversal(
        &mut self,
        child: AnonymousTraversal<'g, S>,
        modulator_name: &str,
    ) -> ArgsModulation {
        let endpoint = Some(vec![Endpoint::Traversal(child)]);
        if modulator_name.eq("from") {
            self.from = endpoint;
        } else if modulator_name.eq("to") {
            self.to = endpoint;
        }
        ArgsModulation::Accepted
    }

    fn clone_box(&self) -> Box<dyn GraphStepSideEffectMut<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

impl<'g, S: GraphStorage + 'g> StepAddE<'g, S> {
    /// Every child traversal this step holds: the label and the endpoints.
    fn children_mut(&mut self) -> Vec<&mut AnonymousTraversal<'g, S>> {
        let mut children = Vec::new();
        if let AddLabel::Traversal(child) = &mut self.label {
            children.push(child);
        }
        for endpoints in [&mut self.from, &mut self.to].into_iter().flatten() {
            for endpoint in endpoints {
                if let Endpoint::Traversal(child) = endpoint {
                    children.push(child);
                }
            }
        }
        children
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_none() {
        let mut graph = TraversalGraph::new();
        let n_1 = graph
            .traversal_mut()
            .add_v("quote")
            .next()
            .unwrap()
            .unwrap();
        let n_2 = graph
            .traversal_mut()
            .add_v("product")
            .next()
            .unwrap()
            .unwrap();
        let result = graph
            .traversal_mut()
            .add_e(None)
            .from(n_1)
            .to(n_2)
            .next()
            .unwrap()
            .unwrap();
        match result {
            ElementProperty::Edge(edge) => {
                assert_eq!(edge.id(), "1");
                assert_eq!(edge.label(), None);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn start_label() {
        let mut graph = TraversalGraph::new();
        let n_1 = graph
            .traversal_mut()
            .add_v("quote")
            .next()
            .unwrap()
            .unwrap();
        let n_2 = graph
            .traversal_mut()
            .add_v("product")
            .next()
            .unwrap()
            .unwrap();
        let result = graph
            .traversal_mut()
            .add_e("new label")
            .from(n_1)
            .to(n_2)
            .next()
            .unwrap()
            .unwrap();
        match result {
            ElementProperty::Edge(edge) => {
                assert_eq!(edge.id(), "1");
                assert_eq!(edge.label(), Some("new label"));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn vertex_to() {
        let mut graph = TraversalGraph::new();
        let n_1 = graph
            .traversal_mut()
            .add_v("quote")
            .next()
            .unwrap()
            .unwrap();
        let n_2 = graph
            .traversal_mut()
            .add_v("product")
            .next()
            .unwrap()
            .unwrap();
        let result = graph
            .traversal_mut()
            .v(n_1)
            .add_e(None)
            .to(n_2)
            .next()
            .unwrap()
            .unwrap();
        match result {
            ElementProperty::Edge(edge) => {
                assert_eq!(edge.id(), "1");
                assert_eq!(edge.label(), None);
                assert_eq!(edge.out_v().id(), "1");
                assert_eq!(edge.in_v().id(), "2");
            }
            t => panic!("PANIC: {:?}", t),
        }
    }

    #[test]
    fn vertex_from() {
        let mut graph = TraversalGraph::new();
        let n_1 = graph
            .traversal_mut()
            .add_v("quote")
            .next()
            .unwrap()
            .unwrap();
        let n_2 = graph
            .traversal_mut()
            .add_v("product")
            .next()
            .unwrap()
            .unwrap();
        let result = graph
            .traversal_mut()
            .v(n_1)
            .add_e("new label")
            .from(n_2)
            .next()
            .unwrap()
            .unwrap();
        match result {
            ElementProperty::Edge(edge) => {
                assert_eq!(edge.id(), "1");
                assert_eq!(edge.label(), Some("new label"));
                assert_eq!(edge.out_v().id(), "2");
                assert_eq!(edge.in_v().id(), "1");
            }
            _ => panic!(),
        }
    }

    #[test]
    fn profile() {
        let mut graph = TraversalGraph::tinkerpop_modern();
        let n_1 = graph.traversal_mut().v("1").next().unwrap().unwrap();
        let n_2 = graph.traversal_mut().v("2").next().unwrap().unwrap();
        let result = graph
            .traversal_mut()
            .v(n_1)
            .add_e("new label")
            .from(n_2)
            .profile()
            .unwrap();
        assert!(result.to_string().contains("add_e("));
    }

    #[test]
    fn list_label_is_a_query_error_not_a_panic() {
        let mut graph = TraversalGraph::new();
        let n_1 = graph
            .traversal_mut()
            .add_v("quote")
            .next()
            .unwrap()
            .unwrap();
        let n_2 = graph
            .traversal_mut()
            .add_v("product")
            .next()
            .unwrap()
            .unwrap();
        let err = graph
            .traversal_mut()
            .add_e(vec!["a", "b"])
            .from(n_1)
            .to(n_2)
            .next()
            .expect_err("a list label must not panic at construction time");
        assert!(matches!(
            err.root_cause(),
            TraverserError::QueryExecution { .. }
        ));
    }
}

/*
gremlin> g.V(1).as('a').out('created').in('created').where(neq('a')).
           addE('co-developer').from('a').property('year',2009) //// (1)
==>e[0][1-co-developer->4]
==>e[13][1-co-developer->6]

g.v("1").as_("a").out("created").in_("created").where_p(P.neq("a")).add_e("co-developer").from("a").property("year", 2009);
Error("add_e", "<no data>", CastException(["vertex"], "null"))
*/
