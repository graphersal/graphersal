//! `reverse()`: a string or a list reversed.

use super::list_function::{Kind, kind_of, list_items};
use crate::graph::GraphStorage;
use crate::traversal::step::local_scope::is_null;
use crate::*;

/// `reverse()` (TinkerPop `ReverseStep`); see [`GraphTraversalSource::reverse`] for the
/// semantics.
#[derive(Clone, Default)]
struct StepReverse;

impl fmt::Display for StepReverse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "reverse()")
    }
}

fn reverse_step<'g, S: GraphStorage + 'g>() -> GraphStep<'g, S> {
    GraphStep::Map("reverse", Box::new(StepReverse))
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// `reverse()` (TinkerPop `ReverseStep`): a string with its characters in reverse order
    /// (`"abc"` → `"cba"`), or a list (a `fold()`, a `path()`, a list-valued property) with its
    /// elements in reverse order; a path becomes a list. Every other value, `null`, numbers,
    /// maps and graph elements included, passes through unchanged. Characters are Unicode
    /// scalar values. Details: `book/src/traversal/steps/reverse.md`.
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let graph = GraphSource::tinkerpop_modern();
    /// let lock = graph.read();
    /// let name = lock.traversal().v("1").values("name").reverse().next().unwrap().unwrap();
    /// assert_eq!(name, ElementProperty::from("okram"));
    /// ```
    pub fn reverse(self) -> Self {
        self.push_step(reverse_step())
    }
}

impl __ {
    /// Anonymous `reverse()`; see [`GraphTraversalSource::reverse`].
    pub fn reverse<'g, S: GraphStorage + 'g>() -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().reverse()
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// `reverse()`; see [`GraphTraversalSource::reverse`].
    pub fn reverse(self) -> Self {
        self.push_step(reverse_step())
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepMap<'g, S> for StepReverse {
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
        if is_null(context, &value)? {
            return Ok(Traverser::with_parts(value, path, bulk, sack));
        }
        let value = match kind_of(context, &value)? {
            Kind::List => {
                let mut items = list_items(context, value)?;
                items.reverse();
                TraverserValue::Array(items)
            }
            Kind::Map | Kind::Other => {
                let reversed = match &value {
                    TraverserValue::Computed(_, _)
                    | TraverserValue::VertexProperty(_, _, _)
                    | TraverserValue::EdgeProperty(_, _, _) => {
                        Traverser::as_string_ref(context, &value)?
                            .map(|text| text.chars().rev().collect::<String>())
                    }
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
                match reversed {
                    Some(text) => {
                        context.check_string_bytes(text.len())?;
                        TraverserValue::Computed(ElementProperty::String(text.into()), None)
                    }
                    None => value,
                }
            }
        };
        Ok(Traverser::with_parts(value, path, bulk, sack))
    }

    fn clone_box(&self) -> Box<dyn GraphStepMap<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

#[cfg(all(test, feature = "script"))]
mod tests {
    use super::*;

    #[test]
    fn reverses_strings_and_keeps_null() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .inject(vec![ElementProperty::from("feature"), "test one".into(), ElementProperty::Null])
                .reverse()
                .to_list(),
            rhai: [r#"g.inject("feature", "test one", ()).reverse()"#],
            assert: |res| assert_eq!(res, &["erutaef".into(), "eno tset".into(), ElementProperty::Null]),
        );
    }

    #[test]
    fn numbers_pass_through() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").values("age").reverse().to_list(),
            rhai: [r#"g.V("1").values("age").reverse()"#],
            assert: |res| assert_eq!(res, &[29.into()]),
        );
    }

    #[test]
    fn reverses_lists_and_paths() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.inject(vec![ElementProperty::Array(vec![ElementProperty::from(3), "three".into()])]).reverse().to_list(),
            rhai: [r#"g.inject([3, "three"]).reverse()"#],
            assert: |res| assert_eq!(res, &[ElementProperty::Array(vec![ElementProperty::from("three"), 3.into()])]),
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").out(vec!["created"]).path().by("name").reverse().to_list(),
            rhai: [
                r#"g.V("1").out("created").path().by("name").reverse()"#,
                r#"g.v("1").out("created").path().by("name").reverse()"#,
            ],
            assert: |res| assert_eq!(res, &[ElementProperty::from(vec!["lop", "marko"])]),
        );
    }

    #[test]
    fn vertices_and_maps_pass_through() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").reverse().values("name").to_list(),
            rhai: [r#"g.V("1").reverse().values("name")"#],
            assert: |res| assert_eq!(res, &["marko".into()]),
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").project(vec!["a", "b"]).by("name").by("age").reverse().select(vec!["a"]).to_list(),
            rhai: [r#"g.V("1").project("a", "b").by("name").by("age").reverse().select("a")"#],
            assert: |res| assert_eq!(res, &["marko".into()]),
        );
    }
}
