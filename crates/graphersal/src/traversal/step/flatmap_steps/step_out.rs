use crate::graph::GraphStorage;
use crate::*;

#[derive(Default, Clone)]
struct StepOut {
    labels: Option<Vec<String>>,
}

impl fmt::Display for StepOut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "out({})", display_option_vec_string(&self.labels))
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Moves to the adjacent vertices over outgoing edges, optionally only edges with the given
    /// labels.
    pub fn out<T: Into<ArgStrings>>(self, labels: T) -> Self {
        let labels = match labels.into() {
            ArgStrings::None => None,
            ArgStrings::String(v) => Some(vec![v]),
            ArgStrings::List(v) => Some(v),
        };
        self.push_step(GraphStep::FlatMap("out", Box::new(StepOut { labels })))
    }
}

impl __ {
    /// Moves to the adjacent vertices over outgoing edges, optionally only edges with the given
    /// labels. Starts an anonymous traversal (`__.out()`).
    pub fn out<'g, S: GraphStorage + 'g, T: Into<ArgStrings>>(
        labels: T,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().out(labels)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Moves to the adjacent vertices over outgoing edges, optionally only edges with the given
    /// labels.
    pub fn out<T: Into<ArgStrings>>(self, labels: T) -> Self {
        let labels = match labels.into() {
            ArgStrings::None => None,
            ArgStrings::String(v) => Some(vec![v]),
            ArgStrings::List(v) => Some(v),
        };
        let step = GraphStep::FlatMap("out", Box::new(StepOut { labels }));
        self.push_step(step)
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepFlatMap<'g, S> for StepOut {
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
                for edge in context.storage.out_edges(node_index)? {
                    let target = context.storage.edge_endpoints(edge)?.map(|(_, to)| to);
                    if let Some(target) = target {
                        let edge_label = context.storage.edge_label(edge)?;
                        let mut found = false;
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
                                TraverserValue::Vertex(target),
                                source.path,
                                source.bulk,
                                source.sack,
                            ));
                        }
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
                for edge in context.storage.out_edges(node_index)? {
                    if context.storage.edge_endpoints(edge)?.is_some() {
                        let edge_label = context.storage.edge_label(edge)?;
                        let mut found = false;
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
                }
                crate::traversal::bulk::mul_bulk(count, source.bulk, "out")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").out(None).count().to_list(),
            rhai: [
                r#"g.V("1").out().count()"#,
                r#"g.v("1").out().count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(3)),
        );
    }

    #[test]
    fn label() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").out("created").count().to_list(),
            rhai: [
                r#"g.V("1").out("created").count()"#,
                r#"g.v("1").out("created").count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
    }

    #[test]
    fn labels() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").out(vec!["created", "knows"]).count().to_list(),
            rhai: [
                r#"g.V("1").out(["created", "knows"]).count()"#,
                r#"g.v("1").out(["created", "knows"]).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(3)),
        );
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g
            .v("1")
            .out(vec!["created", "knows"])
            .count()
            .profile()
            .unwrap();
        assert!(result.to_string().contains("out("));
    }
}
