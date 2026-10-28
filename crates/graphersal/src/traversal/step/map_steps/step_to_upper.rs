use crate::graph::GraphStorage;
use crate::traversal::step::local_scope::{LocalMapped, bounded_string, map_local_strings};
use crate::value::Scope;
use crate::*;

/// `to_upper()` (TinkerPop `toUpper()`): upper-cases a string. With `Scope::Local` it
/// transforms each string of the list a traverser holds instead (a `null` element stays `null`).
#[derive(Default, Clone)]
struct StepToUpper {
    scope: Scope,
}

impl fmt::Display for StepToUpper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.scope {
            Scope::Global => write!(f, "to_upper()"),
            Scope::Local => write!(f, "to_upper(Scope.local)"),
        }
    }
}

/// The per-string transformation, shared by the single-value and the per-element path.
fn transform(text: &str) -> ElementProperty {
    ElementProperty::String(text.to_uppercase().into())
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Upper-cases a string; fails on any other value.
    pub fn to_upper(self) -> Self {
        self.to_upper_scoped(Scope::Global)
    }

    /// `to_upper(Scope)`: `Scope::Global` is [`to_upper()`](Self::to_upper); `Scope::Local` transforms each
    /// string of the list a traverser holds (`g.V().values("name").fold().to_upper(Scope.local)` → `[MARKO, VADAS, ...]`); a `null` element or input stays `null`, a
    /// single string is transformed as by the unscoped step.
    pub fn to_upper_scoped(self, scope: Scope) -> Self {
        self.push_step(GraphStep::Map("to_upper", Box::new(StepToUpper { scope })))
    }
}

impl __ {
    /// Anonymous `to_upper()`.
    pub fn to_upper<'g, S: GraphStorage + 'g>() -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().to_upper()
    }

    /// Anonymous `to_upper(Scope)`; see [`GraphTraversalSource::to_upper_scoped`].
    pub fn to_upper_scoped<'g, S: GraphStorage + 'g>(scope: Scope) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().to_upper_scoped(scope)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Upper-cases a string; see [`GraphTraversalSource::to_upper`].
    pub fn to_upper(self) -> Self {
        self.to_upper_scoped(Scope::Global)
    }

    /// `to_upper(Scope)`; see [`GraphTraversalSource::to_upper_scoped`].
    pub fn to_upper_scoped(self, scope: Scope) -> Self {
        self.push_step(GraphStep::Map("to_upper", Box::new(StepToUpper { scope })))
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepMap<'g, S> for StepToUpper {
    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<Traverser<S>> {
        let path = source.path;
        let bulk = source.bulk;
        let sack = source.sack;
        let value = match self.scope {
            Scope::Global => source.value,
            Scope::Local => match map_local_strings(context, source.value, "toUpper", |text| {
                Ok(transform(text))
            })? {
                LocalMapped::Done(value) => {
                    return Ok(Traverser::with_parts(value, path, bulk, sack));
                }
                LocalMapped::Single(value) => value,
            },
        };
        // TinkerPop: a `null` maps to `null` (`inject("a", null).toUpper()` → "A", null).
        if crate::traversal::step::local_scope::is_null(context, &value)? {
            return Ok(Traverser::with_parts(
                TraverserValue::Computed(ElementProperty::Null, None),
                path,
                bulk,
                sack,
            ));
        }
        match Traverser::as_string_ref(context, &value)? {
            Some(text) => Ok(Traverser::with_parts(
                TraverserValue::Computed(bounded_string(context, transform(&text))?, None),
                path,
                bulk,
                sack,
            )),
            None => Err(Box::new(TraverserError::cast_exception_str(
                &["string"],
                Traverser::observed_type_name(context, &value),
            ))),
        }
    }

    fn materializes(&self) -> bool {
        self.scope == Scope::Local
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
    fn v_simple() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.inject(vec!["HeLlO"]).to_upper().to_list(),
            rhai: [
                r#"g.inject("HeLlO").toUpper()"#,
                r#"g.inject("HeLlO").to_upper()"#,
            ],
            assert: |res| assert_eq!(res[0], ElementProperty::String("HELLO".into())),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn where_t() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.inject(vec!["HeLlO"]).where_t(__::to_upper()).to_list(),
            rhai: [
                r#"g.inject("HeLlO").where(__.toUpper())"#,
                r#"g.inject("HeLlO").where(__.to_upper())"#,
            ],
            assert: |res| assert_eq!(res[0], ElementProperty::String("HeLlO".into())),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn where_t_inner() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.inject(vec!["HeLlO"]).where_t(__::to_lower().to_upper()).to_list(),
            rhai: [
                r#"g.inject("HeLlO").where(__.toLower().toUpper())"#,
                r#"g.inject("HeLlO").where(__.to_lower().to_upper())"#,
            ],
            assert: |res| assert_eq!(res[0], ElementProperty::String("HeLlO".into())),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_simple_none() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.inject(vec!["HELLO"]).to_upper().to_list(),
            rhai: [
                r#"g.inject("HELLO").toUpper()"#,
                r#"g.inject("HELLO").to_upper()"#,
            ],
            assert: |res| assert_eq!(res[0], ElementProperty::String("HELLO".into())),
        );
    }

    #[test]
    fn v_err_number() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.inject(vec![42]).to_upper().next();
        assert!(result.is_err());
    }

    #[test]
    fn v_err_v() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.v("1").to_upper().next();
        assert!(result.is_err());
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.inject(vec!["HELLO"]).to_upper().profile().unwrap();
        assert!(result.to_string().contains("to_upper("));
    }
}
