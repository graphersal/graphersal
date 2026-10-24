//! Parsing temporal text (ISO 8601 / RFC 3339 / RFC 9557), stricter than ISO 8601 and never
//! guessing (no locale formats, no two-digit years, no content sniffing: these parsers run only
//! where a temporal value is asked for).
//!
//! - date: `YYYY-MM-DD`, a year before 1 or after 9999 with its sign (`-0044-03-15`);
//! - time: `HH:MM`, `HH:MM:SS`, `HH:MM:SS.f` (1-9 fraction digits, kept to the nanosecond);
//! - local date-time: a date, optionally `T` (or `t`, or a space) and a time; a date alone is
//!   midnight;
//! - date-time: a local date-time, then an offset (`Z`, `+02:00`, `+0200`, `+02`, `+00:19:32`)
//!   and/or a time zone in brackets (`[Europe/Berlin]`, RFC 9557). Without either it is UTC (as
//!   TinkerPop's `asDate("2023-08-02")`). With only a zone the wall-clock time is resolved in
//!   it (a skipped time moves forward, a repeated one takes the earlier offset); with both the
//!   offset must be the zone's at that instant;
//! - duration: `[+-]P[nY][nM][nW][nD][T[nH][nM][n[.f]S]]`, each number with an optional sign
//!   of its own (`P1M-2D`), letters in either case.

use super::arith::{Amount, DurationParts, cascade};
use super::civil::{self, NANOS_PER_DAY, NANOS_PER_SECOND};
use super::zone::Zone;
use super::{
    Date, DateTime, Duration, LocalDateTime, MAX_OFFSET_SECONDS, Temporal, TemporalError,
    TemporalKind, TemporalResult, Time,
};

struct Cursor<'a> {
    text: &'a str,
    bytes: &'a [u8],
    pos: usize,
    expected: &'static str,
}

impl<'a> Cursor<'a> {
    fn new(text: &'a str, expected: &'static str) -> Self {
        Cursor {
            text,
            bytes: text.as_bytes(),
            pos: 0,
            expected,
        }
    }

