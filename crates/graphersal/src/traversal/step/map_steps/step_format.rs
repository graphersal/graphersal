//! `format(pattern)`: a string built from a pattern with `%{name}` placeholders.

use compact_str::CompactString;

use super::step_conjoin::element_string;
use super::step_select::scope_value;
use crate::graph::GraphStorage;
use crate::traversal::optimizer::path_analysis::{PathAnalysis, PathRequirement};
use crate::traversal::step::local_scope::is_null;
use crate::*;

/// The placeholder name that takes the next `by()` modulator of the ring.
const FROM_BY: &str = "_";

/// One piece of a parsed `format()` pattern.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Segment {
    /// Literal text, copied as is.
    Text(String),
    /// A `%{name}` placeholder (`%{_}` takes the next `by()`).
    Var(CompactString),
}

/// Whether `c` ends a line for Java's `.` (which does not match it).
fn is_line_terminator(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}')
}

/// Parses a pattern the way TinkerPop's `(?<!%)%\{(.*?)\}` finds placeholders: `%{` not preceded
/// by `%`, the name up to the first `}` on the same line. Everything else, `%%{x}` included, is
/// literal text (nothing is unescaped).
fn parse_pattern(pattern: &str) -> Vec<Segment> {
    let bytes = pattern.as_bytes();
    let mut segments = Vec::new();
    let mut last = 0;
    let mut from = 0;
    while let Some(offset) = pattern[from..].find("%{") {
        let start = from + offset;
        if start > 0 && bytes[start - 1] == b'%' {
            from = start + 1;
            continue;
        }
        let name_start = start + 2;
        let close = pattern[name_start..]
            .char_indices()
            .find(|(_, c)| *c == '}' || is_line_terminator(*c));
        match close {
            Some((end, '}')) => {
                if start > last {
                    segments.push(Segment::Text(pattern[last..start].to_string()));
                }
                segments.push(Segment::Var(CompactString::new(
                    &pattern[name_start..name_start + end],
                )));
                last = name_start + end + 1;
                from = last;
            }
            // No `}` before the end of the line: not a placeholder at this position.
            Some(_) | None => from = start + 1,
        }
    }
    if last < pattern.len() {
        segments.push(Segment::Text(pattern[last..].to_string()));
    }
    segments
}

/// `format()` (TinkerPop `FormatStep`); see [`GraphTraversalSource::format`] for the semantics.
struct StepFormat<'g, S: GraphStorage + 'g> {
    pattern: String,
    segments: Vec<Segment>,
    by: Vec<By<'g, S>>,
    by_check: ByModulatorCheck,
}

impl<'g, S: GraphStorage + 'g> Clone for StepFormat<'g, S> {
    fn clone(&self) -> Self {
        Self {
            pattern: self.pattern.clone(),
            segments: self.segments.clone(),
            by: self.by.clone(),
            by_check: self.by_check.clone(),
        }
    }
}

impl<'g, S: GraphStorage + 'g> fmt::Display for StepFormat<'g, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let pattern = crate::value::literal::rhai_string(&self.pattern);
        write!(f, "format({pattern})")?;
        for by in &self.by {
            f.write_str(&crate::traversal::render_by(by))?;
        }
        Ok(())
    }
}

fn format_step<'g, S: GraphStorage + 'g>(pattern: String) -> GraphStep<'g, S> {
    GraphStep::Map(
        "format",
        Box::new(StepFormat {
            segments: parse_pattern(&pattern),
            pattern,
            by: Vec::new(),
            by_check: ByModulatorCheck::default(),
        }),
    )
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// `format(pattern)` (TinkerPop `FormatStep`): a string built from `pattern`, each
    /// `%{name}` placeholder replaced by a value read from the incoming traverser.
    ///
    /// A placeholder `%{name}` is resolved in this order: the property `name` of an incoming
    /// vertex or edge; else, as `select(name)` resolves it, the key `name` of an incoming map
    /// (`elementMap()`, `project()`), a side-effect key, or the last path value labelled `name`
    /// (`as("name")`). `%{_}` takes the next `by()` modulator of the ring (round robin over the
    /// `by()`s, restarting for every traverser), or the incoming value itself without any
    /// `by()`. When a placeholder resolves to nothing or to `null` (a missing property, an
    /// unproductive `by()`, a `null` map value or path value), the traverser is filtered out. A
    /// stored `null` property is present: it is written as the text `null`, as TinkerPop's
    /// `StringBuilder.append` writes it. `%%{x}` is literal text. Values are written as `as_string()` writes them; a list
    /// or a map is a cast error (TinkerPop writes Java's `toString`). Details:
    /// `book/src/traversal/steps/format.md`.
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let graph = GraphSource::tinkerpop_modern();
    /// let lock = graph.read();
    /// let text = lock.traversal().v("1").format("%{name} is %{age} years old").next().unwrap().unwrap();
    /// assert_eq!(text, ElementProperty::from("marko is 29 years old"));
    /// ```
    pub fn format(self, pattern: impl Into<String>) -> Self {
        self.push_step(format_step(pattern.into()))
    }
}

