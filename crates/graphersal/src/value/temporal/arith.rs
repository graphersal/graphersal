//! Arithmetic on temporal values: adding units and durations, differences, truncation and parts
//! (book "Dates and Times").
//!
//! - Units below a day ([`DT::Microsecond`] to [`DT::Hour`]) are fixed lengths: they move the
//!   instant of a `datetime` (whose offset stays, or becomes the zone's offset at the new instant
//!   when a zone is given) and wrap a `time` modulo 24 hours (PostgreSQL).
//! - [`DT::Day`] and [`DT::Week`] add calendar days to the wall-clock date: in a zone the
//!   wall-clock time stays across a daylight saving change (a day is then 23 or 25 hours).
//! - [`DT::Month`] and larger add months to the wall-clock date and clamp the day to the end of
//!   the target month (PostgreSQL, `java.time`).
//! - A wall-clock time a zone skips moves forward by the gap, one it repeats takes the earlier
//!   offset ("compatible" disambiguation, `zone.rs`).
//!
//! Every operation is checked: a result outside the years -9999 to 9999 or an overflowing count
//! is [`TemporalError::OutOfRange`], a combination a type does not have (hours of a `date`, the
//! offset of a `time`, a part-only token as a unit) is [`TemporalError::Unsupported`]. A zone is
//! accepted for a `datetime` only: the other types have no instant to place in a zone.

use super::civil::{self, MICROS_PER_DAY, MICROS_PER_SECOND};
use super::zone::Zone;
use super::{
    Date, DateTime, Duration, LocalDateTime, Temporal, TemporalError, TemporalKind, TemporalResult,
    Time,
};
use crate::value::Number;
use crate::value::dt::DT;

/// The supported range, for the error text.
const RESULT: &str = "the result";

fn out_of_range() -> TemporalError {
    TemporalError::out_of_range(RESULT)
}

/// Refuses a zone for a type that has no instant.
fn no_zone(kind: TemporalKind, zone: Option<&Zone>) -> TemporalResult<()> {
    match zone {
        Some(_) => Err(TemporalError::unsupported("a time zone argument", kind)),
        None => Ok(()),
    }
}

/// The wall-clock view of a `datetime`: in the zone when one is given, else at its own offset.
fn local_of(value: DateTime, zone: Option<&Zone>) -> TemporalResult<LocalDateTime> {
    match zone {
        Some(zone) => Ok(zone.at(value)?.local()),
        None => Ok(value.local()),
    }
}

/// The `datetime` of a wall-clock time computed from `original`: resolved in the zone when one
/// is given, else read at the original offset.
fn from_local_of(
    local: LocalDateTime,
    original: DateTime,
    zone: Option<&Zone>,
) -> TemporalResult<DateTime> {
    match zone {
        Some(zone) => zone.resolve(local),
        None => DateTime::from_local(local, original.offset_seconds()),
    }
}

/// `n` units of `len` microseconds, checked.
fn scaled(n: i64, len: i64) -> TemporalResult<i64> {
    n.checked_mul(len).ok_or_else(out_of_range)
}

/// A local microsecond count plus `delta`, in range.
fn shift_local(local: LocalDateTime, delta: i64) -> TemporalResult<LocalDateTime> {
    let micros = local.micros().checked_add(delta).ok_or_else(out_of_range)?;
    LocalDateTime::from_micros(micros).map_err(|_| out_of_range())
}

/// A day number plus `days`, in range.
fn shift_days(days: i32, delta: i64) -> TemporalResult<i32> {
    let days = (days as i64).checked_add(delta).ok_or_else(out_of_range)?;
    if civil::day_in_range(days) {
        Ok(days as i32)
    } else {
        Err(out_of_range())
    }
}

/// A local date-time plus `months`, the day clamped to the end of the month, the time kept.
fn local_add_months(local: LocalDateTime, months: i64) -> TemporalResult<LocalDateTime> {
    let days =
        civil::add_months(local.date().days_since_epoch(), months).ok_or_else(out_of_range)?;
    Ok(LocalDateTime::new(Date { days }, local.time()))
}

/// A local date-time plus `days` calendar days.
fn local_add_days(local: LocalDateTime, days: i64) -> TemporalResult<LocalDateTime> {
    let micros = days.checked_mul(MICROS_PER_DAY).ok_or_else(out_of_range)?;
    shift_local(local, micros)
}

/// The operation text of a unit.
fn adding(unit: DT) -> String {
    format!("adding the unit {unit}")
}

