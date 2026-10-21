//! Component access and text (3g.T5): `x.year`, `x.week`, `x.millisecond`, `x.offset`,
//! `x.epochSeconds`, the duration totals (`dur.months`, `dur.seconds`) and remainders
//! (`dur.monthsOfYear`, `dur.nanosecondsOfSecond`) with Neo4j's names and meanings (U11, U12);
//! `toString(x)` in the Neo4j / `java.time` text form (U6; `CAST(x AS STRING)` keeps the engine
//! text, and the shared `ElementProperty::cast` is never changed).
//!
//! # Components (Neo4j, `java.time`)
//!
//! A component name matches in any ASCII case (`d.weekyear` = `d.weekYear`), as in Neo4j.
//!
//! | Of | Components |
//! |---|---|
//! | a date, a local datetime, a datetime | `year`, `quarter` (1-4), `month`, `week` and `weekYear` (ISO 8601: Monday weeks, week 1 holds January 4), `dayOfQuarter` (= `quarterDay`, 1-92), `day`, `ordinalDay` (1-366), `dayOfWeek` (= `weekDay`, 1 = Monday .. 7 = Sunday) |
//! | a local time, a local datetime, a datetime | `hour`, `minute`, `second`, `millisecond`, `microsecond`, `nanosecond`; each fraction counts from the start of the second (`.645876` gives 645, 645876, 645876000) |
//! | a datetime | `timezone` and `offset` (the offset text `Z`, `+01:00`, `-02:05:07`: a datetime keeps its offset, not a zone name, U3), `offsetMinutes`, `offsetSeconds`, `epochSeconds`, `epochMillis` (floor) |
//! | a duration | totals `years` (months / 12), `quarters` (months / 3), `months`, `weeks` (days / 7), `days`, `hours` (seconds / 3600), `minutes` (seconds / 60), `seconds`, `milliseconds`, `microseconds`, `nanoseconds` (seconds and the fraction); remainders `quartersOfYear`, `monthsOfQuarter`, `monthsOfYear`, `daysOfWeek`, `minutesOfHour`, `secondsOfMinute`, `millisecondsOfSecond`, `microsecondsOfSecond`, `nanosecondsOfSecond` |
//!
//! A duration's months, days and seconds stay apart (the seconds never fold into days); the
//! seconds and the fraction are read as Neo4j normalizes them, the seconds rounded down and the
//! fraction non-negative (`-0.1 s` is seconds -1, `millisecondsOfSecond` 900). Integer divisions
//! truncate toward zero (Java): `duration({months: -16}).years` is -1, `.monthsOfYear` -4.
//!
//! Any other name, or a name of another kind (`date.hour`), is 22G03 naming the components of
//! the value's kind; a total beyond the integer range (`nanoseconds` of a very long duration)
//! is 22003.

use super::super::super::eval::{EvalError, Value};
use super::super::super::lexer::Span;
use super::kind_name;
use crate::value::temporal::{Date, DateTime, Duration, Temporal, Time, civil, format};

/// Microseconds in one second.
const MICROS_PER_SECOND: i64 = 1_000_000;

/// A component of a temporal value (Neo4j's names, matched in any ASCII case).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    // Date components.
    Year,
    Quarter,
    Month,
    Week,
    WeekYear,
    DayOfQuarter,
    Day,
    OrdinalDay,
    DayOfWeek,
    // Time components.
    Hour,
    Minute,
    Second,
    Millisecond,
    Microsecond,
    Nanosecond,
    // Offset and instant components.
    Timezone,
    Offset,
    OffsetMinutes,
    OffsetSeconds,
    EpochSeconds,
    EpochMillis,
    // Duration totals.
    Years,
    Quarters,
    Months,
    Weeks,
    Days,
    Hours,
    Minutes,
    Seconds,
    Milliseconds,
    Microseconds,
    Nanoseconds,
    // Duration remainders.
    QuartersOfYear,
    MonthsOfQuarter,
    MonthsOfYear,
    DaysOfWeek,
    MinutesOfHour,
    SecondsOfMinute,
    MillisecondsOfSecond,
    MicrosecondsOfSecond,
    NanosecondsOfSecond,
}

