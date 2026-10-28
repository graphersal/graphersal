//! `gql:not_self_loop`: drops a self-loop edge on the second branch of a GQL either-direction
//! edge pattern (feature `gql`, internal).

use crate::graph::GraphStorage;
use crate::*;

/// Keeps an edge whose two endpoints differ and drops a self-loop (an edge from a vertex to
/// itself); any other value passes, a removed edge is dropped.
///
/// The GQL lowering walks an either-direction edge pattern `-[e]-` as
/// `union(__.out_e(..)..in_v(), __.in_e(..).gql:not_self_loop()..out_v())`: a self-loop is both
/// an outgoing and an incoming edge of its vertex, so without this check it would match twice,
/// while GQL (and Cypher) bind it once. It reads only the edge itself, never the path, so the
/// pattern keeps labels-only paths and bulk merging (`both_e().other_v()` would force full
/// paths). A pure filter: the bulk passes unchanged, nothing is written; it asks no permission of
/// its own (it reads only the element the preceding `in_e()` already requested). Built only by
/// the GQL lowering; not a DSL step.
#[derive(Clone, Copy, Default)]
struct StepGqlNotSelfLoop;

impl fmt::Display for StepGqlNotSelfLoop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("gql:not_self_loop()")
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Appends `gql:not_self_loop()`; see [`StepGqlNotSelfLoop`].
    pub(crate) fn gql_not_self_loop(self) -> Self {
        self.push_step(GraphStep::Filter(
            "gql:not_self_loop",
            Box::new(StepGqlNotSelfLoop),
        ))
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepFilter<'g, S> for StepGqlNotSelfLoop {
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: &Traverser<S>,
    ) -> TraverserResult<bool> {
        match &source.value {
            TraverserValue::Edge(edge) => Ok(context
                .storage
                .edge_endpoints(*edge)?
                .is_some_and(|(from, to)| from != to)),
            _ => Ok(true),
        }
    }

    fn clone_box(&self) -> Box<dyn GraphStepFilter<'g, S> + 'g> {
        Box::new(*self)
    }
}