/// Adds `n` units to a value (PostgreSQL `value + n * interval '1 unit'`, TinkerPop
/// `dateAdd(DT.unit, n)`).
pub(crate) fn add(
    value: Temporal,
    unit: DT,
    n: i64,
    zone: Option<&Zone>,
) -> TemporalResult<Temporal> {
    let kind = value.kind();
    if !unit.is_unit() {
        return Err(TemporalError::unsupported(adding(unit), kind));
    }
    let fixed = unit.fixed_micros();
    let days = unit.calendar_days();
    let months = unit.calendar_months();
    match value {
        Temporal::DateTime(v) => {
            if let Some(len) = fixed {
                let micros = v
                    .epoch_micros()
                    .checked_add(scaled(n, len)?)
                    .ok_or_else(out_of_range)?;
                let instant = DateTime::from_micros(micros, 0).map_err(|_| out_of_range())?;
                let offset = match zone {
                    Some(zone) => zone.offset_at(instant)?,
                    None => v.offset_seconds(),
                };
                return Ok(Temporal::DateTime(
                    DateTime::from_micros(micros, offset).map_err(|_| out_of_range())?,
                ));
            }
            let local = local_of(v, zone)?;
            let shifted = match (days, months) {
                (Some(k), _) => local_add_days(local, scaled(n, k)?)?,
                (None, Some(k)) => local_add_months(local, scaled(n, k)?)?,
                (None, None) => return Err(TemporalError::unsupported(adding(unit), kind)),
            };
            Ok(Temporal::DateTime(
                from_local_of(shifted, v, zone).map_err(|_| out_of_range())?,
            ))
        }
        Temporal::LocalDateTime(v) => {
            no_zone(kind, zone)?;
            let shifted = match (fixed, days, months) {
                (Some(len), _, _) => shift_local(v, scaled(n, len)?)?,
                (None, Some(k), _) => local_add_days(v, scaled(n, k)?)?,
                (None, None, Some(k)) => local_add_months(v, scaled(n, k)?)?,
                (None, None, None) => {
                    return Err(TemporalError::unsupported(adding(unit), kind));
                }
            };
            Ok(Temporal::LocalDateTime(shifted))
        }
        Temporal::Date(v) => {
            no_zone(kind, zone)?;
            let new_days = match (days, months) {
                (Some(k), _) => shift_days(v.days, scaled(n, k)?)?,
                (None, Some(k)) => {
                    civil::add_months(v.days, scaled(n, k)?).ok_or_else(out_of_range)?
                }
                (None, None) => return Err(TemporalError::unsupported(adding(unit), kind)),
            };
            Ok(Temporal::Date(Date { days: new_days }))
        }
        Temporal::Time(v) => {
            no_zone(kind, zone)?;
            let Some(len) = fixed else {
                return Err(TemporalError::unsupported(adding(unit), kind));
            };
            let micros =
                (v.micros as i128 + n as i128 * len as i128).rem_euclid(MICROS_PER_DAY as i128);
            Ok(Temporal::Time(Time {
                micros: micros as i64,
            }))
        }
        Temporal::Duration(v) => {
            no_zone(kind, zone)?;
            let delta = match (fixed, days, months) {
                (Some(len), _, _) => Duration::from_micros(scaled(n, len)?),
                (None, Some(k), _) => Duration::new(0, to_i32(scaled(n, k)?)?, 0),
                (None, None, Some(k)) => Duration::new(to_i32(scaled(n, k)?)?, 0, 0),
                (None, None, None) => {
                    return Err(TemporalError::unsupported(adding(unit), kind));
                }
            };
            Ok(Temporal::Duration(
                v.checked_add(delta).map_err(|_| out_of_range())?,
            ))
        }
    }
}

fn to_i32(value: i64) -> TemporalResult<i32> {
    i32::try_from(value).map_err(|_| out_of_range())
}

/// Adds a duration: its months, then its days, then its microseconds (PostgreSQL order).
pub(crate) fn add_duration(
    value: Temporal,
    duration: Duration,
    zone: Option<&Zone>,
) -> TemporalResult<Temporal> {
    let kind = value.kind();
    let (months, days, micros) = (
        duration.months as i64,
        duration.days as i64,
        duration.micros,
    );
    match value {
        Temporal::DateTime(v) => {
            let local = local_of(v, zone)?;
            let local = local_add_days(local_add_months(local, months)?, days)?;
            let shifted = from_local_of(local, v, zone).map_err(|_| out_of_range())?;
            let instant = shifted
                .epoch_micros()
                .checked_add(micros)
                .ok_or_else(out_of_range)?;
            let probe = DateTime::from_micros(instant, 0).map_err(|_| out_of_range())?;
            let offset = match zone {
                Some(zone) => zone.offset_at(probe)?,
                None => v.offset_seconds(),
            };
            Ok(Temporal::DateTime(
                DateTime::from_micros(instant, offset).map_err(|_| out_of_range())?,
            ))
        }
        Temporal::LocalDateTime(v) => {
            no_zone(kind, zone)?;
            let local = local_add_days(local_add_months(v, months)?, days)?;
            Ok(Temporal::LocalDateTime(shift_local(local, micros)?))
        }
        Temporal::Date(v) => {
            no_zone(kind, zone)?;
            if micros != 0 {
                return Err(TemporalError::unsupported(
                    "adding a duration with a time part",
                    kind,
                ));
            }
            let d = civil::add_months(v.days, months).ok_or_else(out_of_range)?;
            Ok(Temporal::Date(Date {
                days: shift_days(d, days)?,
            }))
        }
        Temporal::Time(v) => {
            no_zone(kind, zone)?;
            if months != 0 || days != 0 {
                return Err(TemporalError::unsupported(
                    "adding a duration with months or days",
                    kind,
                ));
            }
            let micros = (v.micros as i128 + micros as i128).rem_euclid(MICROS_PER_DAY as i128);
            Ok(Temporal::Time(Time {
                micros: micros as i64,
            }))
        }
        Temporal::Duration(v) => {
            no_zone(kind, zone)?;
            Ok(Temporal::Duration(
                v.checked_add(duration).map_err(|_| out_of_range())?,
            ))
        }
    }
}

