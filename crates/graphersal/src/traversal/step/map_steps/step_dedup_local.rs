//! `dedup(Scope.local)`: removes duplicate elements from the list each traverser holds.

use crate::graph::GraphStorage;
use crate::traversal::step::flatbarrier_steps::step_dedup::DedupKey;
use crate::traversal::step::local_scope::stored_value;
use crate::value::Scope;
use crate::*;

/// `dedup(local)` (TinkerPop `DedupLocalStep`): keeps the first occurrence of each distinct
/// element of a list, in list order. Elements compare like the global `dedup()`'s traversers:
/// vertices/edges by identity, everything else by materialized value; the kept elements stay
/// what they were (a folded vertex list stays a list of vertex handles).
///
/// Upstream only deduplicates an `Iterable`; a map is not one, so a map — like any other value —
/// passes through unchanged.
#[derive(Clone)]
struct StepDedupLocal;

impl fmt::Display for StepDedupLocal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "dedup(Scope.local)")
    }
}

/// The physical step of `dedup(scope, labels)`: `dedup(local)` ignores the labels (as TinkerPop
/// does), `dedup(global)` with labels keys by the labelled path objects, without labels it is
/// plain `dedup()` (`None`).
fn dedup_step<'g, S: GraphStorage + 'g>(
    scope: Scope,
    labels: Vec<String>,
) -> Option<GraphStep<'g, S>> {
    match scope {
        Scope::Local => Some(GraphStep::Map("dedup", Box::new(StepDedupLocal))),
        Scope::Global if labels.is_empty() => None,
        Scope::Global => Some(
            crate::traversal::step::flatbarrier_steps::step_dedup::dedup_labels_step(
                labels.into_iter().map(CompactString::from).collect(),
            ),
        ),
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// `dedup(Scope)`: `Scope::Global` is [`dedup()`](Self::dedup) (deduplicates the stream);
    /// `Scope::Local` removes duplicate elements from the list each traverser holds, keeping the
    /// first occurrence of each, in order (`g.V().out().in().values("name").fold()
    /// .dedup(Scope.local)` → `["marko", "josh", "peter"]`). A map or any other non-list value
    /// passes through unchanged.
    ///
    /// ```
    /// use graphersal::prelude::*;
    ///
    /// let graph = TraversalGraph::tinkerpop_modern();
    /// let g = GraphTraversalSource::new(&graph);
    /// let names = g.v(None).out(None).values("name").fold().dedup_scoped(Scope::Local).to_list();
    /// assert!(names.is_ok());
    /// ```
    pub fn dedup_scoped(self, scope: Scope) -> Self {
        self.dedup_scoped_labels(scope, Vec::<String>::new())
    }

    /// `dedup(Scope, labels…)`. With `Scope::Local` the labels are accepted and **ignored**, as
    /// in TinkerPop, whose `dedup(Scope, String...)` hands them only to the global step; the
    /// result is [`dedup_scoped(Scope::Local)`](Self::dedup_scoped). With `Scope::Global` and at
    /// least one label (TinkerPop's `dedup("a", "b")`) the stream is deduplicated by the list of
    /// the objects last labelled so on each traverser's path, each projected by the `by()` if
    /// there is one; a traverser whose `by()` yields nothing is filtered out. With no label it
    /// is [`dedup()`](Self::dedup).
    ///
    /// **Deviation from TinkerPop:** a traverser missing one of the labels is filtered out too,
    /// where TinkerPop raises an error (`book/src/traversal/steps/dedup.md`).
    ///
    /// ```
    /// use graphersal::prelude::*;
    ///
    /// let graph = TraversalGraph::tinkerpop_modern();
    /// let g = GraphTraversalSource::new(&graph);
    /// let pairs = g
    ///     .v(None)
    ///     .as_("a")
    ///     .both(None)
    ///     .as_("b")
    ///     .dedup_scoped_labels(Scope::Global, ["a", "b"])
    ///     .by(By::Label)
    ///     .count()
    ///     .next();
    /// assert_eq!(pairs.unwrap().and_then(|c| c.as_i64()), Some(3));
    /// ```
    pub fn dedup_scoped_labels<I, L>(self, scope: Scope, labels: I) -> Self
    where
        I: IntoIterator<Item = L>,
        L: Into<String>,
    {
        let labels = labels.into_iter().map(Into::into).collect();
        match dedup_step(scope, labels) {
            Some(step) => self.push_step(step),
            None => self.dedup(),
        }
    }
}

