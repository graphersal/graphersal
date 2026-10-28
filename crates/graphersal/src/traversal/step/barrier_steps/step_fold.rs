use crate::graph::GraphStorage;
use crate::*;

#[derive(Default, Clone)]
struct StepFold {
    /// `fold(seed, Operator)`: the seed and the operator; `None` collects a list.
    reducer: Option<(ElementProperty, Operator)>,
}

impl fmt::Display for StepFold {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.reducer {
            None => write!(f, "fold()"),
            Some((seed, operator)) => {
                write!(
                    f,
                    "fold({}, Operator.{})",
                    crate::value::literal::rhai_literal(seed),
                    operator.name()
                )
            }
        }
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Collects the whole stream into one list.
    pub fn fold(self) -> Self {
        self.push_step(GraphStep::Barrier("fold", Box::new(StepFold::default())))
    }

    /// `fold(seed, Operator)`: applies `operator(accumulator, value)` to every value of the stream
    /// in order, starting from `seed`, and emits the accumulator as one traverser. An empty stream
    /// gives the seed. A traverser of bulk `n` applies the operator `n` times (within
    /// `bulk.max_expansion`), so `minus` and `div` stay exact.
    ///
    /// Deviations from TinkerPop (book page "Sack and Operators"): integer overflow is an
    /// `OperatorFailed` error (TinkerPop promotes to `long`/`BigInteger`); only an `Operator` is
    /// accepted, there is no `fold(seed, BiFunction)`.
    pub fn fold_with<T: Into<ElementProperty>>(self, seed: T, operator: Operator) -> Self {
        self.push_step(GraphStep::Barrier(
            "fold",
            Box::new(StepFold {
                reducer: Some((seed.into(), operator)),
            }),
        ))
    }
}

impl __ {
    /// Collects the whole stream into one list. Starts an anonymous traversal (`__.fold()`).
    pub fn fold<'g, S: GraphStorage + 'g>() -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().fold()
    }

    /// `__.fold(seed, Operator)`, see `GraphTraversalSource::fold_with`.
    pub fn fold_with<'g, S: GraphStorage + 'g, T: Into<ElementProperty>>(
        seed: T,
        operator: Operator,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().fold_with(seed, operator)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Collects the whole stream into one list.
    pub fn fold(self) -> Self {
        let step = GraphStep::Barrier("fold", Box::new(StepFold::default()));
        self.push_step(step)
    }

    /// `fold(seed, Operator)`, see `GraphTraversalSource::fold_with`.
    pub fn fold_with<T: Into<ElementProperty>>(self, seed: T, operator: Operator) -> Self {
        let step = GraphStep::Barrier(
            "fold",
            Box::new(StepFold {
                reducer: Some((seed.into(), operator)),
            }),
        );
        self.push_step(step)
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepBarrier<'g, S> for StepFold {
    /// Builds an aggregation/materialization buffer (D3 of the memory-inspection spec) — reported
    /// to `.profile()` as a non-zero `bytes_materialized`.
    fn materializes(&self) -> bool {
        true
    }

    /// TinkerPop's `fold()` is a seeded reducing barrier: an empty stream folds to `[]`, not
    /// nothing (confirmed against TinkerPop 3.7's `FoldStep`, whose seed is an empty list).
    fn always_emits(&self) -> bool {
        true
    }

    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Vec<Traverser<S>>,
    ) -> TraverserResult<Option<Traverser<S>>> {
        // A merged traverser folds as many times as it stands for, within `bulk.max_expansion`,
        // `traversal.max_materialized_bytes` and the `evaluationTimeout` (checked while copying).
        let tmp = crate::traversal::bulk::expand_values_in(context, source, "fold")?;
        let Some((seed, operator)) = &self.reducer else {
            return Ok(Some(Traverser::start(context, TraverserValue::Array(tmp))));
        };
        let mut acc = seed.clone();
        for value in tmp {
            crate::traversal::context::tick_deadline(&mut context.deadline)?;
            let storage: &S = context.storage;
            acc = operator.apply_owned(acc, value.into_node_property(storage)?)?;
        }
        Ok(Some(Traverser::start(context, TraverserValue::from(acc))))
    }

    fn clone_box(&self) -> Box<dyn GraphStepBarrier<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// TinkerPop 3.7's `fold()` is a seeded reducing barrier (`FoldStep`, seed: an empty list):
    /// an empty input stream still folds to `[]`, not nothing (confirmed via `graphersal`:
    /// `g.V().hasLabel("nope").fold()` → `[]`). A fused `count()` must then read `1`, matching
    /// `.to_list().len()`.
    #[cfg(feature = "script")]
    #[test]
    fn empty_input_folds_to_an_empty_list() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).has_label("nope").fold().to_list(),
            rhai: [
                r#"g.V().hasLabel("nope").fold()"#,
                r#"g.v().has_label("nope").fold()"#,
            ],
            assert: |res| {
                assert_eq!(res.len(), 1);
                match &res[0] {
                    ElementProperty::Array(items) => assert!(items.is_empty()),
                    other => panic!("expected an empty array, got {other:?}"),
                }
            },
        );
    }

    /// The fused `count()` on an empty-seeded `fold()` must equal `.to_list().len()` (`1`), not
    /// `0` — the same consistency the count-pushdown rules keep for `group()`/`tree()`.
    #[cfg(feature = "script")]
    #[test]
    fn empty_input_fold_count_is_one() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).has_label("nope").fold().count().to_list(),
            rhai: [
                r#"g.V().hasLabel("nope").fold().count()"#,
                r#"g.v().has_label("nope").fold().count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).fold().count().to_list(),
            rhai: [
                r#"g.V().fold().count()"#,
                r#"g.v().fold().count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_id() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).id().fold().to_list(),
            rhai: [
                r#"g.V().id().fold()"#,
                r#"g.v().id().fold()"#,
            ],
            assert: |res| {
                match &res[0] {
                    ElementProperty::Array(array) => {
                        assert_eq!(array[0], ElementProperty::String("1".into()));
                        assert_eq!(array[1], ElementProperty::String("2".into()));
                        assert_eq!(array[2], ElementProperty::String("3".into()));
                        assert_eq!(array[3], ElementProperty::String("4".into()));
                        assert_eq!(array[4], ElementProperty::String("5".into()));
                        assert_eq!(array[5], ElementProperty::String("6".into()));
                    },
                    _ => panic!(),
                }
            },
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn e() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e(None).fold().count().to_list(),
            rhai: [
                r#"g.E().fold().count()"#,
                r#"g.e().fold().count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn e_values() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e(None).values("weight").fold().to_list(),
            rhai: [
                r#"g.E().values("weight").fold()"#,
                r#"g.e().values("weight").fold()"#,
            ],
            assert: |res| {
                match &res[0] {
                    ElementProperty::Array(array) => {
                        assert_eq!(array[0].as_f64(), Some(0.5));
                        assert_eq!(array[1].as_f64(), Some(0.4));
                        assert_eq!(array[2].as_f64(), Some(1.0));
                        assert_eq!(array[3].as_f64(), Some(0.4));
                        assert_eq!(array[4].as_f64(), Some(1.0));
                        assert_eq!(array[5].as_f64(), Some(0.2));
                    },
                    _ => panic!(),
                }
            },
        );
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.e(None).fold().count().profile().unwrap();
        assert!(result.to_string().contains("fold("));
    }
}
