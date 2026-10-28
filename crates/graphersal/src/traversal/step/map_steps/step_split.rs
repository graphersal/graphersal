//! `split(separator)` / `split(Scope, separator)`: a string split into a list of strings.

use compact_str::CompactString;

use crate::graph::GraphStorage;
use crate::traversal::step::local_scope::{LocalMapped, is_null, map_local_strings};
use crate::value::Scope;
use crate::*;

/// `split()` (TinkerPop `SplitGlobalStep`/`SplitLocalStep`); see
/// [`GraphTraversalSource::split`] for the semantics.
#[derive(Clone)]
struct StepSplit {
    scope: Scope,
    /// `None` splits on whitespace.
    separator: Option<CompactString>,
}

impl fmt::Display for StepSplit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let separator = match &self.separator {
            Some(separator) => crate::value::literal::rhai_string(separator),
            None => "()".to_string(),
        };
        match self.scope {
            Scope::Global => write!(f, "split({separator})"),
            Scope::Local => write!(f, "split(Scope.local, {separator})"),
        }
    }
}

/// Java's `Character.isWhitespace`: the Unicode space separators except the no-break spaces,
/// plus the ASCII controls `\t`, `\n`, `\u{b}`, `\u{c}`, `\r` and `\u{1c}`..=`\u{1f}`.
fn is_java_whitespace(c: char) -> bool {
    (c.is_whitespace() && !matches!(c, '\u{85}' | '\u{a0}' | '\u{2007}' | '\u{202f}'))
        || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// Splits `text` exactly as TinkerPop's `StringUtil.split` does:
///
/// - `None`: on runs of whitespace, no empty strings (`"a  b "` → `[a, b]`);
/// - `""`: into single characters (an empty string gives `[""]`);
/// - otherwise Apache Commons' `splitByWholeSeparator`: adjacent separators produce no empty
///   string, but a separator at the very end leaves a trailing `""` (`"that"` by `"h"` →
///   `[t, at]`, `"ath"` by `"h"` → `[at, ""]`), and an empty string gives `[]`.
fn split_text(text: &str, separator: Option<&str>) -> Vec<ElementProperty> {
    let piece = |s: &str| ElementProperty::String(CompactString::new(s));
    match separator {
        Some("") => {
            if text.is_empty() {
                return vec![piece("")];
            }
            text.chars()
                .map(|c| {
                    let mut buf = [0u8; 4];
                    piece(c.encode_utf8(&mut buf))
                })
                .collect()
        }
        None => text
            .split(is_java_whitespace)
            .filter(|s| !s.is_empty())
            .map(piece)
            .collect(),
        Some(separator) => {
            let mut out = Vec::new();
            if text.is_empty() {
                return out;
            }
            let len = text.len();
            let mut begin = 0;
            let mut end = 0;
            while end < len {
                match text[begin..].find(separator) {
                    Some(offset) => {
                        end = begin + offset;
                        if end > begin {
                            out.push(piece(&text[begin..end]));
                        }
                        begin = end + separator.len();
                    }
                    None => {
                        out.push(piece(&text[begin..]));
                        end = len;
                    }
                }
            }
            out
        }
    }
}

impl StepSplit {
    fn transform(&self, text: &str) -> ElementProperty {
        ElementProperty::Array(split_text(text, self.separator.as_deref()))
    }
}

fn split_step<'g, S: GraphStorage + 'g>(scope: Scope, separator: Option<&str>) -> GraphStep<'g, S> {
    GraphStep::Map(
        "split",
        Box::new(StepSplit {
            scope,
            separator: separator.map(CompactString::new),
        }),
    )
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// `split(separator)` (TinkerPop `SplitGlobalStep`): the incoming string split into a list
    /// of strings around `separator`, which is not kept (`"that"` split by `"h"` →
    /// `["t", "at"]`).
    ///
    /// `None` splits on runs of whitespace; `""` splits into single characters. Adjacent
    /// separators produce no empty string, a separator at the very end leaves a trailing `""`
    /// (Apache Commons' `splitByWholeSeparator`, which TinkerPop uses). A `null` stays `null`;
    /// any other value is a cast error. Details: `book/src/traversal/steps/split.md`.
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let graph = GraphSource::tinkerpop_modern();
    /// let lock = graph.read();
    /// let parts = lock.traversal().inject(vec!["a-b-c"]).split("-").next().unwrap().unwrap();
    /// assert_eq!(parts, ElementProperty::from(vec!["a", "b", "c"]));
    /// ```
    pub fn split<'a>(self, separator: impl Into<Option<&'a str>>) -> Self {
        self.push_step(split_step(Scope::Global, separator.into()))
    }

    /// `split(Scope, separator)`: `Scope::Global` is [`split()`](Self::split); `Scope::Local`
    /// (TinkerPop `SplitLocalStep`) splits each string of the list a traverser holds, giving a
    /// list of lists (`g.V().values("name").fold().split(Scope.local, "a")`); a `null` element
    /// or input stays `null`, a single string is split as by the unscoped step.
    pub fn split_scoped<'a>(self, scope: Scope, separator: impl Into<Option<&'a str>>) -> Self {
        self.push_step(split_step(scope, separator.into()))
    }
}

