//! `gql:merge(pattern, matcher, creator, ..)`: one GQL `MERGE` clause (feature `gql`,
//! internal, stage 3e): match the whole pattern for each row, else create it.

use std::sync::Arc;

use super::step_gql_insert_node::{GqlInsertProperty, insert_rows};
use super::step_gql_set::{GqlSetItem, StepGqlSet, rewritten};
use crate::gql::eval::{Value, status};
use crate::gql::lexer::Span;
use crate::gql::runtime::{Binding, SubqueryAnswers, SubqueryHost, eval_error, labels_read};
use crate::graph::GraphStorage;
use crate::traversal::optimizer::path_analysis::{PathAnalysis, PathRequirement};
use crate::*;

/// What the lowering hands `gql:merge` for one `MERGE` clause (the builder's one argument, so
/// the lowering names every part).
pub(crate) struct GqlMerge<'g, S: GraphStorage + 'g> {
    /// The query text (the errors and the profile quote it).
    pub(crate) query: Arc<str>,
    /// The `MERGE` pattern in the query (`(a:A {k: x})-[:R]->(b)`), for the profile.
    pub(crate) pattern: Span,
    /// The match child: run on each row, it gives one record per match of the whole pattern
    /// (ending in the `gql:project` of the record), nothing when the pattern does not match.
    pub(crate) matcher: AnonymousTraversal<'g, S>,
    /// The create child: run on a row the matcher found nothing for, it creates the pattern
    /// (the INSERT steps of stage 3a, so the schema's defaults and enforcement apply, W37) and
    /// gives the record (ending in the same `gql:project` as the matcher).
    pub(crate) creator: AnonymousTraversal<'g, S>,
    /// The property values of the pattern, every element's in text order, each checked on the
    /// row before the matcher runs: a `NULL` value is 22004 (E1 of stage 3e: MERGE can neither
    /// match nor create `{k: NULL}`).
    pub(crate) checks: Vec<GqlInsertProperty<S>>,
    /// The variable slots the `checks` read (on the incoming row).
    pub(crate) check_bindings: Arc<[Binding]>,
    /// The `ON CREATE SET` items of every `ON CREATE` action in written order (W35), applied
    /// to the created record.
    pub(crate) on_create: Vec<GqlSetItem<S>>,
    /// The `ON MATCH SET` items, applied to each matched record.
    pub(crate) on_match: Vec<GqlSetItem<S>>,
    /// The variable slots the `ON` items read (targets and values), on the record row: the
    /// pattern's variables as its columns (`Binding::Column`), earlier ones through the path.
    pub(crate) item_bindings: Arc<[Binding]>,
    /// The subqueries the `ON` values hold (child `i` answers subquery `i`, one index space
    /// for `on_create` and `on_match`).
    pub(crate) item_children: Vec<AnonymousTraversal<'g, S>>,
    /// Every element the creator can make, as `(kind, label)`: a node once per label (label-less
    /// `None`), an edge with its label; the step's `Create` requests (design C7 of stage 3e).
    pub(crate) creates: Vec<(ElementKind, Option<CompactString>)>,
}

/// Matches or creates the pattern of one GQL `MERGE` clause for each row (W31): the rows of a
/// traverser of bulk n are n rows (W15), processed one after the other, each seeing what the
/// rows before it created (W14 / W34: `FOR x IN [1, 1] MERGE (:N {k: x})` creates one node).
///
/// Per row: [`context.tick()`](TraverserContext::tick); the pattern's property values are
/// evaluated and a `NULL` one is 22004 (E1, at run time as the openCypher TCK wants); the
/// matcher runs on the row; when it gives records, every record is a row of the output and
/// gets the `ON MATCH` items; otherwise the creator runs once on the row and its record gets
/// the `ON CREATE` items. The `ON` items are the `SET` items of `gql:set`
/// ([`StepGqlSet::apply_items`]), applied after the create (C8: a property only `ON CREATE`
/// sets is a second write in the same unit).
///
/// Emits the records as derived traversers of the row (its path and sack, rule 20; like a
/// `CALL` record through `flat_map`, a new path position), each of bulk 1. The children's
/// own path positions do not continue the outer path. Asks `Create` per element the creator
/// can make (`Data { Vertex, label }` per label, `Data { Edge, label }`) and the `ON` items'
/// `Update`s as `gql:set` asks them; the matcher's `Read`s and the creator's `Create`s are
/// also asked by the walk over the children. Built only by the GQL lowering; not a DSL step.
struct StepGqlMerge<'g, S: GraphStorage + 'g> {
    query: Arc<str>,
    pattern: Span,
    matcher: AnonymousTraversal<'g, S>,
    creator: AnonymousTraversal<'g, S>,
    checks: Arc<[GqlInsertProperty<S>]>,
    check_bindings: Arc<[Binding]>,
    /// The step labels the checks read (sorted), for `path_requirement()` (rule 13).
    check_labels: Arc<[CompactString]>,
    /// `on_create` followed by `on_match` as one `SET` item list.
    items: StepGqlSet<'g, S>,
    /// How many of `items` are `ON CREATE` items (the first ones).
    on_create: usize,
    creates: Arc<[(ElementKind, Option<CompactString>)]>,
}

