use super::numeric::{Extremum, ExtremumAccumulator, push_extremum_value};
use crate::graph::GraphStorage;
use crate::*;

#[derive(Default, Clone)]
struct StepMin {}

impl fmt::Display for StepMin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "min()")
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// `min()` of the stream.
    ///
    /// `min()` promotes an integer + float mix to a float (the result is a float even when the
    /// extremum came from an integer; integers beyond 2^53 lose precision when widened) and also
    /// compares strings, booleans and UUIDs. Numbers compare with numbers and text with text; a
    /// mix of kinds, a graph element, a path or a collection fails with an
    /// `IncomparableValues` error (TinkerPop throws for mixed kinds but orders elements by id; a
    /// deliberate deviation, see the "Numeric rules" of the Local Scope page and
    /// `tinkerpop_deviations.md`). An empty or all-null stream emits nothing.
    pub fn min(self) -> Self {
        self.push_step(GraphStep::Barrier("min", Box::new(StepMin::default())))
    }
}

impl __ {
    /// Yields the smallest value of the stream. Starts an anonymous traversal (`__.min()`).
    pub fn min<'g, S: GraphStorage + 'g>() -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().min()
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Yields the smallest value of the stream.
    pub fn min(self) -> Self {
        let step = GraphStep::Barrier("min", Box::new(StepMin::default()));
        self.push_step(step)
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepBarrier<'g, S> for StepMin {
    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Vec<Traverser<S>>,
    ) -> TraverserResult<Option<Traverser<S>>> {
        // Bulk-insensitive: the extreme of a merged traverser is the extreme of its copies.
        let mut accumulator = ExtremumAccumulator::new(Extremum::Min);
        for traverser in source.into_iter() {
            push_extremum_value(&mut accumulator, context, traverser.value)?;
        }
        // TinkerPop's `min()` emits nothing on an empty (or all-null) stream (confirmed
        // against TinkerPop 3.7's `MinGlobalStep`, a seedless `ReducingBarrierStep`).
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

    /// TinkerPop 3.7's `min()` is a seedless reducing barrier (`MinGlobalStep`): an empty (or
    /// all-null) numeric stream emits nothing at all, not an error (confirmed via `graphersal`:
    /// `g.V().hasLabel("nope").values("age").min()` produces no rows).
    #[cfg(feature = "script")]
    #[test]
    fn empty_input_emits_nothing() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).has_label("nope").values("age").min().to_list(),
            rhai: [
                r#"g.V().hasLabel("nope").values("age").min()"#,
                r#"g.v().has_label("nope").values("age").min()"#,
            ],
            assert: |res| assert!(res.is_empty()),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).values("age").min().to_list(),
            rhai: [
                r#"g.V().values("age").min()"#,
                r#"g.v().values("age").min()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(27)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn where_() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).values("age").where_t(__::min().is(P::Gt(30))).count().to_list(),
            rhai: [
                r#"g.V().values("age").where(__.min().is(P::Gt(30))).count()"#,
                r#"g.v().values("age").where(__.min().is(P::Gt(30))).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(2)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn where_inner() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).where_t(__::values("age").min().is(P::Gt(30))).count().to_list(),
            rhai: [
                r#"g.V().where(__.values("age").min().is(P::Gt(30))).count()"#,
                r#"g.v().where(__.values("age").min().is(P::Gt(30))).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(2)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn e() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e(None).values("weight").min().to_list(),
            rhai: [
                r#"g.E().values("weight").min()"#,
                r#"g.e().values("weight").min()"#,
            ],
            assert: |res| assert_eq!(res[0].as_f64(), Some(0.2)),
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
        let result = g_lock.traversal().v(None).values("count").min().next();
        // An int + float mix is promoted to a float.
        assert_eq!(result.unwrap(), Some(ElementProperty::Number(1.4.into())));
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
        let result = g_lock.traversal().v(None).values("count").min().next();
        // An int + float mix is promoted to a float.
        assert_eq!(result.unwrap(), Some(ElementProperty::Number(1.4.into())));
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.v(None).values("age").min().profile().unwrap();
        assert!(result.to_string().contains("min("));
    }
}
