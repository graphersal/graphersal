use crate::graph::GraphStorage;
use crate::traversal::step::order_clauses::OrderClauses;
use crate::*;

/// `order()`: sorts the whole stream by its [`OrderClauses`] (see that type for how multiple
/// `.by()` calls accumulate as tiebreakers). The clause list, key resolution and the stable
/// comparator are shared with `order(Scope.local)` (`map_steps/step_order_local.rs`).
struct StepOrder<'g, S: GraphStorage + 'g> {
    clauses: OrderClauses<'g, S>,
}

impl<'g, S: GraphStorage + 'g> Default for StepOrder<'g, S> {
    fn default() -> Self {
        Self {
            clauses: OrderClauses::default(),
        }
    }
}

impl<'g, S: GraphStorage + 'g> Clone for StepOrder<'g, S> {
    fn clone(&self) -> Self {
        Self {
            clauses: self.clauses.clone(),
        }
    }
}

impl<'g, S: GraphStorage + 'g> fmt::Display for StepOrder<'g, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "order()")?;
        self.clauses.fmt_clauses(f)
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Sorts the whole stream, per Apache TinkerPop's `order()`.
    ///
    /// With no `.by()` clause, traversers sort by their own materialized value in ascending
    /// natural order, using this crate's shared `ElementProperty` total-order comparator.
    ///
    /// Any number of `.by()` calls can be chained. The first is the primary sort key; each
    /// subsequent one is a tiebreaker, applied only when every earlier clause compares equal
    /// between two traversers (`order().by("age", Order.asc).by("name", Order.desc)` sorts by
    /// `age` ascending, breaking ties in `age` by `name` descending). Each clause is a direction
    /// only (`.by(Order.asc)`/`.by(Order.desc)`), a property key (`.by("age")`, implicit
    /// ascending), a property key with an explicit direction (`.by("age", Order.desc)`), or a
    /// nested traversal's per-traverser result, with or without a direction
    /// (`.by(__.outE().count(), Order.desc)`). If every clause compares equal, input order is
    /// preserved (the sort is stable, see below).
    ///
    /// A nested traversal sorts by its *first* result (`by(__.out().values("name"))` sorts by
    /// the first name reached). A traverser for which any clause is unproductive (a missing
    /// property, a nested traversal yielding nothing) is filtered out, as in TinkerPop 3.6+:
    /// `g.V().order().by("age")` returns only the vertices that have an `age`.
    ///
    /// This is a `FlatBarrier` step: it buffers and re-emits the whole incoming stream (whole
    /// batch in, whole batch out), unlike `Barrier` steps (e.g. `group()`, `count()`) which
    /// collapse the stream into a single aggregate traverser. The sort itself is a stable sort
    /// (`Vec::sort_by`, never `sort_unstable_by`): traversers whose sort keys compare equal on
    /// every clause keep their relative input order, matching TinkerPop's own `order()`
    /// contract.
    pub fn order(self) -> Self {
        self.push_step(GraphStep::FlatBarrier(
            "order",
            Box::new(StepOrder::default()),
        ))
    }
}

