use crate::graph::GraphStorage;
use crate::*;

/// The `reason` of the `QueryExecution` error of `id()` over a property element; its `help()` keys
/// on it.
pub(crate) const PROPERTY_ELEMENT_ID_REASON: &str = "property elements have no id in Graphersal: a vertex property is identified by its vertex id and key";

/// `id()`: an element's id as a lazy value handle.
///
/// **Deviation from TinkerPop:** a property element (`properties()`) has no id here (Graphersal has
/// no property ids; TinkerPop returns an opaque one), so `properties().id()` is a
/// `QueryExecution` error whose help names `key()` and `element().id()`.
#[derive(Default, Clone)]
struct StepId {}

impl fmt::Display for StepId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "id()")
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Yields the id of each vertex or edge (a string).
    pub fn id(self) -> Self {
        self.push_step(GraphStep::Map("id", Box::new(StepId::default())))
    }
}

impl __ {
    /// Yields the id of each vertex or edge (a string). Starts an anonymous traversal (`__.id()`).
    pub fn id<'g, S: GraphStorage + 'g>() -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().id()
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Yields the id of each vertex or edge (a string).
    pub fn id(self) -> Self {
        let step = GraphStep::Map("id", Box::new(StepId::default()));
        self.push_step(step)
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepMap<'g, S> for StepId {
    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<Traverser<S>> {
        match &source.value {
            // A removed element has no id to return (`GraphError::ElementRemoved`; a deviation
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
                    CompactString::new("id"),
                    *node_index,
                    PropertyHandleKind::Id,
                ),
                source.path,
                source.bulk,
                source.sack,
            )),

            TraverserValue::Edge(edge_index) => Ok(Traverser::with_parts(
                TraverserValue::EdgeProperty(
                    CompactString::new("id"),
                    *edge_index,
                    PropertyHandleKind::Id,
                ),
                source.path,
                source.bulk,
                source.sack,
            )),

            // **Deviation from TinkerPop:** Graphersal has no property ids, so a property element
            // (`properties()`) has no id to return (TinkerPop returns an opaque id). This is a
            // decision, not a gap: no synthetic id is invented, because it could never equal
            // TinkerPop's and could not be fed back into `V()`/`hasId()`. See the user's guide
            // page "Property elements".
            TraverserValue::VertexProperty(_, _, PropertyHandleKind::Element)
            | TraverserValue::PathProperty(_) => Err(Box::new(TraverserError::QueryExecution {
                query: "id".to_string(),
                reason: PROPERTY_ELEMENT_ID_REASON.to_string(),
            })),

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

    #[test]
    fn v_simple() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").id().to_list(),
            rhai: [
                r#"g.V("1").id()"#,
                r#"g.v("1").id()"#,
            ],
            assert: |res| assert_eq!(res[0], ElementProperty::String("1".into())),
        );
    }

    #[test]
    fn e_simple() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e("1").id().to_list(),
            rhai: [
                r#"g.E("1").id()"#,
                r#"g.e("1").id()"#,
            ],
            assert: |res| assert_eq!(res[0], ElementProperty::String("1".into())),
        );
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.e("1").id().profile().unwrap();
        assert!(result.to_string().contains("id("));
    }
}
