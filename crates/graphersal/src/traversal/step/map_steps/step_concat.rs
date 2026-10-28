//! `concat(string...)` / `concat(traversal...)`: the incoming string with further strings
//! appended.

use compact_str::CompactString;

use super::list_function::kind_name;
use crate::graph::GraphStorage;
use crate::traversal::optimizer::path_analysis::{PathAnalysis, PathRequirement};
use crate::traversal::step::local_scope::is_null;
use crate::*;

/// One argument of `concat()`.
enum ConcatPart<'g, S: GraphStorage + 'g> {
    /// A string constant.
    Text(CompactString),
    /// A `null` constant: skipped.
    Null,
    /// A constant that is neither a string nor `null`: rejected before execution by
    /// [`GraphStepMap::plan_check`].
    Invalid(ElementProperty),
    /// A child traversal; its first result is appended.
    Traversal(AnonymousTraversal<'g, S>),
}

impl<'g, S: GraphStorage + 'g> Clone for ConcatPart<'g, S> {
    fn clone(&self) -> Self {
        match self {
            ConcatPart::Text(text) => ConcatPart::Text(text.clone()),
            ConcatPart::Null => ConcatPart::Null,
            ConcatPart::Invalid(value) => ConcatPart::Invalid(value.clone()),
            ConcatPart::Traversal(traversal) => ConcatPart::Traversal(traversal.clone()),
        }
    }
}

/// `concat()` (TinkerPop `ConcatStep`); see [`GraphTraversalSource::concat`] for the semantics.
struct StepConcat<'g, S: GraphStorage + 'g> {
    parts: Vec<ConcatPart<'g, S>>,
}

impl<'g, S: GraphStorage + 'g> Clone for StepConcat<'g, S> {
    fn clone(&self) -> Self {
        Self {
            parts: self.parts.clone(),
        }
    }
}

impl<'g, S: GraphStorage + 'g> fmt::Display for StepConcat<'g, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let parts: Vec<String> = self
            .parts
            .iter()
            .map(|part| match part {
                ConcatPart::Text(text) => crate::value::literal::rhai_string(text),
                ConcatPart::Null => "()".to_string(),
                ConcatPart::Invalid(value) => crate::value::literal::rhai_literal(value),
                ConcatPart::Traversal(traversal) => traversal.to_string(),
            })
            .collect();
        write!(f, "concat({})", parts.join(", "))
    }
}

fn concat_step<'g, S: GraphStorage + 'g>(parts: Vec<ConcatPart<'g, S>>) -> GraphStep<'g, S> {
    GraphStep::Map("concat", Box::new(StepConcat { parts }))
}

/// The constant parts of `concat(string...)`.
fn constant_parts<'g, S: GraphStorage + 'g, I>(strings: I) -> Vec<ConcatPart<'g, S>>
where
    I: IntoIterator,
    I::Item: Into<ElementProperty>,
{
    strings
        .into_iter()
        .map(|value| match value.into() {
            ElementProperty::String(text) => ConcatPart::Text(text),
            ElementProperty::Null => ConcatPart::Null,
            other => ConcatPart::Invalid(other),
        })
        .collect()
}

/// The child-traversal parts of `concat(traversal...)`.
fn traversal_parts<'g, S: GraphStorage + 'g>(
    traversals: Vec<AnonymousTraversal<'g, S>>,
) -> Vec<ConcatPart<'g, S>> {
    traversals.into_iter().map(ConcatPart::Traversal).collect()
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// `concat(string...)` (TinkerPop `ConcatStep`): the incoming string with every argument
    /// appended, in order (`g.inject("a").concat(vec!["b", "c"])` → `"abc"`).
    ///
    /// `null` is skipped wherever it occurs: a `null` incoming value contributes nothing
    /// (`inject(null, "a").concat(null, "b")` → `"b"`, `"ab"`), and the result is `null` only
    /// when the incoming value and every argument are `null`. An incoming value that is neither
    /// a string nor `null` is a cast error (convert it with `as_string()` first); an argument
    /// that is neither a string nor `null` (`ElementProperty::Null`) is rejected before
    /// execution. See [`concat_traversal`](Self::concat_traversal) for child traversals.
    /// Details: `book/src/traversal/steps/concat.md`.
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let graph = GraphSource::tinkerpop_modern();
    /// let lock = graph.read();
    /// let text = lock.traversal().v("1").values("name").concat(vec![" is ", "here"]).next().unwrap().unwrap();
    /// assert_eq!(text, ElementProperty::from("marko is here"));
    /// ```
    pub fn concat<I>(self, strings: I) -> Self
    where
        I: IntoIterator,
        I::Item: Into<ElementProperty>,
    {
        self.push_step(concat_step(constant_parts(strings)))
    }

    /// `concat(traversal...)`: like [`concat`](Self::concat), but each argument is a child
    /// traversal run on the incoming traverser (path included) whose **first** result is
    /// appended (`inject("a").concat(__.inject("c"))` → `"aa"`, because the child's first result
    /// is the incoming `"a"`). The result must be a string or `null` (skipped); a child that
    /// yields nothing, or a value that is not a string, is a `ValueError::StringFunctionInput`.
    pub fn concat_traversal(self, traversals: Vec<AnonymousTraversal<'g, S>>) -> Self {
        self.push_step(concat_step(traversal_parts(traversals)))
    }
}

