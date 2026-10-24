//! The per-value type tags of the temporal types: the names GraphSON 3.0 gives the `java.time`
//! types (`gx:OffsetDateTime`, ...), used where a value carries its type next to its text (a
//! serialized `ChangeSet`, a GraphSON export). The payload is always the canonical text.
//!
//! | Type | Tag |
//! |---|---|
//! | `datetime` | `gx:OffsetDateTime` |
//! | `date` | `gx:LocalDate` |
//! | `localdatetime` | `gx:LocalDateTime` |
//! | `time` | `gx:LocalTime` |
//! | `duration` of a time part only (no months, no days) | `gx:Duration` (`PT26H`) |
//! | `duration` of months and days only | `gx:Period` (`P1Y2M3D`) |
//! | any other `duration` | `graphersal:Interval` (`P1MT2H`) |
//!
//! The canonical text of each case is also valid `java.time` text of its tag: a time-only
//! duration is written in hours, minutes and seconds (`java.time.Duration` has no Y/M/D units), a
//! months-and-days one in years, months and days (`java.time.Period` has no time part). A zero
//! duration is `gx:Duration` `PT0S`.

use super::{Duration, Temporal, TemporalError, TemporalKind, TemporalResult};

/// `datetime`.
pub(crate) const OFFSET_DATE_TIME: &str = "gx:OffsetDateTime";
/// `date`.
pub(crate) const LOCAL_DATE: &str = "gx:LocalDate";
/// `localdatetime`.
pub(crate) const LOCAL_DATE_TIME: &str = "gx:LocalDateTime";
/// `time`.
pub(crate) const LOCAL_TIME: &str = "gx:LocalTime";
/// A `duration` of a time part only (`java.time.Duration`).
pub(crate) const DURATION: &str = "gx:Duration";
/// A `duration` of months and days only (`java.time.Period`).
pub(crate) const PERIOD: &str = "gx:Period";
/// Any other `duration` (PostgreSQL `interval`; no `java.time` type holds it).
pub(crate) const INTERVAL: &str = "graphersal:Interval";

/// Whether a duration is a `java.time.Duration` (a time part only; zero included).
pub(crate) fn is_fixed_length(duration: Duration) -> bool {
    duration.months() == 0 && duration.days() == 0
}

/// Whether a duration is a `java.time.Period` (months and days only, not zero).
pub(crate) fn is_period(duration: Duration) -> bool {
    duration.nanos() == 0 && !is_fixed_length(duration)
}

/// The tag of a value.
pub(crate) fn tag_of(value: &Temporal) -> &'static str {
    match value {
        Temporal::DateTime(_) => OFFSET_DATE_TIME,
        Temporal::Date(_) => LOCAL_DATE,
        Temporal::LocalDateTime(_) => LOCAL_DATE_TIME,
        Temporal::Time(_) => LOCAL_TIME,
        Temporal::Duration(duration) if is_fixed_length(*duration) => DURATION,
        Temporal::Duration(duration) if is_period(*duration) => PERIOD,
        Temporal::Duration(_) => INTERVAL,
    }
}

/// Whether `tag` is one of the tags above.
#[cfg_attr(not(feature = "serde"), allow(dead_code))]
pub(crate) fn is_tag(tag: &str) -> bool {
    [
        OFFSET_DATE_TIME,
        LOCAL_DATE,
        LOCAL_DATE_TIME,
        LOCAL_TIME,
        DURATION,
        PERIOD,
        INTERVAL,
    ]
    .contains(&tag)
}

/// Reads a value written with [`tag_of`] and its canonical text, strictly: the text must parse as
/// the tag's type, and a `gx:Duration` must hold a time part only, a `gx:Period` months and days
/// only. `None` when `tag` is not one of the tags.
pub(crate) fn read_tagged(tag: &str, text: &str) -> Option<TemporalResult<Temporal>> {
    let kind = match tag {
        OFFSET_DATE_TIME => TemporalKind::DateTime,
        LOCAL_DATE => TemporalKind::Date,
        LOCAL_DATE_TIME => TemporalKind::LocalDateTime,
        LOCAL_TIME => TemporalKind::Time,
        DURATION | PERIOD | INTERVAL => TemporalKind::Duration,
        _ => return None,
    };
    let value = match Temporal::parse_strict(kind, text) {
        Ok(value) => value,
        Err(err) => return Some(Err(err)),
    };
    let fits = match (tag, value) {
        (DURATION, Temporal::Duration(duration)) => is_fixed_length(duration),
        (PERIOD, Temporal::Duration(duration)) => is_period(duration),
        (INTERVAL, Temporal::Duration(duration)) => {
            !is_fixed_length(duration) && !is_period(duration)
        }
        _ => true,
    };
    Some(if fits {
        Ok(value)
    } else {
        Err(TemporalError::parse(
            text,
            "duration",
            "the parts do not fit the tag (gx:Duration: no months or days; gx:Period: no time \
             part; graphersal:Interval: both)",
        ))
    })
}
