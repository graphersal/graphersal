use crate::graph::GraphStorage;
use crate::*;
use rand::Rng;

#[derive(Default, Clone)]
struct StepCoin {
    coin: f32,
}

impl fmt::Display for StepCoin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "coin({})", self.coin)
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// `coin(p)`: keeps each traverser with probability `p` (0.0 drops all, 1.0 keeps all). The
    /// draws come from the execution's random number generator, so
    /// `g.with("random.seed", n)` makes the result reproducible.
    pub fn coin(self, coin: f32) -> Self {
        self.push_step(GraphStep::Filter("coin", Box::new(StepCoin { coin })))
    }
}

impl __ {
    /// Anonymous `coin(p)`; see [`GraphTraversalSource::coin`].
    pub fn coin<'g, S: GraphStorage + 'g>(coin: f32) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().coin(coin)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// `coin(p)`; see [`GraphTraversalSource::coin`].
    pub fn coin(self, coin: f32) -> Self {
        let step = GraphStep::Filter("coin", Box::new(StepCoin { coin }));
        self.push_step(step)
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepFilter<'g, S> for StepCoin {
    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        _source: &Traverser<S>,
    ) -> TraverserResult<bool> {
        let rnd: f32 = context.rng().r#gen();
        Ok(rnd < self.coin)
    }

    /// TinkerPop `CoinStep`: below 100 units one draw per unit of bulk, from there on
    /// `round(p * bulk)`; a result of 0 drops the traverser.
    fn surviving_bulk(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: &Traverser<S>,
    ) -> TraverserResult<u64> {
        let bulk = source.bulk;
        if bulk == 1 {
            let rnd: f32 = context.rng().r#gen();
            return Ok(u64::from(rnd < self.coin));
        }
        if bulk < 100 {
            let mut survivors = 0;
            for _ in 0..bulk {
                let rnd: f32 = context.rng().r#gen();
                if rnd < self.coin {
                    survivors += 1;
                }
            }
            return Ok(survivors);
        }
        let expected = (f64::from(self.coin) * bulk as f64).round();
        Ok(if expected <= 0.0 {
            0
        } else {
            (expected as u64).min(bulk)
        })
    }

    fn clone_box(&self) -> Box<dyn GraphStepFilter<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "script")]
    #[test]
    fn zero() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).coin(0.0).to_list(),
            rhai: [
                r#"g.V().coin(0.0)"#,
                r#"g.v().coin(0.0)"#,
            ],
            assert: |res| assert_eq!(res.len(), 0),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn where_() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).where_t(__::coin(0.0)).to_list(),
            rhai: [
                r#"g.V().where(__.coin(0.0))"#,
                r#"g.v().where(__.coin(0.0))"#,
            ],
            assert: |res| assert_eq!(res.len(), 0),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn where_inner() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).where_t(__::element().coin(1.0)).to_list(),
            rhai: [
                r#"g.V().where(__.element().coin(1.0))"#,
                r#"g.v().where(__.element().coin(1.0))"#,
            ],
            assert: |res| assert_eq!(res.len(), 6),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn one() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).coin(1.0).to_list(),
            rhai: [
                r#"g.V().coin(1.0)"#,
                r#"g.v().coin(1.0)"#,
            ],
            assert: |res| assert_eq!(res.len(), 6),
        );
    }

    /// With `random.seed` the same plan keeps the same traversers on every run.
    #[test]
    fn seeded_is_reproducible() {
        let graph = GraphSource::tinkerpop_modern();
        let lock = graph.read();
        let run = |seed: i64| {
            lock.traversal()
                .with("random.seed", seed)
                .v(None)
                .coin(0.5)
                .id()
                .to_list()
                .unwrap()
        };
        let first = run(42);
        for _ in 0..5 {
            assert_eq!(run(42), first);
        }
        // Different seeds draw differently for at least one of a few seeds.
        assert!((1..20).any(|seed| run(seed) != first));
    }

    #[cfg(feature = "script")]
    #[test]
    fn integer_probability_in_the_dsl() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).coin(1.0).to_list(),
            rhai: [r#"g.V().coin(1)"#, r#"g.v().coin(1)"#],
            assert: |res| assert_eq!(res.len(), 6),
        );
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.v(None).coin(1.0).profile().unwrap();
        assert!(result.to_string().contains("coin("));
    }
}
