use crate::graph::GraphStorage;
use crate::traversal::optimizer::path_analysis::{PathAnalysis, PathRequirement};
use crate::traversal::step::path_window::SimplicityFilter;
use crate::*;

/// `simple_path()`: keeps a traverser only when no object repeats along its path.
///
/// Objects are compared by identity for vertices and edges and by value otherwise (TinkerPop
/// `Path.isSimple()`), along the whole path including the current object.
///
/// `from(label)` / `to(label)` restrict the test to the window of the path between two labelled
/// positions, and `by(...)` projects the window's objects first (a `by()` that yields no value
/// for an object drops the traverser, for `simple_path()` and `cyclic_path()` alike). Bounds follow
/// TinkerPop's `Path.subPath`: the LAST position carrying each label, found independently; an
/// absent label or a `to()` before the `from()` is an error; a repeated `from()`/`to()` replaces
/// the earlier one. The modulated form materializes the path once per traverser; the plain form
/// tests the arena in place. See the "Path windows" page of the user's guide.
struct StepSimplePath<'g, S: GraphStorage + 'g>(SimplicityFilter<'g, S>);

impl<'g, S: GraphStorage + 'g> Clone for StepSimplePath<'g, S> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<'g, S: GraphStorage + 'g> Default for StepSimplePath<'g, S> {
    fn default() -> Self {
        Self(SimplicityFilter::default())
    }
}

impl<'g, S: GraphStorage + 'g> fmt::Display for StepSimplePath<'g, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "simple_path(){}", self.0.render_modulators())
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Keeps a traverser only when its path does not visit any object twice; the usual way to stop
    /// a traversal from walking in cycles.
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let graph = GraphSource::tinkerpop_modern();
    /// let lock = graph.read();
    /// let count = lock
    ///     .traversal()
    ///     .v("1")
    ///     .both(None)
    ///     .both(None)
    ///     .simple_path()
    ///     .count()
    ///     .next()
    ///     .unwrap();
    /// assert_eq!(count.and_then(|c| c.as_i64()), Some(4));
    /// ```
    pub fn simple_path(self) -> Self {
        self.push_step(GraphStep::Filter(
            "simple_path",
            Box::new(StepSimplePath::default()),
        ))
    }
}

impl __ {
    /// Anonymous `simple_path()`; see [`GraphTraversalSource::simple_path`].
    pub fn simple_path<'g, S: GraphStorage + 'g>() -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().simple_path()
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Keeps a traverser only when its path does not visit any object twice; the usual way to stop
    /// a traversal from walking in cycles.
    pub fn simple_path(self) -> Self {
        self.push_step(GraphStep::Filter(
            "simple_path",
            Box::new(StepSimplePath::default()),
        ))
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepFilter<'g, S> for StepSimplePath<'g, S> {
    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: &Traverser<S>,
    ) -> TraverserResult<bool> {
        Ok(self
            .0
            .is_simple("simple_path", context, source)?
            .unwrap_or(false))
    }

    fn path_requirement(&self) -> PathRequirement {
        PathRequirement::Full
    }

    fn plan_check(&self) -> TraverserResult<()> {
        self.0.plan_check("simple_path")
    }

    fn push_modulator_by(&mut self, args: By<'g, S>, _modulator_name: &str) -> ByModulation {
        self.0.push_by(args)
    }

    fn push_modulator_args(&mut self, args: Vec<String>, modulator_name: &str) -> ArgsModulation {
        self.0.push_args(args, modulator_name)
    }

    fn child_traversals_mut(&mut self) -> Vec<&mut crate::traversal::AnonymousTraversal<'g, S>> {
        self.0.traversals_mut()
    }

    fn analyze_nested_paths(
        &mut self,
        analysis: &PathAnalysis,
        _downstream: &PathRequirement,
    ) -> PathRequirement {
        self.0.analyze(analysis)
    }

    fn clone_box(&self) -> Box<dyn GraphStepFilter<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

#[cfg(all(test, feature = "script"))]
mod tests {
    use super::*;

    #[test]
    fn both_both() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").both(None).both(None).simple_path().values("name").to_list(),
            rhai: [
                r#"g.V("1").both().both().simplePath().values("name")"#,
                r#"g.v("1").both().both().simple_path().values("name")"#,
            ],
            assert: |res| {
                assert_eq!(res.len(), 4);
                assert!(res.iter().all(|r| r.as_string() != Some("marko")));
            },
        );
    }

    #[test]
    fn anonymous() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").out(None).where_t(__::in_(None).simple_path()).values("name").to_list(),
            rhai: [
                r#"g.V("1").out().where(__.in().simplePath()).values("name")"#,
                r#"g.v("1").out().where(__.in().simple_path()).values("name")"#,
            ],
            assert: |res| {
                assert_eq!(res.len(), 1);
                assert_eq!(res[0].as_string(), Some("lop"));
            },
        );
    }
}