impl<'g, S: GraphStorage + 'g> Clone for StepGqlMerge<'g, S> {
    fn clone(&self) -> Self {
        Self {
            query: Arc::clone(&self.query),
            pattern: self.pattern,
            matcher: self.matcher.clone(),
            creator: self.creator.clone(),
            checks: Arc::clone(&self.checks),
            check_bindings: Arc::clone(&self.check_bindings),
            check_labels: Arc::clone(&self.check_labels),
            items: self.items.clone(),
            on_create: self.on_create,
            creates: Arc::clone(&self.creates),
        }
    }
}

impl<'g, S: GraphStorage + 'g> fmt::Display for StepGqlMerge<'g, S> {
    /// `gql:merge("<pattern>", __.<matcher>, __.<creator>[, "ON CREATE SET .."][, "ON MATCH
    /// SET .."])`: the GQL texts in string literals (respelling leaves them alone), the children
    /// as their optimized plans.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let pattern = self
            .query
            .get(self.pattern.start..self.pattern.end)
            .unwrap_or_default();
        write!(
            f,
            "gql:merge({}, {}, {}",
            crate::value::literal::rhai_string(pattern),
            self.matcher,
            self.creator
        )?;
        let all = self.items.len();
        for (action, range) in [
            ("ON CREATE SET ", 0..self.on_create),
            ("ON MATCH SET ", self.on_create..all),
        ] {
            if !range.is_empty() {
                let text = format!("{action}{}", self.items.text_of(range));
                write!(f, ", {}", crate::value::literal::rhai_string(&text))?;
            }
        }
        f.write_str(")")
    }
}

impl<'g, S: GraphStorage + 'g> StepGqlMerge<'g, S> {
    fn new(merge: GqlMerge<'g, S>) -> Self {
        let check_labels = labels_read(
            merge.checks.iter().map(|check| &check.value),
            &merge.check_bindings,
        );
        let on_create = merge.on_create.len();
        let mut items = merge.on_create;
        items.extend(merge.on_match);
        Self {
            items: StepGqlSet::new(
                Arc::clone(&merge.query),
                items,
                merge.item_bindings,
                merge.item_children,
            ),
            query: merge.query,
            pattern: merge.pattern,
            matcher: merge.matcher,
            creator: merge.creator,
            checks: merge.checks.into(),
            check_bindings: merge.check_bindings,
            check_labels: check_labels.into(),
            on_create,
            creates: merge.creates.into(),
        }
    }

    /// E1: every property value of the pattern on `row` is not `NULL`.
    fn check(
        &self,
        context: &mut TraverserContext<'_, S>,
        row: &Traverser<S>,
    ) -> TraverserResult<()> {
        if self.checks.is_empty() {
            return Ok(());
        }
        let host = SubqueryHost::<'g, S>::default();
        let mut answers = SubqueryAnswers::default();
        for check in self.checks.iter() {
            let value = host
                .evaluate(context, row, &self.check_bindings, &mut answers, |r| {
                    check.value.eval(r)
                })?
                .map_err(|error| Box::new(eval_error(&self.query, error)))?;
            if matches!(value, Value::Null) {
                return Err(Box::new(self.null_property(check)));
            }
        }
        Ok(())
    }

    #[inline(never)]
    fn null_property(&self, check: &GqlInsertProperty<S>) -> TraverserError {
        let span = check.value.span();
        let text = self.query.get(span.start..span.end).unwrap_or("x");
        let fixed = rewritten(&self.query, span, &format!("coalesce({text}, 0)"));
        GqlErrorBuilder::new(
            GqlErrorKind::Data,
            span.start..span.end,
            format!(
                "MERGE cannot match or create `{{{}: NULL}}`: a property value is NULL",
                check.key
            ),
        )
        .gqlstatus(status::NULL_VALUE)
        .alternative(match fixed {
            Some(fixed) => format!(
                "keep only the rows where it has a value with FILTER {text} IS NOT NULL before \
                 the MERGE, or give it a default with coalesce(), as in {fixed}"
            ),
            None => format!(
                "keep only the rows where it has a value with FILTER {text} IS NOT NULL before \
                 the MERGE, or give it a default with coalesce({text}, ..)"
            ),
        })
        .topic(GqlHelpTopic::Merge)
        .into_error(&self.query)
    }

    /// The records `child` gives for `row`, as derived traversers of the row of bulk 1 each
    /// (a record of bulk n is n rows, W15), then `items` applied to each.
    fn records(
        &self,
        context: &mut TraverserContext<'_, S>,
        row: &Traverser<S>,
        produced: Vec<Traverser<S>>,
        items: std::ops::Range<usize>,
        output: &mut Vec<Traverser<S>>,
    ) -> TraverserResult<()> {
        for record in produced {
            let record = Traverser::with_parts(record.value, row.path, record.bulk, row.sack);
            for unit in insert_rows(context, Some(record), "gql:merge")?.0 {
                if !items.is_empty() {
                    self.items.apply_items(context, &unit, items.clone())?;
                }
                output.push(unit);
            }
        }
        Ok(())
    }
}

