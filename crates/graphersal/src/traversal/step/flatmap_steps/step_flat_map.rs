//! `flatMap(traversal)`: each traverser becomes every result of a child traversal.

use crate::graph::GraphStorage;
use crate::traversal::optimizer::path_analysis::{PathAnalysis, PathRequirement};
use crate::*;

/// `flatMap(traversal)` (TinkerPop `FlatMapStep` with a traversal); see
/// [`GraphTraversalSource::flat_map`].
struct StepFlatMap<'g, S: GraphStorage + 'g> {
    child: AnonymousTraversal<'g, S>,
}

impl<'g, S: GraphStorage + 'g> Clone for StepFlatMap<'g, S> {
    fn clone(&self) -> Self {
        Self {
            child: self.child.clone(),
        }
    }
}

impl<'g, S: GraphStorage + 'g> fmt::Display for StepFlatMap<'g, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "flat_map({})", self.child)
    }
}

fn flat_map_step<'g, S: GraphStorage + 'g>(child: AnonymousTraversal<'g, S>) -> GraphStep<'g, S> {
    GraphStep::FlatMap("flat_map", Box::new(StepFlatMap { child }))
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// `flatMap(traversal)` (TinkerPop `FlatMapStep`): replaces each traverser by **every**
    /// result of `traversal` run on it (path, bulk and sack included); a traverser for which
    /// `traversal` yields nothing is dropped.
    ///
    /// Unlike [`local`](Self::local), the steps of `traversal` are hidden from a later `path()`:
    /// each result is one new path position after the incoming traverser's path
    /// (`g.V().flat_map(__.out().out()).path()` → `[marko, ripple]`, `[marko, lop]`). For only
    /// the first result, use [`map`](Self::map). Details: `book/src/traversal/steps/flat_map.md`.
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let graph = GraphSource::tinkerpop_modern();
    /// let lock = graph.read();
    /// let names = lock.traversal().v("1").flat_map(__::out(None)).count().next().unwrap();
    /// assert_eq!(names, Some(ElementProperty::from(3i64)));
    /// ```
    pub fn flat_map(self, traversal: AnonymousTraversal<'g, S>) -> Self {
        self.push_step(flat_map_step(traversal))
    }
}

impl __ {
    /// Anonymous `flatMap(traversal)`; see [`GraphTraversalSource::flat_map`].
    pub fn flat_map<'g, S: GraphStorage + 'g>(
        traversal: AnonymousTraversal<'g, S>,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().flat_map(traversal)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// `flatMap(traversal)`; see [`GraphTraversalSource::flat_map`].
    pub fn flat_map(self, traversal: AnonymousTraversal<'g, S>) -> Self {
        self.push_step(flat_map_step(traversal))
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepFlatMap<'g, S> for StepFlatMap<'g, S> {
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
        output: &mut Vec<Traverser<S>>,
    ) -> TraverserResult<()> {
        // `apply_to` already multiplies every result's bulk by the incoming bulk; the results
        // continue the incoming path and keep its sack (TinkerPop splits the incoming traverser
        // once per result).
        let produced = self.child.apply_to(context, &source)?;
        output.extend(
            produced
                .into_iter()
                .map(|item| Traverser::with_parts(item.value, source.path, item.bulk, source.sack)),
        );
        Ok(())
    }

    fn child_traversals_mut(&mut self) -> Vec<&mut AnonymousTraversal<'g, S>> {
        vec![&mut self.child]
    }

    fn analyze_nested_paths(
        &mut self,
        analysis: &PathAnalysis,
        _downstream: &PathRequirement,
    ) -> PathRequirement {
        // The child runs on the incoming traverser, path included; its own path positions do
        // not continue the outer path.
        self.child.analyze_paths(analysis, PathRequirement::None)
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
    fn every_result() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).values("name").flat_map(__::split("a").unfold()).to_list(),
            rhai: [
                r#"g.V().values("name").flatMap(__.split("a").unfold())"#,
                r#"g.v().values("name").flat_map(__.split("a").unfold())"#,
            ],
            assert: |res| assert_eq!(res.len(), 9),
        );
    }

    /// The child's steps are hidden from `path()`.
    #[cfg(feature = "script")]
    #[test]
    fn hides_the_child_path() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).flat_map(__::out(None).out(None)).path().to_list(),
            rhai: [r#"g.V().flatMap(__.out().out()).path()"#],
            assert: |res| {
                assert_eq!(res.len(), 2);
                for path in res {
                    let ElementProperty::Array(path) = path else { panic!("{res:?}") };
                    assert_eq!(path.len(), 2);
                }
            },
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn reads_the_incoming_path() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).as_("a").flat_map(__::select("a")).count().to_list(),
            rhai: [r#"g.V().as("a").flatMap(__.select("a")).count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(6)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn keeps_bulk() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).both(None).barrier().flat_map(__::out(None)).count().to_list(),
            rhai: [r#"g.V().both().barrier().flatMap(__.out()).count()"#],
            // The same as g.V().both().out().count().
            assert: |res| assert_eq!(res[0].as_i64(), Some(16)),
        );
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let lock = graph.read();
        let result = lock
            .traversal()
            .v(None)
            .flat_map(__::out(None))
            .profile()
            .unwrap();
        assert!(result.to_string().contains("flat_map("), "{result}");
    }
}
