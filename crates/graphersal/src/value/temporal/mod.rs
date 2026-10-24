//! Dates and times: the five temporal value types and their calendar, text, arithmetic and time
//! zone operations (book "Dates and Times").
//!
//! | Type | Holds | Text |
//! |---|---|---|
//! | [`Date`] | a calendar day | `2023-08-02` |
//! | [`LocalDateTime`] | a wall-clock date and time, no zone | `2023-08-02T09:30:00` |
//! | [`DateTime`] | an instant plus the UTC offset it was written with | `2023-08-02T09:30:00+02:00` |
//! | [`Time`] | a time of day, no zone | `09:30:00` |
//! | [`Duration`] | months, days and nanoseconds (PostgreSQL `interval`) | `P1M2DT3H` |
//!
//! Every type with a time has a resolution of one nanosecond (as `java.time` and TinkerPop) and
//! the calendar types cover the years -9999 to 9999 of the proleptic Gregorian calendar. Values
//! are plain integers (no allocation, `Copy`); the IANA time zone rules come from a bundled
//! database, so every machine gives the same answer.
//!
//! **Equality.** A [`DateTime`] compares by its instant only: `12:00:00Z` equals
//! `14:00:00+02:00` (TinkerPop: "dates are evaluated based on the numerical comparison of Unix
//! Epoch time"; PostgreSQL `timestamptz`); the offset is kept and shown but never compared. A
//! [`Duration`] compares field by field (`P1M` is not `P30D`: they differ when added to
//! January 31), and orders by its length with a month counted as 30 days and a day as 24 hours,
//! ties broken by the fields.

use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::str::FromStr;

// The date steps (`date_add`, `date_diff`, `date_trunc`, `date_part`, `age`) call these.
pub(crate) mod arith;
pub(crate) mod civil;
// `format::neo4j` and `parse::parse_extended` are the GQL text forms; the rest stays private.
pub(crate) mod format;
pub(crate) mod parse;
#[cfg_attr(not(any(feature = "serde", feature = "io")), allow(dead_code))]
// ChangeSet serde, GraphSON.
pub(crate) mod tags;
#[cfg(test)]
mod tests;
pub(crate) mod zone;

use civil::{MICROS_PER_DAY, NANOS_PER_DAY, NANOS_PER_SECOND};

/// A failed temporal operation: unparsable text, a result outside the supported range, an unknown
/// time zone or an operation a type does not have.
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum TemporalError {
    /// The text is not a value of the expected type.
    #[error("cannot parse {input} as a {expected}: {reason}")]
    #[non_exhaustive]
    Parse {
        /// The text (cut after 64 characters), quoted.
        input: String,
        /// The expected type (`datetime`, `date`, ...).
        expected: &'static str,
        /// What is wrong with it.
        reason: &'static str,
    },
    /// A value or a result lies outside the supported range.
    #[error("{what} is outside the supported range (the years -9999 to 9999)")]
    #[non_exhaustive]
    OutOfRange {
        /// What went out of range.
        what: &'static str,
    },
    /// The name is not a time zone of the bundled IANA database, nor a UTC offset.
    #[error("unknown time zone {name}")]
    #[non_exhaustive]
    UnknownZone {
        /// The name (cut after 64 characters), quoted.
        name: String,
    },
    /// The operation is not defined for the value's type (adding hours to a date, ...).
    #[error("{operation} is not defined for a {kind}")]
    #[non_exhaustive]
    Unsupported {
        /// The operation (`adding hours`, `the part hour`, ...).
        operation: String,
        /// The type it was applied to.
        kind: &'static str,
    },
}

impl TemporalError {
    pub(crate) fn parse(input: &str, expected: &'static str, reason: &'static str) -> Self {
        TemporalError::Parse {
            input: quoted(input),
            expected,
            reason,
        }
    }

    pub(crate) fn out_of_range(what: &'static str) -> Self {
        TemporalError::OutOfRange { what }
    }

    pub(crate) fn unknown_zone(name: &str) -> Self {
        TemporalError::UnknownZone { name: quoted(name) }
    }

    pub(crate) fn unsupported(operation: impl Into<String>, kind: TemporalKind) -> Self {
        TemporalError::Unsupported {
            operation: operation.into(),
            kind: kind.name(),
        }
    }
}

