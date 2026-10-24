//! Truncation (3g.T7): `date.truncate`, `localtime.truncate`, `time.truncate` (temporal v2),
//! `localdatetime.truncate`, `datetime.truncate(unit, value [, map])` with the openCypher (Neo4j)
//! meanings:
//!
//! - **Units** (ASCII case-insensitive): `millennium`, `century`, `decade`, `year`, `weekYear`,
//!   `quarter`, `month`, `week`, `day` for the types with a date; `day`, `hour`, `minute`,
//!   `second`, `millisecond`, `microsecond`, `nanosecond` for the types with a time of day (on a
//!   `localtime`, `day` gives midnight). A unit the type lacks (`date.truncate('hour', ..)`,
//!   `localtime.truncate('month', ..)`) and an unknown unit are 22023. `nanosecond` keeps the
//!   value as it is (values hold nanoseconds).
//! - **Millennium and century as Neo4j** (U4): `2017` truncates to `2000`, `1984` to `1900`
//!   (`year div 1000 x 1000`, `div 100 x 100`, floor division for negative years). Rhai
//!   `date_trunc` keeps PostgreSQL's rule (2001, 1901); `weekYear` is the Monday of ISO week 1
//!   of the value's week-numbering year (1984-01-01 gives 1983-01-03). The other units use the
//!   engine truncation (`arith::trunc`), whose meaning is the same.
//! - **The value** may be of another temporal type, projected first as Neo4j does: the date of a
//!   local datetime or of a datetime (its wall clock at its own offset), the time of day of
//!   either, midnight for a date given to a type with a time of day. `datetime.truncate` keeps
//!   the offset of a datetime (and its zone name, resolving the truncated wall clock in that
//!   zone again) and gives UTC to a value without one; `time.truncate` keeps the offset of a
//!   datetime or a time the same way. A value without the part
//!   the type needs (`date.truncate(.., localtime)`, `localtime.truncate(.., date)`) or a
//!   duration is 22G03.
//! - **The map** overrides fields of the truncated value as the constructors' base form does
//!   (`{day: 2}`, `{dayOfWeek: 2}`, `{hour: 1}`, `{millisecond: 5}`): date fields in one of
//!   the groups `month` / `day`, `week` / `dayOfWeek` / `weekYear` (`year` is then the
//!   week-numbering year), `ordinalDay`, `quarter` / `dayOfQuarter`; time fields `hour`,
//!   `minute`, `second` and the summed `millisecond` + `microsecond` + `nanosecond` (with the
//!   unit `millisecond` a `microsecond` / `nanosecond` adds to the kept milliseconds, with
//!   `microsecond` a `nanosecond` to the kept microseconds, as Neo4j); `timezone` for
//!   `datetime.truncate` keeps the wall clock and resolves it in that zone (a zone name is kept
//!   in the value), for `time.truncate` it replaces the offset (a zone name gives its offset at
//!   the time the query started). A key the type lacks or groups that conflict are 22007, a
//!   field out of range 22008, a `NULL` field `NULL`.
//! - `NULL` in any argument gives `NULL` (F2).

use super::super::super::eval::{Env, EvalError, Value, status};
use super::super::super::lexer::Span;
use super::super::super::parser::{TemporalFunction, TemporalType};
use super::super::{Arg, any_null};
use super::construct::temporal_error;
use super::{cypher_name, kind_name};
use crate::CompactString;
use crate::ElementProperty;
use crate::value::dt::DT;
use crate::value::number::N;
use crate::value::temporal::arith;
use crate::value::temporal::civil;
use crate::value::temporal::zone::{Zone, ZoneId};
use crate::value::temporal::{
    Date, DateTime, LocalDateTime, OffsetTime, Temporal, TemporalError, Time,
};

/// A truncation unit (Neo4j's names).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unit {
    Millennium,
    Century,
    Decade,
    Year,
    WeekYear,
    Quarter,
    Month,
    Week,
    Day,
    Hour,
    Minute,
    Second,
    Millisecond,
    Microsecond,
    Nanosecond,
}

/// Every unit with its name, largest first.
const UNITS: &[(&str, Unit)] = &[
    ("millennium", Unit::Millennium),
    ("century", Unit::Century),
    ("decade", Unit::Decade),
    ("year", Unit::Year),
    ("weekYear", Unit::WeekYear),
    ("quarter", Unit::Quarter),
    ("month", Unit::Month),
    ("week", Unit::Week),
    ("day", Unit::Day),
    ("hour", Unit::Hour),
    ("minute", Unit::Minute),
    ("second", Unit::Second),
    ("millisecond", Unit::Millisecond),
    ("microsecond", Unit::Microsecond),
    ("nanosecond", Unit::Nanosecond),
];

impl Unit {
    fn parse(name: &str) -> Option<Unit> {
        UNITS
            .iter()
            .find(|(known, _)| known.eq_ignore_ascii_case(name))
            .map(|(_, unit)| *unit)
    }