    fn fail<T>(&self, reason: &'static str) -> TemporalResult<T> {
        Err(TemporalError::parse(self.text, self.expected, reason))
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn at_end(&self) -> bool {
        self.pos == self.bytes.len()
    }

    fn eat(&mut self, byte: u8) -> bool {
        if self.peek() == Some(byte) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, byte: u8, reason: &'static str) -> TemporalResult<()> {
        if self.eat(byte) {
            Ok(())
        } else {
            self.fail(reason)
        }
    }

    /// Exactly `count` ASCII digits.
    fn digits(&mut self, count: usize, reason: &'static str) -> TemporalResult<u32> {
        let mut value = 0u32;
        for _ in 0..count {
            match self.peek() {
                Some(b @ b'0'..=b'9') => {
                    value = value * 10 + (b - b'0') as u32;
                    self.pos += 1;
                }
                _ => return self.fail(reason),
            }
        }
        Ok(value)
    }

    /// A run of digits (at least one), as `i64`; `None` when there is none.
    fn number(&mut self) -> TemporalResult<Option<i64>> {
        let start = self.pos;
        let mut value: i64 = 0;
        while let Some(b @ b'0'..=b'9') = self.peek() {
            value = match value
                .checked_mul(10)
                .and_then(|v| v.checked_add((b - b'0') as i64))
            {
                Some(v) => v,
                None => return self.fail("a number is too large"),
            };
            self.pos += 1;
        }
        Ok((self.pos > start).then_some(value))
    }

    fn finish(&self) -> TemporalResult<()> {
        if self.at_end() {
            Ok(())
        } else {
            self.fail("unexpected text after the value")
        }
    }
}

fn read_date(c: &mut Cursor) -> TemporalResult<i32> {
    let sign = if c.eat(b'-') {
        Some(-1)
    } else if c.eat(b'+') {
        Some(1)
    } else {
        None
    };
    let start = c.pos;
    let mut year: i64 = 0;
    while let Some(b @ b'0'..=b'9') = c.peek() {
        year = year * 10 + (b - b'0') as i64;
        c.pos += 1;
        if c.pos - start > 6 {
            return c.fail("the year has too many digits");
        }
    }
    let width = c.pos - start;
    if width < 4 || (sign.is_none() && width != 4) {
        return c.fail("expected a four-digit year (YYYY-MM-DD)");
    }
    let year = year * sign.unwrap_or(1);
    c.expect(b'-', "expected '-' after the year (YYYY-MM-DD)")?;
    let month = c.digits(2, "expected a two-digit month (YYYY-MM-DD)")?;
    c.expect(b'-', "expected '-' after the month (YYYY-MM-DD)")?;
    let day = c.digits(2, "expected a two-digit day (YYYY-MM-DD)")?;
    if year < civil::MIN_YEAR as i64 || year > civil::MAX_YEAR as i64 {
        return c.fail("the year is outside -9999 to 9999");
    }
    let year = year as i32;
    if !(1..=12).contains(&month) {
        return c.fail("the month is not 01-12");
    }
    if day == 0 || day > civil::days_in_month(year, month as u8) as u32 {
        return c.fail("the day does not exist in that month");
    }
    Ok(civil::days_from_civil(year, month as u8, day as u8))
}

/// Up to nine fraction digits after a `.` or `,`, as nanoseconds (exact).
fn read_fraction(c: &mut Cursor) -> TemporalResult<i64> {
    if !(c.eat(b'.') || c.eat(b',')) {
        return Ok(0);
    }
    let mut nanos = 0i64;
    let mut count = 0;
    while let Some(b @ b'0'..=b'9') = c.peek() {
        if count < 9 {
            nanos = nanos * 10 + (b - b'0') as i64;
        }
        count += 1;
        c.pos += 1;
        if count > 9 {
            return c.fail("more than nine fraction digits");
        }
    }
    if count == 0 {
        return c.fail("expected digits after the decimal point");
    }
    for _ in count..9 {
        nanos *= 10;
    }
    Ok(nanos)
}

fn read_time(c: &mut Cursor) -> TemporalResult<i64> {
    let hour = c.digits(2, "expected a two-digit hour (HH:MM:SS)")?;
    c.expect(b':', "expected ':' after the hour (HH:MM:SS)")?;
    let minute = c.digits(2, "expected two-digit minutes (HH:MM:SS)")?;
    let (second, fraction) = if c.eat(b':') {
        let second = c.digits(2, "expected two-digit seconds (HH:MM:SS)")?;
        (second, read_fraction(c)?)
    } else {
        (0, 0)
    };
    if hour > 23 {
        return c.fail("the hour is not 00-23");
    }
    if minute > 59 {
        return c.fail("the minutes are not 00-59");
    }
    if second > 59 {
        return c.fail("the seconds are not 00-59");
    }
    Ok(((hour as i64 * 60 + minute as i64) * 60 + second as i64) * NANOS_PER_SECOND + fraction)
}

/// A date, optionally followed by a separator and a time; local nanoseconds.
fn read_local(c: &mut Cursor) -> TemporalResult<i128> {
    let days = read_date(c)?;
    let mut nanos = days as i128 * NANOS_PER_DAY as i128;
    if matches!(c.peek(), Some(b'T' | b't' | b' ')) {
        c.pos += 1;
        nanos += read_time(c)? as i128;
    }
    Ok(nanos)
}

/// `Z`, `+HH:MM[:SS]`, `+HHMM`, `+HH`; `None` when no offset follows.
fn read_offset(c: &mut Cursor) -> TemporalResult<Option<i32>> {
    if c.eat(b'Z') || c.eat(b'z') {
        return Ok(Some(0));
    }
    let sign = if c.eat(b'+') {
        1
    } else if c.eat(b'-') {
        -1
    } else {
        return Ok(None);
    };
    let hours = c.digits(2, "expected a two-digit offset hour (+HH:MM)")?;
    let mut minutes = 0;
    let mut seconds = 0;
    if c.eat(b':') {
        minutes = c.digits(2, "expected two-digit offset minutes (+HH:MM)")?;
        if c.eat(b':') {
            seconds = c.digits(2, "expected two-digit offset seconds (+HH:MM:SS)")?;
        }
    } else if matches!(c.peek(), Some(b'0'..=b'9')) {
        minutes = c.digits(2, "expected two-digit offset minutes (+HHMM)")?;
    }
    if minutes > 59 || seconds > 59 {
        return c.fail("the offset minutes or seconds are not 00-59");
    }
    let total = sign * (hours * 3600 + minutes * 60 + seconds) as i32;
    if total.abs() > MAX_OFFSET_SECONDS {
        return c.fail("the offset is beyond ±18:00");
    }
    Ok(Some(total))
}

/// `[Zone/Name]` (RFC 9557; a leading `!` critical flag is accepted); `None` when absent.
fn read_zone<'a>(c: &mut Cursor<'a>) -> TemporalResult<Option<&'a str>> {
    if !c.eat(b'[') {
        return Ok(None);
    }
    c.eat(b'!');
    let start = c.pos;
    while let Some(b) = c.peek() {
        if b == b']' {
            break;
        }
        c.pos += 1;
    }
    let name = &c.text[start..c.pos];
    c.expect(b']', "expected ']' after the time zone name")?;
    if name.is_empty() {
        return c.fail("the time zone name in brackets is empty");
    }
    Ok(Some(name))
}

