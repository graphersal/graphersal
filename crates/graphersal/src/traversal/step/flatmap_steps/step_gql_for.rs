//! `gql:for(expr)`: the `FOR` / `UNWIND` stage of a GQL query (feature `gql`, internal).

use std::sync::Arc;

use crate::gql::ast::OrdinalKind;
use crate::gql::eval::Value;
use crate::gql::runtime::{
    Binding, RowExpr, RowValue, SubqueryAnswers, SubqueryHost, eval_error, history_read,
    into_traverser_value, labels_read, source_text,
};
use crate::graph::GraphStorage;
use crate::traversal::optimizer::path_analysis::PathRequirement;
use crate::*;

/// The position variable of `FOR x IN list WITH ORDINALITY i` / `WITH OFFSET i`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GqlOrdinal {
    /// The variable name as written.
    pub(crate) name: CompactString,
    /// Where the count starts: `ORDINALITY` 1, `OFFSET` 0.
    pub(crate) kind: OrdinalKind,
}

/// Evaluates the list of a GQL `FOR x IN list` (Cypher `UNWIND list AS x`) on each traverser
/// and emits one traverser per item, in list order: duplicates and `NULL` items are rows of
/// their own (an item `NULL` is a row with `x = NULL`). A `NULL` list gives no row; a value
/// that is not a list gives one row of that value (design E1, Neo4j's `UNWIND`; \[ISO\]: GQL may
/// make it a type error). A list item is the value `gql:project` would hold for it: an element
/// stays the live handle, a nested list an array, a map a record.
///
/// Without an ordinal the item itself is the current object (design E2), so the next step
/// reads `x` as `Binding::Current` and a pattern continues from an element item directly. With
/// `WITH ORDINALITY i` (1, 2, ...) or `WITH OFFSET i` (0, 1, ...) each output is the record
/// `{x, i}` (keys shared by every row), read like the row of a materializing `WITH`
/// (`Binding::Column` while current, `Binding::RowColumn` through a label after leaving it,
/// `gql:bind` to continue a pattern from `x`).
///
/// A flat map: every output is derived from its input (rule 20: same path, bulk and sack, so a
/// merged row of bulk `n` gives items of bulk `n` each), and the executor records it as a new
/// path position like any adjacency step. The list reads the step labels its expression reads
/// (rule 13). `context.tick()` runs once per item, so a long list honours the deadline and the
/// cancel token; the list itself is a value its source already bounded (a parameter, a stored
/// property, a list a `gql:*` step built under its limits). It asks no permission of its own
/// (value-only, like `unfold()`): an element in the list was reached, and its read requested,
/// by the step that bound it. A list holding `EXISTS` runs the bodies as the children of a
/// [`SubqueryHost`] (stage 2b), each at most once per row. Built only by the GQL lowering; not a
/// DSL step.
pub(crate) struct StepGqlFor<'g, S: GraphStorage + 'g> {
    /// The whole query, for the error excerpt.
    query: Arc<str>,
    /// The rendering argument: `x IN <list text>` plus the ordinal clause.
    text: Arc<str>,
    /// The list expression.
    list: Arc<RowExpr<S>>,
    /// The source of each variable slot the list reads.
    bindings: Arc<[Binding]>,
    /// The record keys `[x, i]` and the first position, in record mode; `None` emits the item.
    record: Option<(Arc<Vec<RecordKey>>, i64)>,
    /// The step labels the list reads (sorted).
    labels: Arc<[CompactString]>,
    /// The labels it reads in every occurrence (group variables, sorted).
    history: Arc<[CompactString]>,
    /// The subqueries the list holds.
    host: SubqueryHost<'g, S>,
}

impl<'g, S: GraphStorage + 'g> Clone for StepGqlFor<'g, S> {
    fn clone(&self) -> Self {
        Self {
            query: Arc::clone(&self.query),
            text: Arc::clone(&self.text),
            list: Arc::clone(&self.list),
            bindings: Arc::clone(&self.bindings),
            record: self
                .record
                .as_ref()
                .map(|(keys, first)| (Arc::clone(keys), *first)),
            labels: Arc::clone(&self.labels),
            history: Arc::clone(&self.history),
            host: self.host.clone(),
        }
    }
}

impl<'g, S: GraphStorage + 'g> fmt::Display for StepGqlFor<'g, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // One string literal, so the camelCase respelling leaves the GQL text alone.
        write!(
            f,
            "gql:for({})",
            crate::value::literal::rhai_string(&self.text)
        )
    }
}

