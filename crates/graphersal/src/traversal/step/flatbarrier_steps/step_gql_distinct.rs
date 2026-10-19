//! `gql:distinct()`: the `RETURN DISTINCT` / `WITH DISTINCT` of a GQL query (feature `gql`,
//! internal).

use std::collections::HashSet;

use fnv::FnvBuildHasher;

use crate::gql::aggregate::GroupKey;
use crate::gql::runtime::{GqlElement, Row};
use crate::graph::GraphStorage;
use crate::*;

/// Keeps the first row of every distinct row map under GQL equality, the equality of grouping
/// (`gql::aggregate::GroupKey`, decision C3): `1` and `1.0` are one value, `NULL` equals `NULL`,
/// lists and paths element-wise, maps in any entry order, elements by identity. Gremlin's
/// `dedup()` keeps `1` and `1.0` apart and stays unchanged.
///
/// It reads the incoming row map of `gql:project` / `gql:aggregate` column by column (a value
/// that is not a row map is one column); the key of a row is built in a reused buffer and
/// stored only for a new row (rule 4). A kept row keeps its value, path and sack and gets bulk 1:
/// it stands for one row, as `dedup()` does (a derived traverser, rule 20). Rows keep their
/// order. A flat barrier that passes its input on, so the permission flow and the path analysis
/// treat it like `dedup()`. It reads no path labels. Built only by the GQL lowering; not a DSL
/// step.
#[derive(Clone, Copy, Default)]
struct StepGqlDistinct;

impl fmt::Display for StepGqlDistinct {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("gql:distinct()")
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Appends `gql:distinct()`; see [`StepGqlDistinct`].
    pub(crate) fn gql_distinct(self) -> Self {
        self.push_step(GraphStep::FlatBarrier(
            "gql:distinct",
            Box::new(StepGqlDistinct),
        ))
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Appends `gql:distinct()`; see [`StepGqlDistinct`].
    pub(crate) fn gql_distinct(self) -> Self {
        self.push_step(GraphStep::FlatBarrier(
            "gql:distinct",
            Box::new(StepGqlDistinct),
        ))
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepFlatBarrier<'g, S> for StepGqlDistinct {
    /// Like `dedup()`, it holds the key of every distinct row.
    fn materializes(&self) -> bool {
        true
    }

    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Vec<Traverser<S>>,
    ) -> TraverserResult<Option<Vec<Traverser<S>>>> {
        let mut seen: HashSet<Vec<GroupKey<GqlElement<S>>>, FnvBuildHasher> = HashSet::default();
        let mut key: Vec<GroupKey<GqlElement<S>>> = Vec::new();
        let mut output = Vec::new();
        for traverser in source {
            context.tick()?;
            key.clear();
            {
                let row = Row::new(context, &traverser, &[]);
                match &traverser.value {
                    TraverserValue::Record { values, .. } => {
                        for value in values {
                            key.push(GroupKey(row.value(value)));
                        }
                    }
                    other => key.push(GroupKey(row.value(other))),
                }
                row.finish()?;
            }
            if seen.contains(key.as_slice()) {
                continue;
            }
            seen.insert(std::mem::take(&mut key));
            output.push(Traverser::with_parts(
                traverser.value,
                traverser.path,
                1,
                traverser.sack,
            ));
        }
        Ok(Some(output))
    }

    fn clone_box(&self) -> Box<dyn GraphStepFlatBarrier<'g, S> + 'g> {
        Box::new(*self)
    }
}
