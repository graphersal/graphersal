//! Constructors and clocks (3g.T4): `date(..)`, `local_time(..)` / `localtime(..)`,
//! `local_datetime(..)` / `localdatetime(..)`, `zoned_datetime(..)` / `datetime(..)` from a map,
//! a string or another temporal value (no argument: the time the execution started, the pinned
//! `now()`, U9), `duration(map | string)`, `X.statement([zone])` / `X.transaction([zone])` (the
//! pinned `now()` too) and `datetime.fromepoch(seconds, nanoseconds)` /
//! `datetime.fromepochmillis(milliseconds)`, with the openCypher (Neo4j) meanings:
//!
//! - **From a map** (keys ASCII case-insensitive): a date from `year` with one of the groups
//!   `month` / `day`, `week` / `dayOfWeek` (ISO 8601 weeks: `year` is then the week-numbering
//!   year), `ordinalDay`, `quarter` / `dayOfQuarter`; a time from `hour`, `minute`, `second`
//!   and `millisecond` + `microsecond` + `nanosecond` (summed: 123 ms + 456 µs); a datetime's
//!   `timezone` (`'+01:00'`, `'Europe/Stockholm'`). A base value projects: `date: d` gives the
//!   date (of a date, a local datetime or a datetime at its own offset), `time: t` the time of
//!   day (and, for a datetime, the offset of a datetime `t`), `datetime: x` both; the other
//!   keys then override single fields as `java.time`'s `with` does (`{date: d, day: 28}`,
//!   `{date: d, week: 1}` keeps the day of the week, `{date: d, quarter: 3}` the month within
//!   the quarter). A `timezone` with a base that has an offset converts the instant
//!   (`12:00+01:00` to `16:00+05:00`); without one it is attached. No zone: UTC.
//! - **From a string**: ISO 8601 in full as Neo4j reads it (basic and extended forms, week and
//!   ordinal dates, `2015`, `2140`, `+0100`, fractions on any duration unit;
//!   `value::temporal::parse::parse_extended`).
//! - **From another temporal value**: its projection onto the type (`date(datetime)` the date
//!   at its offset, `datetime(localdatetime)` at UTC).
//! - **From the epoch** (`datetime` only): `{epochSeconds: s}` with an optional `millisecond`
//!   / `microsecond` / `nanosecond` fraction, or `{epochMillis: ms}`: that instant as a base
//!   `datetime` at UTC, which the other fields override (`java.time`'s `with`, as for
//!   `datetime: x`) and `timezone` converts; both together, or next to `date` / `time` /
//!   `datetime`, is 22007.
//! - A zone name resolves to its offset at that time; the value keeps the offset only (U3).
//! - A non-zero part below the microsecond is 0A000 "not yet" (U1): our values hold
//!   microseconds, and a silently cut value would be a wrong answer.
//! - Errors: a `NULL` argument (or a `NULL` field in the map) gives `NULL` (F2); text that is
//!   no value of the type, an unknown or conflicting map field and an unknown zone are 22007; a
//!   field or result out of range (month 13, the year 10000) 22008; a value of the wrong kind
//!   22G03.

use super::super::super::eval::{Env, EvalError, Value, function_name, status};
use super::super::super::lexer::Span;
use super::super::super::parser::{Clock, Function, TemporalFunction, TemporalType};
use super::super::{Arg, any_null, integer_arg, wrong_kind};
use super::{kind_name, sub_micro};
use crate::ElementProperty;
use crate::value::number::N;
use crate::value::temporal::arith::{Amount, DurationParts, cascade};
use crate::value::temporal::civil;
use crate::value::temporal::parse::parse_extended;
use crate::value::temporal::zone::Zone;
use crate::value::temporal::{
    Date, DateTime, LocalDateTime, Temporal, TemporalError, TemporalKind, Time,
};

/// A temporal value as a GQL value.
fn temporal<E>(value: Temporal) -> Value<E> {
    Value::Scalar(ElementProperty::Temporal(value))
}

/// An engine temporal error as the GQL error at `span`: unparsable text and an unknown zone
/// 22007, a value out of range 22008, an operation the type lacks 22G03.
pub(crate) fn temporal_error(error: TemporalError, span: Span) -> EvalError {
    let gqlstatus = match &error {
        TemporalError::Parse { .. } | TemporalError::UnknownZone { .. } => {
            status::INVALID_DATETIME_FORMAT
        }
        TemporalError::OutOfRange { .. } => status::DATETIME_OVERFLOW,
        TemporalError::Unsupported { .. } => status::INVALID_VALUE_TYPE,
    };
    EvalError::new(gqlstatus, error.to_string(), span)
}

/// The 22007 error of a constructor map that is not a value of the type.
fn bad_map(function: Function, what: impl std::fmt::Display, span: Span) -> EvalError {
    EvalError::new(
        status::INVALID_DATETIME_FORMAT,
        format!("{}(): {what}", function_name(function)),
        span,
    )
}

/// The 22008 error of a map field outside its range.
fn field_range(function: Function, field: &str, value: i64, span: Span) -> EvalError {
    EvalError::new(
        status::DATETIME_OVERFLOW,
        format!(
            "{}(): the field {field} cannot be {value}",
            function_name(function)
        ),
        span,
    )
}

