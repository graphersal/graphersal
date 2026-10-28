//! `count(Scope.local)`: the size of each traverser's own collection.

use crate::graph::GraphStorage;
use crate::traversal::step::local_scope::local_len;
use crate::value::Scope;
use crate::*;

/// `count(local)` (TinkerPop `CountLocalStep`): maps each traverser to the number of elements of
/// the collection it holds — the size of a list or map, the length of a path, `1` for any other
/// value. A per-traverser map step, never folded into a source step by `count_pushdown`.
#[derive(Default, Clone)]
struct StepCountLocal {}

impl fmt::Display for StepCountLocal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "count(Scope.local)")
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// `count(Scope)`: `Scope::Global` is [`count()`](Self::count) (the number of traversers in
    /// the stream); `Scope::Local` maps each traverser to the size of the collection it holds —
    /// a list's or map's size, a path's length, `1` for any other value
    /// (`g.V().fold().count(Scope.local)` → `6`).
    pub fn count_scoped(self, scope: Scope) -> Self {
        match scope {
            Scope::Global => self.count(),
            Scope::Local => {
                self.push_step(GraphStep::Map("count", Box::new(StepCountLocal::default())))
            }
        }
    }
}

impl __ {
    /// Anonymous `count(Scope)`; see [`GraphTraversalSource::count_scoped`].
    pub fn count_scoped<'g, S: GraphStorage + 'g>(scope: Scope) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().count_scoped(scope)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// `count(Scope)`; see [`GraphTraversalSource::count_scoped`].
    pub fn count_scoped(self, scope: Scope) -> Self {
        match scope {
            Scope::Global => self.count(),
            Scope::Local => {
                self.push_step(GraphStep::Map("count", Box::new(StepCountLocal::default())))
            }
        }
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepMap<'g, S> for StepCountLocal {
    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<Traverser<S>> {
        let length = local_len(context, &source.value)?;
        Ok(Traverser::with_parts(
            TraverserValue::Computed(ElementProperty::Number(length.into()), None),
            source.path,
            source.bulk,
            source.sack,
        ))
    }

    fn clone_box(&self) -> Box<dyn GraphStepMap<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "script")]
    #[test]
    fn fold() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).fold().count_scoped(Scope::Local).to_list(),
            rhai: [
                r#"g.V().fold().count(Scope.local)"#,
                r#"g.v().fold().count(Scope::local)"#,
            ],
            assert: |res| assert_eq!(res, vec![ElementProperty::Number(6.into())]),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn map() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).group().by(By::Label).count_scoped(Scope::Local).to_list(),
            rhai: [
                r#"g.V().group().by(T.label).count(Scope.local)"#,
                r#"g.v().group().by(T.label).count(Scope::Local)"#,
            ],
            assert: |res| assert_eq!(res, vec![ElementProperty::Number(2.into())]),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn scalar() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).count_scoped(Scope::Local).to_list(),
            rhai: [
                r#"g.V().count(Scope.local)"#,
                r#"g.v().count(Scope::local)"#,
            ],
            assert: |res| {
                assert_eq!(res.len(), 6);
                assert!(res.iter().all(|r| r.as_i64() == Some(1)));
            },
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn global_is_count() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).count_scoped(Scope::Global).to_list(),
            rhai: [
                r#"g.V().count(Scope.global)"#,
                r#"g.v().count(Scope::Global)"#,
            ],
            assert: |res| assert_eq!(res, vec![ElementProperty::Number(6.into())]),
        );
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g
            .v(None)
            .fold()
            .count_scoped(Scope::Local)
            .profile()
            .unwrap();
        assert!(result.to_string().contains("count(Scope.local)"));
    }
}
