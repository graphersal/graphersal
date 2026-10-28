use crate::graph::GraphStorage;
use crate::*;

/// `bothV()`: from an edge, both of its incident vertices, unconditionally.
///
/// Unlike [`otherV()`](GraphTraversalSource::other_v), `bothV()` never needs to know which
/// vertex the traversal arrived from — it always wants both ends of the edge — so it resolves
/// them directly with [`GraphStorage::edge_endpoints`] and needs no path lookback.
#[derive(Default, Clone)]
struct StepBothV {}

impl fmt::Display for StepBothV {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "both_v()")
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// From an edge, both of its incident vertices (the tail then the head), as two traversers.
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let graph = GraphSource::tinkerpop_modern();
    /// let lock = graph.read();
    /// let count = lock.traversal().e(None).both_v().count().to_list().unwrap();
    /// // Twice the edge count: every edge contributes its two endpoints.
    /// assert_eq!(count[0].as_i64(), Some(12));
    /// ```
    pub fn both_v(self) -> Self {
        self.push_step(GraphStep::FlatMap("both_v", Box::new(StepBothV {})))
    }
}

impl __ {
    /// Anonymous `both_v()`; see [`GraphTraversalSource::both_v`].
    pub fn both_v<'g, S: GraphStorage + 'g>() -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().both_v()
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Anonymous `both_v()`; see [`GraphTraversalSource::both_v`].
    pub fn both_v(self) -> Self {
        let step = GraphStep::FlatMap("both_v", Box::new(StepBothV {}));
        self.push_step(step)
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepFlatMap<'g, S> for StepBothV {
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
                Some((from, to)) => {
                    output.push(Traverser::with_parts(
                        TraverserValue::Vertex(from),
                        source.path,
                        source.bulk,
                        source.sack,
                    ));
                    output.push(Traverser::with_parts(
                        TraverserValue::Vertex(to),
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

    #[test]
    fn simple() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("6").out_e(None).both_v().id().to_list(),
            rhai: [
                r#"g.V("6").outE().bothV().id()"#,
                r#"g.v("6").out_e().both_v().id()"#,
            ],
            assert: |res| assert_eq!(res.len(), 2),
        );
    }

    #[test]
    fn every_edge_contributes_two_endpoints() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e(None).both_v().count().to_list(),
            rhai: [r#"g.E().bothV().count()"#, r#"g.e().both_v().count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(12)),
        );
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.e(None).both_v().count().profile().unwrap();
        assert!(result.to_string().contains("both_v("));
    }
}