/// Every component name (Neo4j's spelling) with its field; `quarterDay` and `weekDay` are
/// Neo4j's synonyms of `dayOfQuarter` and `dayOfWeek`.
const NAMES: &[(&str, Field)] = &[
    ("year", Field::Year),
    ("quarter", Field::Quarter),
    ("month", Field::Month),
    ("week", Field::Week),
    ("weekYear", Field::WeekYear),
    ("dayOfQuarter", Field::DayOfQuarter),
    ("quarterDay", Field::DayOfQuarter),
    ("day", Field::Day),
    ("ordinalDay", Field::OrdinalDay),
    ("dayOfWeek", Field::DayOfWeek),
    ("weekDay", Field::DayOfWeek),
    ("hour", Field::Hour),
    ("minute", Field::Minute),
    ("second", Field::Second),
    ("millisecond", Field::Millisecond),
    ("microsecond", Field::Microsecond),
    ("nanosecond", Field::Nanosecond),
    ("timezone", Field::Timezone),
    ("offset", Field::Offset),
    ("offsetMinutes", Field::OffsetMinutes),
    ("offsetSeconds", Field::OffsetSeconds),
    ("epochSeconds", Field::EpochSeconds),
    ("epochMillis", Field::EpochMillis),
    ("years", Field::Years),
    ("quarters", Field::Quarters),
    ("months", Field::Months),
    ("weeks", Field::Weeks),
    ("days", Field::Days),
    ("hours", Field::Hours),
    ("minutes", Field::Minutes),
    ("seconds", Field::Seconds),
    ("milliseconds", Field::Milliseconds),
    ("microseconds", Field::Microseconds),
    ("nanoseconds", Field::Nanoseconds),
    ("quartersOfYear", Field::QuartersOfYear),
    ("monthsOfQuarter", Field::MonthsOfQuarter),
    ("monthsOfYear", Field::MonthsOfYear),
    ("daysOfWeek", Field::DaysOfWeek),
    ("minutesOfHour", Field::MinutesOfHour),
    ("secondsOfMinute", Field::SecondsOfMinute),
    ("millisecondsOfSecond", Field::MillisecondsOfSecond),
    ("microsecondsOfSecond", Field::MicrosecondsOfSecond),
    ("nanosecondsOfSecond", Field::NanosecondsOfSecond),
];

/// The components of a date, for the 22G03 message.
const DATE_NAMES: &str = "year, quarter, month, week, weekYear, dayOfQuarter, day, ordinalDay, \
                          dayOfWeek";
/// The components of a local time.
const TIME_NAMES: &str = "hour, minute, second, millisecond, microsecond, nanosecond";
/// The components only a datetime has.
const ZONED_NAMES: &str = "timezone, offset, offsetMinutes, offsetSeconds, epochSeconds, \
                           epochMillis";
/// The components of a duration.
const DURATION_NAMES: &str = "years, quarters, months, weeks, days, hours, minutes, seconds, \
                              milliseconds, microseconds, nanoseconds, quartersOfYear, \
                              monthsOfQuarter, monthsOfYear, daysOfWeek, minutesOfHour, \
                              secondsOfMinute, millisecondsOfSecond, microsecondsOfSecond, \
                              nanosecondsOfSecond";

/// The value of a component: an integer or the offset text.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Component {
    Integer(i64),
    Text(String),
}

/// Why a component could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Missing {
    /// The value's kind has no such component.
    NoSuchComponent,
    /// The total does not fit an integer.
    OutOfRange,
}

impl Field {
    /// The field named `key` in any ASCII case.
    fn parse(key: &str) -> Option<Field> {
        NAMES
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(key))
            .map(|&(_, field)| field)
    }
}

