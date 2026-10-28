use std::fmt::Debug;

use crate::graph::GraphStorage;
use crate::*;

#[derive(Clone)]
struct StepAny<B: Into<PValue> + Debug + Clone + Send + Sync> {
    predicate: P<B>,
}

impl<B: Into<PValue> + Debug + Clone + Send + Sync> fmt::Display for StepAny<B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "any({})",
            crate::value::predicate::render_predicate(&self.predicate)
        )
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// `any(P)`: keeps a list traverser (`fold()`, an injected list, a stored list property)
    /// whose at least one element satisfies the predicate; a non-list value is filtered out.
    pub fn any<B: Into<PValue> + Debug + Clone + Send + Sync + 'static, T: Into<P<B>> + Debug>(
        self,
        predicate: T,
    ) -> Self {
        self.push_step(GraphStep::Filter(
            "any",
            Box::new(StepAny {
                predicate: predicate.into(),
            }),
        ))
    }
}

impl __ {
    /// `any(P)`: keeps a list traverser (`fold()`, an injected list, a stored list property)
    /// whose at least one element satisfies the predicate; a non-list value is filtered out.
    pub fn any<
        'g,
        S: GraphStorage + 'g,
        B: Into<PValue> + std::fmt::Debug + Clone + Send + Sync + 'static,
        T: Into<P<B>> + Debug,
    >(
        predicate: T,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().any(predicate)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// `any(P)`: keeps a list traverser (`fold()`, an injected list, a stored list property)
    /// whose at least one element satisfies the predicate; a non-list value is filtered out.
    pub fn any<B: Into<PValue> + Debug + Clone + Send + Sync + 'static, T: Into<P<B>> + Debug>(
        self,
        predicate: T,
    ) -> Self {
        let step = GraphStep::Filter(
            "any",
            Box::new(StepAny {
                predicate: predicate.into(),
            }),
        );
        self.push_step(step)
    }
}

impl<'g, S: GraphStorage + 'g, B: Into<PValue> + Debug + Clone + Send + Sync + 'static>
    GraphStepFilter<'g, S> for StepAny<B>
{
    /// TinkerPop's `any(P)`: keeps a list traverser when at least one element satisfies the
    /// predicate. A non-list value (a scalar, `null`, a map) never passes.
    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: &Traverser<S>,
    ) -> TraverserResult<bool> {
        let Some(items) = list_items(context, &source.value)? else {
            return Ok(false);
        };
        for item in &items {
            if self.predicate.test(context, item)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn clone_box(&self) -> Box<dyn GraphStepFilter<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

/// The elements of `value` when it is a list (`fold()`, an injected or constant list, a stored
/// list property); `None` for any other value, which `any()`/`all()` filter out.
pub(crate) fn list_items<S: GraphStorage>(
    context: &TraverserContext<'_, S>,
    value: &TraverserValue<S>,
) -> TraverserResult<Option<Vec<TraverserValue<S>>>> {
    let is_list = match value {
        TraverserValue::Array(_) | TraverserValue::Computed(ElementProperty::Array(_), _) => true,
        TraverserValue::VertexProperty(_, _, _) | TraverserValue::EdgeProperty(_, _, _) => {
            matches!(
                crate::traversal::step::local_scope::stored_value(context, value)?,
                Some(ElementProperty::Array(_))
            )
        }
        TraverserValue::Computed(_, _)
        | TraverserValue::Vertex(_)
        | TraverserValue::Edge(_)
        | TraverserValue::Path(_)
        | TraverserValue::Record { .. }
        | TraverserValue::Object(_)
        | TraverserValue::PathProperty(_)
        | TraverserValue::Entry(_)
        | TraverserValue::Schema(_)
        | TraverserValue::Graph(_) => false,
    };
    if !is_list {
        return Ok(None);
    }
    match crate::traversal::step::local_scope::classify(context, value.clone())? {
        crate::traversal::step::local_scope::LocalShape::List(items) => Ok(Some(items)),
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "script")]
    #[test]
    fn v_simple() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).values("age").fold().any(27).count().to_list(),
            rhai: [
                r#"g.V().values("age").fold().any(27).count()"#,
                r#"g.v().values("age").fold().any(27).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_simple_gte() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).values("age").fold().any(P::Gte(30)).count().to_list(),
            rhai: [
                r#"g.V().values("age").fold().any(P::Gte(30)).count()"#,
                r#"g.v().values("age").fold().any(P::Gte(30)).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_simple_wrong() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).values("age").fold().any(10).count().to_list(),
            rhai: [
                r#"g.V().values("age").fold().any(10).count()"#,
                r#"g.v().values("age").fold().any(10).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(0)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn e_simple() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e(None).values("weight").fold().any(0.4).count().to_list(),
            rhai: [
                r#"g.E().values("weight").fold().any(0.4).count()"#,
                r#"g.e().values("weight").fold().any(0.4).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g
            .e(None)
            .values("weight")
            .fold()
            .any(0.4)
            .count()
            .profile()
            .unwrap();
        assert!(result.to_string().contains("any("));
    }

    /// A non-list value never passes `any()` (TinkerPop), not even a matching scalar or `null`.
    #[cfg(feature = "script")]
    #[test]
    fn scalar_and_null_are_filtered_out() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).values("age").any(P::Gt(0)).count().to_list(),
            rhai: [
                r#"g.V().values("age").any(P.gt(0)).count()"#,
                r#"g.inject((), 1).any(P.eq(())).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(0)),
        );
    }
}