pub(super) fn date(text: &str) -> TemporalResult<Date> {
    let mut c = Cursor::new(text, "date");
    let days = read_date(&mut c)?;
    c.finish()?;
    Ok(Date { days })
}

pub(super) fn time(text: &str) -> TemporalResult<Time> {
    let mut c = Cursor::new(text, "time");
    let nanos = read_time(&mut c)?;
    if !c.at_end() && matches!(c.peek(), Some(b'Z' | b'z' | b'+' | b'-')) {
        return c.fail("a time has no UTC offset");
    }
    c.finish()?;
    Ok(Time { nanos })
}

pub(super) fn local_date_time(text: &str) -> TemporalResult<LocalDateTime> {
    let mut c = Cursor::new(text, "localdatetime");
    let nanos = read_local(&mut c)?;
    if !c.at_end() && matches!(c.peek(), Some(b'Z' | b'z' | b'+' | b'-' | b'[')) {
        return c.fail("a local date-time has no UTC offset or time zone (use a datetime)");
    }
    c.finish()?;
    LocalDateTime::from_nanos(nanos)
}

pub(super) fn date_time(text: &str) -> TemporalResult<DateTime> {
    read_date_time(text, false)
}

/// A date-time that must carry its UTC offset or time zone: where stored data is read (a schema
/// coercion, a typed `gx:OffsetDateTime`, a format bound), a text without one is an error, never
/// read as UTC (rule 7: no guessing).
pub(super) fn date_time_strict(text: &str) -> TemporalResult<DateTime> {
    read_date_time(text, true)
}

fn read_date_time(text: &str, strict: bool) -> TemporalResult<DateTime> {
    let mut c = Cursor::new(text, "datetime");
    let local = read_local(&mut c)?;
    let offset = read_offset(&mut c)?;
    let zone = read_zone(&mut c)?;
    c.finish()?;
    let local = LocalDateTime::from_nanos(local)?;
    match (offset, zone) {
        (Some(offset), None) => DateTime::from_local(local, offset),
        (None, None) if strict => c.fail(
            "a date-time needs its UTC offset (Z, +02:00) or a time zone ([Europe/Berlin]); a \
             text without one is not read as UTC",
        ),
        (None, None) => DateTime::from_local(local, 0),
        (None, Some(name)) => Zone::parse(name)?.resolve(local),
        (Some(offset), Some(name)) => {
            let value = DateTime::from_local(local, offset)?;
            let zone = Zone::parse(name)?;
            if zone.offset_at(value)? != offset {
                return c.fail("the offset is not the time zone's offset at that time");
            }
            Ok(value)
        }
    }
}

