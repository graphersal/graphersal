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
//!   names a returned alias).
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
//! temporal < uuid < string < boolean < number < vector < other < `NULL`), then within the kind:
//! numbers by value across integer and float, strings by code point, `false < true`, temporal
//! values of one type by their own order, elements by id, lists element-wise then by length,
//! maps entry-wise (key, then value) then by size. Values of one kind that do not compare (two
//! temporal types, `NaN`) fall back to the crate's fixed total order (`compare_element_property`),
//! so a sort over mixed input is deterministic. `NULL` is the largest value: last ascending, first
//! descending, as in Neo4j (ISO GQL leaves the default null order to the implementation).

// Used from the GQL lowering (T9a) on; until then only the tests reach it.
#![cfg_attr(not(test), allow(dead_code))]

use std::borrow::Cow;
use std::cell::RefCell;
use std::cmp::Ordering;
use std::sync::Arc;

use super::eval::{Compiled, Env, EvalError, Scope, Value, status};
use crate::error::{BoxedTraverserError, GqlErrorBuilder, GqlErrorKind, TraverserError};
use crate::graph::GraphStorage;
use crate::traversal::{Traverser, TraverserContext};
use crate::value::scalar::compare_scalars;
use crate::value::{RecordKey, TraverserValue, compare_element_property};
use crate::{CompactString, ElementKind, ElementProperty};

#[cfg(test)]
mod tests;

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
    let mut labels: Vec<CompactString> = slots
        .into_iter()
        .filter_map(|slot| match bindings.get(slot) {
            Some(Binding::Label(label)) => Some(label.clone()),
            Some(Binding::Current | Binding::Column(_)) | None => None,
        })
        .collect();
    labels.sort_unstable();
    labels.dedup();
    labels
}

/// A [`Scope`] for [`compile`](super::eval::compile): variable `names[slot]` is slot `slot`,
/// parameters come from `parameter`.
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
        // `DIVISION_BY_ZERO`, `OUT_OF_RANGE`, `INVALID_VALUE_TYPE`: data exceptions.
        _ => GqlErrorKind::Data,
    };
    GqlErrorBuilder::new(kind, error.span.start..error.span.end, error.message)
        .gqlstatus(error.gqlstatus)
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
}

impl<'a, 'c, S: GraphStorage> Row<'a, 'c, S> {
    /// The row of `traverser`, its variables bound by `bindings`.
    pub(crate) fn new(
        context: &'a TraverserContext<'c, S>,
        traverser: &'a Traverser<S>,
        bindings: &'a [Binding],
    ) -> Self {
        Row {
            context,
            traverser,
            bindings,
            failure: RefCell::new(None),
        }
    }

    /// The first value that could not be read, if any.
    pub(crate) fn finish(&self) -> Result<(), BoxedTraverserError> {
        match self.failure.borrow_mut().take() {
            Some(error) => Err(error),
            None => Ok(()),
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
        }
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
            TraverserValue::VertexProperty(..)
            | TraverserValue::EdgeProperty(..)
            | TraverserValue::PathProperty(_)
            | TraverserValue::Path(_)
            | TraverserValue::Object(_)
            | TraverserValue::Entry(_)
            | TraverserValue::Schema(_)
            | TraverserValue::Graph(_) => match value.into_node_property(&*self.context.storage) {
                Ok(property) => Value::from_property(property),
                Err(error) => {
                    self.failure.borrow_mut().get_or_insert(error);
                    Value::Null
                }
            },
        }
    }
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
        id.map(CompactString::from).unwrap_or_default()
    }

    fn property(&self, element: &GqlElement<S>, key: &str) -> Option<ElementProperty> {
        let storage = &*self.context.storage;
        let properties = match element {
            GqlElement::Vertex(v) => storage.vertex_properties(*v),
            GqlElement::Edge(e) => storage.edge_properties(*e),
        };
        properties?.get(key).cloned()
    }

    fn has_label(&self, element: &GqlElement<S>, label: &str) -> bool {
        let storage = &*self.context.storage;
        match element {
            GqlElement::Vertex(v) => storage.vertex_has_label(*v, label),
            GqlElement::Edge(e) => storage.edge_label(*e) == Some(label),
        }
    }

    fn has_any_label(&self, element: &GqlElement<S>) -> bool {
        let storage = &*self.context.storage;
        match element {
            GqlElement::Vertex(v) => !storage.vertex_labels(*v).is_empty(),
            GqlElement::Edge(e) => storage.edge_label(*e).is_some_and(|l| !l.is_empty()),
        }
    }

    fn labels(&self, element: &GqlElement<S>) -> Vec<CompactString> {
        let storage = &*self.context.storage;
        match element {
            GqlElement::Vertex(v) => storage.vertex_labels(*v).to_vec(),
            GqlElement::Edge(e) => storage
                .edge_label(*e)
                .map(|label| vec![CompactString::from(label)])
                .unwrap_or_default(),
        }
    }
}

/// `value` as the traverser value a row map holds (see the module docs).
pub(crate) fn into_traverser_value<S: GraphStorage>(value: RowValue<S>) -> TraverserValue<S> {
    match value {
        Value::Null => TraverserValue::Computed(ElementProperty::Null, None),
        Value::Scalar(property) => TraverserValue::Computed(property, None),
        Value::Element(GqlElement::Vertex(v)) => TraverserValue::Vertex(v),
        Value::Element(GqlElement::Edge(e)) => TraverserValue::Edge(e),
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
        }
    }

    /// The kind's place in the global order of types (see the module docs).
    fn rank(&self) -> u8 {
        match self {
            OrderValue::Map(_) => 0,
            OrderValue::Element(ElementKind::Vertex, _) => 1,
            OrderValue::Element(ElementKind::Edge, _) => 2,
            OrderValue::List(_) => 3,
            OrderValue::Scalar(property) => match property {
                ElementProperty::Temporal(_) => 4,
                ElementProperty::Uuid(_) => 5,
                ElementProperty::String(_)
                | ElementProperty::Compressed(_)
                | ElementProperty::Deduped(_) => 6,
                ElementProperty::Boolean(_) => 7,
                ElementProperty::Number(_) => 8,
                ElementProperty::Vector(_) => 9,
                // Never a scalar of the evaluator; ranked so the order stays total.
                ElementProperty::Null
                | ElementProperty::Object(_)
                | ElementProperty::Array(_)
                | ElementProperty::Vertex(_)
                | ElementProperty::Edge(_)
                | ElementProperty::Map(_) => 10,
            },
            OrderValue::Null => 11,
        }
    }
}

/// The total `ORDER BY` order, ascending (see the module docs).
pub(crate) fn compare_order(a: &OrderValue, b: &OrderValue) -> Ordering {
    let by_rank = a.rank().cmp(&b.rank());
    if by_rank != Ordering::Equal {
        return by_rank;
    }
    match (a, b) {
        (OrderValue::Scalar(a), OrderValue::Scalar(b)) => {
            compare_scalars(a, b).unwrap_or_else(|| compare_element_property(a, b))
        }
        (OrderValue::Element(_, a), OrderValue::Element(_, b)) => a.cmp(b),
        (OrderValue::List(a), OrderValue::List(b)) => a
            .iter()
            .zip(b)
            .map(|(a, b)| compare_order(a, b))
            .find(|order| *order != Ordering::Equal)
            .unwrap_or_else(|| a.len().cmp(&b.len())),
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
