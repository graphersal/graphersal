use super::numeric::resolve_number;
use crate::graph::GraphStorage;
use crate::*;

#[derive(Default, Clone)]
struct StepMedian {}

impl fmt::Display for StepMedian {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "median()")
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Yields the median of the numbers in the stream.
    pub fn median(self) -> Self {
        self.push_step(GraphStep::Barrier(
            "median",
            Box::new(StepMedian::default()),
        ))
    }
}

impl __ {
    /// Yields the median of the numbers in the stream. Starts an anonymous traversal
    /// (`__.median()`).
    pub fn median<'g, S: GraphStorage + 'g>() -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().median()
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Yields the median of the numbers in the stream.
    pub fn median(self) -> Self {
        let step = GraphStep::Barrier("median", Box::new(StepMedian::default()));
        self.push_step(step)
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepBarrier<'g, S> for StepMedian {
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
        // Weighted median: each value counts `bulk` times, without expanding the copies.
        let mut values: Vec<(f64, u64)> = Vec::with_capacity(source.len());
        let mut total: u64 = 0;
        for traverser in source.into_iter() {
            let bulk = traverser.bulk;
            if let Some((number, _)) = resolve_number(context, traverser)? {
                total = total.checked_add(bulk).ok_or_else(|| {
                    Box::new(TraverserError::BulkOverflow {
                        step: "median".to_string(),
                    })
                })?;
                values.push((number.as_f64(), bulk));
            }
        }
        // Matches `sum()`/`mean()`/`min()`/`max()`: nothing on an empty (or all-null) numeric
        // stream, not an error.
        if values.is_empty() {
            return Ok(None);
        }
        values.sort_by(|a, b| a.0.total_cmp(&b.0));
        // The value at position `index` of the sorted, conceptually expanded list.
        let at = |index: u64| -> f64 {
            let mut seen: u64 = 0;
            for (value, bulk) in &values {
                seen += bulk;
                if index < seen {
                    return *value;
                }
            }
            values.last().map_or(0.0, |(value, _)| *value)
        };
        let median = if total.is_multiple_of(2) {
            (at(total / 2 - 1) + at(total / 2)) / 2_f64
        } else {
            at((total - 1) / 2)
        };
        Ok(Some(Traverser::start(
            context,
            TraverserValue::Computed(ElementProperty::Number(median.into()), None),
        )))
    }

    fn clone_box(&self) -> Box<dyn GraphStepBarrier<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Consistent with `sum()`/`mean()`/`min()`/`max()`: an empty (or all-null) numeric stream
    /// emits nothing at all, not an error. `median()` is not itself a TinkerPop step, but it
    /// shares `numeric.rs`'s accumulator and is kept consistent with its siblings' seedless
    /// empty-input behavior.
    #[cfg(feature = "script")]
    #[test]
    fn empty_input_emits_nothing() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).has_label("nope").values("age").median().to_list(),
            rhai: [
                r#"g.V().hasLabel("nope").values("age").median()"#,
                r#"g.v().has_label("nope").values("age").median()"#,
            ],
            assert: |res| assert!(res.is_empty()),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).values("age").median().to_list(),
            rhai: [
                r#"g.V().values("age").median()"#,
                r#"g.v().values("age").median()"#,
            ],
            assert: |res| assert_eq!(res[0].as_f64(), Some(30.5_f64)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn where_() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).values("age").where_t(__::median()).count().to_list(),
            rhai: [
                r#"g.V().values("age").where(__.median()).count()"#,
                r#"g.v().values("age").where(__.median()).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(4)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn where_inner() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).where_t(__::values("age").median()).count().to_list(),
            rhai: [
                r#"g.V().where(__.values("age").median()).count()"#,
                r#"g.v().where(__.values("age").median()).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(4)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn e() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e(None).values("weight").median().to_list(),
            rhai: [
                r#"g.E().values("weight").median()"#,
                r#"g.e().values("weight").median()"#,
            ],
            assert: |res| assert_eq!(res[0].as_f64(), Some(0.45_f64)),
        );
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.e(None).values("weight").median().profile().unwrap();
        assert!(result.to_string().contains("median("));
    }
}
