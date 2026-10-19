//! `gql:aggregate(keys, aggregates, columns)`: the aggregating `RETURN` / `WITH` of a GQL query
//! (feature `gql`, internal).

use std::sync::Arc;

use fnv::FnvBuildHasher;
use indexmap::IndexMap;

use crate::gql::aggregate::{Accumulator, Aggregate, AggregateCall, GroupKey, WithAggregates};
use crate::gql::runtime::{
    Binding, GqlElement, RowExpr, SubqueryAnswers, SubqueryHost, eval_error, history_read,
    into_traverser_value, labels_read, source_text,
};
use crate::graph::GraphStorage;
use crate::traversal::optimizer::path_analysis::PathRequirement;
use crate::*;

/// One output column of an aggregating projection.
pub(crate) struct GqlAggregateColumn<S: GraphStorage> {
    /// The column name (the alias, else the expression text).
    pub(crate) name: CompactString,
    /// The value: for a grouping key the key expression, else an expression whose aggregate
    /// calls read their results ([`crate::gql::aggregate::compile_aggregating`]).
    pub(crate) expr: RowExpr<S>,
    /// A grouping key (an item without an aggregate).
    pub(crate) key: bool,
}

/// The accumulators of one group and the first row that formed it.
struct Group<S: GraphStorage> {
    /// The first traverser of the group: the key variables of the outputs are read from it.
    first: Traverser<S>,
    accumulators: Vec<Accumulator<GqlElement<S>>>,
}

/// Groups the stream by the values of the key columns under GQL grouping equality (`1` and
/// `1.0` are one key, `NULL`s group together: `gql::aggregate::GroupKey`, decision C3), runs
/// the aggregate calls per group, and emits one row map per group in the order the groups first
/// arrived (C7): the columns in written order, a key column as its value (a bare variable as the
/// bound object itself, like `gql:project`), an aggregate column evaluated over the group's
/// results (`count(b) + 1`). Empty input gives no row when there are keys, and one row of the
/// empty aggregates (`count` 0, `collect_list` `[]`, ...) when there are none.
///
/// Unlike `group()` / `fold()` / `count()`, NULL is skipped by every aggregate but `count(*)`,
/// and `min` / `max` order across kinds instead of failing (see `gql::aggregate`). Bulk is
/// honoured without expanding anything (rule 4): `count` adds it, `sum` / `avg` weigh by it,
/// `collect_list` repeats a value by it, `min` / `max` and `DISTINCT` ignore it. The key of a
/// row is built in a reused buffer and stored only for a new group.
///
/// A reducing flat barrier: each output is a *generated* traverser (rule 20: bulk 1, a fresh
/// path, the initial sack), so nothing upstream of it survives into its outputs
/// (`GraphStep::resets_path`). It checks the deadline and cancellation per input row and per
/// group, `bulk.max_expansion` for the repeated values of `collect_list` like `group()`, and
/// `traversal.max_materialized_bytes` and the memory budget for the lists it builds, before
/// building them. It reads the labels its expressions read (rule 13) and is a map-producing kind
/// for the path analysis (`MAP_PRODUCING_KINDS`). A key, an aggregate's argument or an output
/// holding `EXISTS` runs the bodies as the children of a [`SubqueryHost`] (stage 2b) on the
/// input row (an output on the group's first row), each at most once per row. Built only by the
/// GQL lowering; not a DSL step.
struct StepGqlAggregate<'g, S: GraphStorage + 'g> {
    query: Arc<str>,
    /// The column names as record keys, shared by every row.
    keys: Arc<Vec<RecordKey>>,
    /// The column expressions, positionally aligned with `keys`.
    columns: Arc<[RowExpr<S>]>,
    /// The positions of the grouping key columns.
    grouping: Arc<[usize]>,
    /// The aggregate calls; call `i` is what an output reads as aggregate `i`.
    calls: Arc<[AggregateCall<GqlElement<S>>]>,
    bindings: Arc<[Binding]>,
    labels: Arc<[CompactString]>,
    history: Arc<[CompactString]>,
    /// The subqueries the keys, the arguments and the outputs hold.
    host: SubqueryHost<'g, S>,
}

impl<'g, S: GraphStorage + 'g> Clone for StepGqlAggregate<'g, S> {
    fn clone(&self) -> Self {
        Self {
            query: Arc::clone(&self.query),
            keys: Arc::clone(&self.keys),
            columns: Arc::clone(&self.columns),
            grouping: Arc::clone(&self.grouping),
            calls: Arc::clone(&self.calls),
            bindings: Arc::clone(&self.bindings),
            labels: Arc::clone(&self.labels),
            history: Arc::clone(&self.history),
            host: self.host.clone(),
        }
    }
}

