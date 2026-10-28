use crate::graph::GraphStorage;
use crate::traversal::step::local_scope::{LocalMapped, bounded_string, map_local_strings};
use crate::value::Scope;
use crate::*;

/// `substring(start[, end])`: the characters `start..end` of a string (to its end when `end` is
/// absent). A negative index counts from the end of the string; indices are clamped to the
/// string, and an empty range yields `""`. With `Scope::Local` it cuts each string of the list a
/// traverser holds instead (a `null` element stays `null`).
#[derive(Default, Clone)]
struct StepSubstring {
    scope: Scope,
    s_from: i32,
    s_to: Option<i32>,
}

impl fmt::Display for StepSubstring {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "substring(")?;
        if self.scope == Scope::Local {
            write!(f, "Scope.local, ")?;
        }
        match self.s_to {
            Some(s_to) => write!(f, "{}, {})", self.s_from, s_to),
            None => write!(f, "{})", self.s_from),
        }
    }
}

/// Resolves a (possibly negative) character index against a string of `len` characters,
/// clamped to `0..=len`: a negative index counts from the end (TinkerPop's
/// `processStringIndex`). Never recurses, so an empty string with a negative index is `0`.
fn clamp_index(index: i32, len: usize) -> usize {
    let len = i64::try_from(len).unwrap_or(i64::MAX);
    let index = i64::from(index);
    let resolved = if index < 0 {
        (len + index).max(0)
    } else {
        index.min(len)
    };
    usize::try_from(resolved).unwrap_or(0)
}

impl StepSubstring {
    /// The per-string transformation, shared by the single-value and the per-element path.
    /// Works on characters, not bytes, so a multi-byte character is never split.
    fn transform(&self, text: &str) -> ElementProperty {
        let ascii = text.is_ascii();
        let len = if ascii {
            text.len()
        } else {
            text.chars().count()
        };
        let start = clamp_index(self.s_from, len);
        let end = self.s_to.map_or(len, |s_to| clamp_index(s_to, len));
        if start >= end {
            return ElementProperty::String("".into());
        }
        if ascii {
            ElementProperty::String(text.get(start..end).unwrap_or("").into())
        } else {
            let cut: String = text.chars().skip(start).take(end - start).collect();
            ElementProperty::String(cut.into())
        }
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// The characters `s_from..s_to` of a string; a negative index counts from the end.
    pub fn substring(self, s_from: i32, s_to: i32) -> Self {
        self.substring_scoped(Scope::Global, s_from, Some(s_to))
    }

    /// `substring(start)`: the characters from `s_from` to the end of a string; a negative
    /// `s_from` counts from the end (`g.V().values("name").substring(-3)` → `rko`, ...).
    pub fn substring_from(self, s_from: i32) -> Self {
        self.substring_scoped(Scope::Global, s_from, None)
    }

    /// `substring(Scope, start[, end])`: `Scope::Global` is [`substring()`](Self::substring) /
    /// [`substring_from()`](Self::substring_from) (`s_to == None` cuts to the end);
    /// `Scope::Local` cuts each string of the list a traverser holds
    /// (`g.V().hasLabel("software").values("name").order().fold().substring(Scope.local, 1, 4)`
    /// → `[op, ipp]`); a `null` element or input stays `null`, a single string is cut as by the
    /// unscoped step.
    pub fn substring_scoped(self, scope: Scope, s_from: i32, s_to: Option<i32>) -> Self {
        self.push_step(GraphStep::Map(
            "substring",
            Box::new(StepSubstring {
                scope,
                s_from,
                s_to,
            }),
        ))
    }
}

impl __ {
    /// Anonymous `substring(start, end)`.
    pub fn substring<'g, S: GraphStorage + 'g>(
        s_from: i32,
        s_to: i32,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().substring(s_from, s_to)
    }

    /// Anonymous `substring(start)`; see [`GraphTraversalSource::substring_from`].
    pub fn substring_from<'g, S: GraphStorage + 'g>(s_from: i32) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().substring_from(s_from)
    }