    fn name(self) -> &'static str {
        UNITS
            .iter()
            .find(|(_, unit)| *unit == self)
            .map_or("unit", |(name, _)| name)
    }

    /// Whether the unit truncates a date (`day` truncates both).
    fn on_date(self) -> bool {
        match self {
            Unit::Millennium
            | Unit::Century
            | Unit::Decade
            | Unit::Year
            | Unit::WeekYear
            | Unit::Quarter
            | Unit::Month
            | Unit::Week
            | Unit::Day => true,
            Unit::Hour
            | Unit::Minute
            | Unit::Second
            | Unit::Millisecond
            | Unit::Microsecond
            | Unit::Nanosecond => false,
        }
    }

    /// Whether the unit truncates a time of day.
    fn on_time(self) -> bool {
        self == Unit::Day || !self.on_date()
    }

    /// Whether `of` has the unit.
    fn valid_for(self, of: TemporalType) -> bool {
        match of {
            TemporalType::Date => self.on_date(),
            TemporalType::LocalTime | TemporalType::Time => self.on_time(),
            TemporalType::LocalDateTime | TemporalType::DateTime => true,
        }
    }

    /// The engine unit with the same truncation (`None`: Neo4j's own rule or no truncation).
    fn engine(self) -> Option<DT> {
        match self {
            Unit::Decade => Some(DT::Decade),
            Unit::Year => Some(DT::Year),
            Unit::Quarter => Some(DT::Quarter),
            Unit::Month => Some(DT::Month),
            Unit::Week => Some(DT::Week),
            Unit::Day => Some(DT::Day),
            Unit::Hour => Some(DT::Hour),
            Unit::Minute => Some(DT::Minute),
            Unit::Second => Some(DT::Second),
            Unit::Millisecond => Some(DT::Millisecond),
            Unit::Microsecond => Some(DT::Microsecond),
            Unit::Nanosecond => Some(DT::Nanosecond),
            Unit::Millennium | Unit::Century | Unit::WeekYear => None,
        }
    }
}

/// `X.truncate(unit, value [, map])` for `X` of type `of`.
pub(crate) fn truncate<En: Env>(
    of: TemporalType,
    args: Vec<Arg<En::Element>>,
    env: &En,
    span: Span,
) -> Result<Value<En::Element>, EvalError> {
    if any_null(&args) {
        return Ok(Value::Null);
    }
    let name = cypher_name(TemporalFunction::Truncate(of));
    let mut args = args.into_iter();
    let (Some(unit), Some(value)) = (args.next(), args.next()) else {
        return Err(EvalError::new(
            status::SEMANTIC,
            format!("{name}() takes a unit and a value"),
            span,
        ));
    };
    let map = args.next();
    let Some(text) = unit.value.as_str() else {
        return Err(wrong(name, "a unit name such as 'day'", &unit));
    };
    let Some(parsed) = Unit::parse(text) else {
        return Err(EvalError::new(
            status::INVALID_PARAMETER,
            format!(
                "{name}(): unknown unit '{text}' (the units are {})",
                UNITS
                    .iter()
                    .map(|(name, _)| *name)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            unit.span,
        ));
    };
    if !parsed.valid_for(of) {
        return Err(EvalError::new(
            status::INVALID_PARAMETER,
            format!(
                "{name}(): the unit {} is not defined for {}",
                parsed.name(),
                type_name(of)
            ),
            unit.span,
        ));
    }
    let source = match &value.value {
        Value::Scalar(ElementProperty::Temporal(temporal)) => *temporal,
        _ => return Err(wrong(name, "a temporal value", &value)),
    };
    let fields = match &map {
        None => &[][..],
        Some(Arg {
            value: Value::Map(entries),
            ..
        }) => entries.as_slice(),
        Some(other) => return Err(wrong(name, "a map of fields", other)),
    };
    let parts = Parts::of(name, of, source, &value)?;
    let truncated = parts.truncate(name, parsed, value.span)?;
    let map_span = map.as_ref().map_or(span, |arg| arg.span);
    let Some(overridden) = Override::read(name, of, fields, map_span)? else {
        return Ok(Value::Null);
    };
    let result = overridden.apply(name, of, parsed, truncated, env.now(), map_span)?;
    Ok(Value::Scalar(ElementProperty::Temporal(result)))
}

/// The Cypher name of a temporal type in messages.
fn type_name(of: TemporalType) -> &'static str {
    match of {
        TemporalType::Date => "a date",
        TemporalType::LocalTime => "a local time",
        TemporalType::LocalDateTime => "a local datetime",
        TemporalType::DateTime => "a datetime",
        TemporalType::Time => "a time",
    }
}

/// The 22G03 error "`name()` takes `what`, not <kind of `arg`>".
fn wrong<E>(name: &str, what: &str, arg: &Arg<E>) -> EvalError {
    let kind = match &arg.value {
        Value::Scalar(ElementProperty::Temporal(temporal)) => kind_name(temporal),
        other => other.kind_name(),
    };
    EvalError::invalid_type(format!("{name}() takes {what}, not {kind}"), arg.span)
}

/// An engine error of the truncation at `span` (`construct::temporal_error`: out of range
/// 22008, a parse or zone error 22007), its message naming the function.
fn engine_error(name: &str, error: TemporalError, span: Span) -> EvalError {
    let mut error = temporal_error(error, span);
    error.message = format!("{name}(): {}", error.message);
    error
}

/// The 22008 error of a result outside the supported range.
fn overflow(name: &str, span: Span) -> EvalError {
    EvalError::new(
        status::DATETIME_OVERFLOW,
        format!("{name}(): the result is outside the supported range (the years -9999 to 9999)"),
        span,
    )
}

/// The value projected onto what the target type keeps: the date, the time of day, the offset
/// (a datetime's or a time's own, kept by `datetime.truncate` and `time.truncate`) and a
/// datetime's zone name.
#[derive(Debug, Clone, Copy)]
struct Parts {
    date: Option<Date>,
    time: Time,
    offset: Option<i32>,
    zone: Option<ZoneId>,
}

