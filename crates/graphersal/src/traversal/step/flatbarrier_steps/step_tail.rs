use crate::graph::GraphStorage;
use crate::*;

#[derive(Default, Clone)]
struct StepTail {
    tail: usize,
}

impl fmt::Display for StepTail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "tail({})", self.tail)
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Passes on only the last `tail` traversers.
    pub fn tail(self, tail: usize) -> Self {
        self.push_step(GraphStep::FlatBarrier("tail", Box::new(StepTail { tail })))
    }
}

impl __ {
    /// Passes on only the last `tail` traversers. Starts an anonymous traversal (`__.tail()`).
    pub fn tail<'g, S: GraphStorage + 'g>(tail: usize) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().tail(tail)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Passes on only the last `tail` traversers.
    pub fn tail(self, tail: usize) -> Self {
        let step = GraphStep::FlatBarrier("tail", Box::new(StepTail { tail }));
        self.push_step(step)
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepFlatBarrier<'g, S> for StepTail {
    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        mut source: Vec<Traverser<S>>,
    ) -> TraverserResult<Option<Vec<Traverser<S>>>> {
        if context.bulk_active {
            // Counts units, not traversers: a merged traverser straddling the bound is cut.
            return Ok(Some(crate::traversal::bulk::tail_units(
                source,
                self.tail as u64,
            )));
        }
        if source.len() <= self.tail {
            Ok(Some(source))
        } else {
            let mut res: Vec<Traverser<S>> = vec![];
            for _ in 0..self.tail {
                res.push(source.pop().unwrap());
            }
            res.reverse();
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
    fn v_partial() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).tail(2).count().to_list(),
            rhai: [
                r#"g.V().tail(2).count()"#,
                r#"g.v().tail(2).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(2)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_full() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).tail(6).count().to_list(),
            rhai: [
                r#"g.V().tail(6).count()"#,
                r#"g.v().tail(6).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(6)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_more() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).tail(60).count().to_list(),
            rhai: [
                r#"g.V().tail(60).count()"#,
                r#"g.v().tail(60).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(6)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_compare() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).tail(2).to_list(),
            rhai: [
                r#"g.V().tail(2)"#,
                r#"g.v().tail(2)"#,
            ],
            assert: |res| assert_eq!(res.len(), 2),
        );
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.v(None).tail(2).profile().unwrap();
        assert!(result.to_string().contains("tail("));
    }
}
