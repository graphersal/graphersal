use crate::graph::GraphStorage;
use crate::*;

#[derive(Default, Clone)]
struct StepSkip {
    skip: usize,
}

impl fmt::Display for StepSkip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "skip({})", self.skip)
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Skips the first `skip` traversers.
    pub fn skip(self, skip: usize) -> Self {
        self.push_step(GraphStep::FlatBarrier("skip", Box::new(StepSkip { skip })))
    }
}

impl __ {
    /// Skips the first `skip` traversers. Starts an anonymous traversal (`__.skip()`).
    pub fn skip<'g, S: GraphStorage + 'g>(skip: usize) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().skip(skip)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Skips the first `skip` traversers.
    pub fn skip(self, skip: usize) -> Self {
        let step = GraphStep::FlatBarrier("skip", Box::new(StepSkip { skip }));
        self.push_step(step)
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepFlatBarrier<'g, S> for StepSkip {
    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Vec<Traverser<S>>,
    ) -> TraverserResult<Option<Vec<Traverser<S>>>> {
        if context.bulk_active {
            // Counts units, not traversers: a merged traverser straddling the bound is cut.
            let kept = crate::traversal::bulk::slice_units(source, self.skip as u64, u64::MAX);
            return Ok(if kept.is_empty() { None } else { Some(kept) });
        }
        if self.skip >= source.len() {
            Ok(None)
        } else {
            let mut res: Vec<Traverser<S>> = vec![];
            let mut count = 0;
            for t in source.into_iter() {
                if count < self.skip {
                    count += 1;
                    continue;
                }
                res.push(t);
            }
            Ok(Some(res))
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
    fn v_one() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).id().skip(5).to_list(),
            rhai: [
                r#"g.V().id().skip(5)"#,
                r#"g.v().id().skip(5)"#,
            ],
            assert: |res| assert_eq!(res.len(), 1),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_six() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).skip(6).count().to_list(),
            rhai: [
                r#"g.V().skip(6).count()"#,
                r#"g.v().skip(6).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(0)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_zero() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).skip(0).count().to_list(),
            rhai: [
                r#"g.V().skip(0).count()"#,
                r#"g.v().skip(0).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(6)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_four() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).id().skip(4).to_list(),
            rhai: [
                r#"g.V().id().skip(4)"#,
                r#"g.v().id().skip(4)"#,
            ],
            assert: |res| assert_eq!(res.len(), 2),
        );
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.v(None).id().skip(4).profile().unwrap();
        assert!(result.to_string().contains("skip("));
    }
}
