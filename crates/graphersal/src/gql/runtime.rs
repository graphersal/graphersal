//! What the internal `gql:*` steps share at run time: the element references the evaluator works
//! on, where each variable's value comes from, the [`Env`] backed by a traverser, the conversions
//! between evaluator values and traverser values, the mapping of evaluator errors into
//! [`TraverserError::Gql`], and the `ORDER BY` order.
//!
//! # Variables
//!
//! A compiled expression reads its variables by slot ([`Scope::variable`]); the step holding it
//! keeps one [`Binding`] per slot, fixed by the lowering:
//!
//! - [`Binding::Current`]: the traverser's current object (the element the pattern stands on);
//! - [`Binding::Label`]: the object the path labels with a step label (`as("a")`), read through
//!   the per-execution path arena like `select("a")` (D11: only labels a step reads are kept,
//!   which the steps declare through `path_requirement()`);
//! - [`Binding::Column`]: a column of the row map built by `gql:project` (an `ORDER BY` key that
//!   names a returned alias);
//! - [`Binding::RowColumn`]: a column of a row map the path labels (`as("#w1")` after a
//!   materializing `WITH`): the map under the label, then the column, read in place;
//! - [`Binding::Group`]: the group variable of a quantified edge outside its element pattern
//!   (stage 2e: a body variable of a quantified path pattern outside its body): the list of
//!   every occurrence of its label in text order (an empty list for a zero-length match;
//!   `NULL` when its anchor label is absent, an `OPTIONAL MATCH` that did not match);
//! - [`Binding::Path`]: a path variable, assembled in text order from the labels of the
//!   pattern's elements ([`PathPart`], a parenthesized path pattern's iterations from its
//!   body's labels: [`PathPart::Body`]), never from `path()` (no Full path, design C11).
//!
//! A variable whose source holds nothing reads as `NULL`.
//!
//! # Values
//!
//! A vertex or an edge stays a handle ([`GqlElement`]); a stored value enters as
//! [`Value::from_property`] (missing and stored `null` read as `NULL`). Leaving the evaluator, an
//! element becomes the same `TraverserValue::Vertex` / `TraverserValue::Edge` handle `select()`
//! and `project()` hold, so a Gremlin step after `gql()` (`select("p").out()`) walks on from the
//! live element; a list becomes a list of such values and a map a record in written key order;
//! `NULL` becomes a `null` value, never a missing key.
//!
//! # `ORDER BY`
//!
//! Rows sort by [`compare_order`], a total order over every value: first by kind, in Neo4j's
//! global sort order of types where Graphersal has the type (map < vertex < edge < list <
//! path < temporal < uuid < string < boolean < number < vector < other < `NULL`), then within the
//! kind: numbers by value across integer and float, strings by code point, `false < true`,
//! temporal values of one type by their own order, elements by id, lists and paths element-wise
//! then by length, maps entry-wise (key, then value) then by size. Values of one kind that do not compare (two
//! temporal types, `NaN`) fall back to the crate's fixed total order (`compare_element_property`),
//! so a sort over mixed input is deterministic. `NULL` is the largest value: last ascending, first
//! descending, as in Neo4j (ISO GQL leaves the default null order to the implementation).
//! [`compare_values`] is the same order over evaluator values (`min` / `max`, C5).

use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::Arc;

use fnv::FnvBuildHasher;

use super::eval::{Compiled, Env, EvalError, Scope, Value, status};
use crate::error::TraverserResult;
use crate::error::{BoxedTraverserError, GqlErrorBuilder, GqlErrorKind, TraverserError};
use crate::graph::GraphStorage;
use crate::traversal::optimizer::path_analysis::{PathAnalysis, PathRequirement};
use crate::traversal::{AnonymousTraversal, Traverser, TraverserContext, bulk};
use crate::value::path_arena::PathNode;
use crate::value::scalar::compare_scalars;
use crate::value::{RecordKey, TraverserPath, TraverserValue, compare_element_property};
use crate::{CompactString, ElementKind, ElementProperty, GraphError, GraphNodeType};

#[cfg(test)]
mod delete_step_tests;
#[cfg(test)]
mod for_step_tests;
#[cfg(test)]
mod insert_step_tests;
#[cfg(test)]
mod merge_step_tests;
#[cfg(test)]
mod set_step_tests;
#[cfg(test)]
mod step_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod value_step_tests;

/// What the `gql:delete` steps of one execution removed, kept on the traversal context
/// (`TraverserContext::gql_deleted`, shared by every nested traversal of the execution).
pub(crate) struct GqlDeleted<S: GraphStorage> {
    /// Whether a `gql:delete` removed an element in this execution: only then does a returning
    /// `RETURN` look for removed elements in its columns (W28), so a query without a `DELETE`
    /// pays one flag test per row.
    pub(crate) any: bool,
    /// The label of every edge a `gql:delete` removed (also the edges a `DETACH DELETE` took
    /// with a node), so `type(r)` of an edge deleted earlier in the query still answers (E3 of
    /// stage 3d, the openCypher behaviour); every other read of it is the W27 error. Bounded
    /// by the edges the execution removed, like their undo entries.
    pub(crate) edge_types: HashMap<S::EdgeId, CompactString, FnvBuildHasher>,
}

impl<S: GraphStorage> Default for GqlDeleted<S> {
    fn default() -> Self {
        Self {
            any: false,
            edge_types: HashMap::default(),
        }
    }
}

/// The error of a read of a vertex or an edge removed earlier in the query (W27): the
/// storage's `ElementRemoved`, as for the Gremlin reads of a dropped element, so its root
/// cause stays the same everywhere (openCypher `EntityNotFound`).
pub(crate) fn removed_error<S: GraphStorage>(element: &GqlElement<S>) -> BoxedTraverserError {
    let kind = match element {
        GqlElement::Vertex(_) => GraphNodeType::Vertex,
        GqlElement::Edge(_) => GraphNodeType::Edge,
    };
    Box::new(GraphError::element_removed(kind).into())
}

/// Whether `element` is still in the graph.
pub(crate) fn is_live<S: GraphStorage>(storage: &S, element: &GqlElement<S>) -> bool {
    match element {
        GqlElement::Vertex(v) => storage.contains_vertex(*v),
        GqlElement::Edge(e) => storage.contains_edge(*e),
    }
}