/// The two operands of a difference, which must be of the same type.
fn same_kind(a: Temporal, b: Temporal, operation: &str) -> TemporalResult<TemporalKind> {
    if a.kind() == b.kind() {
        Ok(a.kind())
    } else {
        Err(TemporalError::unsupported(
            format!("{operation} with a {}", b.kind()),
            a.kind(),
        ))
    }
}

/// `a - b` in milliseconds, rounded down per operand (TinkerPop `dateDiff`: the difference of
/// the epoch milliseconds of two `datetime`s); for the local types the same on their wall-clock
/// microseconds (a `date` at midnight, a `time` within the day).
pub(crate) fn diff_millis(a: Temporal, b: Temporal) -> TemporalResult<i64> {
    let kind = same_kind(a, b, "the difference")?;
    let millis = |value: Temporal| -> TemporalResult<i64> {
        match value {
            Temporal::DateTime(v) => Ok(v.epoch_millis()),
            Temporal::LocalDateTime(v) => Ok(v.micros.div_euclid(1000)),
            Temporal::Date(v) => Ok(v.days as i64 * 86_400_000),
            Temporal::Time(v) => Ok(v.micros / 1000),
            Temporal::Duration(_) => Err(TemporalError::unsupported("the difference", kind)),
        }
    };
    millis(a)?.checked_sub(millis(b)?).ok_or_else(out_of_range)
}

/// Whole calendar days from `from` to `to` (truncated toward zero by the time of day).
pub(crate) fn days_between(from: LocalDateTime, to: LocalDateTime) -> i64 {
    let mut days = to.date().days as i64 - from.date().days as i64;
    let (from_time, to_time) = (from.time().micros, to.time().micros);
    if days > 0 && to_time < from_time {
        days -= 1;
    } else if days < 0 && to_time > from_time {
        days += 1;
    }
    days
}

/// Whole months from `from` to `to`, truncated toward zero, as `java.time`
/// `ChronoUnit.MONTHS.between` counts them: the month count of the dates, one less when the
/// day of the month (and then the time of day) of `to` has not reached that of `from` (so
/// January 31 to February 28 is 0 months, February 28 to March 31 is 1).
pub(crate) fn months_between(from: LocalDateTime, to: LocalDateTime) -> i64 {
    let from_day = from.date().days;
    let mut end = to.date().days;
    let (from_time, to_time) = (from.time().micros, to.time().micros);
    // `LocalDateTime.until`: an end whose time is before the start's counts from the day before.
    if end > from_day && to_time < from_time {
        end -= 1;
    } else if end < from_day && to_time > from_time {
        end += 1;
    }
    let (fy, fm, fd) = civil::civil_from_days(from_day);
    let (ty, tm, td) = civil::civil_from_days(end);
    let mut months = (ty as i64 * 12 + tm as i64) - (fy as i64 * 12 + fm as i64);
    let days = td as i64 - fd as i64;
    if months > 0 && days < 0 {
        months -= 1;
    } else if months < 0 && days > 0 {
        months += 1;
    }
    months
}

/// The wall-clock views of two values of one type for a calendar difference: a `datetime`'s in
/// the zone, else both at `b`'s offset (as `java.time` `b.until(a, unit)`).
fn local_pair(
    a: Temporal,
    b: Temporal,
    zone: Option<&Zone>,
    operation: &str,
) -> TemporalResult<(LocalDateTime, LocalDateTime)> {
    let kind = same_kind(a, b, operation)?;
    match (a, b) {
        (Temporal::DateTime(a), Temporal::DateTime(b)) => match zone {
            Some(zone) => Ok((zone.at(a)?.local(), zone.at(b)?.local())),
            None => Ok((a.with_offset(b.offset_seconds())?.local(), b.local())),
        },
        (Temporal::LocalDateTime(a), Temporal::LocalDateTime(b)) => {
            no_zone(kind, zone)?;
            Ok((a, b))
        }
        (Temporal::Date(a), Temporal::Date(b)) => {
            no_zone(kind, zone)?;
            Ok((
                LocalDateTime::new(a, Time { micros: 0 }),
                LocalDateTime::new(b, Time { micros: 0 }),
            ))
        }
        _ => Err(TemporalError::unsupported(operation.to_string(), kind)),
    }
}

