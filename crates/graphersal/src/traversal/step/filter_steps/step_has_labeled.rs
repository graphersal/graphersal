//! `has(label, key, value)` / `has(label, key, P)`: TinkerPop's three-argument `has`.
//!
//! Pure builder sugar: it pushes the existing `has_label` step followed by `has`/`has_p`, so the
//! optimizer folds the label into the source step (`v(labels: [...])`) and `.profile()` shows the
//! ordinary step names.

use std::fmt::Debug;

use crate::graph::GraphStorage;
use crate::*;

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Gremlin `has(label, key, value)`: keeps elements with label `label` whose property `key`
    /// equals `value`. Equivalent to `.has_label(label).has(key, value)`.
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let graph = GraphSource::tinkerpop_modern();
    /// let lock = graph.read();
    /// let ages = lock
    ///     .traversal()
    ///     .v(None::<()>)
    ///     .has_labeled("person", "name", "marko")
    ///     .values("age")
    ///     .to_list()
    ///     .unwrap();
    /// assert_eq!(ages, vec![ElementProperty::Number(29.into())]);
    /// ```
    pub fn has_labeled<K: Into<PropKey>, T: Into<ElementProperty>>(
        self,
        label: &str,
        key: K,
        value: T,
    ) -> Self {
        self.has_label(label).has(key, value)
    }

    /// Gremlin `has(label, key, P)`: keeps elements with label `label` whose property `key`
    /// matches the predicate. Equivalent to `.has_label(label).has_p(key, predicate)`.
    pub fn has_labeled_p<
        B: Into<PValue> + Debug + Clone + Send + Sync + 'static,
        T: Into<P<B>> + Debug,
    >(
        self,
        label: &str,
        key: impl Into<PropKey>,
        predicate: T,
    ) -> Self {
        self.has_label(label).has_p(key, predicate)
    }
}

impl __ {
    /// Anonymous `has(label, key, value)`; see [`GraphTraversalSource::has_labeled`].
    pub fn has_labeled<'g, S: GraphStorage + 'g, T: Into<ElementProperty>>(
        label: &str,
        key: impl Into<PropKey>,
        value: T,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().has_labeled(label, key, value)
    }

    /// Anonymous `has(label, key, P)`; see [`GraphTraversalSource::has_labeled_p`].
    pub fn has_labeled_p<
        'g,
        S: GraphStorage + 'g,
        B: Into<PValue> + Debug + Clone + Send + Sync + 'static,
        T: Into<P<B>> + Debug,
    >(
        label: &str,
        key: impl Into<PropKey>,
        predicate: T,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().has_labeled_p(label, key, predicate)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Anonymous `has(label, key, value)`; see [`GraphTraversalSource::has_labeled`].
    pub fn has_labeled<T: Into<ElementProperty>>(
        self,
        label: &str,
        key: impl Into<PropKey>,
        value: T,
    ) -> Self {
        self.has_label(label).has(key, value)
    }

    /// Anonymous `has(label, key, P)`; see [`GraphTraversalSource::has_labeled_p`].
    pub fn has_labeled_p<
        B: Into<PValue> + Debug + Clone + Send + Sync + 'static,
        T: Into<P<B>> + Debug,
    >(
        self,
        label: &str,
        key: impl Into<PropKey>,
        predicate: T,
    ) -> Self {
        self.has_label(label).has_p(key, predicate)
    }
}

#[cfg(all(test, feature = "script"))]
mod tests {
    use super::*;

    #[test]
    fn three_argument_has_in_a_child_traversal() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v(None)
                .where_t(__::has_labeled("software", "lang", "java"))
                .count()
                .to_list(),
            rhai: [r#"g.V().where(__.has("software", "lang", "java")).count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(2)),
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v(None)
                .where_t(__::has_labeled_p("person", "age", P::Gt(30)))
                .values("name")
                .to_list(),
            rhai: [r#"g.V().where(__.has("person", "age", P.gt(30))).values("name")"#],
            assert: |res| assert_eq!(res, &["josh".into(), "peter".into()]),
        );
        // the label must match too: no software vertex has an age
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v(None)
                .where_t(__::has_labeled_p("software", "age", P::Gt(0)))
                .count()
                .to_list(),
            rhai: [r#"g.V().where(__.has("software", "age", P.gt(0))).count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(0)),
        );
    }
}