/// The engine kind a constructor builds.
fn engine_kind(of: TemporalType) -> TemporalKind {
    match of {
        TemporalType::Date => TemporalKind::Date,
        TemporalType::LocalTime => TemporalKind::Time,
        TemporalType::LocalDateTime => TemporalKind::LocalDateTime,
        TemporalType::DateTime => TemporalKind::DateTime,
    }
}

/// `date(..)`, `localtime(..)`, `localdatetime(..)`, `datetime(..)` (zero or one argument).
pub(crate) fn construct<En: Env>(
    of: TemporalType,
    args: Vec<Arg<En::Element>>,
    env: &En,
    span: Span,
) -> Result<Value<En::Element>, EvalError> {
    let function = Function::Temporal(TemporalFunction::Construct(of));
    let Some(arg) = args.into_iter().next() else {
        return now_as(of, env.now(), None, span).map(temporal);
    };
    match &arg.value {
        Value::Null => Ok(Value::Null),
        Value::Map(entries) => {
            from_map(function, of, entries, env, arg.span).map(|v| v.map_or(Value::Null, temporal))
        }
        Value::Scalar(ElementProperty::Temporal(value)) => {
            project(function, of, *value, &arg).map(temporal)
        }
        value => match value.as_str() {
            Some(text) => from_text(engine_kind(of), text, arg.span).map(temporal),
            None => Err(wrong_kind(
                function,
                "a map, a string or a temporal value",
                &arg,
            )),
        },
    }
}

/// `text` read as a value of `kind` (the ISO 8601 forms of `parse_extended`).
fn from_text(kind: TemporalKind, text: &str, span: Span) -> Result<Temporal, EvalError> {
    let read = parse_extended(kind, text).map_err(|e| temporal_error(e, span))?;
    if read.sub_micro {
        return Err(sub_micro(span));
    }
    Ok(read.value)
}

/// `now` (an instant) in `zone` (UTC when `None`), as a value of type `of`.
fn now_as(
    of: TemporalType,
    now: DateTime,
    zone: Option<&Zone>,
    span: Span,
) -> Result<Temporal, EvalError> {
    let at = match zone {
        Some(zone) => zone.at(now),
        None => now.with_offset(0),
    }
    .map_err(|e| temporal_error(e, span))?;
    Ok(match of {
        TemporalType::Date => Temporal::Date(at.local().date()),
        TemporalType::LocalTime => Temporal::Time(at.local().time()),
        TemporalType::LocalDateTime => Temporal::LocalDateTime(at.local()),
        TemporalType::DateTime => Temporal::DateTime(at),
    })
}

/// Another temporal value projected onto type `of` (`date(datetime)`, `localtime(datetime)`,
/// `datetime(localdatetime)` at UTC, ...); a value without the parts of `of` is 22G03.
fn project<E>(
    function: Function,
    of: TemporalType,
    value: Temporal,
    arg: &Arg<E>,
) -> Result<Temporal, EvalError> {
    let projected = match (of, value) {
        (TemporalType::Date, Temporal::Date(_))
        | (TemporalType::LocalTime, Temporal::Time(_))
        | (TemporalType::LocalDateTime, Temporal::LocalDateTime(_))
        | (TemporalType::DateTime, Temporal::DateTime(_)) => Some(value),
        (TemporalType::Date, Temporal::LocalDateTime(v)) => Some(Temporal::Date(v.date())),
        (TemporalType::Date, Temporal::DateTime(v)) => Some(Temporal::Date(v.local().date())),
        (TemporalType::LocalTime, Temporal::LocalDateTime(v)) => Some(Temporal::Time(v.time())),
        (TemporalType::LocalTime, Temporal::DateTime(v)) => Some(Temporal::Time(v.local().time())),
        (TemporalType::LocalDateTime, Temporal::DateTime(v)) => {
            Some(Temporal::LocalDateTime(v.local()))
        }
        (TemporalType::DateTime, Temporal::LocalDateTime(v)) => Some(Temporal::DateTime(
            DateTime::from_local(v, 0).map_err(|e| temporal_error(e, arg.span))?,
        )),
        (TemporalType::Date, Temporal::Time(_) | Temporal::Duration(_))
        | (TemporalType::LocalTime, Temporal::Date(_) | Temporal::Duration(_))
        | (
            TemporalType::LocalDateTime | TemporalType::DateTime,
            Temporal::Date(_) | Temporal::Time(_) | Temporal::Duration(_),
        ) => None,
    };
    projected.ok_or_else(|| {
        EvalError::invalid_type(
            format!(
                "{}() cannot be built from {}",
                function_name(function),
                kind_name(&value)
            ),
            arg.span,
        )
    })
}

/// The fields of a constructor map.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Key {
    Date,
    Time,
    DateTime,
    Year,
    Quarter,
    Month,
    Week,
    OrdinalDay,
    DayOfQuarter,
    DayOfWeek,
    Day,
    Hour,
    Minute,
    Second,
    Millisecond,
    Microsecond,
    Nanosecond,
    Timezone,
    EpochSeconds,
    EpochMillis,
}

/// The map keys by name (matched ASCII case-insensitively), in the order messages list them.
const KEYS: &[(&str, Key)] = &[
    ("date", Key::Date),
    ("time", Key::Time),
    ("datetime", Key::DateTime),
    ("year", Key::Year),
    ("quarter", Key::Quarter),
    ("month", Key::Month),
    ("week", Key::Week),
    ("ordinalDay", Key::OrdinalDay),
    ("dayOfQuarter", Key::DayOfQuarter),
    ("dayOfWeek", Key::DayOfWeek),
    ("day", Key::Day),
    ("hour", Key::Hour),
    ("minute", Key::Minute),
    ("second", Key::Second),
    ("millisecond", Key::Millisecond),
    ("microsecond", Key::Microsecond),
    ("nanosecond", Key::Nanosecond),
    ("timezone", Key::Timezone),
    ("epochSeconds", Key::EpochSeconds),
    ("epochMillis", Key::EpochMillis),
];

