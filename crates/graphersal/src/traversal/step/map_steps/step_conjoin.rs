//! `conjoin(delimiter)`: the elements of a list joined into one string.

use compact_str::CompactString;

use super::list_function::{Kind, input_error, kind_name, kind_of, list_items};
use crate::graph::GraphStorage;
use crate::traversal::step::flatmap_steps::step_cast::element_text;
use crate::traversal::step::local_scope::is_null;
use crate::*;

/// `conjoin(delimiter)` (TinkerPop `ConjoinStep`): joins the elements of the list a traverser
/// holds into one string, `delimiter` between two elements. `null` elements are skipped (no
/// delimiter is written for them); every other element is written as its string form, the one
/// `as_string()` gives (`27`, `v[1]`). A nested list or a map has no string form here (TinkerPop
/// writes Java's `toString`): it is a cast error. A `null` or non-list incoming value is a
/// `ValueError::ListFunctionInput`.
#[derive(Clone)]
struct StepConjoin {
    delimiter: CompactString,
}

impl fmt::Display for StepConjoin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let delimiter = crate::value::literal::rhai_string(&self.delimiter);
        write!(f, "conjoin({delimiter})")
    }
}

fn conjoin_step<'g, S: GraphStorage + 'g>(delimiter: CompactString) -> GraphStep<'g, S> {
    GraphStep::Map("conjoin", Box::new(StepConjoin { delimiter }))
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// `conjoin(delimiter)` (TinkerPop `ConjoinStep`): the elements of the list a traverser holds
    /// (a `fold()`, a `path()`, a list-valued property) joined into one string with `delimiter`
    /// between them. `null` elements are skipped; numbers, booleans, UUIDs and graph elements
    /// are written as `as_string()` writes them; an empty list gives `""`. A nested list or map is
    /// a cast error (Graphersal writes no Java `toString` text). A `null` or non-list incoming
    /// value is a `ValueError::ListFunctionInput`. Details:
    /// `book/src/traversal/steps/conjoin.md`.
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let graph = GraphSource::tinkerpop_modern();
    /// let lock = graph.read();
    /// let joined = lock.traversal().v(None).values("age").order().fold().conjoin(";").next().unwrap().unwrap();
    /// assert_eq!(joined, ElementProperty::from("27;29;32;35"));
    /// ```
    pub fn conjoin(self, delimiter: impl Into<CompactString>) -> Self {
        self.push_step(conjoin_step(delimiter.into()))
    }
}

impl __ {
    /// Anonymous `conjoin(delimiter)`; see [`GraphTraversalSource::conjoin`].
    pub fn conjoin<'g, S: GraphStorage + 'g>(
        delimiter: impl Into<CompactString>,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().conjoin(delimiter)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// `conjoin(delimiter)`; see [`GraphTraversalSource::conjoin`].
    pub fn conjoin(self, delimiter: impl Into<CompactString>) -> Self {
        self.push_step(conjoin_step(delimiter.into()))
    }
}

/// The string form of one non-null element.
pub(super) fn element_string<S: GraphStorage>(
    context: &TraverserContext<'_, S>,
    item: &TraverserValue<S>,
) -> TraverserResult<String> {
    if let Some(ElementProperty::String(text)) = element_text(context, item)? {
        return Ok(text.to_string());
    }
    let value = item.into_node_property(context.storage)?;
    match value.cast(&ValueType::String)? {
        ElementProperty::String(text) => Ok(text.to_string()),
        other => Err(Box::new(TraverserError::cast_exception_target(
            "string",
            crate::graph::schema::value_type_name(&other),
            None,
            None,
        ))),
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepMap<'g, S> for StepConjoin {
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
                reason: "conjoin() never drops a traverser".to_string(),
            })
        })
    }

    fn process_filter(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<Option<Traverser<S>>> {
        if kind_of(context, &source.value)? != Kind::List {
            return Err(input_error(
                "conjoin",
                "incoming value",
                "list",
                kind_name(context, &source.value),
            ));
        }
        let Traverser {
            value,
            path,
            bulk,
            sack,
        } = source;
        let mut joined = String::new();
        let mut first = true;
        // Checked against `traversal.max_string_bytes` before every append: a long list with a
        // long delimiter multiplies the length.
        let max = context.options.max_string_bytes;
        for item in list_items(context, value)? {
            if is_null(context, &item)? {
                continue;
            }
            let text = element_string(context, &item)?;
            let delimiter = if first { "" } else { self.delimiter.as_str() };
            crate::traversal::context::check_string_bytes(
                joined
                    .len()
                    .saturating_add(delimiter.len())
                    .saturating_add(text.len()),
                max,
            )?;
            joined.push_str(delimiter);
            first = false;
            joined.push_str(&text);
        }
        Ok(Some(Traverser::with_parts(
            TraverserValue::Computed(ElementProperty::from(joined), None),
            path,
            bulk,
            sack,
        )))
    }

    fn may_filter(&self) -> bool {
        true
    }

    /// Builds a new string.
    fn materializes(&self) -> bool {
        true
    }

    fn clone_box(&self) -> Box<dyn GraphStepMap<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

#[cfg(all(test, feature = "script"))]
mod tests {
    use super::*;

    #[test]
    fn joins_and_skips_nulls() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .inject(vec![ElementProperty::Array(vec![
                    ElementProperty::Null,
                    "a".into(),
                    ElementProperty::Null,
                    "b".into(),
                ])])
                .conjoin("+")
                .to_list(),
            rhai: [r#"g.inject([(), "a", (), "b"]).conjoin("+")"#],
            assert: |res| assert_eq!(res, &["a+b".into()]),
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).values("nonexistent").fold().conjoin(";").to_list(),
            rhai: [r#"g.V().values("nonexistent").fold().conjoin(";")"#],
            assert: |res| assert_eq!(res, &["".into()]),
        );
    }

    #[test]
    fn numbers_paths_and_vertices() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).values("age").order().fold().conjoin(";").to_list(),
            rhai: [
                r#"g.V().values("age").order().fold().conjoin(";")"#,
                r#"g.v().values("age").order().fold().conjoin(";")"#,
            ],
            assert: |res| assert_eq!(res, &["27;29;32;35".into()]),
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").out("created").path().conjoin("-").to_list(),
            rhai: [r#"g.V("1").out("created").path().conjoin("-")"#],
            assert: |res| assert_eq!(res, &["v[1]-v[3]".into()]),
        );
    }

    #[test]
    fn rejects_non_lists_and_nested_lists() {
        test_dual_err!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).values("name").conjoin("1").to_list(),
            rhai: [r#"g.V().values("name").conjoin("1")"#],
            assert: |err| assert!(err.contains("The conjoin() step takes a list as its incoming value, but got string"), "{err}"),
        );
        test_dual_err!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.inject(vec![ElementProperty::Null]).conjoin("1").to_list(),
            rhai: [r#"g.inject(()).conjoin("1")"#],
            assert: |err| assert!(err.contains("but got null"), "{err}"),
        );
        test_dual_err!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .inject(vec![ElementProperty::Array(vec![ElementProperty::from(vec![1, 2])])])
                .conjoin(",")
                .to_list(),
            rhai: [r#"g.inject([[1, 2]]).conjoin(",")"#],
            assert: |err| assert!(err.to_lowercase().contains("cast"), "{err}"),
        );
    }
}