/// W28: a `RETURN` column holding a vertex or an edge removed earlier in the query (itself, or
/// inside a list, a map or a path) is an error, since a removed element can be neither
/// materialized nor walked from. Checked only after a `gql:delete` removed something in this
/// execution ([`GqlDeleted::any`]); an explicit stack, so a deeply nested value cannot
/// overflow the call stack.
pub(crate) fn check_returned<S: GraphStorage>(
    context: &TraverserContext<'_, S>,
    value: &TraverserValue<S>,
) -> TraverserResult<()> {
    if !context.gql_deleted.any {
        return Ok(());
    }
    let storage = &*context.storage;
    let mut stack: Vec<&TraverserValue<S>> = vec![value];
    while let Some(value) = stack.pop() {
        match value {
            TraverserValue::Vertex(v) => {
                if !storage.contains_vertex(*v) {
                    return Err(removed_error(&GqlElement::<S>::Vertex(*v)));
                }
            }
            TraverserValue::Edge(e) => {
                if !storage.contains_edge(*e) {
                    return Err(removed_error(&GqlElement::<S>::Edge(*e)));
                }
            }
            TraverserValue::Array(items) => stack.extend(items.iter()),
            TraverserValue::Record { values, .. } => stack.extend(values.iter()),
            TraverserValue::Path(path) => stack.extend(path.objects()),
            TraverserValue::Computed(..)
            | TraverserValue::VertexProperty(..)
            | TraverserValue::EdgeProperty(..)
            | TraverserValue::PathProperty(_)
            | TraverserValue::Object(_)
            | TraverserValue::Entry(_)
            | TraverserValue::Schema(_)
            | TraverserValue::Graph(_) => {}
        }
    }
    Ok(())
}

/// A vertex or an edge as the evaluator sees it: a storage handle.
pub(crate) enum GqlElement<S: GraphStorage> {
    /// A vertex.
    Vertex(S::VertexId),
    /// An edge.
    Edge(S::EdgeId),
}

impl<S: GraphStorage> Clone for GqlElement<S> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<S: GraphStorage> Copy for GqlElement<S> {}

impl<S: GraphStorage> PartialEq for GqlElement<S> {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (GqlElement::Vertex(a), GqlElement::Vertex(b)) => a == b,
            (GqlElement::Edge(a), GqlElement::Edge(b)) => a == b,
            (GqlElement::Vertex(_), GqlElement::Edge(_))
            | (GqlElement::Edge(_), GqlElement::Vertex(_)) => false,
        }
    }
}

impl<S: GraphStorage> Eq for GqlElement<S> {}

impl<S: GraphStorage> std::hash::Hash for GqlElement<S> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        match self {
            GqlElement::Vertex(v) => {
                state.write_u8(0);
                v.hash(state);
            }
            GqlElement::Edge(e) => {
                state.write_u8(1);
                e.hash(state);
            }
        }
    }
}

impl<S: GraphStorage> std::fmt::Debug for GqlElement<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GqlElement::Vertex(v) => write!(f, "Vertex({v:?})"),
            GqlElement::Edge(e) => write!(f, "Edge({e:?})"),
        }
    }
}

/// A GQL value over the elements of storage `S`.
pub(crate) type RowValue<S> = Value<GqlElement<S>>;

/// A compiled GQL expression over the elements of storage `S`.
pub(crate) type RowExpr<S> = Compiled<GqlElement<S>>;

/// Where the value of a variable slot comes from (see the module docs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Binding {
    /// The traverser's current object.
    Current,
    /// The object the path labels with this step label.
    Label(CompactString),
    /// The column at this position of the incoming `gql:project` row.
    Column(usize),
    /// The column at position `column` of the row map the path labels with `label`.
    RowColumn {
        /// The step label of the row map.
        label: CompactString,
        /// The column.
        column: usize,
    },
    /// Every object the path labels with `label`, as a list in text order: path order, or its
    /// reverse when the quantified edge was walked against the text direction.
    Group {
        /// The step label set on each occurrence (the edge of each iteration).
        label: CompactString,
        /// A label the path holds whenever the pattern matched (`None`: always matched); its
        /// absence reads as `NULL`.
        anchor: Option<CompactString>,
        /// Walked from the node written after the edge: the list is the reverse of path order.
        reversed: bool,
    },
    /// A path value assembled from these parts in text order.
    Path(Arc<[PathPart]>),
}

/// One part of a path variable, in text order (see [`Binding::Path`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PathPart {
    /// One node or edge of the pattern: the object most recently labelled with this label.
    Element(CompactString),
    /// The edges of a quantified edge and the vertices between them. `edges` labels each
    /// iteration's edge, `vertices` the vertex each iteration reaches, both in walk order.
    /// `reversed`: the pattern was walked against the text direction, so the walk order is the
    /// reverse of the text order. The vertex reached by the last iteration is not part of the
    /// group (it is the next node of the pattern).
    Group {
        /// The label of each iteration's edge.
        edges: CompactString,
        /// The label of the vertex each iteration reaches.
        vertices: CompactString,
        /// Walked against the text direction.
        reversed: bool,
    },
    /// The iterations of a parenthesized path pattern (stage 2e, design M5): `labels` are the
    /// labels of its body's elements after the first node, in text order (each edge and the
    /// node it reaches), each set once per iteration. Iteration `t` in text order is the `t`-th
    /// occurrence of every label, or the `t`-th from the end when `reversed` (walked from the
    /// node after the pattern). The first node of each iteration is the node before it (the
    /// node before the pattern, merge rule), the last node of the last iteration the node
    /// after the pattern: a vertex next to the same vertex appears once.
    Body {
        /// The labels of the body's elements after its first node, in text order.
        labels: Arc<[CompactString]>,
        /// Walked against the text direction.
        reversed: bool,
    },
    /// No object: a label the path holds whenever its pattern matched (the witness node or the
    /// part label of an `OPTIONAL MATCH` part); its absence makes the whole path `NULL`, also
    /// when every element of the path was bound before the part.
    Anchor(CompactString),
}

impl Binding {
    /// The step labels this binding reads in every occurrence (a group variable, the groups of
    /// a path variable), appended to `out`; see `GraphStep::history_labels`.
    fn history_into(&self, out: &mut Vec<CompactString>) {
        match self {
            Binding::Current
            | Binding::Column(_)
            | Binding::Label(_)
            | Binding::RowColumn { .. } => {}
            Binding::Group { label, .. } => out.push(label.clone()),
            Binding::Path(parts) => {
                for part in parts.iter() {
                    match part {
                        PathPart::Element(_) | PathPart::Anchor(_) => {}
                        PathPart::Group {
                            edges, vertices, ..
                        } => {
                            out.push(edges.clone());
                            out.push(vertices.clone());
                        }
                        PathPart::Body { labels, .. } => out.extend(labels.iter().cloned()),
                    }
                }
            }
        }
    }