impl Parts {
    fn of<E>(
        name: &str,
        of: TemporalType,
        source: Temporal,
        arg: &Arg<E>,
    ) -> Result<Parts, EvalError> {
        let midnight = Time::from_nanos_of_day(0).map_err(|e| engine_error(name, e, arg.span))?;
        let (date, time, offset, has_time) = match source {
            Temporal::Date(date) => (Some(date), midnight, None, false),
            Temporal::Time(time) => (None, time, None, true),
            Temporal::OffsetTime(value) => (None, value.time(), Some(value.offset_seconds()), true),
            Temporal::LocalDateTime(local) => (Some(local.date()), local.time(), None, true),
            Temporal::DateTime(value) => {
                let local = value.local();
                (
                    Some(local.date()),
                    local.time(),
                    Some(value.offset_seconds()),
                    true,
                )
            }
            Temporal::Duration(_) => return Err(wrong(name, "a date, a time or a datetime", arg)),
        };
        let zone = match source {
            Temporal::DateTime(value) => value.zone_id(),
            Temporal::Date(_)
            | Temporal::Time(_)
            | Temporal::OffsetTime(_)
            | Temporal::LocalDateTime(_)
            | Temporal::Duration(_) => None,
        };
        match of {
            TemporalType::LocalTime | TemporalType::Time if !has_time => {
                Err(wrong(name, "a value with a time of day", arg))
            }
            TemporalType::Date | TemporalType::LocalDateTime | TemporalType::DateTime
                if date.is_none() =>
            {
                Err(wrong(name, "a value with a date", arg))
            }
            TemporalType::Date
            | TemporalType::LocalTime
            | TemporalType::Time
            | TemporalType::LocalDateTime
            | TemporalType::DateTime => Ok(Parts {
                date,
                time,
                offset,
                zone,
            }),
        }
    }

    /// The parts truncated to `unit` (valid for the target type).
    fn truncate(self, name: &str, unit: Unit, span: Span) -> Result<Parts, EvalError> {
        let range = |_| TemporalError::out_of_range("the truncated value");
        let error = |e| engine_error(name, e, span);
        let midnight = Time::from_nanos_of_day(0).map_err(error)?;
        if let Some(engine) = unit.engine() {
            return match self.date {
                // `day` on a time of day alone gives midnight.
                None if unit == Unit::Day => Ok(Parts {
                    time: midnight,
                    ..self
                }),
                None => match arith::trunc(Temporal::Time(self.time), engine, None) {
                    Ok(Temporal::Time(time)) => Ok(Parts { time, ..self }),
                    Ok(_) => Err(error(range(()))),
                    Err(e) => Err(error(e)),
                },
                Some(date) => {
                    let local = Temporal::LocalDateTime(LocalDateTime::new(date, self.time));
                    match arith::trunc(local, engine, None) {
                        Ok(Temporal::LocalDateTime(local)) => Ok(Parts {
                            date: Some(local.date()),
                            time: local.time(),
                            ..self
                        }),
                        Ok(_) => Err(error(range(()))),
                        Err(e) => Err(error(e)),
                    }
                }
            };
        }
        let start_of = |year: i64| -> Result<Date, EvalError> {
            let year = i32::try_from(year).map_err(|_| error(range(())))?;
            Date::from_ymd(year, 1, 1).map_err(|_| error(range(())))
        };
        let date = match (unit, self.date) {
            (_, None) => return Err(error(range(()))),
            (Unit::Millennium, Some(date)) => {
                start_of((date.year() as i64).div_euclid(1000) * 1000)?
            }
            (Unit::Century, Some(date)) => start_of((date.year() as i64).div_euclid(100) * 100)?,
            (Unit::WeekYear, Some(date)) => {
                let (week_year, _) = civil::iso_week(date.days_since_epoch());
                Date::from_days_since_epoch(civil::week_year_start(week_year))
                    .map_err(|_| error(range(())))?
            }
            (
                Unit::Decade
                | Unit::Year
                | Unit::Quarter
                | Unit::Month
                | Unit::Week
                | Unit::Day
                | Unit::Hour
                | Unit::Minute
                | Unit::Second
                | Unit::Millisecond
                | Unit::Microsecond
                | Unit::Nanosecond,
                Some(_),
            ) => return Err(error(range(()))),
        };
        Ok(Parts {
            date: Some(date),
            time: midnight,
            ..self
        })
    }
}

/// The fields of the override map, read and checked against the target type.
#[derive(Debug, Default)]
struct Override {
    year: Option<i64>,
    month: Option<i64>,
    day: Option<i64>,
    week_year: Option<i64>,
    week: Option<i64>,
    day_of_week: Option<i64>,
    ordinal_day: Option<i64>,
    quarter: Option<i64>,
    day_of_quarter: Option<i64>,
    hour: Option<i64>,
    minute: Option<i64>,
    second: Option<i64>,
    millisecond: Option<i64>,
    microsecond: Option<i64>,
    nanosecond: Option<i64>,
    timezone: Option<CompactString>,
}

/// The date groups of the map (at most one, besides `year`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Group {
    Calendar,
    Week,
    Ordinal,
    Quarter,
}

