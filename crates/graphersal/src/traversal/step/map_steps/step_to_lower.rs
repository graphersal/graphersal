use crate::graph::GraphStorage;
use crate::traversal::step::local_scope::{LocalMapped, bounded_string, map_local_strings};
use crate::value::Scope;
use crate::*;

/// `to_lower()` (TinkerPop `toLower()`): lower-cases a string. With `Scope::Local` it
/// transforms each string of the list a traverser holds instead (a `null` element stays `null`).
#[derive(Default, Clone)]
struct StepToLower {
    scope: Scope,
}

impl fmt::Display for StepToLower {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.scope {
            Scope::Global => write!(f, "to_lower()"),
            Scope::Local => write!(f, "to_lower(Scope.local)"),
        }
    }
}

/// The lower-casing of `to_lower()`: Rust's `str::to_lowercase` (Unicode, no locale). The ONE
/// definition, shared with the `to_lower` transform of a property index
/// ([`PropertyIndexTransform::ToLower`](crate::graph::property_index::PropertyIndexTransform)),
/// so an index key is exactly what the step computes.
pub(crate) fn lower_text(text: &str) -> String {
    text.to_lowercase()
}

/// The per-string transformation, shared by the single-value and the per-element path.
fn transform(text: &str) -> ElementProperty {
    ElementProperty::String(lower_text(text).into())
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Lower-cases a string; fails on any other value.
    pub fn to_lower(self) -> Self {
        self.to_lower_scoped(Scope::Global)
    }

    /// `to_lower(Scope)`: `Scope::Global` is [`to_lower()`](Self::to_lower); `Scope::Local` transforms each
    /// string of the list a traverser holds (`g.V().values("name").fold().to_lower(Scope.local)`); a `null` element or input stays `null`, a
    /// single string is transformed as by the unscoped step.
    pub fn to_lower_scoped(self, scope: Scope) -> Self {
        self.push_step(GraphStep::Map("to_lower", Box::new(StepToLower { scope })))
    }
}

impl __ {
    /// Anonymous `to_lower()`.
    pub fn to_lower<'g, S: GraphStorage + 'g>() -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().to_lower()
    }

    /// Anonymous `to_lower(Scope)`; see [`GraphTraversalSource::to_lower_scoped`].
    pub fn to_lower_scoped<'g, S: GraphStorage + 'g>(scope: Scope) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().to_lower_scoped(scope)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Lower-cases a string; see [`GraphTraversalSource::to_lower`].
    pub fn to_lower(self) -> Self {
        self.to_lower_scoped(Scope::Global)
    }

    /// `to_lower(Scope)`; see [`GraphTraversalSource::to_lower_scoped`].
    pub fn to_lower_scoped(self, scope: Scope) -> Self {
        self.push_step(GraphStep::Map("to_lower", Box::new(StepToLower { scope })))
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepMap<'g, S> for StepToLower {
    fn index_transform(&self) -> Option<crate::graph::PropertyIndexTransform> {
        // Only the unscoped form: it maps one string to its lowered text, as the index does.
        (self.scope == Scope::Global).then_some(crate::graph::PropertyIndexTransform::ToLower)
    }

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
            Scope::Local => match map_local_strings(context, source.value, "toLower", |text| {
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
            rust: |g| g.inject(vec!["HeLlO"]).to_lower().to_list(),
            rhai: [
                r#"g.inject("HeLlO").toLower()"#,
                r#"g.inject("HeLlO").to_lower()"#,
            ],
            assert: |res| assert_eq!(res[0], ElementProperty::String("hello".into())),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn where_t() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.inject(vec!["HeLlO"]).where_t(__::to_lower()).to_list(),
            rhai: [
                r#"g.inject("HeLlO").where(__.toLower())"#,
                r#"g.inject("HeLlO").where(__.to_lower())"#,
            ],
            assert: |res| assert_eq!(res[0], ElementProperty::String("HeLlO".into())),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn where_t_inner() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.inject(vec!["HeLlO"]).where_t(__::trim().to_lower()).to_list(),
            rhai: [
                r#"g.inject("HeLlO").where(__.trim().toLower())"#,
                r#"g.inject("HeLlO").where(__.trim().to_lower())"#,
            ],
            assert: |res| assert_eq!(res[0], ElementProperty::String("HeLlO".into())),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_simple_none() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.inject(vec!["hello"]).to_lower().to_list(),
            rhai: [
                r#"g.inject("hello").toLower()"#,
                r#"g.inject("hello").to_lower()"#,
            ],
            assert: |res| assert_eq!(res[0], ElementProperty::String("hello".into())),
        );
    }

    #[test]
    fn v_err_number() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.inject(vec![42]).to_lower().next();
        assert!(result.is_err());
    }

    #[test]
    fn v_err_v() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.v("1").to_lower().next();
        assert!(result.is_err());
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.inject(vec!["HeLlO"]).to_lower().profile().unwrap();
        assert!(result.to_string().contains("to_lower("));
    }
}
