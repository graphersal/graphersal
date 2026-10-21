//! Differences (3g.T8): `duration.between(from, to)` and `duration.inMonths` / `inDays` /
//! `inSeconds(from, to)` with the openCypher (Neo4j) meanings:
//!
//! - **`duration.between`** is Neo4j's algorithm (`arith::between_neo4j`): the whole months from
//!   `from` to `to` (a month counts once the day of the month and the time of day are reached),
//!   then the whole days after those months, then the rest of the time; every part carries the
//!   sign of the direction (`2015-07-21T21:40:32.142` to `2015-06-24` is
//!   `P-27DT-21H-40M-32.142S`). Rhai `age` keeps PostgreSQL's borrowing rule.
//! - **`inMonths`** gives the whole months only, **`inDays`** the whole days only (both
//!   truncated toward zero, as `java.time` `ChronoUnit.between`), **`inSeconds`** the elapsed
//!   time as a duration of seconds only (no months, no days).
//! - **Mixed operands** are aligned first, as Neo4j's `TemporalValue.until` does (U15), in this
//!   order: when only one side has a time of day, the other gets midnight (for `inSeconds`
//!   both sides always have one, so a `date` gets midnight); when only one side has a date, the
//!   other gets that date (a `localtime` beside a `date` is read on that day, so the months and
//!   days are 0 and only the times count: `date('1984-10-11')` to `localtime('16:30')` is
//!   `PT16H30M`); when only one side has an offset, the other is read at that offset. A pair of
//!   `datetime`s is compared at `from`'s offset for the months and days (`java.time`
//!   `withOffsetSameInstant`), so the time part is the elapsed time.
//! - Zone names are resolved to their offset when the value is built (U3): an operand without an
//!   offset is read at the other side's fixed offset, not in its named zone, so a difference that
//!   crosses a daylight saving change differs from Neo4j's (those TCK cases are excluded as a
//!   deviation in 3g.T9).
//! - `inMonths` / `inDays` of two `localtime`s is 22G03 (neither has a calendar, as Neo4j's
//!   unsupported unit); a duration or a non-temporal operand is 22G03; a result outside the
//!   supported range 22008. `NULL` in either argument gives `NULL` (F2).

use super::super::super::eval::{Env, EvalError, Value, status};
use super::super::super::lexer::Span;
use super::super::super::parser::{Between, TemporalFunction};
use super::super::{Arg, any_null};
use super::{cypher_name, kind_name};
use crate::ElementProperty;
use crate::value::temporal::arith;
use crate::value::temporal::{
    Date, DateTime, Duration, LocalDateTime, Temporal, TemporalError, TemporalResult, Time,
};

/// GQLSTATUS 22008, datetime field overflow (a result out of range).
/// `status::DATETIME_OVERFLOW` once 3g.T4 has added it.
const DATETIME_OVERFLOW: &str = "22008";

/// `duration.between(a, b)` or one of its `in*` forms (`form`).
pub(crate) fn between<En: Env>(
    form: Between,
    args: Vec<Arg<En::Element>>,
    _env: &En,
    span: Span,
) -> Result<Value<En::Element>, EvalError> {
    if any_null(&args) {
        return Ok(Value::Null);
    }
    let name = cypher_name(TemporalFunction::Between(form));
    let mut args = args.into_iter();
    let (Some(from), Some(to)) = (args.next(), args.next()) else {
        return Err(EvalError::new(
            status::SEMANTIC,
            format!("{name}() takes two temporal values"),
            span,
        ));
    };
    let from_value = instant(name, &from)?;
    let to_value = instant(name, &to)?;
    let duration = difference(form, from_value, to_value)
        .map_err(|error| engine_error(name, error, span))?
        .ok_or_else(|| {
            EvalError::invalid_type(
                format!(
                    "{name}() needs a date on at least one side: two local times have no {}",
                    match form {
                        Between::InDays => "days",
                        Between::Full | Between::InMonths | Between::InSeconds => "months",
                    }
                ),
                span,
            )
        })?;
    Ok(Value::Scalar(ElementProperty::Temporal(
        Temporal::Duration(duration),
    )))
}