impl __ {
    /// Anonymous `split(separator)`; see [`GraphTraversalSource::split`].
    pub fn split<'a, 'g, S: GraphStorage + 'g>(
        separator: impl Into<Option<&'a str>>,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().split(separator)
    }

    /// Anonymous `split(Scope, separator)`; see [`GraphTraversalSource::split_scoped`].
    pub fn split_scoped<'a, 'g, S: GraphStorage + 'g>(
        scope: Scope,
        separator: impl Into<Option<&'a str>>,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().split_scoped(scope, separator)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// `split(separator)`; see [`GraphTraversalSource::split`].
    pub fn split<'a>(self, separator: impl Into<Option<&'a str>>) -> Self {
        self.push_step(split_step(Scope::Global, separator.into()))
    }

    /// `split(Scope, separator)`; see [`GraphTraversalSource::split_scoped`].
    pub fn split_scoped<'a>(self, scope: Scope, separator: impl Into<Option<&'a str>>) -> Self {
        self.push_step(split_step(scope, separator.into()))
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepMap<'g, S> for StepSplit {
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<Traverser<S>> {
        let Traverser {
            value,
            path,
            bulk,
            sack,
        } = source;
        let value = match self.scope {
            Scope::Global => value,
            Scope::Local => {
                match map_local_strings(context, value, "split", |text| Ok(self.transform(text)))? {
                    LocalMapped::Done(value) => {
                        return Ok(Traverser::with_parts(value, path, bulk, sack));
                    }
                    LocalMapped::Single(value) => value,
                }
            }
        };
        // TinkerPop: a `null` maps to `null`.
        let result = if is_null(context, &value)? {
            ElementProperty::Null
        } else {
            match Traverser::as_string_ref(context, &value)? {
                Some(text) => self.transform(&text),
                None => {
                    return Err(Box::new(TraverserError::cast_exception_str(
                        &["string"],
                        Traverser::observed_type_name(context, &value),
                    )));
                }
            }
        };
        Ok(Traverser::with_parts(
            TraverserValue::Computed(result, None),
            path,
            bulk,
            sack,
        ))
    }

    /// Builds a new list.
    fn materializes(&self) -> bool {
        true
    }

    /// The step never drops a traverser, but it can fail: a downstream fused `count()` must
    /// still run it (the default `process_filter` calls `process`).
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

    fn strings(parts: Vec<ElementProperty>) -> Vec<String> {
        parts
            .into_iter()
            .map(|p| p.as_str().unwrap_or("<not a string>").to_string())
            .collect()
    }

    #[test]
    fn splits_like_commons_split_by_whole_separator() {
        assert_eq!(strings(split_text("that", Some("h"))), ["t", "at"]);
        assert_eq!(strings(split_text("this", Some("h"))), ["t", "is"]);
        assert_eq!(strings(split_text("test", Some("h"))), ["test"]);
        assert_eq!(
            strings(split_text("ab-!-cd-!-ef", Some("-!-"))),
            ["ab", "cd", "ef"]
        );
        assert_eq!(strings(split_text("a--b", Some("-"))), ["a", "b"]);
        assert_eq!(strings(split_text("hat", Some("h"))), ["at"]);
        assert_eq!(strings(split_text("ath", Some("h"))), ["at", ""]);
        assert_eq!(strings(split_text("hh", Some("h"))), [""]);
        assert!(split_text("", Some("h")).is_empty());
    }

    #[test]
    fn splits_on_whitespace_and_into_characters() {
        assert_eq!(
            strings(split_text(" hello \t world ", None)),
            ["hello", "world"]
        );
        assert_eq!(strings(split_text("a\u{a0}b c", None)), ["a\u{a0}b", "c"]);
        assert!(split_text("   ", None).is_empty());
        assert_eq!(strings(split_text("añb", Some(""))), ["a", "ñ", "b"]);
        assert_eq!(strings(split_text("", Some(""))), [""]);
    }

    #[test]
    fn global_split_keeps_null() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .inject(vec![ElementProperty::from("that"), ElementProperty::Null])
                .split("h")
                .to_list(),
            rhai: [r#"g.inject("that", ()).split("h")"#],
            assert: |res| assert_eq!(res, &[ElementProperty::from(vec!["t", "at"]), ElementProperty::Null]),
        );
    }

    #[test]
    fn null_separator_splits_on_whitespace() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.inject(vec!["hello world"]).split(None).to_list(),
            rhai: [r#"g.inject("hello world").split(())"#],
            assert: |res| assert_eq!(res, &[ElementProperty::from(vec!["hello", "world"])]),
        );
    }

    #[test]
    fn local_split_maps_each_element() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v(None)
                .has_label("person")
                .values("name")
                .order()
                .fold()
                .split_scoped(Scope::Local, "a")
                .unfold()
                .to_list(),
            rhai: [
                r#"g.V().hasLabel("person").values("name").order().fold().split(Scope.local, "a").unfold()"#,
                r#"g.v().has_label("person").values("name").order().fold().split(Scope::local, "a").unfold()"#,
            ],
            assert: |res| assert_eq!(
                res,
                &[
                    ElementProperty::from(vec!["josh"]),
                    ElementProperty::from(vec!["m", "rko"]),
                    ElementProperty::from(vec!["peter"]),
                    ElementProperty::from(vec!["v", "d", "s"]),
                ]
            ),
        );
    }

    #[test]
    fn rejects_non_strings() {
        test_dual_err!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.inject(vec![vec!["a", "b"]]).split("a").to_list(),
            rhai: [r#"g.inject(["a", "b"]).split("a")"#],
            assert: |err| assert!(err.to_string().contains("string"), "{err}"),
        );
    }

    #[test]
    fn profile_names_the_step() {
        let graph = GraphSource::tinkerpop_modern();
        let lock = graph.read();
        let metrics = lock
            .traversal()
            .inject(vec!["a b"])
            .split_scoped(Scope::Local, None)
            .profile()
            .unwrap();
        assert!(
            metrics.to_string().contains("split(Scope.local, ())"),
            "{metrics}"
        );
    }
}
