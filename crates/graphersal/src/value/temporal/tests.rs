//! Unit tests of the temporal values: text round trips (also a pseudo-random sweep over the whole
//! range), the TinkerPop inputs, parse rejections, equality and order, time zones with daylight
//! saving, and every unit x type combination of the arithmetic.

use std::cmp::Ordering;
use std::collections::HashSet;

use super::arith::{add, add_duration, age, diff, diff_millis, part, trunc};
use super::civil::{MAX_MICROS, MICROS_PER_DAY, MICROS_PER_SECOND, MIN_MICROS};
use super::zone::Zone;
use super::*;
use crate::value::Number;
use crate::value::dt::DT;

fn date(text: &str) -> Date {
    text.parse().unwrap()
}

fn time(text: &str) -> Time {
    text.parse().unwrap()
}

fn ldt(text: &str) -> LocalDateTime {
    text.parse().unwrap()
}

fn dt(text: &str) -> DateTime {
    text.parse().unwrap()
}

fn dur(text: &str) -> Duration {
    text.parse().unwrap()
}

fn berlin() -> Zone {
    Zone::parse("Europe/Berlin").unwrap()
}

/// Every field of a value, so a test can tell `12:00Z` from `14:00+02:00`.
fn same(a: Temporal, b: Temporal) -> bool {
    a.bits() == b.bits()
}

fn assert_same(a: Temporal, b: Temporal) {
    assert!(same(a, b), "{a:?} is not exactly {b:?}");
}

fn int(n: i64) -> Number {
    Number::from_i64(n)
}

/// A deterministic xorshift generator for the sweeps.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// A value in `low..=high`.
    fn range(&mut self, low: i64, high: i64) -> i64 {
        let span = (high as i128 - low as i128 + 1) as u128;
        (low as i128 + (self.next() as u128 % span) as i128) as i64
    }
}

// ---------------------------------------------------------------------------------------------
// Text

#[test]
fn canonical_text_of_every_type() {
    assert_eq!(date("2023-08-02").to_string(), "2023-08-02");
    assert_eq!(
        Date::from_ymd(-44, 3, 15).unwrap().to_string(),
        "-0044-03-15"
    );
    assert_eq!(Date::from_ymd(0, 1, 1).unwrap().to_string(), "0000-01-01");
    assert_eq!(time("09:30").to_string(), "09:30:00");
    assert_eq!(time("09:30:00.25").to_string(), "09:30:00.250");
    assert_eq!(time("09:30:00.2505").to_string(), "09:30:00.250500");
    assert_eq!(time("23:59:59.999999").to_string(), "23:59:59.999999");
    assert_eq!(
        ldt("2023-08-02T09:30:00").to_string(),
        "2023-08-02T09:30:00"
    );
    assert_eq!(ldt("2023-08-02 09:30").to_string(), "2023-08-02T09:30:00");
    assert_eq!(ldt("2023-08-02").to_string(), "2023-08-02T00:00:00");
    assert_eq!(
        dt("2023-08-02T00:00:00Z").to_string(),
        "2023-08-02T00:00:00Z"
    );
    assert_eq!(
        dt("2023-08-02T00:00:00+00:00").to_string(),
        "2023-08-02T00:00:00Z"
    );
    assert_eq!(
        dt("2023-08-02T00:00:00-0700").to_string(),
        "2023-08-02T00:00:00-07:00"
    );
    assert_eq!(
        dt("2023-08-02T00:00:00+05").to_string(),
        "2023-08-02T00:00:00+05:00"
    );
    assert_eq!(
        dt("1900-01-01T00:00:00+00:19:32").to_string(),
        "1900-01-01T00:00:00+00:19:32"
    );
    assert_eq!(
        dt("2023-08-02T00:00:00.001Z").to_string(),
        "2023-08-02T00:00:00.001Z"
    );
    assert_eq!(dur("PT0S").to_string(), "PT0S");
    assert_eq!(dur("P1Y2M3DT4H5M6.5S").to_string(), "P1Y2M3DT4H5M6.500S");
    assert_eq!(dur("P2W").to_string(), "P14D");
    assert_eq!(dur("-P1DT2H").to_string(), "-P1DT2H");
    assert_eq!(dur("P1M-2D").to_string(), "P1M-2D");
    assert_eq!(dur("P1MT-0.5S").to_string(), "P1MT-0.500S");
    assert_eq!(dur("PT36H").to_string(), "PT36H");
    assert_eq!(dur("p1dt1h").to_string(), "P1DT1H");
}

#[test]
fn fractions_beyond_the_microsecond_are_cut() {
    assert_eq!(time("10:00:00.123456789").to_string(), "10:00:00.123456");
    assert_eq!(
        dt("2023-08-02T10:00:00,5Z").to_string(),
        "2023-08-02T10:00:00.500Z"
    );
}

#[test]
fn the_tinkerpop_inputs() {
    let utc = dt("2023-08-02T00:00:00Z");
    assert_eq!(utc.offset_seconds(), 0);
    let pacific = dt("2023-08-02T00:00:00-07:00");
    assert_eq!(pacific.offset_seconds(), -7 * 3600);
    assert_eq!(
        pacific.epoch_micros() - utc.epoch_micros(),
        7 * 3600 * MICROS_PER_SECOND
    );
    // A date alone is midnight UTC (TinkerPop `asDate("2020-08-02")`).
    let day = dt("2020-08-02");
    assert_eq!(day.to_string(), "2020-08-02T00:00:00Z");
    assert_eq!(dt("1970-01-01T00:00Z").epoch_micros(), 0);
    assert_eq!(
        DateTime::from_epoch_millis(1_694_017_707_000)
            .unwrap()
            .to_string(),
        "2023-09-06T16:28:27Z"
    );
    assert_eq!(dt("2023-09-06T16:28:27Z").epoch_millis(), 1_694_017_707_000);
}

#[test]
fn epoch_millis_round_down() {
    let before = DateTime::from_micros(-1, 0).unwrap();
    assert_eq!(before.epoch_millis(), -1);
    assert_eq!(before.to_string(), "1969-12-31T23:59:59.999999Z");
    assert_eq!(DateTime::from_micros(1_999, 0).unwrap().epoch_millis(), 1);
}

#[test]
fn zone_suffixes() {
    // Only a zone: the wall-clock time resolved in it.
    assert_eq!(
        dt("2023-08-02T10:00:00[Europe/Berlin]").to_string(),
        "2023-08-02T10:00:00+02:00"
    );
    assert_eq!(
        dt("2023-01-02T10:00:00[Europe/Berlin]").to_string(),
        "2023-01-02T10:00:00+01:00"
    );
    // An offset and a matching zone; the critical flag is accepted.
    assert_eq!(
        dt("2023-08-02T10:00:00+02:00[!Europe/Berlin]").to_string(),
        "2023-08-02T10:00:00+02:00"
    );
    // An offset the zone does not have then.
    assert!(matches!(
        "2023-08-02T10:00:00+01:00[Europe/Berlin]".parse::<DateTime>(),
        Err(TemporalError::Parse { .. })
    ));
    assert!(matches!(
        "2023-08-02T10:00:00[Not/AZone]".parse::<DateTime>(),
        Err(TemporalError::UnknownZone { .. })
    ));
    assert!(matches!(
        Zone::parse(""),
        Err(TemporalError::UnknownZone { .. })
    ));
    assert!(matches!(
        Zone::parse("+19:00"),
        Err(TemporalError::Parse { .. })
    ));
    assert!(Zone::parse("UTC").is_ok());
    assert!(Zone::parse("Z").is_ok());
    assert!(Zone::parse("-05:30").is_ok());
}

#[test]
fn parse_rejections() {
    let dates = [
        "",
        "2023",
        "2023-02-29",
        "2023-13-01",
        "2023-00-10",
        "2023-08-00",
        "2023-8-2",
        "23-08-02",
        "10000-01-01",
        "-10000-01-01",
        "+1234567-01-01",
        "2023-08-02x",
        "2023/08/02",
        " 2023-08-02",
        "2023-08-02T10:00",
    ];
    for text in dates {
        assert!(
            matches!(text.parse::<Date>(), Err(TemporalError::Parse { .. })),
            "date {text:?}"
        );
    }
    let times = [
        "",
        "24:00",
        "10:60",
        "10:00:60",
        "1:00",
        "10:0",
        "10",
        "10:00:00.",
        "10:00Z",
        "10:00+02:00",
        "10:00:00.1234567890",
        "10:00 ",
    ];
    for text in times {
        assert!(
            matches!(text.parse::<Time>(), Err(TemporalError::Parse { .. })),
            "time {text:?}"
        );
    }
    let locals = [
        "2023-08-02T10:00Z",
        "2023-08-02T10:00+02:00",
        "2023-08-02T10:00[UTC]",
        "2023-08-02T",
    ];
    for text in locals {
        assert!(
            matches!(
                text.parse::<LocalDateTime>(),
                Err(TemporalError::Parse { .. })
            ),
            "local {text:?}"
        );
    }
    let date_times = [
        "2023-08-02T10:00:00+19:00",
        "2023-08-02T10:00:00+02:60",
        "2023-08-02T10:00:00+2",
        "2023-08-02T10:00:00[]",
        "2023-08-02T10:00:00[Europe/Berlin",
        "2023-08-02T10:00:00ZZ",
    ];
    for text in date_times {
        assert!(text.parse::<DateTime>().is_err(), "datetime {text:?}");
    }
    let durations = [
        "",
        "P",
        "PT",
        "P1DT",
        "1D",
        "P1H",
        "PT1D",
        "P1.5D",
        "PT1.5H",
        "P1D1Y",
        "PT1S1M",
        "P1",
        "P-",
        "P1X",
        "P9223372036854775808D",
        "P2147483648M",
        "PT9223372036854775807H",
    ];
    for text in durations {
        assert!(
            matches!(text.parse::<Duration>(), Err(TemporalError::Parse { .. })),
            "duration {text:?}"
        );
    }
}