/// Quotes user text for an error message, cut after 64 characters.
fn quoted(text: &str) -> String {
    const MAX: usize = 64;
    match text.char_indices().nth(MAX) {
        Some((cut, _)) => format!("\"{}…\" ({} chars)", &text[..cut], text.chars().count()),
        None => format!("\"{text}\""),
    }
}

/// The result type of temporal operations.
pub type TemporalResult<T> = Result<T, TemporalError>;

/// The five temporal types, in their order between each other (orderability).
///
/// Closed by definition is NOT claimed: a later type (a zoned date-time) may be added.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum TemporalKind {
    /// [`Date`].
    Date,
    /// [`LocalDateTime`].
    LocalDateTime,
    /// [`DateTime`].
    DateTime,
    /// [`Time`].
    Time,
    /// [`Duration`].
    Duration,
}

impl TemporalKind {
    /// The type's name in diagnostics and schemas (`date`, `localdatetime`, `datetime`, `time`,
    /// `duration`).
    pub fn name(self) -> &'static str {
        match self {
            TemporalKind::Date => "date",
            TemporalKind::LocalDateTime => "localdatetime",
            TemporalKind::DateTime => "datetime",
            TemporalKind::Time => "time",
            TemporalKind::Duration => "duration",
        }
    }
}

impl fmt::Display for TemporalKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A calendar day without a time or a zone (PostgreSQL `date`, Python `date`).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Date {
    days: i32,
}

/// A signed 96-bit integer of nanoseconds kept as three 32-bit words: nanoseconds over the
/// years -9999 to 9999 (about ±3.2e20) do not fit an `i64`, and an `i128` field would double the
/// alignment and the size of every temporal value. Its alignment of 4 keeps a [`Duration`] at 20
/// bytes and so a [`Temporal`] at 24 (an `ElementProperty` stays at 32). The words are a fixed
/// function of the value, so the derived equality and hash are those of the value; ordering goes
/// through [`get`](Self::get).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
struct Nanos96 {
    lo: u32,
    mid: u32,
    hi: i32,
}

impl Nanos96 {
    const ZERO: Nanos96 = Nanos96 {
        lo: 0,
        mid: 0,
        hi: 0,
    };

    /// The value `value`, which every caller has range-checked (well within 96 bits).
    const fn new(value: i128) -> Nanos96 {
        Nanos96 {
            lo: value as u32,
            mid: (value >> 32) as u32,
            hi: (value >> 64) as i32,
        }
    }

    /// The value.
    const fn get(self) -> i128 {
        ((self.hi as i128) << 64) | ((self.mid as i128) << 32) | self.lo as i128
    }
}

/// A time of day without a date or a zone, `00:00:00` to `23:59:59.999999999` (PostgreSQL
/// `time`, Python `time`, `java.time.LocalTime`).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Time {
    /// Nanoseconds after midnight, `0..86_400_000_000_000`.
    nanos: i64,
}

/// A wall-clock date and time without a zone (PostgreSQL `timestamp`, Python naive `datetime`,
/// `java.time.LocalDateTime`).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct LocalDateTime {
    /// Nanoseconds after 1970-01-01T00:00:00 (wall clock).
    nanos: Nanos96,
}

/// An instant plus the UTC offset it was written with (TinkerPop `datetime`, Java
/// `OffsetDateTime`, SQL `TIMESTAMP WITH TIME ZONE`, Python aware `datetime`).
///
/// Equality, hashing and ordering use the instant only; see the module documentation.
#[derive(Clone, Copy)]
pub struct DateTime {
    /// Nanoseconds since 1970-01-01T00:00:00Z.
    nanos: Nanos96,
    /// The offset from UTC in seconds, within ±18 hours.
    offset_seconds: i32,
}

/// A length of time in months, days and nanoseconds, each with its own sign (PostgreSQL
/// `interval`): months and days stay calendar units, so `P1M` added to January 31 gives the end
/// of February and `P1D` across a daylight saving change in a zone keeps the wall-clock time.
///
/// The time part holds what fits an `i64` of microseconds, about ±292,000 years, at nanosecond
/// resolution.
///
/// Equality is field by field; ordering is by the length (a month = 30 days, a day = 24 hours),
/// ties broken by the fields.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Duration {
    months: i32,
    days: i32,
    nanos: Nanos96,
}