/// Pins `now` on every creating step of `steps`, child traversals included.
fn stamp<'g, S: GraphStorage + 'g>(
    steps: &mut [GraphStep<'g, S>],
    now: crate::value::temporal::DateTime,
) {
    for step in steps {
        if let GraphStep::SideEffectMut(_, step) = step {
            step.set_create_time(now);
        }
        for child in step.child_traversals_mut() {
            stamp(&mut child.steps, now);
        }
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Appends `gql:merge(..)`: one `MERGE` clause; see [`StepGqlMerge`].
    pub(crate) fn gql_merge(self, merge: GqlMerge<'g, S>) -> Self {
        self.push_step(GraphStep::SideEffectMut(
            "gql:merge",
            Box::new(StepGqlMerge::new(merge)),
        ))
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Appends `gql:merge(..)`; see [`StepGqlMerge`].
    pub(crate) fn gql_merge(self, merge: GqlMerge<'g, S>) -> Self {
        self.push_step(GraphStep::SideEffectMut(
            "gql:merge",
            Box::new(StepGqlMerge::new(merge)),
        ))
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepSideEffectMut<'g, S> for StepGqlMerge<'g, S> {
    fn set_create_time(&mut self, now: crate::value::temporal::DateTime) {
        stamp(&mut self.creator.steps, now);
    }

    fn gql_creates(&self) -> Vec<(Option<ElementKind>, Option<&str>)> {
        self.creates
            .iter()
            .map(|(kind, label)| (Some(*kind), label.as_deref()))
            .collect()
    }

    fn gql_updates(&self) -> Vec<(Option<ElementKind>, Option<&str>)> {
        self.items.gql_updates()
    }

    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Option<Traverser<S>>,
    ) -> TraverserResult<Vec<Traverser<S>>> {
        let (rows, _) = insert_rows(context, source, "gql:merge")?;
        let mut output = crate::traversal::capacity::vec_capped(rows.len());
        let all = self.items.len();
        for row in rows {
            context.tick()?;
            self.check(context, &row)?;
            // The matcher runs after the previous row's writes (W34).
            let matched = self.matcher.apply_to(context, &row)?;
            if !matched.is_empty() {
                self.records(context, &row, matched, self.on_create..all, &mut output)?;
                continue;
            }
            let created = self.creator.apply_to(context, &row)?;
            self.records(context, &row, created, 0..self.on_create, &mut output)?;
        }
        Ok(output)
    }

    fn path_requirement(&self) -> PathRequirement {
        PathRequirement::labels(self.check_labels.iter().cloned())
            .union(self.items.path_requirement())
    }

    fn history_labels(&self) -> &[CompactString] {
        self.items.history_labels()
    }

    fn child_traversals_mut(&mut self) -> Vec<&mut AnonymousTraversal<'g, S>> {
        let mut children = vec![&mut self.matcher, &mut self.creator];
        children.extend(self.items.child_traversals_mut());
        children
    }

    fn analyze_nested_paths(
        &mut self,
        analysis: &PathAnalysis,
        downstream: &PathRequirement,
    ) -> PathRequirement {
        // Both children run on the incoming row, path included; their own path positions do
        // not continue the outer path (the record does, as one new position).
        self.matcher
            .analyze_paths(analysis, PathRequirement::None)
            .union(self.creator.analyze_paths(analysis, PathRequirement::None))
            .union(self.items.analyze_nested_paths(analysis, downstream))
    }

    fn clone_box(&self) -> Box<dyn GraphStepSideEffectMut<'g, S> + 'g> {
        Box::new(self.clone())
    }
}