    /// The step labels this binding reads, appended to `out`.
    fn labels_into(&self, out: &mut Vec<CompactString>) {
        match self {
            Binding::Current | Binding::Column(_) => {}
            Binding::Label(label) | Binding::RowColumn { label, .. } => out.push(label.clone()),
            Binding::Group { label, anchor, .. } => {
                out.push(label.clone());
                out.extend(anchor.iter().cloned());
            }
            Binding::Path(parts) => {
                for part in parts.iter() {
                    match part {
                        PathPart::Element(label) | PathPart::Anchor(label) => {
                            out.push(label.clone());
                        }
                        PathPart::Group {
                            edges, vertices, ..
                        } => {
                            out.push(edges.clone());
                            out.push(vertices.clone());
                        }
                        PathPart::Body { labels, .. } => out.extend(labels.iter().cloned()),
                    }
                }
            }
        }
    }
}

/// The step labels `exprs` read through `bindings`, for `path_requirement()` (rule 13): sorted,
/// deduplicated; empty when they read none.
pub(crate) fn labels_read<'a, S: GraphStorage + 'a>(
    exprs: impl IntoIterator<Item = &'a RowExpr<S>>,
    bindings: &[Binding],
) -> Vec<CompactString> {
    let mut slots = Vec::new();
    for expr in exprs {
        expr.variables(&mut slots);
    }
    let mut labels: Vec<CompactString> = Vec::new();
    for slot in slots {
        if let Some(binding) = bindings.get(slot) {
            binding.labels_into(&mut labels);
        }
    }
    labels.sort_unstable();
    labels.dedup();
    labels
}

/// The step labels `exprs` read in every occurrence through `bindings` (group variables and
/// the groups of path variables), for `history_labels()`: sorted, deduplicated.
pub(crate) fn history_read<'a, S: GraphStorage + 'a>(
    exprs: impl IntoIterator<Item = &'a RowExpr<S>>,
    bindings: &[Binding],
) -> Vec<CompactString> {
    let mut slots = Vec::new();
    for expr in exprs {
        expr.variables(&mut slots);
    }
    let mut labels: Vec<CompactString> = Vec::new();
    for slot in slots {
        if let Some(binding) = bindings.get(slot) {
            binding.history_into(&mut labels);
        }
    }
    labels.sort_unstable();
    labels.dedup();
    labels
}

/// Every step label `bindings` read, whatever expression reads them: sorted, deduplicated.
pub(crate) fn labels_read_by(bindings: &[Binding]) -> Vec<CompactString> {
    let mut labels = Vec::new();
    for binding in bindings {
        binding.labels_into(&mut labels);
    }
    labels.sort_unstable();
    labels.dedup();
    labels
}

/// Every step label `bindings` read in every occurrence (group variables, the groups of path
/// variables): sorted, deduplicated.
pub(crate) fn history_read_by(bindings: &[Binding]) -> Vec<CompactString> {
    let mut labels = Vec::new();
    for binding in bindings {
        binding.history_into(&mut labels);
    }
    labels.sort_unstable();
    labels.dedup();
    labels
}

/// A [`Scope`] for [`compile`](super::eval::compile): variable `names[slot]` is slot `slot`,
/// parameters come from `parameter`.
#[cfg_attr(not(test), allow(dead_code, reason = "used by the tests"))]
pub(crate) struct BindingScope<'a, F: Fn(&str) -> Option<ElementProperty>> {
    /// The variable name of each slot.
    pub(crate) names: &'a [CompactString],
    /// The value of a `$parameter`.
    pub(crate) parameter: F,
}

impl<F: Fn(&str) -> Option<ElementProperty>> Scope for BindingScope<'_, F> {
    fn variable(&self, name: &str) -> Option<usize> {
        self.names.iter().position(|n| n.as_str() == name)
    }

    fn parameter(&self, name: &str) -> Option<ElementProperty> {
        (self.parameter)(name)
    }
}

/// An evaluator error as the [`TraverserError::Gql`] of `query`, keeping its GQLSTATUS: a data
/// exception (class `22`) is [`GqlErrorKind::Data`], an unknown name (`42`) a semantic error,
/// an unsupported construct (`0A`) [`GqlErrorKind::NotYet`].
pub(crate) fn eval_error(query: &str, error: EvalError) -> TraverserError {
    let kind = match error.gqlstatus {
        status::SEMANTIC => GqlErrorKind::Semantic,
        status::NOT_SUPPORTED => GqlErrorKind::NotYet,
        // A refused `Env::reserve` outside a row (a row reports its own limit error first).
        status::LIMIT => GqlErrorKind::Limit,
        // `DIVISION_BY_ZERO`, `OUT_OF_RANGE`, `INVALID_VALUE_TYPE` and the other class `22`
        // codes of stage 1c: data exceptions.
        _ => GqlErrorKind::Data,
    };
    GqlErrorBuilder::new(kind, error.span.start..error.span.end, error.message)
        .gqlstatus(error.gqlstatus)
        .topic(error.topic)
        .into_error(query)
}

/// The text of `expr` in `query` (empty when the span does not fit, never a panic).
pub(crate) fn source_text<'q, E>(query: &'q str, expr: &Compiled<E>) -> &'q str {
    let span = expr.span();
    query.get(span.start..span.end).unwrap_or_default()
}

/// The [`Env`] of one traverser: its current object, its path labels, the graph.
///
/// A failure to read a value (a materialization error) cannot pass through the evaluator's
/// infallible `Env` methods: it is kept and returned by [`Self::finish`], which the step calls
/// after every evaluation, so it is never lost.
pub(crate) struct Row<'a, 'c, S: GraphStorage> {
    context: &'a TraverserContext<'c, S>,
    traverser: &'a Traverser<S>,
    bindings: &'a [Binding],
    failure: RefCell<Option<BoxedTraverserError>>,
    /// The subqueries this row already ran ([`SubqueryHost`]); `None`: none.
    answers: Option<&'a SubqueryAnswers<S>>,
    /// The subquery the evaluation asked for and the row has not run yet.
    pending: Cell<Option<usize>>,
}

impl<'a, 'c, S: GraphStorage> Row<'a, 'c, S> {
    /// The row of `traverser`, its variables bound by `bindings`.
    pub(crate) fn new(
        context: &'a TraverserContext<'c, S>,
        traverser: &'a Traverser<S>,
        bindings: &'a [Binding],
    ) -> Self {
        Self::with_answers(context, traverser, bindings, None)
    }

    /// The row of `traverser` with the subqueries it already ran.
    fn with_answers(
        context: &'a TraverserContext<'c, S>,
        traverser: &'a Traverser<S>,
        bindings: &'a [Binding],
        answers: Option<&'a SubqueryAnswers<S>>,
    ) -> Self {
        Row {
            context,
            traverser,
            bindings,
            failure: RefCell::new(None),
            answers,
            pending: Cell::new(None),
        }
    }

