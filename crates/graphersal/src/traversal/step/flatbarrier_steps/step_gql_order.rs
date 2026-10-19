//! `gql:order(keys)`: the `ORDER BY` of a GQL query (feature `gql`, internal).

use std::sync::Arc;

use crate::gql::runtime::{
    Binding, OrderValue, Row, RowExpr, compare_order, eval_error, history_read, labels_read,
    source_text,
};
use crate::graph::GraphStorage;
use crate::traversal::optimizer::path_analysis::PathRequirement;
use crate::*;

/// One `ORDER BY` item.
pub(crate) struct GqlSortKey<S: GraphStorage> {
    /// The sort expression.
    pub(crate) expr: RowExpr<S>,
    /// `DESC`.
    pub(crate) descending: bool,
}

/// Sorts the whole stream by GQL sort keys, NULL-aware: every row is kept (a missing value is
/// `NULL`, never a dropped row as with `order().by(k)`), `NULL` sorts last ascending and first
/// descending, and values of different kinds follow one fixed order of types
/// (`gql::runtime::compare_order`, Neo4j's global sort order where Graphersal has the type).
/// The keys are evaluated once per traverser, the sort is stable (equal keys keep their input
/// order), and each traverser keeps its bulk, path and sack, as `order()` does.
///
/// The keys read the row map of the preceding `gql:project` (`Binding::Column`) or path labels
/// (`Binding::Label`, declared as the path requirement, rule 13). A flat barrier that passes its
/// input on, so the permission flow and the path analysis treat it like `order()`. Built only by
/// the GQL lowering; not a DSL step.
struct StepGqlOrder<S: GraphStorage> {
    query: Arc<str>,
    keys: Arc<[GqlSortKey<S>]>,
    bindings: Arc<[Binding]>,
    labels: Arc<[CompactString]>,
    history: Arc<[CompactString]>,
}

impl<S: GraphStorage> Clone for StepGqlOrder<S> {
    fn clone(&self) -> Self {
        Self {
            query: Arc::clone(&self.query),
            keys: Arc::clone(&self.keys),
            bindings: Arc::clone(&self.bindings),
            labels: Arc::clone(&self.labels),
            history: Arc::clone(&self.history),
        }
    }
}

impl<S: GraphStorage> fmt::Display for StepGqlOrder<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let keys: Vec<String> = self
            .keys
            .iter()
            .map(|key| {
                let text = source_text(&self.query, &key.expr);
                if key.descending {
                    format!("{text} DESC")
                } else {
                    text.to_string()
                }
            })
            .collect();
        write!(
            f,
            "gql:order({})",
            crate::value::literal::rhai_strings(&keys)
        )
    }
}

impl<S: GraphStorage> StepGqlOrder<S> {
    fn new(query: Arc<str>, keys: Vec<GqlSortKey<S>>, bindings: Arc<[Binding]>) -> Self {
        let labels = labels_read(keys.iter().map(|key| &key.expr), &bindings).into();
        let history = history_read(keys.iter().map(|key| &key.expr), &bindings).into();
        Self {
            query,
            keys: keys.into(),
            bindings,
            labels,
            history,
        }
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Appends `gql:order(keys)`; see [`StepGqlOrder`].
    pub(crate) fn gql_order(
        self,
        query: Arc<str>,
        keys: Vec<GqlSortKey<S>>,
        bindings: Arc<[Binding]>,
    ) -> Self {
        self.push_step(GraphStep::FlatBarrier(
            "gql:order",
            Box::new(StepGqlOrder::new(query, keys, bindings)),
        ))
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepFlatBarrier<'g, S> for StepGqlOrder<S> {
    /// Like `order()`, it holds the whole stream with its keys.
    fn materializes(&self) -> bool {
        true
    }

    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Vec<Traverser<S>>,
    ) -> TraverserResult<Option<Vec<Traverser<S>>>> {
        let mut keyed: Vec<(Vec<OrderValue>, Traverser<S>)> = Vec::with_capacity(source.len());
        for traverser in source {
            context.tick()?;
            let row = Row::new(context, &traverser, &self.bindings);
            let mut values = Vec::with_capacity(self.keys.len());
            for key in self.keys.iter() {
                let value = key
                    .expr
                    .eval(&row)
                    .map_err(|error| Box::new(eval_error(&self.query, error)))?;
                row.finish()?;
                values.push(OrderValue::of(value, &row));
            }
            drop(row);
            keyed.push((values, traverser));
        }
        keyed.sort_by(|(a, _), (b, _)| {
            for ((a, b), key) in a.iter().zip(b).zip(self.keys.iter()) {
                let order = compare_order(a, b);
                let order = if key.descending {
                    order.reverse()
                } else {
                    order
                };
                if order != std::cmp::Ordering::Equal {
                    return order;
                }
            }
            std::cmp::Ordering::Equal
        });
        // The sort itself cannot be interrupted; a budget it used up fails here.
        context.check_deadline()?;
        Ok(Some(
            keyed.into_iter().map(|(_, traverser)| traverser).collect(),
        ))
    }

    fn path_requirement(&self) -> PathRequirement {
        PathRequirement::labels(self.labels.iter().cloned())
    }

    fn history_labels(&self) -> &[CompactString] {
        &self.history
    }

    fn clone_box(&self) -> Box<dyn GraphStepFlatBarrier<'g, S> + 'g> {
        Box::new(self.clone())
    }
}