impl __ {
    /// See [`GraphTraversalSource::order`].
    pub fn order<'g, S: GraphStorage + 'g>() -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().order()
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// See [`GraphTraversalSource::order`].
    pub fn order(self) -> Self {
        let step = GraphStep::FlatBarrier("order", Box::new(StepOrder::default()));
        self.push_step(step)
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepFlatBarrier<'g, S> for StepOrder<'g, S> {
    fn property_access(
        &self,
        visit: &mut dyn FnMut(Option<&str>, crate::graph::workload::AccessKind),
    ) {
        for key in self.clauses.property_keys() {
            visit(Some(key), crate::graph::workload::AccessKind::OrderGroup);
        }
    }
    /// `order()` materializes the whole stream into a `Vec` to sort it (D3 of the
    /// memory-inspection spec) — reported to `.profile()` as a non-zero `bytes_materialized`.
    fn materializes(&self) -> bool {
        true
    }

    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Vec<Traverser<S>>,
    ) -> TraverserResult<Option<Vec<Traverser<S>>>> {
        self.clauses.check("order")?;
        // Resolve every clause's sort key for every traverser once up front (not repeatedly
        // inside the comparator, which would re-run property/traversal resolution O(n log n)
        // times per clause instead of O(n)). An unproductive `by()` filters the traverser out, as
        // TinkerPop 3.6+'s `OrderGlobalStep` does.
        let mut keyed: Vec<(Vec<ElementProperty>, Traverser<S>)> = Vec::with_capacity(source.len());
        for traverser in source {
            context.tick()?;
            if let Some(keys) = self.clauses.keys(context, &traverser)? {
                keyed.push((keys, traverser));
            }
        }
        self.clauses.sort(&mut keyed, context);
        // The sort itself cannot be interrupted; a budget it used up fails here, at `order()`.
        context.check_deadline()?;
        Ok(Some(
            keyed.into_iter().map(|(_, traverser)| traverser).collect(),
        ))
    }

    /// Merges a `.by()` modulator into the clause list (see [`OrderClauses::push_by`]).
    fn push_modulator_by(&mut self, args: By<'g, S>, _modulator_name: &str) -> ByModulation {
        self.clauses.push_by(args)
    }

    fn child_traversals_mut(&mut self) -> Vec<&mut crate::traversal::AnonymousTraversal<'g, S>> {
        self.clauses.traversals_mut()
    }

    fn analyze_nested_paths(
        &mut self,
        analysis: &crate::traversal::optimizer::path_analysis::PathAnalysis,
        _downstream: &crate::traversal::optimizer::path_analysis::PathRequirement,
    ) -> crate::traversal::optimizer::path_analysis::PathRequirement {
        self.clauses.analyze_paths(analysis)
    }

    fn get_name(&self) -> String {
        self.to_string()
    }