    /// The first value that could not be read, if any.
    pub(crate) fn finish(&self) -> Result<(), BoxedTraverserError> {
        match self.failure.borrow_mut().take() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// Keeps the W27 error when `element` was removed earlier in the query: an `Env` read that
    /// found nothing of it (no id, no properties, no label) is then no silent `NULL`, empty
    /// list or `FALSE` but the storage's `ElementRemoved` for [`Self::finish`]. Called only
    /// when a read already came back empty, so a live element costs nothing more on the
    /// common path.
    #[inline(never)]
    fn removed(&self, element: &GqlElement<S>) {
        if !is_live(&*self.context.storage, element) {
            self.failure
                .borrow_mut()
                .get_or_insert_with(|| removed_error(element));
        }
    }

    /// The traverser value bound to `slot`, as stored (`None`: nothing bound, read as `NULL`).
    pub(crate) fn bound(&self, slot: usize) -> Option<Cow<'a, TraverserValue<S>>> {
        match self.bindings.get(slot)? {
            Binding::Current => Some(Cow::Borrowed(&self.traverser.value)),
            Binding::Label(label) => self
                .context
                .paths
                .by_label(self.traverser.path, label)
                .map(Cow::Owned),
            Binding::Column(index) => match &self.traverser.value {
                TraverserValue::Record { values, .. } => values.get(*index).map(Cow::Borrowed),
                _ => None,
            },
            Binding::RowColumn { label, column } => match &self.labelled(label)?.value {
                TraverserValue::Record { values, .. } => values.get(*column).map(Cow::Borrowed),
                _ => None,
            },
            Binding::Group {
                label,
                anchor,
                reversed,
            } => {
                if let Some(anchor) = anchor
                    && self.labelled(anchor).is_none()
                {
                    return None;
                }
                Some(Cow::Owned(TraverserValue::Array(
                    self.occurrences_in(label, *reversed),
                )))
            }
            Binding::Path(parts) => self.path(parts).map(Cow::Owned),
        }
    }

    /// The newest path position labelled `label`.
    fn labelled(&self, label: &str) -> Option<&'a PathNode<S>> {
        self.context
            .paths
            .iter_rev(self.traverser.path)
            .find(|node| has_label(node, label))
    }

    /// Every object labelled `label`, in path order.
    fn occurrences(&self, label: &str) -> Vec<TraverserValue<S>> {
        self.occurrences_in(label, false)
    }

    /// Every object labelled `label`, in path order, or in reverse path order when `reversed`.
    fn occurrences_in(&self, label: &str, reversed: bool) -> Vec<TraverserValue<S>> {
        let mut found: Vec<TraverserValue<S>> = self
            .context
            .paths
            .iter_rev(self.traverser.path)
            .filter(|node| has_label(node, label))
            .map(|node| node.value.clone())
            .collect();
        if !reversed {
            found.reverse();
        }
        found
    }

    /// The path value of `parts` (see [`PathPart`]); `None` when a part's label is absent.
    fn path(&self, parts: &[PathPart]) -> Option<TraverserValue<S>> {
        let mut objects: Vec<TraverserValue<S>> = Vec::new();
        for part in parts {
            match part {
                PathPart::Element(label) => {
                    let value = &self.labelled(label)?.value;
                    push_path_object(&mut objects, value.clone());
                }
                PathPart::Anchor(label) => {
                    self.labelled(label)?;
                }
                PathPart::Group {
                    edges,
                    vertices,
                    reversed,
                } => {
                    let edges = self.occurrences(edges);
                    let vertices = self.occurrences(vertices);
                    // Iteration `i` reached `vertices[i]`; the last one is the next node.
                    if vertices.len() + 1 < edges.len() {
                        return None;
                    }
                    let count = edges.len();
                    if *reversed {
                        // Walk order e_k .. e_1: the vertex after e_i in the text is the one
                        // iteration `i` reached before it.
                        for (i, edge) in edges.iter().enumerate().rev() {
                            objects.push(edge.clone());
                            if i > 0
                                && let Some(vertex) = vertices.get(i - 1)
                            {
                                objects.push(vertex.clone());
                            }
                        }
                    } else {
                        for (i, edge) in edges.iter().enumerate() {
                            objects.push(edge.clone());
                            if i + 1 < count
                                && let Some(vertex) = vertices.get(i)
                            {
                                objects.push(vertex.clone());
                            }
                        }
                    }
                }
                PathPart::Body { labels, reversed } => {
                    self.body_path(labels, *reversed, &mut objects)?
                }
            }
        }
        Some(TraverserValue::Path(TraverserPath::from_objects(objects)))
    }

    /// Appends the iterations of a parenthesized path pattern ([`PathPart::Body`]) to
    /// `objects`; `None` when its labels do not occur equally often (not one walk of the body).
    fn body_path(
        &self,
        labels: &[CompactString],
        reversed: bool,
        objects: &mut Vec<TraverserValue<S>>,
    ) -> Option<()> {
        let occurrences: Vec<Vec<TraverserValue<S>>> =
            labels.iter().map(|label| self.occurrences(label)).collect();
        let count = occurrences.first().map_or(0, Vec::len);
        if occurrences.iter().any(|o| o.len() != count) {
            return None;
        }
        for t in 0..count {
            let at = if reversed { count - 1 - t } else { t };
            for found in &occurrences {
                push_path_object(objects, found.get(at)?.clone());
            }
        }
        Some(())
    }

    /// `value` as a GQL value (a row map's column, a sort key); a materialization error is kept
    /// for [`Self::finish`].
    pub(crate) fn value(&self, value: &TraverserValue<S>) -> RowValue<S> {
        self.to_value(value)
    }

    /// `value` as a GQL value; a materialization error is kept for [`Self::finish`].
    fn to_value(&self, value: &TraverserValue<S>) -> RowValue<S> {
        match value {
            TraverserValue::Vertex(v) => Value::Element(GqlElement::Vertex(*v)),
            TraverserValue::Edge(e) => Value::Element(GqlElement::Edge(*e)),
            TraverserValue::Computed(property, _) => Value::from_property(property.clone()),
            TraverserValue::Array(items) => {
                Value::List(items.iter().map(|item| self.to_value(item)).collect())
            }
            TraverserValue::Record { keys, values } => Value::Map(
                keys.iter()
                    .zip(values)
                    .map(|(key, value)| (record_key_name(key), self.to_value(value)))
                    .collect(),
            ),
            TraverserValue::Path(path) => match path_elements(path) {
                Some(elements) => Value::Path(elements),
                None => self.materialized(value),
            },
            TraverserValue::VertexProperty(..)
            | TraverserValue::EdgeProperty(..)
            | TraverserValue::PathProperty(_)
            | TraverserValue::Object(_)
            | TraverserValue::Entry(_)
            | TraverserValue::Schema(_)
            | TraverserValue::Graph(_) => self.materialized(value),
        }
    }

    /// `value` materialized as a property value; a failure is kept for [`Self::finish`].
    #[inline(never)]
    fn materialized(&self, value: &TraverserValue<S>) -> RowValue<S> {
        match value.into_node_property(&*self.context.storage) {
            Ok(property) => Value::from_property(property),
            Err(error) => {
                self.failure.borrow_mut().get_or_insert(error);
                Value::Null
            }
        }
    }
}

