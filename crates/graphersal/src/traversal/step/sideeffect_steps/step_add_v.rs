use crate::graph::GraphStorage;
use crate::*;

#[derive(Default, Clone)]
struct StepAddV {
    /// The vertex's initial label set, in the order given. `None` means no labels (was
    /// `add_v()`/`add_v(None)`); `Some(vec![])` never occurs — an empty list argument still yields
    /// `None` here, matching `add_v()`'s no-label call.
    args: Option<Vec<String>>,
    /// The user-supplied id from a folded `property(T.id, v)`; `None` lets storage assign one.
    id: Option<CompactString>,
    /// The properties the vertex is created with, folded in by the `add_property_fold` rule;
    /// empty when the rule did not fire. Built once per step, cloned per created vertex.
    properties: PropertyMap,
    /// The time of `defaultFunction` `now`/`today` the script DSL pinned for this step (the
    /// evaluation's `now()`); `None`: the execution's ([`TraverserContext::create_context`]).
    create_time: Option<crate::value::temporal::DateTime>,
}

impl fmt::Display for StepAddV {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "add_v({}", display_option_vec_string(&self.args))?;
        if !self.properties.is_empty() {
            let sep = if self.args.is_some() { ", " } else { "" };
            let keys: Vec<&str> = self.properties.keys().map(|k| k.as_str()).collect();
            write!(
                f,
                "{sep}properties: {}",
                crate::value::literal::rhai_string_array(&keys)
            )?;
        }
        write!(f, ")")?;
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

impl AddElementStep for StepAddV {
    fn set_id(&mut self, id: CompactString) {
        self.id = Some(id);
    }

    fn set_label(&mut self, label: CompactString) -> bool {
        self.args = Some(vec![label.to_string()]);
        true
    }

    fn fold_property(&mut self, key: CompactString, value: ElementProperty) {
        self.properties.insert(key, value);
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Adds a vertex with the given label; chain property() to set its properties.
    pub fn add_v<T: Into<ArgStrings>>(self, args: T) -> Self {
        let args = match args.into() {
            ArgStrings::None => None,
            ArgStrings::String(v) => Some(vec![v]),
            // A list is the intended way to create a multi-label vertex (D8) — no longer an
            // error shape.
            ArgStrings::List(v) => Some(v),
        };

        let step = GraphStep::SideEffectMut(
            "add_v",
            Box::new(StepAddV {
                args,
                id: None,
                properties: PropertyMap::default(),
                create_time: None,
            }),
        );
        self.push_step(step)
    }
}

impl __ {
    /// Adds a vertex with the given label; chain property() to set its properties. Starts an
    /// anonymous traversal (`__.add_v()`).
    pub fn add_v<'g, S: GraphStorage + 'g, T: Into<ArgStrings>>(
        labels: T,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().add_v(labels)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Adds a vertex with the given label; chain property() to set its properties.
    pub fn add_v<T: Into<ArgStrings>>(self, args: T) -> Self {
        let args = match args.into() {
            ArgStrings::None => None,
            ArgStrings::String(v) => Some(vec![v]),
            ArgStrings::List(v) => Some(v),
        };
        let step = GraphStep::SideEffectMut(
            "add_v",
            Box::new(StepAddV {
                args,
                id: None,
                properties: PropertyMap::default(),
                create_time: None,
            }),
        );
        self.push_step(step)
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepSideEffectMut<'g, S> for StepAddV {
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
        self.args.iter().flatten().map(String::as_str).collect()
    }

    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Option<Traverser<S>>,
    ) -> TraverserResult<Vec<Traverser<S>>> {
        let labels = self
            .args
            .clone()
            .unwrap_or_default()
            .into_iter()
            .map(CompactString::from)
            .collect();
        let create = context.create_context_or(self.create_time)?;
        let node_index = context.storage.add_vertex_with(
            self.id.clone(),
            labels,
            self.properties.clone(),
            &create,
        )?;
        match source {
            // Derived from the incoming traverser (TinkerPop `split`): its path, bulk and sack.
            Some(source) => Ok(vec![source.split(TraverserValue::Vertex(node_index))]),
            None => Ok(vec![Traverser::start(
                context,
                TraverserValue::Vertex(node_index),
            )]),
        }
    }

    fn as_add_element_mut(&mut self) -> Option<&mut dyn AddElementStep> {
        Some(self)
    }

