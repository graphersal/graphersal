//! Shared numeric resolution and accumulation for aggregation barrier steps.
//!
//! `max()`, `min()`, `sum(), `mean()` and `median()` consume the same stream shapes and fail in
//! the same ways. Resolving and accumulating them in one place keeps their diagnostics identical
//! and lets every failure carry the offending value together with the graph element it was read
//! from, instead of reporting a bare type mismatch.

use std::cmp::Ordering;

use compact_str::CompactString;

use crate::error::{TraverserError, TraverserResult, ValueError, ValueOrigin};
use crate::graph::GraphStorage;
use crate::traversal::{Traverser, TraverserContext};
use crate::value::scalar::compare_scalars;
use crate::value::{ElementProperty, N, TraverserValue};

/// Maximum length of a rendered value in a diagnostic message.
const VALUE_PREVIEW_LIMIT: usize = 96;

/// A numeric stream item, preserving the domain (integer or floating-point) it was read in.
#[derive(Clone, Copy, Debug)]
pub(crate) enum StreamNumber {
    Int(i64),
    Float(f64),
}

impl StreamNumber {
    /// Widens the value to `f64` for aggregations that operate on a single float domain.
    pub(crate) fn as_f64(self) -> f64 {
        match self {
            StreamNumber::Int(value) => value as f64,
            StreamNumber::Float(value) => value,
        }
    }

    /// The value repeated `bulk` times and summed, in its own domain: an integer multiplies with
    /// `checked_mul` (overflow is the same `InvalidNumber` error `sum()` reports when adding), a
    /// float multiplies as `f64`. Used by the reducers so a merged traverser counts as many
    /// times as it stands for.
    pub(crate) fn times(self, bulk: u64, step: &'static str) -> TraverserResult<StreamNumber> {
        if bulk == 1 {
            return Ok(self);
        }
        match self {
            StreamNumber::Int(value) => i64::try_from(bulk)
                .ok()
                .and_then(|bulk| value.checked_mul(bulk))
                .map(StreamNumber::Int)
                .ok_or_else(|| {
                    Box::new(TraverserError::Value(ValueError::InvalidNumber {
                        reason: format!(
                            "'{step}' overflowed the signed 64-bit integer range at value {value}"
                        ),
                    }))
                }),
            StreamNumber::Float(value) => Ok(StreamNumber::Float(value * bulk as f64)),
        }
    }
}

/// Renders a property value for a diagnostic message, truncating long values.
pub(crate) fn preview(value: &ElementProperty) -> String {
    let rendered = crate::value::literal::rhai_literal(value);
    if rendered.chars().count() <= VALUE_PREVIEW_LIMIT {
        return rendered;
    }
    let truncated: String = rendered.chars().take(VALUE_PREVIEW_LIMIT).collect();
    format!("{truncated}…")
}

/// Lazy reference to the element a stream value was read from.
///
/// Resolving an ID and a label into owned strings costs two allocations per stream item, so the
/// provenance is carried as plain graph indices and only materialized into a [`ValueOrigin`] when
/// an error is actually reported.
pub(crate) enum ValueSource<S: GraphStorage> {
    /// The value carries no element provenance, e.g. an injected constant.
    Anonymous,
    Vertex(S::VertexId, Option<CompactString>),
    Edge(S::EdgeId, Option<CompactString>),
}

impl<S: GraphStorage> ValueSource<S> {
    /// Materializes the provenance for a diagnostic message. An id or label the storage cannot
    /// read is left out (`None`, as for an element without one): the caller's error is the one
    /// to report, only without that detail, never the storage's error.
    pub(crate) fn origin(&self, context: &TraverserContext<'_, S>) -> Option<ValueOrigin> {
        match self {
            ValueSource::Anonymous => None,
            ValueSource::Vertex(node, property) => Some(ValueOrigin::vertex(
                detail(context.storage.vertex_id(*node), "the id of a vertex"),
                detail(context.storage.vertex_label(*node), "the label of a vertex"),
                property.as_ref().map(|p| p.to_string()),
            )),
            ValueSource::Edge(edge, property) => Some(ValueOrigin::edge(
                detail(context.storage.edge_id(*edge), "the id of an edge"),
                detail(context.storage.edge_label(*edge), "the label of an edge"),
                property.as_ref().map(|p| p.to_string()),
            )),
        }
    }
}

/// One detail of a diagnostic read from the storage: `None` when the element has none or the
/// storage cannot read it (logged, [`crate::error::unreadable_detail`]).
fn detail(read: Result<Option<&str>, crate::error::GraphError>, what: &str) -> Option<String> {
    match read {
        Ok(text) => text.map(str::to_string),
        Err(error) => {
            crate::error::unreadable_detail(what, &error);
            None
        }
    }
}

