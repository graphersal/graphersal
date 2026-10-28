use crate::graph::GraphStorage;
use crate::traversal::step::local_scope::{LocalMapped, map_local_strings};
use crate::value::Scope;
use crate::*;

/// `replace(from, to)`: replaces every occurrence of `from` in a string by `to`. With
/// `Scope::Local` it does so in each string of the list a traverser holds instead (a `null`
/// element stays `null`). A `None` (`null`) or empty `from`, or a `None` `to`, leaves the string
/// unchanged, as TinkerPop's `StringUtils.replace` does.
#[derive(Default, Clone)]
struct StepReplace {
    scope: Scope,
    r_from: Option<String>,
    r_to: Option<String>,
}

impl fmt::Display for StepReplace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = |value: &Option<String>| match value {
            Some(value) => crate::value::literal::rhai_string(value),
            None => "()".to_string(),
        };
        match self.scope {
            Scope::Global => write!(f, "replace({}, {})", text(&self.r_from), text(&self.r_to)),
            Scope::Local => {
                write!(
                    f,
                    "replace(Scope.local, {}, {})",
                    text(&self.r_from),
                    text(&self.r_to)
                )
            }
        }
    }
}

impl StepReplace {
    fn new(scope: Scope, r_from: Option<&str>, r_to: Option<&str>) -> Self {
        StepReplace {
            scope,
            r_from: r_from.map(str::to_string),
            r_to: r_to.map(str::to_string),
        }
    }

    /// The per-string transformation, shared by the single-value and the per-element path.
    ///
    /// Replacing can multiply the length of a string (every one of `n` matches grows by the
    /// difference of `to` and `from`), so the length of the result is computed from the match
    /// count first and checked against `traversal.max_string_bytes` (`max`) BEFORE the string is
    /// built: a too long result is never allocated.
    fn transform(&self, text: &str, max: u64) -> TraverserResult<ElementProperty> {
        match (&self.r_from, &self.r_to) {
            (Some(from), Some(to)) if !from.is_empty() => {
                if to.len() > from.len() {
                    let matches = text.matches(from.as_str()).count() as u64;
                    let grown = (text.len() as u64)
                        .saturating_add(matches.saturating_mul((to.len() - from.len()) as u64));
                    crate::traversal::context::check_string_bytes(
                        usize::try_from(grown).unwrap_or(usize::MAX),
                        max,
                    )?;
                }
                Ok(ElementProperty::String(
                    text.replace(from.as_str(), to).into(),
                ))
            }
            _ => Ok(ElementProperty::String(text.into())),
        }
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Replaces every occurrence of `r_from` in a string by `r_to`; fails on any other value.
    /// A `None` (`null`) or empty `r_from`, or a `None` `r_to`, leaves the string unchanged
    /// (TinkerPop's `StringUtils.replace`): `replace(None, "g")` is the identity on strings.
    pub fn replace<'a>(
        self,
        r_from: impl Into<Option<&'a str>>,
        r_to: impl Into<Option<&'a str>>,
    ) -> Self {
        self.replace_scoped(Scope::Global, r_from, r_to)
    }

    /// `replace(Scope, from, to)`: `Scope::Global` is [`replace()`](Self::replace);
    /// `Scope::Local` replaces in each string of the list a traverser holds
    /// (`g.V().hasLabel("software").values("name").order().fold().replace(Scope.local, "p", "g")`
    /// → `[log, riggle]`); a `null` element or input stays `null`, a single string is handled as
    /// by the unscoped step.
    pub fn replace_scoped<'a>(
        self,
        scope: Scope,
        r_from: impl Into<Option<&'a str>>,
        r_to: impl Into<Option<&'a str>>,
    ) -> Self {
        self.push_step(GraphStep::Map(
            "replace",
            Box::new(StepReplace::new(scope, r_from.into(), r_to.into())),
        ))
    }
}

