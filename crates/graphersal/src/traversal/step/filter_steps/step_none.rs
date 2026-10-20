use std::fmt::Debug;

use crate::graph::GraphStorage;
use crate::traversal::step::filter_steps::step_any::list_items;
use crate::*;

#[derive(Clone)]
struct StepNone<B: Into<PValue> + Debug + Clone + Send + Sync> {
    predicate: P<B>,
}

impl<B: Into<PValue> + Debug + Clone + Send + Sync> fmt::Display for StepNone<B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "none({})",
            crate::value::predicate::render_predicate(&self.predicate)
        )
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// `none(P)`: keeps a list traverser (`fold()`, an injected list, a stored list property)
    /// none of whose elements satisfies the predicate (an empty list passes); a non-list value
    /// is filtered out. TinkerPop 3.8's list predicate step, the sibling of `any(P)`/`all(P)`;
    /// the old filter-everything `none()` is now [`discard()`](Self::discard).
    pub fn none<B: Into<PValue> + Debug + Clone + Send + Sync + 'static, T: Into<P<B>> + Debug>(
        self,
        predicate: T,
    ) -> Self {
        self.push_step(GraphStep::Filter(
            "none",
            Box::new(StepNone {
                predicate: predicate.into(),
            }),
        ))
    }
}

impl __ {
    /// `none(P)`: keeps a list traverser (`fold()`, an injected list, a stored list property)
    /// none of whose elements satisfies the predicate (an empty list passes); a non-list value
    /// is filtered out. TinkerPop 3.8's list predicate step, the sibling of `any(P)`/`all(P)`;
    /// the old filter-everything `none()` is now [`discard()`](Self::discard).
    pub fn none<
        'g,
        S: GraphStorage + 'g,
        B: Into<PValue> + std::fmt::Debug + Clone + Send + Sync + 'static,
        T: Into<P<B>> + Debug,
    >(
        predicate: T,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().none(predicate)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// `none(P)`: keeps a list traverser (`fold()`, an injected list, a stored list property)
    /// none of whose elements satisfies the predicate (an empty list passes); a non-list value
    /// is filtered out. TinkerPop 3.8's list predicate step, the sibling of `any(P)`/`all(P)`;
    /// the old filter-everything `none()` is now [`discard()`](Self::discard).
    pub fn none<B: Into<PValue> + Debug + Clone + Send + Sync + 'static, T: Into<P<B>> + Debug>(
        self,
        predicate: T,
    ) -> Self {
        let step = GraphStep::Filter(
            "none",
            Box::new(StepNone {
                predicate: predicate.into(),
            }),
        );
        self.push_step(step)
    }
}

impl<'g, S: GraphStorage + 'g, B: Into<PValue> + Debug + Clone + Send + Sync + 'static>
    GraphStepFilter<'g, S> for StepNone<B>
{
    /// TinkerPop's `none(P)` (`NoneStep`): keeps a list traverser when no element satisfies
    /// the predicate (an empty list passes). A non-list value (a scalar, `null`, a map) never
    /// passes.
    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: &Traverser<S>,
    ) -> TraverserResult<bool> {
        let Some(items) = list_items(context, &source.value)? else {
            return Ok(false);
        };
        for item in &items {
            if self.predicate.test(context, item)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn clone_box(&self) -> Box<dyn GraphStepFilter<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_a_list_without_a_match() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).values("age").fold().none(P::Lt(10)).count().to_list(),
            rhai: [
                r#"g.V().values("age").fold().none(P.lt(10)).count()"#,
                r#"g.v().values("age").fold().none(P::Lt(10)).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
    }

    #[test]
    fn drops_a_list_with_a_match() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).values("age").fold().none(P::Gt(30)).count().to_list(),
            rhai: [
                r#"g.V().values("age").fold().none(P.gt(30)).count()"#,
                r#"g.v().values("age").fold().none(P::Gt(30)).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(0)),
        );
    }

    /// An empty list passes (no element matches), and `null` elements compare like values.
    #[test]
    fn empty_list_passes_and_null_elements_compare() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .inject(vec![
                    ElementProperty::Array(vec![ElementProperty::Null, 1.into()]),
                    ElementProperty::Array(vec![]),
                ])
                .none(P::Eq(PValue::Null))
                .to_list(),
            rhai: [r#"g.inject([(), 1], []).none(P.eq(()))"#],
            assert: |res| assert_eq!(res, &[ElementProperty::Array(vec![])]),
        );
    }

    /// A non-list value never passes `none()` (TinkerPop), not even a non-matching scalar.
    #[test]
    fn scalars_are_filtered_out() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).values("age").none(P::Gt(100)).count().to_list(),
            rhai: [
                r#"g.V().values("age").none(P.gt(100)).count()"#,
                r#"g.inject(7, ()).none(P.eq(8)).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(0)),
        );
    }

    #[test]
    fn in_a_child_traversal() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v(None)
                .has_label("person")
                .where_t(__::out(None).values("name").fold().none(P::Eq("lop")))
                .values("name")
                .to_list(),
            rhai: [
                r#"g.V().hasLabel("person").where(__.out().values("name").fold().none(P.eq("lop"))).values("name")"#,
                r#"g.v().has_label("person").where(__.out().values("name").fold().none(P.eq("lop"))).values("name")"#,
            ],
            assert: |res| {
                let mut names: Vec<_> = res.iter().filter_map(|v| v.as_str().map(str::to_string)).collect();
                names.sort();
                assert_eq!(names, vec!["vadas".to_string()]);
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
            .values("age")
            .fold()
            .none(P::Gt(40))
            .profile()
            .unwrap();
        assert!(result.to_string().contains("none("));
    }
}
