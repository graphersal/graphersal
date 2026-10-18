//! `gql:error`: the marker of a GQL query that does not parse or bind (feature `gql`, internal,
//! D13).

use crate::graph::GraphStorage;
use crate::*;

/// Stands in the plan of a GQL query that failed to parse, bind or lower: it carries the
/// prebuilt [`TraverserError::Gql`] and raises it in the plan-time validation pass
/// (`plan_check`, like a `math()` equation that does not parse), so the query fails before the
/// traversal runs, even when no traverser would reach the step (an empty graph, a filter that
/// drops everything before it). Builders cannot fail, hence the marker. The error is located at
/// this step (`locate_pre_run_error`). Built only by the GQL lowering; not a DSL step.
#[derive(Clone)]
struct StepGqlError {
    /// The error to raise (a [`TraverserError::Gql`]; shared, since errors are not `Clone`).
    error: std::sync::Arc<TraverserError>,
}

impl fmt::Display for StepGqlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let TraverserError::Gql { gqlstatus, .. } = &*self.error {
            write!(
                f,
                "gql:error({})",
                crate::value::literal::rhai_string(gqlstatus)
            )
        } else {
            f.write_str("gql:error()")
        }
    }
}

impl StepGqlError {
    /// A fresh copy of the carried error.
    fn error(&self) -> TraverserResult<()> {
        let TraverserError::Gql {
            kind,
            message,
            offset,
            line,
            column,
            width,
            gqlstatus,
            context,
        } = &*self.error
        else {
            return Err(Box::new(TraverserError::wrong_query(
                self.error.to_string(),
            )));
        };
        Err(Box::new(TraverserError::Gql {
            kind: *kind,
            message: message.clone(),
            offset: *offset,
            line: *line,
            column: *column,
            width: *width,
            gqlstatus: gqlstatus.clone(),
            context: context.clone(),
        }))
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Appends the marker of a GQL query that failed before it could be lowered; see
    /// [`StepGqlError`].
    #[allow(dead_code)] // the GQL lowering (T9a) is the caller
    pub(crate) fn gql_error(self, error: TraverserError) -> Self {
        self.push_step(GraphStep::Filter(
            "gql:error",
            Box::new(StepGqlError {
                error: std::sync::Arc::new(error),
            }),
        ))
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepFilter<'g, S> for StepGqlError {
    fn process(
        &self,
        _context: &mut TraverserContext<'_, S>,
        _source: &Traverser<S>,
    ) -> TraverserResult<bool> {
        self.error().map(|()| false)
    }

    fn plan_check(&self) -> TraverserResult<()> {
        self.error()
    }

    fn clone_box(&self) -> Box<dyn GraphStepFilter<'g, S> + 'g> {
        Box::new(self.clone())
    }
}
