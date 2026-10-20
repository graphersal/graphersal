use super::where_scope::ScopeLabels;
use crate::graph::GraphStorage;
use crate::*;

/// `where(traversal)`: keeps a traverser when the child traversal produces a result.
///
/// A leading `as("a")` of the child is a *start label* (the child starts at the object labelled
/// `a`) and a trailing `as("b")` an *end label* (some child result must equal the object
/// labelled `b`), as in TinkerPop's `WhereStartStep`/`WhereEndStep`; see
/// `filter_steps/where_scope.rs` for the lookup order and the deliberate deviations. A start
/// label that resolves to nothing filters the traverser out.
struct StepWhereT<'g, S: GraphStorage + 'g> {
    traversal: AnonymousTraversal<'g, S>,
    scope: ScopeLabels,
    /// A start or end step with several labels, reported by `plan_check`.
    build_error: Option<&'static str>,
}

impl<'g, S: GraphStorage + 'g> StepWhereT<'g, S> {
    fn new(mut traversal: AnonymousTraversal<'g, S>) -> Self {
        match ScopeLabels::extract(&mut traversal) {
            Ok(scope) => Self {
                traversal,
                scope,
                build_error: None,
            },
            Err(reason) => Self {
                traversal,
                scope: ScopeLabels::default(),
                build_error: Some(reason),
            },
        }
    }
}

impl<'g, S: GraphStorage + 'g> Clone for StepWhereT<'g, S> {
    fn clone(&self) -> Self {
        Self {
            traversal: self.traversal.clone(),
            scope: self.scope.clone(),
            build_error: self.build_error,
        }
    }
}

impl<'g, S: GraphStorage + 'g> fmt::Display for StepWhereT<'g, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "where({})", self.scope.render_child(&self.traversal))
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Graphersal form of where(traversal): keeps the traverser when the child traversal yields a
    /// result.
    pub fn where_t(self, traversal: AnonymousTraversal<'g, S>) -> Self {
        self.push_step(GraphStep::Filter(
            "where_t",
            Box::new(StepWhereT::new(traversal)),
        ))
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Graphersal form of where(traversal): keeps the traverser when the child traversal yields a
    /// result.
    pub fn where_t(self, traversal: AnonymousTraversal<'g, S>) -> Self {
        let step = GraphStep::Filter("where_t", Box::new(StepWhereT::new(traversal)));
        self.push_step(step)
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepFilter<'g, S> for StepWhereT<'g, S> {
    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: &Traverser<S>,
    ) -> TraverserResult<bool> {
        self.scope.test(&self.traversal, context, source)
    }

    fn has_scope_labels(&self) -> bool {
        !self.scope.is_empty()
    }

    fn plan_check(&self) -> TraverserResult<()> {
        match self.build_error {
            Some(reason) => Err(Box::new(TraverserError::QueryExecution {
                query: "where".to_string(),
                reason: reason.to_string(),
            })),
            None => Ok(()),
        }
    }

    fn path_requirement(&self) -> crate::traversal::optimizer::path_analysis::PathRequirement {
        self.scope.requirement()
    }

    fn as_where_traversal(&self) -> Option<&AnonymousTraversal<'g, S>> {
        Some(&self.traversal)
    }

    fn into_inner_steps(self: Box<Self>) -> Option<Vec<GraphStep<'g, S>>> {
        Some(self.traversal.steps)
    }

    fn child_traversals_mut(&mut self) -> Vec<&mut crate::traversal::AnonymousTraversal<'g, S>> {
        vec![&mut self.traversal]
    }

    fn analyze_nested_paths(
        &mut self,
        analysis: &crate::traversal::optimizer::path_analysis::PathAnalysis,
        _downstream: &crate::traversal::optimizer::path_analysis::PathRequirement,
    ) -> crate::traversal::optimizer::path_analysis::PathRequirement {
        // The start/end labels are read by `path_requirement`. The child is evaluated for its existence only; its output does not continue the path.
        self.traversal.analyze_paths(
            analysis,
            crate::traversal::optimizer::path_analysis::PathRequirement::None,
        )
    }

    fn clone_box(&self) -> Box<dyn GraphStepFilter<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn out() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).where_t(__::out(None)).count().to_list(),
            rhai: [
                r#"g.V().where(__.out()).count()"#,
                r#"g.v().where(__.out()).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(3)),
        );
    }

    #[test]
    fn out_by_label() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).where_t(__::out("knows")).count().to_list(),
            rhai: [
                r#"g.V().where(__.out("knows")).count()"#,
                r#"g.v().where(__.out("knows")).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
    }

    #[test]
    fn as_() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").as_("A").out("created").in_("created").where_t(__::is("A")).id().to_list(),
            rhai: [
                r#"g.V("1").as("A").out("created").in("created").where(__.is("A")).id()"#,
                r#"g.v("1").as("A").out("created").in("created").where(__.is("A")).id()"#,
            ],
            // `is()` compares with the literal "A" (TinkerPop), never the label `A`.
            assert: |res| assert!(res.is_empty()),
        );
    }

    #[test]
    fn where_inside_where() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).where_t(__::out(None).where_t(__::has_label("person"))).id().to_list(),
            rhai: [
                r#"g.V().where(__.out().where(__.hasLabel("person"))).id()"#,
                r#"g.v().where(__.out().where(__.has_label("person"))).id()"#,
            ],
            assert: |res| assert_eq!(res[0], ElementProperty::String("1".into())),
        );
    }

    #[test]
    fn static_anon_traversal_where_t() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).where_t(__::has_label("person")).to_list(),
            rhai: [
                r#"g.V().where(__.hasLabel("person"))"#,
                r#"g.v().where(__.has_label("person"))"#,
            ],
            assert: |res| assert_eq!(res.len(), 4),
        );
    }
}