impl Override {
    /// The map's fields; `None` when a field is `NULL` (the result is `NULL`).
    fn read<E>(
        name: &str,
        of: TemporalType,
        fields: &[(CompactString, Value<E>)],
        span: Span,
    ) -> Result<Option<Override>, EvalError> {
        let has_date = !matches!(of, TemporalType::LocalTime | TemporalType::Time);
        let has_time = !matches!(of, TemporalType::Date);
        let has_zone = matches!(of, TemporalType::DateTime | TemporalType::Time);
        let mut read = Override::default();
        for (key, value) in fields {
            let lower = key.to_ascii_lowercase();
            let slot = match lower.as_str() {
                "year" if has_date => &mut read.year,
                "month" if has_date => &mut read.month,
                "day" if has_date => &mut read.day,
                "weekyear" if has_date => &mut read.week_year,
                "week" if has_date => &mut read.week,
                "dayofweek" if has_date => &mut read.day_of_week,
                "ordinalday" if has_date => &mut read.ordinal_day,
                "quarter" if has_date => &mut read.quarter,
                "dayofquarter" if has_date => &mut read.day_of_quarter,
                "hour" if has_time => &mut read.hour,
                "minute" if has_time => &mut read.minute,
                "second" if has_time => &mut read.second,
                "millisecond" if has_time => &mut read.millisecond,
                "microsecond" if has_time => &mut read.microsecond,
                "nanosecond" if has_time => &mut read.nanosecond,
                "timezone" if has_zone => {
                    if read.timezone.is_some() {
                        return Err(twice(name, key, span));
                    }
                    match value {
                        Value::Null => return Ok(None),
                        _ => match value.as_str() {
                            Some(zone) => read.timezone = Some(CompactString::from(zone)),
                            None => {
                                return Err(EvalError::invalid_type(
                                    format!(
                                        "{name}(): the field timezone takes a string, not {}",
                                        value.kind_name()
                                    ),
                                    span,
                                ));
                            }
                        },
                    }
                    continue;
                }
                _ => {
                    return Err(EvalError::new(
                        status::INVALID_DATETIME_FORMAT,
                        format!("{name}(): {key} is not a field of {}", type_name(of)),
                        span,
                    ));
                }
            };
            if slot.is_some() {
                return Err(twice(name, key, span));
            }
            match value {
                Value::Null => return Ok(None),
                Value::Scalar(ElementProperty::Number(number)) => match number.n {
                    N::NegInt(n) => *slot = Some(n),
                    N::Float(_) => return Err(not_integer(name, key, value, span)),
                },
                _ => return Err(not_integer(name, key, value, span)),
            }
        }
        Ok(Some(read))
    }

    /// The date group the map uses; two groups (or `year` beside `weekYear`) are 22007.
    fn group(&self, name: &str, span: Span) -> Result<Option<Group>, EvalError> {
        let mut found: Option<Group> = None;
        for (present, group) in [
            (self.month.is_some() || self.day.is_some(), Group::Calendar),
            (
                self.week_year.is_some() || self.week.is_some() || self.day_of_week.is_some(),
                Group::Week,
            ),
            (self.ordinal_day.is_some(), Group::Ordinal),
            (
                self.quarter.is_some() || self.day_of_quarter.is_some(),
                Group::Quarter,
            ),
        ] {
            if present {
                if found.is_some() {
                    return Err(conflict(name, span));
                }
                found = Some(group);
            }
        }
        if self.year.is_some() && self.week_year.is_some() {
            return Err(conflict(name, span));
        }
        Ok(found)
    }

    /// The truncated parts with the fields replaced, as a value of `of`.
    fn apply(
        self,
        name: &str,
        of: TemporalType,
        unit: Unit,
        parts: Parts,
        now: DateTime,
        span: Span,
    ) -> Result<Temporal, EvalError> {
        let date = match parts.date {
            Some(date) => Some(self.date(name, date, span)?),
            None => None,
        };
        let time = self.time(name, unit, parts.time, span)?;
        let local = |date: Option<Date>| match date {
            Some(date) => Ok(LocalDateTime::new(date, time)),
            None => Err(overflow(name, span)),
        };
        Ok(match of {
            TemporalType::Date => Temporal::Date(date.ok_or_else(|| overflow(name, span))?),
            TemporalType::LocalTime => Temporal::Time(time),
            TemporalType::Time => {
                let offset = match &self.timezone {
                    Some(zone) => Zone::parse(zone)
                        .and_then(|zone| zone.offset_at(now))
                        .map_err(|e| engine_error(name, e, span))?,
                    None => parts.offset.unwrap_or(0),
                };
                Temporal::OffsetTime(
                    OffsetTime::new(time, offset).map_err(|e| engine_error(name, e, span))?,
                )
            }
            TemporalType::LocalDateTime => Temporal::LocalDateTime(local(date)?),
            TemporalType::DateTime => {
                let local = local(date)?;
                let value = match (&self.timezone, parts.zone) {
                    (Some(zone), _) => Zone::parse(zone).and_then(|zone| zone.resolve_named(local)),
                    (None, Some(id)) => id.zone().and_then(|zone| zone.resolve_named(local)),
                    (None, None) => DateTime::from_local(local, parts.offset.unwrap_or(0)),
                }
                .map_err(|e| engine_error(name, e, span))?;
                Temporal::DateTime(value)
            }
        })
    }

