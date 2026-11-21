use crate::graph::{GraphStorage, LayerOrigin};
use crate::*;

/// `layer()`: the origin of each vertex or edge in a layered graph, as its name (`"base"`,
/// `"added"`, `"modified"`; [`LayerOrigin::as_str`]). A storage without layers answers `"base"`
/// for every element. Like `label()`, a removed element fails with `GraphError::ElementRemoved`
/// and every other value with a cast error.
#[derive(Default, Clone)]
struct StepLayer {}

impl fmt::Display for StepLayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "layer()")
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Yields the layer origin of each vertex or edge: `"base"` (the base graph's element,
    /// unchanged), `"added"` (created in the layer) or `"modified"` (a base element whose labels
    /// or properties the layer changed). On a graph without layers every element is `"base"`,
    /// so a query using it runs on any graph. A Graphersal extension.
    pub fn layer(self) -> Self {
        self.push_step(GraphStep::Map("layer", Box::new(StepLayer::default())))
    }
}

impl __ {
    /// Yields the layer origin of each vertex or edge. Starts an anonymous traversal
    /// (`__.layer()`); see [`GraphTraversalSource::layer`].
    pub fn layer<'g, S: GraphStorage + 'g>() -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().layer()
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Yields the layer origin of each vertex or edge; see [`GraphTraversalSource::layer`].
    pub fn layer(self) -> Self {
        self.push_step(GraphStep::Map("layer", Box::new(StepLayer::default())))
    }
}

/// The origin's name as a string value. The names are static and short, so the
/// `CompactString` borrows them (`const_new`): no allocation per traverser.
#[inline]
fn origin_value<S: GraphStorage>(origin: LayerOrigin) -> TraverserValue<S> {
    TraverserValue::Computed(
        ElementProperty::String(CompactString::const_new(origin.as_str())),
        None,
    )
}

impl<'g, S: GraphStorage + 'g> GraphStepMap<'g, S> for StepLayer {
    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<Traverser<S>> {
        let origin = match &source.value {
            // A removed element has no origin to return, like `label()`
            // (`GraphError::ElementRemoved`).
            TraverserValue::Vertex(node_index) => context
                .storage
                .vertex_layer(*node_index)?
                .ok_or_else(|| Box::new(GraphError::vertex_removed().into()))?,
            TraverserValue::Edge(edge_index) => context
                .storage
                .edge_layer(*edge_index)?
                .ok_or_else(|| Box::new(GraphError::edge_removed().into()))?,
            // Only vertices and edges belong to a layer: a property element takes the origin of
            // its element (`select` it first), a value has none.
            v => {
                return Err(Box::new(TraverserError::cast_exception(
                    &["vertex", "edge"],
                    v,
                )));
            }
        };
        Ok(Traverser::with_parts(
            origin_value(origin),
            source.path,
            source.bulk,
            source.sack,
        ))
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
    fn a_plain_graph_is_all_base() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).layer().dedup().to_list(),
            rhai: [r#"g.v().layer().dedup()"#],
            assert: |res| assert_eq!(res, vec![ElementProperty::String("base".into())]),
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e(None).layer().count().to_list(),
            rhai: [r#"g.e().layer().count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(6)),
        );
    }

    #[test]
    fn a_value_is_a_cast_error() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let err = g_lock
            .traversal()
            .v("1")
            .values("name")
            .layer()
            .to_list()
            .unwrap_err();
        let cause = err.root_cause().to_string();
        assert!(
            cause.contains("vertex") && cause.contains("edge"),
            "{cause}"
        );
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.v("1").layer().profile().unwrap();
        assert!(result.to_string().contains("layer()"));
    }
}
