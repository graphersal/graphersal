use crate::graph::GraphStorage;
use crate::*;

/// `label()`: a vertex/edge label, or the key of a vertex property element from `properties()`
/// (TinkerPop: a `VertexProperty` is an element labeled by its key). Edge properties and value
/// handles are not elements and fail with a cast error.
#[derive(Default, Clone)]
struct StepLabel {}

impl fmt::Display for StepLabel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "label()")
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Yields the label of each vertex or edge.
    pub fn label(self) -> Self {
        self.push_step(GraphStep::Map("label", Box::new(StepLabel::default())))
    }
}

impl __ {
    /// Yields the label of each vertex or edge. Starts an anonymous traversal (`__.label()`).
    pub fn label<'g, S: GraphStorage + 'g>() -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().label()
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Yields the label of each vertex or edge.
    pub fn label(self) -> Self {
        let step = GraphStep::Map("label", Box::new(StepLabel::default()));
        self.push_step(step)
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepMap<'g, S> for StepLabel {
    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<Traverser<S>> {
        match &source.value {
            // A removed element has no label to return (`GraphError::ElementRemoved`; a deviation
            // from TinkerGraph, which still knows it).
            TraverserValue::Vertex(node_index)
                if !context.storage.contains_vertex(*node_index)? =>
            {
                Err(Box::new(GraphError::vertex_removed().into()))
            }
            TraverserValue::Edge(edge_index) if !context.storage.contains_edge(*edge_index)? => {
                Err(Box::new(GraphError::edge_removed().into()))
            }
            TraverserValue::Vertex(node_index) => Ok(Traverser::with_parts(
                TraverserValue::VertexProperty(
                    CompactString::new("label"),
                    *node_index,
                    PropertyHandleKind::Label,
                ),
                source.path,
                source.bulk,
                source.sack,
            )),

            TraverserValue::Edge(edge_index) => Ok(Traverser::with_parts(
                TraverserValue::EdgeProperty(
                    CompactString::new("label"),
                    *edge_index,
                    PropertyHandleKind::Label,
                ),
                source.path,
                source.bulk,
                source.sack,
            )),

            // A vertex property element (`properties()`): its label is its key, as in TinkerPop,
            // where a `VertexProperty` is an `Element`. A value handle (`values()`, `id()`, ...)
            // and an edge property are not elements and keep the cast error (see the user's guide
            // page "Property elements").
            TraverserValue::VertexProperty(key, _, PropertyHandleKind::Element) => {
                Ok(Traverser::with_parts(
                    TraverserValue::Computed(ElementProperty::String(key.clone()), None),
                    source.path,
                    source.bulk,
                    source.sack,
                ))
            }

            // A path property element: its label is its canonical path text, like a key.
            TraverserValue::PathProperty(prop) => Ok(Traverser::with_parts(
                TraverserValue::Computed(ElementProperty::String(prop.0.clone()), None),
                source.path,
                source.bulk,
                source.sack,
            )),

            v => Err(Box::new(TraverserError::cast_exception(
                &["vertex", "edge", "vertex property"],
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
    fn v_simple() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").label().to_list(),
            rhai: [
                r#"g.V("1").label()"#,
                r#"g.v("1").label()"#,
            ],
            assert: |res| assert_eq!(res[0], ElementProperty::String("person".into())),
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("3").label().to_list(),
            rhai: [
                r#"g.V("3").label()"#,
                r#"g.v("3").label()"#,
            ],
            assert: |res| assert_eq!(res[0], ElementProperty::String("software".into())),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn e_simple() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e("0").label().to_list(),
            rhai: [
                r#"g.E("0").label()"#,
                r#"g.e("0").label()"#,
            ],
            assert: |res| assert_eq!(res[0], ElementProperty::String("knows".into())),
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e("1").label().to_list(),
            rhai: [
                r#"g.E("1").label()"#,
                r#"g.e("1").label()"#,
            ],
            assert: |res| assert_eq!(res[0], ElementProperty::String("created".into())),
        );
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.v("1").label().profile().unwrap();
        assert!(result.to_string().contains("label("));
    }
}
