use super::where_scope::ScopeLabels;
use crate::graph::GraphStorage;
use crate::*;

/// `and(t1, t2, ...)`. Inside a `where(traversal)` whose first step this is, a leading or
/// trailing `as()` of a child is a start/end label (`configure_where_scope`, see
/// `filter_steps/where_scope.rs`); elsewhere it keeps its plain meaning.
struct StepAnd<'g, S: GraphStorage + 'g> {
    traversals: Vec<AnonymousTraversal<'g, S>>,
    /// One entry per child once configured by a `where()`; empty otherwise.
    scopes: Vec<ScopeLabels>,
    build_error: Option<&'static str>,
}

impl<'g, S: GraphStorage + 'g> Clone for StepAnd<'g, S> {
    fn clone(&self) -> Self {
        Self {
            traversals: self.traversals.clone(),
            scopes: self.scopes.clone(),
            build_error: self.build_error,
        }
    }
}

impl<'g, S: GraphStorage + 'g> fmt::Display for StepAnd<'g, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "and(")?;
        for (i, traversal) in self.traversals.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            match self.scopes.get(i) {
                Some(scope) => f.write_str(&scope.render_child(traversal))?,
                None => write!(f, "{traversal}")?,
            }
        }
        write!(f, ")")
    }
}

impl __ {
    /// Keeps the traverser when every child traversal yields a result. Starts an anonymous
    /// traversal (`__.and()`).
    pub fn and<'g, S: GraphStorage + 'g>(
        traversals: Vec<AnonymousTraversal<'g, S>>,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().and(traversals)
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Keeps the traverser when every child traversal yields a result.
    pub fn and(self, traversals: Vec<AnonymousTraversal<'g, S>>) -> Self {
        self.push_step(GraphStep::Filter(
            "and",
            Box::new(StepAnd {
                traversals,
                scopes: Vec::new(),
                build_error: None,
            }),
        ))
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Keeps the traverser when every child traversal yields a result.
    pub fn and(self, traversals: Vec<AnonymousTraversal<'g, S>>) -> Self {
        let step = GraphStep::Filter(
            "and",
            Box::new(StepAnd {
                traversals,
                scopes: Vec::new(),
                build_error: None,
            }),
        );
        self.push_step(step)
    }
}

impl<'g, S: GraphStorage + 'g> StepAnd<'g, S> {
    /// Whether child `i` passes for `source`, honouring its start/end label when configured.
    fn test(
        &self,
        i: usize,
        child: &AnonymousTraversal<'g, S>,
        context: &mut TraverserContext<'_, S>,
        source: &Traverser<S>,
    ) -> TraverserResult<bool> {
        match self.scopes.get(i) {
            Some(scope) => scope.test(child, context, source),
            None => child.exists(context, source),
        }
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepFilter<'g, S> for StepAnd<'g, S> {
    fn configure_where_scope(&mut self) {
        let mut scopes = Vec::with_capacity(self.traversals.len());
        for child in &mut self.traversals {
            match ScopeLabels::extract(child) {
                Ok(scope) => scopes.push(scope),
                Err(reason) => {
                    self.build_error = Some(reason);
                    scopes.push(ScopeLabels::default());
                }
            }
        }
        self.scopes = scopes;
    }

    fn plan_check(&self) -> TraverserResult<()> {
        match self.build_error {
            Some(reason) => Err(Box::new(TraverserError::QueryExecution {
                query: "and".to_string(),
                reason: reason.to_string(),
            })),
            None => Ok(()),
        }
    }

    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: &Traverser<S>,
    ) -> TraverserResult<bool> {
        for (i, src) in self.traversals.iter().enumerate() {
            if !self.test(i, src, context, source)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn child_traversals_mut(&mut self) -> Vec<&mut crate::traversal::AnonymousTraversal<'g, S>> {
        self.traversals.iter_mut().collect()
    }

    fn analyze_nested_paths(
        &mut self,
        analysis: &crate::traversal::optimizer::path_analysis::PathAnalysis,
        _downstream: &crate::traversal::optimizer::path_analysis::PathRequirement,
    ) -> crate::traversal::optimizer::path_analysis::PathRequirement {
        // The children are evaluated for their existence only; their output does not continue
        // the path.
        let labels = self.scopes.iter().fold(
            crate::traversal::optimizer::path_analysis::PathRequirement::None,
            |need, scope| need.union(scope.requirement()),
        );
        self.traversals.iter_mut().fold(labels, |need, child| {
            need.union(child.analyze_paths(
                analysis,
                crate::traversal::optimizer::path_analysis::PathRequirement::None,
            ))
        })
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
    fn simple() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).and(vec![
                __::values("age").is(P::Lt(30)),
                __::values("name").is("marko"),
            ]).count().to_list(),
            rhai: [
                r#"g.V().and([__.values("age").is(P::Lt(30)), __.values("name").is("marko")]).count()"#,
                r#"g.v().and([__.values("age").is(P::Lt(30)), __.values("name").is("marko")]).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn where_() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).where_t(__::and(vec![
                __::values("age").is(P::Lt(30)),
                __::values("name").is("marko"),
            ])).count().to_list(),
            rhai: [
                r#"g.V().where(__.and([__.values("age").is(P::Lt(30)), __.values("name").is("marko")])).count()"#,
                r#"g.v().where(__.and([__.values("age").is(P::Lt(30)), __.values("name").is("marko")])).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn where_inner() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).where_t(__::element().and(vec![
                __::values("age").is(P::Lt(30)),
                __::values("name").is("marko"),
            ])).count().to_list(),
            rhai: [
                r#"g.V().where(__.element().and([__.values("age").is(P::Lt(30)), __.values("name").is("marko")])).count()"#,
                r#"g.v().where(__.element().and([__.values("age").is(P::Lt(30)), __.values("name").is("marko")])).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g
            .v(None)
            .and(vec![
                __::values("age").is(P::Lt(30)),
                __::values("name").is("marko"),
            ])
            .profile()
            .unwrap();
        assert!(result.to_string().contains("and("));
    }
}