    /// Anonymous `substring(Scope, start[, end])`; see
    /// [`GraphTraversalSource::substring_scoped`].
    pub fn substring_scoped<'g, S: GraphStorage + 'g>(
        scope: Scope,
        s_from: i32,
        s_to: Option<i32>,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().substring_scoped(scope, s_from, s_to)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// `substring(start, end)`; see [`GraphTraversalSource::substring`].
    pub fn substring(self, s_from: i32, s_to: i32) -> Self {
        self.substring_scoped(Scope::Global, s_from, Some(s_to))
    }

    /// `substring(start)`; see [`GraphTraversalSource::substring_from`].
    pub fn substring_from(self, s_from: i32) -> Self {
        self.substring_scoped(Scope::Global, s_from, None)
    }

    /// `substring(Scope, start[, end])`; see [`GraphTraversalSource::substring_scoped`].
    pub fn substring_scoped(self, scope: Scope, s_from: i32, s_to: Option<i32>) -> Self {
        self.push_step(GraphStep::Map(
            "substring",
            Box::new(StepSubstring {
                scope,
                s_from,
                s_to,
            }),
        ))
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepMap<'g, S> for StepSubstring {
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
                match map_local_strings(context, source.value, "substring", |text| {
                    Ok(self.transform(text))
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
        match Traverser::as_string_ref(context, &value)? {
            Some(text) => Ok(Traverser::with_parts(
                TraverserValue::Computed(bounded_string(context, self.transform(&text))?, None),
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
            rust: |g| g.inject(vec!["hello world"]).substring(1, 8).to_list(),
            rhai: [
                r#"g.inject("hello world").substring(1, 8)"#,
                r#"g.inject("hello world").substring(1, 8)"#,
            ],
            assert: |res| assert_eq!(res[0], ElementProperty::String("ello wo".into())),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn where_t() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.inject(vec!["hello world"]).where_t(__::substring(1, 8).is("ello wo")).to_list(),
            rhai: [
                r#"g.inject("hello world").where(__.substring(1, 8).is("ello wo"))"#,
                r#"g.inject("hello world").where(__.substring(1, 8).is("ello wo"))"#,
            ],
            assert: |res| assert_eq!(res[0], ElementProperty::String("hello world".into())),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn where_t_inner() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.inject(vec!["hello world"]).where_t(__::trim().substring(1, 8).is("ello wo")).to_list(),
            rhai: [
                r#"g.inject("hello world").where(__.trim().substring(1, 8).is("ello wo"))"#,
                r#"g.inject("hello world").where(__.trim().substring(1, 8).is("ello wo"))"#,
            ],
            assert: |res| assert_eq!(res[0], ElementProperty::String("hello world".into())),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_simple_1() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.inject(vec!["test"]).substring(1, 8).to_list(),
            rhai: [
                r#"g.inject("test").substring(1, 8)"#,
                r#"g.inject("test").substring(1, 8)"#,
            ],
            assert: |res| assert_eq!(res[0], ElementProperty::String("est".into())),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_simple_none() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.inject(vec!["hello"]).substring(10, 20).to_list(),
            rhai: [
                r#"g.inject("hello").substring(10, 20)"#,
                r#"g.inject("hello").substring(10, 20)"#,
            ],
            assert: |res| assert_eq!(res[0], ElementProperty::String("".into())),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_simple_none_1() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.inject(vec!["hello"]).substring(2, 0).to_list(),
            rhai: [
                r#"g.inject("hello").substring(2, 0)"#,
                r#"g.inject("hello").substring(2, 0)"#,
            ],
            assert: |res| assert_eq!(res[0], ElementProperty::String("".into())),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_simple_negative() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.inject(vec!["hello world"]).substring(-4, 20).to_list(),
            rhai: [
                r#"g.inject("hello world").substring(-4, 20)"#,
                r#"g.inject("hello world").substring(-4, 20)"#,
            ],
            assert: |res| assert_eq!(res[0], ElementProperty::String("orld".into())),
        );
    }

    #[test]
    fn v_err() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.inject(vec![10]).substring(-4, 20).next();
        assert!(result.is_err());
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g
            .inject(vec!["hello world"])
            .substring(-4, 20)
            .profile()
            .unwrap();
        assert!(result.to_string().contains("substring("));
    }
}