impl __ {
    /// Anonymous `dedup(Scope)`; see [`GraphTraversalSource::dedup_scoped`].
    pub fn dedup_scoped<'g, S: GraphStorage + 'g>(scope: Scope) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().dedup_scoped(scope)
    }

    /// Anonymous `dedup(Scope, labels…)`; see [`GraphTraversalSource::dedup_scoped_labels`].
    pub fn dedup_scoped_labels<'g, S: GraphStorage + 'g, I, L>(
        scope: Scope,
        labels: I,
    ) -> AnonymousTraversal<'g, S>
    where
        I: IntoIterator<Item = L>,
        L: Into<String>,
    {
        AnonymousTraversal::new().dedup_scoped_labels(scope, labels)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// `dedup(Scope)`; see [`GraphTraversalSource::dedup_scoped`].
    pub fn dedup_scoped(self, scope: Scope) -> Self {
        self.dedup_scoped_labels(scope, Vec::<String>::new())
    }

    /// `dedup(Scope, labels…)`; see [`GraphTraversalSource::dedup_scoped_labels`].
    pub fn dedup_scoped_labels<I, L>(self, scope: Scope, labels: I) -> Self
    where
        I: IntoIterator<Item = L>,
        L: Into<String>,
    {
        let labels = labels.into_iter().map(Into::into).collect();
        match dedup_step(scope, labels) {
            Some(step) => self.push_step(step),
            None => self.dedup(),
        }
    }
}

/// The elements of the list `dedup(local)` works on, or the value itself, unchanged, when it is
/// not a list (a map is not one, see [`StepDedupLocal`]). Only a stored list is cloned; a
/// scalar property handle stays lazy.
fn list_items<S: GraphStorage>(
    context: &TraverserContext<'_, S>,
    value: TraverserValue<S>,
) -> Result<Result<Vec<TraverserValue<S>>, TraverserValue<S>>, crate::error::GraphError> {
    Ok(match value {
        TraverserValue::Array(items) => Ok(items),
        TraverserValue::Computed(ElementProperty::Array(items), _) => {
            Ok(items.into_iter().map(TraverserValue::from).collect())
        }
        TraverserValue::VertexProperty(_, _, _) | TraverserValue::EdgeProperty(_, _, _) => {
            match stored_value(context, &value)? {
                Some(ElementProperty::Array(items)) => {
                    Ok(items.iter().cloned().map(TraverserValue::from).collect())
                }
                _ => Err(value),
            }
        }
        TraverserValue::Computed(_, _)
        | TraverserValue::Vertex(_)
        | TraverserValue::Edge(_)
        | TraverserValue::Path(_)
        | TraverserValue::Record { .. }
        | TraverserValue::Object(_)
        | TraverserValue::PathProperty(_)
        | TraverserValue::Entry(_)
        | TraverserValue::Schema(_)
        | TraverserValue::Graph(_) => Err(value),
    })
}

