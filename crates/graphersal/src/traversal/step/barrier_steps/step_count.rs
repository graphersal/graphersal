use crate::graph::GraphStorage;
use crate::*;

#[derive(Default, Clone)]
struct StepCount {}

impl fmt::Display for StepCount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "count()")
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Counts the traversers (bulk included).
    pub fn count(self) -> Self {
        self.push_step(GraphStep::Count("count", Box::new(StepCount::default())))
    }
}

impl __ {
    /// Counts the traversers (bulk included). Starts an anonymous traversal (`__.count()`).
    pub fn count<'g, S: GraphStorage + 'g>() -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().count()
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Counts the traversers (bulk included).
    pub fn count(self) -> Self {
        let step = GraphStep::Count("count", Box::new(StepCount::default()));
        self.push_step(step)
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepCount<'g, S> for StepCount {
    fn clone_box(&self) -> Box<dyn GraphStepCount<'g, S> + 'g> {
        Box::new(self.clone())
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "script")]
    #[test]
    fn v_count() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).count().to_list(),
            rhai: [
                r#"g.V().count()"#,
                r#"g.v().count()"#,
            ],
            assert: |res| assert_eq!(res[0], ElementProperty::Number(6.into())),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_where_count() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).id().where_t(__::out(None).count().is(P::Eq(2))).to_list(),
            rhai: [
                r#"g.V().id().where(__.out().count().is(P::Eq(2)))"#,
                r#"g.v().id().where(__.out().count().is(P::Eq(2)))"#,
            ],
            assert: |res| assert_eq!(res[0], ElementProperty::String("4".into())),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn e_count() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e(None).count().to_list(),
            rhai: [
                r#"g.E().count()"#,
                r#"g.e().count()"#,
            ],
            assert: |res| assert_eq!(res[0], ElementProperty::Number(6.into())),
        );
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.e(None).count().profile().unwrap();
        assert!(result.to_string().contains("count("));
    }
}