/// Whether `node` carries the step label `label`.
fn has_label<S: GraphStorage>(node: &PathNode<S>, label: &str) -> bool {
    node.labels
        .as_ref()
        .is_some_and(|labels| labels.iter().any(|l| l == label))
}

/// Appends a node or an edge to a path being assembled. A vertex right after a vertex is the
/// same node again (the far end of a zero-length quantified edge), so it is not repeated.
fn push_path_object<S: GraphStorage>(
    objects: &mut Vec<TraverserValue<S>>,
    value: TraverserValue<S>,
) {
    if matches!(value, TraverserValue::Vertex(_))
        && matches!(objects.last(), Some(TraverserValue::Vertex(_)))
    {
        return;
    }
    objects.push(value);
}

/// The elements of a path made only of vertices and edges (a GQL path value), else `None`.
fn path_elements<S: GraphStorage>(path: &TraverserPath<S>) -> Option<Vec<GqlElement<S>>> {
    path.objects()
        .map(|object| match object {
            TraverserValue::Vertex(v) => Some(GqlElement::Vertex(*v)),
            TraverserValue::Edge(e) => Some(GqlElement::Edge(*e)),
            _ => None,
        })
        .collect()
}

/// The name of a record key as a GQL map key.
fn record_key_name(key: &RecordKey) -> CompactString {
    match key {
        RecordKey::Name(name) => name.clone(),
        other => CompactString::from(other.to_string()),
    }
}

impl<S: GraphStorage> Env for Row<'_, '_, S> {
    type Element = GqlElement<S>;

    fn variable(&self, slot: usize) -> RowValue<S> {
        match self.bound(slot) {
            Some(value) => self.to_value(&value),
            None => Value::Null,
        }
    }

    fn kind(&self, element: &GqlElement<S>) -> ElementKind {
        match element {
            GqlElement::Vertex(_) => ElementKind::Vertex,
            GqlElement::Edge(_) => ElementKind::Edge,
        }
    }

    fn element_id(&self, element: &GqlElement<S>) -> CompactString {
        let storage = &*self.context.storage;
        let id = match element {
            GqlElement::Vertex(v) => storage.vertex_id(*v),
            GqlElement::Edge(e) => storage.edge_id(*e),
        };
        match id {
            Some(id) => CompactString::from(id),
            None => {
                self.removed(element);
                CompactString::default()
            }
        }
    }

    fn property(&self, element: &GqlElement<S>, key: &str) -> Option<ElementProperty> {
        let storage = &*self.context.storage;
        let properties = match element {
            GqlElement::Vertex(v) => storage.vertex_properties(*v),
            GqlElement::Edge(e) => storage.edge_properties(*e),
        };
        match properties {
            Some(properties) => properties.get(key).cloned(),
            None => {
                self.removed(element);
                None
            }
        }
    }

    fn has_label(&self, element: &GqlElement<S>, label: &str) -> bool {
        let storage = &*self.context.storage;
        let found = match element {
            GqlElement::Vertex(v) => storage.vertex_has_label(*v, label),
            GqlElement::Edge(e) => storage.edge_label(*e) == Some(label),
        };
        if !found {
            self.removed(element);
        }
        found
    }

    fn has_any_label(&self, element: &GqlElement<S>) -> bool {
        let storage = &*self.context.storage;
        let found = match element {
            GqlElement::Vertex(v) => !storage.vertex_labels(*v).is_empty(),
            GqlElement::Edge(e) => storage.edge_label(*e).is_some_and(|l| !l.is_empty()),
        };
        if !found {
            self.removed(element);
        }
        found
    }

    fn labels(&self, element: &GqlElement<S>) -> Vec<CompactString> {
        let storage = &*self.context.storage;
        let labels = match element {
            GqlElement::Vertex(v) => storage.vertex_labels(*v).to_vec(),
            GqlElement::Edge(e) => storage
                .edge_label(*e)
                .map(|label| vec![CompactString::from(label)])
                .unwrap_or_default(),
        };
        if labels.is_empty() {
            self.removed(element);
        }
        labels
    }

    /// The label of a live edge; of an edge a `gql:delete` removed earlier in this execution
    /// the label it had (E3 of stage 3d, the openCypher behaviour); of any other removed edge
    /// the W27 error.
    fn edge_type(&self, edge: &GqlElement<S>) -> Option<CompactString> {
        match edge {
            GqlElement::Vertex(_) => None,
            GqlElement::Edge(e) => match self.context.storage.edge_label(*e) {
                Some(label) => Some(CompactString::from(label)),
                None => match self.context.gql_deleted.edge_types.get(e) {
                    Some(label) => Some(label.clone()),
                    None => {
                        self.removed(edge);
                        None
                    }
                },
            },
        }
    }

    fn properties(&self, element: &GqlElement<S>) -> Vec<(CompactString, ElementProperty)> {
        let storage = &*self.context.storage;
        let properties = match element {
            GqlElement::Vertex(v) => storage.vertex_properties(*v),
            GqlElement::Edge(e) => storage.edge_properties(*e),
        };
        match properties {
            Some(map) => map
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
            None => {
                self.removed(element);
                Vec::new()
            }
        }
    }

    fn endpoints(&self, edge: &GqlElement<S>) -> Option<(GqlElement<S>, GqlElement<S>)> {
        match edge {
            GqlElement::Vertex(_) => None,
            GqlElement::Edge(e) => match self.context.storage.edge_endpoints(*e) {
                Some((source, target)) => {
                    Some((GqlElement::Vertex(source), GqlElement::Vertex(target)))
                }
                None => {
                    self.removed(edge);
                    None
                }
            },
        }
    }

    fn subquery(&self, index: usize) -> Option<RowValue<S>> {
        match self.answers.and_then(|answers| answers.get(index)) {
            Some(value) => Some(value),
            None => {
                self.pending.set(Some(index));
                None
            }
        }
    }

    /// `traversal.max_materialized_bytes` and the memory budget, like `collect_list` in
    /// `gql:aggregate`; a refusal keeps the limit error for [`Row::finish`].
    fn reserve(&self, bytes: u64) -> bool {
        let max = self.context.options.max_materialized_bytes;
        let checked = if bytes > max {
            Err(bulk::materialized_bytes_error(max, bytes))
        } else {
            match &self.context.options.memory {
                Some(budget) if bytes > 0 => budget.check(bytes),
                _ => Ok(()),
            }
        };
        match checked {
            Ok(()) => true,
            Err(error) => {
                self.failure.borrow_mut().get_or_insert(error);
                false
            }
        }
    }
}

