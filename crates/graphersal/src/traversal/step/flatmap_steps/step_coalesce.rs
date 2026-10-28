use crate::graph::GraphStorage;
use crate::*;

struct StepCoalesce<'g, S: GraphStorage + 'g> {
    traversals: Vec<AnonymousTraversal<'g, S>>,
}

impl<'g, S: GraphStorage + 'g> Clone for StepCoalesce<'g, S> {
    fn clone(&self) -> Self {
        Self {
            traversals: self.traversals.clone(),
        }
    }
}

impl<'g, S: GraphStorage + 'g> fmt::Display for StepCoalesce<'g, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "coalesce({})",
            crate::traversal::render_traversals(&self.traversals)
        )
    }
}

impl __ {
    /// Runs the child traversals in order and emits the results of the first one that yields
    /// anything. Starts an anonymous traversal (`__.coalesce()`).
    pub fn coalesce<'g, S: GraphStorage + 'g>(
        traversals: Vec<AnonymousTraversal<'g, S>>,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().coalesce(traversals)
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Runs the child traversals in order and emits the results of the first one that yields
    /// anything.
    pub fn coalesce(self, traversals: Vec<AnonymousTraversal<'g, S>>) -> Self {
        self.push_step(GraphStep::FlatMap(
            "coalesce",
            Box::new(StepCoalesce { traversals }),
        ))
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Runs the child traversals in order and emits the results of the first one that yields
    /// anything.
    pub fn coalesce(self, traversals: Vec<AnonymousTraversal<'g, S>>) -> Self {
        let step = GraphStep::FlatMap("coalesce", Box::new(StepCoalesce { traversals }));
        self.push_step(step)
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepFlatMap<'g, S> for StepCoalesce<'g, S> {
    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
        output: &mut Vec<Traverser<S>>,
    ) -> TraverserResult<()> {
        for src in &self.traversals {
            let x = src.apply_to(&mut *context, &source)?;
            if !x.is_empty() {
                output.extend(x);
                return Ok(());
            }
        }
        Ok(())
    }

    fn child_traversals_mut(&mut self) -> Vec<&mut crate::traversal::AnonymousTraversal<'g, S>> {
        self.traversals.iter_mut().collect()
    }

    fn extends_path(&self) -> bool {
        false
    }

    fn analyze_nested_paths(
        &mut self,
        analysis: &crate::traversal::optimizer::path_analysis::PathAnalysis,
        downstream: &crate::traversal::optimizer::path_analysis::PathRequirement,
    ) -> crate::traversal::optimizer::path_analysis::PathRequirement {
        // The output of the chosen child continues the path, so each child keeps writing what
        // the steps after `coalesce()` read.
        self.traversals.iter_mut().fold(
            crate::traversal::optimizer::path_analysis::PathRequirement::None,
            |need, child| need.union(child.analyze_paths(analysis, downstream.clone())),
        )
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
    fn v_simple() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").coalesce(vec![__::out_e("knows"), __::out_e("created")]).in_v().values("name").to_list(),
            rhai: [
                r#"g.V("1").coalesce([__.outE("knows"), __.outE("created")]).inV().values("name")"#,
                r#"g.v("1").coalesce([__.out_e("knows"), __.out_e("created")]).in_v().values("name")"#,
            ],
            assert: |res| {
                assert_eq!(res.len(), 2);
                assert_eq!(res[0].as_string(), Some("josh"));
                assert_eq!(res[1].as_string(), Some("vadas"));
            },
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_simple_1() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").coalesce(vec![__::out_e("created"), __::out_e("knows")]).in_v().values("name").to_list(),
            rhai: [
                r#"g.V("1").coalesce([__.outE("created"), __.outE("knows")]).inV().values("name")"#,
                r#"g.v("1").coalesce([__.out_e("created"), __.out_e("knows")]).in_v().values("name")"#,
            ],
            assert: |res| {
                assert_eq!(res.len(), 1);
                assert_eq!(res[0].as_string(), Some("lop"));
            },
        );
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g
            .v("1")
            .coalesce(vec![__::out_e("knows"), __::out_e("created")])
            .in_v()
            .values("name")
            .profile()
            .unwrap();
        assert!(result.to_string().contains("coalesce("));
    }
}
