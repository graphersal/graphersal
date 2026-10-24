//! Time zones: a fixed UTC offset or an IANA zone of the bundled database (`jiff-tzdb`, the same
//! rules on every machine and in wasm32; they change only with a Graphersal release).
//!
//! An operation that takes a zone argument (`atZone("Europe/Berlin")`,
//! `dateAdd(DT.day, 1, "Europe/Berlin")`) gives a [`DateTime`] carrying the zone's offset at its
//! instant and, for an IANA zone, the zone's name (a fixed offset such as `+02:00` gives no
//! name). A `datetime` written with a zone name (`[Europe/Berlin]`, the GQL `timezone` field)
//! keeps the name too. The name is kept as a [`ZoneId`]: a two-byte index into the table of the
//! bundled database's names ([`names`]), so a value stays plain integers. A wall-clock time the
//! zone skips (a spring-forward gap) moves forward by the gap; one it repeats (a fall-back fold)
//! takes the earlier offset, as PostgreSQL and `java.time` do ("compatible" disambiguation).

use std::cmp::Ordering;
use std::num::NonZeroU16;
use std::sync::OnceLock;

use super::civil::{self, NANOS_PER_SECOND};
use super::{DateTime, LocalDateTime, TemporalError, TemporalResult};

/// The longest accepted zone name (IANA names are far shorter).
const MAX_ZONE_NAME: usize = 128;

/// The names of the bundled database's zones, sorted ignoring ASCII case (the order the
/// database itself searches in), built once. A [`ZoneId`] is an index into it plus one.
fn names() -> &'static [Box<str>] {
    static NAMES: OnceLock<Vec<Box<str>>> = OnceLock::new();
    NAMES.get_or_init(|| {
        let mut names: Vec<Box<str>> = jiff::tz::db()
            .available()
            .map(|name| Box::from(name.as_str()))
            .collect();
        names.sort_by(|a, b| cmp_ignore_ascii_case(a, b));
        names.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
        names
    })
}

fn cmp_ignore_ascii_case(a: &str, b: &str) -> Ordering {
    let a = a.bytes().map(|b| b.to_ascii_lowercase());
    let b = b.bytes().map(|b| b.to_ascii_lowercase());
    a.cmp(b)
}

/// An IANA zone name kept in a `datetime`: an index into the bundled database's names (stable
/// within one Graphersal build; never written to a file, which holds the name itself).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct ZoneId(NonZeroU16);

impl ZoneId {
    /// The id of an IANA name of the bundled database (ASCII case ignored); `None` for any
    /// other text.
    pub(crate) fn lookup(name: &str) -> Option<ZoneId> {
        let names = names();
        let index = names
            .binary_search_by(|known| cmp_ignore_ascii_case(known, name))
            .ok()?;
        u16::try_from(index + 1)
            .ok()
            .and_then(NonZeroU16::new)
            .map(ZoneId)
    }

    /// The zone's name as the database spells it (`Europe/Berlin`).
    pub(crate) fn name(self) -> &'static str {
        names()
            .get(self.0.get() as usize - 1)
            .map_or("UTC", |name| name)
    }

    /// The raw id (a key part of a property index).
    pub(crate) fn raw(self) -> u16 {
        self.0.get()
    }

    /// The zone's rules.
    pub(crate) fn zone(self) -> TemporalResult<Zone> {
        let name = self.name();
        jiff::tz::TimeZone::get(name)
            .map(|tz| Zone::Iana(self, tz))
            .map_err(|_| TemporalError::unknown_zone(name))
    }
}

/// A time zone argument.
#[derive(Clone, Debug)]
pub(crate) enum Zone {
    /// A fixed offset from UTC in seconds.
    Fixed(i32),
    /// An IANA zone (`Europe/Berlin`, `UTC`, ...): its name and its rules.
    Iana(ZoneId, jiff::tz::TimeZone),
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
        match ZoneId::lookup(name) {
            Some(id) => id.zone(),
            None => Err(TemporalError::unknown_zone(name)),
        }
    }

    /// The IANA name of the zone; `None` for a fixed offset.
    pub(crate) fn id(&self) -> Option<ZoneId> {
        match self {
            Zone::Fixed(_) => None,
            Zone::Iana(id, _) => Some(*id),
        }
    }

    /// The zone's offset at the instant of `value`, in seconds.
    pub(crate) fn offset_at(&self, value: DateTime) -> TemporalResult<i32> {
        match self {
            Zone::Fixed(offset) => Ok(*offset),
            Zone::Iana(_, tz) => {
                let timestamp = jiff::Timestamp::from_nanosecond(value.epoch_nanos())
                    .map_err(|_| TemporalError::out_of_range("the date-time"))?;
                Ok(tz.to_offset(timestamp).seconds())
            }
        }
    }

    /// The same instant written with the zone's offset at it (no zone name).
    pub(crate) fn at(&self, value: DateTime) -> TemporalResult<DateTime> {
        value.with_offset(self.offset_at(value)?)
    }

    /// [`at`](Self::at), keeping the zone's name in the value when it is an IANA zone.
    pub(crate) fn at_named(&self, value: DateTime) -> TemporalResult<DateTime> {
        Ok(self.at(value)?.with_zone_id(self.id()))
    }

    /// The instant of a wall-clock time in this zone (gap: forward, fold: earlier offset), with
    /// the zone's offset and no zone name.
    pub(crate) fn resolve(&self, local: LocalDateTime) -> TemporalResult<DateTime> {
        match self {
            Zone::Fixed(offset) => DateTime::from_local(local, *offset),
            Zone::Iana(_, tz) => {
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

    /// [`resolve`](Self::resolve), keeping the zone's name in the value when it is an IANA zone.
    pub(crate) fn resolve_named(&self, local: LocalDateTime) -> TemporalResult<DateTime> {
        Ok(self.resolve(local)?.with_zone_id(self.id()))
    }
}