    fn clone_box(&self) -> Box<dyn GraphStepSideEffectMut<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

/// `addV(traversal)`: the label of the new vertex is the first result of a child traversal, run
/// on the incoming traverser (on a `null` seed for the start form `g.addV(..)`), TinkerPop's
/// `AddVertexStep` with a label traversal. The result must be a string; no result or another
/// value is a cast error.
struct StepAddVBy<'g, S: GraphStorage + 'g> {
    label: AnonymousTraversal<'g, S>,
    /// The user-supplied id from a folded `property(T.id, v)`.
    id: Option<CompactString>,
    /// The properties folded in by the `add_property_fold` rule.
    properties: PropertyMap,
    /// The time of `defaultFunction` `now`/`today` the script DSL pinned for this step (the
    /// evaluation's `now()`); `None`: the execution's ([`TraverserContext::create_context`]).
    create_time: Option<crate::value::temporal::DateTime>,
}

impl<'g, S: GraphStorage + 'g> Clone for StepAddVBy<'g, S> {
    fn clone(&self) -> Self {
        Self {
            label: self.label.clone(),
            id: self.id.clone(),
            properties: self.properties.clone(),
            create_time: self.create_time,
        }
    }
}

impl<'g, S: GraphStorage + 'g> fmt::Display for StepAddVBy<'g, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "add_v({}", self.label)?;
        if !self.properties.is_empty() {
            let keys: Vec<&str> = self.properties.keys().map(|k| k.as_str()).collect();
            write!(
                f,
                ", properties: {}",
                crate::value::literal::rhai_string_array(&keys)
            )?;
        }
        write!(f, ")")?;
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

impl<'g, S: GraphStorage + 'g> AddElementStep for StepAddVBy<'g, S> {
    fn set_id(&mut self, id: CompactString) {
        self.id = Some(id);
    }

    /// The label comes from the traversal; a `property(T.label, ..)` stays a separate step.
    fn set_label(&mut self, _label: CompactString) -> bool {
        false
    }

    fn fold_property(&mut self, key: CompactString, value: ElementProperty) {
        self.properties.insert(key, value);
    }
}