pub(super) fn duration(text: &str) -> TemporalResult<Duration> {
    let mut c = Cursor::new(text, "duration");
    let negative = if c.eat(b'-') {
        true
    } else {
        c.eat(b'+');
        false
    };
    if !(c.eat(b'P') || c.eat(b'p')) {
        return c.fail("expected 'P' (ISO 8601 duration such as P1DT2H)");
    }
    // Wide accumulators: the parts are summed and the leading sign applied before the range
    // check, so `-P178956970Y8M` (i32::MIN months) and `-PT2562047788H0M54.775808S` (the
    // smallest time part, i64::MIN microseconds) parse back.
    let mut months: i128 = 0;
    let mut days: i128 = 0;
    let mut nanos: i128 = 0;
    let mut any = false;
    let mut in_time = false;
    // The units in their required order; the index only grows.
    const DATE_UNITS: &[u8] = b"YMWD";
    const TIME_UNITS: &[u8] = b"HMS";
    const LIMIT: i128 = 1 << 100;
    let mut next_unit = 0usize;
    loop {
        if c.at_end() {
            break;
        }
        if !in_time && (c.eat(b'T') || c.eat(b't')) {
            in_time = true;
            next_unit = 0;
            if c.at_end() {
                return c.fail("expected a time part after 'T'");
            }
            continue;
        }
        let sign: i128 = if c.eat(b'-') {
            -1
        } else {
            c.eat(b'+');
            1
        };
        let Some(number) = c.number()? else {
            return c.fail("expected a number before a duration unit");
        };
        let fraction = if in_time { read_fraction(&mut c)? } else { 0 };
        let Some(unit) = c.peek().map(|b| b.to_ascii_uppercase()) else {
            return c.fail("expected a unit (Y, M, W, D, H, M or S) after the number");
        };
        c.pos += 1;
        let units = if in_time { TIME_UNITS } else { DATE_UNITS };
        let Some(index) = units[next_unit..].iter().position(|&u| u == unit) else {
            return c.fail("unknown or out-of-order duration unit");
        };
        next_unit += index + 1;
        if fraction != 0 && unit != b'S' {
            return c.fail("only seconds may have a fraction");
        }
        // `number` is at most i64::MAX (an hour of nanoseconds is below 2^42, the product below
        // 2^105) and at most seven parts are summed: no i128 overflow.
        let n = number as i128 * sign;
        let second = NANOS_PER_SECOND as i128;
        match (in_time, unit) {
            (false, b'Y') => months += n * 12,
            (false, b'M') => months += n,
            (false, b'W') => days += n * 7,
            (false, _) => days += n,
            (true, b'H') => nanos += n * 3600 * second,
            (true, b'M') => nanos += n * 60 * second,
            (true, _) => nanos += n * second + fraction as i128 * sign,
        }
        if months.abs() > LIMIT || days.abs() > LIMIT || nanos.abs() > LIMIT {
            return c.fail("the duration is too large");
        }
        any = true;
    }
    if !any {
        return c.fail("a duration needs at least one part (PT0S for zero)");
    }
    if negative {
        months = -months;
        days = -days;
        nanos = -nanos;
    }
    match (i32::try_from(months), i32::try_from(days)) {
        (Ok(months), Ok(days)) => match Duration::with_nanos(months, days, nanos) {
            Ok(duration) => Ok(duration),
            Err(_) => c.fail("the duration is too large"),
        },
        _ => c.fail("the duration is too large"),
    }
}

// ---------------------------------------------------------------------------------------------
// The extended forms of the GQL constructors (`date('2015W302')`, `duration('P0.75M')`): ISO
// 8601 in full as Neo4j reads it. Used by GQL only; the parsers above (Rhai, schemas, imports)
// are unchanged.
// ---------------------------------------------------------------------------------------------

