//! `branch(traversal)`, the fan-out counterpart of `choose(traversal)` (TinkerPop `BranchStep`).
//!
//! Shares its representation (selector + `.option(pick, traversal)` list) with `choose()` —
//! `StepBranch` in `step_choose.rs`, which also carries this module's doc comment on the
//! confirmed `choose()` vs. `branch()` multiplicity semantics (spec D1). This file only adds the
//! `branch()` constructors (`BranchKind::Branch`, every matching option fires) and its tests,
//! including the fan-out test that is the one actually proving `choose()`/`branch()` are not
//! just aliases of the same code path with no observable difference.

use crate::graph::GraphStorage;
use crate::traversal::step::flatmap_steps::step_choose::{BranchKind, StepBranch};
use crate::*;

impl __ {
    /// `__.branch(traversal)`: the anonymous-traversal form, for use as a nested child of another
    /// step; see [`GraphTraversalSource::branch`].
    pub fn branch<'g, S: GraphStorage + 'g>(
        selector: AnonymousTraversal<'g, S>,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().branch(selector)
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// `branch(traversal)`: the fan-out counterpart of
    /// [`choose_switch`](Self::choose_switch) — every chained `.option(pick, traversal)` whose
    /// key accepts `selector`'s produced value runs (a literal by value, a predicate, or a
    /// traversal key via [`option_traversal`](Self::option_traversal)), so one traverser can flow
    /// into more than one branch's results. `Pick.any`'s branch additionally runs for every
    /// traverser; `Pick.none`'s is the fallback when nothing explicit matched, and
    /// `Pick.unproductive`'s when `selector` produced nothing; with neither, the traverser is
    /// dropped (unlike `choose()`, which passes it through). See `step_choose.rs`'s module doc
    /// comment and `book/src/users_guide/option_semantics.md`.
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let graph = GraphSource::tinkerpop_modern();
    /// let lock = graph.read();
    /// let names = lock
    ///     .traversal()
    ///     .v("1")
    ///     .branch(__::label())
    ///     .option(ElementProperty::from("person"), __::out("knows"))
    ///     .option(ElementProperty::from("person"), __::out("created"))
    ///     .values("name")
    ///     .to_list()
    ///     .unwrap();
    /// assert_eq!(names.len(), 3); // both "person" options fire: knows -> {vadas, josh}, created -> {lop}
    /// ```
    pub fn branch(self, selector: AnonymousTraversal<'g, S>) -> Self {
        self.push_step(GraphStep::FlatMap(
            "branch",
            Box::new(StepBranch::new_switch(BranchKind::Branch, selector)),
        ))
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Anonymous `branch(traversal)`; see [`GraphTraversalSource::branch`].
    pub fn branch(self, selector: AnonymousTraversal<'g, S>) -> Self {
        let step = GraphStep::FlatMap(
            "branch",
            Box::new(StepBranch::new_switch(BranchKind::Branch, selector)),
        );
        self.push_step(step)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "script")]
    #[test]
    fn branch_fans_out_on_multiple_matches() {
        // The key test proving branch() differs from choose(): both options key on "person" and
        // BOTH fire for marko (a person), unlike choose()'s first-match-only. marko knows vadas
        // and josh (2 results) and created lop (1 result) => 3 total, more than one input
        // traverser produces.
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v("1")
                .branch(__::label())
                .option(ElementProperty::from("person"), __::out("knows"))
                .option(ElementProperty::from("person"), __::out("created"))
                .values("name")
                .to_list(),
            rhai: [
                r#"g.V("1").branch(__.label()).option("person", __.out("knows")).option("person", __.out("created")).values("name")"#,
                r#"g.v("1").branch(__.label()).option("person", __.out("knows")).option("person", __.out("created")).values("name")"#,
            ],
            assert: |res| {
                let mut names: Vec<String> = res.iter().filter_map(|v| v.as_string().map(String::from)).collect();
                names.sort();
                assert_eq!(names, vec!["josh", "lop", "vadas"]);
            },
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn branch_pick_any_runs_additionally() {
        // Pick.any's branch runs for every traverser, in addition to whichever explicit option
        // also matched, unlike Pick.none (fallback only when nothing explicit matched).
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v(None)
                .branch(__::label())
                .option(ElementProperty::from("person"), __::constant(ElementProperty::from("matched")))
                .option_token(crate::traversal::step::flatmap_steps::step_choose::OptionToken::Any, __::constant(ElementProperty::from("always")))
                .to_list(),
            rhai: [
                r#"g.V().branch(__.label()).option("person", __.constant("matched")).option(Pick.any, __.constant("always"))"#,
                r#"g.v().branch(__.label()).option("person", __.constant("matched")).option(Pick.any, __.constant("always"))"#,
            ],
            assert: |res| {
                // 6 vertices, each gets an "always" (Pick.any); the 4 persons additionally get a
                // "matched" => 6 + 4 = 10.
                assert_eq!(res.len(), 10);
                let always = res.iter().filter(|v| v.as_string() == Some("always")).count();
                let matched = res.iter().filter(|v| v.as_string() == Some("matched")).count();
                assert_eq!(always, 6);
                assert_eq!(matched, 4);
            },
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn branch_no_match_drops_traverser() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v(None)
                .branch(__::label())
                .option(ElementProperty::from("person"), __::constant(ElementProperty::from("matched")))
                .to_list(),
            rhai: [
                r#"g.V().branch(__.label()).option("person", __.constant("matched"))"#,
                r#"g.v().branch(__.label()).option("person", __.constant("matched"))"#,
            ],
            assert: |res| {
                assert_eq!(res.len(), 4); // software vertices are dropped, no Pick.none default
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
            .branch(__::label())
            .option(ElementProperty::from("person"), __::identity())
            .profile()
            .unwrap();
        assert!(result.to_string().contains("branch("));
    }
}