#[test]
fn parse_errors_quote_the_input_and_cut_it() {
    let long = "x".repeat(200);
    let Err(TemporalError::Parse {
        input, expected, ..
    }) = long.parse::<Date>()
    else {
        panic!("expected a parse error");
    };
    assert_eq!(expected, "date");
    assert!(input.starts_with("\"xxx"));
    assert!(input.ends_with("(200 chars)"));
    let message = "2023-02-30".parse::<Date>().unwrap_err().to_string();
    assert!(message.contains("\"2023-02-30\""), "{message}");
}

#[test]
fn duration_edges_round_trip() {
    for value in [
        Duration::new(i32::MIN, i32::MIN, i64::MIN),
        Duration::new(i32::MAX, i32::MAX, i64::MAX),
        Duration::new(i32::MIN, 0, 0),
        Duration::new(0, 0, i64::MIN),
        Duration::new(1, -1, 1),
        Duration::new(-1, 1, -1),
        Duration::new(0, 0, -1),
    ] {
        let text = value.to_string();
        let back: Duration = text.parse().unwrap();
        assert_eq!(back, value, "{text}");
        assert_eq!(back.bits(), value.bits(), "{text}");
    }
}

impl Duration {
    fn bits(self) -> TemporalBits {
        Temporal::Duration(self).bits()
    }
}

#[test]
fn pseudo_random_round_trips_over_the_whole_range() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let min_day = civil::MIN_DAY as i64;
    let max_day = civil::MAX_DAY as i64;
    let max_offset = MAX_OFFSET_SECONDS as i64;
    for _ in 0..20_000 {
        let d = Date::from_days_since_epoch(rng.range(min_day, max_day) as i32).unwrap();
        assert_eq!(d.to_string().parse::<Date>().unwrap(), d);

        let t = Time::from_micros_of_day(rng.range(0, MICROS_PER_DAY - 1)).unwrap();
        assert_eq!(t.to_string().parse::<Time>().unwrap(), t);

        let l = LocalDateTime::from_micros(rng.range(MIN_MICROS, MAX_MICROS)).unwrap();
        assert_eq!(l.to_string().parse::<LocalDateTime>().unwrap(), l);

        let offset = rng.range(-max_offset, max_offset) as i32;
        let margin = max_offset * MICROS_PER_SECOND;
        let instant = rng.range(MIN_MICROS + margin, MAX_MICROS - margin);
        let x = DateTime::from_micros(instant, offset).unwrap();
        let back: DateTime = x.to_string().parse().unwrap();
        assert_same(Temporal::DateTime(back), Temporal::DateTime(x));

        let duration = Duration::new(rng.next() as i32, rng.next() as i32, rng.next() as i64);
        let back: Duration = duration.to_string().parse().unwrap();
        assert_eq!(back.bits(), duration.bits(), "{duration}");

        // Temporal::parse_as is the same reader.
        for value in [
            Temporal::Date(d),
            Temporal::Time(t),
            Temporal::LocalDateTime(l),
            Temporal::DateTime(x),
            Temporal::Duration(duration),
        ] {
            let parsed = Temporal::parse_as(value.kind(), &value.to_string()).unwrap();
            assert_same(parsed, value);
        }
    }
}

#[test]
fn range_edges_are_inclusive() {
    let first = date("-9999-01-01");
    let last = date("9999-12-31");
    assert_eq!(first.days_since_epoch(), civil::MIN_DAY);
    assert_eq!(last.days_since_epoch(), civil::MAX_DAY);
    assert_eq!(ldt("9999-12-31T23:59:59.999999").micros(), MAX_MICROS);
    assert_eq!(ldt("-9999-01-01T00:00:00").micros(), MIN_MICROS);
    assert!(Date::from_days_since_epoch(civil::MAX_DAY + 1).is_err());
    assert!(LocalDateTime::from_micros(MAX_MICROS + 1).is_err());
    assert!(LocalDateTime::from_micros(i64::MIN).is_err());
    // The local time of a datetime must stay in range too.
    assert!(DateTime::from_micros(MAX_MICROS, 3600).is_err());
    assert!(DateTime::from_micros(MAX_MICROS, -3600).is_ok());
    assert!("9999-12-31T23:30:00-01:00".parse::<DateTime>().is_err());
    assert!(DateTime::from_epoch_millis(i64::MAX).is_err());
    assert!(DateTime::from_micros(0, MAX_OFFSET_SECONDS + 1).is_err());
    assert!(Time::from_hms_micro(24, 0, 0, 0).is_err());
    assert!(Time::from_micros_of_day(MICROS_PER_DAY).is_err());
    assert!(Date::from_ymd(2023, 2, 29).is_err());
    assert!(Date::from_ymd(2024, 2, 29).is_ok());
    assert!(Date::from_ymd(10_000, 1, 1).is_err());
}

#[test]
fn accessors() {
    let d = date("2023-08-02");
    assert_eq!((d.year(), d.month(), d.day()), (2023, 8, 2));
    let t = time("09:30:15.250500");
    assert_eq!(
        (t.hour(), t.minute(), t.second(), t.microsecond()),
        (9, 30, 15, 250_500)
    );
    assert_eq!(Time::from_hms_micro(9, 30, 15, 250_500).unwrap(), t);
    let l = ldt("2023-08-02T09:30:15.250500");
    assert_eq!(l.date(), d);
    assert_eq!(l.time(), t);
    assert_eq!(LocalDateTime::new(d, t), l);
    let x = dt("2023-08-02T09:30:00+02:00");
    assert_eq!(x.local(), ldt("2023-08-02T09:30:00"));
    assert_eq!(
        x.with_offset(0).unwrap().to_string(),
        "2023-08-02T07:30:00Z"
    );
    let before_epoch = ldt("1969-12-31T23:00:00");
    assert_eq!(before_epoch.date(), date("1969-12-31"));
    assert_eq!(before_epoch.time(), time("23:00"));
    let span = dur("P1M2DT3S");
    assert_eq!(
        (span.months(), span.days(), span.micros()),
        (1, 2, 3_000_000)
    );
    assert!(!span.is_zero());
    assert!(Duration::default().is_zero());
    assert!(Duration::new(i32::MIN, 0, 0).checked_neg().is_err());
    assert!(
        Duration::new(i32::MAX, 0, 0)
            .checked_add(dur("P1M"))
            .is_err()
    );
    assert_eq!(Temporal::from(d).kind(), TemporalKind::Date);
    assert_eq!(TemporalKind::LocalDateTime.to_string(), "localdatetime");
    assert_eq!(
        format!("{:?}", Temporal::from(x)),
        "DateTime(2023-08-02T09:30:00+02:00)"
    );
}

// ---------------------------------------------------------------------------------------------
// Equality and order

#[test]
fn a_datetime_equals_by_its_instant_only() {
    let a = dt("2023-08-02T12:00:00Z");
    let b = dt("2023-08-02T14:00:00+02:00");
    assert_eq!(a, b);
    assert_eq!(a.cmp(&b), Ordering::Equal);
    let mut set = HashSet::new();
    set.insert(Temporal::DateTime(a));
    assert!(set.contains(&Temporal::DateTime(b)));
    // The offset is kept and printed.
    assert_eq!(b.to_string(), "2023-08-02T14:00:00+02:00");
    assert!(!same(Temporal::DateTime(a), Temporal::DateTime(b)));
    assert!(dt("2023-08-02T13:00:00+02:00") < a);
}

#[test]
fn durations_equal_field_by_field_and_order_by_length() {
    assert_ne!(dur("P1M"), dur("P30D"));
    assert_ne!(dur("P1D"), dur("PT24H"));
    // Same normalized length: the fields break the tie, consistently with `!=`.
    assert_eq!(dur("P1M").cmp(&dur("P30D")), Ordering::Greater);
    assert_eq!(dur("P1D").cmp(&dur("PT24H")), Ordering::Greater);
    assert!(dur("P1M") < dur("P31D"));
    assert!(dur("PT25H") > dur("P1D"));
    assert!(dur("-P1D") < dur("PT0S"));
    assert_eq!(dur("P1Y"), dur("P12M"));
    let extreme = [
        Duration::new(i32::MIN, i32::MIN, i64::MIN),
        Duration::new(i32::MAX, i32::MAX, i64::MAX),
    ];
    assert!(extreme[0] < extreme[1]);
}

