use super::numeric::{Extremum, ExtremumAccumulator, push_extremum_value};
use crate::graph::GraphStorage;
use crate::*;

#[derive(Default, Clone)]
struct StepMax {}

impl fmt::Display for StepMax {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "max()")
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// `max()` of the stream.
    ///
    /// `max()` promotes an integer + float mix to a float (the result is a float even when the
    /// extremum came from an integer; integers beyond 2^53 lose precision when widened) and also
    /// compares strings, booleans and UUIDs. Numbers compare with numbers and text with text; a
    /// mix of kinds, a graph element, a path or a collection fails with an
    /// `IncomparableValues` error (TinkerPop throws for mixed kinds but orders elements by id; a
    /// deliberate deviation, see the "Numeric rules" of the Local Scope page and
    /// `tinkerpop_deviations.md`). An empty or all-null stream emits nothing.
    pub fn max(self) -> Self {
        self.push_step(GraphStep::Barrier("max", Box::new(StepMax::default())))
    }
}

impl __ {
    /// Yields the largest value of the stream. Starts an anonymous traversal (`__.max()`).
    pub fn max<'g, S: GraphStorage + 'g>() -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().max()
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Yields the largest value of the stream.
    pub fn max(self) -> Self {
        let step = GraphStep::Barrier("max", Box::new(StepMax::default()));
        self.push_step(step)
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepBarrier<'g, S> for StepMax {
    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Vec<Traverser<S>>,
    ) -> TraverserResult<Option<Traverser<S>>> {
        // Bulk-insensitive: the extreme of a merged traverser is the extreme of its copies.
        let mut accumulator = ExtremumAccumulator::new(Extremum::Max);
        for traverser in source.into_iter() {
            push_extremum_value(&mut accumulator, context, traverser.value)?;
        }
        // TinkerPop's `max()` emits nothing on an empty (or all-null) stream (confirmed
        // against TinkerPop 3.7's `MaxGlobalStep`, a seedless `ReducingBarrierStep`).
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

    /// TinkerPop 3.7's `max()` is a seedless reducing barrier (`MaxGlobalStep`): an empty (or
    /// all-null) numeric stream emits nothing at all, not an error (confirmed via `graphersal`:
    /// `g.V().hasLabel("nope").values("age").max()` produces no rows).
    #[cfg(feature = "script")]
    #[test]
    fn empty_input_emits_nothing() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).has_label("nope").values("age").max().to_list(),
            rhai: [
                r#"g.V().hasLabel("nope").values("age").max()"#,
                r#"g.v().has_label("nope").values("age").max()"#,
            ],
            assert: |res| assert!(res.is_empty()),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).values("age").max().to_list(),
            rhai: [
                r#"g.V().values("age").max()"#,
                r#"g.v().values("age").max()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(35)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn where_t() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).values("age").where_t(__::max().is(P::Gt(30))).count().to_list(),
            rhai: [
                r#"g.V().values("age").where(__.max().is(P::Gt(30))).count()"#,
                r#"g.v().values("age").where(__.max().is(P::Gt(30))).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(2)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn where_t_inner() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).where_t(__::values("age").max().is(P::Gt(28))).count().to_list(),
            rhai: [
                r#"g.V().where(__.values("age").max().is(P::Gt(28))).count()"#,
                r#"g.v().where(__.values("age").max().is(P::Gt(28))).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(3)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn e() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e(None).values("weight").max().to_list(),
            rhai: [
                r#"g.E().values("weight").max()"#,
                r#"g.e().values("weight").max()"#,
            ],
            assert: |res| assert_eq!(res[0].as_f64(), Some(1.0)),
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
        let result = g.v(None).values("age").max().profile().unwrap();
        assert!(result.to_string().contains("max("));
    }
}