/// `a - b` in whole units, truncated toward zero (PostgreSQL / `java.time` `ChronoUnit.between`):
/// fixed units from the elapsed microseconds, days and weeks from the wall-clock dates and
/// times, months and larger from whole months (a month counts once the day of the month and the
/// time of day are reached: January 31 to February 28 is 0 months).
pub(crate) fn diff(a: Temporal, b: Temporal, unit: DT, zone: Option<&Zone>) -> TemporalResult<i64> {
    let kind = same_kind(a, b, "the difference")?;
    let operation = format!("the difference in the unit {unit}");
    if !unit.is_unit() {
        return Err(TemporalError::unsupported(operation, kind));
    }
    if let Some(len) = unit.fixed_micros() {
        let elapsed: i128 = match (a, b) {
            (Temporal::DateTime(a), Temporal::DateTime(b)) => {
                a.epoch_micros() as i128 - b.epoch_micros() as i128
            }
            (Temporal::LocalDateTime(a), Temporal::LocalDateTime(b)) => {
                no_zone(kind, zone)?;
                a.micros as i128 - b.micros as i128
            }
            (Temporal::Time(a), Temporal::Time(b)) => {
                no_zone(kind, zone)?;
                a.micros as i128 - b.micros as i128
            }
            _ => return Err(TemporalError::unsupported(operation, kind)),
        };
        return i64::try_from(elapsed / len as i128).map_err(|_| out_of_range());
    }
    let (la, lb) = local_pair(a, b, zone, &operation)?;
    if let Some(k) = unit.calendar_days() {
        return Ok(days_between(lb, la) / k);
    }
    match unit.calendar_months() {
        Some(k) => Ok(months_between(lb, la) / k),
        None => Err(TemporalError::unsupported(operation, kind)),
    }
}

/// `a - b` as PostgreSQL `age` computes it: a symbolic difference of the fields (years,
/// months, days, time of day), every part with the same sign. The fields are subtracted, then a
/// negative time borrows a day and a negative day count borrows the length of the EARLIER
/// value's month, so `age('2001-04-10', '1957-06-13')` is `43 years 9 mons 27 days` and
/// `age(b, a)` is always the negation of `age(a, b)`.
pub(crate) fn age(a: Temporal, b: Temporal, zone: Option<&Zone>) -> TemporalResult<Duration> {
    let kind = same_kind(a, b, "the age")?;
    if let (Temporal::Time(a), Temporal::Time(b)) = (a, b) {
        no_zone(kind, zone)?;
        return Ok(Duration::from_micros(a.micros - b.micros));
    }
    let (la, lb) = local_pair(a, b, zone, "the age")?;
    let negative = la < lb;
    // The earlier of the two, whose month lengths the borrowing uses.
    let earlier = if negative { la } else { lb };
    let (ya, ma, da) = civil::civil_from_days(la.date().days);
    let (yb, mb, db) = civil::civil_from_days(lb.date().days);
    let (ye, me, _) = civil::civil_from_days(earlier.date().days);
    let sign = if negative { -1 } else { 1 };
    let mut time = (la.time().micros - lb.time().micros) * sign;
    let mut days = (da as i64 - db as i64) * sign;
    let mut months = (ma as i64 - mb as i64) * sign;
    let mut years = (ya as i64 - yb as i64) * sign;
    if time < 0 {
        time += MICROS_PER_DAY;
        days -= 1;
    }
    while days < 0 {
        days += civil::days_in_month(ye, me) as i64;
        months -= 1;
    }
    while months < 0 {
        months += 12;
        years -= 1;
    }
    Ok(Duration::new(
        to_i32((years * 12 + months) * sign)?,
        to_i32(days * sign)?,
        time * sign,
    ))
}

/// The first year of the decade of `year`.
fn decade_start(year: i32) -> i32 {
    year.div_euclid(10) * 10
}

/// The first year of the century of `year` (centuries start at year 1, as PostgreSQL's: 2001
/// starts the 21st century).
fn century_start(year: i32) -> i32 {
    (year - 1).div_euclid(100) * 100 + 1
}

/// The first year of the millennium of `year` (2001 starts the 3rd millennium).
fn millennium_start(year: i32) -> i32 {
    (year - 1).div_euclid(1000) * 1000 + 1
}