/// The most subqueries one step hosts (one bit each in [`SubqueryAnswers`]): the binder admits
/// at most `bind::MAX_SUBQUERIES` (64) per query, and a host runs each of them at most once.
pub(crate) const MAX_HOSTED_SUBQUERIES: usize = 64;

/// The subqueries one row has run so far and their results: `EXISTS` TRUE or FALSE, one bit
/// per hosted subquery, no allocation per row (rule 4); a pattern comprehension its list (the
/// result value itself, the only allocation).
pub(crate) struct SubqueryAnswers<S: GraphStorage> {
    known: u64,
    found: u64,
    /// The values of the pattern comprehensions, `COLLECT { }` and `COUNT { }` subqueries the
    /// row ran, by hosted index.
    values: Vec<(usize, RowValue<S>)>,
}

impl<S: GraphStorage> Default for SubqueryAnswers<S> {
    fn default() -> Self {
        Self {
            known: 0,
            found: 0,
            values: Vec::new(),
        }
    }
}

impl<S: GraphStorage> SubqueryAnswers<S> {
    /// The result of subquery `index`, when the row ran it.
    fn get(&self, index: usize) -> Option<RowValue<S>> {
        let bit = 1u64.checked_shl(u32::try_from(index).ok()?)?;
        if self.known & bit == 0 {
            return None;
        }
        match self.values.iter().find(|(i, _)| *i == index) {
            Some((_, list)) => Some(list.clone()),
            None => Some(Value::Scalar(ElementProperty::Boolean(
                self.found & bit != 0,
            ))),
        }
    }

    /// Records the value (a list, a count) of subquery `index`; `false` past
    /// [`MAX_HOSTED_SUBQUERIES`] (which the lowering never builds).
    fn set_value(&mut self, index: usize, value: RowValue<S>) -> bool {
        if !self.set(index, true) {
            return false;
        }
        self.values.push((index, value));
        true
    }

    /// Records the result of subquery `index`; `false` past [`MAX_HOSTED_SUBQUERIES`] (which
    /// the lowering never builds).
    fn set(&mut self, index: usize, found: bool) -> bool {
        let Some(bit) = u32::try_from(index)
            .ok()
            .and_then(|shift| 1u64.checked_shl(shift))
        else {
            return false;
        };
        self.known |= bit;
        if found {
            self.found |= bit;
        } else {
            self.found &= !bit;
        }
        true
    }
}

/// The subqueries (`EXISTS` anywhere in an expression but a top-level conjunct: under `OR`,
/// `CASE`, in a `RETURN` column, an `ORDER BY` key, an aggregate's argument, a `FOR` list;
/// design M2, the 2b hook; every pattern comprehension, stage 2c; every `COUNT { }` and
/// `COLLECT { }` and whole-query bodies, stage 2d) a `gql:*` step hosts: one
/// child traversal each, lowered from the
/// step's site (the body's plan for its current element). The step embeds it and hands its
/// children to every walker: `child_traversals_mut` (optimizer, step numbering, authorization,
/// embedder binding, `find_fault`), `analyze_nested_paths` (path analysis: the children read
/// the labels their bodies read on the step's input, their output never continues the path),
/// and the profile (each child is a numbered child of the step).
///
/// Evaluation is lazy: a row runs a child only when the evaluation reaches its
/// [`Env::subquery`] (`a OR EXISTS {..}` runs nothing when `a` is TRUE), at most once per row
/// ([`SubqueryAnswers`]); the evaluation stops at the first unknown one, the child runs on the
/// step's input traverser (one unit of it, short-circuiting at its first result like
/// `filter(__..)`), and the row is evaluated again. The children run on the same execution
/// context, so every traversal limit, the deadline and cancellation apply inside them.
///
/// A `COUNT { }` child ends with `count()` (bulk counted): it runs on one unit of the input
/// traverser and its one result is the count, `0` when the body matches nothing
/// ([`counted_run`]). A `COLLECT { }` child ends with `gql:value` of its one returned column
/// and collects like a pattern comprehension.
///
/// A pattern comprehension's child ends with `gql:value(expr)` (its `|` part): it runs to the
/// end on one unit of the input traverser, and its results, in arrival order (the child's
/// order, unspecified across plans, D18), become the list, each bulk expanded. The list is
/// checked against `bulk.max_expansion`, `traversal.max_materialized_bytes` and the memory
/// budget before it is built ([`collect_list`]), like `collect_list()` in `gql:aggregate`. An
/// import that is `NULL` (absent) matches nothing: the list is empty.
pub(crate) struct SubqueryHost<'g, S: GraphStorage + 'g> {
    children: Vec<AnonymousTraversal<'g, S>>,
    /// Per child: what it answers.
    modes: Vec<ChildMode>,
}

/// What a hosted child answers ([`SubqueryHost::new`] reads it from the child's last step).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChildMode {
    /// `EXISTS`: whether it gives a result (short-circuiting at the first one).
    Exists,
    /// `COUNT { }`: the one value its closing `count()` gives (stage 2d).
    Count,
    /// A pattern comprehension or `COLLECT { }`: the list of the values its closing
    /// `gql:value` gives.
    Collect,
}

impl<'g, S: GraphStorage + 'g> Clone for SubqueryHost<'g, S> {
    fn clone(&self) -> Self {
        Self {
            children: self.children.clone(),
            modes: self.modes.clone(),
        }
    }
}

impl<'g, S: GraphStorage + 'g> Default for SubqueryHost<'g, S> {
    fn default() -> Self {
        Self {
            children: Vec::new(),
            modes: Vec::new(),
        }
    }
}

