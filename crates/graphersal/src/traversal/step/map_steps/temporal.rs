//! The shared parts of the date steps (`as_date`, `as_duration`, `date_add`, `date_diff`, `age`,
//! `date_trunc`, `date_part`, `at_zone`, `as_local`; book part "Dates and Times"): reading the
//! value a traverser holds without cloning it, the operand of `date_diff()`/`age()` (a constant
//! or the first result of a child traversal), the zone argument (parsed once, when the step is
//! built; an unknown zone is a plan-time error) and the error constructors.
//!
//! Every date step is a `Map` step whose `may_filter()` is true, so a downstream fused `count()`
//! still runs it and raises its errors (a wrong input is an error, never a silent count). The
//! arithmetic lives in `value/temporal/arith.rs`.

use std::borrow::Cow;

use compact_str::CompactString;

use crate::graph::GraphStorage;
use crate::traversal::optimizer::path_analysis::{PathAnalysis, PathRequirement};
use crate::traversal::step::local_scope::stored_value;
use crate::value::temporal::zone::Zone;
use crate::value::temporal::{Temporal, TemporalError};
use crate::*;

/// The error of a failed date operation.
pub(crate) fn temporal_error(step: &'static str, error: TemporalError) -> Box<TraverserError> {
    Box::new(TraverserError::Value(ValueError::Temporal { step, error }))
}

/// The error of an input a date step cannot take.
pub(crate) fn input_error(
    step: &'static str,
    operand: &'static str,
    expected: &'static str,
    actual: String,
) -> Box<TraverserError> {
    Box::new(TraverserError::Value(ValueError::TemporalInput {
        step,
        operand,
        expected,
        actual,
    }))
}

/// A value with its type for a diagnostic: `null`, `int64 7`, `string "x"`.
pub(crate) fn describe(value: &ElementProperty) -> String {
    match value {
        ElementProperty::Null => "null".to_string(),
        other => format!(
            "{} {}",
            crate::graph::schema::value_type_name(other),
            crate::traversal::step::barrier_steps::numeric::preview(other)
        ),
    }
}

/// The property value a traverser holds, borrowed from the storage where it lives; `Err` with
/// the stream kind (`vertex`, `path`, `list`, ...) for a value that is not a property value. An
/// absent property is `null`.
pub(crate) fn held_value<'a, S: GraphStorage>(
    context: &'a TraverserContext<'_, S>,
    value: &'a TraverserValue<S>,
) -> TraverserResult<Result<Cow<'a, ElementProperty>, &'static str>> {
    Ok(Ok(match value {
        TraverserValue::Computed(property, _) => Cow::Borrowed(property),
        TraverserValue::VertexProperty(_, _, kind) | TraverserValue::EdgeProperty(_, _, kind)
            if !kind.reads_property() =>
        {
            Cow::Owned(value.into_node_property(context.storage)?)
        }
        TraverserValue::VertexProperty(..) | TraverserValue::EdgeProperty(..) => {
            match stored_value(context, value) {
                Some(stored) => Cow::Borrowed(stored),
                None => Cow::Owned(ElementProperty::Null),
            }
        }
        TraverserValue::PathProperty(prop) => Cow::Borrowed(&prop.1),
        TraverserValue::Array(_) | TraverserValue::Path(_) => return Ok(Err("list")),
        TraverserValue::Object(_) | TraverserValue::Record { .. } | TraverserValue::Entry(_) => {
            return Ok(Err("map"));
        }
        other @ (TraverserValue::Vertex(_)
        | TraverserValue::Edge(_)
        | TraverserValue::Schema(_)
        | TraverserValue::Graph(_)) => return Ok(Err(other.type_name())),
    }))
}

/// The temporal value of the incoming traverser, or the [`ValueError::TemporalInput`] error.
pub(crate) fn incoming_temporal<S: GraphStorage>(
    context: &TraverserContext<'_, S>,
    value: &TraverserValue<S>,
    step: &'static str,
    expected: &'static str,
) -> TraverserResult<Temporal> {
    match held_value(context, value)? {
        Ok(held) => match held.as_ref() {
            ElementProperty::Temporal(temporal) => Ok(*temporal),
            other => Err(input_error(
                step,
                "incoming value",
                expected,
                describe(other),
            )),
        },
        Err(kind) => Err(input_error(
            step,
            "incoming value",
            expected,
            format!("a {kind}"),
        )),
    }
}

/// A temporal result value.
pub(crate) fn computed<S: GraphStorage>(value: Temporal) -> TraverserValue<S> {
    TraverserValue::Computed(ElementProperty::Temporal(value), None)
}

/// The traverser carrying `value` in place of `source`'s (path, bulk and sack kept).
pub(crate) fn replaced<S: GraphStorage>(
    source: Traverser<S>,
    value: TraverserValue<S>,
) -> Traverser<S> {
    Traverser::with_parts(value, source.path, source.bulk, source.sack)
}

/// A zone argument as written, parsed when the step is built.
#[derive(Clone)]
pub(crate) struct ZoneArg {
    text: CompactString,
    zone: Result<Zone, TemporalError>,
}

impl ZoneArg {
    /// Parses `text` (an IANA name or a UTC offset); a failure is reported by [`check`](Self::check).
    pub(crate) fn parse(text: &str) -> Self {
        ZoneArg {
            text: CompactString::new(text),
            zone: Zone::parse(text),
        }
    }

    /// The parsed zone, or the step's error.
    pub(crate) fn get(&self, step: &'static str) -> TraverserResult<&Zone> {
        self.zone
            .as_ref()
            .map_err(|error| temporal_error(step, error.clone()))
    }

