use crate::graph::GraphStorage;
use crate::*;

/// `union()`.
///
/// **Batch rule** (as TinkerPop's `BranchStep`): a branch that holds global state at its top level
/// (`count`, `sum`, `fold`, `group`, `order`, `dedup`, `limit`, `range`, `tail`, ...) is fed every
/// incoming traverser as one stream, so `union(limit(1), ...)` limits the whole stream and
/// `union(count())` counts it. The branches then run one after the other (branch-major output).
/// Without such a branch each traverser is run through the branches on its own (traverser-major
/// output). `local(...)` is the per-traverser form; `repeat()`, `barrier()` and `none()` do not
/// make a branch stateful. Deviation: none from TinkerPop, but a bulk-merged stream
/// (`bulk.merge`) is fed to the child with its bulks, as for `repeat()` bodies. Details: user's
/// guide, "Branching with stateful children".
///
/// **Deviations from TinkerPop 3.8.2** (user's guide: "union() as a Start Step"): as the first
/// step (`g.union(..)`) the traversal gets one seed traverser holding `null`, so
/// `g.union(__.identity())` yields one `null`. A branch that opens with `inject(..)` runs over an
/// empty source instead of the seed (it is its own start); otherwise
/// `g.union(__.inject(1), __.inject(2))` would echo the seed as `null, 1, null, 2`. In the middle
/// of a traversal `inject` still emits the incoming traverser.
pub(crate) struct StepUnion<'g, S: GraphStorage + 'g> {
    pub branches: Vec<AnonymousTraversal<'g, S>>,
    /// Set when this union is the first step of the root traversal (`g.union(..)`): its input is
    /// then the executor's seed, which a branch opening with `inject` must not echo.
    pub start: bool,
}

impl<'g, S: GraphStorage + 'g> Clone for StepUnion<'g, S> {
    fn clone(&self) -> Self {
        Self {
            branches: self.branches.clone(),
            start: self.start,
        }
    }
}

impl<'g, S: GraphStorage + 'g> fmt::Display for StepUnion<'g, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "union({})",
            crate::traversal::render_traversals(&self.branches)
        )
    }
}

impl __ {
    /// Runs every child traversal on the incoming traversers and merges their results (see the book
    /// page Branch Children and Global State). Starts an anonymous traversal (`__.union()`).
    pub fn union<'g, S: GraphStorage + 'g>(
        branches: Vec<AnonymousTraversal<'g, S>>,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().union(branches)
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Runs every child traversal on the incoming traversers and merges their results (see the book
    /// page Branch Children and Global State).
    pub fn union(self, branches: Vec<AnonymousTraversal<'g, S>>) -> Self {
        self.push_step(GraphStep::FlatMap(
            "union",
            Box::new(StepUnion {
                branches,
                start: false,
            }),
        ))
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Runs every child traversal on the incoming traversers and merges their results (see the book
    /// page Branch Children and Global State).
    pub fn union(self, branches: Vec<AnonymousTraversal<'g, S>>) -> Self {
        let step = GraphStep::FlatMap(
            "union",
            Box::new(StepUnion {
                branches,
                start: false,
            }),
        );
        self.push_step(step)
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepFlatMap<'g, S> for StepUnion<'g, S> {
    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
        output: &mut Vec<Traverser<S>>,
    ) -> TraverserResult<()> {
        for branch in &self.branches {
            // A branch opening with `inject` is its own start: it takes no seed along.
            let own_start = self.start
                && matches!(branch.steps.first(), Some(GraphStep::Append(name, _)) if *name == "inject");
            let branch_results = if own_start {
                branch.to_list(&mut *context, Vec::new())?
            } else {
                branch.apply_to(&mut *context, &source)?
            };
            output.extend(branch_results);
        }
        Ok(())
    }

    fn is_batch(&self) -> bool {
        self.branches.iter().any(|branch| branch.has_barrier())
    }

    fn process_batch(
        &self,
        context: &mut TraverserContext<'_, S>,
        input: Vec<Traverser<S>>,
        output: &mut Vec<Traverser<S>>,
    ) -> TraverserResult<()> {
        if input.is_empty() {
            return Ok(());
        }
        for branch in &self.branches {
            let own_start = self.start
                && matches!(branch.steps.first(), Some(GraphStep::Append(name, _)) if *name == "inject");
            let seed = if own_start { Vec::new() } else { input.clone() };
            output.extend(branch.to_list(&mut *context, seed)?);
        }
        Ok(())
    }

    fn child_traversals_mut(&mut self) -> Vec<&mut crate::traversal::AnonymousTraversal<'g, S>> {
        self.branches.iter_mut().collect()
    }

    fn extends_path(&self) -> bool {
        false
    }

    fn is_start(&self) -> bool {
        true
    }

    fn mark_start(&mut self) {
        self.start = true;
    }

    fn analyze_nested_paths(
        &mut self,
        analysis: &crate::traversal::optimizer::path_analysis::PathAnalysis,
        downstream: &crate::traversal::optimizer::path_analysis::PathRequirement,
    ) -> crate::traversal::optimizer::path_analysis::PathRequirement {
        // The output of every branch continues the path, so each branch keeps writing what
        // the steps after `union()` read.
        self.branches.iter_mut().fold(
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
    fn v_union_out_knows_and_created() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").union(vec![__::out("knows"), __::out("created")]).values("name").to_list(),
            rhai: [
                r#"g.V("1").union([__.out("knows"), __.out("created")]).values("name")"#,
                r#"g.v("1").union([__.out("knows"), __.out("created")]).values("name")"#,
            ],
            assert: |res| {
                assert_eq!(res.len(), 3);
                let names: Vec<String> = res.iter().filter_map(|v| v.as_string().map(String::from)).collect();
                assert_eq!(names, vec!["josh", "vadas", "lop"]);
            },
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_union_identity_and_out() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").union(vec![__::identity(), __::out("knows")]).values("name").to_list(),
            rhai: [
                r#"g.V("1").union([__.identity(), __.out("knows")]).values("name")"#,
                r#"g.v("1").union([__.identity(), __.out("knows")]).values("name")"#,
            ],
            assert: |res| {
                assert_eq!(res.len(), 3);
                let names: Vec<String> = res.iter().filter_map(|v| v.as_string().map(String::from)).collect();
                assert_eq!(names, vec!["marko", "josh", "vadas"]);
            },
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_union_empty() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").union(vec![]).values("name").to_list(),
            rhai: [
                r#"g.V("1").union([]).values("name")"#,
                r#"g.v("1").union([]).values("name")"#,
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
        let result = g
            .v("1")
            .union(vec![__::out("knows"), __::out("created")])
            .values("name")
            .profile()
            .unwrap();
        assert!(result.to_string().contains("union("));
    }
}