impl Key {
    fn name(self) -> &'static str {
        KEYS.iter()
            .find(|(_, key)| *key == self)
            .map_or("field", |(name, _)| name)
    }

    /// Whether a constructor of `of` takes the key.
    fn allowed(self, of: TemporalType) -> bool {
        let date = matches!(
            self,
            Key::Date
                | Key::Year
                | Key::Quarter
                | Key::Month
                | Key::Week
                | Key::OrdinalDay
                | Key::DayOfQuarter
                | Key::DayOfWeek
                | Key::Day
        );
        let time = matches!(
            self,
            Key::Time
                | Key::Hour
                | Key::Minute
                | Key::Second
                | Key::Millisecond
                | Key::Microsecond
                | Key::Nanosecond
        );
        match of {
            TemporalType::Date => date,
            TemporalType::LocalTime => time,
            TemporalType::LocalDateTime => date || time || self == Key::DateTime,
            TemporalType::DateTime => true,
        }
    }
}

/// The fields a constructor map gave, read and kind-checked.
#[derive(Debug, Default)]
struct Fields {
    date: Option<Temporal>,
    time: Option<Temporal>,
    datetime: Option<Temporal>,
    year: Option<i64>,
    quarter: Option<i64>,
    month: Option<i64>,
    week: Option<i64>,
    ordinal_day: Option<i64>,
    day_of_quarter: Option<i64>,
    day_of_week: Option<i64>,
    day: Option<i64>,
    hour: Option<i64>,
    minute: Option<i64>,
    second: Option<i64>,
    millisecond: Option<i64>,
    microsecond: Option<i64>,
    nanosecond: Option<i64>,
    timezone: Option<Zone>,
    epoch_seconds: Option<i64>,
    epoch_millis: Option<i64>,
}

impl Fields {
    fn has_date_field(&self) -> bool {
        self.year.is_some()
            || self.quarter.is_some()
            || self.month.is_some()
            || self.week.is_some()
            || self.ordinal_day.is_some()
            || self.day_of_quarter.is_some()
            || self.day_of_week.is_some()
            || self.day.is_some()
    }

    fn has_time_field(&self) -> bool {
        self.hour.is_some() || self.minute.is_some() || self.second.is_some() || self.has_fraction()
    }

    fn has_fraction(&self) -> bool {
        self.millisecond.is_some() || self.microsecond.is_some() || self.nanosecond.is_some()
    }

    fn is_empty(&self) -> bool {
        self.date.is_none()
            && self.epoch_seconds.is_none()
            && self.epoch_millis.is_none()
            && self.time.is_none()
            && self.datetime.is_none()
            && !self.has_date_field()
            && !self.has_time_field()
    }
}

/// Reads the map's entries into [`Fields`]; `None` when a field is `NULL` (the result is
/// `NULL`, F2).
fn read_fields<E>(
    function: Function,
    of: TemporalType,
    entries: &[(crate::CompactString, Value<E>)],
    span: Span,
) -> Result<Option<Fields>, EvalError> {
    let mut fields = Fields::default();
    for (name, value) in entries {
        let Some(key) = KEYS
            .iter()
            .find(|(known, _)| known.eq_ignore_ascii_case(name))
            .map(|(_, key)| *key)
        else {
            let known: Vec<&str> = KEYS
                .iter()
                .filter(|(_, key)| key.allowed(of))
                .map(|(known, _)| *known)
                .collect();
            return Err(bad_map(
                function,
                format_args!(
                    "the map has an unknown field {name}; the fields are {}",
                    known.join(", ")
                ),
                span,
            ));
        };
        if !key.allowed(of) {
            return Err(bad_map(
                function,
                format_args!("the map cannot have the field {}", key.name()),
                span,
            ));
        }
        if value.is_null() {
            return Ok(None);
        }
        let integer = || -> Result<i64, EvalError> {
            match value {
                Value::Scalar(ElementProperty::Number(number)) => match number.n {
                    N::NegInt(n) => Ok(n),
                    N::Float(_) => Err(field_kind(function, key, value, span)),
                },
                _ => Err(field_kind(function, key, value, span)),
            }
        };
        let base = || -> Result<Temporal, EvalError> {
            match value {
                Value::Scalar(ElementProperty::Temporal(t)) => Ok(*t),
                _ => Err(field_kind(function, key, value, span)),
            }
        };
        match key {
            Key::Date => fields.date = Some(base()?),
            Key::Time => fields.time = Some(base()?),
            Key::DateTime => fields.datetime = Some(base()?),
            Key::Year => fields.year = Some(integer()?),
            Key::Quarter => fields.quarter = Some(integer()?),
            Key::Month => fields.month = Some(integer()?),
            Key::Week => fields.week = Some(integer()?),
            Key::OrdinalDay => fields.ordinal_day = Some(integer()?),
            Key::DayOfQuarter => fields.day_of_quarter = Some(integer()?),
            Key::DayOfWeek => fields.day_of_week = Some(integer()?),
            Key::Day => fields.day = Some(integer()?),
            Key::Hour => fields.hour = Some(integer()?),
            Key::Minute => fields.minute = Some(integer()?),
            Key::Second => fields.second = Some(integer()?),
            Key::Millisecond => fields.millisecond = Some(integer()?),
            Key::Microsecond => fields.microsecond = Some(integer()?),
            Key::Nanosecond => fields.nanosecond = Some(integer()?),
            Key::EpochSeconds => fields.epoch_seconds = Some(integer()?),
            Key::EpochMillis => fields.epoch_millis = Some(integer()?),
            Key::Timezone => {
                let Some(name) = value.as_str() else {
                    return Err(field_kind(function, key, value, span));
                };
                fields.timezone = Some(Zone::parse(name).map_err(|e| temporal_error(e, span))?);
            }
        }
    }
    if fields.epoch_seconds.is_some() || fields.epoch_millis.is_some() {
        if fields.epoch_seconds.is_some() && fields.epoch_millis.is_some() {
            return Err(bad_map(
                function,
                "the map has either epochSeconds or epochMillis, not both",
                span,
            ));
        }
        if fields.date.is_some() || fields.time.is_some() || fields.datetime.is_some() {
            return Err(bad_map(
                function,
                "the fields date, time and datetime cannot stand next to epochSeconds or \
                 epochMillis",
                span,
            ));
        }
    }
    Ok(Some(fields))
}

