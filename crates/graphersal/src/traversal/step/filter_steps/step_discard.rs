use crate::graph::GraphStorage;
use crate::*;

#[derive(Default, Clone)]
struct StepDiscard {}

impl fmt::Display for StepDiscard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "discard()")
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Drops every traverser: the traversal yields nothing (side effects still run).
    pub fn discard(self) -> Self {
        self.push_step(GraphStep::FlatBarrier("discard", Box::new(StepDiscard {})))
    }
}

impl __ {
    /// Drops every traverser: the traversal yields nothing (side effects still run). Starts an
    /// anonymous traversal (`__.discard()`).
    pub fn discard<'g, S: GraphStorage + 'g>() -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().discard()
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Drops every traverser: the traversal yields nothing (side effects still run).
    pub fn discard(self) -> Self {
        let step = GraphStep::FlatBarrier("discard", Box::new(StepDiscard {}));
        self.push_step(step)
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepFlatBarrier<'g, S> for StepDiscard {
    #[inline]
    fn process(
        &self,
        _context: &mut TraverserContext<'_, S>,
        _source: Vec<Traverser<S>>,
    ) -> TraverserResult<Option<Vec<Traverser<S>>>> {
        Ok(None)
    }

    fn clone_box(&self) -> Box<dyn GraphStepFlatBarrier<'g, S> + 'g> {
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
            rust: |g| g.v(None).discard().to_list(),
            rhai: [
                r#"g.V().discard()"#,
                r#"g.v().discard()"#,
            ],
            assert: |res| assert_eq!(res.len(), 0),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn where_() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).where_t(__::discard()).to_list(),
            rhai: [
                r#"g.V().where(__.discard())"#,
                r#"g.v().where(__.discard())"#,
            ],
            assert: |res| assert_eq!(res.len(), 0),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn where_inner() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).where_t(__::element().discard()).to_list(),
            rhai: [
                r#"g.V().where(__.element().discard())"#,
                r#"g.v().where(__.element().discard())"#,
            ],
            assert: |res| assert_eq!(res.len(), 0),
        );
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.v(None).discard().profile().unwrap();
        assert!(result.to_string().contains("discard("));
    }
}