#[test]
fn temporal_kinds_order_date_local_datetime_time_duration() {
    let values = [
        Temporal::Duration(dur("-P100Y")),
        Temporal::Time(time("00:00")),
        Temporal::DateTime(dt("-9999-01-02T00:00:00+18:00")),
        Temporal::LocalDateTime(ldt("-9999-01-01T00:00")),
        Temporal::Date(date("9999-12-31")),
    ];
    let mut sorted = values;
    sorted.sort();
    let kinds: Vec<_> = sorted.iter().map(Temporal::kind).collect();
    assert_eq!(
        kinds,
        [
            TemporalKind::Date,
            TemporalKind::LocalDateTime,
            TemporalKind::DateTime,
            TemporalKind::Time,
            TemporalKind::Duration
        ]
    );
    // Different kinds never equal, even for the same moment.
    assert_ne!(
        Temporal::Date(date("2023-08-02")),
        Temporal::LocalDateTime(ldt("2023-08-02T00:00"))
    );
    assert_ne!(
        Temporal::LocalDateTime(ldt("2023-08-02T00:00")),
        Temporal::DateTime(dt("2023-08-02T00:00Z"))
    );
}

// ---------------------------------------------------------------------------------------------
// Adding

#[test]
fn adding_units_to_a_datetime_keeps_its_offset() {
    let x = Temporal::DateTime(dt("2023-08-02T10:00:00-07:00"));
    let cases = [
        (DT::Microsecond, 1, "2023-08-02T10:00:00.000001-07:00"),
        (DT::Millisecond, 250, "2023-08-02T10:00:00.250-07:00"),
        (DT::Second, -1, "2023-08-02T09:59:59-07:00"),
        (DT::Minute, 90, "2023-08-02T11:30:00-07:00"),
        (DT::Hour, 20, "2023-08-03T06:00:00-07:00"),
        (DT::Day, 1, "2023-08-03T10:00:00-07:00"),
        (DT::Week, -1, "2023-07-26T10:00:00-07:00"),
        (DT::Month, 6, "2024-02-02T10:00:00-07:00"),
        (DT::Quarter, 1, "2023-11-02T10:00:00-07:00"),
        (DT::Year, -23, "2000-08-02T10:00:00-07:00"),
        (DT::Decade, 1, "2033-08-02T10:00:00-07:00"),
        (DT::Century, -1, "1923-08-02T10:00:00-07:00"),
        (DT::Millennium, 1, "3023-08-02T10:00:00-07:00"),
    ];
    for (unit, n, expected) in cases {
        let got = add(x, unit, n, None).unwrap();
        assert_same(got, Temporal::DateTime(dt(expected)));
    }
}

#[test]
fn months_clamp_to_the_end_of_the_month() {
    let jan31 = Temporal::Date(date("2024-01-31"));
    assert_same(
        add(jan31, DT::Month, 1, None).unwrap(),
        Temporal::Date(date("2024-02-29")),
    );
    assert_same(
        add(jan31, DT::Month, 13, None).unwrap(),
        Temporal::Date(date("2025-02-28")),
    );
    assert_same(
        add(jan31, DT::Quarter, 1, None).unwrap(),
        Temporal::Date(date("2024-04-30")),
    );
    let leap = Temporal::LocalDateTime(ldt("2024-02-29T08:00"));
    assert_same(
        add(leap, DT::Year, 1, None).unwrap(),
        Temporal::LocalDateTime(ldt("2025-02-28T08:00")),
    );
    assert_same(
        add(leap, DT::Year, 4, None).unwrap(),
        Temporal::LocalDateTime(ldt("2028-02-29T08:00")),
    );
}

#[test]
fn daylight_saving_in_berlin() {
    let zone = berlin();
    // 2023-03-26 02:00 -> 03:00 (spring forward), 2023-10-29 03:00 -> 02:00 (fall back).
    let before_gap = Temporal::DateTime(dt("2023-03-25T12:00:00+01:00"));
    assert_same(
        add(before_gap, DT::Day, 1, Some(&zone)).unwrap(),
        Temporal::DateTime(dt("2023-03-26T12:00:00+02:00")),
    );
    assert_same(
        add(before_gap, DT::Hour, 24, Some(&zone)).unwrap(),
        Temporal::DateTime(dt("2023-03-26T13:00:00+02:00")),
    );
    // Without a zone a day is 24 hours at the value's own offset.
    assert_same(
        add(before_gap, DT::Day, 1, None).unwrap(),
        Temporal::DateTime(dt("2023-03-26T12:00:00+01:00")),
    );
    let before_fold = Temporal::DateTime(dt("2023-10-28T12:00:00+02:00"));
    assert_same(
        add(before_fold, DT::Day, 1, Some(&zone)).unwrap(),
        Temporal::DateTime(dt("2023-10-29T12:00:00+01:00")),
    );
    assert_same(
        add(before_fold, DT::Hour, 24, Some(&zone)).unwrap(),
        Temporal::DateTime(dt("2023-10-29T11:00:00+01:00")),
    );
    // A skipped wall-clock time moves forward; a repeated one takes the earlier offset.
    assert_eq!(
        zone.resolve(ldt("2023-03-26T02:30")).unwrap().to_string(),
        "2023-03-26T03:30:00+02:00"
    );
    assert_eq!(
        zone.resolve(ldt("2023-10-29T02:30")).unwrap().to_string(),
        "2023-10-29T02:30:00+02:00"
    );
    let into_gap = Temporal::DateTime(dt("2023-03-25T02:30:00+01:00"));
    assert_same(
        add(into_gap, DT::Day, 1, Some(&zone)).unwrap(),
        Temporal::DateTime(dt("2023-03-26T03:30:00+02:00")),
    );
    // A duration of one day keeps the wall-clock time too; its hours do not.
    assert_same(
        add_duration(before_gap, dur("P1DT1H"), Some(&zone)).unwrap(),
        Temporal::DateTime(dt("2023-03-26T13:00:00+02:00")),
    );
    assert_same(
        add_duration(before_gap, dur("PT24H"), Some(&zone)).unwrap(),
        Temporal::DateTime(dt("2023-03-26T13:00:00+02:00")),
    );
    // Truncation and parts on the zone's wall clock.
    let evening = Temporal::DateTime(dt("2023-03-26T22:30:00Z"));
    assert_same(
        trunc(evening, DT::Day, Some(&zone)).unwrap(),
        Temporal::DateTime(dt("2023-03-27T00:00:00+02:00")),
    );
    assert_eq!(part(evening, DT::Day, Some(&zone)).unwrap(), int(27));
    assert_eq!(part(evening, DT::Offset, Some(&zone)).unwrap(), int(7200));
    assert_eq!(part(evening, DT::Offset, None).unwrap(), int(0));
    // The zone's day across the gap is 23 hours.
    let start = Temporal::DateTime(dt("2023-03-26T00:00:00+01:00"));
    let end = Temporal::DateTime(dt("2023-03-27T00:00:00+02:00"));
    assert_eq!(diff(end, start, DT::Hour, None).unwrap(), 23);
    assert_eq!(diff(end, start, DT::Day, Some(&zone)).unwrap(), 1);
}

#[test]
fn time_wraps_and_durations_add_fields() {
    let t = Temporal::Time(time("23:30"));
    assert_same(
        add(t, DT::Hour, 1, None).unwrap(),
        Temporal::Time(time("00:30")),
    );
    assert_same(
        add(t, DT::Minute, -1440 * 3 - 30, None).unwrap(),
        Temporal::Time(time("23:00")),
    );
    assert_same(add(t, DT::Hour, i64::MAX, None).unwrap(), {
        let wrapped = (time("23:30").micros_of_day() as i128 + i64::MAX as i128 * 3_600_000_000)
            .rem_euclid(MICROS_PER_DAY as i128) as i64;
        Temporal::Time(Time::from_micros_of_day(wrapped).unwrap())
    });
    let d = Temporal::Duration(dur("P1M"));
    assert_eq!(add(d, DT::Year, 1, None).unwrap().to_string(), "P1Y1M");
    assert_eq!(add(d, DT::Week, 1, None).unwrap().to_string(), "P1M7D");
    assert_eq!(add(d, DT::Hour, 2, None).unwrap().to_string(), "P1MT2H");
    assert_same(
        add_duration(Temporal::Date(date("2024-01-31")), dur("P1M1D"), None).unwrap(),
        Temporal::Date(date("2024-03-01")),
    );
    assert_same(
        add_duration(Temporal::Time(time("10:00")), dur("-PT11H"), None).unwrap(),
        Temporal::Time(time("23:00")),
    );
    assert_same(
        add_duration(
            Temporal::LocalDateTime(ldt("2024-01-31T10:00")),
            dur("P1MT14H"),
            None,
        )
        .unwrap(),
        Temporal::LocalDateTime(ldt("2024-03-01T00:00")),
    );
    assert_eq!(
        add_duration(d, dur("P1DT1S"), None).unwrap().to_string(),
        "P1M1DT1S"
    );
    assert!(matches!(
        add_duration(Temporal::Date(date("2024-01-31")), dur("PT1H"), None),
        Err(TemporalError::Unsupported { .. })
    ));
    assert!(matches!(
        add_duration(Temporal::Time(time("10:00")), dur("P1D"), None),
        Err(TemporalError::Unsupported { .. })
    ));
}