/// A local date-time truncated to a unit (PostgreSQL `date_trunc`; `week` starts on Monday).
fn trunc_local(local: LocalDateTime, unit: DT) -> TemporalResult<LocalDateTime> {
    if let Some(len) = unit.fixed_micros() {
        let micros = local.micros.div_euclid(len) * len;
        return LocalDateTime::from_micros(micros).map_err(|_| out_of_range());
    }
    let days = local.date().days;
    let (year, month, _) = civil::civil_from_days(days);
    let date = match unit {
        DT::Day => Date { days },
        DT::Week => Date {
            days: shift_days(days, -(civil::iso_day_of_week(days) as i64 - 1))?,
        },
        DT::Month => Date::from_ymd(year, month, 1)?,
        DT::Quarter => Date::from_ymd(year, (month - 1) / 3 * 3 + 1, 1)?,
        DT::Year => Date::from_ymd(year, 1, 1)?,
        DT::Decade => Date::from_ymd(decade_start(year), 1, 1).map_err(|_| out_of_range())?,
        DT::Century => Date::from_ymd(century_start(year), 1, 1).map_err(|_| out_of_range())?,
        DT::Millennium => {
            Date::from_ymd(millennium_start(year), 1, 1).map_err(|_| out_of_range())?
        }
        // Units below a day returned above; parts are refused by the callers.
        DT::Microsecond
        | DT::Millisecond
        | DT::Second
        | DT::Minute
        | DT::Hour
        | DT::DayOfWeek
        | DT::IsoDayOfWeek
        | DT::DayOfYear
        | DT::IsoYear
        | DT::Epoch
        | DT::Offset => return Err(out_of_range()),
    };
    Ok(LocalDateTime::new(date, Time { micros: 0 }))
}

/// Truncates a value to a unit (PostgreSQL `date_trunc`): the fields below the unit become
/// their first value. A `datetime` is truncated on its wall clock (in the zone when one is
/// given, then resolved in it). A `duration` keeps the fields at and above the unit (`week` is
/// not defined for it, as in PostgreSQL).
pub(crate) fn trunc(value: Temporal, unit: DT, zone: Option<&Zone>) -> TemporalResult<Temporal> {
    let kind = value.kind();
    let operation = || format!("truncating to the unit {unit}");
    if !unit.is_unit() {
        return Err(TemporalError::unsupported(operation(), kind));
    }
    match value {
        Temporal::DateTime(v) => {
            let local = trunc_local(local_of(v, zone)?, unit)?;
            Ok(Temporal::DateTime(
                from_local_of(local, v, zone).map_err(|_| out_of_range())?,
            ))
        }
        Temporal::LocalDateTime(v) => {
            no_zone(kind, zone)?;
            Ok(Temporal::LocalDateTime(trunc_local(v, unit)?))
        }
        Temporal::Date(v) => {
            no_zone(kind, zone)?;
            if unit.fixed_micros().is_some() {
                return Err(TemporalError::unsupported(operation(), kind));
            }
            Ok(Temporal::Date(
                trunc_local(LocalDateTime::new(v, Time { micros: 0 }), unit)?.date(),
            ))
        }
        Temporal::Time(v) => {
            no_zone(kind, zone)?;
            let Some(len) = unit.fixed_micros() else {
                return Err(TemporalError::unsupported(operation(), kind));
            };
            Ok(Temporal::Time(Time {
                micros: v.micros / len * len,
            }))
        }
        Temporal::Duration(v) => {
            no_zone(kind, zone)?;
            let truncated = if let Some(len) = unit.fixed_micros() {
                Duration::new(v.months, v.days, v.micros / len * len)
            } else if unit == DT::Day {
                Duration::new(v.months, v.days, 0)
            } else if let Some(k) = unit.calendar_months() {
                let k = k as i32;
                Duration::new(v.months / k * k, 0, 0)
            } else {
                return Err(TemporalError::unsupported(operation(), kind));
            };
            Ok(Temporal::Duration(truncated))
        }
    }
}

/// The century of a year as PostgreSQL counts it (1 BC is year 0 here, century -1 there).
fn century_of(year: i32) -> i64 {
    let year = year as i64;
    if year > 0 {
        (year + 99) / 100
    } else {
        -((100 - year) / 100)
    }
}

/// The millennium of a year as PostgreSQL counts it.
fn millennium_of(year: i32) -> i64 {
    let year = year as i64;
    if year > 0 {
        (year + 999) / 1000
    } else {
        -((1000 - year) / 1000)
    }
}

/// A part of a wall-clock date-time (every unit and part except `offset`).
fn local_part(local: LocalDateTime, unit: DT) -> Option<Number> {
    let days = local.date().days;
    let (year, month, day) = civil::civil_from_days(days);
    let of_day = local.time().micros;
    let int = |v: i64| Some(Number::from_i64(v));
    match unit {
        DT::Microsecond => int(of_day % MICROS_PER_SECOND),
        DT::Millisecond => int(of_day % MICROS_PER_SECOND / 1000),
        DT::Second => int(of_day / MICROS_PER_SECOND % 60),
        DT::Minute => int(of_day / (60 * MICROS_PER_SECOND) % 60),
        DT::Hour => int(of_day / (3600 * MICROS_PER_SECOND)),
        DT::Day => int(day as i64),
        DT::Week => int(civil::iso_week(days).1 as i64),
        DT::Month => int(month as i64),
        DT::Quarter => int((month as i64 - 1) / 3 + 1),
        DT::Year => int(year as i64),
        DT::Decade => int((year as i64).div_euclid(10)),
        DT::Century => int(century_of(year)),
        DT::Millennium => int(millennium_of(year)),
        DT::DayOfWeek => int(civil::day_of_week(days) as i64),
        DT::IsoDayOfWeek => int(civil::iso_day_of_week(days) as i64),
        DT::DayOfYear => int(civil::day_of_year(days) as i64),
        DT::IsoYear => int(civil::iso_week(days).0 as i64),
        DT::Epoch => Some(Number::from_f64(
            local.micros as f64 / MICROS_PER_SECOND as f64,
        )),
        DT::Offset => None,
    }
}

