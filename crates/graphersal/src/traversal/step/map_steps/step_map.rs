//! `map(traversal)`: each traverser becomes the first result of a child traversal.

use crate::graph::GraphStorage;
use crate::traversal::optimizer::path_analysis::{PathAnalysis, PathRequirement};
use crate::*;

/// `map(traversal)` (TinkerPop `TraversalMapStep`); see [`GraphTraversalSource::map`].
struct StepMap<'g, S: GraphStorage + 'g> {
    child: AnonymousTraversal<'g, S>,
}

impl<'g, S: GraphStorage + 'g> Clone for StepMap<'g, S> {
    fn clone(&self) -> Self {
        Self {
            child: self.child.clone(),
        }
    }
}

impl<'g, S: GraphStorage + 'g> fmt::Display for StepMap<'g, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "map({})", self.child)
    }
}

fn map_step<'g, S: GraphStorage + 'g>(child: AnonymousTraversal<'g, S>) -> GraphStep<'g, S> {
    GraphStep::Map("map", Box::new(StepMap { child }))
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// `map(traversal)` (TinkerPop `TraversalMapStep`): replaces each traverser's value by the
    /// **first** result of `traversal` run on it (path, bulk and sack included); a traverser
    /// for which `traversal` yields nothing is dropped
    /// (`g.V().map(__.out("created"))` keeps only the vertices that created something).
    ///
    /// The result is one new path position: the steps of `traversal` do not appear in a later
    /// `path()`. For every result instead of the first, use
    /// [`flat_map`](Self::flat_map). Details: `book/src/traversal/steps/map.md`.
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let graph = GraphSource::tinkerpop_modern();
    /// let lock = graph.read();
    /// let names = lock.traversal().v("1").map(__::values("name")).to_list().unwrap();
    /// assert_eq!(names, vec![ElementProperty::from("marko")]);
    /// ```
    pub fn map(self, traversal: AnonymousTraversal<'g, S>) -> Self {
        self.push_step(map_step(traversal))
    }
}

impl __ {
    /// Anonymous `map(traversal)`; see [`GraphTraversalSource::map`].
    pub fn map<'g, S: GraphStorage + 'g>(
        traversal: AnonymousTraversal<'g, S>,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().map(traversal)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// `map(traversal)`; see [`GraphTraversalSource::map`].
    pub fn map(self, traversal: AnonymousTraversal<'g, S>) -> Self {
        self.push_step(map_step(traversal))
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepMap<'g, S> for StepMap<'g, S> {
    /// Never called by the executor: `may_filter()` is true, and it calls `process_filter`.
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<Traverser<S>> {
        self.process_filter(context, source)?.ok_or_else(|| {
            Box::new(TraverserError::QueryExecution {
                query: self.to_string(),
                reason: "map() found no result for a traverser".to_string(),
            })
        })
    }

    fn process_filter(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<Option<Traverser<S>>> {
        Ok(self.child.first(context, &source)?.map(|first| {
            // Derived from the incoming traverser: its path, bulk and sack.
            Traverser::with_parts(first.value, source.path, source.bulk, source.sack)
        }))
    }

    fn may_filter(&self) -> bool {
        true
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

    fn child_traversals_mut(&mut self) -> Vec<&mut AnonymousTraversal<'g, S>> {
        vec![&mut self.child]
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
    fn maps_to_the_first_result() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").out(None).map(__::values("name")).map(__::length()).to_list(),
            rhai: [
                r#"g.V("1").out().map(__.values("name")).map(__.length())"#,
                r#"g.v("1").out().map(__.values("name")).map(__.length())"#,
            ],
            assert: |res| {
                let mut lengths: Vec<i64> = res.iter().filter_map(ElementProperty::as_i64).collect();
                lengths.sort();
                assert_eq!(lengths, vec![3, 4, 5]);
            },
        );
    }

    /// A traverser the child maps to nothing is dropped; `null` is a result like any other.
    #[cfg(feature = "script")]
    #[test]
    fn no_result_drops_null_is_kept() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).map(__::out(vec!["created"])).count().to_list(),
            rhai: [r#"g.V().map(__.out("created")).count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(3)),
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).map(__::constant(ElementProperty::Null)).to_list(),
            rhai: [r#"g.V().map(__.constant(()))"#],
            assert: |res| assert_eq!(res, &vec![ElementProperty::Null; 6]),
        );
    }

    /// The child reads the incoming path; its own steps are not path positions.
    #[cfg(feature = "script")]
    #[test]
    fn reads_the_path_and_adds_one_position() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v("1")
                .as_("a")
                .out(vec!["created"])
                .map(__::select("a").values("name"))
                .path()
                .to_list(),
            rhai: [r#"g.V("1").as("a").out("created").map(__.select("a").values("name")).path()"#],
            assert: |res| {
                let ElementProperty::Array(path) = &res[0] else { panic!("{res:?}") };
                assert_eq!(path.len(), 3);
                assert_eq!(path[2], ElementProperty::from("marko"));
            },
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn keeps_bulk() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).both(None).barrier().map(__::label()).count().to_list(),
            rhai: [r#"g.V().both().barrier().map(__.label()).count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(12)),
        );
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let lock = graph.read();
        let result = lock
            .traversal()
            .v(None)
            .map(__::values("name"))
            .profile()
            .unwrap();
        assert!(result.to_string().contains("map("), "{result}");
    }
}