/// A temporal value of any of the five types.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Temporal {
    /// A calendar day.
    Date(Date),
    /// A wall-clock date and time.
    LocalDateTime(LocalDateTime),
    /// An instant with its offset.
    DateTime(DateTime),
    /// A time of day.
    Time(Time),
    /// A length of time.
    Duration(Duration),
}

/// The largest UTC offset, ±18 hours (as `java.time.ZoneOffset`).
pub(crate) const MAX_OFFSET_SECONDS: i32 = 18 * 3600;

/// Nanoseconds in a microsecond.
const NANOS_PER_MICRO: i128 = 1_000;

/// The smallest time part of a [`Duration`] in nanoseconds (`i64::MIN` microseconds).
const MIN_DURATION_NANOS: i128 = i64::MIN as i128 * NANOS_PER_MICRO;
/// The largest time part of a [`Duration`] in nanoseconds (`i64::MAX` microseconds and 999
/// nanoseconds).
const MAX_DURATION_NANOS: i128 = i64::MAX as i128 * NANOS_PER_MICRO + 999;

impl Date {
    /// The day `year-month-day`; `OutOfRange` for an impossible or unsupported date.
    pub fn from_ymd(year: i32, month: u8, day: u8) -> TemporalResult<Date> {
        if !(civil::MIN_YEAR..=civil::MAX_YEAR).contains(&year)
            || !(1..=12).contains(&month)
            || day == 0
            || day > civil::days_in_month(year, month)
        {
            return Err(TemporalError::out_of_range("the date"));
        }
        Ok(Date {
            days: civil::days_from_civil(year, month, day),
        })
    }

    /// The day `days` days after 1970-01-01 (negative before it).
    pub fn from_days_since_epoch(days: i32) -> TemporalResult<Date> {
        if civil::day_in_range(days as i64) {
            Ok(Date { days })
        } else {
            Err(TemporalError::out_of_range("the date"))
        }
    }

    /// Days since 1970-01-01.
    pub fn days_since_epoch(self) -> i32 {
        self.days
    }

    /// The year (-9999 to 9999; 0 is 1 BC).
    pub fn year(self) -> i32 {
        civil::civil_from_days(self.days).0
    }

    /// The month, 1-12.
    pub fn month(self) -> u8 {
        civil::civil_from_days(self.days).1
    }

    /// The day of the month, 1-31.
    pub fn day(self) -> u8 {
        civil::civil_from_days(self.days).2
    }
}

impl Time {
    /// Midnight.
    pub(crate) const MIDNIGHT: Time = Time { nanos: 0 };

    /// The time `hour:minute:second.micro`; `OutOfRange` for an impossible time.
    pub fn from_hms_micro(hour: u8, minute: u8, second: u8, micro: u32) -> TemporalResult<Time> {
        if micro > 999_999 {
            return Err(TemporalError::out_of_range("the time"));
        }
        Time::from_hms_nano(hour, minute, second, micro * 1000)
    }

    /// The time `hour:minute:second.nano`; `OutOfRange` for an impossible time.
    pub fn from_hms_nano(hour: u8, minute: u8, second: u8, nano: u32) -> TemporalResult<Time> {
        if hour > 23 || minute > 59 || second > 59 || nano > 999_999_999 {
            return Err(TemporalError::out_of_range("the time"));
        }
        Ok(Time {
            nanos: ((hour as i64 * 60 + minute as i64) * 60 + second as i64) * NANOS_PER_SECOND
                + nano as i64,
        })
    }

    /// The time `micros` microseconds after midnight (`0..86_400_000_000`).
    pub fn from_micros_of_day(micros: i64) -> TemporalResult<Time> {
        if (0..MICROS_PER_DAY).contains(&micros) {
            Ok(Time {
                nanos: micros * 1000,
            })
        } else {
            Err(TemporalError::out_of_range("the time"))
        }
    }

    /// The time `nanos` nanoseconds after midnight (`0..86_400_000_000_000`).
    pub fn from_nanos_of_day(nanos: i64) -> TemporalResult<Time> {
        if (0..NANOS_PER_DAY).contains(&nanos) {
            Ok(Time { nanos })
        } else {
            Err(TemporalError::out_of_range("the time"))
        }
    }

