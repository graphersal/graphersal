//! `gql:project(columns)`: the `RETURN` row of a GQL query (feature `gql`, internal).

use std::sync::Arc;

use crate::gql::runtime::{
    Binding, Row, RowExpr, SubqueryAnswers, SubqueryHost, check_returned, eval_error, history_read,
    into_traverser_value, labels_read,
};
use crate::graph::GraphStorage;
use crate::traversal::optimizer::path_analysis::PathRequirement;
use crate::*;

/// One `RETURN` item: the column name and its expression.
pub(crate) struct GqlColumn<S: GraphStorage> {
    /// The column name (the alias, else the expression text).
    pub(crate) name: CompactString,
    /// The value.
    pub(crate) expr: RowExpr<S>,
}

/// Turns each traverser into the row map of a GQL `RETURN`: one entry per column in written
/// order, the same record `project()` builds (keys shared by every row), so Gremlin steps after
/// `gql()` read it like any map (`select("p")`, `select("name")`). Unlike `project().by(k)`, a
/// column whose value is missing is `null`, never a dropped key (Phase 0 Q7). A column that is
/// one variable (`RETURN p`) holds the bound object as it is: a vertex or an edge stays the
/// live element handle, so `select("p").out()` walks on from it; any other column is evaluated
/// (see `gql::runtime` for the values).
///
/// A map step: the traverser keeps its path, bulk and sack (a derived traverser, rule 20), so
/// equal rows stay merged until a terminal expands them. It reads the step labels its columns
/// read (rule 13) and is a map-producing kind for the path analysis (`MAP_PRODUCING_KINDS`).
/// A column holding `EXISTS` (`RETURN EXISTS { (p)-->() } AS x`) runs the bodies as the
/// children of a [`SubqueryHost`] (stage 2b), each at most once per row.
/// Built only by the GQL lowering; not a DSL step.
struct StepGqlProject<'g, S: GraphStorage + 'g> {
    query: Arc<str>,
    /// The column names as record keys, shared by every row.
    keys: Arc<Vec<RecordKey>>,
    /// The column expressions, positionally aligned with `keys`.
    columns: Arc<[RowExpr<S>]>,
    bindings: Arc<[Binding]>,
    labels: Arc<[CompactString]>,
    history: Arc<[CompactString]>,
    /// The subqueries the columns hold.
    host: SubqueryHost<'g, S>,
    /// The `RETURN` that ends the query (not a `WITH`, not a subquery body's): a column holding
    /// an element a `DELETE` removed earlier is the W28 error (`runtime::check_returned`).
    returning: bool,
}

impl<'g, S: GraphStorage + 'g> Clone for StepGqlProject<'g, S> {
    fn clone(&self) -> Self {
        Self {
            query: Arc::clone(&self.query),
            keys: Arc::clone(&self.keys),
            columns: Arc::clone(&self.columns),
            bindings: Arc::clone(&self.bindings),
            labels: Arc::clone(&self.labels),
            history: Arc::clone(&self.history),
            host: self.host.clone(),
            returning: self.returning,
        }
    }
}

impl<'g, S: GraphStorage + 'g> fmt::Display for StepGqlProject<'g, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<&str> = self.keys.iter().map(RecordKey::as_str).collect();
        write!(
            f,
            "gql:project({})",
            crate::value::literal::rhai_strings(&names)
        )
    }
}

