//! `sum/min/max/mean(Scope.local)`: reduce each traverser's own list of numbers (`min`/`max`
//! also of strings, booleans or UUIDs).

use crate::graph::GraphStorage;
use crate::traversal::step::barrier_steps::numeric::{
    Extremum, ExtremumAccumulator, MeanAccumulator, StreamNumber, ValueSource, WideningNumeric,
    number_of, push_extremum_value, resolve_value_number,
};
use crate::traversal::step::local_scope::{LocalShape, classify, stored_value};
use crate::value::Scope;
use crate::*;

/// Which reduction a [`StepReduceLocal`] applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReduceKind {
    Sum,
    Min,
    Max,
    Mean,
}

impl ReduceKind {
    /// The Gremlin step name, used in the profile name and in diagnostics.
    fn name(self) -> &'static str {
        match self {
            ReduceKind::Sum => "sum",
            ReduceKind::Min => "min",
            ReduceKind::Max => "max",
            ReduceKind::Mean => "mean",
        }
    }
}

/// `sum(local)`/`min(local)`/`max(local)`/`mean(local)` (TinkerPop `SumLocalStep`, ...): maps
/// each traverser holding a list to the reduction of its elements, with exactly the numeric
/// domains and diagnostics of the global `sum()`/`min()`/`max()`/`mean()` (shared through
/// `barrier_steps/numeric.rs`); `min`/`max` also compare strings, booleans and UUIDs). `null` elements are skipped; an
/// empty or all-null list drops the traverser. Any non-collection value is reduced as a
/// one-element list (`g.V("1").values("age").sum(Scope.local)` → `29`, `mean` → `29.0`).
#[derive(Clone)]
struct StepReduceLocal {
    kind: ReduceKind,
}

impl fmt::Display for StepReduceLocal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}(Scope.local)", self.kind.name())
    }
}

/// The running reduction of one traverser's list.
enum Reducer {
    Sum(WideningNumeric),
    Min(ExtremumAccumulator),
    Max(ExtremumAccumulator),
    Mean(MeanAccumulator),
}

impl Reducer {
    fn new(kind: ReduceKind) -> Self {
        match kind {
            ReduceKind::Sum => Reducer::Sum(WideningNumeric::new()),
            ReduceKind::Min => Reducer::Min(ExtremumAccumulator::new(Extremum::Min)),
            ReduceKind::Max => Reducer::Max(ExtremumAccumulator::new(Extremum::Max)),
            ReduceKind::Mean => Reducer::Mean(MeanAccumulator::new()),
        }
    }

    /// Folds one number into a `sum` or `mean`; `min`/`max` take whole property values instead.
    fn push_number(&mut self, number: StreamNumber) -> TraverserResult<()> {
        match self {
            Reducer::Sum(accumulator) => {
                accumulator.push("sum", number, i64::checked_add, |a, b| a + b)
            }
            Reducer::Mean(mean) => {
                mean.push(number);
                Ok(())
            }
            Reducer::Min(_) | Reducer::Max(_) => Ok(()),
        }
    }

    /// Folds one list element (a lazy handle or a computed value); nulls and absent properties
    /// are skipped.
    fn push_value<S: GraphStorage>(
        &mut self,
        context: &TraverserContext<'_, S>,
        value: TraverserValue<S>,
    ) -> TraverserResult<()> {
        match self {
            Reducer::Min(accumulator) | Reducer::Max(accumulator) => {
                push_extremum_value(accumulator, context, value)
            }
            Reducer::Sum(_) | Reducer::Mean(_) => match resolve_value_number(context, value)? {
                Some((number, _source)) => self.push_number(number),
                None => Ok(()),
            },
        }
    }