/// The component `key` of `value`: an integer, or the offset text for `timezone` / `offset`.
/// An unknown name, or one of another kind, is 22G03; a total beyond the integer range 22003.
pub(crate) fn component<E>(value: &Temporal, key: &str, span: Span) -> Result<Value<E>, EvalError> {
    let read = match Field::parse(key) {
        Some(field) => read(value, field),
        None => Err(Missing::NoSuchComponent),
    };
    match read {
        Ok(Component::Integer(number)) => Ok(Value::integer(number)),
        Ok(Component::Text(text)) => Ok(Value::string(text)),
        Err(Missing::OutOfRange) => Err(EvalError::out_of_range(span)),
        Err(Missing::NoSuchComponent) => Err(EvalError::invalid_type(
            format!(
                "{} has no component {key}; its components are {}",
                kind_name(value),
                names_of(value)
            ),
            span,
        )),
    }
}

/// The component names of `value`'s kind, for the 22G03 message.
fn names_of(value: &Temporal) -> String {
    match value {
        Temporal::Date(_) => DATE_NAMES.to_owned(),
        Temporal::Time(_) => TIME_NAMES.to_owned(),
        Temporal::LocalDateTime(_) => format!("{DATE_NAMES}, {TIME_NAMES}"),
        Temporal::DateTime(_) => format!("{DATE_NAMES}, {TIME_NAMES}, {ZONED_NAMES}"),
        Temporal::Duration(_) => DURATION_NAMES.to_owned(),
    }
}

/// Reads `field` of `value`.
fn read(value: &Temporal, field: Field) -> Result<Component, Missing> {
    match *value {
        Temporal::Date(date) => date_field(date, field),
        Temporal::Time(time) => time_field(time, field),
        Temporal::LocalDateTime(local) => {
            date_field(local.date(), field).or_else(|_| time_field(local.time(), field))
        }
        Temporal::DateTime(instant) => {
            let local = instant.local();
            date_field(local.date(), field)
                .or_else(|_| time_field(local.time(), field))
                .or_else(|_| zoned_field(instant, field))
        }
        Temporal::Duration(duration) => duration_field(duration, field),
    }
}

/// A component of a calendar day.
fn date_field(date: Date, field: Field) -> Result<Component, Missing> {
    let days = date.days_since_epoch();
    let month = date.month();
    let quarter = (month - 1) / 3 + 1;
    let number = match field {
        Field::Year => i64::from(date.year()),
        Field::Quarter => i64::from(quarter),
        Field::Month => i64::from(month),
        Field::Week => i64::from(civil::iso_week(days).1),
        Field::WeekYear => i64::from(civil::iso_week(days).0),
        Field::DayOfQuarter => {
            let first = civil::days_from_civil(date.year(), (quarter - 1) * 3 + 1, 1);
            i64::from(days) - i64::from(first) + 1
        }
        Field::Day => i64::from(date.day()),
        Field::OrdinalDay => i64::from(civil::day_of_year(days)),
        Field::DayOfWeek => i64::from(civil::iso_day_of_week(days)),
        _ => return Err(Missing::NoSuchComponent),
    };
    Ok(Component::Integer(number))
}

/// A component of a time of day; the fractions count from the start of the second.
fn time_field(time: Time, field: Field) -> Result<Component, Missing> {
    let micro = i64::from(time.microsecond());
    let number = match field {
        Field::Hour => i64::from(time.hour()),
        Field::Minute => i64::from(time.minute()),
        Field::Second => i64::from(time.second()),
        Field::Millisecond => micro / 1000,
        Field::Microsecond => micro,
        Field::Nanosecond => micro * 1000,
        _ => return Err(Missing::NoSuchComponent),
    };
    Ok(Component::Integer(number))
}

/// A component only a datetime has: its offset and its instant.
fn zoned_field(instant: DateTime, field: Field) -> Result<Component, Missing> {
    let offset = instant.offset_seconds();
    let number = match field {
        // A datetime keeps its offset, not a zone name (U3): the zone is the offset text.
        Field::Timezone | Field::Offset => {
            let mut text = String::new();
            // Writing into a `String` never fails.
            let _ = format::write_offset(&mut text, offset);
            return Ok(Component::Text(text));
        }
        Field::OffsetMinutes => i64::from(offset / 60),
        Field::OffsetSeconds => i64::from(offset),
        Field::EpochSeconds => instant.epoch_micros().div_euclid(MICROS_PER_SECOND),
        Field::EpochMillis => instant.epoch_micros().div_euclid(1000),
        _ => return Err(Missing::NoSuchComponent),
    };
    Ok(Component::Integer(number))
}