impl<'g, S: GraphStorage + 'g> StepGqlProject<'g, S> {
    fn new(
        query: Arc<str>,
        columns: Vec<GqlColumn<S>>,
        bindings: Arc<[Binding]>,
        host: SubqueryHost<'g, S>,
        returning: bool,
    ) -> Self {
        let labels = labels_read(columns.iter().map(|column| &column.expr), &bindings).into();
        let history = history_read(columns.iter().map(|column| &column.expr), &bindings).into();
        let (keys, columns): (Vec<RecordKey>, Vec<RowExpr<S>>) = columns
            .into_iter()
            .map(|column| (RecordKey::Name(column.name), column.expr))
            .unzip();
        Self {
            query,
            keys: Arc::new(keys),
            columns: columns.into(),
            bindings,
            labels,
            history,
            host,
            returning,
        }
    }

    /// The value of `column` on `row` (see the type docs).
    fn column(
        &self,
        row: &Row<'_, '_, S>,
        column: &RowExpr<S>,
    ) -> TraverserResult<TraverserValue<S>> {
        let value = match column.as_variable() {
            Some(slot) => match row.bound(slot) {
                Some(value) => value.into_owned(),
                None => TraverserValue::Computed(ElementProperty::Null, None),
            },
            None => match column.eval(row) {
                Ok(value) => into_traverser_value(value),
                Err(error) => {
                    // A limit error the row kept (a refused `Env::reserve`) wins over the
                    // evaluator's fallback error.
                    row.finish()?;
                    return Err(Box::new(eval_error(&self.query, error)));
                }
            },
        };
        row.finish()?;
        Ok(value)
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Appends `gql:project(columns)`; see [`StepGqlProject`].
    #[cfg_attr(not(test), allow(dead_code, reason = "used by the tests"))]
    pub(crate) fn gql_project(
        self,
        query: Arc<str>,
        columns: Vec<GqlColumn<S>>,
        bindings: Arc<[Binding]>,
    ) -> Self {
        self.push_step(GraphStep::Map(
            "gql:project",
            Box::new(StepGqlProject::new(
                query,
                columns,
                bindings,
                SubqueryHost::default(),
                false,
            )),
        ))
    }

    /// Appends `gql:project(columns)` hosting the subqueries `children` (child `i` answers the
    /// columns' subquery `i`); see [`StepGqlProject`].
    pub(crate) fn gql_project_hosted(
        self,
        query: Arc<str>,
        columns: Vec<GqlColumn<S>>,
        bindings: Arc<[Binding]>,
        children: Vec<AnonymousTraversal<'g, S>>,
    ) -> Self {
        self.push_step(GraphStep::Map(
            "gql:project",
            Box::new(StepGqlProject::new(
                query,
                columns,
                bindings,
                SubqueryHost::new(children),
                false,
            )),
        ))
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Appends `gql:project(columns)` hosting the subqueries `children` (child `i` answers the
    /// columns' subquery `i`); see [`StepGqlProject`].
    pub(crate) fn gql_project_hosted(
        self,
        query: Arc<str>,
        columns: Vec<GqlColumn<S>>,
        bindings: Arc<[Binding]>,
        children: Vec<AnonymousTraversal<'g, S>>,
    ) -> Self {
        self.push_step(GraphStep::Map(
            "gql:project",
            Box::new(StepGqlProject::new(
                query,
                columns,
                bindings,
                SubqueryHost::new(children),
                false,
            )),
        ))
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Appends `gql:project(columns)` as the `RETURN` that ends the query: like
    /// [`Self::gql_project_hosted`], and a column holding an element a `DELETE` removed earlier
    /// in the query is the W28 error; see [`StepGqlProject`].
    pub(crate) fn gql_project_returning(
        self,
        query: Arc<str>,
        columns: Vec<GqlColumn<S>>,
        bindings: Arc<[Binding]>,
        children: Vec<AnonymousTraversal<'g, S>>,
    ) -> Self {
        self.push_step(GraphStep::Map(
            "gql:project",
            Box::new(StepGqlProject::new(
                query,
                columns,
                bindings,
                SubqueryHost::new(children),
                true,
            )),
        ))
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Appends `gql:project(columns)` as the `RETURN` that ends the query (a `UNION` operand
    /// or the query after `NEXT`); see [`GraphTraversalSource::gql_project_returning`].
    pub(crate) fn gql_project_returning(
        self,
        query: Arc<str>,
        columns: Vec<GqlColumn<S>>,
        bindings: Arc<[Binding]>,
        children: Vec<AnonymousTraversal<'g, S>>,
    ) -> Self {
        self.push_step(GraphStep::Map(
            "gql:project",
            Box::new(StepGqlProject::new(
                query,
                columns,
                bindings,
                SubqueryHost::new(children),
                true,
            )),
        ))
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepMap<'g, S> for StepGqlProject<'g, S> {
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<Traverser<S>> {
        let mut values = Vec::with_capacity(self.columns.len());
        if self.host.is_empty() {
            let row = Row::new(context, &source, &self.bindings);
            for column in self.columns.iter() {
                values.push(self.column(&row, column)?);
            }
        } else {
            let mut answers = SubqueryAnswers::default();
            for column in self.columns.iter() {
                let value =
                    self.host
                        .evaluate(context, &source, &self.bindings, &mut answers, |row| {
                            self.column(row, column).map_err(|error| *error)
                        })?;
                values.push(value.map_err(Box::new)?);
            }
        }
        if self.returning {
            for value in &values {
                check_returned(context, value)?;
            }
        }
        Ok(source.split(TraverserValue::Record {
            keys: Arc::clone(&self.keys),
            values,
        }))
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
