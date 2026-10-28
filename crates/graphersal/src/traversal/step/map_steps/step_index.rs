//! `index()`: pairs every element of a traverser's collection with its position.

use crate::graph::GraphStorage;
use crate::traversal::step::local_scope::{LocalShape, classify};
use crate::*;

/// `index()` (TinkerPop `IndexStep` with its default list indexer); see
/// [`GraphTraversalSource::index`].
#[derive(Clone, Default)]
struct StepIndex {
    /// `with(WithOptions.indexer, WithOptions.map)`: a `{position: element}` map instead of
    /// `[element, position]` pairs.
    map: bool,
}

impl fmt::Display for StepIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.map {
            write!(f, "index().with(WithOptions.indexer, WithOptions.map)")
        } else {
            write!(f, "index()")
        }
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// `index()` (TinkerPop `IndexStep`, list indexer): maps a collection to the list of its
    /// `[element, position]` pairs, positions counted from 0
    /// (`g.V().values("name").fold().index()` → `[["marko", 0], ["vadas", 1], ...]`). A list
    /// and a path are indexed element by element, a map entry by entry; any other value
    /// (a number, a vertex, `null`) is a one-element collection
    /// (`g.V("1").values("age").index()` → `[[29, 0]]`).
    ///
    /// Followed by `with_option(WithOptions::Indexer, Some(WithOptions::Map))` (TinkerPop's
    /// `with(WithOptions.indexer, WithOptions.map)`) it builds a `{position: element}` map
    /// instead. Details: `book/src/traversal/steps/index.md`.
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let graph = GraphSource::tinkerpop_modern();
    /// let lock = graph.read();
    /// let pairs = lock.traversal().v("1").values("age").index().next().unwrap().unwrap();
    /// assert_eq!(
    ///     pairs,
    ///     ElementProperty::Array(vec![ElementProperty::Array(vec![29i64.into(), 0i64.into()])])
    /// );
    /// ```
    pub fn index(self) -> Self {
        self.push_step(GraphStep::Map("index", Box::new(StepIndex::default())))
    }
}

impl __ {
    /// Anonymous `index()`; see [`GraphTraversalSource::index`].
    pub fn index<'g, S: GraphStorage + 'g>() -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().index()
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// `index()`; see [`GraphTraversalSource::index`].
    pub fn index(self) -> Self {
        self.push_step(GraphStep::Map("index", Box::new(StepIndex::default())))
    }
}

/// The `[element, position]` pairs of `items`, or with `map` the `{position: element}` map.
fn indexed<S: GraphStorage>(
    items: impl Iterator<Item = TraverserValue<S>>,
    map: bool,
) -> TraverserValue<S> {
    if map {
        return TraverserValue::Object(
            items
                .enumerate()
                .map(|(position, item)| {
                    let position = i64::try_from(position).unwrap_or(i64::MAX);
                    (
                        TraverserValue::Computed(ElementProperty::Number(position.into()), None),
                        item,
                    )
                })
                .collect(),
        );
    }
    TraverserValue::Array(
        items
            .enumerate()
            .map(|(position, item)| {
                let position = i64::try_from(position).unwrap_or(i64::MAX);
                TraverserValue::Array(vec![
                    item,
                    TraverserValue::Computed(ElementProperty::Number(position.into()), None),
                ])
            })
            .collect(),
    )
}

impl<'g, S: GraphStorage + 'g> GraphStepMap<'g, S> for StepIndex {
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<Traverser<S>> {
        let path = source.path;
        let bulk = source.bulk;
        let sack = source.sack;
        let value = match classify(context, source.value)? {
            LocalShape::List(items) => indexed(items.into_iter(), self.map),
            LocalShape::Map(entries) => indexed(
                entries
                    .into_iter()
                    .map(|entry| TraverserValue::Entry(Box::new(entry))),
                self.map,
            ),
            LocalShape::Path(path) => indexed(path.objects().cloned(), self.map),
            LocalShape::Scalar(value) => indexed(std::iter::once(value), self.map),
        };
        Ok(Traverser::with_parts(value, path, bulk, sack))
    }

    /// `with(WithOptions.indexer, WithOptions.list | WithOptions.map)`.
    fn push_with_option(&mut self, key: WithOptions, value: Option<WithOptions>) -> Option<String> {
        if key != WithOptions::Indexer {
            return Some(format!(
                "{key} is not an option of index(), which takes WithOptions.indexer"
            ));
        }
        match value {
            Some(WithOptions::List) => self.map = false,
            Some(WithOptions::Map) => self.map = true,
            _ => {
                return Some(
                    "WithOptions.indexer takes WithOptions.list or WithOptions.map".to_string(),
                );
            }
        }
        None
    }

    fn clone_box(&self) -> Box<dyn GraphStepMap<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "script")]
    fn pair(item: ElementProperty, position: i64) -> ElementProperty {
        ElementProperty::Array(vec![item, position.into()])
    }

    #[cfg(feature = "script")]
    #[test]
    fn indexes_a_list() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v(None)
                .has_label("software")
                .values("name")
                .fold()
                .order_scoped(crate::value::Scope::Local)
                .index()
                .to_list(),
            rhai: [
                r#"g.V().hasLabel("software").values("name").fold().order(Scope.local).index()"#,
                r#"g.v().has_label("software").values("name").fold().order(Scope.local).index()"#,
            ],
            assert: |res| assert_eq!(
                res,
                &[ElementProperty::Array(vec![pair("lop".into(), 0), pair("ripple".into(), 1)])]
            ),
        );
    }

    /// A non-collection is a one-element collection.
    #[cfg(feature = "script")]
    #[test]
    fn indexes_a_scalar_and_an_element() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").values("age").index().unfold().unfold().to_list(),
            rhai: [r#"g.V("1").values("age").index().unfold().unfold()"#],
            assert: |res| assert_eq!(res, &[29i64.into(), 0i64.into()]),
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).has_label("software").index().unfold().count().to_list(),
            rhai: [r#"g.V().hasLabel("software").index().unfold().count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(2)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn indexes_map_entries() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").project(vec!["a", "b"]).by("name").by("age").index().unfold().count().to_list(),
            rhai: [r#"g.V("1").project("a", "b").by("name").by("age").index().unfold().count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(2)),
        );
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let lock = graph.read();
        let result = lock.traversal().v(None).index().profile().unwrap();
        assert!(result.to_string().contains("index()"), "{result}");
    }
}