impl<'g, S: GraphStorage + 'g> fmt::Display for StepGqlAggregate<'g, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use crate::value::literal::rhai_strings;
        // GQL text in string literals, so the camelCase respelling leaves it alone.
        let keys: Vec<&str> = self
            .grouping
            .iter()
            .filter_map(|&i| self.columns.get(i))
            .map(|expr| source_text(&self.query, expr))
            .collect();
        let calls: Vec<&str> = self
            .calls
            .iter()
            .map(|call| {
                self.query
                    .get(call.span.start..call.span.end)
                    .unwrap_or_default()
            })
            .collect();
        let names: Vec<&str> = self.keys.iter().map(RecordKey::as_str).collect();
        write!(
            f,
            "gql:aggregate([{}], [{}], [{}])",
            rhai_strings(&keys),
            rhai_strings(&calls),
            rhai_strings(&names)
        )
    }
}

impl<'g, S: GraphStorage + 'g> StepGqlAggregate<'g, S> {
    fn new(
        query: Arc<str>,
        columns: Vec<GqlAggregateColumn<S>>,
        calls: Vec<AggregateCall<GqlElement<S>>>,
        bindings: Arc<[Binding]>,
        host: SubqueryHost<'g, S>,
    ) -> Self {
        let exprs = || {
            columns
                .iter()
                .map(|column| &column.expr)
                .chain(calls.iter().filter_map(|call| call.argument.as_ref()))
        };
        let labels = labels_read(exprs(), &bindings).into();
        let history = history_read(exprs(), &bindings).into();
        let grouping: Arc<[usize]> = columns
            .iter()
            .enumerate()
            .filter(|(_, column)| column.key)
            .map(|(i, _)| i)
            .collect();
        let (keys, columns): (Vec<RecordKey>, Vec<RowExpr<S>>) = columns
            .into_iter()
            .map(|column| (RecordKey::Name(column.name), column.expr))
            .unzip();
        Self {
            query,
            keys: Arc::new(keys),
            columns: columns.into(),
            grouping,
            calls: calls.into(),
            bindings,
            labels,
            history,
            host,
        }
    }

    fn fresh(&self) -> Vec<Accumulator<GqlElement<S>>> {
        self.calls.iter().map(AggregateCall::accumulator).collect()
    }

    /// `bulk.max_expansion` over the input when a `collect_list` repeats values by their bulk,
    /// as `group()` checks it for its value lists.
    fn check_expansion(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: &[Traverser<S>],
    ) -> TraverserResult<()> {
        let repeats = self
            .calls
            .iter()
            .any(|call| call.aggregate == Aggregate::CollectList && !call.distinct);
        if !repeats || !context.bulk_active {
            return Ok(());
        }
        let total = bulk::total_bulk(context, source, "gql:aggregate")?;
        let limit = context.options.bulk_max_expansion;
        if total > limit {
            return Err(Box::new(TraverserError::BulkExpansionLimit {
                step: "gql:aggregate".to_string(),
                requested: total,
                limit,
            }));
        }
        Ok(())
    }

    /// `traversal.max_materialized_bytes` and the memory budget over every list the groups'
    /// `collect_list` calls are about to build.
    fn check_lists(
        &self,
        context: &TraverserContext<'_, S>,
        groups: &IndexMap<Vec<GroupKey<GqlElement<S>>>, Group<S>, FnvBuildHasher>,
    ) -> TraverserResult<()> {
        let each = std::mem::size_of::<TraverserValue<S>>() as u64;
        let max = context.options.max_materialized_bytes;
        let mut bytes: u64 = 0;
        for group in groups.values() {
            for accumulator in &group.accumulators {
                bytes = bytes.saturating_add(accumulator.collected().saturating_mul(each));
            }
            if bytes > max {
                return Err(bulk::materialized_bytes_error(max, bytes));
            }
        }
        match &context.options.memory {
            Some(budget) if bytes > 0 => budget.check(bytes),
            _ => Ok(()),
        }
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Appends `gql:aggregate(..)`; see [`StepGqlAggregate`].
    #[cfg_attr(not(test), allow(dead_code, reason = "used by the tests"))]
    pub(crate) fn gql_aggregate(
        self,
        query: Arc<str>,
        columns: Vec<GqlAggregateColumn<S>>,
        calls: Vec<AggregateCall<GqlElement<S>>>,
        bindings: Arc<[Binding]>,
    ) -> Self {
        self.push_step(GraphStep::FlatBarrier(
            "gql:aggregate",
            Box::new(StepGqlAggregate::new(
                query,
                columns,
                calls,
                bindings,
                SubqueryHost::default(),
            )),
        ))
    }

    /// Appends `gql:aggregate(..)` hosting the subqueries `children` (child `i` answers the
    /// columns' and arguments' subquery `i`); see [`StepGqlAggregate`].
    pub(crate) fn gql_aggregate_hosted(
        self,
        query: Arc<str>,
        columns: Vec<GqlAggregateColumn<S>>,
        calls: Vec<AggregateCall<GqlElement<S>>>,
        bindings: Arc<[Binding]>,
        children: Vec<AnonymousTraversal<'g, S>>,
    ) -> Self {
        self.push_step(GraphStep::FlatBarrier(
            "gql:aggregate",
            Box::new(StepGqlAggregate::new(
                query,
                columns,
                calls,
                bindings,
                SubqueryHost::new(children),
            )),
        ))
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Appends `gql:aggregate(..)` hosting the subqueries `children` (child `i` answers the
    /// columns' and arguments' subquery `i`); see [`StepGqlAggregate`].
    pub(crate) fn gql_aggregate_hosted(
        self,
        query: Arc<str>,
        columns: Vec<GqlAggregateColumn<S>>,
        calls: Vec<AggregateCall<GqlElement<S>>>,
        bindings: Arc<[Binding]>,
        children: Vec<AnonymousTraversal<'g, S>>,
    ) -> Self {
        self.push_step(GraphStep::FlatBarrier(
            "gql:aggregate",
            Box::new(StepGqlAggregate::new(
                query,
                columns,
                calls,
                bindings,
                SubqueryHost::new(children),
            )),
        ))
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepFlatBarrier<'g, S> for StepGqlAggregate<'g, S> {
    /// Like `group()`, it holds the accumulators of every group.
    fn materializes(&self) -> bool {
        true
    }

    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Vec<Traverser<S>>,
    ) -> TraverserResult<Option<Vec<Traverser<S>>>> {
        self.check_expansion(context, &source)?;
        let failed = |error| Box::new(eval_error(&self.query, error));
        let mut groups: IndexMap<Vec<GroupKey<GqlElement<S>>>, Group<S>, FnvBuildHasher> =
            IndexMap::default();
        // The key of the current row, reused: stored only when it starts a new group.
        let mut key: Vec<GroupKey<GqlElement<S>>> = Vec::with_capacity(self.grouping.len());
        for traverser in source {
            context.tick()?;
            let mut answers = SubqueryAnswers::default();
            key.clear();
            for &column in self.grouping.iter() {
                let Some(expr) = self.columns.get(column) else {
                    continue;
                };
                // A limit error the row kept (a refused `Env::reserve`) wins over the
                // evaluator's fallback error (`SubqueryHost::evaluate`).
                let value = self.host.evaluate(
                    context,
                    &traverser,
                    &self.bindings,
                    &mut answers,
                    |row| expr.eval(row),
                )?;
                key.push(GroupKey(value.map_err(failed)?));
            }
            let index = match groups.get_index_of(key.as_slice()) {
                Some(index) => index,
                None => {
                    let group = Group {
                        first: traverser.clone(),
                        accumulators: self.fresh(),
                    };
                    groups.insert_full(std::mem::take(&mut key), group).0
                }
            };
            if let Some((_, group)) = groups.get_index_mut(index) {
                for (call, accumulator) in self.calls.iter().zip(group.accumulators.iter_mut()) {
                    // The argument is evaluated before anything is added, so a row stopped at
                    // a subquery adds nothing before it runs again.
                    let added = self.host.evaluate(
                        context,
                        &traverser,
                        &self.bindings,
                        &mut answers,
                        |row| call.add(accumulator, row, traverser.bulk),
                    )?;
                    added.map_err(failed)?;
                }
            }
        }
        if groups.is_empty() && self.grouping.is_empty() {
            // No input and no keys: one row of the empty aggregates; every variable is NULL.
            let first = Traverser::start(
                context,
                TraverserValue::Computed(ElementProperty::Null, None),
            );
            groups.insert(
                Vec::new(),
                Group {
                    first,
                    accumulators: self.fresh(),
                },
            );
        }
        self.check_lists(context, &groups)?;

        let mode = context.path_mode.clone();
        let mut output = Vec::with_capacity(groups.len());
        for (_, group) in groups {
            context.tick()?;
            let results = group
                .accumulators
                .into_iter()
                .map(Accumulator::finish)
                .collect::<Result<Vec<_>, _>>()
                .map_err(failed)?;
            let values = {
                let mut answers = SubqueryAnswers::default();
                let mut values = Vec::with_capacity(self.columns.len());
                for expr in self.columns.iter() {
                    let value = self.host.evaluate(
                        context,
                        &group.first,
                        &self.bindings,
                        &mut answers,
                        |row| match expr.as_variable() {
                            Some(slot) => Ok(match row.bound(slot) {
                                Some(value) => value.into_owned(),
                                None => TraverserValue::Computed(ElementProperty::Null, None),
                            }),
                            None => expr
                                .eval(&WithAggregates {
                                    env: row,
                                    results: &results,
                                })
                                .map(into_traverser_value),
                        },
                    )?;
                    values.push(value.map_err(failed)?);
                }
                values
            };
            let traverser = Traverser::start(
                context,
                TraverserValue::Record {
                    keys: Arc::clone(&self.keys),
                    values,
                },
            );
            output.push(traverser.start_path(&mut context.paths, &mode)?);
        }
        context.check_deadline()?;
        Ok(Some(output))
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

    fn clone_box(&self) -> Box<dyn GraphStepFlatBarrier<'g, S> + 'g> {
        Box::new(self.clone())
    }
}