    /// Folds the elements of a materialized list, borrowed from the graph or the traverser.
    fn push_properties<S: GraphStorage>(
        &mut self,
        context: &TraverserContext<'_, S>,
        items: &[ElementProperty],
        source: &ValueSource<S>,
    ) -> TraverserResult<()> {
        for item in items {
            match self {
                Reducer::Min(accumulator) | Reducer::Max(accumulator) => {
                    accumulator.push_property(item, context, source)?
                }
                Reducer::Sum(_) | Reducer::Mean(_) => {
                    if let Some(number) = number_of(context, item, source)? {
                        self.push_number(number)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn finish(self) -> Option<ElementProperty> {
        match self {
            Reducer::Sum(accumulator) => accumulator.finish(),
            Reducer::Min(accumulator) | Reducer::Max(accumulator) => accumulator.finish(),
            Reducer::Mean(mean) => mean.finish(),
        }
    }
}

impl StepReduceLocal {
    /// Reduces one traverser's value, or `None` when it holds no number at all.
    fn reduce<S: GraphStorage>(
        &self,
        context: &TraverserContext<'_, S>,
        value: TraverserValue<S>,
    ) -> TraverserResult<Option<ElementProperty>> {
        let mut reducer = Reducer::new(self.kind);
        // A stored list property is read in place, without cloning it.
        if let Some(ElementProperty::Array(items)) = stored_value(context, &value)? {
            let source = match &value {
                TraverserValue::VertexProperty(property, node, _) => {
                    ValueSource::Vertex(*node, Some(property.clone()))
                }
                TraverserValue::EdgeProperty(property, edge, _) => {
                    ValueSource::Edge(*edge, Some(property.clone()))
                }
                TraverserValue::Computed(_, _)
                | TraverserValue::Vertex(_)
                | TraverserValue::Edge(_)
                | TraverserValue::Path(_)
                | TraverserValue::Record { .. }
                | TraverserValue::Object(_)
                | TraverserValue::PathProperty(_)
                | TraverserValue::Entry(_)
                | TraverserValue::Array(_)
                | TraverserValue::Schema(_)
                | TraverserValue::Graph(_) => ValueSource::Anonymous,
            };
            reducer.push_properties(context, items, &source)?;
            return Ok(reducer.finish());
        }
        if let TraverserValue::Computed(ElementProperty::Array(items), node) = &value {
            let source = match node {
                Some(node) => ValueSource::Vertex(*node, None),
                None => ValueSource::Anonymous,
            };
            reducer.push_properties(context, items, &source)?;
            return Ok(reducer.finish());
        }
        match classify(context, value)? {
            LocalShape::List(items) => {
                for item in items {
                    reducer.push_value(context, item)?;
                }
            }
            LocalShape::Path(path) => {
                for step in path.steps {
                    reducer.push_value(context, step.value)?;
                }
            }
            LocalShape::Scalar(value) => reducer.push_value(context, value)?,
            LocalShape::Map(_) => {
                return Err(Box::new(TraverserError::cast_exception_str(
                    &["list", "number"],
                    "map".to_string(),
                )));
            }
        }
        Ok(reducer.finish())
    }

    fn step<'g, S: GraphStorage + 'g>(kind: ReduceKind) -> GraphStep<'g, S> {
        GraphStep::Map(kind.name(), Box::new(StepReduceLocal { kind }))
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// `sum(Scope)`: `Scope::Global` is [`sum()`](Self::sum); `Scope::Local` sums the list each
    /// traverser holds (`g.V().values("age").fold().sum(Scope.local)` → `123`), skipping nulls
    /// and dropping a traverser whose list holds no number. A non-list value is summed as a
    /// one-element list.
    pub fn sum_scoped(self, scope: Scope) -> Self {
        match scope {
            Scope::Global => self.sum(),
            Scope::Local => self.push_step(StepReduceLocal::step(ReduceKind::Sum)),
        }
    }

    /// `min(Scope)`: `Scope::Global` is [`min()`](Self::min); `Scope::Local` is the smallest
    /// number of the list each traverser holds; see [`sum_scoped`](Self::sum_scoped).
    pub fn min_scoped(self, scope: Scope) -> Self {
        match scope {
            Scope::Global => self.min(),
            Scope::Local => self.push_step(StepReduceLocal::step(ReduceKind::Min)),
        }
    }

    /// `max(Scope)`: `Scope::Global` is [`max()`](Self::max); `Scope::Local` is the largest
    /// number of the list each traverser holds; see [`sum_scoped`](Self::sum_scoped).
    pub fn max_scoped(self, scope: Scope) -> Self {
        match scope {
            Scope::Global => self.max(),
            Scope::Local => self.push_step(StepReduceLocal::step(ReduceKind::Max)),
        }
    }

    /// `mean(Scope)`: `Scope::Global` is [`mean()`](Self::mean); `Scope::Local` is the mean (a
    /// double) of the list each traverser holds; see [`sum_scoped`](Self::sum_scoped).
    pub fn mean_scoped(self, scope: Scope) -> Self {
        match scope {
            Scope::Global => self.mean(),
            Scope::Local => self.push_step(StepReduceLocal::step(ReduceKind::Mean)),
        }
    }
}

impl __ {
    /// Anonymous `sum(Scope)`; see [`GraphTraversalSource::sum_scoped`].
    pub fn sum_scoped<'g, S: GraphStorage + 'g>(scope: Scope) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().sum_scoped(scope)
    }

    /// Anonymous `min(Scope)`; see [`GraphTraversalSource::min_scoped`].
    pub fn min_scoped<'g, S: GraphStorage + 'g>(scope: Scope) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().min_scoped(scope)
    }

    /// Anonymous `max(Scope)`; see [`GraphTraversalSource::max_scoped`].
    pub fn max_scoped<'g, S: GraphStorage + 'g>(scope: Scope) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().max_scoped(scope)
    }

    /// Anonymous `mean(Scope)`; see [`GraphTraversalSource::mean_scoped`].
    pub fn mean_scoped<'g, S: GraphStorage + 'g>(scope: Scope) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().mean_scoped(scope)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// `sum(Scope)`; see [`GraphTraversalSource::sum_scoped`].
    pub fn sum_scoped(self, scope: Scope) -> Self {
        match scope {
            Scope::Global => self.sum(),
            Scope::Local => self.push_step(StepReduceLocal::step(ReduceKind::Sum)),
        }
    }

    /// `min(Scope)`; see [`GraphTraversalSource::min_scoped`].
    pub fn min_scoped(self, scope: Scope) -> Self {
        match scope {
            Scope::Global => self.min(),
            Scope::Local => self.push_step(StepReduceLocal::step(ReduceKind::Min)),
        }
    }

    /// `max(Scope)`; see [`GraphTraversalSource::max_scoped`].
    pub fn max_scoped(self, scope: Scope) -> Self {
        match scope {
            Scope::Global => self.max(),
            Scope::Local => self.push_step(StepReduceLocal::step(ReduceKind::Max)),
        }
    }

    /// `mean(Scope)`; see [`GraphTraversalSource::mean_scoped`].
    pub fn mean_scoped(self, scope: Scope) -> Self {
        match scope {
            Scope::Global => self.mean(),
            Scope::Local => self.push_step(StepReduceLocal::step(ReduceKind::Mean)),
        }
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepMap<'g, S> for StepReduceLocal {
    /// Never called by the executor: `may_filter()` is true, so it calls
    /// [`process_filter`](Self::process_filter). Delegates to it; a dropped traverser (an empty
    /// or all-null list) is an error here, since this signature cannot drop one.
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<Traverser<S>> {
        self.process_filter(context, source)?.ok_or_else(|| {
            Box::new(TraverserError::QueryExecution {
                query: self.to_string(),
                reason: format!(
                    "{}(local) dropped a traverser in a context that cannot filter",
                    self.kind.name()
                ),
            })
        })
    }

    fn process_filter(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<Option<Traverser<S>>> {
        let path = source.path;
        let bulk = source.bulk;
        let sack = source.sack;
        Ok(self.reduce(context, source.value)?.map(|value| {
            Traverser::with_parts(TraverserValue::Computed(value, None), path, bulk, sack)
        }))
    }

    /// An empty (or all-null) list drops the traverser, so a fused `count()` must run this step.
    fn may_filter(&self) -> bool {
        true
    }

    fn clone_box(&self) -> Box<dyn GraphStepMap<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sum_fold() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).values("age").fold().sum_scoped(Scope::Local).to_list(),
            rhai: [
                r#"g.V().values("age").fold().sum(Scope.local)"#,
                r#"g.v().values("age").fold().sum(Scope::local)"#,
            ],
            assert: |res| assert_eq!(res, vec![ElementProperty::Number(123.into())]),
        );
    }

    #[test]
    fn sum_empty_list_emits_nothing() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).values("foo").fold().sum_scoped(Scope::Local).to_list(),
            rhai: [
                r#"g.V().values("foo").fold().sum(Scope.local)"#,
                r#"g.v().values("foo").fold().sum(Scope::local)"#,
            ],
            assert: |res| assert!(res.is_empty()),
        );
    }

    #[test]
    fn min_max_fold() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).values("age").fold().min_scoped(Scope::Local).to_list(),
            rhai: [
                r#"g.V().values("age").fold().min(Scope.local)"#,
                r#"g.v().values("age").fold().min(Scope::local)"#,
            ],
            assert: |res| assert_eq!(res, vec![ElementProperty::Number(27.into())]),
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).values("age").fold().max_scoped(Scope::Local).to_list(),
            rhai: [
                r#"g.V().values("age").fold().max(Scope.local)"#,
                r#"g.v().values("age").fold().max(Scope::local)"#,
            ],
            assert: |res| assert_eq!(res, vec![ElementProperty::Number(35.into())]),
        );
    }

    #[test]
    fn mean_scalar_is_double() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").values("age").mean_scoped(Scope::Local).to_list(),
            rhai: [
                r#"g.V("1").values("age").mean(Scope.local)"#,
                r#"g.v("1").values("age").mean(Scope::local)"#,
            ],
            assert: |res| assert_eq!(res, vec![ElementProperty::Number(29.0.into())]),
        );
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g
            .v(None)
            .values("age")
            .fold()
            .sum_scoped(Scope::Local)
            .profile()
            .unwrap();
        assert!(result.to_string().contains("sum(Scope.local)"));
    }

    /// The modern graph with marko's `tags = [4, 2, 6]` and edge 0's `ws = [0.5, 1.5]`.
    fn listed() -> Graph {
        let graph = GraphSource::tinkerpop_modern();
        {
            let mut lock = graph.write();
            lock.traversal_mut()
                .v("1")
                .property("tags", vec![4, 2, 6])
                .next()
                .unwrap();
            lock.traversal_mut()
                .e("0")
                .property("ws", vec![0.5, 1.5])
                .next()
                .unwrap();
        }
        graph
    }

    #[test]
    fn stored_list_properties_reduce_in_place() {
        for (kind, expected) in [
            ("sum", ElementProperty::from(12)),
            ("min", 2.into()),
            ("max", 6.into()),
            ("mean", 4.0.into()),
        ] {
            let rust = |g: GraphTraversalSource<'_, &TraversalGraph>| {
                let t = g.v("1").values("tags");
                match kind {
                    "sum" => t.sum_scoped(Scope::Local),
                    "min" => t.min_scoped(Scope::Local),
                    "max" => t.max_scoped(Scope::Local),
                    _ => t.mean_scoped(Scope::Local),
                }
                .to_list()
            };
            let camel = format!(r#"g.V("1").values("tags").{kind}(Scope.local)"#);
            let snake = format!(r#"g.v("1").values("tags").{kind}(Scope::local)"#);
            test_dual!(
                graph: listed(),
                rust: rust,
                rhai: [camel.as_str(), snake.as_str()],
                assert: |res| assert_eq!(res, std::slice::from_ref(&expected)),
            );
        }
        // an edge's list property, read through its value and its property handle
        test_dual!(
            graph: listed(),
            rust: |g| g.e("0").values("ws").sum_scoped(Scope::Local).to_list(),
            rhai: [r#"g.E("0").values("ws").sum(Scope.local)"#],
            assert: |res| assert_eq!(res, &[2.0.into()]),
        );
        test_dual!(
            graph: listed(),
            rust: |g| g.e("0").properties("ws").max_scoped(Scope::Local).to_list(),
            rhai: [r#"g.E("0").properties("ws").max(Scope.local)"#],
            assert: |res| assert_eq!(res, &[1.5.into()]),
        );
    }

    #[test]
    fn a_scalar_reduces_to_itself_and_a_child_traversal_reduces_its_list() {
        test_dual!(
            graph: listed(),
            rust: |g| g.v("1").values("age").sum_scoped(Scope::Local).to_list(),
            rhai: [r#"g.V("1").values("age").sum(Scope.local)"#],
            assert: |res| assert_eq!(res, &[29.into()]),
        );
        test_dual!(
            graph: listed(),
            rust: |g| g
                .v(None)
                .values("age")
                .fold()
                .local(__::mean_scoped(Scope::Local))
                .to_list(),
            rhai: [r#"g.V().values("age").fold().local(__.mean(Scope.local))"#],
            assert: |res| assert_eq!(res, &[30.75.into()]),
        );
        test_dual!(
            graph: listed(),
            rust: |g| g
                .v("1")
                .project(vec!["s", "lo", "hi"])
                .by(__::values("tags").sum_scoped(Scope::Local))
                .by(__::values("tags").min_scoped(Scope::Local))
                .by(__::values("tags").max_scoped(Scope::Local))
                .to_list(),
            rhai: [r#"g.V("1").project("s", "lo", "hi").by(__.values("tags").sum(Scope.local)).by(__.values("tags").min(Scope.local)).by(__.values("tags").max(Scope.local))"#],
            assert: |res| {
                let m = res[0].as_object().unwrap();
                assert_eq!(m.get("s"), Some(&12.into()));
                assert_eq!(m.get("lo"), Some(&2.into()));
                assert_eq!(m.get("hi"), Some(&6.into()));
            },
        );
    }

    #[test]
    fn an_empty_list_drops_the_traverser_and_a_path_with_an_element_fails() {
        test_dual!(
            graph: listed(),
            rust: |g| g
                .v(None)
                .values("nope")
                .fold()
                .sum_scoped(Scope::Local)
                .count()
                .to_list(),
            rhai: [r#"g.V().values("nope").fold().sum(Scope.local).count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(0)),
        );
        // a path is iterated: here it holds a vertex, which is no number
        test_dual_err!(
            graph: listed(),
            rust: |g| g.v("1").values("age").path().sum_scoped(Scope::Local).to_list(),
            rhai: [r#"g.V("1").values("age").path().sum(Scope.local)"#],
            assert: |err| assert!(err.contains("got type 'vertex'"), "{err}"),
        );
        // a list holding a list cannot be ordered among numbers
        test_dual_err!(
            graph: listed(),
            rust: |g| g.v("1").values("tags").fold().max_scoped(Scope::Local).to_list(),
            rhai: [r#"g.V("1").values("tags").fold().max(Scope.local)"#],
            assert: |err| assert!(err.contains("cannot order a array<int64> value"), "{err}"),
        );
    }
}