/// The temporal instant (not a duration) of `arg`, else 22G03.
fn instant<E>(name: &str, arg: &Arg<E>) -> Result<Temporal, EvalError> {
    match &arg.value {
        Value::Scalar(ElementProperty::Temporal(temporal)) => match temporal {
            Temporal::Date(_)
            | Temporal::LocalDateTime(_)
            | Temporal::DateTime(_)
            | Temporal::Time(_) => Ok(*temporal),
            Temporal::Duration(_) => Err(EvalError::invalid_type(
                format!(
                    "{name}() takes a date, a localtime, a localdatetime or a datetime, not {}",
                    kind_name(temporal)
                ),
                arg.span,
            )),
        },
        other => Err(EvalError::invalid_type(
            format!(
                "{name}() takes a date, a localtime, a localdatetime or a datetime, not {}",
                other.kind_name()
            ),
            arg.span,
        )),
    }
}

/// An engine error at `span`: out of range 22008, anything else (an unexpected pair) 22G03.
fn engine_error(name: &str, error: TemporalError, span: Span) -> EvalError {
    match &error {
        TemporalError::OutOfRange { .. } => {
            EvalError::new(DATETIME_OVERFLOW, format!("{name}(): {error}"), span)
        }
        TemporalError::Parse { .. }
        | TemporalError::UnknownZone { .. }
        | TemporalError::Unsupported { .. } => {
            EvalError::invalid_type(format!("{name}(): {error}"), span)
        }
    }
}

/// The difference `form` from `from` to `to` (two instants); `None` for the months or days of
/// two times of day.
fn difference(form: Between, from: Temporal, to: Temporal) -> TemporalResult<Option<Duration>> {
    let (from, to) = align(from, to, form == Between::InSeconds)?;
    match form {
        Between::Full => arith::between_neo4j(from, to).map(Some),
        Between::InSeconds => {
            elapsed_micros(from, to).map(|micros| Some(Duration::from_micros(micros)))
        }
        Between::InMonths | Between::InDays => {
            let Some((from, to)) = calendar_pair(from, to)? else {
                return Ok(None);
            };
            let count = if form == Between::InMonths {
                arith::months_between(from, to)
            } else {
                arith::days_between(from, to)
            };
            let count = i32::try_from(count).map_err(|_| out_of_range())?;
            Ok(Some(if form == Between::InMonths {
                Duration::new(count, 0, 0)
            } else {
                Duration::new(0, count, 0)
            }))
        }
    }
}

fn out_of_range() -> TemporalError {
    TemporalError::out_of_range("the difference")
}

/// Whether a value has a time of day.
fn has_time(value: Temporal) -> bool {
    match value {
        Temporal::Time(_) | Temporal::LocalDateTime(_) | Temporal::DateTime(_) => true,
        Temporal::Date(_) | Temporal::Duration(_) => false,
    }
}

/// The UTC offset of a value with one.
fn offset_of(value: Temporal) -> Option<i32> {
    match value {
        Temporal::DateTime(v) => Some(v.offset_seconds()),
        Temporal::Date(_)
        | Temporal::LocalDateTime(_)
        | Temporal::Time(_)
        | Temporal::Duration(_) => None,
    }
}

/// The wall-clock date of a value with one (a `datetime` at its own offset).
fn date_of(value: Temporal) -> Option<Date> {
    match value {
        Temporal::Date(v) => Some(v),
        Temporal::LocalDateTime(v) => Some(v.date()),
        Temporal::DateTime(v) => Some(v.local().date()),
        Temporal::Time(_) | Temporal::Duration(_) => None,
    }
}

/// `value` with midnight added (a `date` becomes a `localdatetime`).
fn attach_time(value: Temporal) -> TemporalResult<Temporal> {
    match value {
        Temporal::Date(v) => Ok(Temporal::LocalDateTime(LocalDateTime::new(v, midnight()?))),
        other => Ok(other),
    }
}

/// `00:00` (never fails: 0 is a time of day).
fn midnight() -> TemporalResult<Time> {
    Time::from_micros_of_day(0)
}

/// `value` read on `date` (a `localtime` becomes a `localdatetime`).
fn attach_date(value: Temporal, date: Date) -> Temporal {
    match value {
        Temporal::Time(v) => Temporal::LocalDateTime(LocalDateTime::new(date, v)),
        other => other,
    }
}

/// `value` read at `offset` (a `localdatetime` becomes a `datetime`; a `date` or a `localtime`
/// stays: alignment gave it a time and a date before when the other side has both).
fn attach_offset(value: Temporal, offset: i32) -> TemporalResult<Temporal> {
    match value {
        Temporal::LocalDateTime(v) => Ok(Temporal::DateTime(DateTime::from_local(v, offset)?)),
        other => Ok(other),
    }
}