#[test]
fn overflow_at_the_range_edges_is_an_error() {
    let last = Temporal::DateTime(dt("9999-12-31T23:00:00Z"));
    let out = |r: TemporalResult<Temporal>| matches!(r, Err(TemporalError::OutOfRange { .. }));
    assert!(out(add(last, DT::Hour, 2, None)));
    assert!(out(add(last, DT::Day, 1, None)));
    assert!(out(add(last, DT::Month, 1, None)));
    assert!(out(add(last, DT::Microsecond, i64::MAX, None)));
    assert!(out(add(last, DT::Hour, i64::MIN, None)));
    assert!(out(add(last, DT::Millennium, i64::MAX, None)));
    assert!(out(add(
        Temporal::Date(date("9999-12-31")),
        DT::Day,
        1,
        None
    )));
    assert!(out(add(
        Temporal::Date(date("-9999-01-01")),
        DT::Day,
        -1,
        None
    )));
    assert!(out(add(
        Temporal::Date(date("2023-01-01")),
        DT::Year,
        i64::MAX,
        None
    )));
    assert!(out(add(
        Temporal::Date(date("2023-01-01")),
        DT::Week,
        i64::MIN,
        None
    )));
    assert!(out(add(
        Temporal::LocalDateTime(ldt("-9999-01-01")),
        DT::Second,
        -1,
        None
    )));
    assert!(out(add(
        Temporal::Duration(dur("P1M")),
        DT::Month,
        i64::MAX,
        None
    )));
    assert!(out(add(
        Temporal::Duration(dur("P1D")),
        DT::Day,
        i32::MAX as i64,
        None
    )));
    assert!(out(add(
        Temporal::Duration(Duration::new(0, 0, i64::MAX)),
        DT::Microsecond,
        1,
        None
    )));
    assert!(out(add_duration(last, dur("PT2H"), None)));
    assert!(out(add_duration(
        Temporal::Duration(Duration::new(i32::MAX, 0, 0)),
        dur("P1M"),
        None
    )));
    // The edge itself is reachable.
    assert_same(
        add(last, DT::Minute, 59, None).unwrap(),
        Temporal::DateTime(dt("9999-12-31T23:59:00Z")),
    );
    // A time zone pushing the local time out of range.
    assert!(add(last, DT::Hour, 0, Some(&Zone::parse("+02:00").unwrap())).is_err());
}

// ---------------------------------------------------------------------------------------------
// Differences

#[test]
fn diff_millis_is_tinkerpop_date_diff() {
    let a = Temporal::DateTime(dt("2023-08-08T00:00:30Z"));
    let b = Temporal::DateTime(dt("2023-08-08T00:00:00+00:00"));
    assert_eq!(diff_millis(a, b).unwrap(), 30_000);
    assert_eq!(diff_millis(b, a).unwrap(), -30_000);
    // The offset does not matter, only the instants.
    let c = Temporal::DateTime(dt("2023-08-08T02:00:00+02:00"));
    assert_eq!(diff_millis(b, c).unwrap(), 0);
    assert_eq!(
        diff_millis(
            Temporal::Date(date("2023-08-02")),
            Temporal::Date(date("2023-08-01"))
        )
        .unwrap(),
        86_400_000
    );
    assert_eq!(
        diff_millis(
            Temporal::Time(time("10:00:00.0019")),
            Temporal::Time(time("10:00"))
        )
        .unwrap(),
        1
    );
    assert!(matches!(
        diff_millis(a, Temporal::Date(date("2023-08-02"))),
        Err(TemporalError::Unsupported { .. })
    ));
    assert!(matches!(
        diff_millis(
            Temporal::Duration(dur("P1D")),
            Temporal::Duration(dur("P1D"))
        ),
        Err(TemporalError::Unsupported { .. })
    ));
}

#[test]
fn diff_counts_whole_units_toward_zero() {
    let d = |a: &str, b: &str, unit| {
        diff(Temporal::Date(date(a)), Temporal::Date(date(b)), unit, None).unwrap()
    };
    assert_eq!(d("2023-03-31", "2023-01-31", DT::Month), 2);
    assert_eq!(d("2023-01-31", "2023-03-31", DT::Month), -2);
    // java.time `ChronoUnit.MONTHS.between`: a month counts once the day of the month is
    // reached, no end-of-month clamping (January 31 to February 28 is 0 months).
    assert_eq!(d("2023-02-28", "2023-01-31", DT::Month), 0);
    assert_eq!(d("2023-02-27", "2023-01-31", DT::Month), 0);
    assert_eq!(d("2023-03-31", "2023-02-28", DT::Month), 1);
    assert_eq!(d("2024-02-29", "2024-01-31", DT::Month), 0);
    assert_eq!(d("2023-01-31", "2023-02-28", DT::Month), 0);
    assert_eq!(d("2024-02-29", "2023-02-28", DT::Year), 1);
    assert_eq!(d("2024-02-27", "2023-02-28", DT::Year), 0);
    assert_eq!(d("2023-08-09", "2023-08-02", DT::Week), 1);
    assert_eq!(d("2023-08-08", "2023-08-02", DT::Week), 0);
    assert_eq!(d("2023-08-02", "2023-08-09", DT::Week), -1);
    assert_eq!(d("2123-01-01", "2023-01-01", DT::Century), 1);
    assert_eq!(d("2023-01-01", "1023-01-02", DT::Millennium), 0);
    assert_eq!(d("2033-01-01", "2023-01-01", DT::Decade), 1);
    assert_eq!(d("2023-04-01", "2023-01-01", DT::Quarter), 1);
    let l = |a: &str, b: &str, unit| {
        diff(
            Temporal::LocalDateTime(ldt(a)),
            Temporal::LocalDateTime(ldt(b)),
            unit,
            None,
        )
        .unwrap()
    };
    assert_eq!(l("2023-08-03T09:00", "2023-08-02T10:00", DT::Day), 0);
    assert_eq!(l("2023-08-03T10:00", "2023-08-02T10:00", DT::Day), 1);
    assert_eq!(l("2023-08-02T10:00", "2023-08-03T09:00", DT::Day), 0);
    assert_eq!(l("2023-08-03T09:00", "2023-08-02T10:00", DT::Hour), 23);
    assert_eq!(l("2023-08-02T10:00", "2023-08-03T09:00", DT::Hour), -23);
    assert_eq!(
        l(
            "2023-08-02T10:00:00.0015",
            "2023-08-02T10:00",
            DT::Millisecond
        ),
        1
    );
    assert_eq!(l("2023-03-31T10:00", "2023-02-28T11:00", DT::Month), 1);
    assert_eq!(l("2023-03-28T10:00", "2023-02-28T11:00", DT::Month), 0);
    // A datetime on the other operand's wall clock.
    let a = Temporal::DateTime(dt("2023-08-03T01:00:00+02:00"));
    let b = Temporal::DateTime(dt("2023-08-02T00:00:00Z"));
    assert_eq!(diff(a, b, DT::Day, None).unwrap(), 0);
    assert_eq!(diff(a, b, DT::Hour, None).unwrap(), 23);
    let t = diff(
        Temporal::Time(time("10:00")),
        Temporal::Time(time("08:30")),
        DT::Hour,
        None,
    );
    assert_eq!(t.unwrap(), 1);
    // Extremes stay in range.
    let first = Temporal::DateTime(dt("-9999-01-01T00:00:00Z"));
    let last = Temporal::DateTime(dt("9999-12-31T23:59:59.999999Z"));
    assert!(diff(last, first, DT::Microsecond, None).unwrap() > 0);
    assert_eq!(diff(last, first, DT::Millennium, None).unwrap(), 19);
    assert_eq!(diff(first, last, DT::Year, None).unwrap(), -19_998);
}

#[test]
fn age_is_months_then_days_then_micros_with_one_sign() {
    let ages = |a: &str, b: &str| {
        age(Temporal::Date(date(a)), Temporal::Date(date(b)), None)
            .unwrap()
            .to_string()
    };
    // PostgreSQL's documented example: `age('2001-04-10', '1957-06-13')` is
    // `43 years 9 mons 27 days`, and the reverse is its negation.
    assert_eq!(ages("2001-04-10", "1957-06-13"), "P43Y9M27D");
    assert_eq!(ages("1957-06-13", "2001-04-10"), "-P43Y9M27D");
    // A negative day count borrows the length of the earlier value's month.
    assert_eq!(ages("2023-02-28", "2023-01-31"), "P28D");
    assert_eq!(ages("2023-01-31", "2023-02-28"), "-P28D");
    assert_eq!(ages("2023-03-01", "2023-01-31"), "P1M1D");
    assert_eq!(ages("2024-03-01", "2024-02-29"), "P1D");
    let a = Temporal::LocalDateTime(ldt("2024-03-15T10:00"));
    let b = Temporal::LocalDateTime(ldt("2023-01-20T12:00"));
    assert_eq!(age(a, b, None).unwrap().to_string(), "P1Y1M25DT22H");
    assert_eq!(age(b, a, None).unwrap().to_string(), "-P1Y1M25DT22H");
    assert_eq!(
        age(
            Temporal::Time(time("10:00")),
            Temporal::Time(time("12:30")),
            None
        )
        .unwrap()
        .to_string(),
        "-PT2H30M"
    );
    let x = Temporal::DateTime(dt("2023-08-02T00:00:00Z"));
    assert!(age(x, x, None).unwrap().is_zero());
    assert!(matches!(
        age(
            Temporal::Duration(dur("P1D")),
            Temporal::Duration(dur("P1D")),
            None
        ),
        Err(TemporalError::Unsupported { .. })
    ));
    // One sign, a mirror for the swapped operands, and field ranges as PostgreSQL's.
    let mut rng = Rng(42);
    for _ in 0..2_000 {
        let a = ldt_at(rng.range(-3_000_000_000, 3_000_000_000));
        let b = ldt_at(rng.range(-3_000_000_000, 3_000_000_000));
        let span = age(Temporal::LocalDateTime(a), Temporal::LocalDateTime(b), None).unwrap();
        let signs = [
            span.months().signum(),
            span.days().signum(),
            span.micros().signum() as i32,
        ];
        assert!(
            signs.iter().all(|s| *s >= 0) || signs.iter().all(|s| *s <= 0),
            "{a} - {b} = {span}"
        );
        assert!(span.days().abs() < 31, "{a} - {b} = {span}");
        assert!(span.micros().abs() < 86_400_000_000, "{a} - {b} = {span}");
        let back = age(Temporal::LocalDateTime(b), Temporal::LocalDateTime(a), None).unwrap();
        assert_eq!(back, span.checked_neg().unwrap(), "{a} - {b} = {span}");
    }
}

