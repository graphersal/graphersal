//! `as_date()` (TinkerPop `asDate()`) and `as_date(GType.X)`: converts a value to a date or time.

use crate::graph::GraphStorage;
use crate::traversal::step::map_steps::temporal::{describe, held_value, replaced};
use crate::value::temporal::{
    Date, DateTime, LocalDateTime, OffsetTime, Temporal, TemporalKind, TemporalResult, Time,
};
use crate::*;

/// What `as_date()` converts to.
#[derive(Clone, Debug)]
enum Target {
    /// `as_date()`: a `datetime`, TinkerPop's reading.
    Tinkerpop,
    /// `as_date(GType.X)` naming a date or time type.
    Kind(TemporalKind),
    /// A token that names no date or time type (`GType.STRING`, `GType.DURATION`, ...):
    /// rejected before execution.
    Rejected(GType),
}

/// `as_date()` / `as_date(GType.X)`.
///
/// `as_date()` is TinkerPop's `asDate()`: a `datetime` passes unchanged; a string is parsed as
/// ISO 8601 / RFC 3339 leniently (no offset is UTC, a date alone is midnight UTC); an `int64` is
/// milliseconds since the epoch, in UTC; a `date` or `localdatetime` becomes the `datetime` at
/// UTC. Everything else (a float, `null`, a list, a map, a `time`, a `duration`, a graph element)
/// fails with "Can't parse" (TinkerPop raises the same for a float, `null` and a list).
///
/// `as_date(GType.DATETIME | DATE | LOCALDATETIME | TIME | OFFSETTIME)` converts to that type (a
/// Graphersal extension): a string must be a text of that type (only `DATETIME` reads it
/// leniently, like `as_date()`); a `datetime` gives its wall-clock date, date-time or time at its
/// own offset, or its time with its offset; a `localdatetime` its date or time (an `offsettime`
/// at UTC); a `date` the `localdatetime` at midnight; a `time` the `offsettime` at UTC; an
/// `offsettime` its local `time`; a number converts only to `DATETIME` (epoch milliseconds).
/// `GType.DURATION` is `as_duration()`'s.
#[derive(Clone)]
struct StepAsDate {
    target: Target,
}

impl fmt::Display for StepAsDate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.target {
            Target::Tinkerpop => f.write_str("as_date()"),
            Target::Kind(kind) => write!(f, "as_date({})", gtype_of(*kind).token()),
            Target::Rejected(token) => write!(f, "as_date({})", token.token()),
        }
    }
}

/// The `GType` token of a temporal kind.
fn gtype_of(kind: TemporalKind) -> GType {
    match kind {
        TemporalKind::Date => GType::DATE,
        TemporalKind::LocalDateTime => GType::LOCALDATETIME,
        TemporalKind::DateTime => GType::DATETIME,
        TemporalKind::Time => GType::TIME,
        TemporalKind::OffsetTime => GType::OFFSETTIME,
        TemporalKind::Duration => GType::DURATION,
    }
}

fn target_of(target: GType) -> Target {
    let kind = target
        .as_type()
        .and_then(crate::graph::schema::ValueType::temporal_kind);
    match kind {
        Some(TemporalKind::Duration) | None => Target::Rejected(target),
        Some(kind) => Target::Kind(kind),
    }
}

fn step<'g, S: GraphStorage + 'g>(target: Target) -> GraphStep<'g, S> {
    GraphStep::Map("as_date", Box::new(StepAsDate { target }))
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// `as_date()` (TinkerPop `asDate()`): converts a string (ISO 8601; no offset is UTC, a date
    /// alone midnight UTC) or an `int64` of epoch milliseconds to a `datetime`; a `datetime`
    /// passes. A float, `null` or a list fails ("Can't parse"). Book part "Dates and Times".
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let graph = GraphSource::empty();
    /// let lock = graph.read();
    /// let g = lock.traversal();
    /// let value = g.inject(vec![1694017707000i64]).as_date().next().unwrap().unwrap();
    /// assert_eq!(value.as_temporal().unwrap().to_string(), "2023-09-06T16:28:27Z");
    /// ```
    pub fn as_date(self) -> Self {
        self.push_step(step(Target::Tinkerpop))
    }

    /// `as_date(GType.X)`: converts to the date or time type `target` (`GType::DATE`,
    /// `GType::LOCALDATETIME`, `GType::TIME`, `GType::OFFSETTIME`, `GType::DATETIME`); see the
    /// step's documentation in
    /// the book part "Dates and Times". Another token fails before execution.
    pub fn as_date_to(self, target: GType) -> Self {
        self.push_step(step(target_of(target)))
    }
}