/// Converts a property value into a stream number, reporting the value and its origin on failure.
///
/// Null values yield `None`, matching Gremlin semantics where missing values are skipped rather
/// than aborting the aggregation.
pub(crate) fn number_of<S: GraphStorage>(
    context: &TraverserContext<'_, S>,
    value: &ElementProperty,
    source: &ValueSource<S>,
) -> TraverserResult<Option<StreamNumber>> {
    match value {
        ElementProperty::Null => Ok(None),
        ElementProperty::Number(number) => {
            let number = match &number.n {
                N::NegInt(value) => StreamNumber::Int(*value),
                N::Float(value) => StreamNumber::Float(**value),
            };
            Ok(Some(number))
        }
        other => Err(Box::new(TraverserError::cast_exception_at(
            &["number"],
            crate::graph::schema::value_type_name(other),
            Some(preview(other)),
            source.origin(context),
        ))),
    }
}

/// Resolves a stream item into a number together with a lazy reference to the element it came from.
///
/// Returns `Ok(None)` for null values and for properties that are absent on the element, so that
/// sparse properties do not fail an aggregation.
pub(crate) fn resolve_number<S: GraphStorage>(
    context: &TraverserContext<'_, S>,
    traverser: Traverser<S>,
) -> TraverserResult<Option<(StreamNumber, ValueSource<S>)>> {
    resolve_value_number(context, traverser.value)
}

/// [`resolve_number`] for a bare stream value, e.g. one element of a folded list reduced by
/// `sum(local)`.
pub(crate) fn resolve_value_number<S: GraphStorage>(
    context: &TraverserContext<'_, S>,
    value: TraverserValue<S>,
) -> TraverserResult<Option<(StreamNumber, ValueSource<S>)>> {
    match value {
        TraverserValue::Computed(value, node) => {
            let source = match node {
                Some(node) => ValueSource::Vertex(node, None),
                None => ValueSource::Anonymous,
            };
            Ok(number_of(context, &value, &source)?.map(|number| (number, source)))
        }
        TraverserValue::VertexProperty(property, node, _) => {
            let value = context
                .storage
                .vertex_properties(node)?
                .and_then(|properties| properties.get(&property));
            match value {
                Some(value) => {
                    let source = ValueSource::Vertex(node, Some(property));
                    Ok(number_of(context, value, &source)?.map(|number| (number, source)))
                }
                None => Ok(None),
            }
        }
        TraverserValue::EdgeProperty(property, edge, _) => {
            let value = context
                .storage
                .edge_properties(edge)?
                .and_then(|properties| properties.get(&property));
            match value {
                Some(value) => {
                    let source = ValueSource::Edge(edge, Some(property));
                    Ok(number_of(context, value, &source)?.map(|number| (number, source)))
                }
                None => Ok(None),
            }
        }
        other => Err(Box::new(TraverserError::cast_exception_str(
            &["computed", "vertexProperty", "edgeProperty"],
            other.type_name().to_string(),
        ))),
    }
}

/// Accumulator of `mean()`: the arithmetic mean of every value, in the floating-point domain
/// (integers and floats may mix), so the result is always a double.
pub(crate) struct MeanAccumulator {
    total: f64,
    count: u64,
}

impl MeanAccumulator {
    /// Creates an accumulator that has not seen any value yet.
    pub(crate) fn new() -> Self {
        MeanAccumulator {
            total: 0.0,
            count: 0,
        }
    }

    /// Folds one value into the mean.
    pub(crate) fn push(&mut self, number: StreamNumber) {
        self.total += number.as_f64();
        self.count += 1;
    }

    /// Folds `number` in `bulk` times: the total grows by `number * bulk`, the count by `bulk`.
    pub(crate) fn push_n(&mut self, number: StreamNumber, bulk: u64) -> TraverserResult<()> {
        self.count = self.count.checked_add(bulk).ok_or_else(|| {
            Box::new(TraverserError::BulkOverflow {
                step: "mean".to_string(),
            })
        })?;
        self.total += number.as_f64() * bulk as f64;
        Ok(())
    }

    /// The mean, or `None` when no numeric value was seen — TinkerPop's `mean()` emits nothing on
    /// an empty (or all-null) numeric stream (`MeanGlobalStep`, a seedless reducing barrier).
    pub(crate) fn finish(self) -> Option<ElementProperty> {
        if self.count == 0 {
            return None;
        }
        Some(ElementProperty::Number(
            (self.total / self.count as f64).into(),
        ))
    }
}

