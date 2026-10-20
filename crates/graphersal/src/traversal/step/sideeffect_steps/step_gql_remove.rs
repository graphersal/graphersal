//! `gql:remove(items)`: the items of one GQL `REMOVE` clause (feature `gql`, internal).

use std::sync::Arc;

use super::step_gql_set::{GqlWriteTarget, Resolved, WriteItems, remove_property, write_rows};
use crate::gql::lexer::Span;
use crate::graph::GraphStorage;
use crate::traversal::optimizer::path_analysis::PathRequirement;
use crate::*;

/// One item of a GQL `REMOVE` clause, applied left to right on each row.
#[derive(Debug, Clone)]
pub(crate) enum GqlRemoveItem {
    /// `n.k`: removes `k` (a missing key is a no-op).
    Property {
        target: GqlWriteTarget,
        key: CompactString,
        /// The whole item, for the profile text.
        span: Span,
    },
    /// `n:A:B` / `n IS A & B`: removes each label in order (idempotent; the last label may go,
    /// W26).
    Labels {
        target: GqlWriteTarget,
        labels: Vec<CompactString>,
        span: Span,
    },
}

/// Applies `REMOVE` items left to right on every row: `n.k` removes `k` through the storage's
/// `drop_*_property` (a missing key is a no-op, like `remove_property()`); `n:A:B` removes each
/// label (a missing label is a no-op, the last label may go, like `drop_label()`, W26). The
/// storage write path applies the schema (a `required` property cannot be removed).
///
/// Targets as in `gql:set`: a `NULL` target makes the item a no-op (W25); a target removed
/// earlier in the query is the storage's `ElementRemoved` error (W27); a value that is not an
/// element 22G03; a label item on an edge 22G03.
///
/// Emits each row unchanged; a traverser of bulk n is n rows, each emitted with bulk 1 (W15,
/// rule 20). Asks what `gql:set` asks for the same items: `Update` on the target's data like
/// `remove_property()`, a label item `Update` on `Data { Vertex, None }` like `drop_label()`.
/// Built only by the GQL lowering; not a DSL step.
#[derive(Clone)]
struct StepGqlRemove {
    items: Arc<[GqlRemoveItem]>,
    write: WriteItems,
    /// Every step label the targets read (sorted), for `path_requirement()`.
    labels: Arc<[CompactString]>,
    /// The step labels the targets read in every occurrence, for `history_labels()`.
    history: Arc<[CompactString]>,
}

impl fmt::Display for StepGqlRemove {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "gql:remove({})",
            crate::value::literal::rhai_string(&self.write.text())
        )
    }
}

fn remove_step<'g, S: GraphStorage + 'g>(
    query: Arc<str>,
    items: Vec<GqlRemoveItem>,
) -> GraphStep<'g, S> {
    let write = WriteItems::new(
        query,
        "REMOVE",
        items.iter().map(|item| match item {
            GqlRemoveItem::Property { target, span, .. } => (target.clone(), *span, false),
            GqlRemoveItem::Labels { target, span, .. } => (target.clone(), *span, true),
        }),
    );
    let labels = write.labels();
    let history = write.history();
    GraphStep::SideEffectMut(
        "gql:remove",
        Box::new(StepGqlRemove {
            items: items.into(),
            write,
            labels: labels.into(),
            history: history.into(),
        }),
    )
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Appends `gql:remove(items)`: the items of one `REMOVE` clause; see [`StepGqlRemove`].
    pub(crate) fn gql_remove(self, query: Arc<str>, items: Vec<GqlRemoveItem>) -> Self {
        self.push_step(remove_step(query, items))
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Appends `gql:remove(items)`; see [`StepGqlRemove`].
    pub(crate) fn gql_remove(self, query: Arc<str>, items: Vec<GqlRemoveItem>) -> Self {
        self.push_step(remove_step(query, items))
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepSideEffectMut<'g, S> for StepGqlRemove {
    fn gql_updates(&self) -> Vec<(Option<ElementKind>, Option<&str>)> {
        self.write.updates_with(|item| match self.items.get(item) {
            Some(GqlRemoveItem::Labels { labels, .. }) => labels.len(),
            Some(GqlRemoveItem::Property { .. }) | None => 1,
        })
    }

    fn gql_relabels(&self) -> bool {
        self.write.relabels()
    }

    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Option<Traverser<S>>,
    ) -> TraverserResult<Vec<Traverser<S>>> {
        let rows = write_rows(context, source, "gql:remove")?;
        for row in &rows {
            context.tick()?;
            for (index, item) in self.items.iter().enumerate() {
                let target = self.write.resolve(context, row, index)?;
                match (item, target) {
                    (_, Resolved::Null) => {}
                    (GqlRemoveItem::Property { key, .. }, target) => {
                        remove_property(context.storage, &target, key)?;
                    }
                    (GqlRemoveItem::Labels { labels, .. }, Resolved::Vertex(v)) => {
                        for label in labels {
                            context.storage.remove_vertex_label(v, label)?;
                        }
                    }
                    (GqlRemoveItem::Labels { .. }, Resolved::Edge(_)) => {
                        return Err(Box::new(self.write.edge_label_item(index)));
                    }
                }
            }
        }
        Ok(rows)
    }

    fn extends_path(&self) -> bool {
        false
    }

    fn path_requirement(&self) -> PathRequirement {
        PathRequirement::labels(self.labels.iter().cloned())
    }

    fn history_labels(&self) -> &[CompactString] {
        &self.history
    }

    fn clone_box(&self) -> Box<dyn GraphStepSideEffectMut<'g, S> + 'g> {
        Box::new(self.clone())
    }
}
