use crate::graph::GraphStorage;
use crate::traversal::step::local_scope::{LocalMapped, bounded_string, map_local_strings};
use crate::value::Scope;
use crate::*;

/// `trim()` (TinkerPop `trim()`): removes leading and trailing whitespace from a string. With `Scope::Local` it
/// transforms each string of the list a traverser holds instead (a `null` element stays `null`).
///
/// Deviation from TinkerPop: whitespace is the Unicode White_Space set (Rust `str::trim*`), while Java's
/// `String.trim()` strips only the characters <= U+0020. A no-break space (U+00A0) or an ideographic space
/// (U+3000) is removed here and kept there. See `book/src/users_guide/type_conversion.md`.
#[derive(Default, Clone)]
struct StepTrim {
    scope: Scope,
}

impl fmt::Display for StepTrim {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.scope {
            Scope::Global => write!(f, "trim()"),
            Scope::Local => write!(f, "trim(Scope.local)"),
        }
    }
}

/// The per-string transformation, shared by the single-value and the per-element path.
fn transform(text: &str) -> ElementProperty {
    ElementProperty::String(text.trim().into())
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Removes leading and trailing whitespace from a string; fails on any other value.
    pub fn trim(self) -> Self {
        self.trim_scoped(Scope::Global)
    }

    /// `trim(Scope)`: `Scope::Global` is [`trim()`](Self::trim); `Scope::Local` transforms each
    /// string of the list a traverser holds (`g.V().values("name").fold().trim(Scope.local)`); a `null` element or input stays `null`, a
    /// single string is transformed as by the unscoped step.
    pub fn trim_scoped(self, scope: Scope) -> Self {
        self.push_step(GraphStep::Map("trim", Box::new(StepTrim { scope })))
    }
}

impl __ {
    /// Anonymous `trim()`.
    pub fn trim<'g, S: GraphStorage + 'g>() -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().trim()
    }

    /// Anonymous `trim(Scope)`; see [`GraphTraversalSource::trim_scoped`].
    pub fn trim_scoped<'g, S: GraphStorage + 'g>(scope: Scope) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().trim_scoped(scope)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Removes leading and trailing whitespace from a string; see [`GraphTraversalSource::trim`].
    pub fn trim(self) -> Self {
        self.trim_scoped(Scope::Global)
    }

    /// `trim(Scope)`; see [`GraphTraversalSource::trim_scoped`].
    pub fn trim_scoped(self, scope: Scope) -> Self {
        self.push_step(GraphStep::Map("trim", Box::new(StepTrim { scope })))
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepMap<'g, S> for StepTrim {
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
            Scope::Local => {
                match map_local_strings(context, source.value, "trim", |text| Ok(transform(text)))?
                {
                    LocalMapped::Done(value) => {
                        return Ok(Traverser::with_parts(value, path, bulk, sack));
                    }
                    LocalMapped::Single(value) => value,
                }
            }
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
            rust: |g| g.inject(vec!["hello"]).trim().to_list(),
            rhai: [
                r#"g.inject("hello").trim()"#,
                r#"g.inject("hello").trim()"#,
            ],
            assert: |res| assert_eq!(res[0], ElementProperty::String("hello".into())),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn where_t() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.inject(vec!["   hello   "]).where_t(__::trim().is("hello")).to_list(),
            rhai: [
                r#"g.inject("   hello   ").where(__.trim().is("hello"))"#,
                r#"g.inject("   hello   ").where(__.trim().is("hello"))"#,
            ],
            assert: |res| assert_eq!(res[0], ElementProperty::String("   hello   ".into())),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn where_t_inner() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.inject(vec!["   hello   "]).where_t(__::to_lower().trim().is("hello")).to_list(),
            rhai: [
                r#"g.inject("   hello   ").where(__.toLower().trim().is("hello"))"#,
                r#"g.inject("   hello   ").where(__.to_lower().trim().is("hello"))"#,
            ],
            assert: |res| assert_eq!(res[0], ElementProperty::String("   hello   ".into())),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_simple_none() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.inject(vec!["    hello   "]).trim().to_list(),
            rhai: [
                r#"g.inject("    hello   ").trim()"#,
                r#"g.inject("    hello   ").trim()"#,
            ],
            assert: |res| assert_eq!(res[0], ElementProperty::String("hello".into())),
        );
    }

    #[test]
    fn v_err_number() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.inject(vec![42]).trim().next();
        assert!(result.is_err());
    }

    #[test]
    fn v_err_v() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.v("1").trim().next();
        assert!(result.is_err());
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.inject(vec!["    hello   "]).trim().profile().unwrap();
        assert!(result.to_string().contains("trim("));
    }
}