/// The 22G03 error of a map field of the wrong kind.
fn field_kind<E>(function: Function, key: Key, value: &Value<E>, span: Span) -> EvalError {
    let what = match key {
        Key::Date => "a date, a local datetime or a datetime",
        Key::Time => "a local time, a local datetime or a datetime",
        Key::DateTime => "a local datetime or a datetime",
        Key::Timezone => "a string (an offset like '+01:00' or a zone like 'Europe/Berlin')",
        _ => "an integer",
    };
    EvalError::invalid_type(
        format!(
            "{}(): the field {} takes {what}, not {}",
            function_name(function),
            key.name(),
            value.kind_name()
        ),
        span,
    )
}

/// A constructor map: the value, or `None` when a field is `NULL`.
fn from_map<En: Env>(
    function: Function,
    of: TemporalType,
    entries: &[(crate::CompactString, Value<En::Element>)],
    env: &En,
    span: Span,
) -> Result<Option<Temporal>, EvalError> {
    // Only `timezone`: the current time in that zone (`date({timezone: 'Europe/Berlin'})`).
    let only_zone = !entries.is_empty()
        && entries
            .iter()
            .all(|(name, _)| name.eq_ignore_ascii_case("timezone"));
    if only_zone {
        let zone = match entries.last() {
            Some((_, value)) => clock_zone(function, value, span)?,
            None => None,
        };
        return match zone {
            Some(zone) => now_as(of, env.now(), Some(&zone), span).map(Some),
            None => Ok(None),
        };
    }
    let Some(fields) = read_fields(function, of, entries, span)? else {
        return Ok(None);
    };
    if fields.is_empty() {
        return Err(bad_map(
            function,
            match of {
                TemporalType::Date => "the map needs the field year or date",
                TemporalType::LocalTime => "the map needs the field hour or time",
                TemporalType::LocalDateTime | TemporalType::DateTime => {
                    "the map needs the field year, date or datetime"
                }
            },
            span,
        ));
    }
    let build = Build {
        function,
        fields: &fields,
        span,
    };
    build.value(of).map(Some)
}

/// The zone argument of a clock: a zone name or offset, or a map `{timezone: ..}`; `None` for
/// `NULL`.
fn clock_zone<E>(
    function: Function,
    value: &Value<E>,
    span: Span,
) -> Result<Option<Zone>, EvalError> {
    let name = match value {
        Value::Null => return Ok(None),
        Value::Map(entries) => match entries.as_slice() {
            [(key, value)] if key.eq_ignore_ascii_case("timezone") => match value {
                Value::Null => return Ok(None),
                value => value.as_str(),
            },
            _ => {
                return Err(bad_map(
                    function,
                    "the map of a clock has only the field timezone",
                    span,
                ));
            }
        },
        value => value.as_str(),
    };
    let Some(name) = name else {
        return Err(EvalError::invalid_type(
            format!(
                "{}(): the time zone is a string (an offset like '+01:00' or a zone like \
                 'Europe/Berlin'), not {}",
                function_name(function),
                value.kind_name()
            ),
            span,
        ));
    };
    Zone::parse(name)
        .map(Some)
        .map_err(|e| temporal_error(e, span))
}

/// The building of one value from its [`Fields`].
struct Build<'a> {
    function: Function,
    fields: &'a Fields,
    span: Span,
}