/// A local date-time `seconds` seconds after the epoch.
fn ldt_at(seconds: i64) -> LocalDateTime {
    LocalDateTime::from_micros(seconds * MICROS_PER_SECOND).unwrap()
}

// ---------------------------------------------------------------------------------------------
// Truncation and parts

#[test]
fn truncation_follows_postgres_date_trunc() {
    let x = Temporal::LocalDateTime(ldt("2023-08-02T09:30:15.250500"));
    let cases = [
        (DT::Microsecond, "2023-08-02T09:30:15.250500"),
        (DT::Millisecond, "2023-08-02T09:30:15.250"),
        (DT::Second, "2023-08-02T09:30:15"),
        (DT::Minute, "2023-08-02T09:30:00"),
        (DT::Hour, "2023-08-02T09:00:00"),
        (DT::Day, "2023-08-02T00:00:00"),
        (DT::Week, "2023-07-31T00:00:00"),
        (DT::Month, "2023-08-01T00:00:00"),
        (DT::Quarter, "2023-07-01T00:00:00"),
        (DT::Year, "2023-01-01T00:00:00"),
        (DT::Decade, "2020-01-01T00:00:00"),
        (DT::Century, "2001-01-01T00:00:00"),
        (DT::Millennium, "2001-01-01T00:00:00"),
    ];
    for (unit, expected) in cases {
        assert_same(
            trunc(x, unit, None).unwrap(),
            Temporal::LocalDateTime(ldt(expected)),
        );
    }
    let year = |y: &str, unit| {
        trunc(Temporal::Date(date(y)), unit, None)
            .unwrap()
            .to_string()
    };
    assert_eq!(year("2000-06-01", DT::Century), "1901-01-01");
    assert_eq!(year("2001-06-01", DT::Century), "2001-01-01");
    assert_eq!(year("2000-06-01", DT::Millennium), "1001-01-01");
    assert_eq!(year("-0001-06-01", DT::Decade), "-0010-01-01");
    assert_eq!(year("-9999-06-01", DT::Century), "-9999-01-01");
    assert_eq!(year("-9999-06-01", DT::Millennium), "-9999-01-01");
    assert!(trunc(Temporal::Date(date("-9999-06-01")), DT::Decade, None).is_err());
    // A datetime on its own wall clock, the offset kept.
    assert_same(
        trunc(
            Temporal::DateTime(dt("2023-08-02T09:30:00+05:30")),
            DT::Hour,
            None,
        )
        .unwrap(),
        Temporal::DateTime(dt("2023-08-02T09:00:00+05:30")),
    );
    assert_same(
        trunc(Temporal::Time(time("09:30:15.5")), DT::Minute, None).unwrap(),
        Temporal::Time(time("09:30")),
    );
    let span = Temporal::Duration(dur("P1Y5M3DT4H5M6.5S"));
    let cut = |unit| trunc(span, unit, None).unwrap().to_string();
    assert_eq!(cut(DT::Second), "P1Y5M3DT4H5M6S");
    assert_eq!(cut(DT::Hour), "P1Y5M3DT4H");
    assert_eq!(cut(DT::Day), "P1Y5M3D");
    assert_eq!(cut(DT::Month), "P1Y5M");
    assert_eq!(cut(DT::Quarter), "P1Y3M");
    assert_eq!(cut(DT::Year), "P1Y");
    assert_eq!(cut(DT::Decade), "PT0S");
    assert_eq!(
        trunc(Temporal::Duration(dur("-PT1H30M")), DT::Hour, None)
            .unwrap()
            .to_string(),
        "-PT1H"
    );
}

#[test]
fn parts_follow_postgres_extract() {
    let x = Temporal::DateTime(dt("2023-08-02T09:30:15.250500+02:00"));
    let cases = [
        (DT::Microsecond, 250_500),
        (DT::Millisecond, 250),
        (DT::Second, 15),
        (DT::Minute, 30),
        (DT::Hour, 9),
        (DT::Day, 2),
        (DT::Week, 31),
        (DT::Month, 8),
        (DT::Quarter, 3),
        (DT::Year, 2023),
        (DT::Decade, 202),
        (DT::Century, 21),
        (DT::Millennium, 3),
        (DT::DayOfWeek, 3),
        (DT::IsoDayOfWeek, 3),
        (DT::DayOfYear, 214),
        (DT::IsoYear, 2023),
        (DT::Offset, 7200),
    ];
    for (unit, expected) in cases {
        assert_eq!(part(x, unit, None).unwrap(), int(expected), "{unit}");
    }
    assert_eq!(
        part(
            Temporal::DateTime(dt("1970-01-01T00:00:01.5Z")),
            DT::Epoch,
            None
        )
        .unwrap(),
        Number::from_f64(1.5)
    );
    let sunday = Temporal::Date(date("2021-01-03"));
    assert_eq!(part(sunday, DT::DayOfWeek, None).unwrap(), int(0));
    assert_eq!(part(sunday, DT::IsoDayOfWeek, None).unwrap(), int(7));
    assert_eq!(part(sunday, DT::IsoYear, None).unwrap(), int(2020));
    assert_eq!(part(sunday, DT::Week, None).unwrap(), int(53));
    assert_eq!(
        part(sunday, DT::Epoch, None).unwrap(),
        Number::from_f64(1_609_632_000.0)
    );
    let year = |y: &str, unit| part(Temporal::Date(date(y)), unit, None).unwrap();
    assert_eq!(year("2000-01-01", DT::Century), int(20));
    assert_eq!(year("2001-01-01", DT::Century), int(21));
    assert_eq!(year("2000-01-01", DT::Millennium), int(2));
    assert_eq!(year("0000-01-01", DT::Century), int(-1));
    assert_eq!(year("-0100-01-01", DT::Century), int(-2));
    assert_eq!(year("-0001-01-01", DT::Decade), int(-1));
    let t = Temporal::Time(time("09:30:15.5"));
    assert_eq!(part(t, DT::Hour, None).unwrap(), int(9));
    assert_eq!(
        part(t, DT::Epoch, None).unwrap(),
        Number::from_f64(34_215.5)
    );
    let span = Temporal::Duration(dur("P1Y5M3DT4H5M6.5S"));
    let p = |unit| part(span, unit, None).unwrap();
    assert_eq!(p(DT::Year), int(1));
    assert_eq!(p(DT::Month), int(5));
    assert_eq!(p(DT::Quarter), int(2));
    assert_eq!(p(DT::Day), int(3));
    assert_eq!(p(DT::Hour), int(4));
    assert_eq!(p(DT::Minute), int(5));
    assert_eq!(p(DT::Second), int(6));
    assert_eq!(p(DT::Millisecond), int(500));
    assert_eq!(p(DT::Microsecond), int(500_000));
    let epoch = 1.0 * 365.25 * 86_400.0 + 5.0 * 30.0 * 86_400.0 + 3.0 * 86_400.0 + 14_706.5;
    assert_eq!(p(DT::Epoch), Number::from_f64(epoch));
    assert_eq!(
        part(Temporal::Duration(dur("-PT1H30M")), DT::Minute, None).unwrap(),
        int(-30)
    );
}

// ---------------------------------------------------------------------------------------------
// Every unit x type combination

/// Whether an operation is defined for a kind and a unit (the documented matrix).
fn defined(operation: &str, kind: TemporalKind, unit: DT) -> bool {
    let fixed = unit.fixed_micros().is_some();
    let is_unit = unit.is_unit();
    match (operation, kind) {
        ("add" | "trunc" | "diff", TemporalKind::DateTime | TemporalKind::LocalDateTime) => is_unit,
        ("add" | "trunc" | "diff", TemporalKind::Date) => is_unit && !fixed,
        ("add" | "trunc" | "diff", TemporalKind::Time) => fixed,
        ("add", TemporalKind::Duration) => is_unit,
        ("trunc", TemporalKind::Duration) => is_unit && unit != DT::Week,
        ("diff", TemporalKind::Duration) => false,
        ("part", TemporalKind::DateTime) => true,
        ("part", TemporalKind::LocalDateTime) => unit != DT::Offset,
        ("part", TemporalKind::Date) => !fixed && unit != DT::Offset,
        ("part", TemporalKind::Time) => fixed || unit == DT::Epoch,
        ("part", TemporalKind::Duration) => !matches!(
            unit,
            DT::Week | DT::DayOfWeek | DT::IsoDayOfWeek | DT::DayOfYear | DT::IsoYear | DT::Offset
        ),
        _ => unreachable!("{operation}"),
    }
}

