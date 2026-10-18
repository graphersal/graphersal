//! `gql:filter(expr)`: the three-valued `WHERE` of a GQL query (feature `gql`, internal, D12).

use std::sync::Arc;

use crate::gql::eval::Truth;
use crate::gql::runtime::{Binding, Row, RowExpr, eval_error, labels_read, source_text};
use crate::graph::GraphStorage;
use crate::traversal::optimizer::path_analysis::PathRequirement;
use crate::*;

/// Evaluates a compiled GQL condition per traverser and keeps the traverser only when it is
/// TRUE (FALSE and UNKNOWN drop it). The condition reads its variables through `bindings` (the
/// current object, path labels; see `gql::runtime`), so the step declares the labels it reads
/// as its path requirement and the path analysis keeps them (rule 13). An evaluation error (a
/// division by zero, an operand of the wrong kind) fails the traversal as a
/// [`TraverserError::Gql`] with the evaluator's GQLSTATUS and a caret under the expression.
///
/// A pure filter: the bulk passes unchanged, the sack is untouched, nothing is written. The
/// optimizer's `filter_reorder` may move it within a run of filters (cost rank 5, so after the
/// `has()` steps), never across the `as()` that sets a label it reads (an `as()` ends the run).
/// Built only by the GQL lowering; not a DSL step.
struct StepGqlFilter<S: GraphStorage> {
    /// The whole query, for the error excerpt and the rendering.
    query: Arc<str>,
    /// The condition.
    condition: Arc<RowExpr<S>>,
    /// The source of each variable slot.
    bindings: Arc<[Binding]>,
    /// The step labels the condition reads (sorted).
    labels: Arc<[CompactString]>,
}

impl<S: GraphStorage> Clone for StepGqlFilter<S> {
    fn clone(&self) -> Self {
        Self {
            query: Arc::clone(&self.query),
            condition: Arc::clone(&self.condition),
            bindings: Arc::clone(&self.bindings),
            labels: Arc::clone(&self.labels),
        }
    }
}

impl<S: GraphStorage> fmt::Display for StepGqlFilter<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The expression is a string literal, so the camelCase respelling leaves the GQL text
        // (`element_id(n)`) alone.
        write!(
            f,
            "gql:filter({})",
            crate::value::literal::rhai_string(source_text(&self.query, &self.condition))
        )
    }
}

impl<S: GraphStorage> StepGqlFilter<S> {
    fn new(query: Arc<str>, condition: RowExpr<S>, bindings: Arc<[Binding]>) -> Self {
        let labels = labels_read([&condition], &bindings).into();
        Self {
            query,
            condition: Arc::new(condition),
            bindings,
            labels,
        }
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Appends `gql:filter(condition)`; see [`StepGqlFilter`].
    #[allow(dead_code)] // the GQL lowering (T9a) is the caller
    pub(crate) fn gql_filter(
        self,
        query: Arc<str>,
        condition: RowExpr<S>,
        bindings: Arc<[Binding]>,
    ) -> Self {
        self.push_step(GraphStep::Filter(
            "gql:filter",
            Box::new(StepGqlFilter::new(query, condition, bindings)),
        ))
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Appends `gql:filter(condition)`; see [`StepGqlFilter`].
    #[allow(dead_code)] // the GQL lowering (T9a) is the caller
    pub(crate) fn gql_filter(
        self,
        query: Arc<str>,
        condition: RowExpr<S>,
        bindings: Arc<[Binding]>,
    ) -> Self {
        self.push_step(GraphStep::Filter(
            "gql:filter",
            Box::new(StepGqlFilter::new(query, condition, bindings)),
        ))
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepFilter<'g, S> for StepGqlFilter<S> {
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: &Traverser<S>,
    ) -> TraverserResult<bool> {
        let row = Row::new(context, source, &self.bindings);
        let truth = self.condition.truth(&row);
        row.finish()?;
        match truth {
            Ok(truth) => Ok(truth == Truth::True),
            Err(error) => Err(Box::new(eval_error(&self.query, error))),
        }
    }

    fn path_requirement(&self) -> PathRequirement {
        PathRequirement::labels(self.labels.iter().cloned())
    }

    fn clone_box(&self) -> Box<dyn GraphStepFilter<'g, S> + 'g> {
        Box::new(self.clone())
    }
}