    /// Microseconds after midnight, the nanoseconds below them cut.
    pub fn micros_of_day(self) -> i64 {
        self.nanos / 1000
    }

    /// Nanoseconds after midnight.
    pub fn nanos_of_day(self) -> i64 {
        self.nanos
    }

    /// The hour, 0-23.
    pub fn hour(self) -> u8 {
        (self.nanos / (3600 * NANOS_PER_SECOND)) as u8
    }

    /// The minute, 0-59.
    pub fn minute(self) -> u8 {
        (self.nanos / (60 * NANOS_PER_SECOND) % 60) as u8
    }

    /// The second, 0-59.
    pub fn second(self) -> u8 {
        (self.nanos / NANOS_PER_SECOND % 60) as u8
    }

    /// The whole microseconds within the second, 0-999999 (the nanoseconds below them cut).
    pub fn microsecond(self) -> u32 {
        (self.nanos % NANOS_PER_SECOND / 1000) as u32
    }

    /// The nanoseconds within the second, 0-999999999.
    pub fn nanosecond(self) -> u32 {
        (self.nanos % NANOS_PER_SECOND) as u32
    }
}

impl LocalDateTime {
    /// The date at the time.
    pub fn new(date: Date, time: Time) -> LocalDateTime {
        LocalDateTime {
            nanos: Nanos96::new(date.days as i128 * NANOS_PER_DAY as i128 + time.nanos as i128),
        }
    }

    /// The wall-clock time `micros` microseconds after 1970-01-01T00:00:00.
    pub fn from_micros(micros: i64) -> TemporalResult<LocalDateTime> {
        LocalDateTime::from_nanos(micros as i128 * NANOS_PER_MICRO)
    }

    /// The wall-clock time `nanos` nanoseconds after 1970-01-01T00:00:00.
    pub fn from_nanos(nanos: i128) -> TemporalResult<LocalDateTime> {
        if civil::nanos_in_range(nanos) {
            Ok(LocalDateTime {
                nanos: Nanos96::new(nanos),
            })
        } else {
            Err(TemporalError::out_of_range("the local date-time"))
        }
    }

    /// Microseconds after 1970-01-01T00:00:00 (wall clock, no zone), rounded down.
    pub fn micros(self) -> i64 {
        // In range by construction: the years -9999..9999 fit an i64 of microseconds.
        self.nanos.get().div_euclid(NANOS_PER_MICRO) as i64
    }

    /// Nanoseconds after 1970-01-01T00:00:00 (wall clock, no zone).
    pub fn nanos(self) -> i128 {
        self.nanos.get()
    }

    /// The date part.
    pub fn date(self) -> Date {
        Date {
            days: civil::split_nanos(self.nanos.get()).0 as i32,
        }
    }

    /// The time part.
    pub fn time(self) -> Time {
        Time {
            nanos: civil::split_nanos(self.nanos.get()).1,
        }
    }
}

impl PartialOrd for LocalDateTime {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for LocalDateTime {
    fn cmp(&self, other: &Self) -> Ordering {
        self.nanos.get().cmp(&other.nanos.get())
    }
}

impl DateTime {
    /// 1970-01-01T00:00:00Z.
    #[cfg_attr(not(feature = "script"), allow(dead_code))]
    pub(crate) const EPOCH: DateTime = DateTime {
        nanos: Nanos96::ZERO,
        offset_seconds: 0,
    };

    /// The instant `micros` microseconds after 1970-01-01T00:00:00Z, written with the offset
    /// `offset_seconds` (within ±18 hours). Both the instant and its local time must lie in the
    /// supported years.
    pub fn from_micros(micros: i64, offset_seconds: i32) -> TemporalResult<DateTime> {
        DateTime::from_nanos(micros as i128 * NANOS_PER_MICRO, offset_seconds)
    }

    /// The instant `nanos` nanoseconds after 1970-01-01T00:00:00Z, written with the offset
    /// `offset_seconds` (within ±18 hours). Both the instant and its local time must lie in the
    /// supported years.
    pub fn from_nanos(nanos: i128, offset_seconds: i32) -> TemporalResult<DateTime> {
        if offset_seconds.abs() > MAX_OFFSET_SECONDS {
            return Err(TemporalError::out_of_range(
                "the UTC offset (at most ±18:00)",
            ));
        }
        let local = nanos.checked_add(offset_seconds as i128 * NANOS_PER_SECOND as i128);
        match local {
            Some(local) if civil::nanos_in_range(nanos) && civil::nanos_in_range(local) => {
                Ok(DateTime {
                    nanos: Nanos96::new(nanos),
                    offset_seconds,
                })
            }
            _ => Err(TemporalError::out_of_range("the date-time")),
        }
    }