fn samples() -> Vec<(Temporal, Temporal)> {
    vec![
        (
            Temporal::Date(date("2023-08-02")),
            Temporal::Date(date("2021-01-31")),
        ),
        (
            Temporal::LocalDateTime(ldt("2023-08-02T09:30:15.25")),
            Temporal::LocalDateTime(ldt("2020-02-29T23:59")),
        ),
        (
            Temporal::DateTime(dt("2023-08-02T09:30:15.25-07:00")),
            Temporal::DateTime(dt("2020-02-29T23:59Z")),
        ),
        (
            Temporal::Time(time("09:30:15.25")),
            Temporal::Time(time("23:59")),
        ),
        (
            Temporal::Duration(dur("P1Y2M3DT4H5M6.25S")),
            Temporal::Duration(dur("-P3D")),
        ),
    ]
}

fn check<T: std::fmt::Debug>(
    operation: &str,
    kind: TemporalKind,
    unit: DT,
    result: TemporalResult<T>,
) {
    match (defined(operation, kind, unit), result) {
        (true, Ok(_)) => {}
        (
            false,
            Err(TemporalError::Unsupported {
                kind: name,
                operation: text,
            }),
        ) => {
            assert_eq!(name, kind.name());
            assert!(text.contains(unit.name()), "{text}");
        }
        (expected, got) => {
            panic!("{operation} {kind} {unit}: defined = {expected}, got {got:?}")
        }
    }
}

#[test]
fn every_unit_and_type_combination_is_defined_or_unsupported() {
    for (value, other) in samples() {
        let kind = value.kind();
        for &unit in DT::ALL {
            for n in [3, -3, 0] {
                check("add", kind, unit, add(value, unit, n, None));
            }
            check("trunc", kind, unit, trunc(value, unit, None));
            check("part", kind, unit, part(value, unit, None));
            check("diff", kind, unit, diff(value, other, unit, None));
            check("diff", kind, unit, diff(other, value, unit, None));
        }
    }
}

#[test]
fn a_zone_is_for_a_datetime_only() {
    let zone = berlin();
    for (value, other) in samples() {
        let results = [
            add(value, DT::Hour, 1, Some(&zone)).err(),
            trunc(value, DT::Day, Some(&zone)).err(),
            part(value, DT::Day, Some(&zone)).err(),
            diff(value, other, DT::Day, Some(&zone)).err(),
            add_duration(value, dur("P1D"), Some(&zone)).err(),
        ];
        if value.kind() == TemporalKind::DateTime {
            assert!(results.iter().all(Option::is_none), "{results:?}");
            assert!(age(value, other, Some(&zone)).is_ok());
        } else {
            for error in results {
                assert!(
                    matches!(error, Some(TemporalError::Unsupported { .. })),
                    "{value:?}: {error:?}"
                );
            }
        }
    }
}

#[test]
fn mixed_kinds_are_unsupported() {
    let samples = samples();
    for (a, _) in &samples {
        for (b, _) in &samples {
            if a.kind() == b.kind() {
                continue;
            }
            assert!(matches!(
                diff(*a, *b, DT::Day, None),
                Err(TemporalError::Unsupported { .. })
            ));
            assert!(matches!(
                diff_millis(*a, *b),
                Err(TemporalError::Unsupported { .. })
            ));
            assert!(matches!(
                age(*a, *b, None),
                Err(TemporalError::Unsupported { .. })
            ));
        }
    }
}

#[test]
fn pseudo_random_arithmetic_never_panics() {
    let mut rng = Rng(7);
    let zones = [None, Some(berlin()), Some(Zone::parse("-03:30").unwrap())];
    for _ in 0..20_000 {
        let micros = rng.range(MIN_MICROS, MAX_MICROS);
        let value = match rng.next() % 5 {
            0 => Temporal::Date(LocalDateTime::from_micros(micros).unwrap().date()),
            1 => Temporal::LocalDateTime(LocalDateTime::from_micros(micros).unwrap()),
            2 => match DateTime::from_micros(micros, rng.range(-64_800, 64_800) as i32) {
                Ok(v) => Temporal::DateTime(v),
                Err(_) => Temporal::DateTime(DateTime::from_micros(0, 0).unwrap()),
            },
            3 => Temporal::Time(LocalDateTime::from_micros(micros).unwrap().time()),
            _ => Temporal::Duration(Duration::new(
                rng.next() as i32,
                rng.next() as i32,
                rng.next() as i64,
            )),
        };
        let unit = DT::ALL[(rng.next() % DT::ALL.len() as u64) as usize];
        let n = match rng.next() % 4 {
            0 => rng.next() as i64,
            1 => i64::MAX,
            2 => i64::MIN,
            _ => rng.range(-1000, 1000),
        };
        let zone = zones[(rng.next() % 3) as usize].as_ref();
        let _ = add(value, unit, n, zone);
        let _ = trunc(value, unit, zone);
        let _ = part(value, unit, zone);
        let _ = diff(value, value, unit, zone);
        let _ = age(value, value, zone);
        let _ = add_duration(
            value,
            Duration::new(rng.next() as i32, rng.next() as i32, rng.next() as i64),
            zone,
        );
    }
}

// ---------------------------------------------------------------------------------------------
// The physical codec

#[test]
fn the_physical_form_is_the_canonical_text_and_lifts_back_by_kind() {
    use crate::value::ElementProperty;
    use crate::value::physical::{PhysicalValue, lift_temporal, lift_untyped, lower};
    let values = [
        Temporal::Date(date("2023-08-02")),
        Temporal::LocalDateTime(ldt("2023-08-02T09:30")),
        Temporal::DateTime(dt("2023-08-02T09:30:00-07:00")),
        Temporal::Time(time("09:30:00.5")),
        Temporal::Duration(dur("P1MT2H")),
    ];
    for value in values {
        let property = ElementProperty::Temporal(value);
        let physical = lower(&property);
        let PhysicalValue::String(text) = &physical else {
            panic!("{physical:?}");
        };
        assert_eq!(text.as_ref(), value.to_string());
        let ElementProperty::Temporal(back) = lift_temporal(&physical, value.kind()).unwrap()
        else {
            panic!("not temporal");
        };
        assert_same(back, value);
        // Without a declared type the text stays a string (rule 7).
        assert_eq!(
            lift_untyped(physical.clone()),
            ElementProperty::from(value.to_string())
        );
    }
    assert!(lift_temporal(&PhysicalValue::Int(5), TemporalKind::DateTime).is_err());
    let date_text = PhysicalValue::String("2023-08-02".into());
    assert!(lift_temporal(&date_text, TemporalKind::Time).is_err());
}

// ---------------------------------------------------------------------------------------------
// The GQL helpers (3g.T3): the extended parser, the Neo4j text, the fraction cascade and
// `duration.between`. The expected texts are the openCypher TCK's (Temporal1, 2, 6, 8, 10).
// ---------------------------------------------------------------------------------------------

mod gql_helpers {
    use super::super::arith::{
        Amount, Cascaded, DurationParts, between_neo4j, cascade, scale_duration,
    };
    use super::super::format::neo4j;
    use super::super::parse::{Extended, parse_extended};
    use super::*;

    /// The Neo4j text of what `text` parses to as a `kind`; panics on a parse error.
    fn ext(kind: TemporalKind, text: &str) -> String {
        match parse_extended(kind, text) {
            Ok(parsed) => neo4j(&parsed.value),
            Err(e) => panic!("{kind} {text:?}: {e}"),
        }
    }

    fn rows(kind: TemporalKind, cases: &[(&str, &str)]) {
        for (text, expected) in cases {
            assert_eq!(ext(kind, text), *expected, "{kind} {text:?}");
        }
    }

    #[test]
    fn dates_in_every_iso_form() {
        rows(
            TemporalKind::Date,
            &[
                ("2015-07-21", "2015-07-21"),
                ("20150721", "2015-07-21"),
                ("2015-07", "2015-07-01"),
                ("201507", "2015-07-01"),
                ("2015-W30-2", "2015-07-21"),
                ("2015W302", "2015-07-21"),
                ("2015-W30", "2015-07-20"),
                ("2015W30", "2015-07-20"),
                ("2015-202", "2015-07-21"),
                ("2015202", "2015-07-21"),
                ("2015", "2015-01-01"),
                ("2015-Q3-21", "2015-07-21"),
                ("2015Q3", "2015-07-01"),
                ("1817-W01-1", "1816-12-30"),
                ("+2015-07-21", "2015-07-21"),
                ("-0044-03-15", "-0044-03-15"),
                ("+0000-01-01", "0000-01-01"),
            ],
        );
    }