impl __ {
    /// Anonymous `as_date()`.
    pub fn as_date<'g, S: GraphStorage + 'g>() -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().as_date()
    }

    /// Anonymous `as_date(GType.X)`; see [`GraphTraversalSource::as_date_to`].
    pub fn as_date_to<'g, S: GraphStorage + 'g>(target: GType) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().as_date_to(target)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// `as_date()`; see [`GraphTraversalSource::as_date`].
    pub fn as_date(self) -> Self {
        self.push_step(step(Target::Tinkerpop))
    }

    /// `as_date(GType.X)`; see [`GraphTraversalSource::as_date_to`].
    pub fn as_date_to(self, target: GType) -> Self {
        self.push_step(step(target_of(target)))
    }
}

/// Why a value does not convert, for [`ValueError::TemporalParse`].
type Refusal = String;

/// Converts a temporal value to `kind`.
fn from_temporal(value: Temporal, kind: TemporalKind) -> Result<Temporal, Refusal> {
    let midnight = Time::from_micros_of_day(0).map_err(|err| err.to_string())?;
    let converted: TemporalResult<Temporal> = match (kind, value) {
        (TemporalKind::DateTime, Temporal::DateTime(_))
        | (TemporalKind::Date, Temporal::Date(_))
        | (TemporalKind::LocalDateTime, Temporal::LocalDateTime(_))
        | (TemporalKind::Time, Temporal::Time(_))
        | (TemporalKind::OffsetTime, Temporal::OffsetTime(_)) => Ok(value),
        (TemporalKind::OffsetTime, Temporal::DateTime(v)) => {
            OffsetTime::new(v.local().time(), v.offset_seconds()).map(Temporal::OffsetTime)
        }
        (TemporalKind::OffsetTime, Temporal::LocalDateTime(v)) => {
            OffsetTime::new(v.time(), 0).map(Temporal::OffsetTime)
        }
        (TemporalKind::OffsetTime, Temporal::Time(v)) => {
            OffsetTime::new(v, 0).map(Temporal::OffsetTime)
        }
        (TemporalKind::Time, Temporal::OffsetTime(v)) => Ok(Temporal::Time(v.time())),
        (TemporalKind::DateTime, Temporal::Date(v)) => {
            DateTime::from_local(LocalDateTime::new(v, midnight), 0).map(Temporal::DateTime)
        }
        (TemporalKind::DateTime, Temporal::LocalDateTime(v)) => {
            DateTime::from_local(v, 0).map(Temporal::DateTime)
        }
        (TemporalKind::Date, Temporal::DateTime(v)) => Ok(Temporal::Date(v.local().date())),
        (TemporalKind::Date, Temporal::LocalDateTime(v)) => Ok(Temporal::Date(v.date())),
        (TemporalKind::LocalDateTime, Temporal::DateTime(v)) => {
            Ok(Temporal::LocalDateTime(v.local()))
        }
        (TemporalKind::LocalDateTime, Temporal::Date(v)) => {
            Ok(Temporal::LocalDateTime(LocalDateTime::new(v, midnight)))
        }
        (TemporalKind::Time, Temporal::DateTime(v)) => Ok(Temporal::Time(v.local().time())),
        (TemporalKind::Time, Temporal::LocalDateTime(v)) => Ok(Temporal::Time(v.time())),
        _ => {
            return Err(format!(
                "a {} does not convert to a {}",
                value.kind(),
                kind.name()
            ));
        }
    };
    converted.map_err(|err| err.to_string())
}

