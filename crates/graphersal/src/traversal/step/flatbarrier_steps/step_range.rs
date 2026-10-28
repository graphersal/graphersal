use crate::graph::GraphStorage;
use crate::*;

/// `range(low, high)`: TinkerPop's `[low, high)` stream slicing/pagination step.
///
/// `low` is the zero-based, inclusive index of the first traverser to emit; `high` is the
/// exclusive upper bound, or the sentinel `-1` meaning "no upper bound" (mirroring Apache
/// TinkerPop's own `RangeGlobalStep`, whose Java constructor treats `high == -1` as unbounded
/// and otherwise rejects only `high < low`). Both fields are stored as `i64` (rather than
/// `usize`, unlike `limit()`/`skip()`/`tail()`) specifically so an invalid negative `low`, or a
/// `high` below `-1`, can be detected and reported as a clear [`TraverserError::InvalidRange`]
/// at execution time instead of wrapping into a nonsensical `usize` via `as usize`.
#[derive(Default, Clone)]
struct StepRange {
    low: i64,
    /// `-1` means "unbounded" (no upper limit), matching TinkerPop's own sentinel.
    high: i64,
}

impl fmt::Display for StepRange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "range({}, {})", self.low, self.high)
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Slices the stream to `[low, high)`, skipping the first `low` traversers and emitting up
    /// to `high - low` after that. `high == -1` means no upper bound (emit every traverser from
    /// `low` onward), matching Apache TinkerPop's `range()`. `low` must be zero or positive;
    /// `high` must be `-1` or a value at least `low`, otherwise the traversal fails with
    /// [`TraverserError::InvalidRange`] when executed.
    ///
    /// ```
    /// use graphersal::{GraphTraversalSource, TraversalGraph};
    ///
    /// let graph = TraversalGraph::new();
    /// let g = GraphTraversalSource::new(&graph);
    /// // range(0, -1) emits everything, same as no range() at all.
    /// let _ = g.v(None).range(0, -1).to_list();
    /// ```
    pub fn range(self, low: i64, high: i64) -> Self {
        self.push_step(GraphStep::FlatBarrier(
            "range",
            Box::new(StepRange { low, high }),
        ))
    }
}

impl __ {
    /// See [`GraphTraversalSource::range`].
    pub fn range<'g, S: GraphStorage + 'g>(low: i64, high: i64) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().range(low, high)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// See [`GraphTraversalSource::range`].
    pub fn range(self, low: i64, high: i64) -> Self {
        let step = GraphStep::FlatBarrier("range", Box::new(StepRange { low, high }));
        self.push_step(step)
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepFlatBarrier<'g, S> for StepRange {
    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Vec<Traverser<S>>,
    ) -> TraverserResult<Option<Vec<Traverser<S>>>> {
        if self.low < 0 {
            return Err(Box::new(TraverserError::InvalidRange {
                low: self.low,
                high: self.high,
                reason: "low must be zero or a positive integer".to_string(),
            }));
        }
        if self.high < -1 {
            return Err(Box::new(TraverserError::InvalidRange {
                low: self.low,
                high: self.high,
                reason: "high must be -1 (unbounded) or zero or a positive integer".to_string(),
            }));
        }
        if self.high != -1 && self.high < self.low {
            return Err(Box::new(TraverserError::InvalidRange {
                low: self.low,
                high: self.high,
                reason: "high must be at least low".to_string(),
            }));
        }

        let low = self.low as usize;
        if context.bulk_active {
            // Counts units, not traversers: a merged traverser straddling a bound is cut.
            let take = if self.high == -1 {
                u64::MAX
            } else {
                (self.high - self.low) as u64
            };
            return Ok(Some(crate::traversal::bulk::slice_units(
                source,
                self.low as u64,
                take,
            )));
        }
        // Whole batch passes through unchanged: nothing to skip and nothing to trim. Mirrors
        // `limit()`'s own short-circuit for the equivalent no-op case.
        if low == 0 && (self.high == -1 || self.high as usize >= source.len()) {
            return Ok(Some(source));
        }

        if self.high == -1 {
            Ok(Some(source.into_iter().skip(low).collect()))
        } else {
            let take_n = (self.high as usize).saturating_sub(low);
            Ok(Some(source.into_iter().skip(low).take(take_n).collect()))
        }
    }

    fn clone_box(&self) -> Box<dyn GraphStepFlatBarrier<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "script")]
    #[test]
    fn first_three() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).range(0, 3).count().to_list(),
            rhai: [
                r#"g.V().range(0, 3).count()"#,
                r#"g.v().range(0, 3).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(3)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn skip_two_take_three() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).range(2, 5).count().to_list(),
            rhai: [
                r#"g.V().range(2, 5).count()"#,
                r#"g.v().range(2, 5).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(3)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn unbounded_high_from_low() {
        // tinkerpop_modern has 6 vertices; range(2, -1) should return every vertex from index 2
        // onward, i.e. 4 of them.
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).range(2, -1).count().to_list(),
            rhai: [
                r#"g.V().range(2, -1).count()"#,
                r#"g.v().range(2, -1).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(4)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn zero_and_unbounded_is_a_no_op() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).range(0, -1).count().to_list(),
            rhai: [
                r#"g.V().range(0, -1).count()"#,
                r#"g.v().range(0, -1).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(6)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn high_exceeds_stream_length() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).range(1, 60).count().to_list(),
            rhai: [
                r#"g.V().range(1, 60).count()"#,
                r#"g.v().range(1, 60).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(5)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn compare_elements() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).range(1, 3).to_list(),
            rhai: [
                r#"g.V().range(1, 3)"#,
                r#"g.v().range(1, 3)"#,
            ],
            assert: |res| assert_eq!(res.len(), 2),
        );
    }

    #[test]
    fn negative_low_errors() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.v(None).range(-2, 3).next();
        assert!(result.is_err());
    }

    #[test]
    fn high_below_negative_one_errors() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.v(None).range(0, -2).next();
        assert!(result.is_err());
    }

    #[test]
    fn high_less_than_low_errors() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.v(None).range(5, 2).next();
        assert!(result.is_err());
    }

    #[cfg(feature = "script")]
    #[test]
    fn order_by_then_range_paginates() {
        // Pagination: order().by("name") gives a stable order, range(2, 4) takes page 2 of
        // size 2. Combined coverage now that order() has landed in this working tree.
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).has_label("person").order().by("name").by(Order::Asc).range(2, 4).values("name").to_list(),
            rhai: [
                r#"g.V().hasLabel("person").order().by("name", Order.asc).range(2, 4).values("name")"#,
                r#"g.v().has_label("person").order().by("name").by(Order::Asc).range(2, 4).values("name")"#,
            ],
            assert: |res| assert_eq!(res.len(), 2),
        );
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.v(None).range(1, 3).profile().unwrap();
        assert!(result.to_string().contains("range("));
    }
}