    #[test]
    fn local_times_and_date_times_in_every_iso_form() {
        rows(
            TemporalKind::Time,
            &[
                ("21:40:32.142", "21:40:32.142"),
                ("214032.142", "21:40:32.142"),
                ("21:40:32", "21:40:32"),
                ("214032", "21:40:32"),
                ("21:40", "21:40"),
                ("2140", "21:40"),
                ("21", "21:00"),
                ("00:00:00.000001", "00:00:00.000001"),
                ("12:31:14,5", "12:31:14.500"),
            ],
        );
        rows(
            TemporalKind::LocalDateTime,
            &[
                ("2015-07-21T21:40:32.142", "2015-07-21T21:40:32.142"),
                ("2015-W30-2T214032.142", "2015-07-21T21:40:32.142"),
                ("2015-202T21:40:32", "2015-07-21T21:40:32"),
                ("2015T214032", "2015-01-01T21:40:32"),
                ("20150721T21:40", "2015-07-21T21:40"),
                ("2015-W30T2140", "2015-07-20T21:40"),
                ("2015202T21", "2015-07-21T21:00"),
                ("2015-07-21", "2015-07-21T00:00"),
            ],
        );
        rows(
            TemporalKind::DateTime,
            &[
                (
                    "2015-07-21T21:40:32.142+0100",
                    "2015-07-21T21:40:32.142+01:00",
                ),
                ("2015-W30-2T214032.142Z", "2015-07-21T21:40:32.142Z"),
                ("2015-202T21:40:32+01:00", "2015-07-21T21:40:32+01:00"),
                ("2015T214032-0100", "2015-01-01T21:40:32-01:00"),
                ("20150721T21:40-01:30", "2015-07-21T21:40-01:30"),
                ("2015-W30T2140-00:00", "2015-07-20T21:40Z"),
                ("2015-W30T2140-02", "2015-07-20T21:40-02:00"),
                ("2015202T21+18:00", "2015-07-21T21:00+18:00"),
                ("2015-07-21T21:40:32.142", "2015-07-21T21:40:32.142Z"),
                // A zone name is resolved to its offset at that time (the name is not kept).
                (
                    "2015-07-21T21:40:32.142+02:00[Europe/Stockholm]",
                    "2015-07-21T21:40:32.142+02:00",
                ),
                (
                    "2015-07-21T21:40:32.142+0845[Australia/Eucla]",
                    "2015-07-21T21:40:32.142+08:45",
                ),
                (
                    "2015-07-21T21:40:32.142-04[America/New_York]",
                    "2015-07-21T21:40:32.142-04:00",
                ),
                (
                    "2015-07-21T21:40:32.142[Europe/London]",
                    "2015-07-21T21:40:32.142+01:00",
                ),
                (
                    "1818-07-21T21:40:32.142[Europe/Stockholm]",
                    "1818-07-21T21:40:32.142+00:53:28",
                ),
            ],
        );
        let named = parse_extended(TemporalKind::DateTime, "2015-07-21T21:40[Europe/London]");
        assert!(named.is_ok_and(|p| p.zone_name && !p.sub_micro));
        let fixed = parse_extended(TemporalKind::DateTime, "2015-07-21T21:40+01:00");
        assert!(fixed.is_ok_and(|p| !p.zone_name));
    }

    #[test]
    fn digits_below_the_microsecond_are_reported_never_kept() {
        let parsed = parse_extended(TemporalKind::Time, "12:31:14.645876123");
        let Ok(Extended {
            value, sub_micro, ..
        }) = parsed
        else {
            panic!("{parsed:?}");
        };
        assert!(sub_micro);
        assert_eq!(neo4j(&value), "12:31:14.645876");
        for (kind, text) in [
            (TemporalKind::Time, "12:31:14.645876000"),
            (TemporalKind::Time, "12:31:14.6"),
            (TemporalKind::LocalDateTime, "1984-10-11T12:31:14.645876"),
            (TemporalKind::Duration, "PT0.000001S"),
        ] {
            assert!(
                parse_extended(kind, text).is_ok_and(|p| !p.sub_micro),
                "{text}"
            );
        }
        for (kind, text) in [
            (TemporalKind::LocalDateTime, "1984-10-11T12:31:14.645876123"),
            (TemporalKind::DateTime, "1984-10-11T12:31:14.000000001Z"),
            (TemporalKind::Duration, "PT0.000000001S"),
            (TemporalKind::Duration, "P0.000000001D"),
        ] {
            assert!(
                parse_extended(kind, text).is_ok_and(|p| p.sub_micro),
                "{text}"
            );
        }
    }

    #[test]
    fn durations_with_fractions_and_the_alternative_form() {
        rows(
            TemporalKind::Duration,
            &[
                ("P14DT16H12M", "P14DT16H12M"),
                ("P5M1.5D", "P5M1DT12H"),
                ("P0.75M", "P22DT19H51M49.5S"),
                ("PT0.75M", "PT45S"),
                ("P2.5W", "P17DT12H"),
                ("P12Y5M14DT16H12M70S", "P12Y5M14DT16H13M10S"),
                ("P2012-02-02T14:37:21.545", "P2012Y2M2DT14H37M21.545S"),
                ("PT-1.999S", "PT-1.999S"),
                ("PT-1M-0.001S", "PT-1M-0.001S"),
                ("P12Y5M-14DT16H", "P12Y5M-14DT16H"),
                ("-P1DT2H", "P-1DT-2H"),
                ("+P1D", "P1D"),
                ("P0.5Y", "P6M"),
                ("PT1,5H", "PT1H30M"),
                ("PT0S", "PT0S"),
                ("p1dt1s", "P1DT1S"),
            ],
        );
        for bad in [
            "",
            "P",
            "PT",
            "1D",
            "P1",
            "P1X",
            "PD",
            "P1.D",
            "P1D1Y",
            "P1H",
            "PT1D",
            "P0.0000000001D",
            "P2012-02-02",
            "P2012-13-02T00:00:00",
            "P1DT",
            "PT1S1M",
        ] {
            assert!(
                parse_extended(TemporalKind::Duration, bad).is_err(),
                "{bad:?}"
            );
        }
        assert!(matches!(
            parse_extended(TemporalKind::Duration, "P9999999999999Y"),
            Err(TemporalError::OutOfRange { .. })
        ));
    }

    #[test]
    fn bad_extended_texts_are_errors() {
        for (kind, bad) in [
            (TemporalKind::Date, ""),
            (TemporalKind::Date, "2015-"),
            (TemporalKind::Date, "201"),
            (TemporalKind::Date, "2015-13"),
            (TemporalKind::Date, "2015-02-30"),
            (TemporalKind::Date, "2015-W54"),
            (TemporalKind::Date, "2015-W30-8"),
            (TemporalKind::Date, "2015-366"),
            (TemporalKind::Date, "2015-Q5"),
            (TemporalKind::Date, "2015-07-21T10:00"),
            (TemporalKind::Date, "20150721x"),
            (TemporalKind::Date, "2015-0"),
            (TemporalKind::Time, "24:00"),
            (TemporalKind::Time, "12:60"),
            (TemporalKind::Time, "1"),
            (TemporalKind::Time, "12:"),
            (TemporalKind::Time, "12:00:00."),
            (TemporalKind::Time, "12:00:00.1234567890"),
            (TemporalKind::Time, "2140-02"),
            (TemporalKind::Time, "21:40Z"),
            (TemporalKind::LocalDateTime, "2015-07-21T21:40Z"),
            (TemporalKind::LocalDateTime, "2015-07-21T"),
            (TemporalKind::DateTime, "2015-07-21T21:40+19:00"),
            (TemporalKind::DateTime, "2015-07-21T21:40[Mars/Base]"),
            (
                TemporalKind::DateTime,
                "2015-07-21T21:40+05:00[Europe/London]",
            ),
        ] {
            assert!(parse_extended(kind, bad).is_err(), "{kind} {bad:?}");
        }
        // A well-formed year outside the range is a range error, not a parse error.
        for text in ["-999999999-01-01", "+10000-01-01", "+20150721"] {
            assert!(
                matches!(
                    parse_extended(TemporalKind::Date, text),
                    Err(TemporalError::OutOfRange { .. })
                ),
                "{text}"
            );
        }
    }

    /// Every canonical text the strict parsers read gives the same value here, and the Neo4j
    /// text of every value reads back to it exactly (also by the canonical parser).
    #[test]
    fn the_extended_parser_includes_the_canonical_forms_and_reads_the_neo4j_text() {
        let mut rng = Rng(0x5EED_3637);
        let mut values: Vec<Temporal> = Vec::new();
        for _ in 0..4000 {
            let micros = rng.range(MIN_MICROS, MAX_MICROS);
            let micros = match rng.range(0, 3) {
                0 => micros - micros.rem_euclid(MICROS_PER_SECOND),
                1 => micros - micros.rem_euclid(60 * MICROS_PER_SECOND),
                _ => micros,
            };
            let local = LocalDateTime::from_micros(micros).unwrap();
            values.push(Temporal::LocalDateTime(local));
            values.push(Temporal::Date(local.date()));
            values.push(Temporal::Time(local.time()));
            let offset = rng.range(-18 * 3600, 18 * 3600) as i32;
            if let Ok(value) = DateTime::from_local(local, offset) {
                values.push(Temporal::DateTime(value));
            }
            let months = rng.range(-30_000, 30_000) as i32;
            let days = rng.range(-30_000, 30_000) as i32;
            let micros = rng.range(-(1 << 50), 1 << 50);
            values.push(Temporal::Duration(Duration::new(months, days, micros)));
        }
        for value in values {
            let canonical = value.to_string();
            let parsed = parse_extended(value.kind(), &canonical)
                .unwrap_or_else(|e| panic!("{canonical}: {e}"));
            assert_same(parsed.value, value);
            assert!(!parsed.sub_micro && !parsed.zone_name);
            let text = neo4j(&value);
            let back =
                parse_extended(value.kind(), &text).unwrap_or_else(|e| panic!("{text}: {e}"));
            assert_same(back.value, value);
            assert_same(Temporal::parse_as(value.kind(), &text).unwrap(), value);
        }
    }