    /// The instant `millis` milliseconds after the Unix epoch, in UTC (TinkerPop
    /// `asDate(number)`).
    pub fn from_epoch_millis(millis: i64) -> TemporalResult<DateTime> {
        DateTime::from_nanos(millis as i128 * 1_000_000, 0)
    }

    /// The wall-clock time `local` read at the fixed offset `offset_seconds`.
    pub fn from_local(local: LocalDateTime, offset_seconds: i32) -> TemporalResult<DateTime> {
        let nanos = local.nanos.get() - offset_seconds as i128 * NANOS_PER_SECOND as i128;
        DateTime::from_nanos(nanos, offset_seconds)
    }

    /// Microseconds since 1970-01-01T00:00:00Z, rounded down.
    pub fn epoch_micros(self) -> i64 {
        // In range by construction: the years -9999..9999 fit an i64 of microseconds.
        self.nanos.get().div_euclid(NANOS_PER_MICRO) as i64
    }

    /// Nanoseconds since 1970-01-01T00:00:00Z.
    pub fn epoch_nanos(self) -> i128 {
        self.nanos.get()
    }

    /// Milliseconds since the Unix epoch, rounded down (TinkerPop `asNumber()`).
    pub fn epoch_millis(self) -> i64 {
        self.nanos.get().div_euclid(1_000_000) as i64
    }

    /// The UTC offset in seconds.
    pub fn offset_seconds(self) -> i32 {
        self.offset_seconds
    }

    /// The wall-clock date and time at the value's own offset.
    pub fn local(self) -> LocalDateTime {
        // In range by construction.
        LocalDateTime {
            nanos: Nanos96::new(
                self.nanos.get() + self.offset_seconds as i128 * NANOS_PER_SECOND as i128,
            ),
        }
    }

    /// The same instant written with another offset; `OutOfRange` when its local time leaves the
    /// supported years.
    pub fn with_offset(self, offset_seconds: i32) -> TemporalResult<DateTime> {
        DateTime::from_nanos(self.nanos.get(), offset_seconds)
    }
}

impl PartialEq for DateTime {
    fn eq(&self, other: &Self) -> bool {
        self.nanos == other.nanos
    }
}

impl Eq for DateTime {}

impl Hash for DateTime {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.nanos.hash(state);
    }
}

impl PartialOrd for DateTime {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for DateTime {
    fn cmp(&self, other: &Self) -> Ordering {
        self.nanos.get().cmp(&other.nanos.get())
    }
}

impl Duration {
    /// The duration of `months` months, `days` days and `micros` microseconds; each part keeps
    /// its own sign.
    pub fn new(months: i32, days: i32, micros: i64) -> Duration {
        Duration {
            months,
            days,
            nanos: Nanos96::new(micros as i128 * NANOS_PER_MICRO),
        }
    }

    /// The duration of `months` months, `days` days and `nanos` nanoseconds; each part keeps its
    /// own sign. `OutOfRange` when the nanoseconds exceed an `i64` of microseconds.
    pub fn with_nanos(months: i32, days: i32, nanos: i128) -> TemporalResult<Duration> {
        if (MIN_DURATION_NANOS..=MAX_DURATION_NANOS).contains(&nanos) {
            Ok(Duration {
                months,
                days,
                nanos: Nanos96::new(nanos),
            })
        } else {
            Err(TemporalError::out_of_range("the duration"))
        }
    }

    /// A duration of `micros` microseconds (no calendar part).
    pub fn from_micros(micros: i64) -> Duration {
        Duration::new(0, 0, micros)
    }

    /// A duration of `nanos` nanoseconds (no calendar part); `OutOfRange` beyond an `i64` of
    /// microseconds.
    pub fn from_nanos(nanos: i128) -> TemporalResult<Duration> {
        Duration::with_nanos(0, 0, nanos)
    }

