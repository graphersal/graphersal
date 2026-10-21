//! TEMPORARY (stage 3e.T3, replaced by stage 3e.T4): the stand-in of the `gql:merge` builder.
//!
//! The lowering of `MERGE` (3e.T3) emits one `gql:merge` step per clause through
//! `gql_merge_stand_in`, whose signature is the one agreed for the real builder `gql_merge`
//! (3e.T1T2 report: the match child, the create child, the E1 checks, the `ON CREATE` /
//! `ON MATCH` items and a [`GqlMergeSpec`]). Until the step itself lands (3e.T4,
//! `sideeffect_steps/step_gql_merge.rs`), the stand-in keeps every bound `MERGE` refused with
//! the "not yet" error the binder stage refused it with: it is a `gql:error` marker raising
//! that error in the plan-time validation pass, so no `MERGE` query runs and none changes the
//! graph. When 3e.T4 is merged, the lowering calls `gql_merge` instead and this file goes.

use std::sync::Arc;

use crate::error::{GqlErrorBuilder, GqlErrorKind};
use crate::gql::lexer::Span;
use crate::gql::runtime::{Binding, SubqueryHost};
use crate::graph::GraphStorage;
use crate::traversal::step::sideeffect_steps::step_gql_insert_node::GqlInsertProperty;
use crate::traversal::step::sideeffect_steps::step_gql_set::GqlSetItem;
use crate::*;

/// What a `gql:merge` step needs besides its children and items (the agreed builder argument;
/// 3e.T4 may define its own and the lowering then fills that one).
#[allow(
    dead_code,
    reason = "the stand-in reads only the query and the keyword; the real step (3e.T4) reads every field"
)]
pub(crate) struct GqlMergeSpec<'g, S: GraphStorage + 'g> {
    /// The query text.
    pub(crate) query: Arc<str>,
    /// The `MERGE` keyword as written.
    pub(crate) keyword: Span,
    /// The path pattern as written (the profile text).
    pub(crate) pattern: Span,
    /// Every element the create child can make, as an access request names it (C7): a node
    /// per label, `None` for a node without labels; an edge with its label.
    pub(crate) create_labels: Vec<(ElementKind, Option<CompactString>)>,
    /// The slots the checks read, on the incoming row.
    pub(crate) check_bindings: Arc<[Binding]>,
    /// The slots the `ON` items' values read, on the record (a record column as
    /// `Binding::Column`, a binding before the clause through its label or row).
    pub(crate) bindings: Arc<[Binding]>,
    /// The subqueries the `ON` items' values hold, hosted on the record.
    pub(crate) hosts: SubqueryHost<'g, S>,
}

/// The stand-in step: a `gql:error` marker raising [`merge_not_yet_error`] before the run.
#[derive(Clone)]
struct StepGqlMergeStandIn {
    query: Arc<str>,
    keyword: Span,
}

impl fmt::Display for StepGqlMergeStandIn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The name of the `gql:error` marker: its failure leads with the GQL error.
        f.write_str("gql:error(\"0A000\")")
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepFilter<'g, S> for StepGqlMergeStandIn {
    fn process(
        &self,
        _context: &mut TraverserContext<'_, S>,
        _source: &Traverser<S>,
    ) -> TraverserResult<bool> {
        Err(Box::new(merge_not_yet_error(&self.query, self.keyword)))
    }

    fn plan_check(&self) -> TraverserResult<()> {
        Err(Box::new(merge_not_yet_error(&self.query, self.keyword)))
    }

    fn clone_box(&self) -> Box<dyn GraphStepFilter<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

/// The "not yet" error of a `MERGE` at `keyword`: the Gremlin steps that do it.
pub(crate) fn merge_not_yet_error(query: &str, keyword: Span) -> TraverserError {
    GqlErrorBuilder::new(
        GqlErrorKind::NotYet,
        keyword.start..keyword.end,
        "MERGE is not yet supported in this version",
    )
    .alternative(
        "find or create the element with the Gremlin steps merge_v() and merge_e(), for \
         example g.merge_v(#{\"T.label\": \"person\", name: \"marko\"})",
    )
    .into_error(query)
}

/// The stand-in step of `spec`'s clause (the arguments are the real builder's).
fn stand_in<'g, S: GraphStorage + 'g>(spec: &GqlMergeSpec<'g, S>) -> GraphStep<'g, S> {
    GraphStep::Filter(
        "gql:error",
        Box::new(StepGqlMergeStandIn {
            query: Arc::clone(&spec.query),
            keyword: spec.keyword,
        }),
    )
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// TEMPORARY stand-in of `gql_merge` (3e.T4): one `MERGE` clause, refused before the run.
    /// `matcher` runs on the incoming row and ends in the record's `gql:project`; `creator`
    /// creates the pattern from it and ends in the same `gql:project`; `checks` are the
    /// pattern's values (a `NULL` one is 22004); the items run on each record.
    pub(crate) fn gql_merge_stand_in(
        self,
        matcher: AnonymousTraversal<'g, S>,
        creator: AnonymousTraversal<'g, S>,
        checks: Vec<GqlInsertProperty<S>>,
        on_create: Vec<GqlSetItem<S>>,
        on_match: Vec<GqlSetItem<S>>,
        spec: GqlMergeSpec<'g, S>,
    ) -> Self {
        let _ = (matcher, creator, checks, on_create, on_match);
        self.push_step(stand_in(&spec))
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// TEMPORARY stand-in of `gql_merge` (3e.T4); see
    /// [`GraphTraversalSource::gql_merge_stand_in`].
    pub(crate) fn gql_merge_stand_in(
        self,
        matcher: AnonymousTraversal<'g, S>,
        creator: AnonymousTraversal<'g, S>,
        checks: Vec<GqlInsertProperty<S>>,
        on_create: Vec<GqlSetItem<S>>,
        on_match: Vec<GqlSetItem<S>>,
        spec: GqlMergeSpec<'g, S>,
    ) -> Self {
        let _ = (matcher, creator, checks, on_create, on_match);
        self.push_step(stand_in(&spec))
    }
}