/// A total or a remainder of a duration (Neo4j `DurationValue`).
fn duration_field(duration: Duration, field: Field) -> Result<Component, Missing> {
    let months = i64::from(duration.months());
    let days = i64::from(duration.days());
    // Neo4j's normalized pair: whole seconds rounded down, a non-negative fraction.
    let seconds = i128::from(duration.micros().div_euclid(MICROS_PER_SECOND));
    let micros = i128::from(duration.micros().rem_euclid(MICROS_PER_SECOND));
    let number: i128 = match field {
        Field::Years => i128::from(months / 12),
        Field::Quarters => i128::from(months / 3),
        Field::Months => i128::from(months),
        Field::Weeks => i128::from(days / 7),
        Field::Days => i128::from(days),
        Field::Hours => seconds / 3600,
        Field::Minutes => seconds / 60,
        Field::Seconds => seconds,
        Field::Milliseconds => seconds * 1000 + micros / 1000,
        Field::Microseconds => seconds * 1_000_000 + micros,
        Field::Nanoseconds => seconds * 1_000_000_000 + micros * 1000,
        Field::QuartersOfYear => i128::from(months / 3 % 4),
        Field::MonthsOfQuarter => i128::from(months % 3),
        Field::MonthsOfYear => i128::from(months % 12),
        Field::DaysOfWeek => i128::from(days % 7),
        Field::MinutesOfHour => seconds / 60 % 60,
        Field::SecondsOfMinute => seconds % 60,
        Field::MillisecondsOfSecond => micros / 1000,
        Field::MicrosecondsOfSecond => micros,
        Field::NanosecondsOfSecond => micros * 1000,
        _ => return Err(Missing::NoSuchComponent),
    };
    i64::try_from(number)
        .map(Component::Integer)
        .map_err(|_| Missing::OutOfRange)
}