    /// The months part.
    pub fn months(self) -> i32 {
        self.months
    }

    /// The days part.
    pub fn days(self) -> i32 {
        self.days
    }

    /// The time part in whole microseconds, the nanoseconds below them cut (toward zero).
    pub fn micros(self) -> i64 {
        // In range by construction.
        (self.nanos.get() / NANOS_PER_MICRO) as i64
    }

    /// The time part in nanoseconds.
    pub fn nanos(self) -> i128 {
        self.nanos.get()
    }

    /// Whether every part is zero.
    pub fn is_zero(self) -> bool {
        self.months == 0 && self.days == 0 && self.nanos.get() == 0
    }

    /// The length used for ordering: a month counted as 30 days, a day as 24 hours.
    pub(crate) fn ordering_nanos(self) -> i128 {
        (self.months as i128 * 30 + self.days as i128) * NANOS_PER_DAY as i128 + self.nanos.get()
    }

    /// Every part negated; `OutOfRange` for `i32::MIN` months or days or the smallest time part.
    pub fn checked_neg(self) -> TemporalResult<Duration> {
        match (self.months.checked_neg(), self.days.checked_neg()) {
            (Some(months), Some(days)) => Duration::with_nanos(months, days, -self.nanos.get())
                .map_err(|_| TemporalError::out_of_range("the negated duration")),
            _ => Err(TemporalError::out_of_range("the negated duration")),
        }
    }

    /// The part-wise sum; `OutOfRange` on overflow.
    pub fn checked_add(self, other: Duration) -> TemporalResult<Duration> {
        match (
            self.months.checked_add(other.months),
            self.days.checked_add(other.days),
        ) {
            (Some(months), Some(days)) => {
                Duration::with_nanos(months, days, self.nanos.get() + other.nanos.get())
            }
            _ => Err(TemporalError::out_of_range("the duration")),
        }
    }
}

impl PartialOrd for Duration {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Duration {
    fn cmp(&self, other: &Self) -> Ordering {
        self.ordering_nanos()
            .cmp(&other.ordering_nanos())
            .then_with(|| {
                (self.months, self.days, self.nanos.get()).cmp(&(
                    other.months,
                    other.days,
                    other.nanos.get(),
                ))
            })
    }
}

/// Every field of a temporal value, for a key that must tell apart equal values written
/// differently (a `datetime`'s offset): the kind, two small integers and the nanoseconds.
pub(crate) type TemporalBits = (TemporalKind, i64, i64, i128);

impl Temporal {
    /// Every field of the value (see [`TemporalBits`]).
    pub(crate) fn bits(&self) -> TemporalBits {
        match *self {
            Temporal::Date(v) => (TemporalKind::Date, v.days as i64, 0, 0),
            Temporal::LocalDateTime(v) => (TemporalKind::LocalDateTime, 0, 0, v.nanos.get()),
            Temporal::DateTime(v) => (
                TemporalKind::DateTime,
                v.offset_seconds as i64,
                0,
                v.nanos.get(),
            ),
            Temporal::Time(v) => (TemporalKind::Time, 0, 0, v.nanos as i128),
            Temporal::Duration(v) => (
                TemporalKind::Duration,
                v.months as i64,
                v.days as i64,
                v.nanos.get(),
            ),
        }
    }

    /// Whether the value has a non-zero part below the microsecond (`12:00:00.000000001`): such a
    /// value has no exact microsecond form (a file format or a binding that holds microseconds).
    pub fn has_sub_micros(&self) -> bool {
        match *self {
            Temporal::Date(_) => false,
            Temporal::LocalDateTime(v) => v.nanos.get() % NANOS_PER_MICRO != 0,
            Temporal::DateTime(v) => v.nanos.get() % NANOS_PER_MICRO != 0,
            Temporal::Time(v) => v.nanos % 1000 != 0,
            Temporal::Duration(v) => v.nanos.get() % NANOS_PER_MICRO != 0,
        }
    }

    /// The value's type.
    pub fn kind(&self) -> TemporalKind {
        match self {
            Temporal::Date(_) => TemporalKind::Date,
            Temporal::LocalDateTime(_) => TemporalKind::LocalDateTime,
            Temporal::DateTime(_) => TemporalKind::DateTime,
            Temporal::Time(_) => TemporalKind::Time,
            Temporal::Duration(_) => TemporalKind::Duration,
        }
    }