/// A part of a value (PostgreSQL `extract`), except that `second`, `millisecond` and
/// `microsecond` are the plain fields (0-59, 0-999, 0-999999), not PostgreSQL's seconds with
/// their fraction scaled. `epoch` is a float of seconds; `offset` (a `datetime` only) the offset
/// in seconds (in the zone when one is given). Parts of a `duration` are those of PostgreSQL's
/// `interval` (`year` = months / 12, `month` = months % 12, `day` = days, `hour` and below from
/// the microseconds, each with the sign of its field; `epoch` counts a year as 365.25 days and a
/// month as 30).
pub(crate) fn part(value: Temporal, unit: DT, zone: Option<&Zone>) -> TemporalResult<Number> {
    let kind = value.kind();
    let unsupported = || TemporalError::unsupported(format!("the part {unit}"), kind);
    match value {
        Temporal::DateTime(v) => {
            if unit == DT::Epoch {
                return Ok(Number::from_f64(
                    v.epoch_micros() as f64 / MICROS_PER_SECOND as f64,
                ));
            }
            if unit == DT::Offset {
                let offset = match zone {
                    Some(zone) => zone.offset_at(v)?,
                    None => v.offset_seconds(),
                };
                return Ok(Number::from_i64(offset as i64));
            }
            local_part(local_of(v, zone)?, unit).ok_or_else(unsupported)
        }
        Temporal::LocalDateTime(v) => {
            no_zone(kind, zone)?;
            local_part(v, unit).ok_or_else(unsupported)
        }
        Temporal::Date(v) => {
            no_zone(kind, zone)?;
            if unit.fixed_micros().is_some() {
                return Err(unsupported());
            }
            local_part(LocalDateTime::new(v, Time { micros: 0 }), unit).ok_or_else(unsupported)
        }
        Temporal::Time(v) => {
            no_zone(kind, zone)?;
            if unit.fixed_micros().is_none() && unit != DT::Epoch {
                return Err(unsupported());
            }
            local_part(LocalDateTime { micros: v.micros }, unit).ok_or_else(unsupported)
        }
        Temporal::Duration(v) => {
            no_zone(kind, zone)?;
            duration_part(v, unit).ok_or_else(unsupported)
        }
    }
}

