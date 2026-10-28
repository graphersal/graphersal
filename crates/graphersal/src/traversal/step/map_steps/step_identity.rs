use crate::graph::GraphStorage;
use crate::*;

#[derive(Default, Clone)]
struct StepIdentity {}

impl fmt::Display for StepIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "identity()")
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Passes every traverser through unchanged.
    pub fn identity(self) -> Self {
        self.push_step(GraphStep::Map(
            "identity",
            Box::new(StepIdentity::default()),
        ))
    }
}

impl __ {
    /// Passes every traverser through unchanged. Starts an anonymous traversal (`__.identity()`).
    pub fn identity<'g, S: GraphStorage + 'g>() -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().identity()
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Passes every traverser through unchanged.
    pub fn identity(self) -> Self {
        let step = GraphStep::Map("identity", Box::new(StepIdentity::default()));
        self.push_step(step)
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepMap<'g, S> for StepIdentity {
    #[inline]
    fn process(
        &self,
        _context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<Traverser<S>> {
        Ok(source)
    }

    fn extends_path(&self) -> bool {
        false
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
    fn v_identity() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).identity().count().to_list(),
            rhai: [
                r#"g.V().identity().count()"#,
                r#"g.v().identity().count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(6)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_identity_values() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").identity().values("name").to_list(),
            rhai: [
                r#"g.V("1").identity().values("name")"#,
                r#"g.v("1").identity().values("name")"#,
            ],
            assert: |res| assert_eq!(res[0], ElementProperty::String("marko".into())),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn e_identity() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e("1").identity().id().to_list(),
            rhai: [
                r#"g.E("1").identity().id()"#,
                r#"g.e("1").identity().id()"#,
            ],
            assert: |res| assert_eq!(res[0], ElementProperty::String("1".into())),
        );
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.v("1").identity().profile().unwrap();
        assert!(result.to_string().contains("identity("));
    }
}
