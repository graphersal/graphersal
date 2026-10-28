use crate::graph::GraphStorage;
use crate::*;

struct StepOptional<'g, S: GraphStorage + 'g> {
    traversal: AnonymousTraversal<'g, S>,
}

impl<'g, S: GraphStorage + 'g> Clone for StepOptional<'g, S> {
    fn clone(&self) -> Self {
        Self {
            traversal: self.traversal.clone(),
        }
    }
}

impl<'g, S: GraphStorage + 'g> fmt::Display for StepOptional<'g, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "optional({})", self.traversal)
    }
}

impl __ {
    /// Emits the results of the child traversal, or the traverser itself when it yields nothing.
    /// Starts an anonymous traversal (`__.optional()`).
    pub fn optional<'g, S: GraphStorage + 'g>(
        traversal: AnonymousTraversal<'g, S>,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().optional(traversal)
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Emits the results of the child traversal, or the traverser itself when it yields nothing.
    pub fn optional(self, traversal: AnonymousTraversal<'g, S>) -> Self {
        self.push_step(GraphStep::FlatMap(
            "optional",
            Box::new(StepOptional { traversal }),
        ))
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Emits the results of the child traversal, or the traverser itself when it yields nothing.
    pub fn optional(self, traversal: AnonymousTraversal<'g, S>) -> Self {
        let step = GraphStep::FlatMap("optional", Box::new(StepOptional { traversal }));
        self.push_step(step)
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepFlatMap<'g, S> for StepOptional<'g, S> {
    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
        output: &mut Vec<Traverser<S>>,
    ) -> TraverserResult<()> {
        let x = self.traversal.apply_to(&mut *context, &source)?;
        if x.is_empty() {
            output.push(source);
        } else {
            output.extend(x);
        }
        Ok(())
    }

    fn child_traversals_mut(&mut self) -> Vec<&mut crate::traversal::AnonymousTraversal<'g, S>> {
        vec![&mut self.traversal]
    }

    fn extends_path(&self) -> bool {
        false
    }

    fn analyze_nested_paths(
        &mut self,
        analysis: &crate::traversal::optimizer::path_analysis::PathAnalysis,
        downstream: &crate::traversal::optimizer::path_analysis::PathRequirement,
    ) -> crate::traversal::optimizer::path_analysis::PathRequirement {
        // The child's output continues the path; when the child yields nothing, the incoming
        // traverser passes through unchanged, so what the later steps read is needed on entry too.
        self.traversal
            .analyze_paths(analysis, downstream.clone())
            .union(downstream.clone())
    }

    fn clone_box(&self) -> Box<dyn GraphStepFlatMap<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "script")]
    #[test]
    fn is_false() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("5").optional(__::out(None)).id().to_list(),
            rhai: [
                r#"g.V("5").optional(__.out()).id()"#,
                r#"g.v("5").optional(__.out()).id()"#,
            ],
            assert: |res| assert_eq!(res[0].as_string(), Some("5")),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn is_true() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("6").optional(__::out(None)).id().to_list(),
            rhai: [
                r#"g.V("6").optional(__.out()).id()"#,
                r#"g.v("6").optional(__.out()).id()"#,
            ],
            assert: |res| assert_eq!(res[0].as_string(), Some("3")),
        );
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.v("6").optional(__::out(None)).id().profile().unwrap();
        assert!(result.to_string().contains("optional("));
    }
}