/// Neo4j's operand alignment (`TemporalValue.until`): a time of day (both when `time_based`,
/// else when only one side has one), then a date, then an offset, each lent by the side that
/// has it.
fn align(from: Temporal, to: Temporal, time_based: bool) -> TemporalResult<(Temporal, Temporal)> {
    let (mut from, mut to) = (from, to);
    if time_based {
        from = attach_time(from)?;
        to = attach_time(to)?;
    } else if has_time(from) && !has_time(to) {
        to = attach_time(to)?;
    } else if has_time(to) && !has_time(from) {
        from = attach_time(from)?;
    }
    match (date_of(from), date_of(to)) {
        (Some(date), None) => to = attach_date(to, date),
        (None, Some(date)) => from = attach_date(from, date),
        (Some(_), Some(_)) | (None, None) => {}
    }
    match (offset_of(from), offset_of(to)) {
        (Some(offset), None) => to = attach_offset(to, offset)?,
        (None, Some(offset)) => from = attach_offset(from, offset)?,
        (Some(_), Some(_)) | (None, None) => {}
    }
    Ok((from, to))
}

/// The elapsed microseconds from `from` to `to` (aligned, so of one type with a time of day).
fn elapsed_micros(from: Temporal, to: Temporal) -> TemporalResult<i64> {
    let micros = |value: Temporal| -> TemporalResult<i64> {
        match value {
            Temporal::DateTime(v) => Ok(v.epoch_micros()),
            Temporal::LocalDateTime(v) => Ok(v.micros()),
            Temporal::Time(v) => Ok(v.micros_of_day()),
            Temporal::Date(_) | Temporal::Duration(_) => Err(TemporalError::unsupported(
                "duration.inSeconds",
                value.kind(),
            )),
        }
    };
    match (from, to) {
        (Temporal::DateTime(_), Temporal::DateTime(_))
        | (Temporal::LocalDateTime(_), Temporal::LocalDateTime(_))
        | (Temporal::Time(_), Temporal::Time(_)) => micros(to)?
            .checked_sub(micros(from)?)
            .ok_or_else(out_of_range),
        _ => Err(TemporalError::unsupported(
            "duration.inSeconds",
            from.kind(),
        )),
    }
}