    /// Parses the canonical (or any accepted ISO 8601) text of a value of type `kind`.
    pub fn parse_as(kind: TemporalKind, text: &str) -> TemporalResult<Temporal> {
        Ok(match kind {
            TemporalKind::Date => Temporal::Date(text.parse()?),
            TemporalKind::LocalDateTime => Temporal::LocalDateTime(text.parse()?),
            TemporalKind::DateTime => Temporal::DateTime(text.parse()?),
            TemporalKind::Time => Temporal::Time(text.parse()?),
            TemporalKind::Duration => Temporal::Duration(text.parse()?),
        })
    }

    /// Like [`parse_as`](Self::parse_as), except that a `datetime` must carry its UTC offset or
    /// time zone: `"2024-01-01T10:00"` is an error here, never read as UTC. This is the parser
    /// for stored data (a schema coercion into `format: date-time`, a typed `gx:OffsetDateTime`,
    /// a `formatMinimum` bound); only an explicit conversion (`asDate()`, the `datetime(..)`
    /// constructor) keeps TinkerPop's lenient "no offset is UTC".
    pub fn parse_strict(kind: TemporalKind, text: &str) -> TemporalResult<Temporal> {
        match kind {
            TemporalKind::DateTime => parse::date_time_strict(text).map(Temporal::DateTime),
            TemporalKind::Date
            | TemporalKind::LocalDateTime
            | TemporalKind::Time
            | TemporalKind::Duration => Temporal::parse_as(kind, text),
        }
    }
}

/// Orders by type first (date, local date-time, date-time, time, duration), then by value.
impl PartialOrd for Temporal {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Temporal {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Temporal::Date(a), Temporal::Date(b)) => a.cmp(b),
            (Temporal::LocalDateTime(a), Temporal::LocalDateTime(b)) => a.cmp(b),
            (Temporal::DateTime(a), Temporal::DateTime(b)) => a.cmp(b),
            (Temporal::Time(a), Temporal::Time(b)) => a.cmp(b),
            (Temporal::Duration(a), Temporal::Duration(b)) => a.cmp(b),
            _ => self.kind().cmp(&other.kind()),
        }
    }
}

macro_rules! text_traits {
    ($($ty:ident => $parse:path),* $(,)?) => {$(
        impl FromStr for $ty {
            type Err = TemporalError;
            fn from_str(text: &str) -> TemporalResult<$ty> {
                $parse(text)
            }
        }
        impl fmt::Debug for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({})", stringify!($ty), self)
            }
        }
    )*};
}

text_traits!(
    Date => parse::date,
    Time => parse::time,
    LocalDateTime => parse::local_date_time,
    DateTime => parse::date_time,
    Duration => parse::duration,
);

impl fmt::Display for Temporal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Temporal::Date(v) => fmt::Display::fmt(v, f),
            Temporal::LocalDateTime(v) => fmt::Display::fmt(v, f),
            Temporal::DateTime(v) => fmt::Display::fmt(v, f),
            Temporal::Time(v) => fmt::Display::fmt(v, f),
            Temporal::Duration(v) => fmt::Display::fmt(v, f),
        }
    }
}

impl fmt::Debug for Temporal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Temporal::Date(v) => fmt::Debug::fmt(v, f),
            Temporal::LocalDateTime(v) => fmt::Debug::fmt(v, f),
            Temporal::DateTime(v) => fmt::Debug::fmt(v, f),
            Temporal::Time(v) => fmt::Debug::fmt(v, f),
            Temporal::Duration(v) => fmt::Debug::fmt(v, f),
        }
    }
}

impl From<Date> for Temporal {
    fn from(v: Date) -> Self {
        Temporal::Date(v)
    }
}

impl From<Time> for Temporal {
    fn from(v: Time) -> Self {
        Temporal::Time(v)
    }
}

impl From<LocalDateTime> for Temporal {
    fn from(v: LocalDateTime) -> Self {
        Temporal::LocalDateTime(v)
    }
}

impl From<DateTime> for Temporal {
    fn from(v: DateTime) -> Self {
        Temporal::DateTime(v)
    }
}

impl From<Duration> for Temporal {
    fn from(v: Duration) -> Self {
        Temporal::Duration(v)
    }
}