    fn clone_box(&self) -> Box<dyn GraphStepFlatBarrier<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No `.by()`: natural ascending order over the traversers' own materialized value
    /// (`compare_element_property`'s defined order — numeric here, `age` values 27/29/32/35).
    #[test]
    fn no_by_natural_order() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).values("age").order().to_list(),
            rhai: [
                r#"g.V().values("age").order()"#,
                r#"g.v().values("age").order()"#,
            ],
            assert: |res| {
                let ages: Vec<i64> = res.iter().map(|v| v.as_i64().unwrap()).collect();
                assert_eq!(ages, vec![27, 29, 32, 35]);
            },
        );
    }

    /// A single `.by("age")` call, no second call at all: implicit ascending, one clause, one
    /// `push_modulator_by` invocation. Regression check that a genuinely single `.by(key)` call
    /// (not followed by a merging `.by(Order)`) still works after the `Option` -> `Vec` change.
    #[test]
    fn by_key_only_implicit_ascending() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).has_label("person").order().by("age").values("name").to_list(),
            rhai: [
                r#"g.V().hasLabel("person").order().by("age").values("name")"#,
                r#"g.v().has_label("person").order().by("age").values("name")"#,
            ],
            assert: |res| {
                let names: Vec<&str> = res.iter().map(|v| v.as_str().unwrap()).collect();
                assert_eq!(names, vec!["vadas", "marko", "josh", "peter"]);
            },
        );
    }

    /// `.by("age", Order.asc)` — explicit ascending, property-keyed.
    #[test]
    fn by_age_asc() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).has_label("person").order().by("age").by(Order::Asc).values("name").to_list(),
            rhai: [
                r#"g.V().hasLabel("person").order().by("age", Order.asc).values("name")"#,
                r#"g.v().has_label("person").order().by("age").by(Order::Asc).values("name")"#,
            ],
            assert: |res| {
                let names: Vec<&str> = res.iter().map(|v| v.as_str().unwrap()).collect();
                assert_eq!(names, vec!["vadas", "marko", "josh", "peter"]);
            },
        );
    }

    /// `.by("age", Order.desc)` — explicit descending, property-keyed. Matches part 2's
    /// acceptance query.
    #[test]
    fn by_age_desc() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).has_label("person").order().by("age").by(Order::Desc).values("name").to_list(),
            rhai: [
                r#"g.V().hasLabel("person").order().by("age", Order.desc).values("name")"#,
                r#"g.v().has_label("person").order().by("age").by(Order::Desc).values("name")"#,
            ],
            assert: |res| {
                let names: Vec<&str> = res.iter().map(|v| v.as_str().unwrap()).collect();
                assert_eq!(names, vec!["peter", "josh", "marko", "vadas"]);
            },
        );
    }

    /// `.by(__.outE().count(), Order.desc)` — nested-traversal sort key, descending. `marko`
    /// (id "1") has 3 outgoing edges, `josh` (id "4") has 2, everyone else has 0.
    #[test]
    fn by_traversal_out_e_count_desc() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v(None)
                .has_label("person")
                .order()
                .by(__::out_e(None).count())
                .by(Order::Desc)
                .id()
                .to_list(),
            rhai: [
                r#"g.V().hasLabel("person").order().by(__.outE().count(), Order.desc).id()"#,
                r#"g.v().has_label("person").order().by(__.out_e().count(), Order::Desc).id()"#,
            ],
            assert: |res| {
                let ids: Vec<&str> = res.iter().map(|v| v.as_str().unwrap()).collect();
                assert_eq!(ids[0], "1");
                assert_eq!(ids[1], "4");
            },
        );
    }

    /// Stability: two traversers whose sort key compares equal (both `person` vertices'
    /// `label()` is the literal string `"person"`) must keep their relative input order. `V()`
    /// on `tinkerpop_modern` yields vertices in insertion order 1 (marko), 2 (vadas), 3 (lop),
    /// 4 (josh), 5 (ripple), 6 (peter); filtering to `person` gives input order
    /// marko, vadas, josh, peter (ids 1, 2, 4, 6) — `order()` on the constant `"person"` label
    /// must reproduce that exact order, not merely "some" ordering.
    #[test]
    fn stable_sort_preserves_input_order_on_equal_keys() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).has_label("person").order().by(By::Label).id().to_list(),
            rhai: [
                r#"g.V().hasLabel("person").order().by(By::Label).id()"#,
                r#"g.v().has_label("person").order().by(By::Label).id()"#,
            ],
            assert: |res| {
                let ids: Vec<&str> = res.iter().map(|v| v.as_str().unwrap()).collect();
                assert_eq!(ids, vec!["1", "2", "4", "6"]);
            },
        );
    }

    /// A bare `.by(Order.desc)` sorts by the traverser's own current value, direction only —
    /// no property/traversal key.
    #[test]
    fn by_order_only_desc() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).values("age").order().by(Order::Desc).to_list(),
            rhai: [
                r#"g.V().values("age").order().by(Order.desc)"#,
                r#"g.v().values("age").order().by(Order::Desc)"#,
            ],
            assert: |res| {
                let ages: Vec<i64> = res.iter().map(|v| v.as_i64().unwrap()).collect();
                assert_eq!(ages, vec![35, 32, 29, 27]);
            },
        );
    }

    /// Part 3's core behavior change from part 2: a second, *unrelated* `.by()` call now
    /// accumulates as a tiebreaker instead of overwriting the first clause. All 6 vertices tie
    /// on `label()` within their own label (4 `person`, 2 `software`), so
    /// `.by(By::Label).by("name", Order.desc)` sorts by label first (grouping `person` before
    /// `software`, ascending), then breaks each group's tie by `name` descending. If the second
    /// call had instead overwritten the first (part 2's old behavior), the result would be a
    /// flat, ungrouped sort by `name` descending alone
    /// (`vadas, ripple, peter, marko, lop, josh`) — a different, distinguishable order from the
    /// grouped-then-tiebroken one this test asserts, which is what actually proves accumulation
    /// rather than merely re-checking a single-key sort.
    #[test]
    fn second_unrelated_by_call_accumulates_as_tiebreaker() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v(None)
                .order()
                .by(By::Label)
                .by("name")
                .by(Order::Desc)
                .values("name")
                .to_list(),
            rhai: [
                r#"g.V().order().by(By::Label).by("name", Order.desc).values("name")"#,
                r#"g.v().order().by(By::Label).by("name").by(Order::Desc).values("name")"#,
            ],
            assert: |res| {
                let names: Vec<&str> = res.iter().map(|v| v.as_str().unwrap()).collect();
                // person group ("person" < "software" ascending) sorted by name descending,
                // then the software group sorted by name descending.
                assert_eq!(names, vec!["vadas", "peter", "marko", "josh", "ripple", "lop"]);
            },
        );
    }

    /// Two genuinely independent property keys, `label()` then `name`, where the first key
    /// produces real ties (4 `person` vertices, 2 `software` vertices) that the second key
    /// breaks. Same shape as the accumulation test above but phrased as the spec's dedicated
    /// "two-key sort with a provable tiebreak" case, using ascending order throughout.
    #[test]
    fn two_key_sort_label_then_name_tiebreak() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).order().by(By::Label).by("name").values("name").to_list(),
            rhai: [
                r#"g.V().order().by(By::Label).by("name").values("name")"#,
                r#"g.v().order().by(By::Label).by("name").values("name")"#,
            ],
            assert: |res| {
                let names: Vec<&str> = res.iter().map(|v| v.as_str().unwrap()).collect();
                assert_eq!(names, vec!["josh", "marko", "peter", "vadas", "lop", "ripple"]);
            },
        );
    }

    /// Two-key chain mixing a nested-traversal-keyed clause (`.by(__.outE().count(), desc)`)
    /// with a property-keyed tiebreaker (`.by("name")`), scoped to `person` vertices only.
    /// `marko` has 3 outgoing edges, `josh` has 2, `vadas` and `peter` both have 0 — a real tie
    /// on the traversal key that the `name` tiebreaker must break (alphabetically: peter before
    /// vadas).
    #[test]
    fn two_key_traversal_then_property_tiebreak() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v(None)
                .has_label("person")
                .order()
                .by(__::out_e(None).count())
                .by(Order::Desc)
                .by("name")
                .values("name")
                .to_list(),
            rhai: [
                r#"g.V().hasLabel("person").order().by(__.outE().count(), Order.desc).by("name").values("name")"#,
                r#"g.v().has_label("person").order().by(__.out_e().count(), Order::Desc).by("name").values("name")"#,
            ],
            assert: |res| {
                let names: Vec<&str> = res.iter().map(|v| v.as_str().unwrap()).collect();
                assert_eq!(names, vec!["marko", "josh", "peter", "vadas"]);
            },
        );
    }

    /// Three-key chain over all 6 vertices, mixing property keys and a nested-traversal key:
    /// `label()` (ties: 4 `person`, 2 `software`) -> outgoing-edge count descending (further
    /// ties within each label group: `vadas`/`peter` both have 0 outgoing edges among persons,
    /// both `software` vertices have 0 outgoing edges) -> `name` ascending, which finally breaks
    /// every remaining tie.
    #[test]
    fn three_key_chain_label_outedges_name() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v(None)
                .order()
                .by(By::Label)
                .by(__::out_e(None).count())
                .by(Order::Desc)
                .by("name")
                .values("name")
                .to_list(),
            rhai: [
                r#"g.V().order().by(By::Label).by(__.outE().count(), Order.desc).by("name").values("name")"#,
                r#"g.v().order().by(By::Label).by(__.out_e().count(), Order::Desc).by("name").values("name")"#,
            ],
            assert: |res| {
                let names: Vec<&str> = res.iter().map(|v| v.as_str().unwrap()).collect();
                assert_eq!(names, vec!["marko", "josh", "peter", "vadas", "lop", "ripple"]);
            },
        );
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.v(None).values("age").order().profile().unwrap();
        assert!(result.to_string().contains("order("));
    }
}