impl<'g, S: GraphStorage + 'g> StepGqlFor<'g, S> {
    pub(crate) fn new(
        query: Arc<str>,
        variable: CompactString,
        list: RowExpr<S>,
        bindings: Arc<[Binding]>,
        ordinal: Option<GqlOrdinal>,
    ) -> Self {
        let labels = labels_read([&list], &bindings).into();
        let history = history_read([&list], &bindings).into();
        let mut text = format!("{variable} IN {}", source_text(&query, &list));
        let record = ordinal.map(|ordinal| {
            let (clause, first) = match ordinal.kind {
                OrdinalKind::Ordinality => ("ORDINALITY", 1),
                OrdinalKind::Offset => ("OFFSET", 0),
            };
            text.push_str(" WITH ");
            text.push_str(clause);
            text.push(' ');
            text.push_str(&ordinal.name);
            let keys = vec![RecordKey::Name(variable), RecordKey::Name(ordinal.name)];
            (Arc::new(keys), first)
        });
        Self {
            query,
            text: text.into(),
            list: Arc::new(list),
            bindings,
            record,
            labels,
            history,
            host: SubqueryHost::default(),
        }
    }

    /// The step hosting the subqueries `children` (child `i` answers the list's subquery `i`).
    pub(crate) fn hosting(mut self, children: Vec<AnonymousTraversal<'g, S>>) -> Self {
        self.host = SubqueryHost::new(children);
        self
    }

    /// The traverser value of one item: the item itself, or the record `{x, i}` with `index`
    /// the item's position in the list.
    fn output(&self, item: RowValue<S>, index: usize) -> TraverserValue<S> {
        let item = into_traverser_value(item);
        match &self.record {
            None => item,
            Some((keys, first)) => {
                let position = i64::try_from(index)
                    .unwrap_or(i64::MAX)
                    .saturating_add(*first);
                TraverserValue::Record {
                    keys: Arc::clone(keys),
                    values: vec![
                        item,
                        TraverserValue::Computed(ElementProperty::from(position), None),
                    ],
                }
            }
        }
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Appends `gql:for(variable IN list)`; see [`StepGqlFor`].
    #[cfg_attr(not(test), allow(dead_code, reason = "used by the tests"))]
    pub(crate) fn gql_for(
        self,
        query: Arc<str>,
        variable: CompactString,
        list: RowExpr<S>,
        bindings: Arc<[Binding]>,
        ordinal: Option<GqlOrdinal>,
    ) -> Self {
        self.push_step(GraphStep::FlatMap(
            "gql:for",
            Box::new(StepGqlFor::new(query, variable, list, bindings, ordinal)),
        ))
    }

    /// Appends `gql:for(variable IN list)` hosting the subqueries `children`; see
    /// [`StepGqlFor`].
    pub(crate) fn gql_for_hosted(
        self,
        query: Arc<str>,
        variable: CompactString,
        list: RowExpr<S>,
        bindings: Arc<[Binding]>,
        ordinal: Option<GqlOrdinal>,
        children: Vec<AnonymousTraversal<'g, S>>,
    ) -> Self {
        self.push_step(GraphStep::FlatMap(
            "gql:for",
            Box::new(StepGqlFor::new(query, variable, list, bindings, ordinal).hosting(children)),
        ))
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Appends `gql:for(variable IN list)` hosting the subqueries `children`; see
    /// [`StepGqlFor`].
    pub(crate) fn gql_for_hosted(
        self,
        query: Arc<str>,
        variable: CompactString,
        list: RowExpr<S>,
        bindings: Arc<[Binding]>,
        ordinal: Option<GqlOrdinal>,
        children: Vec<AnonymousTraversal<'g, S>>,
    ) -> Self {
        self.push_step(GraphStep::FlatMap(
            "gql:for",
            Box::new(StepGqlFor::new(query, variable, list, bindings, ordinal).hosting(children)),
        ))
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepFlatMap<'g, S> for StepGqlFor<'g, S> {
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
        output: &mut Vec<Traverser<S>>,
    ) -> TraverserResult<()> {
        // A limit error the row kept (a refused `Env::reserve`) wins over the evaluator's
        // fallback error (`SubqueryHost::evaluate`).
        let list = self
            .host
            .evaluate(
                context,
                &source,
                &self.bindings,
                &mut SubqueryAnswers::default(),
                |row| self.list.eval(row),
            )?
            .map_err(|error| Box::new(eval_error(&self.query, error)))?;
        match list {
            Value::Null => {}
            Value::List(items) => {
                output.reserve(items.len());
                for (index, item) in items.into_iter().enumerate() {
                    context.tick()?;
                    output.push(Traverser::with_parts(
                        self.output(item, index),
                        source.path,
                        source.bulk,
                        source.sack,
                    ));
                }
            }
            other @ (Value::Scalar(_) | Value::Map(_) | Value::Element(_) | Value::Path(_)) => {
                context.tick()?;
                output.push(Traverser::with_parts(
                    self.output(other, 0),
                    source.path,
                    source.bulk,
                    source.sack,
                ));
            }
        }
        Ok(())
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

    fn clone_box(&self) -> Box<dyn GraphStepFlatMap<'g, S> + 'g> {
        Box::new(self.clone())
    }
}
