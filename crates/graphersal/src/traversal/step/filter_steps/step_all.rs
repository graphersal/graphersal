use std::fmt::Debug;

use crate::graph::GraphStorage;
use crate::*;

#[derive(Clone)]
struct StepAll<B: Into<PValue> + Debug + Clone + Send + Sync> {
    predicate: P<B>,
}

impl<B: Into<PValue> + Debug + Clone + Send + Sync> fmt::Display for StepAll<B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "all({})",
            crate::value::predicate::render_predicate(&self.predicate)
        )
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// `all(P)`: keeps a list traverser (`fold()`, an injected list, a stored list property)
    /// whose every element (an empty list passes) satisfies the predicate; a non-list value is filtered out.
    pub fn all<B: Into<PValue> + Debug + Clone + Send + Sync + 'static, T: Into<P<B>> + Debug>(
        self,
        predicate: T,
    ) -> Self {
        self.push_step(GraphStep::Filter(
            "all",
            Box::new(StepAll {
                predicate: predicate.into(),
            }),
        ))
    }
}

impl __ {
    /// `all(P)`: keeps a list traverser (`fold()`, an injected list, a stored list property)
    /// whose every element (an empty list passes) satisfies the predicate; a non-list value is filtered out.
    pub fn all<
        'g,
        S: GraphStorage + 'g,
        B: Into<PValue> + std::fmt::Debug + Clone + Send + Sync + 'static,
        T: Into<P<B>> + Debug,
    >(
        predicate: T,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().all(predicate)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// `all(P)`: keeps a list traverser (`fold()`, an injected list, a stored list property)
    /// whose every element (an empty list passes) satisfies the predicate; a non-list value is filtered out.
    pub fn all<B: Into<PValue> + Debug + Clone + Send + Sync + 'static, T: Into<P<B>> + Debug>(
        self,
        predicate: T,
    ) -> Self {
        let step = GraphStep::Filter(
            "all",
            Box::new(StepAll {
                predicate: predicate.into(),
            }),
        );
        self.push_step(step)
    }
}

impl<'g, S: GraphStorage + 'g, B: Into<PValue> + Debug + Clone + Send + Sync + 'static>
    GraphStepFilter<'g, S> for StepAll<B>
{
    /// TinkerPop's `all(P)`: keeps a list traverser when every element satisfies the predicate
    /// (an empty list passes). A non-list value (a scalar, `null`, a map) never passes.
    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: &Traverser<S>,
    ) -> TraverserResult<bool> {
        let Some(items) =
            crate::traversal::step::filter_steps::step_any::list_items(context, &source.value)?
        else {
            return Ok(false);
        };
        for item in &items {
            if !self.predicate.test(context, item)? {
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
    fn v_simple() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).values("age").fold().all(27).count().to_list(),
            rhai: [
                r#"g.V().values("age").fold().all(27).count()"#,
                r#"g.v().values("age").fold().all(27).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(0)),
        );
    }

    #[test]
    fn v_simple_other() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).values("age").fold().all(P::Gt(10)).count().to_list(),
            rhai: [
                r#"g.V().values("age").fold().all(P::Gt(10)).count()"#,
                r#"g.v().values("age").fold().all(P::Gt(10)).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
    }

    #[test]
    fn v_simple_gte() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).values("age").fold().all(P::Gte(30)).count().to_list(),
            rhai: [
                r#"g.V().values("age").fold().all(P::Gte(30)).count()"#,
                r#"g.v().values("age").fold().all(P::Gte(30)).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(0)),
        );
    }

    #[test]
    fn e_simple() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e(None).values("weight").fold().all(0.4).count().to_list(),
            rhai: [
                r#"g.E().values("weight").fold().all(0.4).count()"#,
                r#"g.e().values("weight").fold().all(0.4).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(0)),
        );
    }

    #[test]
    fn e_simple_other_lte() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e(None).values("weight").fold().all(P::Lte(100)).count().to_list(),
            rhai: [
                r#"g.E().values("weight").fold().all(P::Lte(100)).count()"#,
                r#"g.e().values("weight").fold().all(P::Lte(100)).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
    }

    #[test]
    fn e_simple_other_gte_minus_one() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e(None).values("weight").fold().all(P::Gte(-1)).count().to_list(),
            rhai: [
                r#"g.E().values("weight").fold().all(P::Gte(-1)).count()"#,
                r#"g.e().values("weight").fold().all(P::Gte(-1)).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
    }

    #[test]
    fn e_simple_other_gte_zero() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e(None).values("weight").fold().all(P::Gte(0.0)).count().to_list(),
            rhai: [
                r#"g.E().values("weight").fold().all(P::Gte(0.0)).count()"#,
                r#"g.e().values("weight").fold().all(P::Gte(0.0)).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
    }

    #[test]
    fn e_simple_other_gte_zero_1() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e(None).values("weight").fold().all(P::Gte(0)).count().to_list(),
            rhai: [
                r#"g.E().values("weight").fold().all(P::Gte(0)).count()"#,
                r#"g.e().values("weight").fold().all(P::Gte(0)).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g
            .e(None)
            .values("weight")
            .fold()
            .all(P::Gte(0))
            .count()
            .profile()
            .unwrap();
        assert!(result.to_string().contains("all("));
    }

    /// A non-list value never passes `all()` (TinkerPop), not even a matching scalar or `null`.
    #[test]
    fn scalar_and_null_are_filtered_out() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).values("age").all(P::Gt(0)).count().to_list(),
            rhai: [
                r#"g.V().values("age").all(P.gt(0)).count()"#,
                r#"g.inject((), 1).all(P.eq(())).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(0)),
        );
    }
}