    #[test]
    fn neo4j_text_differs_from_the_canonical_text_only_in_its_forms() {
        let cases: [(Temporal, &str, &str); 9] = [
            (time("12:00").into(), "12:00", "12:00:00"),
            (time("12:31:14").into(), "12:31:14", "12:31:14"),
            (time("00:00:00.25").into(), "00:00:00.250", "00:00:00.250"),
            (
                ldt("1984-10-11T12:31").into(),
                "1984-10-11T12:31",
                "1984-10-11T12:31:00",
            ),
            (
                dt("1984-10-11T12:31Z").into(),
                "1984-10-11T12:31Z",
                "1984-10-11T12:31:00Z",
            ),
            (
                dt("1984-10-11T12:31:14+02:05:59").into(),
                "1984-10-11T12:31:14+02:05:59",
                "1984-10-11T12:31:14+02:05:59",
            ),
            (
                dur("PT23H59M59.9S").into(),
                "PT23H59M59.9S",
                "PT23H59M59.900S",
            ),
            (dur("-PT1M0.001S").into(), "PT-1M-0.001S", "-PT1M0.001S"),
            (dur("-P1YT4M50S").into(), "P-1YT-4M-50S", "-P1YT4M50S"),
        ];
        for (value, neo4j_text, canonical) in cases {
            assert_eq!(neo4j(&value), neo4j_text);
            assert_eq!(value.to_string(), canonical);
        }
        assert_eq!(neo4j(&dur("P1M-2D").into()), "P1M-2D");
        assert_eq!(neo4j(&dur("PT8784H").into()), "PT8784H");
        assert_eq!(neo4j(&dur("PT0S").into()), "PT0S");
        assert_eq!(neo4j(&dur("PT0.000001S").into()), "PT0.000001S");
        assert_eq!(neo4j(&date("-0044-03-15").into()), "-0044-03-15");
    }

    fn whole(n: i64) -> Amount {
        Amount::from_i64(n)
    }

    fn real(x: f64) -> Amount {
        Amount::from_f64(x).unwrap()
    }

    fn cascaded(parts: DurationParts) -> String {
        let c = cascade(&parts).unwrap();
        assert!(!c.sub_micro);
        neo4j(&c.duration.into())
    }

    #[test]
    fn the_fraction_cascade_is_neo4js() {
        // Temporal1 [12], without the nanosecond row.
        let p = DurationParts::default;
        assert_eq!(
            cascaded(DurationParts {
                days: whole(14),
                hours: whole(16),
                minutes: whole(12),
                ..p()
            }),
            "P14DT16H12M"
        );
        assert_eq!(
            cascaded(DurationParts {
                months: whole(5),
                days: real(1.5),
                ..p()
            }),
            "P5M1DT12H"
        );
        assert_eq!(
            cascaded(DurationParts {
                months: real(0.75),
                ..p()
            }),
            "P22DT19H51M49.5S"
        );
        assert_eq!(
            cascaded(DurationParts {
                weeks: real(2.5),
                ..p()
            }),
            "P17DT12H"
        );
        assert_eq!(
            cascaded(DurationParts {
                years: whole(12),
                months: whole(5),
                days: whole(14),
                hours: whole(16),
                minutes: whole(12),
                seconds: whole(70),
                ..p()
            }),
            "P12Y5M14DT16H13M10S"
        );
        assert_eq!(
            cascaded(DurationParts {
                days: whole(14),
                seconds: whole(70),
                milliseconds: whole(1),
                ..p()
            }),
            "P14DT1M10.001S"
        );
        assert_eq!(
            cascaded(DurationParts {
                days: whole(14),
                seconds: whole(70),
                microseconds: whole(1),
                ..p()
            }),
            "P14DT1M10.000001S"
        );
        assert_eq!(
            cascaded(DurationParts {
                minutes: real(1.5),
                seconds: whole(1),
                ..p()
            }),
            "PT1M31S"
        );
        // Negative fractions cascade toward zero; a fraction of a year becomes months.
        assert_eq!(
            cascaded(DurationParts {
                days: real(-1.5),
                ..p()
            }),
            "P-1DT-12H"
        );
        assert_eq!(
            cascaded(DurationParts {
                years: real(12.5),
                months: real(5.5),
                ..p()
            }),
            "P12Y11M15DT5H14M33S"
        );
        // A nanosecond is reported, not kept.
        let ns = cascade(&DurationParts {
            days: whole(14),
            seconds: whole(70),
            nanoseconds: whole(1),
            ..p()
        })
        .unwrap();
        assert!(ns.sub_micro);
        assert_eq!(neo4j(&ns.duration.into()), "P14DT1M10S");
        // Beyond the fields.
        assert!(
            cascade(&DurationParts {
                months: whole(i64::MAX),
                ..p()
            })
            .is_err()
        );
        assert!(
            cascade(&DurationParts {
                hours: whole(i64::MAX),
                ..p()
            })
            .is_err()
        );
        assert_eq!(Amount::from_f64(f64::NAN), None);
        assert_eq!(Amount::from_f64(f64::INFINITY), None);
        assert_eq!(Amount::from_f64(1e19), None);
        assert_eq!(real(0.1).nano_units(), 100_000_000);
        assert_eq!(Amount::from_nano_units(1).nano_units(), 1);
        assert!(real(2.0).is_whole() && !real(2.5).is_whole());
    }

    #[test]
    fn durations_scale_by_numbers_as_neo4j() {
        let d = dur("P12Y5M14DT16H13M10S");
        let text = |c: Cascaded| neo4j(&c.duration.into());
        // Temporal8 [7], without the nanosecond.
        for (factor, divide, expected) in [
            (whole(1), false, "P12Y5M14DT16H13M10S"),
            (whole(2), false, "P24Y10M28DT32H26M20S"),
            (real(0.5), false, "P6Y2M22DT13H21M8S"),
            (whole(1), true, "P12Y5M14DT16H13M10S"),
            (whole(2), true, "P6Y2M22DT13H21M8S"),
            (real(0.5), true, "P24Y10M28DT32H26M20S"),
        ] {
            assert_eq!(text(scale_duration(d, factor, divide).unwrap()), expected);
        }
        assert_eq!(
            text(scale_duration(dur("P1M"), real(0.5), false).unwrap()),
            "P15DT5H14M33S"
        );
        assert_eq!(
            text(scale_duration(dur("P1D"), whole(-3), true).unwrap()),
            "PT-8H"
        );
        assert!(scale_duration(d, whole(0), true).is_err());
        assert!(scale_duration(d, whole(i64::MAX), false).is_err());
        let third = scale_duration(dur("PT1S"), whole(3), true).unwrap();
        assert!(third.sub_micro);
        assert_eq!(text(third), "PT0.333333S");
    }

    #[test]
    fn duration_between_is_neo4js() {
        let between = |a: Temporal, b: Temporal| neo4j(&between_neo4j(a, b).unwrap().into());
        // Temporal10 [1] and [2], the rows of one type.
        let local = |a: &str, b: &str| between(ldt(a).into(), ldt(b).into());
        assert_eq!(local("2018-01-01T12:00", "2018-01-02T10:00"), "PT22H");
        assert_eq!(local("2018-01-02T10:00", "2018-01-01T12:00"), "PT-22H");
        assert_eq!(
            local("2018-01-01T10:00:00.2", "2018-01-02T10:00:00.1"),
            "PT23H59M59.9S"
        );
        assert_eq!(
            local("2018-01-02T10:00:00.1", "2018-01-01T10:00:00.2"),
            "PT-23H-59M-59.9S"
        );
        assert_eq!(
            local("2015-07-21T21:40:32.142", "2016-07-21T21:45:22.142"),
            "P1YT4M50S"
        );
        assert_eq!(
            local("2015-07-21T21:40:32.142", "2015-06-24T00:00"),
            "P-27DT-21H-40M-32.142S"
        );
        assert_eq!(
            between(
                dt("2017-10-28T23:00+02:00").into(),
                dt("2017-10-29T04:00+01:00").into()
            ),
            "PT6H"
        );
        assert_eq!(
            between(
                dt("2014-07-21T21:40:36.143+02:00").into(),
                dt("2015-07-21T21:40:32.142+01:00").into()
            ),
            "P1YT59M55.999S"
        );
        assert_eq!(
            between(date("1984-10-11").into(), date("2015-06-24").into()),
            "P30Y8M13D"
        );
        assert_eq!(between(time("14:30").into(), time("16:30").into()), "PT2H");
        assert_eq!(between(time("16:30").into(), time("14:30").into()), "PT-2H");
        // Month ends: January 31 to February 28 is 28 days, not a month.
        assert_eq!(
            between(date("2023-01-31").into(), date("2023-02-28").into()),
            "P28D"
        );
        assert_eq!(
            between(date("2023-01-31").into(), date("2023-03-01").into()),
            "P1M1D"
        );
        // Mixed types are aligned by the caller; durations have no between.
        assert!(between_neo4j(date("2023-01-31").into(), time("10:00").into()).is_err());
        assert!(between_neo4j(dur("P1D").into(), dur("P2D").into()).is_err());
    }
}
