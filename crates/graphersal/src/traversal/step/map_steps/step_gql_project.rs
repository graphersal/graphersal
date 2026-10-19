//! `gql:project(columns)`: the `RETURN` row of a GQL query (feature `gql`, internal).

use std::sync::Arc;

use crate::gql::runtime::{
    Binding, Row, RowExpr, eval_error, history_read, into_traverser_value, labels_read,
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
/// Built only by the GQL lowering; not a DSL step.
struct StepGqlProject<S: GraphStorage> {
    query: Arc<str>,
    /// The column names as record keys, shared by every row.
    keys: Arc<Vec<RecordKey>>,
    /// The column expressions, positionally aligned with `keys`.
    columns: Arc<[RowExpr<S>]>,
    bindings: Arc<[Binding]>,
    labels: Arc<[CompactString]>,
    history: Arc<[CompactString]>,
}

impl<S: GraphStorage> Clone for StepGqlProject<S> {
    fn clone(&self) -> Self {
        Self {
            query: Arc::clone(&self.query),
            keys: Arc::clone(&self.keys),
            columns: Arc::clone(&self.columns),
            bindings: Arc::clone(&self.bindings),
            labels: Arc::clone(&self.labels),
            history: Arc::clone(&self.history),
        }
    }
}

impl<S: GraphStorage> fmt::Display for StepGqlProject<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<&str> = self.keys.iter().map(RecordKey::as_str).collect();
        write!(
            f,
            "gql:project({})",
            crate::value::literal::rhai_strings(&names)
        )
    }
}

impl<S: GraphStorage> StepGqlProject<S> {
    fn new(query: Arc<str>, columns: Vec<GqlColumn<S>>, bindings: Arc<[Binding]>) -> Self {
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
        }
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Appends `gql:project(columns)`; see [`StepGqlProject`].
    pub(crate) fn gql_project(
        self,
        query: Arc<str>,
        columns: Vec<GqlColumn<S>>,
        bindings: Arc<[Binding]>,
    ) -> Self {
        self.push_step(GraphStep::Map(
            "gql:project",
            Box::new(StepGqlProject::new(query, columns, bindings)),
        ))
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Appends `gql:project(columns)`; see [`StepGqlProject`].
    pub(crate) fn gql_project(
        self,
        query: Arc<str>,
        columns: Vec<GqlColumn<S>>,
        bindings: Arc<[Binding]>,
    ) -> Self {
        self.push_step(GraphStep::Map(
            "gql:project",
            Box::new(StepGqlProject::new(query, columns, bindings)),
        ))
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepMap<'g, S> for StepGqlProject<S> {
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<Traverser<S>> {
        let row = Row::new(context, &source, &self.bindings);
        let mut values = Vec::with_capacity(self.columns.len());
        for column in self.columns.iter() {
            let value = match column.as_variable() {
                Some(slot) => match row.bound(slot) {
                    Some(value) => value.into_owned(),
                    None => TraverserValue::Computed(ElementProperty::Null, None),
                },
                None => match column.eval(&row) {
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
            values.push(value);
        }
        Ok(source.split(TraverserValue::Record {
            keys: Arc::clone(&self.keys),
            values,
        }))
    }

    fn path_requirement(&self) -> PathRequirement {
        PathRequirement::labels(self.labels.iter().cloned())
    }

    fn history_labels(&self) -> &[CompactString] {
        &self.history
    }

    fn clone_box(&self) -> Box<dyn GraphStepMap<'g, S> + 'g> {
        Box::new(self.clone())
    }
}