impl Build<'_> {
    fn value(&self, of: TemporalType) -> Result<Temporal, EvalError> {
        let f = self.fields;
        match of {
            TemporalType::Date => {
                let base = f.date.map(|t| self.date_of(t, Key::Date)).transpose()?;
                let days = self
                    .date_part(base)?
                    .ok_or_else(|| self.needs("year or date"))?;
                Ok(Temporal::Date(self.date(days)?))
            }
            TemporalType::LocalTime => {
                let base = f.time.map(|t| self.time_of(t, Key::Time)).transpose()?;
                let micros = self
                    .time_part(base)?
                    .ok_or_else(|| self.needs("hour or time"))?;
                Ok(Temporal::Time(self.time(micros)?))
            }
            TemporalType::LocalDateTime => self.local().map(Temporal::LocalDateTime),
            TemporalType::DateTime => self.datetime().map(Temporal::DateTime),
        }
    }

    /// The local date and time of a `localdatetime` / `datetime` map.
    fn local(&self) -> Result<LocalDateTime, EvalError> {
        let f = self.fields;
        let base = match self.epoch()? {
            Some(utc) => Some(utc.local()),
            None => f.datetime.map(|t| self.local_of(t)).transpose()?,
        };
        if base.is_some() && (f.date.is_some() || f.time.is_some()) {
            return Err(bad_map(
                self.function,
                "the field datetime cannot be combined with date or time",
                self.span,
            ));
        }
        let date_base = match (base, f.date) {
            (Some(base), _) => Some(base.date().days_since_epoch()),
            (None, Some(t)) => Some(self.date_of(t, Key::Date)?),
            (None, None) => None,
        };
        let time_base = match (base, f.time) {
            (Some(base), _) => Some(base.time().micros_of_day()),
            (None, Some(t)) => Some(self.time_of(t, Key::Time)?),
            (None, None) => None,
        };
        let days = self
            .date_part(date_base)?
            .ok_or_else(|| self.needs("year, date or datetime"))?;
        let micros = self.time_part(time_base)?.unwrap_or(0);
        Ok(LocalDateTime::new(self.date(days)?, self.time(micros)?))
    }

    /// A `datetime` map: the local value, at the base's offset when a `datetime` or `time` base
    /// has one, then converted to (or, without such a base, placed in) `timezone`; UTC when
    /// neither gives a zone.
    fn datetime(&self) -> Result<DateTime, EvalError> {
        let f = self.fields;
        let base_offset = match (f.datetime, f.time) {
            (Some(Temporal::DateTime(v)), _) | (None, Some(Temporal::DateTime(v))) => {
                Some(v.offset_seconds())
            }
            _ if self.epoch()?.is_some() => Some(0),
            _ => None,
        };
        let local = self.local()?;
        let error = |e| temporal_error(e, self.span);
        match (base_offset, &f.timezone) {
            (Some(offset), Some(zone)) => {
                zone.at(DateTime::from_local(local, offset).map_err(error)?)
            }
            (Some(offset), None) => DateTime::from_local(local, offset),
            (None, Some(zone)) => zone.resolve(local),
            (None, None) => DateTime::from_local(local, 0),
        }
        .map_err(error)
    }

    /// The UTC datetime of `epochSeconds` / `epochMillis` (Neo4j: a base `datetime` at UTC that
    /// the other fields override and `timezone` converts); `None` without them.
    fn epoch(&self) -> Result<Option<DateTime>, EvalError> {
        let f = self.fields;
        let error = |e| temporal_error(e, self.span);
        match (f.epoch_seconds, f.epoch_millis) {
            (Some(seconds), _) => seconds
                .checked_mul(civil::MICROS_PER_SECOND)
                .ok_or_else(|| TemporalError::out_of_range("the date-time"))
                .and_then(|micros| DateTime::from_micros(micros, 0))
                .map(Some)
                .map_err(error),
            (None, Some(millis)) => DateTime::from_epoch_millis(millis).map(Some).map_err(error),
            (None, None) => Ok(None),
        }
    }

    fn needs(&self, what: &str) -> EvalError {
        bad_map(
            self.function,
            format_args!("the map needs the field {what}"),
            self.span,
        )
    }

    fn date(&self, days: i32) -> Result<Date, EvalError> {
        Date::from_days_since_epoch(days).map_err(|e| temporal_error(e, self.span))
    }

    fn time(&self, micros: i64) -> Result<Time, EvalError> {
        Time::from_micros_of_day(micros).map_err(|e| temporal_error(e, self.span))
    }

    /// The days since the epoch of a `date` base (a date, a local datetime, a datetime at its
    /// own offset).
    fn date_of(&self, value: Temporal, key: Key) -> Result<i32, EvalError> {
        match value {
            Temporal::Date(d) => Ok(d.days_since_epoch()),
            Temporal::LocalDateTime(v) => Ok(v.date().days_since_epoch()),
            Temporal::DateTime(v) => Ok(v.local().date().days_since_epoch()),
            Temporal::Time(_) | Temporal::Duration(_) => Err(self.base_kind(key, &value)),
        }
    }

    /// The microseconds of the day of a `time` base.
    fn time_of(&self, value: Temporal, key: Key) -> Result<i64, EvalError> {
        match value {
            Temporal::Time(t) => Ok(t.micros_of_day()),
            Temporal::LocalDateTime(v) => Ok(v.time().micros_of_day()),
            Temporal::DateTime(v) => Ok(v.local().time().micros_of_day()),
            Temporal::Date(_) | Temporal::Duration(_) => Err(self.base_kind(key, &value)),
        }
    }

    /// The local value of a `datetime` base.
    fn local_of(&self, value: Temporal) -> Result<LocalDateTime, EvalError> {
        match value {
            Temporal::LocalDateTime(v) => Ok(v),
            Temporal::DateTime(v) => Ok(v.local()),
            Temporal::Date(_) | Temporal::Time(_) | Temporal::Duration(_) => {
                Err(self.base_kind(Key::DateTime, &value))
            }
        }
    }

    fn base_kind(&self, key: Key, value: &Temporal) -> EvalError {
        field_kind::<()>(
            self.function,
            key,
            &Value::Scalar(ElementProperty::Temporal(*value)),
            self.span,
        )
    }

    fn range(&self, key: Key, value: i64) -> EvalError {
        field_range(self.function, key.name(), value, self.span)
    }

    /// `value` of `key` within `low..=high`, else 22008.
    fn within(&self, key: Key, value: i64, low: i64, high: i64) -> Result<i64, EvalError> {
        if (low..=high).contains(&value) {
            Ok(value)
        } else {
            Err(self.range(key, value))
        }
    }

    /// The year field as an `i32` within the supported years.
    fn year(&self, year: i64) -> Result<i32, EvalError> {
        self.within(
            Key::Year,
            year,
            civil::MIN_YEAR as i64,
            civil::MAX_YEAR as i64,
        )
        .map(|y| y as i32)
    }

    /// The date part: the days since the epoch, from `base` with the date fields applied, or
    /// from the fields alone; `None` when neither gives a date.
    fn date_part(&self, base: Option<i32>) -> Result<Option<i32>, EvalError> {
        let f = self.fields;
        let calendar = f.month.is_some() || f.day.is_some();
        let week = f.week.is_some() || f.day_of_week.is_some();
        let ordinal = f.ordinal_day.is_some();
        let quarter = f.quarter.is_some() || f.day_of_quarter.is_some();
        let groups = [calendar, week, ordinal, quarter]
            .iter()
            .filter(|g| **g)
            .count();
        if groups > 1 {
            return Err(bad_map(
                self.function,
                "a date takes one of month / day, week / dayOfWeek, ordinalDay or quarter / \
                 dayOfQuarter",
                self.span,
            ));
        }
        let Some(days) = base else {
            return self.date_from_fields(calendar, week, ordinal, quarter);
        };
        let mut days = days;
        if let Some(year) = f.year {
            let year = self.year(year)?;
            if week {
                // The week-numbering year, keeping the week (at most the year's last) and the
                // day of the week.
                let (_, w) = civil::iso_week(days);
                let w = w.min(civil::weeks_in_week_year(year));
                days = civil::from_iso_week(year, w, civil::iso_day_of_week(days))
                    .ok_or_else(|| self.range(Key::Year, year as i64))?;
            } else {
                let (_, month, day) = civil::civil_from_days(days);
                let day = day.min(civil::days_in_month(year, month));
                days = civil::days_from_civil(year, month, day);
            }
        }
        if calendar {
            let (year, month, day) = civil::civil_from_days(days);
            let month = match f.month {
                Some(m) => self.within(Key::Month, m, 1, 12)? as u8,
                None => month,
            };
            let day = match f.day {
                Some(d) => {
                    self.within(Key::Day, d, 1, civil::days_in_month(year, month) as i64)? as u8
                }
                None => day.min(civil::days_in_month(year, month)),
            };
            days = civil::days_from_civil(year, month, day);
        }
        if week {
            let (week_year, w) = civil::iso_week(days);
            let w = match f.week {
                Some(w) => {
                    self.within(Key::Week, w, 1, civil::weeks_in_week_year(week_year) as i64)? as u8
                }
                None => w,
            };
            let dow = match f.day_of_week {
                Some(d) => self.within(Key::DayOfWeek, d, 1, 7)? as u8,
                None => civil::iso_day_of_week(days),
            };
            days = civil::from_iso_week(week_year, w, dow)
                .ok_or_else(|| self.range(Key::Week, w as i64))?;
        }
        if let Some(ordinal) = f.ordinal_day {
            let (year, _, _) = civil::civil_from_days(days);
            let length = if civil::is_leap(year) { 366 } else { 365 };
            let ordinal = self.within(Key::OrdinalDay, ordinal, 1, length)? as u16;
            days = civil::from_ordinal(year, ordinal)
                .ok_or_else(|| self.range(Key::OrdinalDay, ordinal as i64))?;
        }
        if quarter {
            let (year, month, day) = civil::civil_from_days(days);
            let current = (month - 1) / 3 + 1;
            if let Some(q) = f.quarter {
                // `java.time`: the month moves by whole quarters, the day of the month stays
                // (at most the month's last).
                let q = self.within(Key::Quarter, q, 1, 4)?;
                let month = (month as i64 + (q - current as i64) * 3) as u8;
                let day = day.min(civil::days_in_month(year, month));
                days = civil::days_from_civil(year, month, day);
            }
            if let Some(d) = f.day_of_quarter {
                let (year, month, _) = civil::civil_from_days(days);
                let q = (month - 1) / 3 + 1;
                let d = self.within(Key::DayOfQuarter, d, 1, 92)?;
                days = civil::from_quarter(year, q, d as u8)
                    .ok_or_else(|| self.range(Key::DayOfQuarter, d))?;
            }
        }
        if !civil::day_in_range(days as i64) {
            return Err(temporal_error(
                TemporalError::out_of_range("the date"),
                self.span,
            ));
        }
        Ok(Some(days))
    }

    /// The date of the fields without a base: `year` with at most one group.
    fn date_from_fields(
        &self,
        calendar: bool,
        week: bool,
        ordinal: bool,
        quarter: bool,
    ) -> Result<Option<i32>, EvalError> {
        let f = self.fields;
        let Some(year) = f.year else {
            if f.has_date_field() {
                return Err(self.needs("year (or date)"));
            }
            return Ok(None);
        };
        let year = self.year(year)?;
        let days = if calendar {
            if f.month.is_none() {
                return Err(self.needs("month (a day needs its month)"));
            }
            let month = self.within(Key::Month, f.month.unwrap_or(1), 1, 12)? as u8;
            let day = self.within(
                Key::Day,
                f.day.unwrap_or(1),
                1,
                civil::days_in_month(year, month) as i64,
            )? as u8;
            Some(civil::days_from_civil(year, month, day))
        } else if week {
            if f.week.is_none() {
                return Err(self.needs("week (a dayOfWeek needs its week)"));
            }
            let w = self.within(
                Key::Week,
                f.week.unwrap_or(1),
                1,
                civil::weeks_in_week_year(year) as i64,
            )?;
            let dow = self.within(Key::DayOfWeek, f.day_of_week.unwrap_or(1), 1, 7)?;
            civil::from_iso_week(year, w as u8, dow as u8)
        } else if ordinal {
            let length = if civil::is_leap(year) { 366 } else { 365 };
            let ordinal = self.within(Key::OrdinalDay, f.ordinal_day.unwrap_or(1), 1, length)?;
            civil::from_ordinal(year, ordinal as u16)
        } else if quarter {
            if f.quarter.is_none() {
                return Err(self.needs("quarter (a dayOfQuarter needs its quarter)"));
            }
            let q = self.within(Key::Quarter, f.quarter.unwrap_or(1), 1, 4)?;
            let d = self.within(Key::DayOfQuarter, f.day_of_quarter.unwrap_or(1), 1, 92)?;
            Some(
                civil::from_quarter(year, q as u8, d as u8)
                    .ok_or_else(|| self.range(Key::DayOfQuarter, d))?,
            )
        } else {
            Some(civil::days_from_civil(year, 1, 1))
        };
        match days {
            Some(days) if civil::day_in_range(days as i64) => Ok(Some(days)),
            _ => Err(temporal_error(
                TemporalError::out_of_range("the date"),
                self.span,
            )),
        }
    }

    /// The fraction of the second in nanoseconds from `millisecond` + `microsecond` +
    /// `nanosecond`, each within its range (`microsecond` below 1000 next to a
    /// `millisecond`, ...); `None` when none is given.
    fn fraction(&self) -> Result<Option<i64>, EvalError> {
        let f = self.fields;
        if !f.has_fraction() {
            return Ok(None);
        }
        let ms = match f.millisecond {
            Some(ms) => self.within(Key::Millisecond, ms, 0, 999)?,
            None => 0,
        };
        let us_max = if f.millisecond.is_some() {
            999
        } else {
            999_999
        };
        let us = match f.microsecond {
            Some(us) => self.within(Key::Microsecond, us, 0, us_max)?,
            None => 0,
        };
        let ns_max = if f.millisecond.is_some() || f.microsecond.is_some() {
            999
        } else {
            999_999_999
        };
        let ns = match f.nanosecond {
            Some(ns) => self.within(Key::Nanosecond, ns, 0, ns_max)?,
            None => 0,
        };
        Ok(Some(ms * 1_000_000 + us * 1000 + ns))
    }

    /// The time part: the microseconds of the day, from `base` with the time fields applied, or
    /// from the fields alone; `None` when neither gives a time.
    fn time_part(&self, base: Option<i64>) -> Result<Option<i64>, EvalError> {
        let f = self.fields;
        let fraction = self.fraction()?;
        if fraction.is_some_and(|nanos| nanos % 1000 != 0) {
            return Err(sub_micro(self.span));
        }
        let (hour, minute, second, micros) = match base {
            Some(base) => {
                let seconds = base / civil::MICROS_PER_SECOND;
                (
                    seconds / 3600,
                    seconds / 60 % 60,
                    seconds % 60,
                    base % civil::MICROS_PER_SECOND,
                )
            }
            None => {
                if f.hour.is_none() {
                    if f.has_time_field() {
                        return Err(self.needs("hour (or time)"));
                    }
                    return Ok(None);
                }
                if f.second.is_some() && f.minute.is_none() {
                    return Err(self.needs("minute (a second needs its minute)"));
                }
                if f.has_fraction() && f.second.is_none() {
                    return Err(self.needs("second (a fraction of a second needs its second)"));
                }
                (0, 0, 0, 0)
            }
        };
        let hour = match f.hour {
            Some(h) => self.within(Key::Hour, h, 0, 23)?,
            None => hour,
        };
        let minute = match f.minute {
            Some(m) => self.within(Key::Minute, m, 0, 59)?,
            None => minute,
        };
        let second = match f.second {
            Some(s) => self.within(Key::Second, s, 0, 59)?,
            None => second,
        };
        let micros = fraction.map_or(micros, |nanos| nanos / 1000);
        Ok(Some(
            ((hour * 60 + minute) * 60 + second) * civil::MICROS_PER_SECOND + micros,
        ))
    }
}

