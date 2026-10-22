//! `gql:call_query("folder/name", __.<body>)`: a GQL call of a saved query (feature `gql`,
//! internal; stage 3h).

use std::sync::Arc;

use crate::graph::GraphStorage;
use crate::traversal::optimizer::path_analysis::{PathAnalysis, PathRequirement};
use crate::value::PathRef;
use crate::*;

/// What the lowering hands the step (`lower.rs`, `Instr::CallQuery`).
pub(crate) struct GqlCallQuery<'g, S: GraphStorage + 'g> {
    name: CompactString,
    path: CompactString,
    child: AnonymousTraversal<'g, S>,
    outputs: Vec<(CompactString, usize)>,
    unit: bool,
}

impl<'g, S: GraphStorage + 'g> GqlCallQuery<'g, S> {
    /// The call of the saved query `name` (shown as `path`, its folder and name): `child` is
    /// its body lowered from one fresh row; per yielded column its name in the caller and the
    /// index of the body's column it holds; `unit` for a body without `RETURN`.
    pub(crate) fn new(
        name: CompactString,
        path: CompactString,
        child: AnonymousTraversal<'g, S>,
        outputs: Vec<(CompactString, usize)>,
        unit: bool,
    ) -> Self {
        GqlCallQuery {
            name,
            path,
            child,
            outputs,
            unit,
        }
    }
}

/// Runs the body of a called saved query once per incoming row (3h.P0 D7) and emits one row
/// per record of its result: the record of the yielded columns, in yield order, under the
/// names they get in the caller (keys shared by every row), read like the row of a `CALL { }`.
///
/// The body runs on a FRESH traverser holding the unit row (the integer `0`, what `inject(0)`
/// starts a query with): an empty path, so the body's own labels never meet the caller's, the
/// execution's initial sack, and the bulk of the incoming row (the child runs once per unit of
/// it when it writes, else once with its records multiplied by it). The arguments are
/// constants of the body (its parameters), so it reads nothing of the row. Every record is
/// derived from the incoming row (rule 20: its path, its sack, the bulk of the record): a new
/// path position after it, like the records of `flat_map()`.
///
/// A body without `RETURN` (ending in `FINISH` or a write; `unit`) yields no record: the step
/// runs it and emits nothing, and stands inside `optional()`, which passes the row on once.
///
/// It asks `Execute` on the saved query, what `g.query(name)` asks first; the body's own
/// requests are its child's, asked of the caller's authorizer by the walk over the children,
/// so a read-only source refuses a body that writes. The path analysis of the body starts
/// empty: it reads nothing of the caller's path. Built only by the GQL lowering; not a DSL
/// step.
struct StepGqlCallQuery<'g, S: GraphStorage + 'g> {
    name: CompactString,
    path: CompactString,
    child: AnonymousTraversal<'g, S>,
    /// The record keys of the yielded columns, shared by every row.
    keys: Arc<Vec<RecordKey>>,
    /// Per yielded column, the index of the body's column it holds.
    columns: Arc<[usize]>,
    unit: bool,
}

impl<'g, S: GraphStorage + 'g> Clone for StepGqlCallQuery<'g, S> {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            path: self.path.clone(),
            child: self.child.clone(),
            keys: Arc::clone(&self.keys),
            columns: Arc::clone(&self.columns),
            unit: self.unit,
        }
    }
}

impl<'g, S: GraphStorage + 'g> fmt::Display for StepGqlCallQuery<'g, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "gql:call_query({}, {})",
            crate::value::literal::rhai_string(&self.path),
            self.child
        )
    }
}

impl<'g, S: GraphStorage + 'g> StepGqlCallQuery<'g, S> {
    fn new(call: GqlCallQuery<'g, S>) -> Self {
        let (keys, columns): (Vec<RecordKey>, Vec<usize>) = call
            .outputs
            .into_iter()
            .map(|(name, column)| (RecordKey::Name(name), column))
            .unzip();
        StepGqlCallQuery {
            name: call.name,
            path: call.path,
            child: call.child,
            keys: Arc::new(keys),
            columns: columns.into(),
            unit: call.unit,
        }
    }

    fn step(call: GqlCallQuery<'g, S>) -> GraphStep<'g, S> {
        GraphStep::FlatMap("gql:call_query", Box::new(Self::new(call)))
    }

    /// The record of the yielded columns of one record of the body.
    fn record(&self, value: TraverserValue<S>) -> TraverserResult<TraverserValue<S>> {
        let TraverserValue::Record { values, .. } = value else {
            return Err(Box::new(TraverserError::wrong_query(format!(
                "internal error: the saved query {} returned no record",
                self.path
            ))));
        };
        let mut picked = Vec::with_capacity(self.columns.len());
        for &column in self.columns.iter() {
            picked.push(
                values
                    .get(column)
                    .cloned()
                    .unwrap_or(TraverserValue::Computed(ElementProperty::Null, None)),
            );
        }
        Ok(TraverserValue::Record {
            keys: Arc::clone(&self.keys),
            values: picked,
        })
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Appends `gql:call_query(..)`; see [`StepGqlCallQuery`].
    pub(crate) fn gql_call_query(self, call: GqlCallQuery<'g, S>) -> Self {
        self.push_step(StepGqlCallQuery::step(call))
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Appends `gql:call_query(..)`; see [`StepGqlCallQuery`].
    pub(crate) fn gql_call_query(self, call: GqlCallQuery<'g, S>) -> Self {
        self.push_step(StepGqlCallQuery::step(call))
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepFlatMap<'g, S> for StepGqlCallQuery<'g, S> {
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
        output: &mut Vec<Traverser<S>>,
    ) -> TraverserResult<()> {
        context.tick()?;
        // A fresh row (generated: empty path, initial sack) standing for the incoming bulk.
        let seed = Traverser::with_parts(
            TraverserValue::from(ElementProperty::from(0_i64)),
            PathRef::NONE,
            source.bulk,
            context.initial_sack,
        );
        let produced = self.child.apply_to(context, &seed)?;
        if self.unit {
            return Ok(());
        }
        for item in produced {
            let record = self.record(item.value)?;
            output.push(Traverser::with_parts(
                record,
                source.path,
                item.bulk,
                source.sack,
            ));
        }
        Ok(())
    }

    fn child_traversals_mut(&mut self) -> Vec<&mut AnonymousTraversal<'g, S>> {
        vec![&mut self.child]
    }

    fn saved_query(&self) -> Option<&str> {
        Some(&self.name)
    }

    fn analyze_nested_paths(
        &mut self,
        analysis: &PathAnalysis,
        _downstream: &PathRequirement,
    ) -> PathRequirement {
        // The body starts on a fresh row: it reads nothing of the incoming path.
        self.child.analyze_paths(analysis, PathRequirement::None);
        PathRequirement::None
    }

    fn clone_box(&self) -> Box<dyn GraphStepFlatMap<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
#[path = "../../../gql/runtime/call_query_step_tests.rs"]
mod tests;