/// A value read by [`parse_extended`], with what our types could not keep.
#[cfg_attr(not(feature = "gql"), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Extended {
    /// The value.
    pub(crate) value: Temporal,
    /// Whether the text named an IANA zone (`[Europe/London]`), resolved to its offset at that
    /// time: the value keeps only the offset (3g U3).
    pub(crate) zone_name: bool,
}

/// The number of ASCII digits from the cursor on, without consuming them.
fn digit_run(c: &Cursor) -> usize {
    c.bytes[c.pos..]
        .iter()
        .take_while(|b| b.is_ascii_digit())
        .count()
}

/// A year: four digits, or a sign and four to nine digits (`+2015`, `-0044`; a signed run of
/// digits is the year, as Neo4j reads `+20150721`); a year outside -9999..9999 is
/// [`TemporalError::OutOfRange`].
fn read_year_extended(c: &mut Cursor) -> TemporalResult<i32> {
    let sign: i64 = if c.eat(b'-') {
        -1
    } else if c.eat(b'+') {
        1
    } else {
        let year = c.digits(4, "expected a four-digit year")?;
        return Ok(year as i32);
    };
    let run = digit_run(c);
    if !(4..=9).contains(&run) {
        return c.fail("expected a year of four to nine digits after the sign");
    }
    let year = sign * c.digits(run, "expected a year")? as i64;
    if year < civil::MIN_YEAR as i64 || year > civil::MAX_YEAR as i64 {
        return Err(TemporalError::out_of_range("the year"));
    }
    Ok(year as i32)
}

/// A date in any ISO 8601 form, each `-` optional: calendar (`2015-07-21`, `20150721`), year and
/// month (`2015-07`, `201507`: the first), week (`2015-W30-2`, `2015W302`; `2015-W30`: its
/// Monday), ordinal (`2015-202`, `2015202`), quarter (`2015-Q3-21`, `2015-Q3`) or the year
/// alone (`2015`: January 1). Days since 1970-01-01.
fn read_date_extended(c: &mut Cursor) -> TemporalResult<i32> {
    let year = read_year_extended(c)?;
    let dash = c.eat(b'-');
    let missing = "the date does not exist";
    let days = match c.peek() {
        Some(b'W' | b'w') => {
            c.pos += 1;
            let week = c.digits(2, "expected a two-digit week (2015-W30)")?;
            let dash = c.eat(b'-');
            let day = if dash || matches!(c.peek(), Some(b'0'..=b'9')) {
                c.digits(1, "expected a one-digit day of the week (2015-W30-2)")?
            } else {
                1
            };
            civil::from_iso_week(year, week as u8, day as u8)
        }
        Some(b'Q' | b'q') => {
            c.pos += 1;
            let quarter = c.digits(1, "expected a one-digit quarter (2015-Q3)")?;
            let dash = c.eat(b'-');
            let day = if dash || matches!(c.peek(), Some(b'0'..=b'9')) {
                c.digits(2, "expected a two-digit day of the quarter (2015-Q3-21)")?
            } else {
                1
            };
            civil::from_quarter(year, quarter as u8, day as u8)
        }
        Some(b'0'..=b'9') => match digit_run(c) {
            3 => {
                let ordinal = c.digits(3, "expected a three-digit day of the year")?;
                civil::from_ordinal(year, ordinal as u16)
            }
            2 | 4 => {
                let month = c.digits(2, "expected a two-digit month")?;
                let dash = c.eat(b'-');
                let day = if dash || matches!(c.peek(), Some(b'0'..=b'9')) {
                    c.digits(2, "expected a two-digit day")?
                } else {
                    1
                };
                if (1..=12).contains(&month) && day >= 1 {
                    let (month, day) = (month as u8, day as u8);
                    (day <= civil::days_in_month(year, month))
                        .then(|| civil::days_from_civil(year, month, day))
                } else {
                    None
                }
            }
            _ => return c.fail("expected a month, a week, a quarter or a day of the year"),
        },
        _ if dash => return c.fail("expected a month, a week, a quarter or a day after '-'"),
        _ => Some(civil::days_from_civil(year, 1, 1)),
    };
    match days {
        Some(days) => Ok(days),
        None => c.fail(missing),
    }
}

