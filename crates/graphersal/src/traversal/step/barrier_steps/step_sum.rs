use super::numeric::{WideningNumeric, resolve_number};
use crate::graph::GraphStorage;
use crate::*;

#[derive(Default, Clone)]
struct StepSum {}

impl fmt::Display for StepSum {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "sum()")
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// `sum()` of the numbers in the stream. An integer + float mix is summed as a float
    /// (integers beyond 2^53 lose precision when widened, as in TinkerPop); integer-only streams
    /// stay exact and overflow raises `InvalidNumber`. An empty or all-null stream emits nothing.
    pub fn sum(self) -> Self {
        self.push_step(GraphStep::Barrier("sum", Box::new(StepSum::default())))
    }
}

impl __ {
    /// Yields the sum of the numbers in the stream. Starts an anonymous traversal (`__.sum()`).
    pub fn sum<'g, S: GraphStorage + 'g>() -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().sum()
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Yields the sum of the numbers in the stream.
    pub fn sum(self) -> Self {
        let step = GraphStep::Barrier("sum", Box::new(StepSum::default()));
        self.push_step(step)
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepBarrier<'g, S> for StepSum {
    /// Builds an aggregation/materialization buffer (D3 of the memory-inspection spec) — reported
    /// to `.profile()` as a non-zero `bytes_materialized`.
    fn materializes(&self) -> bool {
        true
    }

    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Vec<Traverser<S>>,
    ) -> TraverserResult<Option<Traverser<S>>> {
        let mut accumulator = WideningNumeric::new();
        for traverser in source.into_iter() {
            let bulk = traverser.bulk;
            if let Some((number, _source)) = resolve_number(context, traverser)? {
                // A merged traverser stands for `bulk` equal values.
                let number = number.times(bulk, "sum")?;
                accumulator.push("sum", number, i64::checked_add, |a, b| a + b)?;
            }
        }
        // TinkerPop's `sum()` emits nothing on an empty (or all-null) numeric stream, matching
        // `mean()`/`min()`/`max()`/`median()` (confirmed against TinkerPop 3.7's `SumGlobalStep`,
        // a seedless `ReducingBarrierStep`).
        Ok(accumulator
            .finish()
            .map(|value| Traverser::start(context, TraverserValue::Computed(value, None))))
    }

    fn clone_box(&self) -> Box<dyn GraphStepBarrier<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// TinkerPop 3.7's `sum()` is a seedless reducing barrier (`SumGlobalStep`): an empty (or
    /// all-null) numeric stream emits nothing at all, not `0` and not an error (confirmed via
    /// `graphersal`: `g.V().hasLabel("nope").values("age").sum()` produces no rows). A fused
    /// `count()` must then read `0`, matching `.to_list().len()`.
    #[cfg(feature = "script")]
    #[test]
    fn empty_input_emits_nothing() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).has_label("nope").values("age").sum().to_list(),
            rhai: [
                r#"g.V().hasLabel("nope").values("age").sum()"#,
                r#"g.v().has_label("nope").values("age").sum()"#,
            ],
            assert: |res| assert!(res.is_empty()),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn empty_input_count_is_zero() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).has_label("nope").values("age").sum().count().to_list(),
            rhai: [
                r#"g.V().hasLabel("nope").values("age").sum().count()"#,
                r#"g.v().has_label("nope").values("age").sum().count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(0)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_age() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).values("age").sum().to_list(),
            rhai: [
                r#"g.V().values("age").sum()"#,
                r#"g.v().values("age").sum()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(123)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn where_() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).values("age").where_t(__::sum().is(P::Gt(1))).count().to_list(),
            rhai: [
                r#"g.V().values("age").where(__.sum().is(P::Gt(1))).count()"#,
                r#"g.v().values("age").where(__.sum().is(P::Gt(1))).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(4)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn where_inner() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).where_t(__::values("age").sum().is(P::Gt(1))).count().to_list(),
            rhai: [
                r#"g.V().where(__.values("age").sum().is(P::Gt(1))).count()"#,
                r#"g.v().where(__.values("age").sum().is(P::Gt(1))).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(4)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn inject_float() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.inject(vec![0.3, 0.2, 0.3]).sum().to_list(),
            rhai: [
                r#"g.inject(0.3).inject(0.2).inject(0.3).sum()"#,
            ],
            assert: |res| assert_eq!(res[0].as_f64(), Some(0.8)),
        );
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.v(None).values("age").sum().profile().unwrap();
        assert!(result.to_string().contains("sum("));
    }
}