/// Accumulator of `sum()`, `min()` and `max()` over numbers.
///
/// Integers stay exact (`i64`, overflow is an `InvalidNumber` error). The first float promotes
/// the accumulator to `f64` and every later integer widens, like TinkerPop's `NumberHelper`; the
/// result is independent of the stream order for `min`/`max`/`sum`. A promoted stream reports a
/// float even when the extremum came from an integer (`max` of `29, 0.5` is `29.0`).
pub(crate) enum WideningNumeric {
    Empty,
    Int(i64),
    Float(f64),
}

impl WideningNumeric {
    /// Creates an accumulator that has not seen any value yet.
    pub(crate) fn new() -> Self {
        WideningNumeric::Empty
    }

    /// Folds one value into the accumulator using the domain-specific combinators.
    pub(crate) fn push(
        &mut self,
        step: &'static str,
        number: StreamNumber,
        combine_int: fn(i64, i64) -> Option<i64>,
        combine_float: fn(f64, f64) -> f64,
    ) -> TraverserResult<()> {
        *self = match (&*self, number) {
            (WideningNumeric::Empty, StreamNumber::Int(value)) => WideningNumeric::Int(value),
            (WideningNumeric::Empty, StreamNumber::Float(value)) => WideningNumeric::Float(value),
            (WideningNumeric::Int(accumulated), StreamNumber::Int(value)) => {
                match combine_int(*accumulated, value) {
                    Some(combined) => WideningNumeric::Int(combined),
                    None => {
                        return Err(Box::new(TraverserError::Value(ValueError::InvalidNumber {
                            reason: format!(
                                "'{step}' overflowed the signed 64-bit integer range at value {value}"
                            ),
                        })));
                    }
                }
            }
            (WideningNumeric::Float(accumulated), StreamNumber::Float(value)) => {
                WideningNumeric::Float(combine_float(*accumulated, value))
            }
            (WideningNumeric::Int(accumulated), StreamNumber::Float(value)) => {
                WideningNumeric::Float(combine_float(*accumulated as f64, value))
            }
            (WideningNumeric::Float(accumulated), StreamNumber::Int(value)) => {
                WideningNumeric::Float(combine_float(*accumulated, value as f64))
            }
        };
        Ok(())
    }

    /// Produces the aggregated value, or `None` when the stream carried no numeric value at all
    /// (TinkerPop's `sum()`/`min()`/`max()` emit nothing on an empty or all-null stream).
    pub(crate) fn finish(self) -> Option<ElementProperty> {
        match self {
            WideningNumeric::Empty => None,
            WideningNumeric::Int(value) => Some(ElementProperty::Number(value.into())),
            WideningNumeric::Float(value) => Some(ElementProperty::Number(value.into())),
        }
    }
}

/// Which extremum an [`ExtremumAccumulator`] keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Extremum {
    Min,
    Max,
}

impl Extremum {
    fn name(self) -> &'static str {
        match self {
            Extremum::Min => "min",
            Extremum::Max => "max",
        }
    }

    /// The ordering of a candidate against the held value that makes it the new extremum.
    fn pick(self) -> Ordering {
        match self {
            Extremum::Min => Ordering::Less,
            Extremum::Max => Ordering::Greater,
        }
    }
}

/// The kinds `min()`/`max()` can compare, named in the error of an incomparable value.
const COMPARABLE_KINDS: &[&str] = &[
    "number",
    "string",
    "boolean",
    "uuid",
    "date",
    "localdatetime",
    "datetime",
    "time",
    "duration",
];

/// Accumulator of `min()` and `max()`: numbers (widening int/float) or, alternatively, strings,
/// booleans or UUIDs of one kind. `null` is skipped. A value of another kind than the one held,
/// or a graph element, path or container, is a `ValueError::IncomparableValues`.
pub(crate) struct ExtremumAccumulator {
    which: Extremum,
    numbers: WideningNumeric,
    other: Option<ElementProperty>,
}

impl ExtremumAccumulator {
    /// Creates an accumulator that has not seen any value yet.
    pub(crate) fn new(which: Extremum) -> Self {
        ExtremumAccumulator {
            which,
            numbers: WideningNumeric::new(),
            other: None,
        }
    }

