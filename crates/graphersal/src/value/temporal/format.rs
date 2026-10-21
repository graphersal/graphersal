//! The canonical text of every temporal type (ISO 8601 / RFC 3339).
//!
//! - Years are four digits with a `-` for years before 1 (`-0044-03-15`; year 0 is 1 BC).
//! - Seconds are always written; a fraction only when it is not zero, in groups of three digits
//!   (`.250`, `.250500`), as `java.time` writes them.
//! - An offset of zero is `Z`, others `+HH:MM`, with `:SS` only when the offset has seconds.
//! - A duration is `P[nY][nM][nD][T[nH][nM][n[.f]S]]`, `PT0S` when zero; when every non-zero
//!   part is negative the whole is written negative (`-P1DT2H`), otherwise each negative part
//!   carries its own sign (`P1M-2D`).
//!
//! Parsing the text gives the same value back (`parse.rs`, tested in `tests.rs`).

use std::fmt::{self, Write};

use super::civil::{self, MICROS_PER_SECOND};
use super::{Date, DateTime, Duration, LocalDateTime, Temporal, Time};

fn write_date(f: &mut impl Write, days: i32) -> fmt::Result {
    let (year, month, day) = civil::civil_from_days(days);
    if year < 0 {
        write!(f, "-{:04}-{month:02}-{day:02}", -year)
    } else {
        write!(f, "{year:04}-{month:02}-{day:02}")
    }
}

/// Writes a fraction of a second (`.250`, `.250500`) when it is not zero.
fn write_fraction(f: &mut impl Write, micros: i64) -> fmt::Result {
    if micros == 0 {
        Ok(())
    } else if micros % 1000 == 0 {
        write!(f, ".{:03}", micros / 1000)
    } else {
        write!(f, ".{micros:06}")
    }
}