fn add_v_by_step<'g, S: GraphStorage + 'g>(label: AnonymousTraversal<'g, S>) -> GraphStep<'g, S> {
    GraphStep::SideEffectMut(
        "add_v",
        Box::new(StepAddVBy {
            label,
            id: None,
            properties: PropertyMap::default(),
            create_time: None,
        }),
    )
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// `addV(traversal)`: adds a vertex whose label is the first result of `label`, run on the
    /// incoming traverser (on a `null` seed when the step starts the traversal); it must be a
    /// string.
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let mut graph = TraversalGraph::tinkerpop_modern();
    /// let label = graph
    ///     .traversal_mut()
    ///     .v("1")
    ///     .add_v_by(__::label())
    ///     .label()
    ///     .next()
    ///     .unwrap();
    /// assert_eq!(label.as_ref().and_then(|l| l.as_string()), Some("person"));
    /// ```
    pub fn add_v_by(self, label: AnonymousTraversal<'g, S>) -> Self {
        self.push_step(add_v_by_step(label))
    }
}

impl __ {
    /// `__.addV(traversal)`; see [`GraphTraversalSource::add_v_by`].
    pub fn add_v_by<'g, S: GraphStorage + 'g>(
        label: AnonymousTraversal<'g, S>,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().add_v_by(label)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// `addV(traversal)` inside a child traversal; see [`GraphTraversalSource::add_v_by`].
    pub fn add_v_by(self, label: AnonymousTraversal<'g, S>) -> Self {
        self.push_step(add_v_by_step(label))
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepSideEffectMut<'g, S> for StepAddVBy<'g, S> {
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

    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Option<Traverser<S>>,
    ) -> TraverserResult<Vec<Traverser<S>>> {
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
        let label = match self.label.first(context, parent)? {
            Some(found) => match found.value.into_node_property(context.storage)? {
                ElementProperty::String(text) => text,
                other => {
                    return Err(Box::new(TraverserError::cast_exception_str(
                        &["string"],
                        crate::graph::schema::value_type_name(&other).to_string(),
                    )));
                }
            },
            None => {
                return Err(Box::new(TraverserError::cast_exception_str(
                    &["string"],
                    "null".to_string(),
                )));
            }
        };
        let create = context.create_context_or(self.create_time)?;
        let node_index = context.storage.add_vertex_with(
            self.id.clone(),
            vec![label],
            self.properties.clone(),
            &create,
        )?;
        match source {
            // Derived from the incoming traverser: its path, bulk and sack.
            Some(source) => Ok(vec![source.split(TraverserValue::Vertex(node_index))]),
            // Generated: the start form.
            None => Ok(vec![Traverser::start(
                context,
                TraverserValue::Vertex(node_index),
            )]),
        }
    }

    fn child_traversals_mut(&mut self) -> Vec<&mut AnonymousTraversal<'g, S>> {
        vec![&mut self.label]
    }

    fn analyze_nested_paths(
        &mut self,
        analysis: &crate::traversal::optimizer::path_analysis::PathAnalysis,
        _downstream: &crate::traversal::optimizer::path_analysis::PathRequirement,
    ) -> crate::traversal::optimizer::path_analysis::PathRequirement {
        // The label traversal runs on the incoming traverser; only its first result is used.
        self.label.analyze_paths(
            analysis,
            crate::traversal::optimizer::path_analysis::PathRequirement::None,
        )
    }

    fn as_add_element_mut(&mut self) -> Option<&mut dyn AddElementStep> {
        Some(self)
    }

    fn clone_box(&self) -> Box<dyn GraphStepSideEffectMut<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_v_by_traversal_label() {
        let mut graph = TraversalGraph::tinkerpop_modern();
        let label = graph
            .traversal_mut()
            .add_v_by(__::constant("prefix_").concat_traversal(vec![__::v("1").label()]))
            .label()
            .next()
            .unwrap();
        assert_eq!(
            label.as_ref().and_then(|l| l.as_string()),
            Some("prefix_person")
        );
    }

    #[test]
    fn add_v_by_non_string_label_is_a_cast_error() {
        let mut graph = TraversalGraph::tinkerpop_modern();
        let err = graph
            .traversal_mut()
            .add_v_by(__::constant(1))
            .next()
            .expect_err("a number is not a label");
        let report = crate::error::format_diagnostic(&err);
        assert!(report.contains("string"), "{report}");
    }

    #[test]
    fn start_none() {
        let mut graph = TraversalGraph::new();
        let result = graph.traversal_mut().add_v(None).next().unwrap().unwrap();
        match result {
            ElementProperty::Vertex(vertex) => {
                assert_eq!(vertex.id(), "1");
                assert_eq!(vertex.label(), None);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn start_label() {
        let mut graph = TraversalGraph::new();
        let result = graph
            .traversal_mut()
            .add_v("product")
            .next()
            .unwrap()
            .unwrap();
        match result {
            ElementProperty::Vertex(vertex) => {
                assert_eq!(vertex.id(), "1");
                assert_eq!(vertex.label(), Some("product"));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn second_none() {
        let mut graph = TraversalGraph::new();
        let result = graph
            .traversal_mut()
            .add_v(None)
            .add_v(None)
            .next()
            .unwrap()
            .unwrap();
        match result {
            ElementProperty::Vertex(vertex) => {
                assert_eq!(vertex.id(), "2");
                assert_eq!(vertex.label(), None);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn second_label() {
        let mut graph = TraversalGraph::new();
        let result = graph
            .traversal_mut()
            .add_v("quote")
            .add_v("product")
            .next()
            .unwrap()
            .unwrap();
        match result {
            ElementProperty::Vertex(vertex) => {
                assert_eq!(vertex.id(), "2");
                assert_eq!(vertex.label(), Some("product"));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn profile() {
        let mut graph = TraversalGraph::new();
        let result = graph
            .traversal_mut()
            .add_v("quote")
            .add_v("product")
            .profile()
            .unwrap();
        assert!(result.to_string().contains("add_v("));
    }

    #[test]
    fn list_label_creates_a_multi_label_vertex() {
        // A list argument is now the intended way to create a multi-label vertex (D8), not an
        // error shape.
        let mut graph = TraversalGraph::new();
        let result = graph
            .traversal_mut()
            .add_v(vec!["Person", "Admin"])
            .next()
            .unwrap()
            .unwrap();
        match result {
            ElementProperty::Vertex(vertex) => {
                assert_eq!(vertex.id(), "1");
                // elementMap()'s "label" key stays single-valued: the primary (first) label.
                assert_eq!(vertex.label(), Some("Person"));
            }
            _ => panic!(),
        }
        let idx = GraphStorage::get_vertex_by_id(&graph, "1")
            .unwrap()
            .unwrap();
        let labels = GraphStorage::vertex_labels(&graph, idx).unwrap();
        assert_eq!(
            labels,
            &[CompactString::new("Person"), CompactString::new("Admin")][..]
        );
    }
}
