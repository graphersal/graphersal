use crate::graph::GraphStorage;
use crate::*;

/// `local(traversal)`: runs `child` once per incoming traverser, in isolation from every other
/// traverser in the batch, and pushes every result the child produces for that one input into
/// `output`. This is what makes `g.V().local(__.outE().limit(2))` limit edges *per vertex*,
/// unlike a bare `g.V().outE().limit(2)`, which limits the whole flattened stream to 2 edges
/// total: here each vertex gets its own, independent run of `child`, so each contributes up to 2
/// of its own out-edges.
///
/// TinkerPop's own clarification applies as-is: `local()` isolates the *traversal stream*, not
/// side-effect state — a side-effect write inside `child` (e.g. `aggregate()`) is still shared/
/// global across every per-traverser run, exactly like anywhere else in this engine. `local()`
/// does not create a new transaction or side-effect scope.
struct StepLocal<'g, S: GraphStorage + 'g> {
    child: AnonymousTraversal<'g, S>,
}

impl<'g, S: GraphStorage + 'g> Clone for StepLocal<'g, S> {
    fn clone(&self) -> Self {
        Self {
            child: self.child.clone(),
        }
    }
}

impl<'g, S: GraphStorage + 'g> fmt::Display for StepLocal<'g, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "local({})", self.child)
    }
}

impl __ {
    /// `__.local(traversal)`: the anonymous-traversal form, for use as a nested child of another
    /// step (e.g. `g.V().local(__.outE().limit(2))`'s own nested `__.outE().limit(2)`, or inside
    /// `repeat()`/`union()`/etc.).
    pub fn local<'g, S: GraphStorage + 'g>(
        traversal: AnonymousTraversal<'g, S>,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().local(traversal)
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// `g.V().local(traversal)`: run `traversal` once per incoming traverser, in isolation, and
    /// continue with whatever that single run produced. See [`__::local`] for the nested form.
    pub fn local(self, traversal: AnonymousTraversal<'g, S>) -> Self {
        self.push_step(GraphStep::FlatMap(
            "local",
            Box::new(StepLocal { child: traversal }),
        ))
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// `__.local(traversal)`'s builder half — see [`__::local`].
    pub fn local(self, traversal: AnonymousTraversal<'g, S>) -> Self {
        let step = GraphStep::FlatMap("local", Box::new(StepLocal { child: traversal }));
        self.push_step(step)
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepFlatMap<'g, S> for StepLocal<'g, S> {
    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
        output: &mut Vec<Traverser<S>>,
    ) -> TraverserResult<()> {
        let x = self.child.apply_to(&mut *context, &source)?;
        output.extend(x);
        Ok(())
    }

    fn child_traversals_mut(&mut self) -> Vec<&mut crate::traversal::AnonymousTraversal<'g, S>> {
        vec![&mut self.child]
    }

    fn extends_path(&self) -> bool {
        false
    }

    fn analyze_nested_paths(
        &mut self,
        analysis: &crate::traversal::optimizer::path_analysis::PathAnalysis,
        downstream: &crate::traversal::optimizer::path_analysis::PathRequirement,
    ) -> crate::traversal::optimizer::path_analysis::PathRequirement {
        // The child's output continues the path, so it keeps writing what the steps after
        // `local()` read. Unlike `optional()`, `local()` has no "pass the input through
        // unchanged" fallback path, so there is no extra `downstream` union here.
        self.child.analyze_paths(analysis, downstream.clone())
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
    fn limits_per_vertex_not_per_stream() {
        // marko (id 1) has 3 out-edges (knows x2, created x1); vadas/josh/peter have out-edges
        // too in `tinkerpop_modern`. `local(outE().limit(1))` must keep exactly one edge *per
        // vertex that has at least one*, unlike a bare `outE().limit(1)` which would keep 1
        // total across the whole flattened stream.
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).local(__::out_e(None).limit(1)).count().to_list(),
            rhai: [
                r#"g.V().local(__.outE().limit(1)).count()"#,
                r#"g.v().local(__.out_e().limit(1)).count()"#,
            ],
            assert: |res| {
                // Vertices with at least one out-edge in tinkerpop_modern: marko(1), josh(4),
                // peter(6). vadas(2)/lop(3)/ripple(5) are outE sinks. So 3 vertices contribute
                // exactly 1 edge each under `local()`.
                assert_eq!(res[0], ElementProperty::Number(3.into()));
            },
        );
    }

    #[test]
    fn differs_from_bare_limit_on_the_flattened_stream() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let local_count = g
            .v(None)
            .local(__::out_e(None).limit(1))
            .count()
            .to_list()
            .unwrap();
        let g = g_lock.traversal();
        let bare_count = g.v(None).out_e(None).limit(1).count().to_list().unwrap();
        assert_eq!(local_count[0], ElementProperty::Number(3.into()));
        assert_eq!(bare_count[0], ElementProperty::Number(1.into()));
        assert_ne!(local_count, bare_count);
    }

    #[cfg(feature = "script")]
    #[test]
    fn per_vertex_property_extraction() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").local(__::properties(vec!["name"])).count().to_list(),
            rhai: [
                r#"g.V("1").local(__.properties("name")).count()"#,
                r#"g.v("1").local(__.properties("name")).count()"#,
            ],
            assert: |res| {
                assert_eq!(res[0], ElementProperty::Number(1.into()));
            },
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn empty_result_when_child_filters_everything_out() {
        // vadas (id 2) has no out-edges, so `local(outE().limit(1))` yields nothing for it.
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("2").local(__::out_e(None).limit(1)).to_list(),
            rhai: [
                r#"g.V("2").local(__.outE().limit(1))"#,
                r#"g.v("2").local(__.out_e().limit(1))"#,
            ],
            assert: |res| {
                assert!(res.is_empty());
            },
        );
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.v(None).local(__::out_e(None).limit(1)).profile().unwrap();
        assert!(result.to_string().contains("local("));
    }
}