impl<'g, S: GraphStorage + 'g> GraphStepMap<'g, S> for StepDedupLocal {
    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<Traverser<S>> {
        let path = source.path;
        let bulk = source.bulk;
        let sack = source.sack;
        let items = match list_items(context, source.value)? {
            Ok(items) => items,
            // Not a list: passes through unchanged.
            Err(value) => return Ok(Traverser::with_parts(value, path, bulk, sack)),
        };
        let mut seen: std::collections::HashSet<DedupKey<S>, fnv::FnvBuildHasher> =
            std::collections::HashSet::with_capacity_and_hasher(items.len(), Default::default());
        let mut kept = Vec::with_capacity(items.len());
        for item in items {
            if seen.insert(DedupKey::of(context, &item)?) {
                kept.push(item);
            }
        }
        Ok(Traverser::with_parts(
            TraverserValue::Array(kept),
            path,
            bulk,
            sack,
        ))
    }

    /// Builds a new list.
    fn materializes(&self) -> bool {
        true
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
    fn list_first_occurrence_wins() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).out(None).in_(None).values("name").fold().dedup_scoped(Scope::Local).unfold().to_list(),
            rhai: [
                r#"g.V().out().in().values("name").fold().dedup(Scope.local).unfold()"#,
                r#"g.v().out().in_().values("name").fold().dedup(Scope::local).unfold()"#,
            ],
            assert: |res| {
                let names: Vec<&str> = res.iter().filter_map(|v| v.as_str()).collect();
                assert_eq!(names, vec!["marko", "peter", "josh"]);
            },
        );
    }

    #[test]
    fn profile_name() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g
            .v(None)
            .fold()
            .dedup_scoped(Scope::Local)
            .profile()
            .unwrap();
        assert!(
            result.to_string().contains("dedup(Scope.local)"),
            "{result}"
        );
    }

    /// The modern graph with marko's `tags = [1, 2, 1, 3, 2]`.
    #[cfg(feature = "script")]
    fn tagged() -> Graph {
        let graph = GraphSource::tinkerpop_modern();
        graph
            .write()
            .traversal_mut()
            .v("1")
            .property("tags", vec![1, 2, 1, 3, 2])
            .next()
            .unwrap();
        graph
    }

    #[cfg(feature = "script")]
    #[test]
    fn a_stored_list_property_is_deduplicated() {
        test_dual!(
            graph: tagged(),
            rust: |g| g.v("1").values("tags").dedup_scoped(Scope::Local).to_list(),
            rhai: [
                r#"g.V("1").values("tags").dedup(Scope.local)"#,
                r#"g.v("1").values("tags").dedup(Scope::local)"#,
            ],
            assert: |res| assert_eq!(res, &[ElementProperty::from(vec![1, 2, 3])]),
        );
        // anonymous, inside local()
        test_dual!(
            graph: tagged(),
            rust: |g| g
                .v("1")
                .local(__::values("tags").dedup_scoped(Scope::Local))
                .to_list(),
            rhai: [r#"g.V("1").local(__.values("tags").dedup(Scope.local))"#],
            assert: |res| assert_eq!(res, &[ElementProperty::from(vec![1, 2, 3])]),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn a_non_list_passes_through_unchanged() {
        // TinkerPop's DedupLocalStep only touches collections
        test_dual!(
            graph: tagged(),
            rust: |g| g.v("1").values("name").dedup_scoped(Scope::Local).to_list(),
            rhai: [r#"g.V("1").values("name").dedup(Scope.local)"#],
            assert: |res| assert_eq!(res, &["marko".into()]),
        );
        test_dual!(
            graph: tagged(),
            rust: |g| g.v("1").dedup_scoped(Scope::Local).values("name").to_list(),
            rhai: [r#"g.V("1").dedup(Scope.local).values("name")"#],
            assert: |res| assert_eq!(res, &["marko".into()]),
        );
        // a map stays a map: the list inside it is not touched
        test_dual!(
            graph: tagged(),
            rust: |g| g
                .v("1")
                .value_map(vec!["tags"])
                .dedup_scoped(Scope::Local)
                .select(vec!["tags"])
                .unfold()
                .count()
                .to_list(),
            rhai: [r#"g.V("1").valueMap("tags").dedup(Scope.local).select("tags").unfold().count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn global_dedup_by_labels_keys_by_the_labelled_objects() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v(None)
                .as_("a")
                .out(None)
                .dedup_scoped_labels(Scope::Global, vec!["a"])
                .count()
                .to_list(),
            rhai: [
                r#"g.V().as("a").out().dedup(Scope.global, "a").count()"#,
                r#"g.V().as("a").out().dedup("a").count()"#,
            ],
            // marko, josh and peter have outgoing edges: one survivor per start vertex.
            assert: |res| assert_eq!(res[0].as_i64(), Some(3)),
        );
        // without labels, a global dedup is the plain one
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v(None)
                .out(None)
                .where_t(__::dedup_scoped(Scope::Global))
                .dedup_scoped(Scope::Global)
                .count()
                .to_list(),
            rhai: [r#"g.V().out().where(__.dedup(Scope.global)).dedup(Scope.global).count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(4)),
        );
    }
}
