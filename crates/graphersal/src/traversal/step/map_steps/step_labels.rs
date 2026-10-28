use crate::graph::GraphStorage;
use crate::*;

/// `labels()` — the plural counterpart to [`label()`](super::step_label): exposes a vertex's full
/// ordered label set instead of only the primary one. This is the foundation-level step a future
/// Cypher/GQL front end's `labels(n)` maps onto directly; `label()` keeps returning the single
/// primary label for TinkerPop-facing code, unchanged.
#[derive(Default, Clone)]
struct StepLabels {}

impl fmt::Display for StepLabels {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "labels()")
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Returns the full ordered label set of each vertex in the stream (a list value), or, for an
    /// edge, a single-element list containing its one label (empty if the edge is unlabeled). Use
    /// [`Self::label`] when only the single primary/edge label is needed.
    pub fn labels(self) -> Self {
        self.push_step(GraphStep::Map("labels", Box::new(StepLabels::default())))
    }
}

impl __ {
    /// Yields the full label set of each vertex as a list (Graphersal multi-label vertices). Starts
    /// an anonymous traversal (`__.labels()`).
    pub fn labels<'g, S: GraphStorage + 'g>() -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().labels()
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// See [`GraphTraversalSource::labels`].
    pub fn labels(self) -> Self {
        let step = GraphStep::Map("labels", Box::new(StepLabels::default()));
        self.push_step(step)
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepMap<'g, S> for StepLabels {
    #[inline]
    fn process(
        &self,
        _context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<Traverser<S>> {
        match &source.value {
            TraverserValue::Vertex(node_index) => Ok(Traverser::with_parts(
                TraverserValue::VertexProperty(
                    CompactString::new("labels"),
                    *node_index,
                    PropertyHandleKind::Labels,
                ),
                source.path,
                source.bulk,
                source.sack,
            )),

            TraverserValue::Edge(edge_index) => Ok(Traverser::with_parts(
                TraverserValue::EdgeProperty(
                    CompactString::new("labels"),
                    *edge_index,
                    PropertyHandleKind::Labels,
                ),
                source.path,
                source.bulk,
                source.sack,
            )),

            v => Err(Box::new(TraverserError::cast_exception(
                &["vertex", "edge"],
                v,
            ))),
        }
    }

    fn clone_box(&self) -> Box<dyn GraphStepMap<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "script")]
    #[test]
    fn v_single_label() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").labels().to_list(),
            rhai: [
                r#"g.V("1").labels()"#,
                r#"g.v("1").labels()"#,
            ],
            assert: |res| assert_eq!(
                res[0],
                ElementProperty::Array(vec![ElementProperty::String("person".into())])
            ),
        );
    }

    #[test]
    fn v_multi_label() {
        let mut graph = TraversalGraph::new();
        graph
            .traversal_mut()
            .add_v(vec!["Person", "Admin"])
            .next()
            .unwrap()
            .unwrap();
        let g = graph.traversal();
        let result = g.v("1").labels().to_list().unwrap();
        assert_eq!(
            result[0],
            ElementProperty::Array(vec![
                ElementProperty::String("Person".into()),
                ElementProperty::String("Admin".into()),
            ])
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn e_simple() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e("0").labels().to_list(),
            rhai: [
                r#"g.E("0").labels()"#,
                r#"g.e("0").labels()"#,
            ],
            assert: |res| assert_eq!(
                res[0],
                ElementProperty::Array(vec![ElementProperty::String("knows".into())])
            ),
        );
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.v("1").labels().profile().unwrap();
        assert!(result.to_string().contains("labels("));
    }
}
