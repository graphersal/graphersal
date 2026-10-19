//! `gql:value(expr)`: the `|` part of a GQL pattern comprehension (feature `gql`, internal).

use std::sync::Arc;

use crate::gql::runtime::{
    Binding, GQL_VALUE, RowExpr, SubqueryAnswers, SubqueryHost, eval_error, history_read,
    into_traverser_value, labels_read, source_text,
};
use crate::graph::GraphStorage;
use crate::traversal::optimizer::path_analysis::PathRequirement;
use crate::*;

/// Turns each traverser into the value of one compiled GQL expression: the single-column twin of
/// `gql:project`, without the row map. It is the last step of the child traversal of a pattern
/// comprehension `[(a)-->(b) WHERE cond | expr]` (stage 2c, design M2): the step hosting the
/// comprehension (`runtime::SubqueryHost`) runs the child and collects these values into the
/// list. A value that is one variable holds the bound object as it is (a vertex or an edge
/// stays the live element handle, like `gql:project`); any other value is evaluated.
///
/// A map step: the traverser keeps its path, bulk and sack (a derived traverser, rule 20). It
/// reads the step labels its expression reads (rule 13) and is a map-producing kind for the
/// path analysis (`MAP_PRODUCING_KINDS`). An expression holding a subquery (a nested pattern
/// comprehension, `EXISTS`) runs the bodies as the children of a [`SubqueryHost`].
/// Built only by the GQL lowering; not a DSL step.
struct StepGqlValue<'g, S: GraphStorage + 'g> {
    query: Arc<str>,
    expr: Arc<RowExpr<S>>,
    bindings: Arc<[Binding]>,
    labels: Arc<[CompactString]>,
    history: Arc<[CompactString]>,
    /// The subqueries the expression holds.
    host: SubqueryHost<'g, S>,
}

impl<'g, S: GraphStorage + 'g> Clone for StepGqlValue<'g, S> {
    fn clone(&self) -> Self {
        Self {
            query: Arc::clone(&self.query),
            expr: Arc::clone(&self.expr),
            bindings: Arc::clone(&self.bindings),
            labels: Arc::clone(&self.labels),
            history: Arc::clone(&self.history),
            host: self.host.clone(),
        }
    }
}

impl<'g, S: GraphStorage + 'g> fmt::Display for StepGqlValue<'g, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // A string literal, so the camelCase respelling leaves the GQL text alone.
        write!(
            f,
            "{GQL_VALUE}({})",
            crate::value::literal::rhai_string(source_text(&self.query, &self.expr))
        )
    }
}

impl<'g, S: GraphStorage + 'g> StepGqlValue<'g, S> {
    fn new(
        query: Arc<str>,
        expr: RowExpr<S>,
        bindings: Arc<[Binding]>,
        host: SubqueryHost<'g, S>,
    ) -> Self {
        let labels = labels_read([&expr], &bindings).into();
        let history = history_read([&expr], &bindings).into();
        Self {
            query,
            expr: Arc::new(expr),
            bindings,
            labels,
            history,
            host,
        }
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Appends `gql:value(expr)` hosting the subqueries `children` (child `i` answers the
    /// expression's subquery `i`); see [`StepGqlValue`].
    #[cfg_attr(not(test), allow(dead_code, reason = "used by the tests"))]
    pub(crate) fn gql_value_hosted(
        self,
        query: Arc<str>,
        expr: RowExpr<S>,
        bindings: Arc<[Binding]>,
        children: Vec<AnonymousTraversal<'g, S>>,
    ) -> Self {
        self.push_step(GraphStep::Map(
            GQL_VALUE,
            Box::new(StepGqlValue::new(
                query,
                expr,
                bindings,
                SubqueryHost::new(children),
            )),
        ))
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Appends `gql:value(expr)` hosting the subqueries `children` (child `i` answers the
    /// expression's subquery `i`); see [`StepGqlValue`].
    pub(crate) fn gql_value_hosted(
        self,
        query: Arc<str>,
        expr: RowExpr<S>,
        bindings: Arc<[Binding]>,
        children: Vec<AnonymousTraversal<'g, S>>,
    ) -> Self {
        self.push_step(GraphStep::Map(
            GQL_VALUE,
            Box::new(StepGqlValue::new(
                query,
                expr,
                bindings,
                SubqueryHost::new(children),
            )),
        ))
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepMap<'g, S> for StepGqlValue<'g, S> {
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<Traverser<S>> {
        let value = self.host.evaluate(
            context,
            &source,
            &self.bindings,
            &mut SubqueryAnswers::default(),
            |row| match self.expr.as_variable() {
                Some(slot) => Ok(match row.bound(slot) {
                    Some(value) => value.into_owned(),
                    None => TraverserValue::Computed(ElementProperty::Null, None),
                }),
                None => self.expr.eval(row).map(into_traverser_value),
            },
        )?;
        match value {
            Ok(value) => Ok(source.split(value)),
            Err(error) => Err(Box::new(eval_error(&self.query, error))),
        }
    }

    fn path_requirement(&self) -> PathRequirement {
        PathRequirement::labels(self.labels.iter().cloned())
    }

    fn child_traversals_mut(&mut self) -> Vec<&mut AnonymousTraversal<'g, S>> {
        self.host.children_mut()
    }

    fn analyze_nested_paths(
        &mut self,
        analysis: &crate::traversal::optimizer::path_analysis::PathAnalysis,
        _downstream: &PathRequirement,
    ) -> PathRequirement {
        self.host.analyze_paths(analysis)
    }

    fn history_labels(&self) -> &[CompactString] {
        &self.history
    }

    fn clone_box(&self) -> Box<dyn GraphStepMap<'g, S> + 'g> {
        Box::new(self.clone())
    }
}