impl __ {
    /// Anonymous `format(pattern)`; see [`GraphTraversalSource::format`].
    pub fn format<'g, S: GraphStorage + 'g>(
        pattern: impl Into<String>,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().format(pattern)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// `format(pattern)`; see [`GraphTraversalSource::format`].
    pub fn format(self, pattern: impl Into<String>) -> Self {
        self.push_step(format_step(pattern.into()))
    }
}

impl<'g, S: GraphStorage + 'g> StepFormat<'g, S> {
    /// The stored property `name` of an incoming vertex or edge (a stored `null` included);
    /// `None` for any other value and for an absent property.
    fn element_property(
        context: &TraverserContext<'_, S>,
        value: &TraverserValue<S>,
        name: &str,
    ) -> Result<Option<ElementProperty>, GraphError> {
        let properties = match value {
            TraverserValue::Vertex(vertex) => context.storage.vertex_properties(*vertex)?,
            TraverserValue::Edge(edge) => context.storage.edge_properties(*edge)?,
            TraverserValue::Computed(_, _)
            | TraverserValue::VertexProperty(_, _, _)
            | TraverserValue::EdgeProperty(_, _, _)
            | TraverserValue::Path(_)
            | TraverserValue::Record { .. }
            | TraverserValue::Object(_)
            | TraverserValue::PathProperty(_)
            | TraverserValue::Entry(_)
            | TraverserValue::Array(_)
            | TraverserValue::Schema(_)
            | TraverserValue::Graph(_) => None,
        };
        Ok(properties.and_then(|properties| properties.get(name).map(ElementProperty::to_logical)))
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepMap<'g, S> for StepFormat<'g, S> {
    /// Never called by the executor: `may_filter()` is true, and it calls `process_filter`.
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<Traverser<S>> {
        self.process_filter(context, source)?.ok_or_else(|| {
            Box::new(TraverserError::QueryExecution {
                query: self.to_string(),
                reason: "a placeholder of format() resolved to nothing".to_string(),
            })
        })
    }

    fn process_filter(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<Option<Traverser<S>>> {
        self.by_check.check("format")?;
        let mut out = String::with_capacity(self.pattern.len());
        let mut ring = 0usize;
        for segment in &self.segments {
            let name = match segment {
                Segment::Text(text) => {
                    out.push_str(text);
                    continue;
                }
                Segment::Var(name) => name,
            };
            if name != FROM_BY
                && let Some(property) = Self::element_property(context, &source.value, name)?
            {
                if property == ElementProperty::Null {
                    // TinkerPop appends the present property's `null` value: the text `null`.
                    out.push_str("null");
                    continue;
                }
                out.push_str(&element_string(
                    context,
                    &TraverserValue::Computed(property, None),
                )?);
                continue;
            }
            let value = if name == FROM_BY {
                if self.by.is_empty() {
                    Some(source.value.clone())
                } else {
                    let by = &self.by[ring % self.by.len()];
                    ring += 1;
                    TraverserValue::fetch_traverser_value_by(context, &source, &source.value, by)?
                }
            } else {
                scope_value(context, &source, name)?
            };
            match value {
                Some(value) if !is_null(context, &value)? => {
                    out.push_str(&element_string(context, &value)?);
                }
                // Unproductive: TinkerPop's `EmptyTraverser`.
                Some(_) | None => return Ok(None),
            }
            context.check_string_bytes(out.len())?;
        }
        Ok(Some(Traverser::with_parts(
            TraverserValue::Computed(ElementProperty::String(out.into()), None),
            source.path,
            source.bulk,
            source.sack,
        )))
    }

    fn may_filter(&self) -> bool {
        true
    }

    fn push_modulator_by(&mut self, args: By<'g, S>, _modulator_name: &str) -> ByModulation {
        self.by_check.observe_ring(&args);
        self.by.push(args);
        ByModulation::Accepted
    }

    fn path_requirement(&self) -> PathRequirement {
        // A placeholder that is not a property or a map key is a path label.
        PathRequirement::labels(self.segments.iter().filter_map(|segment| match segment {
            Segment::Var(name) if name != FROM_BY => Some(name.clone()),
            Segment::Var(_) | Segment::Text(_) => None,
        }))
    }

    fn analyze_nested_paths(
        &mut self,
        analysis: &PathAnalysis,
        _downstream: &PathRequirement,
    ) -> PathRequirement {
        // `by(__...)` children run on the incoming traverser, path included.
        By::analyze_all_paths(&mut self.by, analysis)
    }

    fn child_traversals_mut(&mut self) -> Vec<&mut AnonymousTraversal<'g, S>> {
        self.by.iter_mut().filter_map(By::traversal_mut).collect()
    }

    fn clone_box(&self) -> Box<dyn GraphStepMap<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> Segment {
        Segment::Text(s.to_string())
    }

    fn var(s: &str) -> Segment {
        Segment::Var(CompactString::new(s))
    }

    #[test]
    fn parses_placeholders_like_tinkerpop() {
        assert_eq!(
            parse_pattern("%{name} is %{age}!"),
            vec![var("name"), text(" is "), var("age"), text("!")]
        );
        assert_eq!(parse_pattern("Hello world"), vec![text("Hello world")]);
        assert_eq!(parse_pattern("%%{a} %{b}"), vec![text("%%{a} "), var("b")]);
        assert_eq!(parse_pattern("%{a"), vec![text("%{a")]);
        assert_eq!(
            parse_pattern("%{a\n} %{b}"),
            vec![text("%{a\n} "), var("b")]
        );
        assert_eq!(parse_pattern("%{}%{_}"), vec![var(""), var("_")]);
        assert_eq!(parse_pattern(""), Vec::<Segment>::new());
    }

    #[test]
    fn reads_properties_and_filters_missing_ones() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).format("%{name} is %{age} years old").to_list(),
            rhai: [r#"g.V().format("%{name} is %{age} years old")"#],
            assert: |res| assert_eq!(
                res,
                &[
                    "marko is 29 years old".into(),
                    "vadas is 27 years old".into(),
                    "josh is 32 years old".into(),
                    "peter is 35 years old".into(),
                ]
            ),
        );
    }

