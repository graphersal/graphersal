//! `sample(Scope.local, n)`: a random sample of the elements of each traverser's own list or
//! map. The builders live with `sample(n)` (`flatbarrier_steps/step_sample.rs`).

use rand::Rng;

use crate::graph::GraphStorage;
use crate::traversal::step::local_scope::{LocalShape, classify};
use crate::value::StreamMap;
use crate::*;

/// `sample(local, n)` (TinkerPop `SampleLocalStep`): maps a list to `n` of its elements and a
/// map to `n` of its entries, drawn at random without replacement and kept in the order they
/// were drawn; a list or map of at most `n` elements passes unchanged, and any other value
/// (a number, a vertex, a path, `null`) passes through.
#[derive(Clone)]
struct StepSampleLocal {
    amount: usize,
}

/// The `sample(local, n)` step, for the builders of `sample_scoped`.
pub(crate) fn sample_local_step<'g, S: GraphStorage + 'g>(amount: usize) -> GraphStep<'g, S> {
    GraphStep::Map("sample", Box::new(StepSampleLocal { amount }))
}

impl fmt::Display for StepSampleLocal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "sample(Scope.local, {})", self.amount)
    }
}

/// Draws `amount` of `items` without replacement, in draw order (TinkerPop removes a random
/// index from the remaining ones until it has enough).
fn draw<T, S: GraphStorage>(
    context: &mut TraverserContext<'_, S>,
    mut items: Vec<T>,
    amount: usize,
) -> Vec<T> {
    if items.len() <= amount {
        return items;
    }
    let mut drawn = Vec::with_capacity(amount);
    while drawn.len() < amount {
        let index = context.rng().gen_range(0..items.len());
        drawn.push(items.swap_remove(index));
    }
    drawn
}

impl<'g, S: GraphStorage + 'g> GraphStepMap<'g, S> for StepSampleLocal {
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<Traverser<S>> {
        let path = source.path;
        let bulk = source.bulk;
        let sack = source.sack;
        let value = match classify(context, source.value)? {
            LocalShape::List(items) => TraverserValue::Array(draw(context, items, self.amount)),
            LocalShape::Map(entries) => TraverserValue::Object(
                draw(context, entries, self.amount)
                    .into_iter()
                    .collect::<StreamMap<S>>(),
            ),
            LocalShape::Path(path) => TraverserValue::Path(path),
            LocalShape::Scalar(value) => value,
        };
        Ok(Traverser::with_parts(value, path, bulk, sack))
    }

    fn clone_box(&self) -> Box<dyn GraphStepMap<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::Scope;

    #[cfg(feature = "script")]
    #[test]
    fn a_scalar_passes_through() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").values("age").sample_scoped(Scope::Local, 5).to_list(),
            rhai: [r#"g.V("1").values("age").sample(Scope.local, 5)"#],
            assert: |res| assert_eq!(res, &[29i64.into()]),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn samples_a_list() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.with("random.seed", 9).v(None).values("name").fold().sample_scoped(Scope::Local, 2).to_list(),
            rhai: [r#"g.with("random.seed", 9).V().values("name").fold().sample(Scope.local, 2)"#],
            assert: |res| {
                let ElementProperty::Array(items) = &res[0] else { panic!("{res:?}") };
                assert_eq!(items.len(), 2);
                assert_ne!(items[0], items[1]);
            },
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn a_short_list_and_a_map() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).has_label("software").values("name").fold().sample_scoped(Scope::Local, 5).to_list(),
            rhai: [r#"g.V().hasLabel("software").values("name").fold().sample(Scope.local, 5)"#],
            assert: |res| assert_eq!(res, &[ElementProperty::Array(vec!["lop".into(), "ripple".into()])]),
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").value_map(None).sample_scoped(Scope::Local, 1).unfold().count().to_list(),
            rhai: [r#"g.V("1").valueMap().sample(Scope.local, 1).unfold().count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
    }

    #[test]
    fn seeded_is_reproducible() {
        let graph = GraphSource::tinkerpop_modern();
        let lock = graph.read();
        let run = || {
            lock.traversal()
                .with("random.seed", 5)
                .v(None)
                .values("name")
                .fold()
                .sample_scoped(Scope::Local, 3)
                .to_list()
                .unwrap()
        };
        let first = run();
        assert_eq!(run(), first);
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let lock = graph.read();
        let result = lock
            .traversal()
            .v(None)
            .values("age")
            .fold()
            .sample_scoped(Scope::Local, 2)
            .profile()
            .unwrap();
        assert!(
            result.to_string().contains("sample(Scope.local, 2)"),
            "{result}"
        );
    }
}