/// A part of a duration (PostgreSQL `extract` from an `interval`); `None` when undefined.
fn duration_part(v: Duration, unit: DT) -> Option<Number> {
    let months = v.months as i64;
    let years = months / 12;
    let int = |x: i64| Some(Number::from_i64(x));
    match unit {
        DT::Microsecond => int(v.micros % MICROS_PER_SECOND),
        DT::Millisecond => int(v.micros % MICROS_PER_SECOND / 1000),
        DT::Second => int(v.micros / MICROS_PER_SECOND % 60),
        DT::Minute => int(v.micros / (60 * MICROS_PER_SECOND) % 60),
        DT::Hour => int(v.micros / (3600 * MICROS_PER_SECOND)),
        DT::Day => int(v.days as i64),
        DT::Month => int(months % 12),
        DT::Quarter => int(months % 12 / 3 + 1),
        DT::Year => int(years),
        DT::Decade => int(years / 10),
        DT::Century => int(years / 100),
        DT::Millennium => int(years / 1000),
        DT::Epoch => {
            let seconds = years as f64 * 365.25 * 86_400.0
                + (months % 12) as f64 * 30.0 * 86_400.0
                + v.days as f64 * 86_400.0
                + v.micros as f64 / MICROS_PER_SECOND as f64;
            Some(Number::from_f64(seconds))
        }
        DT::Week | DT::DayOfWeek | DT::IsoDayOfWeek | DT::DayOfYear | DT::IsoYear | DT::Offset => {
            None
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Helpers of the GQL temporal functions (the semantics the openCypher TCK measures, Neo4j's;
// used by GQL only, never by the date steps)
// ---------------------------------------------------------------------------------------------

/// Nanoseconds in a second.
const NANOS_PER_SECOND: i128 = 1_000_000_000;
/// Nanoseconds in a day of 24 hours.
const NANOS_PER_DAY: i128 = 86_400 * NANOS_PER_SECOND;
/// Nanoseconds in the average Gregorian month, 30.436875 days (365.2425 / 12), which a
/// fraction of a month is worth (`duration({months: 0.75})` = `P22DT19H51M49.5S`).
const NANOS_PER_AVERAGE_MONTH: i128 = 2_629_746 * NANOS_PER_SECOND;

/// An exact decimal amount of one duration unit with up to nine fraction digits (`1.5` days,
/// `0.75` months), held as the amount times 10^9 ("nano-units"): every amount the GQL forms
/// take is exact, so the fraction cascade ([`cascade`]) needs no floating point.
#[allow(dead_code, reason = "the GQL temporal functions (3g.T4-T8) call it")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Amount(i128);

#[allow(dead_code, reason = "the GQL temporal functions (3g.T4-T8) call it")]
impl Amount {
    /// Nano-units in one unit.
    pub(crate) const SCALE: i128 = 1_000_000_000;

    /// A whole number of units.
    pub(crate) fn from_i64(n: i64) -> Amount {
        Amount(n as i128 * Self::SCALE)
    }

    /// A floating-point number of units, rounded to nine fraction digits (`0.75` stays exactly
    /// `0.75`); `None` for a non-finite value or one beyond ±2^63.
    pub(crate) fn from_f64(x: f64) -> Option<Amount> {
        if !x.is_finite() || x.abs() >= 9.2e18 {
            return None;
        }
        let whole = x.trunc();
        // `x - whole` is exact; scaling the fraction keeps its decimal digits.
        let fraction = ((x - whole) * 1e9).round();
        Some(Amount(whole as i128 * Self::SCALE + fraction as i128))
    }

    /// An amount given in nano-units (the amount times 10^9).
    pub(crate) fn from_nano_units(n: i128) -> Amount {
        Amount(n)
    }

    /// The amount times 10^9.
    pub(crate) fn nano_units(self) -> i128 {
        self.0
    }

    /// Whether the amount is a whole number.
    pub(crate) fn is_whole(self) -> bool {
        self.0 % Self::SCALE == 0
    }
}

/// The components of a duration as the constructor map and the unit text give them (each
/// possibly fractional and of either sign), summed by [`cascade`].
#[allow(dead_code, reason = "the GQL temporal functions (3g.T4-T8) call it")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct DurationParts {
    /// Years (12 months each).
    pub(crate) years: Amount,
    /// Months.
    pub(crate) months: Amount,
    /// Weeks (7 days each).
    pub(crate) weeks: Amount,
    /// Days.
    pub(crate) days: Amount,
    /// Hours.
    pub(crate) hours: Amount,
    /// Minutes.
    pub(crate) minutes: Amount,
    /// Seconds.
    pub(crate) seconds: Amount,
    /// Milliseconds.
    pub(crate) milliseconds: Amount,
    /// Microseconds.
    pub(crate) microseconds: Amount,
    /// Nanoseconds.
    pub(crate) nanoseconds: Amount,
}

/// A duration computed by [`cascade`] or [`scale_duration`], with whether non-zero digits below
/// the microsecond were cut (truncated toward zero): our durations hold microseconds, and the
/// GQL layer answers such a value with "not yet" rather than cut it silently.
#[allow(dead_code, reason = "the GQL temporal functions (3g.T4-T8) call it")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Cascaded {
    /// The duration (microseconds truncated toward zero).
    pub(crate) duration: Duration,
    /// Whether a non-zero part below the microsecond was cut.
    pub(crate) sub_micro: bool,
}

/// `a * b` on `i128`, the range error on overflow.
fn mul(a: i128, b: i128) -> TemporalResult<i128> {
    a.checked_mul(b).ok_or_else(out_of_range)
}

/// `a + b` on `i128`, the range error on overflow.
fn plus(a: i128, b: i128) -> TemporalResult<i128> {
    a.checked_add(b).ok_or_else(out_of_range)
}

/// The fraction cascade over `months / den` months, `days / den` days and `nanos / den`
/// nanoseconds (Neo4j's `DurationValue.approximate`, exact here): the whole months stay months,
/// a fraction of a month is worth [`NANOS_PER_AVERAGE_MONTH`] and joins the days, the whole
/// days stay days, a fraction of a day joins the time part, and the time part is truncated
/// toward zero to whole nanoseconds. Seconds are never folded into days (`PT49H` stays).
fn cascade_ratio(months: i128, days: i128, nanos: i128, den: i128) -> TemporalResult<Cascaded> {
    if den == 0 {
        return Err(out_of_range());
    }
    let (months, days, nanos, den) = if den < 0 {
        (-months, -days, -nanos, -den)
    } else {
        (months, days, nanos, den)
    };
    let whole_months = months / den;
    let month_rest = months - whole_months * den;
    // Nanoseconds times `den`.
    let day_total = plus(
        mul(days, NANOS_PER_DAY)?,
        mul(month_rest, NANOS_PER_AVERAGE_MONTH)?,
    )?;
    let day_unit = mul(NANOS_PER_DAY, den)?;
    let whole_days = day_total / day_unit;
    let rest = plus(day_total - whole_days * day_unit, nanos)?;
    let total_nanos = rest / den;
    let duration = Duration::new(
        i32::try_from(whole_months).map_err(|_| out_of_range())?,
        i32::try_from(whole_days).map_err(|_| out_of_range())?,
        i64::try_from(total_nanos / 1000).map_err(|_| out_of_range())?,
    );
    Ok(Cascaded {
        duration,
        sub_micro: total_nanos % 1000 != 0,
    })
}

/// The duration of the given components (the GQL `duration({..})` map and the unit text
/// `P1.5D`), with Neo4j's fraction cascade: years and months sum to months, weeks and days to
/// days, the rest to nanoseconds; a fraction of a month becomes days and time at 30.436875
/// days a month, a fraction of a day becomes time (`P0.75M` = `P22DT19H51M49.5S`, `P1.5D` =
/// `P1DT12H`, `PT1.5M` = `PT1M30S`). A result outside the fields is
/// [`TemporalError::OutOfRange`].
#[allow(dead_code, reason = "the GQL temporal functions (3g.T4-T8) call it")]
pub(crate) fn cascade(parts: &DurationParts) -> TemporalResult<Cascaded> {
    let months = plus(mul(parts.years.0, 12)?, parts.months.0)?;
    let days = plus(mul(parts.weeks.0, 7)?, parts.days.0)?;
    // Nanoseconds times 10^9.
    let mut nanos = mul(mul(parts.hours.0, 3600)?, NANOS_PER_SECOND)?;
    nanos = plus(nanos, mul(mul(parts.minutes.0, 60)?, NANOS_PER_SECOND)?)?;
    nanos = plus(nanos, mul(parts.seconds.0, NANOS_PER_SECOND)?)?;
    nanos = plus(nanos, mul(parts.milliseconds.0, 1_000_000)?)?;
    nanos = plus(nanos, mul(parts.microseconds.0, 1000)?)?;
    nanos = plus(nanos, parts.nanoseconds.0)?;
    cascade_ratio(months, days, nanos, Amount::SCALE)
}

/// `duration * factor` (`divide` false) or `duration / factor` (`divide` true), Neo4j's
/// `DurationValue.mul` / `div`: each field scaled, then the fraction cascade of [`cascade`]
/// (`P1M * 0.5` = `P15DT5H14M33S`; a whole factor scales exactly). A zero divisor or a result
/// outside the fields is [`TemporalError::OutOfRange`] (the caller reports a zero divisor
/// first).
#[allow(dead_code, reason = "the GQL temporal functions (3g.T4-T8) call it")]
pub(crate) fn scale_duration(
    duration: Duration,
    factor: Amount,
    divide: bool,
) -> TemporalResult<Cascaded> {
    let (num, den) = if divide {
        (Amount::SCALE, factor.0)
    } else {
        (factor.0, Amount::SCALE)
    };
    cascade_ratio(
        mul(duration.months as i128, num)?,
        mul(duration.days as i128, num)?,
        mul(mul(duration.micros as i128, 1000)?, num)?,
        den,
    )
}

/// `duration.between(from, to)` as Neo4j computes it (`java.time` `until`): the whole months
/// from `from` to `to` (a month counts once the day of the month and the time of day are
/// reached, [`months_between`]), then the whole days from `from` plus those months, then the
/// rest of the time. The two values are of one type (the GQL layer aligns mixed operands
/// first):
///
/// - two `date`s: months and days;
/// - two `localdatetime`s: months, days and the time;
/// - two `datetime`s: as two wall-clock values after `to` is moved to `from`'s offset (so the
///   time part is the elapsed time);
/// - two `time`s: the time between them within the day (negative when `to` is earlier).
///
/// Unlike [`age`] (PostgreSQL), every part has the sign of the direction and the borrowing
/// follows `java.time`: `2018-01-01T10:00:00.2` to `2018-01-02T10:00:00.1` is
/// `PT23H59M59.9S`.
#[allow(dead_code, reason = "the GQL temporal functions (3g.T4-T8) call it")]
pub(crate) fn between_neo4j(from: Temporal, to: Temporal) -> TemporalResult<Duration> {
    let kind = same_kind(from, to, "duration.between")?;
    let (from, to) = match (from, to) {
        (Temporal::Time(a), Temporal::Time(b)) => {
            return Ok(Duration::from_micros(b.micros - a.micros));
        }
        (Temporal::Date(a), Temporal::Date(b)) => (
            LocalDateTime::new(a, Time { micros: 0 }),
            LocalDateTime::new(b, Time { micros: 0 }),
        ),
        (Temporal::LocalDateTime(a), Temporal::LocalDateTime(b)) => (a, b),
        (Temporal::DateTime(a), Temporal::DateTime(b)) => {
            (a.local(), b.with_offset(a.offset_seconds())?.local())
        }
        _ => return Err(TemporalError::unsupported("duration.between", kind)),
    };
    let months = months_between(from, to);
    let after_months = local_add_months(from, months)?;
    let days = days_between(after_months, to);
    let after_days = local_add_days(after_months, days)?;
    Ok(Duration::new(
        to_i32(months)?,
        to_i32(days)?,
        to.micros() - after_days.micros(),
    ))
}
