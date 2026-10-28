use crate::graph::GraphStorage;
use crate::*;

#[derive(Default, Clone)]
struct StepLimit {
    limit: usize,
    /// Appended by a bounded visualizer (`RenderOptions::max_rows`), not written by the user.
    /// It behaves exactly like a user-written `limit()`; only its name marks it, so a plan in a
    /// diagnostic never shows a step the user did not write without saying where it came from.
    display: bool,
}

impl fmt::Display for StepLimit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.display {
            write!(
                f,
                "limit({}){}",
                self.limit,
                crate::traversal::render::DISPLAY_MARK
            )
        } else {
            write!(f, "limit({})", self.limit)
        }
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Passes on only the first `limit` traversers.
    pub fn limit(self, limit: usize) -> Self {
        self.push_step(GraphStep::FlatBarrier(
            "limit",
            Box::new(StepLimit {
                limit,
                display: false,
            }),
        ))
    }

    /// Appends the internal `limit(n)` of a bounded visualizer, so execution stops producing
    /// results once `n` exist.
    pub(crate) fn display_limit(self, limit: usize) -> Self {
        self.push_step(GraphStep::FlatBarrier(
            "limit",
            Box::new(StepLimit {
                limit,
                display: true,
            }),
        ))
    }
}

impl __ {
    /// Passes on only the first `limit` traversers. Starts an anonymous traversal (`__.limit()`).
    pub fn limit<'g, S: GraphStorage + 'g>(limit: usize) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().limit(limit)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Passes on only the first `limit` traversers.
    pub fn limit(self, limit: usize) -> Self {
        let step = GraphStep::FlatBarrier(
            "limit",
            Box::new(StepLimit {
                limit,
                display: false,
            }),
        );
        self.push_step(step)
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepFlatBarrier<'g, S> for StepLimit {
    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Vec<Traverser<S>>,
    ) -> TraverserResult<Option<Vec<Traverser<S>>>> {
        if context.bulk_active {
            // Counts units, not traversers: a merged traverser straddling the limit is cut.
            return Ok(Some(crate::traversal::bulk::slice_units(
                source,
                0,
                self.limit as u64,
            )));
        }
        if source.len() <= self.limit {
            Ok(Some(source))
        } else {
            Ok(Some(source.into_iter().take(self.limit).collect()))
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
    fn v_partial() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).limit(2).count().to_list(),
            rhai: [
                r#"g.V().limit(2).count()"#,
                r#"g.v().limit(2).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(2)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_full() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).limit(6).count().to_list(),
            rhai: [
                r#"g.V().limit(6).count()"#,
                r#"g.v().limit(6).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(6)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_more() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).limit(60).count().to_list(),
            rhai: [
                r#"g.V().limit(60).count()"#,
                r#"g.v().limit(60).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(6)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_compare() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).limit(2).to_list(),
            rhai: [
                r#"g.V().limit(2)"#,
                r#"g.v().limit(2)"#,
            ],
            assert: |res| assert_eq!(res.len(), 2),
        );
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.v(None).limit(2).profile().unwrap();
        assert!(result.to_string().contains("limit("));
    }
}