    /// The plan-time check: an unknown zone fails before anything runs.
    pub(crate) fn check(&self, step: &'static str) -> TraverserResult<()> {
        self.get(step).map(|_| ())
    }

    /// The argument as a Rhai literal (`"Europe/Berlin"`).
    pub(crate) fn literal(&self) -> String {
        crate::value::literal::rhai_string(&self.text)
    }
}

/// The plan-time check of an optional zone.
pub(crate) fn check_zone(zone: &Option<ZoneArg>, step: &'static str) -> TraverserResult<()> {
    match zone {
        Some(zone) => zone.check(step),
        None => Ok(()),
    }
}

/// The zone of an optional argument, or the step's error.
pub(crate) fn zone_of<'a>(
    zone: &'a Option<ZoneArg>,
    step: &'static str,
) -> TraverserResult<Option<&'a Zone>> {
    match zone {
        Some(zone) => zone.get(step).map(Some),
        None => Ok(None),
    }
}

/// The second operand of `date_diff()` and `age()`: a constant or the first result of a child
/// traversal run on the incoming traverser (path included).
pub(crate) enum Operand<'g, S: GraphStorage + 'g> {
    /// A constant (a temporal value, or `null`).
    Value(ElementProperty),
    /// A child traversal whose first result is the operand.
    Traversal(AnonymousTraversal<'g, S>),
}

impl<'g, S: GraphStorage + 'g> Clone for Operand<'g, S> {
    fn clone(&self) -> Self {
        match self {
            Operand::Value(value) => Operand::Value(value.clone()),
            Operand::Traversal(traversal) => Operand::Traversal(traversal.clone()),
        }
    }
}

impl<'g, S: GraphStorage + 'g> fmt::Display for Operand<'g, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Operand::Value(value) => f.write_str(&crate::value::literal::rhai_literal(value)),
            Operand::Traversal(traversal) => write!(f, "{traversal}"),
        }
    }
}

impl<'g, S: GraphStorage + 'g> Operand<'g, S> {
    /// The operand for `source`: a temporal value, or `None` for `null`. Anything else is a
    /// [`ValueError::TemporalInput`].
    pub(crate) fn resolve(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: &Traverser<S>,
        step: &'static str,
    ) -> TraverserResult<Option<Temporal>> {
        let expected = "a date or time";
        match self {
            Operand::Value(ElementProperty::Temporal(value)) => Ok(Some(*value)),
            Operand::Value(ElementProperty::Null) => Ok(None),
            Operand::Value(other) => Err(input_error(step, "argument", expected, describe(other))),
            Operand::Traversal(traversal) => {
                let Some(first) = traversal.first(context, source)? else {
                    return Err(input_error(
                        step,
                        "traversal argument",
                        expected,
                        "no result (the traversal yielded nothing)".to_string(),
                    ));
                };
                match held_value(context, &first.value)? {
                    Ok(held) => match held.as_ref() {
                        ElementProperty::Temporal(value) => Ok(Some(*value)),
                        ElementProperty::Null => Ok(None),
                        other => Err(input_error(
                            step,
                            "traversal argument",
                            expected,
                            describe(other),
                        )),
                    },
                    Err(kind) => Err(input_error(
                        step,
                        "traversal argument",
                        expected,
                        format!("a {kind}"),
                    )),
                }
            }
        }
    }

    /// The plan-time check of a constant: a temporal value or `null`.
    pub(crate) fn check(&self, step: &'static str) -> TraverserResult<()> {
        match self {
            Operand::Value(ElementProperty::Temporal(_) | ElementProperty::Null)
            | Operand::Traversal(_) => Ok(()),
            Operand::Value(other) => Err(input_error(
                step,
                "argument",
                "a date or time",
                describe(other),
            )),
        }
    }

    /// What the child traversal reads from the incoming path.
    pub(crate) fn analyze_paths(&mut self, analysis: &PathAnalysis) -> PathRequirement {
        match self {
            Operand::Traversal(traversal) => {
                traversal.analyze_paths(analysis, PathRequirement::None)
            }
            Operand::Value(_) => PathRequirement::None,
        }
    }

    /// The child traversal, if any.
    pub(crate) fn children(&mut self) -> Vec<&mut AnonymousTraversal<'g, S>> {
        match self {
            Operand::Traversal(traversal) => vec![traversal],
            Operand::Value(_) => Vec::new(),
        }
    }
}

/// The value `null` stands for as the operand of `date_diff()`: the epoch of the other
/// operand's type (TinkerPop: a `null` date is the epoch), midnight for a `time`.
pub(crate) fn epoch_of(kind: crate::value::temporal::TemporalKind) -> Option<Temporal> {
    use crate::value::temporal::{
        Date, DateTime, LocalDateTime, OffsetTime, TemporalKind as K, Time,
    };
    match kind {
        K::DateTime => DateTime::from_micros(0, 0).ok().map(Temporal::DateTime),
        K::LocalDateTime => LocalDateTime::from_micros(0)
            .ok()
            .map(Temporal::LocalDateTime),
        K::Date => Date::from_days_since_epoch(0).ok().map(Temporal::Date),
        K::Time => Time::from_micros_of_day(0).ok().map(Temporal::Time),
        K::OffsetTime => Time::from_micros_of_day(0)
            .and_then(|midnight| OffsetTime::new(midnight, 0))
            .ok()
            .map(Temporal::OffsetTime),
        K::Duration => None,
    }
}
