//! `order(Scope.local)`: sorts the list or map each traverser holds.

use std::sync::Arc;

use crate::graph::GraphStorage;
use crate::traversal::optimizer::path_analysis::{PathAnalysis, PathRequirement};
use crate::traversal::step::local_scope::{LocalShape, classify};
use crate::traversal::step::order_clauses::OrderClauses;
use crate::value::{Scope, StreamMap};
use crate::*;

/// `order(local)` (TinkerPop `OrderLocalStep`): sorts the elements of the list a traverser holds,
/// or the entries of its map (the result stays a map, in the new entry order), with the same
/// `by()` clauses, tiebreakers, stable sort and "unproductive `by()` drops the element" rule as
/// the global `order()` — the clause list and comparator are shared ([`OrderClauses`]).
///
/// Each element (a map entry as a `TraverserValue::Entry`, so `by(Column::Keys | Column::Values)`
/// reads its key/value) is keyed through a traverser that inherits the incoming traverser's path,
/// so a `by(__.select("x"))` child sees the outer path labels. Vertex/edge handles in a list stay
/// handles; only the sort keys are materialized. Any other value — a scalar, a path — passes
/// through unchanged.
struct StepOrderLocal<'g, S: GraphStorage + 'g> {
    clauses: OrderClauses<'g, S>,
}

impl<'g, S: GraphStorage + 'g> Clone for StepOrderLocal<'g, S> {
    fn clone(&self) -> Self {
        Self {
            clauses: self.clauses.clone(),
        }
    }
}

impl<'g, S: GraphStorage + 'g> fmt::Display for StepOrderLocal<'g, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "order(Scope.local)")?;
        self.clauses.fmt_clauses(f)
    }
}

impl<'g, S: GraphStorage + 'g> StepOrderLocal<'g, S> {
    fn step() -> GraphStep<'g, S> {
        GraphStep::Map(
            "order",
            Box::new(StepOrderLocal {
                clauses: OrderClauses::default(),
            }),
        )
    }

    /// Keys and sorts `items`, dropping those with an unproductive `by()`.
    fn sort_items(
        &self,
        context: &mut TraverserContext<'_, S>,
        path: PathRef,
        sack: SackRef,
        items: Vec<TraverserValue<S>>,
    ) -> TraverserResult<Vec<TraverserValue<S>>> {
        let mut keyed = Vec::with_capacity(items.len());
        for item in items {
            let element = Traverser::with_parts(item, path, 1, sack);
            if let Some(keys) = self.clauses.keys(context, &element)? {
                keyed.push((keys, element.value));
            }
        }
        self.clauses.sort(&mut keyed, context);
        Ok(keyed.into_iter().map(|(_, item)| item).collect())
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// `order(Scope)`: `Scope::Global` is [`order()`](Self::order) (sorts the stream);
    /// `Scope::Local` sorts the list or map each traverser holds, with the same `.by()` clauses
    /// (`g.V().hasLabel("person").fold().order(Scope.local).by("age")` → one list, youngest
    /// first). A map is sorted by entry — `by(Column::Keys)` / `by(Column::Values)` read each
    /// entry's key/value — and stays a map. An element whose `by()` is unproductive (a missing
    /// property) is dropped from the list; any non-collection value passes through unchanged.
    ///
    /// ```
    /// use graphersal::prelude::*;
    /// use graphersal::prelude::Order;
    ///
    /// let graph = TraversalGraph::tinkerpop_modern();
    /// let g = GraphTraversalSource::new(&graph);
    /// let ages = g.v(None).values("age").fold().order_scoped(Scope::Local).by(Order::Desc).to_list();
    /// assert!(ages.is_ok());
    /// ```
    pub fn order_scoped(self, scope: Scope) -> Self {
        match scope {
            Scope::Global => self.order(),
            Scope::Local => self.push_step(StepOrderLocal::step()),
        }
    }
}