/// The Neo4j text of `value` for Cypher `toString(x)` (`12:00`, `1984-10-11T12:31Z`,
/// `PT-1M-0.001S`; `format::neo4j`).
pub(crate) fn to_text(value: &Temporal) -> String {
    format::neo4j(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::temporal::LocalDateTime;

    const SPAN: Span = Span {
        start: 0,
        end: 0,
        line: 1,
        column: 1,
    };

    fn date(year: i32, month: u8, day: u8) -> Date {
        Date::from_ymd(year, month, day).expect("a date")
    }

    fn time(hour: u8, minute: u8, second: u8, micro: u32) -> Time {
        Time::from_hms_micro(hour, minute, second, micro).expect("a time")
    }

    fn int(value: &Temporal, key: &str) -> i64 {
        match read(value, Field::parse(key).expect(key)) {
            Ok(Component::Integer(number)) => number,
            other => panic!("{key}: {other:?}"),
        }
    }

    fn text(value: &Temporal, key: &str) -> String {
        match read(value, Field::parse(key).expect(key)) {
            Ok(Component::Text(text)) => text,
            other => panic!("{key}: {other:?}"),
        }
    }

    fn check(value: &Temporal, rows: &[(&str, i64)]) {
        for &(key, expected) in rows {
            assert_eq!(int(value, key), expected, "{key}");
        }
    }

    #[test]
    fn date_components_follow_temporal5() {
        // Temporal5 [1].
        let d = Temporal::Date(date(1984, 10, 11));
        check(
            &d,
            &[
                ("year", 1984),
                ("quarter", 4),
                ("month", 10),
                ("week", 41),
                ("weekYear", 1984),
                ("day", 11),
                ("ordinalDay", 285),
                ("weekDay", 4),
                ("dayOfWeek", 4),
                ("dayOfQuarter", 11),
                ("quarterDay", 11),
            ],
        );
        // Temporal5 [2]: the last ISO week of the previous week year.
        let d = Temporal::Date(date(1984, 1, 1));
        check(
            &d,
            &[
                ("year", 1984),
                ("weekYear", 1983),
                ("week", 52),
                ("weekDay", 7),
            ],
        );
        // Any ASCII case.
        assert_eq!(int(&d, "WEEKYEAR"), 1983);
        assert_eq!(int(&d, "weekyear"), 1983);
        // The last day of a leap year, a quarter of 92 days.
        let d = Temporal::Date(date(2020, 12, 31));
        check(
            &d,
            &[("ordinalDay", 366), ("dayOfQuarter", 92), ("quarter", 4)],
        );
        // A negative year.
        let d = Temporal::Date(date(-1, 3, 1));
        check(&d, &[("year", -1), ("quarter", 1), ("dayOfQuarter", 60)]);
    }

    #[test]
    fn time_components_count_from_the_start_of_the_second() {
        // Temporal5 [3] at microsecond precision.
        let t = Temporal::Time(time(12, 31, 14, 645_876));
        check(
            &t,
            &[
                ("hour", 12),
                ("minute", 31),
                ("second", 14),
                ("millisecond", 645),
                ("microsecond", 645_876),
                ("nanosecond", 645_876_000),
            ],
        );
        assert_eq!(
            read(&t, Field::Year),
            Err(Missing::NoSuchComponent),
            "a time has no year"
        );
        assert_eq!(read(&t, Field::Offset), Err(Missing::NoSuchComponent));
    }

    #[test]
    fn local_datetime_has_date_and_time_components() {
        // Temporal5 [5] at microsecond precision.
        let v = Temporal::LocalDateTime(LocalDateTime::new(
            date(1984, 11, 11),
            time(12, 31, 14, 645_876),
        ));
        check(
            &v,
            &[
                ("year", 1984),
                ("quarter", 4),
                ("month", 11),
                ("week", 45),
                ("weekYear", 1984),
                ("day", 11),
                ("ordinalDay", 316),
                ("weekDay", 7),
                ("dayOfQuarter", 42),
                ("hour", 12),
                ("minute", 31),
                ("second", 14),
                ("millisecond", 645),
                ("microsecond", 645_876),
                ("nanosecond", 645_876_000),
            ],
        );
        assert_eq!(read(&v, Field::Timezone), Err(Missing::NoSuchComponent));
        assert_eq!(read(&v, Field::EpochSeconds), Err(Missing::NoSuchComponent));
    }

    #[test]
    fn datetime_components_read_the_local_fields_and_the_offset() {
        // Temporal5 [6] with the offset Stockholm has on that day.
        let local = LocalDateTime::new(date(1984, 11, 11), time(12, 31, 14, 645_876));
        let v = Temporal::DateTime(DateTime::from_local(local, 3600).expect("a datetime"));
        check(
            &v,
            &[
                ("year", 1984),
                ("dayOfQuarter", 42),
                ("hour", 12),
                ("millisecond", 645),
                ("offsetMinutes", 60),
                ("offsetSeconds", 3600),
                ("epochSeconds", 469_020_674),
                ("epochMillis", 469_020_674_645),
            ],
        );
        assert_eq!(text(&v, "offset"), "+01:00");
        assert_eq!(text(&v, "timezone"), "+01:00");
        // UTC is `Z` (java.time `ZoneOffset.UTC`), seconds of an offset are written.
        let utc = Temporal::DateTime(DateTime::from_local(local, 0).expect("a datetime"));
        assert_eq!(text(&utc, "timezone"), "Z");
        let odd = DateTime::from_local(local, -(2 * 3600 + 5 * 60 + 7)).expect("a datetime");
        let odd = Temporal::DateTime(odd);
        assert_eq!(text(&odd, "offset"), "-02:05:07");
        assert_eq!(int(&odd, "offsetMinutes"), -125);
        // Before the epoch, epoch seconds and millis round down.
        let early = Temporal::DateTime(DateTime::from_micros(-1, 0).expect("a datetime"));
        assert_eq!(int(&early, "epochSeconds"), -1);
        assert_eq!(int(&early, "epochMillis"), -1);
    }

    #[test]
    fn duration_totals_and_remainders_follow_temporal5() {
        // Temporal5 [7] at microsecond precision: 1 year 4 months, 10 days, 1h 1m 1.111111s.
        let d = Temporal::Duration(Duration::new(16, 10, 3_661_111_111));
        check(
            &d,
            &[
                ("years", 1),
                ("quarters", 5),
                ("months", 16),
                ("weeks", 1),
                ("days", 10),
                ("hours", 1),
                ("minutes", 61),
                ("seconds", 3661),
                ("milliseconds", 3_661_111),
                ("microseconds", 3_661_111_111),
                ("nanoseconds", 3_661_111_111_000),
                ("quartersOfYear", 1),
                ("monthsOfQuarter", 1),
                ("monthsOfYear", 4),
                ("daysOfWeek", 3),
                ("minutesOfHour", 1),
                ("secondsOfMinute", 1),
                ("millisecondsOfSecond", 111),
                ("microsecondsOfSecond", 111_111),
                ("nanosecondsOfSecond", 111_111_000),
            ],
        );
        assert_eq!(read(&d, Field::Year), Err(Missing::NoSuchComponent));
    }

    #[test]
    fn negative_durations_read_as_neo4j_normalizes_them() {
        // Temporal10 [1]: -86399.9 s reads as seconds -86400 and a positive fraction.
        let d = Temporal::Duration(Duration::new(0, 0, -86_399_900_000));
        check(
            &d,
            &[
                ("seconds", -86_400),
                ("nanosecondsOfSecond", 100_000_000),
                ("millisecondsOfSecond", 100),
                ("milliseconds", -86_399_900),
                ("hours", -24),
                ("minutesOfHour", 0),
            ],
        );
        // Months and days truncate toward zero (Java division).
        let d = Temporal::Duration(Duration::new(-16, -10, 0));
        check(
            &d,
            &[
                ("years", -1),
                ("monthsOfYear", -4),
                ("quarters", -5),
                ("quartersOfYear", -1),
                ("monthsOfQuarter", -1),
                ("weeks", -1),
                ("daysOfWeek", -3),
            ],
        );
    }

    #[test]
    fn a_total_beyond_the_integer_range_is_out_of_range() {
        let d = Temporal::Duration(Duration::new(0, 0, i64::MAX));
        assert_eq!(read(&d, Field::Nanoseconds), Err(Missing::OutOfRange));
        assert_eq!(
            read(&d, Field::Microseconds),
            Ok(Component::Integer(i64::MAX))
        );
    }

    #[test]
    fn unknown_names_are_no_component() {
        assert_eq!(Field::parse("yearz"), None);
        assert_eq!(Field::parse(""), None);
        let d = Temporal::Date(date(2020, 1, 1));
        let error = component::<()>(&d, "hour", SPAN).expect_err("no hour");
        assert_eq!(error.gqlstatus, "22G03");
        assert!(
            error
                .message
                .starts_with("a date has no component hour; its components are year, quarter"),
            "{}",
            error.message
        );
        let error = component::<()>(&d, "nope", SPAN).expect_err("no nope");
        assert_eq!(error.gqlstatus, "22G03");
        let long = Temporal::Duration(Duration::new(0, 0, i64::MAX));
        let error = component::<()>(&long, "nanoseconds", SPAN).expect_err("too big");
        assert_eq!(error.gqlstatus, "22003");
    }

    #[test]
    fn to_text_is_the_neo4j_form() {
        assert_eq!(to_text(&Temporal::Time(time(12, 0, 0, 0))), "12:00");
        assert_eq!(
            to_text(&Temporal::Duration(Duration::new(0, 0, -60_001_000))),
            "PT-1M-0.001S"
        );
        assert_eq!(to_text(&Temporal::Date(date(1984, 10, 11))), "1984-10-11");
    }
}