    /// `date` with the date fields replaced (`java.time`'s `with`: a field not given keeps its
    /// value, a day of the month a changed month lacks is clamped).
    fn date(&self, name: &str, date: Date, span: Span) -> Result<Date, EvalError> {
        let group = self.group(name, span)?;
        let days = date.days_since_epoch();
        let built = match group {
            None | Some(Group::Calendar) => {
                let year = self.year.unwrap_or(date.year() as i64);
                let month = self.month.unwrap_or(date.month() as i64);
                let year = field(name, "year", year, civil::MIN_YEAR as i64, 9999, span)?;
                let month = field(name, "month", month, 1, 12, span)? as u8;
                let length = civil::days_in_month(year as i32, month) as i64;
                let day = match self.day {
                    Some(day) => field(name, "day", day, 1, length, span)?,
                    None => (date.day() as i64).min(length),
                };
                Some(civil::days_from_civil(year as i32, month, day as u8))
            }
            Some(Group::Week) => {
                let (current_year, current_week) = civil::iso_week(days);
                let week_year = self.week_year.or(self.year).unwrap_or(current_year as i64);
                let week_year = field(name, "weekYear", week_year, -9999, 9999, span)? as i32;
                let weeks = civil::weeks_in_week_year(week_year) as i64;
                let week = match self.week {
                    Some(week) => field(name, "week", week, 1, weeks, span)?,
                    None => (current_week as i64).min(weeks),
                };
                let day_of_week = self
                    .day_of_week
                    .unwrap_or(civil::iso_day_of_week(days) as i64);
                let day_of_week = field(name, "dayOfWeek", day_of_week, 1, 7, span)?;
                civil::from_iso_week(week_year, week as u8, day_of_week as u8)
            }
            Some(Group::Ordinal) => {
                let year = self.year.unwrap_or(date.year() as i64);
                let year = field(name, "year", year, -9999, 9999, span)? as i32;
                let length = if civil::is_leap(year) { 366 } else { 365 };
                let ordinal = self.ordinal_day.unwrap_or(civil::day_of_year(days) as i64);
                let ordinal = field(name, "ordinalDay", ordinal, 1, length, span)?;
                civil::from_ordinal(year, ordinal as u16)
            }
            Some(Group::Quarter) => {
                let year = self.year.unwrap_or(date.year() as i64);
                let year = field(name, "year", year, -9999, 9999, span)? as i32;
                let current_quarter = (date.month() as i64 - 1) / 3 + 1;
                let quarter = field(
                    name,
                    "quarter",
                    self.quarter.unwrap_or(current_quarter),
                    1,
                    4,
                    span,
                )?;
                match self.day_of_quarter {
                    Some(day) => {
                        let first = civil::days_from_civil(year, (quarter as u8 - 1) * 3 + 1, 1);
                        let next = if quarter == 4 {
                            civil::days_from_civil(year + 1, 1, 1)
                        } else {
                            civil::days_from_civil(year, quarter as u8 * 3 + 1, 1)
                        };
                        let length = (next - first) as i64;
                        let day = field(name, "dayOfQuarter", day, 1, length, span)?;
                        civil::from_quarter(year, quarter as u8, day as u8)
                    }
                    None => {
                        // The month within the quarter and the day of the month stay.
                        let month = (quarter as u8 - 1) * 3 + (date.month() - 1) % 3 + 1;
                        let length = civil::days_in_month(year, month);
                        Some(civil::days_from_civil(year, month, date.day().min(length)))
                    }
                }
            }
        };
        built
            .and_then(|days| Date::from_days_since_epoch(days).ok())
            .ok_or_else(|| overflow(name, span))
    }

    /// `time` with the time fields replaced. Sub-second fields sum (Neo4j); with the unit
    /// `millisecond` a `microsecond` or `nanosecond` adds to the kept milliseconds, with
    /// `microsecond` a `nanosecond` to the kept microseconds.
    fn time(&self, name: &str, unit: Unit, time: Time, span: Span) -> Result<Time, EvalError> {
        let hour = field(
            name,
            "hour",
            self.hour.unwrap_or(time.hour() as i64),
            0,
            23,
            span,
        )?;
        let minute = field(
            name,
            "minute",
            self.minute.unwrap_or(time.minute() as i64),
            0,
            59,
            span,
        )?;
        let second = field(
            name,
            "second",
            self.second.unwrap_or(time.second() as i64),
            0,
            59,
            span,
        )?;
        // The kept nanoseconds of the second.
        let kept = time.nanosecond() as i64;
        let mut millisecond = self.millisecond;
        let mut microsecond = self.microsecond;
        if unit == Unit::Millisecond
            && millisecond.is_none()
            && (microsecond.is_some() || self.nanosecond.is_some())
        {
            millisecond = Some(kept / 1_000_000);
        }
        if unit == Unit::Microsecond && microsecond.is_none() && self.nanosecond.is_some() {
            // The kept microseconds of the second, in place of a millisecond field.
            microsecond = Some(kept / 1000 - millisecond.unwrap_or(0) * 1000);
        }
        let nanos = if millisecond.is_none() && microsecond.is_none() && self.nanosecond.is_none() {
            kept
        } else {
            let ms = millisecond.map_or(Ok(0), |v| field(name, "millisecond", v, 0, 999, span))?;
            let us_max = if millisecond.is_some() { 999 } else { 999_999 };
            let us =
                microsecond.map_or(Ok(0), |v| field(name, "microsecond", v, 0, us_max, span))?;
            let ns_max = if millisecond.is_some() || microsecond.is_some() {
                999
            } else {
                999_999_999
            };
            let ns = self
                .nanosecond
                .map_or(Ok(0), |v| field(name, "nanosecond", v, 0, ns_max, span))?;
            ms * 1_000_000 + us * 1000 + ns
        };
        Time::from_hms_nano(hour as u8, minute as u8, second as u8, nanos as u32)
            .map_err(|e| engine_error(name, e, span))
    }
}

/// `value` of the field `key` checked against `min..=max` (22008 outside).
fn field(
    name: &str,
    key: &str,
    value: i64,
    min: i64,
    max: i64,
    span: Span,
) -> Result<i64, EvalError> {
    if (min..=max).contains(&value) {
        Ok(value)
    } else {
        Err(EvalError::new(
            status::DATETIME_OVERFLOW,
            format!("{name}(): the field {key} cannot be {value}"),
            span,
        ))
    }
}

