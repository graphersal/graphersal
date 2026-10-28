use crate::graph::GraphStorage;
use crate::*;

#[derive(Default, Clone)]
struct StepInV {}

impl fmt::Display for StepInV {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "in_v()")
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Moves from an edge to its in (head) vertex.
    pub fn in_v(self) -> Self {
        self.push_step(GraphStep::FlatMap("in_v", Box::new(StepInV {})))
    }
}

impl __ {
    /// Moves from an edge to its in (head) vertex. Starts an anonymous traversal (`__.in_v()`).
    pub fn in_v<'g, S: GraphStorage + 'g>() -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().in_v()
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Moves from an edge to its in (head) vertex.
    pub fn in_v(self) -> Self {
        let step = GraphStep::FlatMap("in_v", Box::new(StepInV {}));
        self.push_step(step)
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepFlatMap<'g, S> for StepInV {
    fn is_adjacency(&self) -> bool {
        true
    }

    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
        output: &mut Vec<Traverser<S>>,
    ) -> TraverserResult<()> {
        let edge_index: Option<S::EdgeId> = match source.value {
            TraverserValue::Edge(edge_index) | TraverserValue::EdgeProperty(_, edge_index, _) => {
                Some(edge_index)
            }

            TraverserValue::Vertex(_) => {
                return Err(Box::new(TraverserError::cast_exception_str(
                    &["edge", "edgeProperty"],
                    "vertex".to_string(),
                )));
            }
            TraverserValue::VertexProperty(_, _, _) => {
                return Err(Box::new(TraverserError::cast_exception_str(
                    &["edge", "edgeProperty"],
                    "vertexProperty".to_string(),
                )));
            }
            TraverserValue::Computed(_, _) => {
                return Err(Box::new(TraverserError::cast_exception_str(
                    &["edge", "edgeProperty"],
                    "computed".to_string(),
                )));
            }
            TraverserValue::Path(_) => {
                return Err(Box::new(TraverserError::cast_exception_str(
                    &["edge", "edgeProperty"],
                    "path".to_string(),
                )));
            }
            TraverserValue::PathProperty(_) => {
                return Err(Box::new(TraverserError::cast_exception_str(
                    &["edge", "edgeProperty"],
                    "pathProperty".to_string(),
                )));
            }
            TraverserValue::Entry(_) => {
                return Err(Box::new(TraverserError::cast_exception_str(
                    &["edge", "edgeProperty"],
                    "entry".to_string(),
                )));
            }
            TraverserValue::Object(_) | TraverserValue::Record { .. } => {
                return Err(Box::new(TraverserError::cast_exception_str(
                    &["edge", "edgeProperty"],
                    "object".to_string(),
                )));
            }
            TraverserValue::Array(_) => {
                return Err(Box::new(TraverserError::cast_exception_str(
                    &["edge", "edgeProperty"],
                    "array".to_string(),
                )));
            }
            TraverserValue::Schema(_) => {
                return Err(Box::new(TraverserError::cast_exception_str(
                    &["edge", "edgeProperty"],
                    "schema".to_string(),
                )));
            }
            TraverserValue::Graph(_) => {
                return Err(Box::new(TraverserError::cast_exception_str(
                    &["edge", "edgeProperty"],
                    "graph".to_string(),
                )));
            }
        };

        match edge_index {
            Some(edge_index) => match context.storage.edge_endpoints(edge_index)? {
                Some(edge_endpoints) => {
                    output.push(Traverser::with_parts(
                        TraverserValue::Vertex(edge_endpoints.1),
                        source.path,
                        source.bulk,
                        source.sack,
                    ));
                    Ok(())
                }
                // A removed edge (stale handle) has no endpoints: nothing to emit.
                None if !context.storage.contains_edge(edge_index)? => Ok(()),
                None => Err(Box::new(TraverserError::Graph(GraphError::EdgeNotFound {
                    id: format!("{:?}", edge_index),
                }))),
            },
            None => Err(Box::new(TraverserError::cast_exception(
                &["edge", "edgeProperty"],
                &source.value,
            ))),
        }
    }

    fn clone_box(&self) -> Box<dyn GraphStepFlatMap<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "script")]
    #[test]
    fn simple() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("6").out_e(None).in_v().values("name").to_list(),
            rhai: [
                r#"g.V("6").outE().inV().values("name")"#,
                r#"g.v("6").out_e().in_v().values("name")"#,
            ],
            assert: |res| assert_eq!(res[0].as_string(), Some("lop")),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn simple_edge_property() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("6").out_e(None).values("weight").in_v().values("name").to_list(),
            rhai: [
                r#"g.V("6").outE().values("weight").inV().values("name")"#,
                r#"g.v("6").out_e().values("weight").in_v().values("name")"#,
            ],
            assert: |res| assert_eq!(res[0].as_string(), Some("lop")),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn simple__() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("6").out_e(None).where_t(__::in_v()).values("weight").to_list(),
            rhai: [
                r#"g.V("6").outE().where(__.inV()).values("weight")"#,
                r#"g.v("6").out_e().where(__.in_v()).values("weight")"#,
            ],
            assert: |res| assert_eq!(res[0].as_f64(), Some(0.2)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn simple_where() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("6").where_t(__::out_e(None).in_v()).values("name").to_list(),
            rhai: [
                r#"g.V("6").where(__.outE().inV()).values("name")"#,
                r#"g.v("6").where(__.out_e().in_v()).values("name")"#,
            ],
            assert: |res| assert_eq!(res[0].as_string(), Some("peter")),
        );
    }

    #[test]
    fn err_vertex() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.v("6").in_v().next();
        assert!(result.is_err());
    }

    #[test]
    fn err_vertex_property() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.v("6").id().in_v().next();
        assert!(result.is_err());
    }

    #[test]
    fn err_inject() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.inject(vec!["hello"]).in_v().next();
        assert!(result.is_err());
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g
            .v("6")
            .out_e(None)
            .in_v()
            .values("name")
            .profile()
            .unwrap();
        assert!(result.to_string().contains("in_v("));
    }
}