fn write_time(f: &mut impl Write, micros_of_day: i64) -> fmt::Result {
    let seconds = micros_of_day / MICROS_PER_SECOND;
    write!(
        f,
        "{:02}:{:02}:{:02}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    )?;
    write_fraction(f, micros_of_day % MICROS_PER_SECOND)
}

fn write_local(f: &mut impl Write, micros: i64) -> fmt::Result {
    let (days, of_day) = civil::split_micros(micros);
    write_date(f, days as i32)?;
    f.write_char('T')?;
    write_time(f, of_day)
}

/// Writes a UTC offset: `Z`, `+02:00`, `-00:19:32`.
pub(crate) fn write_offset(f: &mut impl Write, offset_seconds: i32) -> fmt::Result {
    if offset_seconds == 0 {
        return f.write_char('Z');
    }
    let sign = if offset_seconds < 0 { '-' } else { '+' };
    let abs = offset_seconds.unsigned_abs();
    write!(f, "{sign}{:02}:{:02}", abs / 3600, abs / 60 % 60)?;
    if !abs.is_multiple_of(60) {
        write!(f, ":{:02}", abs % 60)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// The Neo4j text (GQL `toString`, the Cypher compatibility form; 3g U6)
// ---------------------------------------------------------------------------------------------

/// The time of day as `java.time` `LocalTime.toString` writes it: `HH:MM`, `:SS` only when the
/// seconds or the fraction are not zero, the fraction in groups of three digits.
fn write_neo4j_time(f: &mut impl Write, micros_of_day: i64) -> fmt::Result {
    let seconds = micros_of_day / MICROS_PER_SECOND;
    let fraction = micros_of_day % MICROS_PER_SECOND;
    write!(f, "{:02}:{:02}", seconds / 3600, seconds / 60 % 60)?;
    if seconds % 60 != 0 || fraction != 0 {
        write!(f, ":{:02}", seconds % 60)?;
    }
    write_fraction(f, fraction)
}

/// A local date-time in the Neo4j text: `<date>T<time>` with the short time.
fn write_neo4j_local(f: &mut impl Write, micros: i64) -> fmt::Result {
    let (days, of_day) = civil::split_micros(micros);
    write_date(f, days as i32)?;
    f.write_char('T')?;
    write_neo4j_time(f, of_day)
}

/// A duration in the Neo4j text (`DurationValue.prettyPrint`): `P[nY][nM][nD][T[nH][nM][nS]]`
/// with every component signed on its own, hours not folded into days (`PT49H`), the seconds
/// signed together with their fraction (`PT-1M-0.001S`, `PT-1.999S`), the fraction without
/// trailing zeros (`PT59.9S`), `PT0S` when zero.
fn write_neo4j_duration(f: &mut impl Write, value: Duration) -> fmt::Result {
    if value.is_zero() {
        return f.write_str("PT0S");
    }
    f.write_char('P')?;
    let months = value.months as i64;
    let part = |f: &mut dyn Write, n: i64, unit: char| -> fmt::Result {
        if n != 0 {
            write!(f, "{n}{unit}")?;
        }
        Ok(())
    };
    part(f, months / 12, 'Y')?;
    part(f, months % 12, 'M')?;
    part(f, value.days as i64, 'D')?;
    let micros = value.micros;
    if micros != 0 {
        f.write_char('T')?;
        // Truncating division: every component takes the sign of the time part.
        let (hours, rest) = (
            micros / (3600 * MICROS_PER_SECOND),
            micros % (3600 * MICROS_PER_SECOND),
        );
        let (minutes, rest) = (
            rest / (60 * MICROS_PER_SECOND),
            rest % (60 * MICROS_PER_SECOND),
        );
        part(f, hours, 'H')?;
        part(f, minutes, 'M')?;
        if rest != 0 {
            if rest < 0 {
                f.write_char('-')?;
            }
            let rest = rest.unsigned_abs();
            let micro = MICROS_PER_SECOND as u64;
            write!(f, "{}", rest / micro)?;
            let fraction = rest % micro;
            if fraction != 0 {
                let digits = format!("{fraction:06}");
                write!(f, ".{}", digits.trim_end_matches('0'))?;
            }
            f.write_char('S')?;
        }
    }
    Ok(())
}

/// Writes a value in the text Neo4j gives it (`java.time` `toString`), the form of GQL's
/// `toString` (Cypher compatibility; ISO `CAST(x AS STRING)` keeps the canonical text):
/// `12:00` and `1984-10-11T12:31` without zero seconds, an offset `Z` / `+01:00`, a duration
/// signed per component (`PT-1M-0.001S`). Our values carry no zone name, so the
/// `[Europe/Stockholm]` suffix is never written. The engine parsers read the text back.
#[allow(dead_code, reason = "the GQL temporal functions (3g.T4-T8) call it")]
pub(crate) fn write_neo4j(f: &mut impl Write, value: &Temporal) -> fmt::Result {
    match *value {
        Temporal::Date(v) => write_date(f, v.days),
        Temporal::Time(v) => write_neo4j_time(f, v.micros),
        Temporal::LocalDateTime(v) => write_neo4j_local(f, v.micros),
        Temporal::DateTime(v) => {
            write_neo4j_local(f, v.local().micros)?;
            write_offset(f, v.offset_seconds)
        }
        Temporal::Duration(v) => write_neo4j_duration(f, v),
    }
}

/// [`write_neo4j`] into a new string.
#[allow(dead_code, reason = "the GQL temporal functions (3g.T4-T8) call it")]
pub(crate) fn neo4j(value: &Temporal) -> String {
    let mut text = String::new();
    // Writing into a `String` never fails.
    let _ = write_neo4j(&mut text, value);
    text
}

impl fmt::Display for Date {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_date(f, self.days)
    }
}

impl fmt::Display for Time {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_time(f, self.micros)
    }
}

impl fmt::Display for LocalDateTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_local(f, self.micros)
    }
}

impl fmt::Display for DateTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_local(f, self.local().micros)?;
        write_offset(f, self.offset_seconds)
    }
}

impl fmt::Display for Duration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_zero() {
            return f.write_str("PT0S");
        }
        // Wide integers: `i64::MIN` micros and `i32::MIN` months negate without overflow.
        let mut months = self.months as i128;
        let mut days = self.days as i128;
        let mut micros = self.micros as i128;
        let all_negative = months <= 0 && days <= 0 && micros <= 0;
        if all_negative {
            f.write_char('-')?;
            months = -months;
            days = -days;
            micros = -micros;
        }
        f.write_char('P')?;
        let (years, months) = (months / 12, months % 12);
        if years != 0 {
            write!(f, "{years}Y")?;
        }
        if months != 0 {
            write!(f, "{months}M")?;
        }
        if days != 0 {
            write!(f, "{days}D")?;
        }
        if micros != 0 {
            f.write_char('T')?;
            let second = MICROS_PER_SECOND as i128;
            let (hours, rest) = (micros / (3600 * second), micros % (3600 * second));
            let (minutes, rest) = (rest / (60 * second), rest % (60 * second));
            if hours != 0 {
                write!(f, "{hours}H")?;
            }
            if minutes != 0 {
                write!(f, "{minutes}M")?;
            }
            if rest != 0 {
                if rest < 0 {
                    f.write_char('-')?;
                }
                let rest = rest.abs();
                write!(f, "{}", rest / second)?;
                write_fraction(f, (rest % second) as i64)?;
                f.write_char('S')?;
            }
        }
        Ok(())
    }
}