    /// Folds one property value in; `null` is skipped. A string is compared in place and cloned
    /// only when it becomes the extremum.
    pub(crate) fn push_property<S: GraphStorage>(
        &mut self,
        value: &ElementProperty,
        context: &TraverserContext<'_, S>,
        source: &ValueSource<S>,
    ) -> TraverserResult<()> {
        let step = self.which.name();
        let held_kind = if self.other.is_some() {
            self.other.as_ref().map(kind_name)
        } else if matches!(self.numbers, WideningNumeric::Empty) {
            None
        } else {
            Some("number")
        };
        match value {
            ElementProperty::Null => Ok(()),
            ElementProperty::Number(_) => {
                if let Some(held) = held_kind.filter(|held| *held != "number") {
                    return Err(incomparable(step, &[held], value, context, source));
                }
                let Some(number) = number_of(context, value, source)? else {
                    return Ok(());
                };
                match self.which {
                    Extremum::Min => {
                        self.numbers
                            .push(step, number, |a, b| Some(a.min(b)), f64::min)
                    }
                    Extremum::Max => {
                        self.numbers
                            .push(step, number, |a, b| Some(a.max(b)), f64::max)
                    }
                }
            }
            ElementProperty::String(_)
            | ElementProperty::Compressed(_)
            | ElementProperty::Deduped(_)
            | ElementProperty::Boolean(_)
            | ElementProperty::Uuid(_)
            | ElementProperty::Temporal(_) => {
                match (&self.other, held_kind) {
                    (None, None) => self.other = Some(value.to_logical()),
                    (Some(held), _) if kind_name(held) == kind_name(value) => {
                        if compare_scalars(value, held) == Some(self.which.pick()) {
                            self.other = Some(value.to_logical());
                        }
                    }
                    (_, held) => {
                        let expected = held.unwrap_or("number");
                        return Err(incomparable(step, &[expected], value, context, source));
                    }
                }
                Ok(())
            }
            // A vector has no min/max (it orders in `order()`, but is not a scalar).
            ElementProperty::Vector(_)
            | ElementProperty::Object(_)
            | ElementProperty::Array(_)
            | ElementProperty::Vertex(_)
            | ElementProperty::Edge(_)
            | ElementProperty::Map(_) => {
                Err(incomparable(step, COMPARABLE_KINDS, value, context, source))
            }
        }
    }

    /// The extremum, or `None` when the stream carried no comparable value.
    pub(crate) fn finish(self) -> Option<ElementProperty> {
        match self.other {
            Some(value) => Some(value),
            None => self.numbers.finish(),
        }
    }
}

fn kind_name(value: &ElementProperty) -> &'static str {
    match value {
        ElementProperty::String(_)
        | ElementProperty::Compressed(_)
        | ElementProperty::Deduped(_) => "string",
        ElementProperty::Boolean(_) => "boolean",
        ElementProperty::Uuid(_) => "uuid",
        ElementProperty::Temporal(value) => value.kind().name(),
        ElementProperty::Number(_) => "number",
        ElementProperty::Vector(_) => "vector",
        ElementProperty::Null
        | ElementProperty::Object(_)
        | ElementProperty::Array(_)
        | ElementProperty::Vertex(_)
        | ElementProperty::Edge(_)
        | ElementProperty::Map(_) => "object",
    }
}

fn incomparable<S: GraphStorage>(
    step: &'static str,
    expected: &[&'static str],
    offending: &ElementProperty,
    context: &TraverserContext<'_, S>,
    source: &ValueSource<S>,
) -> crate::error::BoxedTraverserError {
    let origin = source.origin(context);
    Box::new(TraverserError::Value(ValueError::IncomparableValues {
        step,
        expected: expected.join(" or "),
        actual: crate::graph::schema::value_type_name(offending),
        value: Some(preview(offending)),
        origin: origin.map(Box::new),
    }))
}

/// Folds a stream value into a `min()`/`max()` accumulator. Lazy property handles are read in
/// place from storage; absent properties are skipped.
pub(crate) fn push_extremum_value<S: GraphStorage>(
    accumulator: &mut ExtremumAccumulator,
    context: &TraverserContext<'_, S>,
    value: TraverserValue<S>,
) -> TraverserResult<()> {
    match value {
        TraverserValue::Computed(value, node) => {
            let source = match node {
                Some(node) => ValueSource::Vertex(node, None),
                None => ValueSource::Anonymous,
            };
            accumulator.push_property(&value, context, &source)
        }
        TraverserValue::VertexProperty(property, node, _) => {
            let value = context
                .storage
                .vertex_properties(node)?
                .and_then(|properties| properties.get(&property));
            match value {
                Some(value) => {
                    let source = ValueSource::Vertex(node, Some(property));
                    accumulator.push_property(value, context, &source)
                }
                None => Ok(()),
            }
        }
        TraverserValue::EdgeProperty(property, edge, _) => {
            let value = context
                .storage
                .edge_properties(edge)?
                .and_then(|properties| properties.get(&property));
            match value {
                Some(value) => {
                    let source = ValueSource::Edge(edge, Some(property));
                    accumulator.push_property(value, context, &source)
                }
                None => Ok(()),
            }
        }
        other => Err(Box::new(TraverserError::Value(
            ValueError::IncomparableValues {
                step: accumulator.which.name(),
                expected: COMPARABLE_KINDS.join(" or "),
                actual: other.type_name().to_string(),
                value: None,
                origin: None,
            },
        ))),
    }
}
