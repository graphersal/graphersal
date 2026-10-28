use super::where_scope::ScopeLabels;
use crate::graph::GraphStorage;
use crate::*;

/// `not(traversal)`. Inside a `where(traversal)` whose first step this is, a leading or trailing
/// `as()` of the child is a start/end label (`configure_where_scope`, see
/// `filter_steps/where_scope.rs`); elsewhere it keeps its plain meaning.
struct StepNot<'g, S: GraphStorage + 'g> {
    traversal: AnonymousTraversal<'g, S>,
    scope: ScopeLabels,
    build_error: Option<&'static str>,
}

impl<'g, S: GraphStorage + 'g> Clone for StepNot<'g, S> {
    fn clone(&self) -> Self {
        Self {
            traversal: self.traversal.clone(),
            scope: self.scope.clone(),
            build_error: self.build_error,
        }
    }
}

impl<'g, S: GraphStorage + 'g> fmt::Display for StepNot<'g, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "not({})", self.scope.render_child(&self.traversal))
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Keeps the traverser when the child traversal yields nothing.
    pub fn not(self, traversal: AnonymousTraversal<'g, S>) -> Self {
        self.push_step(GraphStep::Filter(
            "not",
            Box::new(StepNot {
                traversal,
                scope: ScopeLabels::default(),
                build_error: None,
            }),
        ))
    }
}

impl __ {
    /// Keeps the traverser when the child traversal yields nothing. Starts an anonymous traversal
    /// (`__.not()`).
    pub fn not<'g, S: GraphStorage + 'g>(
        traversal: AnonymousTraversal<'g, S>,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().not(traversal)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Keeps the traverser when the child traversal yields nothing.
    pub fn not(self, traversal: AnonymousTraversal<'g, S>) -> Self {
        let step = GraphStep::Filter(
            "not",
            Box::new(StepNot {
                traversal,
                scope: ScopeLabels::default(),
                build_error: None,
            }),
        );
        self.push_step(step)
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepFilter<'g, S> for StepNot<'g, S> {
    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: &Traverser<S>,
    ) -> TraverserResult<bool> {
        let exists = self.scope.test(&self.traversal, context, source)?;
        Ok(!exists)
    }

    fn configure_where_scope(&mut self) {
        match ScopeLabels::extract(&mut self.traversal) {
            Ok(scope) => self.scope = scope,
            Err(reason) => self.build_error = Some(reason),
        }
    }

    fn plan_check(&self) -> TraverserResult<()> {
        match self.build_error {
            Some(reason) => Err(Box::new(TraverserError::QueryExecution {
                query: "not".to_string(),
                reason: reason.to_string(),
            })),
            None => Ok(()),
        }
    }

    fn child_traversals_mut(&mut self) -> Vec<&mut crate::traversal::AnonymousTraversal<'g, S>> {
        vec![&mut self.traversal]
    }

    fn analyze_nested_paths(
        &mut self,
        analysis: &crate::traversal::optimizer::path_analysis::PathAnalysis,
        _downstream: &crate::traversal::optimizer::path_analysis::PathRequirement,
    ) -> crate::traversal::optimizer::path_analysis::PathRequirement {
        // The child is evaluated for its existence only; its output does not continue the path.
        self.scope.requirement().union(self.traversal.analyze_paths(
            analysis,
            crate::traversal::optimizer::path_analysis::PathRequirement::None,
        ))
    }

    fn clone_box(&self) -> Box<dyn GraphStepFilter<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "script")]
    #[test]
    fn label() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).not(__::has_label("person")).values("lang").to_list(),
            rhai: [
                r#"g.V().not(__.hasLabel("person")).values("lang")"#,
                r#"g.v().not(__.has_label("person")).values("lang")"#,
            ],
            assert: |res| {
                assert_eq!(res[0].as_string(), Some("java"));
                assert_eq!(res[1].as_string(), Some("java"));
            },
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn where_() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).where_t(__::not(__::has_label("person"))).values("lang").to_list(),
            rhai: [
                r#"g.V().where(__.not(__.hasLabel("person"))).values("lang")"#,
                r#"g.v().where(__.not(__.has_label("person"))).values("lang")"#,
            ],
            assert: |res| {
                assert_eq!(res[0].as_string(), Some("java"));
                assert_eq!(res[1].as_string(), Some("java"));
            },
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn where_inner() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).where_t(__::element().not(__::has_label("person"))).values("lang").to_list(),
            rhai: [
                r#"g.V().where(__.element().not(__.hasLabel("person"))).values("lang")"#,
                r#"g.v().where(__.element().not(__.has_label("person"))).values("lang")"#,
            ],
            assert: |res| {
                assert_eq!(res[0].as_string(), Some("java"));
                assert_eq!(res[1].as_string(), Some("java"));
            },
        );
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g
            .v(None)
            .not(__::has_label("person"))
            .values("lang")
            .profile()
            .unwrap();
        assert!(result.to_string().contains("not("));
    }
}