/// A time of day, each `:` optional: `21:40:32.142`, `214032.142`, `21:40`, `2140`, `21`.
/// Nanoseconds of the day.
fn read_time_extended(c: &mut Cursor) -> TemporalResult<i64> {
    let hour = c.digits(2, "expected a two-digit hour")?;
    let mut minute = 0;
    let mut second = 0;
    let mut fraction = 0;
    let colon = c.eat(b':');
    if colon || matches!(c.peek(), Some(b'0'..=b'9')) {
        minute = c.digits(2, "expected two-digit minutes")?;
        let colon = c.eat(b':');
        if colon || matches!(c.peek(), Some(b'0'..=b'9')) {
            second = c.digits(2, "expected two-digit seconds")?;
            fraction = read_nano_fraction(c)? as i64;
        }
    }
    if hour > 23 {
        return c.fail("the hour is not 00-23");
    }
    if minute > 59 {
        return c.fail("the minutes are not 00-59");
    }
    if second > 59 {
        return c.fail("the seconds are not 00-59");
    }
    Ok(((hour as i64 * 60 + minute as i64) * 60 + second as i64) * NANOS_PER_SECOND + fraction)
}

/// A date, then optionally `T` and a time: local nanoseconds.
fn read_local_extended(c: &mut Cursor) -> TemporalResult<i128> {
    let days = read_date_extended(c)?;
    let mut nanos = days as i128 * NANOS_PER_DAY as i128;
    if c.eat(b'T') || c.eat(b't') {
        nanos += read_time_extended(c)? as i128;
    }
    Ok(nanos)
}

/// Up to nine fraction digits after `.` or `,` as billionths (none: zero).
fn read_nano_fraction(c: &mut Cursor) -> TemporalResult<i128> {
    if !(c.eat(b'.') || c.eat(b',')) {
        return Ok(0);
    }
    let count = digit_run(c);
    if count == 0 {
        return c.fail("expected digits after the decimal point");
    }
    if count > 9 {
        return c.fail("more than nine fraction digits");
    }
    let mut nanos = c.digits(count, "expected fraction digits")? as i128;
    for _ in count..9 {
        nanos *= 10;
    }
    Ok(nanos)
}

/// One number of a unit-based duration: an optional sign, digits, an optional fraction of up
/// to nine digits (`-1.5`).
fn read_amount(c: &mut Cursor) -> TemporalResult<Option<Amount>> {
    let negative = if c.eat(b'-') {
        true
    } else {
        c.eat(b'+');
        false
    };
    let Some(whole) = c.number()? else {
        return Ok(None);
    };
    let amount = whole as i128 * Amount::SCALE + read_nano_fraction(c)?;
    Ok(Some(Amount::from_nano_units(if negative {
        -amount
    } else {
        amount
    })))
}