/// `duration(map | string)`.
pub(crate) fn duration<En: Env>(
    args: Vec<Arg<En::Element>>,
    _env: &En,
    _span: Span,
) -> Result<Value<En::Element>, EvalError> {
    let function = Function::Temporal(TemporalFunction::Duration);
    let Some(arg) = args.into_iter().next() else {
        return Ok(Value::Null);
    };
    let value = match &arg.value {
        Value::Null => return Ok(Value::Null),
        Value::Map(entries) => match duration_map(function, entries, arg.span)? {
            Some(value) => value,
            None => return Ok(Value::Null),
        },
        value => match value.as_str() {
            Some(text) => from_text(TemporalKind::Duration, text, arg.span)?,
            None => return Err(wrong_kind(function, "a map or a string", &arg)),
        },
    };
    Ok(temporal(value))
}

/// The units of a `duration` map, in the order messages list them.
const DURATION_KEYS: &[&str] = &[
    "years",
    "months",
    "weeks",
    "days",
    "hours",
    "minutes",
    "seconds",
    "milliseconds",
    "microseconds",
    "nanoseconds",
];

/// A `duration` map: each unit an integer or a float (fractions cascade, Neo4j's rule);
/// `None` when a unit is `NULL`.
fn duration_map<E>(
    function: Function,
    entries: &[(crate::CompactString, Value<E>)],
    span: Span,
) -> Result<Option<Temporal>, EvalError> {
    let mut parts = DurationParts::default();
    for (name, value) in entries {
        let Some(unit) = DURATION_KEYS
            .iter()
            .position(|known| known.eq_ignore_ascii_case(name))
        else {
            return Err(bad_map(
                function,
                format_args!(
                    "the map has an unknown field {name}; the fields are {}",
                    DURATION_KEYS.join(", ")
                ),
                span,
            ));
        };
        let amount = match value {
            Value::Null => return Ok(None),
            Value::Scalar(ElementProperty::Number(number)) => match number.n {
                N::NegInt(n) => Some(Amount::from_i64(n)),
                N::Float(x) => Amount::from_f64(x.0),
            },
            _ => {
                return Err(EvalError::invalid_type(
                    format!(
                        "duration(): the field {} takes a number, not {}",
                        DURATION_KEYS[unit],
                        value.kind_name()
                    ),
                    span,
                ));
            }
        };
        let Some(amount) = amount else {
            return Err(EvalError::new(
                status::DATETIME_OVERFLOW,
                format!(
                    "duration(): the field {} must be a finite number",
                    DURATION_KEYS[unit]
                ),
                span,
            ));
        };
        let slot = match unit {
            0 => &mut parts.years,
            1 => &mut parts.months,
            2 => &mut parts.weeks,
            3 => &mut parts.days,
            4 => &mut parts.hours,
            5 => &mut parts.minutes,
            6 => &mut parts.seconds,
            7 => &mut parts.milliseconds,
            8 => &mut parts.microseconds,
            _ => &mut parts.nanoseconds,
        };
        *slot = amount;
    }
    let cascaded = cascade(&parts).map_err(|e| temporal_error(e, span))?;
    if cascaded.sub_micro {
        return Err(sub_micro(span));
    }
    Ok(Some(Temporal::Duration(cascaded.duration)))
}