impl __ {
    /// Anonymous `concat(string...)`; see [`GraphTraversalSource::concat`].
    pub fn concat<'g, S: GraphStorage + 'g, I>(strings: I) -> AnonymousTraversal<'g, S>
    where
        I: IntoIterator,
        I::Item: Into<ElementProperty>,
    {
        AnonymousTraversal::new().concat(strings)
    }

    /// Anonymous `concat(traversal...)`; see [`GraphTraversalSource::concat_traversal`].
    pub fn concat_traversal<'g, S: GraphStorage + 'g>(
        traversals: Vec<AnonymousTraversal<'g, S>>,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().concat_traversal(traversals)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// `concat(string...)`; see [`GraphTraversalSource::concat`].
    pub fn concat<I>(self, strings: I) -> Self
    where
        I: IntoIterator,
        I::Item: Into<ElementProperty>,
    {
        self.push_step(concat_step(constant_parts(strings)))
    }

    /// `concat(traversal...)`; see [`GraphTraversalSource::concat_traversal`].
    pub fn concat_traversal(self, traversals: Vec<AnonymousTraversal<'g, S>>) -> Self {
        self.push_step(concat_step(traversal_parts(traversals)))
    }
}

/// The error of a `concat()` argument it cannot take.
fn argument_error(operand: &'static str, actual: String) -> Box<TraverserError> {
    Box::new(TraverserError::Value(ValueError::StringFunctionInput {
        step: "concat",
        operand,
        expected: "string or null",
        actual,
    }))
}

impl<'g, S: GraphStorage + 'g> GraphStepMap<'g, S> for StepConcat<'g, S> {
    /// Never called by the executor: `may_filter()` is true (so a downstream fused `count()`
    /// still runs the step and raises its errors), and it calls `process_filter`.
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<Traverser<S>> {
        self.process_filter(context, source)?.ok_or_else(|| {
            Box::new(TraverserError::QueryExecution {
                query: self.to_string(),
                reason: "concat() never drops a traverser".to_string(),
            })
        })
    }

