//! The start and end labels of a `where(traversal)` child (TinkerPop's `WhereStartStep` and
//! `WhereEndStep`), shared by `where(traversal)` and the `and()`/`or()`/`not()` children that
//! are the root of a `where()`.
//!
//! In `where(__.as("a").out().as("b"))` the leading `as("a")` does not label the child's first
//! position: it names the object the child *starts at* (a map key of the current value, a
//! side-effect key or a path label, in the `Scoping` order `select()` uses). The trailing
//! `as("b")` keeps the traverser only when some result of the child *equals* the object
//! labelled `b`. A start label that resolves to nothing filters the traverser out (like
//! `select()`); an end label that resolves to nothing does too.
//!
//! Deviations from TinkerPop (also in `book/src/users_guide/where_scope.md`):
//! - the end label is looked up on the traverser entering the `where()`, not on the child's
//!   output traverser (the same labels are visible on both; they differ only when the child
//!   itself defines the label);
//! - a start or end step with more than one label is rejected when the query is planned
//!   (TinkerPop rejects an end step with several labels only);
//! - outside a `where()` an `as()` at the start or end of `not()`/`and()`/`or()` keeps its
//!   plain meaning (a label of the child's own path).

use compact_str::CompactString;

use crate::graph::GraphStorage;
use crate::traversal::optimizer::path_analysis::PathRequirement;
use crate::traversal::step::map_steps::step_select::scope_value;
use crate::*;

/// The labels a `where()` child starts at and must end on; both `None` for a plain child.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ScopeLabels {
    /// The label of a leading `as()`: the object the child starts at.
    pub(crate) start: Option<CompactString>,
    /// The label of a trailing `as()`: the object some child result must equal.
    pub(crate) end: Option<CompactString>,
}

/// Why a child's start or end label cannot be configured.
pub(crate) const MULTIPLE_LABELS: &str =
    "the start and the end step of a where()-traversal can have only one label";

impl ScopeLabels {
    /// Whether the child is a plain existence test.
    pub(crate) fn is_empty(&self) -> bool {
        self.start.is_none() && self.end.is_none()
    }

    /// Strips a leading and a trailing `as()` from `traversal` and returns them as labels, then
    /// lets a leading `and()`/`or()`/`not()` of the child configure its own children the same
    /// way. A step with several labels is a build error; the child is left untouched then.
    pub(crate) fn extract<'g, S: GraphStorage + 'g>(
        traversal: &mut AnonymousTraversal<'g, S>,
    ) -> Result<Self, &'static str> {
        let single = |step: &GraphStep<'g, S>| -> Result<Option<CompactString>, &'static str> {
            match step {
                GraphStep::As(_, as_step) => match as_step.step_labels().map(|l| &l[..]) {
                    Some([label]) => Ok(Some(label.clone())),
                    Some([]) | None => Ok(None),
                    Some(_) => Err(MULTIPLE_LABELS),
                },
                _ => Ok(None),
            }
        };
        let start = match traversal.steps.first() {
            Some(step) => single(step)?,
            None => None,
        };
        // A lone `as("a")` is the start step; it is not also the end step.
        let end = if traversal.steps.len() > usize::from(start.is_some()) {
            match traversal.steps.last() {
                Some(step) => single(step)?,
                None => None,
            }
        } else {
            None
        };
        if end.is_some() {
            traversal.steps.pop();
        }
        if start.is_some() {
            traversal.steps.remove(0);
        }
        // Only a connective that is the child's very first step is its root (TinkerPop's
        // `configureStartAndEndSteps`): after a start label it is an ordinary step.
        if start.is_none()
            && let Some(GraphStep::Filter(_, first)) = traversal.steps.first_mut()
        {
            first.configure_where_scope();
        }
        Ok(Self { start, end })
    }

    /// What the labels read from the incoming path.
    pub(crate) fn requirement(&self) -> PathRequirement {
        PathRequirement::labels(self.start.iter().chain(self.end.iter()).cloned())
    }

    /// The child as written, the labels put back where they were stripped from:
    /// `__.as("a").out().as("b")`.
    pub(crate) fn render_child<'g, S: GraphStorage + 'g>(
        &self,
        traversal: &AnonymousTraversal<'g, S>,
    ) -> String {
        use crate::value::literal::rhai_string;
        if self.is_empty() {
            return traversal.to_string();
        }
        let mut text = String::from("__");
        if let Some(start) = &self.start {
            text.push_str(&format!(".as({})", rhai_string(start)));
        }
        for step in &traversal.steps {
            if !matches!(step, GraphStep::Modulator(_, _)) {
                text.push('.');
                text.push_str(&step.get_name());
            }
        }
        if let Some(end) = &self.end {
            text.push_str(&format!(".as({})", rhai_string(end)));
        }
        text
    }

    /// Whether `source` passes the child: starts the child at the start label's object and
    /// requires an equal result for the end label, or is a plain existence test without labels.
    pub(crate) fn test<'g, S: GraphStorage + 'g>(
        &self,
        traversal: &AnonymousTraversal<'g, S>,
        context: &mut TraverserContext<'_, S>,
        source: &Traverser<S>,
    ) -> TraverserResult<bool> {
        if self.is_empty() {
            return traversal.exists(context, source);
        }
        let begin;
        let start: &Traverser<S> = match &self.start {
            None => source,
            Some(label) => {
                let Some(value) = scope_value(context, source, label)? else {
                    return Ok(false);
                };
                begin = Traverser::with_parts(value, source.path, source.bulk, source.sack);
                &begin
            }
        };
        let Some(end) = &self.end else {
            return traversal.exists(context, start);
        };
        let Some(target) = scope_value(context, source, end)? else {
            return Ok(false);
        };
        let results = traversal.apply_to(context, start)?;
        // Logical equality, as `where(P.eq(..))` compares: a computed `29` equals a lazily
        // read `values("age")`.
        for result in &results {
            if crate::value::predicate::values_equal(context, &result.value, &target)? {
                return Ok(true);
            }
        }
        Ok(false)
    }
}
