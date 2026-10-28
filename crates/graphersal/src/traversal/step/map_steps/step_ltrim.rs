use crate::graph::GraphStorage;
use crate::traversal::step::local_scope::{LocalMapped, bounded_string, map_local_strings};
use crate::value::Scope;
use crate::*;

/// `l_trim()` (TinkerPop `lTrim()`): removes leading whitespace from a string. With `Scope::Local` it
/// transforms each string of the list a traverser holds instead (a `null` element stays `null`).
///
/// Deviation from TinkerPop: whitespace is the Unicode White_Space set (Rust `str::trim*`), while Java's
/// `String.trim()` strips only the characters <= U+0020. A no-break space (U+00A0) or an ideographic space
/// (U+3000) is removed here and kept there. See `book/src/users_guide/type_conversion.md`.
#[derive(Default, Clone)]
struct StepLTrim {
    scope: Scope,
}

impl fmt::Display for StepLTrim {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.scope {
            Scope::Global => write!(f, "l_trim()"),
            Scope::Local => write!(f, "l_trim(Scope.local)"),
        }
    }
}

/// The per-string transformation, shared by the single-value and the per-element path.
fn transform(text: &str) -> ElementProperty {
    ElementProperty::String(text.trim_start().into())
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Removes leading whitespace from a string; fails on any other value.
    pub fn l_trim(self) -> Self {
        self.l_trim_scoped(Scope::Global)
    }

    /// `l_trim(Scope)`: `Scope::Global` is [`l_trim()`](Self::l_trim); `Scope::Local` transforms each
    /// string of the list a traverser holds (`g.V().values("name").fold().l_trim(Scope.local)`); a `null` element or input stays `null`, a
    /// single string is transformed as by the unscoped step.
    pub fn l_trim_scoped(self, scope: Scope) -> Self {
        self.push_step(GraphStep::Map("l_trim", Box::new(StepLTrim { scope })))
    }
}

impl __ {
    /// Anonymous `l_trim()`.
    pub fn l_trim<'g, S: GraphStorage + 'g>() -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().l_trim()
    }

    /// Anonymous `l_trim(Scope)`; see [`GraphTraversalSource::l_trim_scoped`].
    pub fn l_trim_scoped<'g, S: GraphStorage + 'g>(scope: Scope) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().l_trim_scoped(scope)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Removes leading whitespace from a string; see [`GraphTraversalSource::l_trim`].
    pub fn l_trim(self) -> Self {
        self.l_trim_scoped(Scope::Global)
    }

    /// `l_trim(Scope)`; see [`GraphTraversalSource::l_trim_scoped`].
    pub fn l_trim_scoped(self, scope: Scope) -> Self {
        self.push_step(GraphStep::Map("l_trim", Box::new(StepLTrim { scope })))
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepMap<'g, S> for StepLTrim {
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
                match map_local_strings(context, source.value, "lTrim", |text| Ok(transform(text)))?
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
                Traverser::observed_type_name(context, &value)?,
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

    #[test]
    fn v_simple() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.inject(vec!["hello"]).l_trim().to_list(),
            rhai: [
                r#"g.inject("hello").lTrim()"#,
                r#"g.inject("hello").l_trim()"#,
            ],
            assert: |res| assert_eq!(res[0], ElementProperty::String("hello".into())),
        );
    }

    #[test]
    fn where_t() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.inject(vec!["hello"]).where_t(__::l_trim().is("hello")).to_list(),
            rhai: [
                r#"g.inject("hello").where(__.lTrim().is("hello"))"#,
                r#"g.inject("hello").where(__.l_trim().is("hello"))"#,
            ],
            assert: |res| assert_eq!(res[0], ElementProperty::String("hello".into())),
        );
    }

    #[test]
    fn where_t_inner() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.inject(vec!["hello"]).where_t(__::to_lower().l_trim().is("hello")).to_list(),
            rhai: [
                r#"g.inject("hello").where(__.toLower().lTrim().is("hello"))"#,
                r#"g.inject("hello").where(__.to_lower().l_trim().is("hello"))"#,
            ],
            assert: |res| assert_eq!(res[0], ElementProperty::String("hello".into())),
        );
    }

    #[test]
    fn v_simple_none() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.inject(vec!["    hello   "]).l_trim().to_list(),
            rhai: [
                r#"g.inject("    hello   ").lTrim()"#,
                r#"g.inject("    hello   ").l_trim()"#,
            ],
            assert: |res| assert_eq!(res[0], ElementProperty::String("hello   ".into())),
        );
    }

    #[test]
    fn v_err_number() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.inject(vec![42]).l_trim().next();
        assert!(result.is_err());
    }

    #[test]
    fn v_err_v() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.v("1").l_trim().next();
        assert!(result.is_err());
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.inject(vec!["    hello   "]).l_trim().profile().unwrap();
        assert!(result.to_string().contains("l_trim("));
    }
}