    fn process_filter(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<Option<Traverser<S>>> {
        let mut out = String::new();
        // Every part is checked against `traversal.max_string_bytes` before it is appended, so a
        // concatenation that doubles its input (`repeat(__.concat(__.identity()))`) stops at
        // the limit instead of allocating past it.
        let max = context.options.max_string_bytes;
        let fits = |out: &String, extra: usize| {
            crate::traversal::context::check_string_bytes(out.len().saturating_add(extra), max)
        };
        // Whether any part (the incoming value included) was not `null`.
        let mut any = false;
        if !is_null(context, &source.value)? {
            match Traverser::as_string_ref(context, &source.value)? {
                Some(text) => {
                    fits(&out, text.len())?;
                    out.push_str(&text)
                }
                None => {
                    return Err(Box::new(TraverserError::cast_exception_str(
                        &["string"],
                        Traverser::observed_type_name(context, &source.value),
                    )));
                }
            }
            any = true;
        }
        for part in &self.parts {
            match part {
                ConcatPart::Text(text) => {
                    fits(&out, text.len())?;
                    out.push_str(text);
                    any = true;
                }
                ConcatPart::Null => {}
                ConcatPart::Invalid(value) => {
                    return Err(argument_error(
                        "argument",
                        crate::graph::schema::value_type_name(value),
                    ));
                }
                ConcatPart::Traversal(traversal) => {
                    let Some(first) = traversal.first(context, &source)? else {
                        return Err(argument_error(
                            "traversal argument",
                            "no result (the traversal yielded nothing)".to_string(),
                        ));
                    };
                    if is_null(context, &first.value)? {
                        continue;
                    }
                    let text = match &first.value {
                        TraverserValue::Computed(_, _)
                        | TraverserValue::VertexProperty(_, _, _)
                        | TraverserValue::EdgeProperty(_, _, _) => {
                            Traverser::as_string_ref(context, &first.value)?
                        }
                        // Elements, paths and containers are never strings.
                        TraverserValue::Vertex(_)
                        | TraverserValue::Edge(_)
                        | TraverserValue::Path(_)
                        | TraverserValue::Record { .. }
                        | TraverserValue::Object(_)
                        | TraverserValue::PathProperty(_)
                        | TraverserValue::Entry(_)
                        | TraverserValue::Array(_)
                        | TraverserValue::Schema(_)
                        | TraverserValue::Graph(_) => None,
                    };
                    match text {
                        Some(text) => {
                            fits(&out, text.len())?;
                            out.push_str(&text)
                        }
                        None => {
                            return Err(argument_error(
                                "traversal argument",
                                kind_name(context, &first.value),
                            ));
                        }
                    }
                    any = true;
                }
            }
        }
        let value = if any {
            ElementProperty::String(out.into())
        } else {
            ElementProperty::Null
        };
        Ok(Some(Traverser::with_parts(
            TraverserValue::Computed(value, None),
            source.path,
            source.bulk,
            source.sack,
        )))
    }

    fn may_filter(&self) -> bool {
        true
    }

    fn plan_check(&self) -> TraverserResult<()> {
        for part in &self.parts {
            if let ConcatPart::Invalid(value) = part {
                return Err(argument_error(
                    "argument",
                    crate::graph::schema::value_type_name(value),
                ));
            }
        }
        Ok(())
    }

    fn analyze_nested_paths(
        &mut self,
        analysis: &PathAnalysis,
        _downstream: &PathRequirement,
    ) -> PathRequirement {
        // The child traversals run on the incoming traverser, path included; their output does
        // not continue the path.
        self.parts
            .iter_mut()
            .fold(PathRequirement::None, |need, part| match part {
                ConcatPart::Traversal(traversal) => {
                    need.union(traversal.analyze_paths(analysis, PathRequirement::None))
                }
                ConcatPart::Text(_) | ConcatPart::Null | ConcatPart::Invalid(_) => need,
            })
    }

    fn child_traversals_mut(&mut self) -> Vec<&mut AnonymousTraversal<'g, S>> {
        self.parts
            .iter_mut()
            .filter_map(|part| match part {
                ConcatPart::Traversal(traversal) => Some(traversal),
                ConcatPart::Text(_) | ConcatPart::Null | ConcatPart::Invalid(_) => None,
            })
            .collect()
    }