impl __ {
    /// Anonymous `order(Scope)`; see [`GraphTraversalSource::order_scoped`].
    pub fn order_scoped<'g, S: GraphStorage + 'g>(scope: Scope) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().order_scoped(scope)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// `order(Scope)`; see [`GraphTraversalSource::order_scoped`].
    pub fn order_scoped(self, scope: Scope) -> Self {
        match scope {
            Scope::Global => self.order(),
            Scope::Local => self.push_step(StepOrderLocal::step()),
        }
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepMap<'g, S> for StepOrderLocal<'g, S> {
    fn property_access(
        &self,
        visit: &mut dyn FnMut(Option<&str>, crate::graph::workload::AccessKind),
    ) {
        for key in self.clauses.property_keys() {
            visit(Some(key), crate::graph::workload::AccessKind::OrderGroup);
        }
    }
    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<Traverser<S>> {
        self.clauses.check("order")?;
        let path = source.path;
        let bulk = source.bulk;
        let sack = source.sack;
        // A record's keys may be tokens (`elementMap()`): sorting works on their string form,
        // so the original keys are kept to rebuild the record with them.
        let record = match &source.value {
            TraverserValue::Record { keys, values } => Some((Arc::clone(keys), values.clone())),
            _ => None,
        };
        let value = match classify(context, source.value)? {
            LocalShape::List(items) => {
                TraverserValue::Array(self.sort_items(context, path, sack, items)?)
            }
            LocalShape::Map(entries) => {
                let entries = entries
                    .into_iter()
                    .map(|entry| TraverserValue::Entry(Box::new(entry)))
                    .collect();
                let sorted = self.sort_items(context, path, sack, entries)?;
                let pairs = sorted.into_iter().filter_map(|entry| match entry {
                    TraverserValue::Entry(entry) => Some(*entry),
                    TraverserValue::VertexProperty(_, _, _)
                    | TraverserValue::EdgeProperty(_, _, _)
                    | TraverserValue::Computed(_, _)
                    | TraverserValue::Vertex(_)
                    | TraverserValue::Edge(_)
                    | TraverserValue::Path(_)
                    | TraverserValue::Record { .. }
                    | TraverserValue::PathProperty(_)
                    | TraverserValue::Object(_)
                    | TraverserValue::Array(_)
                    | TraverserValue::Schema(_)
                    | TraverserValue::Graph(_) => None,
                });
                if let Some(original) = record {
                    rebuild_record(pairs, original)
                } else {
                    TraverserValue::Object(pairs.collect::<StreamMap<S>>())
                }
            }
            LocalShape::Path(path) => TraverserValue::Path(path),
            LocalShape::Scalar(value) => value,
        };
        Ok(Traverser::with_parts(value, path, bulk, sack))
    }

    fn push_modulator_by(&mut self, args: By<'g, S>, _modulator_name: &str) -> ByModulation {
        self.clauses.push_by(args)
    }

    fn child_traversals_mut(&mut self) -> Vec<&mut crate::traversal::AnonymousTraversal<'g, S>> {
        self.clauses.traversals_mut()
    }

    fn analyze_nested_paths(
        &mut self,
        analysis: &PathAnalysis,
        _downstream: &PathRequirement,
    ) -> PathRequirement {
        // A `by(__...)` child runs on each element with the incoming traverser's path.
        self.clauses.analyze_paths(analysis)
    }

    /// Builds a new, sorted list or map.
    fn materializes(&self) -> bool {
        true
    }

    fn clone_box(&self) -> Box<dyn GraphStepMap<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

/// Rebuilds a `Record` from its sorted entries (their keys are the `Computed` strings
/// [`classify`] made of the record's keys), so `select(a, b)`/`element_map()` results keep their
/// kind.
fn rebuild_record<S: GraphStorage>(
    pairs: impl Iterator<Item = (TraverserValue<S>, TraverserValue<S>)>,
    (original_keys, original_values): (Arc<Vec<RecordKey>>, Vec<TraverserValue<S>>),
) -> TraverserValue<S> {
    let mut used = vec![false; original_keys.len()];
    let mut keys = Vec::with_capacity(original_keys.len());
    let mut values = Vec::with_capacity(original_keys.len());
    for (key, value) in pairs {
        let TraverserValue::Computed(ElementProperty::String(text), _) = &key else {
            continue;
        };
        // The original entry of this sorted one: same key text and value (a token and a name
        // key of one text, e.g. `T.label` and a property `label`, are told apart by value; when
        // the values are equal too, either choice gives the same map).
        let same_text = |i: usize| !used[i] && original_keys[i].as_str() == text.as_str();
        let found = (0..original_keys.len())
            .find(|&i| same_text(i) && original_values[i] == value)
            .or_else(|| (0..original_keys.len()).find(|&i| same_text(i)));
        let record_key = match found {
            Some(i) => {
                used[i] = true;
                original_keys[i].clone()
            }
            None => RecordKey::Name(text.clone()),
        };
        keys.push(record_key);
        values.push(value);
    }
    TraverserValue::Record {
        keys: Arc::new(keys),
        values,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_by_age() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).has_label("person").fold().order_scoped(Scope::Local).by("age").unfold().values("name").to_list(),
            rhai: [
                r#"g.V().hasLabel("person").fold().order(Scope.local).by("age").unfold().values("name")"#,
                r#"g.v().has_label("person").fold().order(Scope::local).by("age").unfold().values("name")"#,
            ],
            assert: |res| {
                let names: Vec<&str> = res.iter().filter_map(|v| v.as_str()).collect();
                assert_eq!(names, vec!["vadas", "marko", "josh", "peter"]);
            },
        );
    }

    #[test]
    fn scalar_passes_through() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").values("age").order_scoped(Scope::Local).to_list(),
            rhai: [
                r#"g.V("1").values("age").order(Scope.local)"#,
                r#"g.v("1").values("age").order(Scope::Local)"#,
            ],
            assert: |res| assert_eq!(res, vec![ElementProperty::from(29i64)]),
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
            .order_scoped(Scope::Local)
            .by("age")
            .profile()
            .unwrap();
        assert!(
            result.to_string().contains("order(Scope.local).by("),
            "{result}"
        );
    }
}