impl __ {
    /// Anonymous `replace(from, to)`.
    pub fn replace<'a, 'g, S: GraphStorage + 'g>(
        r_from: impl Into<Option<&'a str>>,
        r_to: impl Into<Option<&'a str>>,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().replace(r_from, r_to)
    }

    /// Anonymous `replace(Scope, from, to)`; see [`GraphTraversalSource::replace_scoped`].
    pub fn replace_scoped<'a, 'g, S: GraphStorage + 'g>(
        scope: Scope,
        r_from: impl Into<Option<&'a str>>,
        r_to: impl Into<Option<&'a str>>,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().replace_scoped(scope, r_from, r_to)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// `replace(from, to)`; see [`GraphTraversalSource::replace`].
    pub fn replace<'a>(
        self,
        r_from: impl Into<Option<&'a str>>,
        r_to: impl Into<Option<&'a str>>,
    ) -> Self {
        self.replace_scoped(Scope::Global, r_from, r_to)
    }

    /// `replace(Scope, from, to)`; see [`GraphTraversalSource::replace_scoped`].
    pub fn replace_scoped<'a>(
        self,
        scope: Scope,
        r_from: impl Into<Option<&'a str>>,
        r_to: impl Into<Option<&'a str>>,
    ) -> Self {
        self.push_step(GraphStep::Map(
            "replace",
            Box::new(StepReplace::new(scope, r_from.into(), r_to.into())),
        ))
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepMap<'g, S> for StepReplace {
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
                let max = context.options.max_string_bytes;
                match map_local_strings(context, source.value, "replace", |text| {
                    self.transform(text, max)
                })? {
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
        let max = context.options.max_string_bytes;
        match Traverser::as_string_ref(context, &value)? {
            Some(text) => Ok(Traverser::with_parts(
                TraverserValue::Computed(self.transform(&text, max)?, None),
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
            rust: |g| g.inject(vec!["hello"]).replace("l", "#").to_list(),
            rhai: [
                r##"g.inject("hello").replace("l", "#")"##,
                r##"g.inject("hello").replace("l", "#")"##,
            ],
            assert: |res| assert_eq!(res[0], ElementProperty::String("he##o".into())),
        );
    }

    #[test]
    fn where_t() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.inject(vec!["hello"]).where_t(__::replace("l", "#").is("he##o")).to_list(),
            rhai: [
                r##"g.inject("hello").where(__.replace("l", "#").is("he##o"))"##,
                r##"g.inject("hello").where(__.replace("l", "#").is("he##o"))"##,
            ],
            assert: |res| assert_eq!(res[0], ElementProperty::String("hello".into())),
        );
    }

    #[test]
    fn where_t_inner() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.inject(vec!["hello"]).where_t(__::trim().replace("l", "#").is("he##o")).to_list(),
            rhai: [
                r##"g.inject("hello").where(__.trim().replace("l", "#").is("he##o"))"##,
                r##"g.inject("hello").where(__.trim().replace("l", "#").is("he##o"))"##,
            ],
            assert: |res| assert_eq!(res[0], ElementProperty::String("hello".into())),
        );
    }

    #[test]
    fn v_simple_none() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.inject(vec!["hello"]).replace("H", "#").to_list(),
            rhai: [
                r##"g.inject("hello").replace("H", "#")"##,
                r##"g.inject("hello").replace("H", "#")"##,
            ],
            assert: |res| assert_eq!(res[0], ElementProperty::String("hello".into())),
        );
    }

    #[test]
    fn null_or_empty_search_leaves_the_string() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).has_label("software").values("name").replace(None, "g").to_list(),
            rhai: [r#"g.V().hasLabel("software").values("name").replace((), "g")"#],
            assert: |res| assert_eq!(res, &["lop".into(), "ripple".into()]),
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.inject(vec!["lop"]).replace("", "x").replace("o", None).to_list(),
            rhai: [r#"g.inject("lop").replace("", "x").replace("o", ())"#],
            assert: |res| assert_eq!(res, &["lop".into()]),
        );
    }

    #[test]
    fn v_err_number() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.inject(vec![42]).replace("4", "2").next();
        assert!(result.is_err());
    }

    #[test]
    fn v_err_v() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.v("1").replace("4", "2").next();
        assert!(result.is_err());
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.inject(vec!["hello"]).replace("l", "#").profile().unwrap();
        assert!(result.to_string().contains("replace("));
    }
}
