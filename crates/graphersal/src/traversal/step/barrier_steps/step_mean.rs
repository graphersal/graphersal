use super::numeric::{MeanAccumulator, resolve_number};
use crate::graph::GraphStorage;
use crate::*;

#[derive(Default, Clone)]
struct StepMean {}

impl fmt::Display for StepMean {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "mean()")
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Yields the arithmetic mean of the stream.
    pub fn mean(self) -> Self {
        self.push_step(GraphStep::Barrier("mean", Box::new(StepMean::default())))
    }
}

impl __ {
    /// Yields the arithmetic mean of the stream. Starts an anonymous traversal (`__.mean()`).
    pub fn mean<'g, S: GraphStorage + 'g>() -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().mean()
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Yields the arithmetic mean of the stream.
    pub fn mean(self) -> Self {
        let step = GraphStep::Barrier("mean", Box::new(StepMean::default()));
        self.push_step(step)
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepBarrier<'g, S> for StepMean {
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
        let mut mean = MeanAccumulator::new();
        for traverser in source.into_iter() {
            let bulk = traverser.bulk;
            if let Some((number, _)) = resolve_number(context, traverser)? {
                mean.push_n(number, bulk)?;
            }
        }
        // TinkerPop's `mean()` emits nothing on an empty (or all-null) numeric stream (confirmed
        // against TinkerPop 3.7's `MeanGlobalStep`, a seedless `ReducingBarrierStep`).
        Ok(mean
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

    /// TinkerPop 3.7's `mean()` is a seedless reducing barrier (`MeanGlobalStep`): an empty (or
    /// all-null) numeric stream emits nothing at all, not an error (confirmed via `graphersal`:
    /// `g.V().hasLabel("nope").values("age").mean()` produces no rows).
    #[cfg(feature = "script")]
    #[test]
    fn empty_input_emits_nothing() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).has_label("nope").values("age").mean().to_list(),
            rhai: [
                r#"g.V().hasLabel("nope").values("age").mean()"#,
                r#"g.v().has_label("nope").values("age").mean()"#,
            ],
            assert: |res| assert!(res.is_empty()),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).values("age").mean().to_list(),
            rhai: [
                r#"g.V().values("age").mean()"#,
                r#"g.v().values("age").mean()"#,
            ],
            assert: |res| assert_eq!(res[0].as_f64(), Some(30.75_f64)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn where_() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).values("age").where_t(__::mean()).count().to_list(),
            rhai: [
                r#"g.V().values("age").where(__.mean()).count()"#,
                r#"g.v().values("age").where(__.mean()).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(4)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn where_inner() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).where_t(__::values("age").mean()).count().to_list(),
            rhai: [
                r#"g.V().where(__.values("age").mean()).count()"#,
                r#"g.v().where(__.values("age").mean()).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(4)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn e() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e(None).values("weight").mean().to_list(),
            rhai: [
                r#"g.E().values("weight").mean()"#,
                r#"g.e().values("weight").mean()"#,
            ],
            assert: |res| assert_eq!(res[0].as_f64(), Some(0.5833333333333334_f64)),
        );
    }

    #[test]
    fn mix_a() {
        let graph = GraphSource::tinkerpop_modern();
        {
            let mut g_write = graph.write();
            let items: [(&str, ElementProperty); 4] = [
                ("1", 5i64.into()),
                ("2", 1.4.into()),
                ("3", 3.4.into()),
                ("4", 50i64.into()),
            ];
            for (id, val) in items {
                let _ = g_write.traversal_mut().v(id).property("count", val).next();
            }
        }
        let g_lock = graph.read();
        let result = g_lock.traversal().v(None).values("count").max().next();
        // An int + float mix is promoted to a float.
        assert_eq!(result.unwrap(), Some(ElementProperty::Number(50.0.into())));
    }

    #[test]
    fn mix_b() {
        let graph = GraphSource::tinkerpop_modern();
        {
            let mut g_write = graph.write();
            let items: [(&str, ElementProperty); 4] = [
                ("1", 5.1.into()),
                ("2", 1.4.into()),
                ("3", 3.4.into()),
                ("4", 50i64.into()),
            ];
            for (id, val) in items {
                let _ = g_write.traversal_mut().v(id).property("count", val).next();
            }
        }
        let g_lock = graph.read();
        let result = g_lock.traversal().v(None).values("count").max().next();
        // An int + float mix is promoted to a float.
        assert_eq!(result.unwrap(), Some(ElementProperty::Number(50.0.into())));
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.v(None).values("age").mean().profile().unwrap();
        assert!(result.to_string().contains("mean("));
    }
}