/// A duration in the unit form with a fraction on any unit (`P5M1.5D`, `P0.75M`, `PT0.75M`,
/// `P2.5W`, `P12Y5M14DT16H12M70S`, `PT-1.999S`) or the ISO alternative form
/// `PYYYY-MM-DDThh:mm:ss[.f]` (`P2012-02-02T14:37:21.545`), with an optional leading sign;
/// summed by the fraction cascade ([`cascade`]).
fn duration_extended(text: &str) -> TemporalResult<Extended> {
    let mut c = Cursor::new(text, "duration");
    let negative = if c.eat(b'-') {
        true
    } else {
        c.eat(b'+');
        false
    };
    if !(c.eat(b'P') || c.eat(b'p')) {
        return c.fail("expected 'P' (ISO 8601 duration such as P1DT2H)");
    }
    let mut parts = DurationParts::default();
    let alternative = digit_run(&c) == 4 && c.bytes.get(c.pos + 4) == Some(&b'-');
    if alternative {
        let years = c.digits(4, "expected a four-digit year (P2012-02-02T14:37:21)")?;
        c.expect(b'-', "expected '-' after the years (P2012-02-02T14:37:21)")?;
        let months = c.digits(2, "expected two-digit months (P2012-02-02T14:37:21)")?;
        c.expect(b'-', "expected '-' after the months (P2012-02-02T14:37:21)")?;
        let days = c.digits(2, "expected two-digit days (P2012-02-02T14:37:21)")?;
        if !(c.eat(b'T') || c.eat(b't')) {
            return c.fail("expected 'T' and the time (P2012-02-02T14:37:21)");
        }
        let hours = c.digits(2, "expected two-digit hours (P2012-02-02T14:37:21)")?;
        c.expect(b':', "expected ':' after the hours (P2012-02-02T14:37:21)")?;
        let minutes = c.digits(2, "expected two-digit minutes (P2012-02-02T14:37:21)")?;
        c.expect(
            b':',
            "expected ':' after the minutes (P2012-02-02T14:37:21)",
        )?;
        let seconds = c.digits(2, "expected two-digit seconds (P2012-02-02T14:37:21)")?;
        let nanos = read_nano_fraction(&mut c)?;
        if months > 12 || days > 31 || hours > 23 || minutes > 59 || seconds > 59 {
            return c.fail("a field of the alternative duration form is out of range");
        }
        parts.years = Amount::from_i64(years as i64);
        parts.months = Amount::from_i64(months as i64);
        parts.days = Amount::from_i64(days as i64);
        parts.hours = Amount::from_i64(hours as i64);
        parts.minutes = Amount::from_i64(minutes as i64);
        parts.seconds = Amount::from_nano_units(seconds as i128 * Amount::SCALE + nanos);
    } else {
        const DATE_UNITS: &[u8] = b"YMWD";
        const TIME_UNITS: &[u8] = b"HMS";
        let mut in_time = false;
        let mut next_unit = 0usize;
        let mut any = false;
        loop {
            if c.at_end() {
                break;
            }
            if !in_time && (c.eat(b'T') || c.eat(b't')) {
                in_time = true;
                next_unit = 0;
                if c.at_end() {
                    return c.fail("expected a time part after 'T'");
                }
                continue;
            }
            let Some(amount) = read_amount(&mut c)? else {
                return c.fail("expected a number before a duration unit");
            };
            let Some(unit) = c.peek().map(|b| b.to_ascii_uppercase()) else {
                return c.fail("expected a unit (Y, M, W, D, H, M or S) after the number");
            };
            c.pos += 1;
            let units = if in_time { TIME_UNITS } else { DATE_UNITS };
            let Some(index) = units[next_unit..].iter().position(|&u| u == unit) else {
                return c.fail("unknown or out-of-order duration unit");
            };
            next_unit += index + 1;
            let slot = match (in_time, unit) {
                (false, b'Y') => &mut parts.years,
                (false, b'M') => &mut parts.months,
                (false, b'W') => &mut parts.weeks,
                (false, _) => &mut parts.days,
                (true, b'H') => &mut parts.hours,
                (true, b'M') => &mut parts.minutes,
                (true, _) => &mut parts.seconds,
            };
            *slot = amount;
            any = true;
        }
        if !any {
            return c.fail("a duration needs at least one part (PT0S for zero)");
        }
    }
    c.finish()?;
    let cascaded = cascade(&parts)?;
    let duration = if negative {
        cascaded.checked_neg()?
    } else {
        cascaded
    };
    Ok(Extended {
        value: Temporal::Duration(duration),
        zone_name: false,
    })
}