    #[test]
    fn a_stored_null_property_is_written_as_null() {
        let graph = || {
            let graph = GraphSource::tinkerpop_modern();
            graph
                .write()
                .traversal_mut()
                .v("1")
                .property("nick", ElementProperty::Null)
                .next()
                .unwrap();
            graph
        };
        test_dual!(
            graph: graph(),
            rust: |g| g.v(None).format("%{name} aka %{nick}").to_list(),
            rhai: [
                r#"g.V().format("%{name} aka %{nick}")"#,
                r#"g.v().format("%{name} aka %{nick}")"#,
            ],
            assert: |res| assert_eq!(res, &["marko aka null".into()]),
        );
        // An absent property still filters.
        test_dual!(
            graph: graph(),
            rust: |g| g.v(None).has_label("software").format("%{name} is %{age}").to_list(),
            rhai: [r#"g.V().hasLabel("software").format("%{name} is %{age}")"#],
            assert: |res| assert!(res.is_empty(), "{res:?}"),
        );
    }

    #[test]
    fn falls_back_to_a_path_label() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .inject(vec![1])
                .as_("age")
                .v(None)
                .has_label("software")
                .format("%{name} is %{age} years old")
                .to_list(),
            rhai: [r#"g.inject(1).as("age").V().hasLabel("software").format("%{name} is %{age} years old")"#],
            assert: |res| assert_eq!(res, &["lop is 1 years old".into(), "ripple is 1 years old".into()]),
        );
    }

    #[test]
    fn the_by_ring_fills_underscores() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v("1")
                .format("%{_} %{_} %{_}")
                .by(__::constant("hello"))
                .by(__::values("name"))
                .to_list(),
            rhai: [
                r#"g.V("1").format("%{_} %{_} %{_}").by(__.constant("hello")).by(__.values("name"))"#,
                r#"g.v("1").format("%{_} %{_} %{_}").by(__.constant("hello")).by("name")"#,
            ],
            assert: |res| assert_eq!(res, &["hello marko hello".into()]),
        );
    }

    #[test]
    fn reads_map_keys() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v("1")
                .project(vec!["name", "count"])
                .by("name")
                .by(__::both_e(None).count())
                .format("%{name} has %{count} connections")
                .to_list(),
            rhai: [r#"g.V("1").project("name", "count").by("name").by(__.bothE().count()).format("%{name} has %{count} connections")"#],
            assert: |res| assert_eq!(res, &["marko has 3 connections".into()]),
        );
    }

    #[test]
    fn underscore_without_by_is_the_value() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").values("name").format("<%{_}>").to_list(),
            rhai: [r#"g.V("1").values("name").format("<%{_}>")"#],
            assert: |res| assert_eq!(res, &["<marko>".into()]),
        );
    }

    #[test]
    fn profile_shows_pattern_and_path_labels() {
        let graph = GraphSource::tinkerpop_modern();
        let lock = graph.read();
        let metrics = lock
            .traversal()
            .v(None)
            .has_label("person")
            .as_("a")
            .values("name")
            .as_("p1")
            .select("a")
            .in_(vec!["knows"])
            .format("%{p1} knows %{name}")
            .profile()
            .unwrap();
        let text = metrics.to_string();
        assert!(text.contains("format(\"%{p1} knows %{name}\")"), "{text}");
    }
}
