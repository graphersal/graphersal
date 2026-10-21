//! `gql:bind(var)`: continues a GQL pattern from a bound variable (feature `gql`, internal).

use std::sync::Arc;

use crate::gql::eval::status;
use crate::gql::runtime::{Binding, Row, labels_read_by};
use crate::graph::GraphStorage;
use crate::traversal::optimizer::path_analysis::PathRequirement;
use crate::*;

/// Moves each traverser to the element a variable holds, so a later `MATCH` continues its
/// pattern from it: `WITH a, count(b) AS n MATCH (a)-->(c)` reads `a` from the row map of the
/// materialized `WITH` (`Binding::RowColumn`) or a path label (`Binding::Label`). A source that
/// holds nothing (`NULL`), or holds something that is not an element of the pattern's kind,
/// gives no row: the pattern cannot match it. With a [`GqlBindCheck`] (a `LET` / `WITH` value
/// written as a node or edge, `bind::Binding::checked`), only `NULL` gives no row; any other
/// value that is not an element of the pattern's kind is a data error (22G03).
///
/// A flat map: the output is derived from its input (rule 20: same path, bulk and sack), and
/// the executor records it as a new path position like any adjacency step. It reads the labels
/// of its binding (rule 13). It asks no permission of its own: the element was reached, and its
/// read requested, by the step that bound it. Built only by the GQL lowering; not a DSL step.
struct StepGqlBind {
    /// The variable name as written, for the rendering.
    name: CompactString,
    /// Where the element comes from.
    binding: Arc<[Binding]>,
    /// The kind of element the pattern expects.
    kind: ElementKind,
    /// The step labels the binding reads (sorted).
    labels: Arc<[CompactString]>,
    /// The run-time kind check of a value written as a pattern element.
    check: Option<Arc<GqlBindCheck>>,
}

/// Where a checked `gql:bind` reports a value of the wrong kind: the query and the span of
/// the variable in the pattern.
pub(crate) struct GqlBindCheck {
    /// The GQL query text.
    pub(crate) query: Arc<str>,
    /// The byte range of the variable in the pattern.
    pub(crate) span: std::ops::Range<usize>,
}

impl Clone for StepGqlBind {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            binding: Arc::clone(&self.binding),
            kind: self.kind,
            labels: Arc::clone(&self.labels),
            check: self.check.clone(),
        }
    }
}

impl fmt::Display for StepGqlBind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "gql:bind({})",
            crate::value::literal::rhai_string(&self.name)
        )
    }
}

impl StepGqlBind {
    fn new(
        name: CompactString,
        binding: Binding,
        kind: ElementKind,
        check: Option<Arc<GqlBindCheck>>,
    ) -> Self {
        let binding: Arc<[Binding]> = Arc::from([binding]);
        let labels = labels_read_by(&binding).into();
        Self {
            name,
            binding,
            kind,
            labels,
            check,
        }
    }

    /// The error of a checked value that is not an element of the pattern's kind.
    #[inline(never)]
    fn wrong_kind(&self, check: &GqlBindCheck) -> TraverserError {
        let name = &self.name;
        let (article, word, example) = match self.kind {
            ElementKind::Vertex => (
                "a",
                "node",
                format!("WITH n AS {name} MATCH ({name})-->(m)"),
            ),
            ElementKind::Edge => (
                "an",
                "edge",
                format!("WITH e AS {name} MATCH ()-[{name}]->(m)"),
            ),
        };
        GqlErrorBuilder::new(
            GqlErrorKind::Data,
            check.span.clone(),
            format!(
                "`{name}` is written as {article} {word} of the pattern but holds a value that \
                 is not {article} {word}"
            ),
        )
        .gqlstatus(status::INVALID_VALUE_TYPE)
        .alternative(format!(
            "give it {article} {word} (or NULL, which matches nothing), for example the \
             {word} variable itself, as in {example}"
        ))
        .topic(GqlHelpTopic::PatternVariable)
        .into_error(&check.query)
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Appends `gql:bind(name)` without a kind check; see [`StepGqlBind`].
    #[cfg(test)]
    pub(crate) fn gql_bind(self, name: CompactString, binding: Binding, kind: ElementKind) -> Self {
        self.gql_bind_checked(name, binding, kind, None)
    }

    /// Appends `gql:bind(name)` with an optional run-time kind check; see [`StepGqlBind`].
    pub(crate) fn gql_bind_checked(
        self,
        name: CompactString,
        binding: Binding,
        kind: ElementKind,
        check: Option<Arc<GqlBindCheck>>,
    ) -> Self {
        self.push_step(GraphStep::FlatMap(
            "gql:bind",
            Box::new(StepGqlBind::new(name, binding, kind, check)),
        ))
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Appends `gql:bind(name)` with an optional run-time kind check; see [`StepGqlBind`].
    pub(crate) fn gql_bind_checked(
        self,
        name: CompactString,
        binding: Binding,
        kind: ElementKind,
        check: Option<Arc<GqlBindCheck>>,
    ) -> Self {
        self.push_step(GraphStep::FlatMap(
            "gql:bind",
            Box::new(StepGqlBind::new(name, binding, kind, check)),
        ))
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepFlatMap<'g, S> for StepGqlBind {
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
        output: &mut Vec<Traverser<S>>,
    ) -> TraverserResult<()> {
        let value = {
            let row = Row::new(context, &source, &self.binding);
            match row.bound(0).as_deref() {
                Some(TraverserValue::Vertex(v)) if self.kind == ElementKind::Vertex => {
                    Some(TraverserValue::Vertex(*v))
                }
                Some(TraverserValue::Edge(e)) if self.kind == ElementKind::Edge => {
                    Some(TraverserValue::Edge(*e))
                }
                None | Some(TraverserValue::Computed(ElementProperty::Null, _)) => None,
                Some(_) => match &self.check {
                    Some(check) => return Err(Box::new(self.wrong_kind(check))),
                    None => None,
                },
            }
        };
        if let Some(value) = value {
            output.push(Traverser::with_parts(
                value,
                source.path,
                source.bulk,
                source.sack,
            ));
        }
        Ok(())
    }

    fn path_requirement(&self) -> PathRequirement {
        PathRequirement::labels(self.labels.iter().cloned())
    }

    fn clone_box(&self) -> Box<dyn GraphStepFlatMap<'g, S> + 'g> {
        Box::new(self.clone())
    }
}