/// Parses the text of a GQL temporal constructor (`date('2015-W30-2')`, `localtime('2140')`,
/// `duration('P0.75M')`) in every ISO 8601 form Neo4j reads, wider than [`Temporal::parse_as`]
/// (whose forms it includes):
///
/// - date: calendar, year-month, week, ordinal, quarter or year alone, each `-` optional
///   (`2015-07-21`, `20150721`, `2015-07`, `2015W302`, `2015-W30`, `2015-202`, `2015`); a year
///   beyond four digits takes a sign (`+2015`, `-0044`);
/// - time (a local time of day): `21:40:32.142`, `214032.142`, `21:40`, `2140`, `21`, up to nine
///   fraction digits; an offset is refused (a time with an offset is another type);
/// - local date-time: a date, optionally `T` and a time (`2015-W30-2T214032.142`,
///   `2015T214032`, `2015202T21`);
/// - date-time: a local date-time, then an offset (`Z`, `+01:00`, `+0100`, `-02`) and / or a
///   zone in brackets (`[Europe/London]`), resolved as by [`Temporal::parse_as`]; neither is
///   UTC;
/// - duration: the unit form with a fraction on any unit, or the alternative form
///   (`duration_extended`).
///
/// Never a guess from the content: the caller names the type. A year outside -9999..9999 or a
/// duration beyond the fields is [`TemporalError::OutOfRange`], any other bad text
/// [`TemporalError::Parse`]. Up to nine fraction digits are kept exactly.
#[cfg_attr(not(feature = "gql"), allow(dead_code))]
pub(crate) fn parse_extended(kind: TemporalKind, text: &str) -> TemporalResult<Extended> {
    let plain = |value: Temporal| Extended {
        value,
        zone_name: false,
    };
    match kind {
        TemporalKind::Duration => duration_extended(text),
        TemporalKind::Date => {
            let mut c = Cursor::new(text, "date");
            let days = read_date_extended(&mut c)?;
            c.finish()?;
            Ok(plain(Temporal::Date(Date { days })))
        }
        TemporalKind::Time => {
            let mut c = Cursor::new(text, "time");
            let nanos = read_time_extended(&mut c)?;
            if matches!(c.peek(), Some(b'Z' | b'z' | b'+' | b'-' | b'[')) {
                return c.fail("a local time has no UTC offset or time zone");
            }
            c.finish()?;
            Ok(plain(Temporal::Time(Time { nanos })))
        }
        TemporalKind::LocalDateTime => {
            let mut c = Cursor::new(text, "localdatetime");
            let nanos = read_local_extended(&mut c)?;
            if matches!(c.peek(), Some(b'Z' | b'z' | b'+' | b'-' | b'[')) {
                return c.fail("a local date-time has no UTC offset or time zone (use a datetime)");
            }
            c.finish()?;
            Ok(plain(Temporal::LocalDateTime(LocalDateTime::from_nanos(
                nanos,
            )?)))
        }
        TemporalKind::DateTime => {
            let mut c = Cursor::new(text, "datetime");
            let local = read_local_extended(&mut c)?;
            let offset = read_offset(&mut c)?;
            let zone = read_zone(&mut c)?;
            c.finish()?;
            let local = LocalDateTime::from_nanos(local)?;
            let value = match (offset, zone) {
                (Some(offset), None) => DateTime::from_local(local, offset)?,
                (None, None) => DateTime::from_local(local, 0)?,
                (None, Some(name)) => Zone::parse(name)?.resolve(local)?,
                (Some(offset), Some(name)) => {
                    let value = DateTime::from_local(local, offset)?;
                    if Zone::parse(name)?.offset_at(value)? != offset {
                        return c.fail("the offset is not the time zone's offset at that time");
                    }
                    value
                }
            };
            Ok(Extended {
                value: Temporal::DateTime(value),
                zone_name: zone.is_some(),
            })
        }
    }
}

/// A UTC offset written alone (`Z`, `+02:00`, `-0530`, `+02`), for a zone argument.
pub(super) fn offset(text: &str) -> TemporalResult<i32> {
    let mut c = Cursor::new(text, "UTC offset");
    let Some(offset) = read_offset(&mut c)? else {
        return c.fail("expected Z or a sign (+HH:MM)");
    };
    c.finish()?;
    Ok(offset)
}