/// The 22007 error of a field given twice (keys compare ASCII case-insensitively).
fn twice(name: &str, key: &str, span: Span) -> EvalError {
    EvalError::new(
        status::INVALID_DATETIME_FORMAT,
        format!("{name}(): the field {key} is given twice"),
        span,
    )
}

/// The 22007 error of date fields from different groups.
fn conflict(name: &str, span: Span) -> EvalError {
    EvalError::new(
        status::INVALID_DATETIME_FORMAT,
        format!(
            "{name}(): the date fields conflict (use one of month / day, week / dayOfWeek / \
             weekYear, ordinalDay, quarter / dayOfQuarter)"
        ),
        span,
    )
}

/// The 22G03 error of a field that is no integer.
fn not_integer<E>(name: &str, key: &str, value: &Value<E>, span: Span) -> EvalError {
    EvalError::invalid_type(
        format!(
            "{name}(): the field {key} takes an integer, not {}",
            value.kind_name()
        ),
        span,
    )
}

#[cfg(test)]
mod tests {
    use super::super::super::super::parser::Function;
    use super::super::super::call;
    use super::super::super::test_env::{El, TestRow, int, string as text};
    use super::*;

    const SPAN: Span = Span {
        start: 0,
        end: 0,
        line: 1,
        column: 1,
    };

    fn arg(value: Value<El>) -> Arg<El> {
        Arg { value, span: SPAN }
    }

    fn temporal(value: Temporal) -> Value<El> {
        Value::Scalar(ElementProperty::Temporal(value))
    }

    fn date(y: i32, m: u8, d: u8) -> Date {
        Date::from_ymd(y, m, d).expect("a date")
    }

    fn time(h: u8, m: u8, s: u8, us: u32) -> Time {
        Time::from_hms_micro(h, m, s, us).expect("a time")
    }

    /// 1984-10-11T12:31:14.645876 (local).
    fn local() -> LocalDateTime {
        LocalDateTime::new(date(1984, 10, 11), time(12, 31, 14, 645_876))
    }

    /// The same wall clock at `offset` seconds.
    fn zoned(offset: i32) -> Temporal {
        Temporal::DateTime(DateTime::from_local(local(), offset).expect("a datetime"))
    }

    fn map(fields: &[(&str, Value<El>)]) -> Value<El> {
        Value::Map(
            fields
                .iter()
                .map(|(key, value)| (CompactString::from(*key), value.clone()))
                .collect(),
        )
    }

    fn run(
        of: TemporalType,
        unit: &str,
        value: Value<El>,
        fields: Option<Value<El>>,
    ) -> Result<Value<El>, EvalError> {
        let mut args = vec![arg(text(unit)), arg(value)];
        args.extend(fields.map(arg));
        call(
            Function::Temporal(TemporalFunction::Truncate(of)),
            args,
            &TestRow::default(),
            SPAN,
        )
    }

    fn ok(of: TemporalType, unit: &str, value: Temporal, fields: Option<Value<El>>) -> Temporal {
        match run(of, unit, temporal(value), fields) {
            Ok(Value::Scalar(ElementProperty::Temporal(t))) => t,
            other => panic!("{unit}: {other:?}"),
        }
    }

