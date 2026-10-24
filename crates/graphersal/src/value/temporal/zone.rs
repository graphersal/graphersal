//! Time zones: a fixed UTC offset or an IANA zone of the bundled database (`jiff-tzdb`, the same
//! rules on every machine and in wasm32; they change only with a Graphersal release).
//!
//! A zone is never stored in a value: operations take one (`atZone("Europe/Berlin")`,
//! `dateAdd(DT.day, 1, "Europe/Berlin")`) and the result is a [`DateTime`] carrying the zone's
//! offset at its instant. A wall-clock time the zone skips (a spring-forward gap) moves forward
//! by the gap; one it repeats (a fall-back fold) takes the earlier offset, as PostgreSQL and
//! `java.time` do ("compatible" disambiguation).

use super::civil::{self, NANOS_PER_SECOND};
use super::{DateTime, LocalDateTime, TemporalError, TemporalResult};

/// The longest accepted zone name (IANA names are far shorter).
const MAX_ZONE_NAME: usize = 128;

/// A time zone argument.
#[derive(Clone, Debug)]
pub(crate) enum Zone {
    /// A fixed offset from UTC in seconds.
    Fixed(i32),
    /// An IANA zone (`Europe/Berlin`, `UTC`, ...).
    Iana(jiff::tz::TimeZone),
}

impl Zone {
    /// `Z`, an offset (`+02:00`) or an IANA name of the bundled database.
    pub(crate) fn parse(name: &str) -> TemporalResult<Zone> {
        if matches!(name.as_bytes().first(), Some(b'+' | b'-')) || name == "Z" || name == "z" {
            return super::parse::offset(name).map(Zone::Fixed);
        }
        if name.is_empty() || name.len() > MAX_ZONE_NAME {
            return Err(TemporalError::unknown_zone(name));
        }
        jiff::tz::TimeZone::get(name)
            .map(Zone::Iana)
            .map_err(|_| TemporalError::unknown_zone(name))
    }

    /// The zone's offset at the instant of `value`, in seconds.
    pub(crate) fn offset_at(&self, value: DateTime) -> TemporalResult<i32> {
        match self {
            Zone::Fixed(offset) => Ok(*offset),
            Zone::Iana(tz) => {
                let timestamp = jiff::Timestamp::from_nanosecond(value.epoch_nanos())
                    .map_err(|_| TemporalError::out_of_range("the date-time"))?;
                Ok(tz.to_offset(timestamp).seconds())
            }
        }
    }

    /// The same instant written with the zone's offset at it.
    pub(crate) fn at(&self, value: DateTime) -> TemporalResult<DateTime> {
        value.with_offset(self.offset_at(value)?)
    }

    /// The instant of a wall-clock time in this zone (gap: forward, fold: earlier offset).
    pub(crate) fn resolve(&self, local: LocalDateTime) -> TemporalResult<DateTime> {
        match self {
            Zone::Fixed(offset) => DateTime::from_local(local, *offset),
            Zone::Iana(tz) => {
                let (days, of_day) = civil::split_nanos(local.nanos());
                let (year, month, day) = civil::civil_from_days(days as i32);
                let seconds = of_day / NANOS_PER_SECOND;
                let nanos = of_day % NANOS_PER_SECOND;
                let out_of_range = |_| TemporalError::out_of_range("the date-time");
                let civil_value = jiff::civil::DateTime::new(
                    year as i16,
                    month as i8,
                    day as i8,
                    (seconds / 3600) as i8,
                    (seconds / 60 % 60) as i8,
                    (seconds % 60) as i8,
                    nanos as i32,
                )
                .map_err(out_of_range)?;
                let timestamp = tz
                    .to_ambiguous_timestamp(civil_value)
                    .compatible()
                    .map_err(out_of_range)?;
                let offset = tz.to_offset(timestamp).seconds();
                DateTime::from_nanos(timestamp.as_nanosecond(), offset)
            }
        }
    }
}
