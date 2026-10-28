use crate::graph::GraphStorage;
use crate::*;

#[derive(Default, Clone)]
struct StepInE {
    labels: Option<Vec<String>>,
}

impl fmt::Display for StepInE {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "in_e({})", display_option_vec_string(&self.labels))
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Moves to the incoming edges, optionally only edges with the given labels.
    pub fn in_e<T: Into<ArgStrings>>(self, labels: T) -> Self {
        let labels = match labels.into() {
            ArgStrings::None => None,
            ArgStrings::String(v) => Some(vec![v]),
            ArgStrings::List(v) => Some(v),
        };
        self.push_step(GraphStep::FlatMap("in_e", Box::new(StepInE { labels })))
    }
}

impl __ {
    /// Moves to the incoming edges, optionally only edges with the given labels. Starts an
    /// anonymous traversal (`__.in_e()`).
    pub fn in_e<'g, S: GraphStorage + 'g, T: Into<ArgStrings>>(
        labels: T,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().in_e(labels)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Moves to the incoming edges, optionally only edges with the given labels.
    pub fn in_e<T: Into<ArgStrings>>(self, labels: T) -> Self {
        let labels = match labels.into() {
            ArgStrings::None => None,
            ArgStrings::String(v) => Some(vec![v]),
            ArgStrings::List(v) => Some(v),
        };
        let step = GraphStep::FlatMap("in_e", Box::new(StepInE { labels }));
        self.push_step(step)
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepFlatMap<'g, S> for StepInE {
    fn is_adjacency(&self) -> bool {
        true
    }

    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
        output: &mut Vec<Traverser<S>>,
    ) -> TraverserResult<()> {
        let node_index: Option<S::VertexId> = match source.value {
            TraverserValue::Vertex(ni) | TraverserValue::VertexProperty(_, ni, _) => Some(ni),
            TraverserValue::Computed(_, ni) => ni,
            TraverserValue::Path(_) => {
                return Err(Box::new(TraverserError::cast_exception_str(
                    &["vertex", "vertexProperty", "computed (linked to vertex)"],
                    "path".to_string(),
                )));
            }
            TraverserValue::Edge(_) => {
                return Err(Box::new(TraverserError::cast_exception_str(
                    &["vertex", "vertexProperty", "computed (linked to vertex)"],
                    "edge".to_string(),
                )));
            }
            TraverserValue::EdgeProperty(_, _, _) => {
                return Err(Box::new(TraverserError::cast_exception_str(
                    &["vertex", "vertexProperty", "computed (linked to vertex)"],
                    "edgeProperty".to_string(),
                )));
            }
            TraverserValue::PathProperty(_) => {
                return Err(Box::new(TraverserError::cast_exception_str(
                    &["vertex", "vertexProperty", "computed (linked to vertex)"],
                    "pathProperty".to_string(),
                )));
            }
            TraverserValue::Entry(_) => {
                return Err(Box::new(TraverserError::cast_exception_str(
                    &["vertex", "vertexProperty", "computed (linked to vertex)"],
                    "entry".to_string(),
                )));
            }
            TraverserValue::Object(_) | TraverserValue::Record { .. } => {
                return Err(Box::new(TraverserError::cast_exception_str(
                    &["vertex", "vertexProperty", "computed (linked to vertex)"],
                    "object".to_string(),
                )));
            }
            TraverserValue::Array(_) => {
                return Err(Box::new(TraverserError::cast_exception_str(
                    &["vertex", "vertexProperty", "computed (linked to vertex)"],
                    "array".to_string(),
                )));
            }
            TraverserValue::Schema(_) => {
                return Err(Box::new(TraverserError::cast_exception_str(
                    &["vertex", "vertexProperty", "computed (linked to vertex)"],
                    "schema".to_string(),
                )));
            }
            TraverserValue::Graph(_) => {
                return Err(Box::new(TraverserError::cast_exception_str(
                    &["vertex", "vertexProperty", "computed (linked to vertex)"],
                    "graph".to_string(),
                )));
            }
        };

        match node_index {
            Some(node_index) => {
                for edge in context.storage.in_edges(node_index)? {
                    let mut found = false;
                    let edge_label = context.storage.edge_label(edge)?;

                    match &self.labels {
                        Some(labels) => {
                            if let Some(edge_label) = edge_label {
                                found = labels.contains(&edge_label.to_string());
                            }
                        }
                        None => found = true,
                    }
                    if found {
                        output.push(Traverser::with_parts(
                            TraverserValue::Edge(edge),
                            source.path,
                            source.bulk,
                            source.sack,
                        ));
                    }
                }
            }
            None => {
                return Err(Box::new(TraverserError::cast_exception(
                    &["vertex", "vertexProperty", "computed (linked to vertex)"],
                    &source.value,
                )));
            }
        }

        Ok(())
    }

    fn count(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<u64> {
        let node_index: Option<S::VertexId> = match source.value {
            TraverserValue::Vertex(ni) | TraverserValue::VertexProperty(_, ni, _) => Some(ni),
            TraverserValue::Computed(_, ni) => ni,
            TraverserValue::Path(_) => {
                return Err(Box::new(TraverserError::cast_exception_str(
                    &["vertex", "vertexProperty", "computed (linked to vertex)"],
                    "path".to_string(),
                )));
            }
            TraverserValue::Edge(_) => {
                return Err(Box::new(TraverserError::cast_exception_str(
                    &["vertex", "vertexProperty", "computed (linked to vertex)"],
                    "edge".to_string(),
                )));
            }
            TraverserValue::EdgeProperty(_, _, _) => {
                return Err(Box::new(TraverserError::cast_exception_str(
                    &["vertex", "vertexProperty", "computed (linked to vertex)"],
                    "edgeProperty".to_string(),
                )));
            }
            TraverserValue::PathProperty(_) => {
                return Err(Box::new(TraverserError::cast_exception_str(
                    &["vertex", "vertexProperty", "computed (linked to vertex)"],
                    "pathProperty".to_string(),
                )));
            }
            TraverserValue::Entry(_) => {
                return Err(Box::new(TraverserError::cast_exception_str(
                    &["vertex", "vertexProperty", "computed (linked to vertex)"],
                    "entry".to_string(),
                )));
            }
            TraverserValue::Object(_) | TraverserValue::Record { .. } => {
                return Err(Box::new(TraverserError::cast_exception_str(
                    &["vertex", "vertexProperty", "computed (linked to vertex)"],
                    "object".to_string(),
                )));
            }
            TraverserValue::Array(_) => {
                return Err(Box::new(TraverserError::cast_exception_str(
                    &["vertex", "vertexProperty", "computed (linked to vertex)"],
                    "array".to_string(),
                )));
            }
            TraverserValue::Schema(_) => {
                return Err(Box::new(TraverserError::cast_exception_str(
                    &["vertex", "vertexProperty", "computed (linked to vertex)"],
                    "schema".to_string(),
                )));
            }
            TraverserValue::Graph(_) => {
                return Err(Box::new(TraverserError::cast_exception_str(
                    &["vertex", "vertexProperty", "computed (linked to vertex)"],
                    "graph".to_string(),
                )));
            }
        };

        match node_index {
            Some(node_index) => {
                let mut count = 0u64;
                for edge in context.storage.in_edges(node_index)? {
                    let mut found = false;
                    let edge_label = context.storage.edge_label(edge)?;

                    match &self.labels {
                        Some(labels) => {
                            if let Some(edge_label) = edge_label {
                                found = labels.contains(&edge_label.to_string());
                            }
                        }
                        None => found = true,
                    }
                    if found {
                        count += 1;
                    }
                }
                crate::traversal::bulk::mul_bulk(count, source.bulk, "in_e")
            }
            None => Err(Box::new(TraverserError::cast_exception(
                &["vertex", "vertexProperty", "computed (linked to vertex)"],
                &source.value,
            ))),
        }
    }

    fn labels(&self) -> Option<&[String]> {
        self.labels.as_deref()
    }

    fn set_labels(&mut self, labels: Vec<String>) {
        self.labels = Some(labels);
    }

    fn clone_box(&self) -> Box<dyn GraphStepFlatMap<'g, S> + 'g> {
        Box::new(self.clone())
    }
}