/// `X.statement([zone])` / `X.transaction([zone])`: the execution's pinned `now()` (both
/// clocks, U9) in `zone` (a name or offset, or `{timezone: ..}`; UTC without one).
pub(crate) fn clock<En: Env>(
    of: TemporalType,
    clock: Clock,
    args: Vec<Arg<En::Element>>,
    env: &En,
    span: Span,
) -> Result<Value<En::Element>, EvalError> {
    let function = Function::Temporal(TemporalFunction::Clock(of, clock));
    let zone = match args.first() {
        None => None,
        Some(arg) => match clock_zone(function, &arg.value, arg.span)? {
            Some(zone) => Some(zone),
            None => return Ok(Value::Null),
        },
    };
    now_as(of, env.now(), zone.as_ref(), span).map(temporal)
}

/// `datetime.fromepoch(seconds, nanoseconds)`: the UTC datetime that many seconds and
/// nanoseconds after 1970-01-01T00:00Z.
pub(crate) fn from_epoch<En: Env>(
    args: Vec<Arg<En::Element>>,
    _env: &En,
    span: Span,
) -> Result<Value<En::Element>, EvalError> {
    let function = Function::Temporal(TemporalFunction::FromEpoch);
    if any_null(&args) {
        return Ok(Value::Null);
    }
    let [seconds, nanos] = args.as_slice() else {
        return Ok(Value::Null);
    };
    let seconds_value = integer_arg(function, seconds, "integer seconds")?;
    let nanos_value = integer_arg(function, nanos, "integer nanoseconds")?;
    if nanos_value % 1000 != 0 {
        return Err(sub_micro(nanos.span));
    }
    let micros = seconds_value
        .checked_mul(civil::MICROS_PER_SECOND)
        .and_then(|micros| micros.checked_add(nanos_value / 1000));
    let value = micros
        .ok_or_else(|| TemporalError::out_of_range("the date-time"))
        .and_then(|micros| DateTime::from_micros(micros, 0))
        .map_err(|e| temporal_error(e, span))?;
    Ok(temporal(Temporal::DateTime(value)))
}

/// `datetime.fromepochmillis(milliseconds)`.
pub(crate) fn from_epoch_millis<En: Env>(
    args: Vec<Arg<En::Element>>,
    _env: &En,
    span: Span,
) -> Result<Value<En::Element>, EvalError> {
    let function = Function::Temporal(TemporalFunction::FromEpochMillis);
    if any_null(&args) {
        return Ok(Value::Null);
    }
    let Some(millis) = args.first() else {
        return Ok(Value::Null);
    };
    let millis = integer_arg(function, millis, "integer milliseconds")?;
    let value = DateTime::from_epoch_millis(millis).map_err(|e| temporal_error(e, span))?;
    Ok(temporal(Temporal::DateTime(value)))
}