    fn status_of(
        of: TemporalType,
        unit: &str,
        value: Value<El>,
        fields: Option<Value<El>>,
    ) -> &'static str {
        match run(of, unit, value, fields) {
            Err(error) => error.gqlstatus,
            Ok(value) => panic!("{unit}: {value:?}"),
        }
    }

    #[test]
    fn date_units_follow_neo4j() {
        let d = |y, m, dd| Temporal::Date(date(y, m, dd));
        for (unit, source, expected) in [
            ("millennium", d(2017, 10, 11), d(2000, 1, 1)),
            ("century", d(1984, 10, 11), d(1900, 1, 1)),
            ("century", d(2000, 1, 1), d(2000, 1, 1)),
            ("millennium", d(-1, 6, 1), d(-1000, 1, 1)),
            ("century", d(-150, 6, 1), d(-200, 1, 1)),
            ("decade", d(1984, 10, 11), d(1980, 1, 1)),
            ("year", d(1984, 10, 11), d(1984, 1, 1)),
            ("weekYear", d(1984, 2, 1), d(1984, 1, 2)),
            ("weekYear", d(1984, 1, 1), d(1983, 1, 3)),
            ("quarter", d(1984, 11, 11), d(1984, 10, 1)),
            ("month", d(1984, 10, 11), d(1984, 10, 1)),
            ("week", d(1984, 10, 11), d(1984, 10, 8)),
            ("day", d(1984, 10, 11), d(1984, 10, 11)),
            ("WEEKYEAR", d(1984, 2, 1), d(1984, 1, 2)),
        ] {
            assert_eq!(
                ok(TemporalType::Date, unit, source, None),
                expected,
                "{unit}"
            );
        }
        // The other types project first: the date of a local datetime and of a datetime (its
        // own wall clock).
        assert_eq!(
            ok(
                TemporalType::Date,
                "month",
                Temporal::LocalDateTime(local()),
                None
            ),
            d(1984, 10, 1)
        );
        assert_eq!(
            ok(TemporalType::Date, "day", zoned(3600), None),
            d(1984, 10, 11)
        );
    }

    #[test]
    fn overrides_replace_fields_after_truncation() {
        let d = |y, m, dd| Temporal::Date(date(y, m, dd));
        let day = |n| Some(map(&[("day", int(n))]));
        assert_eq!(
            ok(TemporalType::Date, "millennium", d(2017, 10, 11), day(2)),
            d(2000, 1, 2)
        );
        assert_eq!(
            ok(TemporalType::Date, "weekYear", d(1984, 2, 1), day(5)),
            d(1984, 1, 5)
        );
        assert_eq!(
            ok(
                TemporalType::Date,
                "weekYear",
                Temporal::LocalDateTime(local()),
                day(5)
            ),
            d(1984, 1, 5)
        );
        assert_eq!(
            ok(
                TemporalType::Date,
                "week",
                d(1984, 10, 11),
                Some(map(&[("dayOfWeek", int(2))]))
            ),
            d(1984, 10, 9)
        );
        assert_eq!(
            ok(
                TemporalType::Date,
                "day",
                d(1984, 11, 11),
                Some(map(&[("week", int(1))]))
            ),
            d(1984, 1, 8)
        );
        assert_eq!(
            ok(
                TemporalType::Date,
                "day",
                d(1984, 11, 11),
                Some(map(&[("ordinalDay", int(28))]))
            ),
            d(1984, 1, 28)
        );
        assert_eq!(
            ok(
                TemporalType::Date,
                "day",
                d(1984, 11, 11),
                Some(map(&[("quarter", int(1))]))
            ),
            d(1984, 2, 11)
        );
        assert_eq!(
            ok(
                TemporalType::Date,
                "year",
                d(1984, 11, 11),
                Some(map(&[("dayOfQuarter", int(45))]))
            ),
            d(1984, 2, 14)
        );
        // A month change clamps the day; an explicit day out of range is 22008.
        assert_eq!(
            ok(
                TemporalType::Date,
                "day",
                d(1984, 1, 31),
                Some(map(&[("month", int(2))]))
            ),
            d(1984, 2, 29)
        );
        assert_eq!(
            status_of(
                TemporalType::Date,
                "month",
                temporal(d(1984, 2, 11)),
                Some(map(&[("day", int(30))]))
            ),
            status::DATETIME_OVERFLOW
        );
    }

    #[test]
    fn time_units_and_sub_second_overrides() {
        let lt = |h, m, s, us| Temporal::Time(time(h, m, s, us));
        let source = Temporal::LocalDateTime(local());
        for (unit, expected) in [
            ("day", lt(0, 0, 0, 0)),
            ("hour", lt(12, 0, 0, 0)),
            ("minute", lt(12, 31, 0, 0)),
            ("second", lt(12, 31, 14, 0)),
            ("millisecond", lt(12, 31, 14, 645_000)),
            ("microsecond", lt(12, 31, 14, 645_876)),
            ("nanosecond", lt(12, 31, 14, 645_876)),
        ] {
            assert_eq!(
                ok(TemporalType::LocalTime, unit, source, None),
                expected,
                "{unit}"
            );
            assert_eq!(
                ok(TemporalType::LocalTime, unit, zoned(-3600), None),
                expected,
                "{unit}"
            );
            if unit != "day" {
                let time_source = lt(12, 31, 14, 645_876);
                assert_eq!(
                    ok(TemporalType::LocalTime, unit, time_source, None),
                    expected
                );
            }
        }
        let fields = |key, n| Some(map(&[(key, int(n))]));
        // millisecond + a microsecond field adds to the kept milliseconds (Neo4j).
        assert_eq!(
            ok(
                TemporalType::LocalTime,
                "millisecond",
                source,
                fields("microsecond", 7)
            ),
            lt(12, 31, 14, 645_007)
        );
        // A millisecond field alone replaces the fraction.
        assert_eq!(
            ok(
                TemporalType::LocalTime,
                "second",
                source,
                fields("millisecond", 5)
            ),
            lt(12, 31, 14, 5_000)
        );
        assert_eq!(
            ok(TemporalType::LocalTime, "hour", source, fields("minute", 7)),
            lt(12, 7, 0, 0)
        );
        // Nanosecond fields are kept exactly (U1).
        assert_eq!(
            ok(
                TemporalType::LocalTime,
                "microsecond",
                source,
                fields("nanosecond", 0)
            ),
            lt(12, 31, 14, 645_876)
        );
        assert_eq!(
            ok(
                TemporalType::LocalTime,
                "second",
                source,
                fields("nanosecond", 3000)
            ),
            lt(12, 31, 14, 3)
        );
        let lt_ns = |h, m, s, ns| Temporal::Time(Time::from_hms_nano(h, m, s, ns).expect("a time"));
        for (unit, expected) in [
            ("microsecond", lt_ns(12, 31, 14, 645_876_002)),
            ("millisecond", lt_ns(12, 31, 14, 645_000_002)),
            ("day", lt_ns(0, 0, 0, 2)),
        ] {
            assert_eq!(
                ok(
                    TemporalType::LocalTime,
                    unit,
                    source,
                    fields("nanosecond", 2)
                ),
                expected,
                "{unit}"
            );
        }
        assert_eq!(
            status_of(
                TemporalType::LocalTime,
                "hour",
                temporal(source),
                fields("hour", 24)
            ),
            status::DATETIME_OVERFLOW
        );
    }

    #[test]
    fn datetime_keeps_its_offset_or_takes_utc_or_the_map_zone() {
        let dt = |y, m, d, h, min, offset| {
            let local = LocalDateTime::new(date(y, m, d), time(h, min, 0, 0));
            Temporal::DateTime(DateTime::from_local(local, offset).expect("a datetime"))
        };
        assert_eq!(
            ok(TemporalType::DateTime, "millennium", zoned(3600), None),
            dt(1000, 1, 1, 0, 0, 3600)
        );
        assert_eq!(
            ok(TemporalType::DateTime, "hour", zoned(-3600), None),
            dt(1984, 10, 11, 12, 0, -3600)
        );
        assert_eq!(
            ok(
                TemporalType::DateTime,
                "year",
                Temporal::Date(date(1984, 10, 11)),
                None
            ),
            dt(1984, 1, 1, 0, 0, 0)
        );
        assert_eq!(
            ok(
                TemporalType::DateTime,
                "minute",
                Temporal::LocalDateTime(local()),
                None
            ),
            dt(1984, 10, 11, 12, 31, 0)
        );
        // The wall clock stays; the zone gives the offset at that time (U3).
        let zone = |name: &str| Some(map(&[("timezone", text(name))]));
        assert_eq!(
            ok(
                TemporalType::DateTime,
                "hour",
                zoned(-3600),
                zone("Europe/Stockholm")
            ),
            dt(1984, 10, 11, 12, 0, 3600)
        );
        assert_eq!(
            ok(
                TemporalType::DateTime,
                "month",
                zoned(0),
                zone("Europe/Stockholm")
            ),
            dt(1984, 10, 1, 0, 0, 3600)
        );
        assert_eq!(
            ok(
                TemporalType::DateTime,
                "day",
                Temporal::Date(date(1984, 7, 1)),
                zone("Europe/Stockholm")
            ),
            dt(1984, 7, 1, 0, 0, 7200)
        );
        assert_eq!(
            ok(TemporalType::DateTime, "day", zoned(0), zone("+05:30")),
            dt(1984, 10, 11, 0, 0, 19_800)
        );
        assert_eq!(
            status_of(
                TemporalType::DateTime,
                "day",
                temporal(zoned(0)),
                zone("Mars/Olympus")
            ),
            status::INVALID_DATETIME_FORMAT
        );
        assert_eq!(
            ok(
                TemporalType::LocalDateTime,
                "week",
                zoned(3600),
                Some(map(&[("dayOfWeek", int(2))]))
            ),
            Temporal::LocalDateTime(LocalDateTime::new(date(1984, 10, 9), time(0, 0, 0, 0)))
        );
    }

    #[test]
    fn errors_and_null() {
        let d = temporal(Temporal::Date(date(1984, 10, 11)));
        let lt = temporal(Temporal::Time(time(12, 0, 0, 0)));
        let dur = temporal(Temporal::Duration(crate::value::temporal::Duration::new(
            1, 0, 0,
        )));
        for (of, unit, value, fields, expected) in [
            (
                TemporalType::Date,
                "hour",
                d.clone(),
                None,
                status::INVALID_PARAMETER,
            ),
            (
                TemporalType::LocalTime,
                "month",
                lt.clone(),
                None,
                status::INVALID_PARAMETER,
            ),
            (
                TemporalType::Date,
                "fortnight",
                d.clone(),
                None,
                status::INVALID_PARAMETER,
            ),
            (
                TemporalType::Date,
                "day",
                lt.clone(),
                None,
                status::INVALID_VALUE_TYPE,
            ),
            (
                TemporalType::LocalTime,
                "hour",
                d.clone(),
                None,
                status::INVALID_VALUE_TYPE,
            ),
            (
                TemporalType::DateTime,
                "day",
                dur,
                None,
                status::INVALID_VALUE_TYPE,
            ),
            (
                TemporalType::Date,
                "day",
                int(1),
                None,
                status::INVALID_VALUE_TYPE,
            ),
            (
                TemporalType::Date,
                "day",
                d.clone(),
                Some(int(1)),
                status::INVALID_VALUE_TYPE,
            ),
            (
                TemporalType::Date,
                "day",
                d.clone(),
                Some(map(&[("hour", int(1))])),
                status::INVALID_DATETIME_FORMAT,
            ),
            (
                TemporalType::LocalDateTime,
                "day",
                d.clone(),
                Some(map(&[("timezone", text("+01:00"))])),
                status::INVALID_DATETIME_FORMAT,
            ),
            (
                TemporalType::Date,
                "day",
                d.clone(),
                Some(map(&[("day", int(1)), ("week", int(1))])),
                status::INVALID_DATETIME_FORMAT,
            ),
            (
                TemporalType::Date,
                "day",
                d.clone(),
                Some(map(&[("day", int(1)), ("DAY", int(2))])),
                status::INVALID_DATETIME_FORMAT,
            ),
            (
                TemporalType::Date,
                "day",
                d.clone(),
                Some(map(&[("day", text("1"))])),
                status::INVALID_VALUE_TYPE,
            ),
            (
                TemporalType::Date,
                "millennium",
                temporal(Temporal::Date(date(-9999, 6, 1))),
                None,
                status::DATETIME_OVERFLOW,
            ),
        ] {
            assert_eq!(status_of(of, unit, value, fields), expected, "{unit}");
        }
        let error = run(TemporalType::Date, "hour", d.clone(), None).unwrap_err();
        assert_eq!(
            error.message,
            "date.truncate(): the unit hour is not defined for a date"
        );
        // NULL anywhere, a NULL field too, gives NULL.
        let null = Value::Null;
        assert_eq!(
            run(TemporalType::Date, "day", null.clone(), None),
            Ok(Value::Null)
        );
        assert_eq!(
            run(TemporalType::Date, "day", d.clone(), Some(null.clone())),
            Ok(Value::Null)
        );
        assert_eq!(
            run(TemporalType::Date, "day", d, Some(map(&[("day", null)]))),
            Ok(Value::Null)
        );
    }
}