/// The wall-clock pair for whole months or days (aligned values): two `datetime`s at `from`'s
/// offset, a `date` at midnight; `None` for two times of day.
fn calendar_pair(
    from: Temporal,
    to: Temporal,
) -> TemporalResult<Option<(LocalDateTime, LocalDateTime)>> {
    match (from, to) {
        (Temporal::Date(a), Temporal::Date(b)) => Ok(Some((
            LocalDateTime::new(a, midnight()?),
            LocalDateTime::new(b, midnight()?),
        ))),
        (Temporal::LocalDateTime(a), Temporal::LocalDateTime(b)) => Ok(Some((a, b))),
        (Temporal::DateTime(a), Temporal::DateTime(b)) => Ok(Some((
            a.local(),
            b.with_offset(a.offset_seconds())?.local(),
        ))),
        (Temporal::Time(_), Temporal::Time(_)) => Ok(None),
        _ => Err(TemporalError::unsupported("duration.between", from.kind())),
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::super::parser::Function;
    use super::super::super::call;
    use super::super::super::test_env::{El, TestRow, int, null, string as text};
    use super::*;
    use crate::value::temporal::TemporalKind;

    const SPAN: Span = Span {
        start: 0,
        end: 0,
        line: 1,
        column: 1,
    };

    /// A value from its kind prefix and ISO text (`d:`, `t:`, `l:`, `z:`).
    fn parse(text: &str) -> Temporal {
        let (kind, rest) = text.split_at(2);
        let kind = match kind {
            "d:" => TemporalKind::Date,
            "t:" => TemporalKind::Time,
            "l:" => TemporalKind::LocalDateTime,
            "z:" => TemporalKind::DateTime,
            other => panic!("kind {other}"),
        };
        Temporal::parse_as(kind, rest).unwrap_or_else(|e| panic!("{text}: {e}"))
    }

    fn run(form: Between, from: Value<El>, to: Value<El>) -> Result<Value<El>, EvalError> {
        call(
            Function::Temporal(TemporalFunction::Between(form)),
            vec![
                Arg {
                    value: from,
                    span: SPAN,
                },
                Arg {
                    value: to,
                    span: SPAN,
                },
            ],
            &TestRow::default(),
            SPAN,
        )
    }

    fn temporal(value: Temporal) -> Value<El> {
        Value::Scalar(ElementProperty::Temporal(value))
    }

    fn ok(form: Between, from: &str, to: &str) -> Duration {
        match run(form, temporal(parse(from)), temporal(parse(to))) {
            Ok(Value::Scalar(ElementProperty::Temporal(Temporal::Duration(d)))) => d,
            other => panic!("{form:?}({from}, {to}): {other:?}"),
        }
    }

    fn status_of(form: Between, from: Value<El>, to: Value<El>) -> &'static str {
        match run(form, from, to) {
            Err(error) => error.gqlstatus,
            Ok(value) => panic!("{form:?}: {value:?}"),
        }
    }

    /// Microseconds of `h` hours, `m` minutes and `ms` milliseconds.
    fn hms(h: i64, m: i64, ms: i64) -> i64 {
        ((h * 60 + m) * 60_000 + ms) * 1000
    }

    /// The left and right operands of the TCK's Temporal10 [2]-[5] (without Cypher's zoned
    /// `time()`, U2).
    const LEFT: [&str; 4] = [
        "d:1984-10-11",
        "t:14:30",
        "l:2015-07-21T21:40:32.142",
        "z:2014-07-21T21:40:36.143+02:00",
    ];
    const RIGHT: [&str; 4] = [
        "d:2015-06-24",
        "l:2016-07-21T21:45:22.142",
        "z:2015-07-21T21:40:32.142+01:00",
        "t:16:30",
    ];

    #[test]
    fn between_follows_the_tck_grid() {
        let expected: [[(i32, i32, i64); 4]; 4] = [
            [
                (368, 13, 0),
                (381, 10, hms(21, 45, 22_142)),
                (369, 10, hms(21, 40, 32_142)),
                (0, 0, hms(16, 30, 0)),
            ],
            [
                (0, 0, -hms(14, 30, 0)),
                (0, 0, hms(7, 15, 22_142)),
                (0, 0, hms(7, 10, 32_142)),
                (0, 0, hms(2, 0, 0)),
            ],
            [
                (0, -27, -hms(21, 40, 32_142)),
                (12, 0, hms(0, 4, 50_000)),
                (0, 0, 0),
                (0, 0, -hms(5, 10, 32_142)),
            ],
            [
                (11, 2, hms(2, 19, 23_857)),
                (24, 0, hms(0, 4, 45_999)),
                (12, 0, hms(0, 59, 55_999)),
                (0, 0, -hms(5, 10, 36_143)),
            ],
        ];
        for (l, row) in expected.iter().enumerate() {
            for (r, (months, days, micros)) in row.iter().enumerate() {
                assert_eq!(
                    ok(Between::Full, LEFT[l], RIGHT[r]),
                    Duration::new(*months, *days, *micros),
                    "between({}, {})",
                    LEFT[l],
                    RIGHT[r]
                );
            }
        }
    }

    #[test]
    fn in_months_and_in_days_follow_the_tck_grid() {
        // `None`: two local times have no calendar.
        let months: [[Option<i32>; 4]; 4] = [
            [Some(368), Some(381), Some(369), Some(0)],
            [Some(0), Some(0), Some(0), None],
            [Some(0), Some(12), Some(0), Some(0)],
            [Some(11), Some(24), Some(12), Some(0)],
        ];
        let days: [[Option<i32>; 4]; 4] = [
            [Some(11213), Some(11606), Some(11240), Some(0)],
            [Some(0), Some(0), Some(0), None],
            [Some(-27), Some(366), Some(0), Some(0)],
            [Some(337), Some(731), Some(365), Some(0)],
        ];
        for l in 0..4 {
            for r in 0..4 {
                let (from, to) = (LEFT[l], RIGHT[r]);
                for (form, table) in [(Between::InMonths, &months), (Between::InDays, &days)] {
                    match table[l][r] {
                        Some(n) => {
                            let expected = if form == Between::InMonths {
                                Duration::new(n, 0, 0)
                            } else {
                                Duration::new(0, n, 0)
                            };
                            assert_eq!(ok(form, from, to), expected, "{form:?}({from}, {to})");
                        }
                        None => assert_eq!(
                            status_of(form, temporal(parse(from)), temporal(parse(to))),
                            status::INVALID_VALUE_TYPE,
                            "{form:?}({from}, {to})"
                        ),
                    }
                }
            }
        }
    }

    #[test]
    fn in_seconds_follows_the_tck_grid() {
        let expected: [[i64; 4]; 4] = [
            [
                hms(269_112, 0, 0),
                hms(278_565, 45, 22_142),
                hms(269_781, 40, 32_142),
                hms(16, 30, 0),
            ],
            [
                -hms(14, 30, 0),
                hms(7, 15, 22_142),
                hms(7, 10, 32_142),
                hms(2, 0, 0),
            ],
            [
                -hms(669, 40, 32_142),
                hms(8784, 4, 50_000),
                0,
                -hms(5, 10, 32_142),
            ],
            [
                hms(8090, 19, 23_857),
                hms(17_544, 4, 45_999),
                hms(8760, 59, 55_999),
                -hms(5, 10, 36_143),
            ],
        ];
        for (l, row) in expected.iter().enumerate() {
            for (r, micros) in row.iter().enumerate() {
                assert_eq!(
                    ok(Between::InSeconds, LEFT[l], RIGHT[r]),
                    Duration::from_micros(*micros),
                    "inSeconds({}, {})",
                    LEFT[l],
                    RIGHT[r]
                );
            }
        }
    }

    #[test]
    fn signs_and_fractions_follow_the_tck() {
        // Temporal10 [6], [11]: the sub-second part keeps the sign of the whole difference.
        for (from, to, micros) in [
            (
                "l:2014-07-21T21:40:36.143",
                "l:2014-07-21T21:40:36.142",
                -1_000,
            ),
            ("t:12:34:54.7", "t:12:34:54.3", -400_000),
            ("t:12:34:54.7", "t:12:44:55.3", hms(0, 10, 600)),
            ("t:12:44:54.7", "t:12:34:55.3", -hms(0, 9, 59_400)),
            ("t:12:44:56", "t:12:34:55.7", -hms(0, 10, 300)),
            ("t:12:34:56.3", "t:12:34:54.7", -1_600_000),
        ] {
            assert_eq!(
                ok(Between::InSeconds, from, to),
                Duration::from_micros(micros),
                "{from} {to}"
            );
        }
        // Temporal10 [7]: negative months, mixed operands, two offsets.
        for (from, to, months) in [
            ("d:2018-03-11", "d:2016-06-24", -20),
            ("d:2018-07-21", "z:2016-07-21T21:40:32.142+01:00", -23),
            ("l:2018-07-21T21:40:32.142", "d:2016-07-21", -24),
            (
                "z:2018-07-21T21:40:36.143+02:00",
                "l:2016-07-21T21:40:36.143",
                -24,
            ),
            (
                "z:2018-07-21T21:40:36.143+05:00",
                "z:1984-07-21T22:40:36.143+02:00",
                -407,
            ),
        ] {
            assert_eq!(
                ok(Between::InMonths, from, to),
                Duration::new(months, 0, 0),
                "{from} {to}"
            );
        }
        // Temporal10 [12]: a value against itself.
        for value in [
            "d:2020-01-01",
            "t:12:00",
            "l:2020-01-01T12:00",
            "z:2020-01-01T12:00+03:00",
        ] {
            assert_eq!(ok(Between::InSeconds, value, value), Duration::default());
            assert_eq!(ok(Between::Full, value, value), Duration::default());
        }
    }

    #[test]
    fn null_and_wrong_kinds() {
        let day = temporal(parse("d:2020-01-01"));
        let span = temporal(Temporal::Duration(Duration::new(0, 1, 0)));
        for form in [
            Between::Full,
            Between::InMonths,
            Between::InDays,
            Between::InSeconds,
        ] {
            assert_eq!(run(form, null(), null()), Ok(Value::Null));
            assert_eq!(run(form, day.clone(), null()), Ok(Value::Null));
            assert_eq!(run(form, null(), int(1)), Ok(Value::Null));
            for (from, to) in [
                (day.clone(), int(1)),
                (text("2020-01-01"), day.clone()),
                (day.clone(), span.clone()),
            ] {
                assert_eq!(status_of(form, from, to), status::INVALID_VALUE_TYPE);
            }
        }
    }
}