impl<'g, S: GraphStorage + 'g> SubqueryHost<'g, S> {
    /// The host of `children`; child `i` answers [`Env::subquery`]`(i)`. A child ending with
    /// `gql:value` (only the lowering of a pattern comprehension or a `COLLECT { }` emits it,
    /// always last) collects a list; a child ending with `count()` (only the lowering of a
    /// `COUNT { }` closes a body with it) answers that count; every other child answers
    /// `EXISTS`. The mode is read here, before the optimizer may fuse the closing `count()`.
    pub(crate) fn new(children: Vec<AnonymousTraversal<'g, S>>) -> Self {
        use crate::traversal::step::GraphStep;
        let modes = children
            .iter()
            .map(|child| match child.steps.last() {
                Some(GraphStep::Map(GQL_VALUE, _)) => ChildMode::Collect,
                Some(GraphStep::Count(..)) => ChildMode::Count,
                _ => ChildMode::Exists,
            })
            .collect();
        Self { children, modes }
    }

    /// Whether it hosts no subquery.
    pub(crate) fn is_empty(&self) -> bool {
        self.children.is_empty()
    }

    /// The children, for `child_traversals_mut` (in subquery order).
    pub(crate) fn children_mut(&mut self) -> Vec<&mut AnonymousTraversal<'g, S>> {
        self.children.iter_mut().collect()
    }

    /// What the children read from the path of the step's input (each tests existence only:
    /// its output does not continue the path), for `analyze_nested_paths`.
    pub(crate) fn analyze_paths(&mut self, analysis: &PathAnalysis) -> PathRequirement {
        self.children
            .iter_mut()
            .fold(PathRequirement::None, |need, child| {
                need.union(child.analyze_paths(analysis, PathRequirement::None))
            })
    }

    /// Runs `eval` on the row of `source` (variables bound by `bindings`), running every
    /// subquery the evaluation reaches first (see the type docs). `answers` holds what the row
    /// ran so far: the caller starts each traverser with the default and passes the same one to
    /// every expression of that traverser, so a subquery runs at most once per row. A limit
    /// error the row kept wins, as after any evaluation (`Row::finish`).
    pub(crate) fn evaluate<T, E>(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: &Traverser<S>,
        bindings: &[Binding],
        answers: &mut SubqueryAnswers<S>,
        mut eval: impl FnMut(&Row<'_, '_, S>) -> Result<T, E>,
    ) -> TraverserResult<Result<T, E>> {
        loop {
            let (result, pending) = {
                let row = Row::with_answers(context, source, bindings, Some(&*answers));
                let result = eval(&row);
                row.finish()?;
                (result, row.pending.get())
            };
            // Only an evaluation that stopped asks for a subquery (the evaluator never drops an
            // error), so nothing `eval` did is repeated.
            // The lowering hosts every subquery it compiles (at most `MAX_HOSTED_SUBQUERIES`);
            // an index without a child keeps the evaluator's error (a guard only).
            match (
                pending.and_then(|index| Some((index, self.children.get(index)?))),
                result,
            ) {
                (Some((index, child)), Err(error)) => {
                    let mode = self.modes.get(index).copied().unwrap_or(ChildMode::Exists);
                    let recorded = match mode {
                        ChildMode::Collect => {
                            let list = collect_list(context, child, source, bindings)?;
                            answers.set_value(index, list)
                        }
                        ChildMode::Count => {
                            let count = counted_run(context, child, source)?;
                            answers.set_value(index, count)
                        }
                        ChildMode::Exists => {
                            let found = child.exists(context, source)?;
                            answers.set(index, found)
                        }
                    };
                    if !recorded {
                        return Ok(Err(error));
                    }
                }
                (_, result) => return Ok(result),
            }
        }
    }
}

/// The step name of a pattern comprehension's `|` part (the last step of its child).
pub(crate) const GQL_VALUE: &str = "gql:value";

/// The count a `COUNT { }` subquery's `child` (its body closed by `count()`, bulk counted) gives
/// for the row of `source`: the child runs on one unit of `source` (the count is per row, not
/// multiplied by the row's bulk); its one result is the count (`0` when the body matches
/// nothing).
fn counted_run<S: GraphStorage>(
    context: &mut TraverserContext<'_, S>,
    child: &AnonymousTraversal<'_, S>,
    source: &Traverser<S>,
) -> TraverserResult<RowValue<S>> {
    let unit;
    let source_unit = if source.bulk == 1 {
        source
    } else {
        unit = Traverser::with_parts(source.value.clone(), source.path, 1, source.sack);
        &unit
    };
    let produced = child.apply_to(context, source_unit)?;
    Ok(match produced.into_iter().next().map(|item| item.value) {
        Some(TraverserValue::Computed(count, _)) => Value::from_property(count),
        _ => Value::Scalar(ElementProperty::from(0_i64)),
    })
}

/// The list a pattern comprehension's `child` builds for the row of `source` ([`SubqueryHost`]):
/// the child runs on one unit of `source`; its results, in arrival order, each repeated by its
/// bulk, are checked against `bulk.max_expansion`, `traversal.max_materialized_bytes` and the
/// memory budget before the list is built.
fn collect_list<S: GraphStorage>(
    context: &mut TraverserContext<'_, S>,
    child: &AnonymousTraversal<'_, S>,
    source: &Traverser<S>,
    bindings: &[Binding],
) -> TraverserResult<RowValue<S>> {
    let unit;
    let source_unit = if source.bulk == 1 {
        source
    } else {
        unit = Traverser::with_parts(source.value.clone(), source.path, 1, source.sack);
        &unit
    };
    let produced = child.apply_to(context, source_unit)?;
    let values = if produced.iter().all(|item| item.bulk == 1) {
        // `bulk::expand` returns these untouched without a size check: check them here.
        let max = context.options.max_materialized_bytes;
        let each = std::mem::size_of::<TraverserValue<S>>() as u64;
        let mut bytes: u64 = 0;
        for item in &produced {
            let estimate =
                crate::traversal::step::profile::estimate_traverser_value_bytes(&item.value);
            bytes = bytes.saturating_add(each.saturating_add(estimate));
            if bytes > max {
                return Err(bulk::materialized_bytes_error(max, bytes));
            }
        }
        if let Some(budget) = &context.options.memory
            && bytes > 0
        {
            budget.check(bytes)?;
        }
        produced.into_iter().map(|item| item.value).collect()
    } else {
        bulk::expand_values_in(context, produced, GQL_VALUE)?
    };
    let row = Row::new(context, source, bindings);
    let list: Vec<RowValue<S>> = values.iter().map(|value| row.value(value)).collect();
    row.finish()?;
    Ok(Value::List(list))
}

/// `value` as the traverser value a row map holds (see the module docs).
pub(crate) fn into_traverser_value<S: GraphStorage>(value: RowValue<S>) -> TraverserValue<S> {
    match value {
        Value::Null => TraverserValue::Computed(ElementProperty::Null, None),
        Value::Scalar(property) => TraverserValue::Computed(property, None),
        Value::Element(GqlElement::Vertex(v)) => TraverserValue::Vertex(v),
        Value::Element(GqlElement::Edge(e)) => TraverserValue::Edge(e),
        Value::Path(elements) => TraverserValue::Path(TraverserPath::from_objects(
            elements.into_iter().map(|element| match element {
                GqlElement::Vertex(v) => TraverserValue::Vertex(v),
                GqlElement::Edge(e) => TraverserValue::Edge(e),
            }),
        )),
        Value::List(items) => {
            TraverserValue::Array(items.into_iter().map(into_traverser_value).collect())
        }
        Value::Map(entries) => {
            let mut keys = Vec::with_capacity(entries.len());
            let mut values = Vec::with_capacity(entries.len());
            for (key, value) in entries {
                keys.push(RecordKey::Name(key));
                values.push(into_traverser_value(value));
            }
            TraverserValue::Record {
                keys: Arc::new(keys),
                values,
            }
        }
    }
}

