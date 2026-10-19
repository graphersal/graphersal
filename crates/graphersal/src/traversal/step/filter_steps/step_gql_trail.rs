//! `gql:trail(labels)`: the trail check of a GQL quantified edge (feature `gql`, internal).

#![allow(
    dead_code,
    reason = "built by the quantified-edge lowering (stage 1b, T8)"
)]

use std::sync::Arc;

use crate::graph::GraphStorage;
use crate::traversal::optimizer::path_analysis::PathRequirement;
use crate::*;

/// Keeps a traverser whose current element equals none of the objects its path labels with
/// one of `labels`, and drops it otherwise: GQL's trail semantics (no edge twice in one match)
/// for the edges of a quantified edge pattern, which the lowering runs as a `repeat()` whose
/// body labels each iteration's edge (`as("#g<id>")`) after this check.
///
/// It walks the path arena from the newest position back (`iter_rev`) and compares element
/// handles, so it allocates nothing per traverser (rule 4). The labels are its path
/// requirement (rule 13): the path analysis keeps exactly those positions, labels only, never a
/// full path. A value that is not a vertex or an edge equals nothing and passes.
///
/// A pure filter: the bulk passes unchanged, the sack is untouched, nothing is written; it asks
/// no permission of its own (it reads only elements earlier steps reached). Built only by the
/// GQL lowering; not a DSL step.
struct StepGqlTrail {
    /// The step labels whose occurrences the current element must differ from (sorted).
    labels: Arc<[CompactString]>,
}

impl Clone for StepGqlTrail {
    fn clone(&self) -> Self {
        Self {
            labels: Arc::clone(&self.labels),
        }
    }
}

impl fmt::Display for StepGqlTrail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "gql:trail({})",
            crate::value::literal::rhai_strings(&self.labels)
        )
    }
}

impl StepGqlTrail {
    fn new(labels: Vec<CompactString>) -> Self {
        let mut labels = labels;
        labels.sort_unstable();
        labels.dedup();
        Self {
            labels: labels.into(),
        }
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Appends `gql:trail(labels)`; see [`StepGqlTrail`].
    pub(crate) fn gql_trail(self, labels: Vec<CompactString>) -> Self {
        self.push_step(GraphStep::Filter(
            "gql:trail",
            Box::new(StepGqlTrail::new(labels)),
        ))
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Appends `gql:trail(labels)`; see [`StepGqlTrail`].
    pub(crate) fn gql_trail(self, labels: Vec<CompactString>) -> Self {
        self.push_step(GraphStep::Filter(
            "gql:trail",
            Box::new(StepGqlTrail::new(labels)),
        ))
    }
}

/// Whether `a` and `b` are the same vertex or the same edge.
fn same_element<S: GraphStorage>(a: &TraverserValue<S>, b: &TraverserValue<S>) -> bool {
    match (a, b) {
        (TraverserValue::Edge(a), TraverserValue::Edge(b)) => a == b,
        (TraverserValue::Vertex(a), TraverserValue::Vertex(b)) => a == b,
        _ => false,
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepFilter<'g, S> for StepGqlTrail {
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: &Traverser<S>,
    ) -> TraverserResult<bool> {
        if !matches!(
            source.value,
            TraverserValue::Edge(_) | TraverserValue::Vertex(_)
        ) {
            return Ok(true);
        }
        let used = context.paths.iter_rev(source.path).any(|node| {
            node.labels.as_ref().is_some_and(|labels| {
                labels
                    .iter()
                    .any(|label| self.labels.binary_search(label).is_ok())
            }) && same_element(&node.value, &source.value)
        });
        Ok(!used)
    }

    fn path_requirement(&self) -> PathRequirement {
        PathRequirement::labels(self.labels.iter().cloned())
    }

    /// Every occurrence of its labels: an `as()` that sets one again keeps the earlier ones.
    fn history_labels(&self) -> &[CompactString] {
        &self.labels
    }

    fn clone_box(&self) -> Box<dyn GraphStepFilter<'g, S> + 'g> {
        Box::new(self.clone())
    }
}