    fn clone_box(&self) -> Box<dyn GraphStepMap<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_constants() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.inject(vec!["a", "b"]).concat(vec!["c", "d"]).to_list(),
            rhai: [r#"g.inject("a", "b").concat("c", "d")"#],
            assert: |res| assert_eq!(res, &["acd".into(), "bcd".into()]),
        );
    }

    #[test]
    fn no_argument_keeps_the_string() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.inject(vec!["a"]).concat(Vec::<&str>::new()).to_list(),
            rhai: [r#"g.inject("a").concat()"#],
            assert: |res| assert_eq!(res, &["a".into()]),
        );
    }

    #[test]
    fn null_is_skipped_and_all_null_is_null() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .inject(vec![ElementProperty::Null, "a".into()])
                .concat(vec![ElementProperty::Null, "b".into()])
                .to_list(),
            rhai: [r#"g.inject((), "a").concat((), "b")"#],
            assert: |res| assert_eq!(res, &["b".into(), "ab".into()]),
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .inject(vec![ElementProperty::Null])
                .concat(vec![ElementProperty::Null])
                .to_list(),
            rhai: [r#"g.inject(()).concat(())"#],
            assert: |res| assert_eq!(res, &[ElementProperty::Null]),
        );
    }

    #[test]
    fn a_traversal_argument_is_its_first_result_on_the_incoming_traverser() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v(None)
                .has_label("software")
                .as_("a")
                .values("name")
                .concat(vec![" uses "])
                .concat_traversal(vec![__::select("a").values("lang")])
                .to_list(),
            rhai: [
                r#"g.V().hasLabel("software").as("a").values("name").concat(" uses ").concat(__.select("a").values("lang"))"#,
                r#"g.v().has_label("software").as_("a").values("name").concat(" uses ").concat(__.select("a").values("lang"))"#,
            ],
            assert: |res| assert_eq!(res, &["lop uses java".into(), "ripple uses java".into()]),
        );
    }

    #[test]
    fn several_traversals_in_order() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v("1")
                .out_e(vec!["created"])
                .as_("e")
                .v("1")
                .values("name")
                .concat_traversal(vec![__::select("e").label(), __::select("e").in_v().values("name")])
                .to_list(),
            rhai: [r#"g.V("1").outE("created").as("e").V("1").values("name").concat(__.select("e").label(), __.select("e").inV().values("name"))"#],
            assert: |res| assert_eq!(res, &["markocreatedlop".into()]),
        );
    }

    #[test]
    fn rejects_non_strings() {
        test_dual_err!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.inject(vec![vec!["a", "b"]]).concat(vec!["d"]).to_list(),
            rhai: [r#"g.inject(["a", "b"]).concat("d")"#],
            assert: |err| assert!(err.to_string().contains("string"), "{err}"),
        );
        test_dual_err!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.inject(vec!["a"]).concat(vec![1]).to_list(),
            rhai: [r#"g.inject("a").concat(1)"#],
            assert: |err| assert!(err.to_string().contains("concat()"), "{err}"),
        );
        test_dual_err!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("3").as_("x").values("name").concat_traversal(vec![__::select("x").values("age")]).to_list(),
            rhai: [r#"g.V("3").as("x").values("name").concat(__.select("x").values("age"))"#],
            assert: |err| assert!(err.to_string().contains("no result"), "{err}"),
        );
        test_dual_err!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").as_("x").values("name").concat_traversal(vec![__::select("x").values("age")]).to_list(),
            rhai: [r#"g.V("1").as("x").values("name").concat(__.select("x").values("age"))"#],
            assert: |err| assert!(err.to_string().contains("int64"), "{err}"),
        );
    }

    #[test]
    fn profile_names_the_step() {
        let graph = GraphSource::tinkerpop_modern();
        let lock = graph.read();
        let metrics = lock
            .traversal()
            .inject(vec!["a"])
            .concat(vec!["b"])
            .profile()
            .unwrap();
        assert!(metrics.to_string().contains("concat(\"b\")"), "{metrics}");
    }
}