/// A sort key value, detached from the graph: elements are their kind and id.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum OrderValue {
    /// `NULL`.
    Null,
    /// A scalar.
    Scalar(ElementProperty),
    /// A list.
    List(Vec<OrderValue>),
    /// A map in written key order.
    Map(Vec<(CompactString, OrderValue)>),
    /// A vertex or an edge, by id.
    Element(ElementKind, CompactString),
    /// A path: its elements in order.
    Path(Vec<OrderValue>),
}

impl OrderValue {
    /// The sort key of `value`, element ids read through `row`.
    pub(crate) fn of<S: GraphStorage>(value: RowValue<S>, row: &Row<'_, '_, S>) -> Self {
        match value {
            Value::Null => OrderValue::Null,
            Value::Scalar(property) => OrderValue::Scalar(property),
            Value::List(items) => {
                OrderValue::List(items.into_iter().map(|v| OrderValue::of(v, row)).collect())
            }
            Value::Map(entries) => OrderValue::Map(
                entries
                    .into_iter()
                    .map(|(key, value)| (key, OrderValue::of(value, row)))
                    .collect(),
            ),
            Value::Element(element) => {
                OrderValue::Element(row.kind(&element), row.element_id(&element))
            }
            Value::Path(elements) => OrderValue::Path(
                elements
                    .into_iter()
                    .map(|element| {
                        OrderValue::Element(row.kind(&element), row.element_id(&element))
                    })
                    .collect(),
            ),
        }
    }

    /// The kind's place in the global order of types (see the module docs).
    fn rank(&self) -> u8 {
        match self {
            OrderValue::Map(_) => rank::MAP,
            OrderValue::Element(kind, _) => rank::element(*kind),
            OrderValue::List(_) => rank::LIST,
            OrderValue::Path(_) => rank::PATH,
            OrderValue::Scalar(property) => rank::scalar(property),
            OrderValue::Null => rank::NULL,
        }
    }
}

/// The places of the kinds in the global order of types (see the module docs).
mod rank {
    use crate::{ElementKind, ElementProperty};

    pub(super) const MAP: u8 = 0;
    pub(super) const LIST: u8 = 3;
    pub(super) const PATH: u8 = 4;
    pub(super) const NULL: u8 = 12;

    pub(super) fn element(kind: ElementKind) -> u8 {
        match kind {
            ElementKind::Vertex => 1,
            ElementKind::Edge => 2,
        }
    }

    pub(super) fn scalar(property: &ElementProperty) -> u8 {
        match property {
            ElementProperty::Temporal(_) => 5,
            ElementProperty::Uuid(_) => 6,
            ElementProperty::String(_)
            | ElementProperty::Compressed(_)
            | ElementProperty::Deduped(_) => 7,
            ElementProperty::Boolean(_) => 8,
            ElementProperty::Number(_) => 9,
            ElementProperty::Vector(_) => 10,
            // Never a scalar of the evaluator; ranked so the order stays total.
            ElementProperty::Null
            | ElementProperty::Object(_)
            | ElementProperty::Array(_)
            | ElementProperty::Vertex(_)
            | ElementProperty::Edge(_)
            | ElementProperty::Map(_) => 11,
        }
    }
}

/// Two scalars in the total order: by value where they compare, else the crate's fixed order.
fn compare_scalar_total(a: &ElementProperty, b: &ElementProperty) -> Ordering {
    compare_scalars(a, b).unwrap_or_else(|| compare_element_property(a, b))
}

/// The total `ORDER BY` order, ascending (see the module docs).
pub(crate) fn compare_order(a: &OrderValue, b: &OrderValue) -> Ordering {
    let by_rank = a.rank().cmp(&b.rank());
    if by_rank != Ordering::Equal {
        return by_rank;
    }
    match (a, b) {
        (OrderValue::Scalar(a), OrderValue::Scalar(b)) => compare_scalar_total(a, b),
        (OrderValue::Element(_, a), OrderValue::Element(_, b)) => a.cmp(b),
        (OrderValue::List(a), OrderValue::List(b)) | (OrderValue::Path(a), OrderValue::Path(b)) => {
            a.iter()
                .zip(b)
                .map(|(a, b)| compare_order(a, b))
                .find(|order| *order != Ordering::Equal)
                .unwrap_or_else(|| a.len().cmp(&b.len()))
        }
        (OrderValue::Map(a), OrderValue::Map(b)) => a
            .iter()
            .zip(b)
            .map(|((ka, va), (kb, vb))| ka.cmp(kb).then_with(|| compare_order(va, vb)))
            .find(|order| *order != Ordering::Equal)
            .unwrap_or_else(|| a.len().cmp(&b.len())),
        // Equal ranks hold equal variants; `NULL` equals `NULL`.
        _ => Ordering::Equal,
    }
}

/// The `ORDER BY` order ([`compare_order`]) over evaluator values, element ids read through
/// `env`: what `min` / `max` compare by (C5). Recursive over nested lists and maps like the
/// values themselves.
pub(crate) fn compare_values<En: Env>(
    a: &Value<En::Element>,
    b: &Value<En::Element>,
    env: &En,
) -> Ordering {
    let rank_of = |value: &Value<En::Element>| match value {
        Value::Map(_) => rank::MAP,
        Value::Element(element) => rank::element(env.kind(element)),
        Value::List(_) => rank::LIST,
        Value::Path(_) => rank::PATH,
        Value::Scalar(property) => rank::scalar(property),
        Value::Null => rank::NULL,
    };
    let by_rank = rank_of(a).cmp(&rank_of(b));
    if by_rank != Ordering::Equal {
        return by_rank;
    }
    let by_id = |a: &En::Element, b: &En::Element| env.element_id(a).cmp(&env.element_id(b));
    match (a, b) {
        (Value::Scalar(a), Value::Scalar(b)) => compare_scalar_total(a, b),
        (Value::Element(a), Value::Element(b)) => by_id(a, b),
        (Value::List(a), Value::List(b)) => a
            .iter()
            .zip(b)
            .map(|(a, b)| compare_values(a, b, env))
            .find(|order| *order != Ordering::Equal)
            .unwrap_or_else(|| a.len().cmp(&b.len())),
        (Value::Path(a), Value::Path(b)) => a
            .iter()
            .zip(b)
            .map(|(a, b)| by_id(a, b))
            .find(|order| *order != Ordering::Equal)
            .unwrap_or_else(|| a.len().cmp(&b.len())),
        (Value::Map(a), Value::Map(b)) => a
            .iter()
            .zip(b)
            .map(|((ka, va), (kb, vb))| ka.cmp(kb).then_with(|| compare_values(va, vb, env)))
            .find(|order| *order != Ordering::Equal)
            .unwrap_or_else(|| a.len().cmp(&b.len())),
        // Equal ranks hold equal variants; `NULL` equals `NULL`.
        _ => Ordering::Equal,
    }
}