/// Parses text as `kind`: `DATETIME` leniently (TinkerPop), the others strictly as their type.
fn from_text(text: &str, kind: TemporalKind) -> Result<Temporal, Refusal> {
    let parsed = match kind {
        TemporalKind::DateTime => text.parse::<DateTime>().map(Temporal::DateTime),
        TemporalKind::Date => text.parse::<Date>().map(Temporal::Date),
        TemporalKind::LocalDateTime => text.parse::<LocalDateTime>().map(Temporal::LocalDateTime),
        TemporalKind::Time => text.parse::<Time>().map(Temporal::Time),
        TemporalKind::OffsetTime => text.parse::<OffsetTime>().map(Temporal::OffsetTime),
        TemporalKind::Duration => Temporal::parse_as(kind, text),
    };
    parsed.map_err(|err| match err {
        crate::value::temporal::TemporalError::Parse { reason, .. } => reason.to_string(),
        other => other.to_string(),
    })
}

/// Converts one value to `kind`.
pub(super) fn convert(value: &ElementProperty, kind: TemporalKind) -> Result<Temporal, Refusal> {
    match value {
        ElementProperty::Temporal(temporal) => from_temporal(*temporal, kind),
        ElementProperty::String(text) => from_text(text, kind),
        ElementProperty::Compressed(text) => text.with_text(|text| from_text(text, kind)),
        ElementProperty::Deduped(text) => from_text(text.as_str(), kind),
        ElementProperty::Number(number) if kind == TemporalKind::DateTime => {
            match number.as_i64() {
                Some(millis) => DateTime::from_epoch_millis(millis)
                    .map(Temporal::DateTime)
                    .map_err(|err| err.to_string()),
                None => Err(
                    "a float is not a number of epoch milliseconds (only an integer is; \
                             as_number(GType.LONG) truncates it)"
                        .to_string(),
                ),
            }
        }
        ElementProperty::Number(_) => Err(format!(
            "a number converts to a datetime only, not to a {}",
            kind.name()
        )),
        ElementProperty::Null => Err("null is not a date".to_string()),
        ElementProperty::Boolean(_)
        | ElementProperty::Uuid(_)
        | ElementProperty::Vector(_)
        | ElementProperty::Array(_)
        | ElementProperty::Object(_)
        | ElementProperty::Map(_)
        | ElementProperty::Vertex(_)
        | ElementProperty::Edge(_) => Err(format!(
            "a {} is not a date or time",
            crate::graph::schema::value_type_name(value)
        )),
    }
}

/// The [`ValueError::TemporalParse`] error.
pub(super) fn parse_error(
    step: &'static str,
    target: &'static str,
    value: String,
    reason: Refusal,
) -> Box<TraverserError> {
    Box::new(TraverserError::Value(ValueError::TemporalParse {
        step,
        target,
        value,
        reason,
    }))
}

impl<'g, S: GraphStorage + 'g> GraphStepMap<'g, S> for StepAsDate {
    /// Never called by the executor: `may_filter()` is true, and it calls `process_filter`.
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<Traverser<S>> {
        let kind = match &self.target {
            Target::Tinkerpop => TemporalKind::DateTime,
            Target::Kind(kind) => *kind,
            Target::Rejected(_) => {
                <Self as GraphStepMap<'g, S>>::plan_check(self)?;
                TemporalKind::DateTime
            }
        };
        let converted = match held_value(context, &source.value)? {
            Ok(held) => convert(held.as_ref(), kind)
                .map_err(|reason| parse_error("as_date", kind.name(), describe(&held), reason))?,
            Err(stream_kind) => {
                return Err(parse_error(
                    "as_date",
                    kind.name(),
                    format!("a {stream_kind}"),
                    format!("a {stream_kind} is not a date or time"),
                ));
            }
        };
        Ok(replaced(
            source,
            crate::traversal::step::map_steps::temporal::computed(converted),
        ))
    }

    fn process_filter(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<Option<Traverser<S>>> {
        self.process(context, source).map(Some)
    }

    fn may_filter(&self) -> bool {
        true
    }

    fn plan_check(&self) -> TraverserResult<()> {
        match &self.target {
            Target::Rejected(token) => Err(Box::new(TraverserError::Value(
                ValueError::UnsupportedCastTarget {
                    target: token.token(),
                    reason: "as_date() converts to GType.DATETIME, GType.DATE, \
                             GType.LOCALDATETIME, GType.TIME or GType.OFFSETTIME; as_duration() \
                             reads durations",
                },
            ))),
            Target::Tinkerpop | Target::Kind(_) => Ok(()),
        }
    }

    fn clone_box(&self) -> Box<dyn GraphStepMap<'g, S> + 'g> {
        Box::new(self.clone())
    }
}
