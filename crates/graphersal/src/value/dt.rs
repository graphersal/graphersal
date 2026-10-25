use std::fmt;

/// The `DT` token: a unit of time (`dateAdd`, `dateDiff`, `date_trunc`) or a part of a temporal
/// value (`date_part`).
///
/// TinkerPop 3.8 defines `DT.second`, `DT.minute`, `DT.hour` and `DT.day`; Graphersal adds the
/// smaller units, the calendar units from `week` to `millennium` (PostgreSQL `date_trunc` /
/// `extract` field names) and the parts that are not units (`day_of_week`, `iso_day_of_week`,
/// `day_of_year`, `iso_year`, `epoch`, `offset`), which only `date_part` accepts.
///
/// - Units below a day ([`Nanosecond`](DT::Nanosecond) to [`Hour`](DT::Hour)) are fixed
///   lengths: adding them moves the instant.
/// - [`Day`](DT::Day) and [`Week`](DT::Week) add calendar days: in a time zone they keep the
///   wall-clock time across a daylight saving change.
/// - [`Month`](DT::Month) and larger add months, clamping the day to the end of the target month
///   (January 31 plus one month is February 28 or 29).
///
/// `#[non_exhaustive]`: more parts (a Julian day, ...) may be added.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DT {
    /// One nanosecond (the resolution of every temporal value with a time).
    Nanosecond,
    /// One microsecond.
    Microsecond,
    /// One millisecond.
    Millisecond,
    /// One second (TinkerPop `DT.second`).
    Second,
    /// One minute (TinkerPop `DT.minute`).
    Minute,
    /// One hour (TinkerPop `DT.hour`).
    Hour,
    /// One calendar day (TinkerPop `DT.day`).
    Day,
    /// Seven calendar days; an ISO 8601 week (Monday first) for truncation and parts.
    Week,
    /// One calendar month.
    Month,
    /// Three calendar months.
    Quarter,
    /// Twelve calendar months.
    Year,
    /// Ten years.
    Decade,
    /// A hundred years (centuries start at year 1: 2001-01-01 starts the 21st).
    Century,
    /// A thousand years (millennia start at year 1: 2001-01-01 starts the 3rd).
    Millennium,
    /// Part only: the day of the week, 0 = Sunday to 6 = Saturday (PostgreSQL `dow`).
    DayOfWeek,
    /// Part only: the ISO day of the week, 1 = Monday to 7 = Sunday (PostgreSQL `isodow`).
    IsoDayOfWeek,
    /// Part only: the day of the year, 1-366 (PostgreSQL `doy`).
    DayOfYear,
    /// Part only: the ISO 8601 week-numbering year (PostgreSQL `isoyear`).
    IsoYear,
    /// Part only: seconds since 1970-01-01T00:00:00Z as a float (PostgreSQL `epoch`).
    Epoch,
    /// Part only: the UTC offset in seconds of a `datetime`.
    Offset,
}

impl DT {
    /// Every token, units first, in the order of the enum.
    pub const ALL: &'static [DT] = &[
        DT::Nanosecond,
        DT::Microsecond,
        DT::Millisecond,
        DT::Second,
        DT::Minute,
        DT::Hour,
        DT::Day,
        DT::Week,
        DT::Month,
        DT::Quarter,
        DT::Year,
        DT::Decade,
        DT::Century,
        DT::Millennium,
        DT::DayOfWeek,
        DT::IsoDayOfWeek,
        DT::DayOfYear,
        DT::IsoYear,
        DT::Epoch,
        DT::Offset,
    ];

    /// The snake_case name of the token (`second`, `day_of_week`, ...).
    pub fn name(self) -> &'static str {
        match self {
            DT::Nanosecond => "nanosecond",
            DT::Microsecond => "microsecond",
            DT::Millisecond => "millisecond",
            DT::Second => "second",
            DT::Minute => "minute",
            DT::Hour => "hour",
            DT::Day => "day",
            DT::Week => "week",
            DT::Month => "month",
            DT::Quarter => "quarter",
            DT::Year => "year",
            DT::Decade => "decade",
            DT::Century => "century",
            DT::Millennium => "millennium",
            DT::DayOfWeek => "day_of_week",
            DT::IsoDayOfWeek => "iso_day_of_week",
            DT::DayOfYear => "day_of_year",
            DT::IsoYear => "iso_year",
            DT::Epoch => "epoch",
            DT::Offset => "offset",
        }
    }

    /// The camelCase name of the token (`second`, `dayOfWeek`, ...).
    pub fn camel_name(self) -> &'static str {
        match self {
            DT::DayOfWeek => "dayOfWeek",
            DT::IsoDayOfWeek => "isoDayOfWeek",
            DT::DayOfYear => "dayOfYear",
            DT::IsoYear => "isoYear",
            DT::Nanosecond
            | DT::Microsecond
            | DT::Millisecond
            | DT::Second
            | DT::Minute
            | DT::Hour
            | DT::Day
            | DT::Week
            | DT::Month
            | DT::Quarter
            | DT::Year
            | DT::Decade
            | DT::Century
            | DT::Millennium
            | DT::Epoch
            | DT::Offset => self.name(),
        }
    }

    /// The token of a snake_case or camelCase name; `None` for any other text.
    pub fn from_name(name: &str) -> Option<DT> {
        DT::ALL
            .iter()
            .copied()
            .find(|dt| dt.name() == name || dt.camel_name() == name)
    }

    /// Whether the token is a unit of time (usable by `dateAdd`, `dateDiff` and `date_trunc`),
    /// not only a part.
    pub fn is_unit(self) -> bool {
        !matches!(
            self,
            DT::DayOfWeek | DT::IsoDayOfWeek | DT::DayOfYear | DT::IsoYear | DT::Epoch | DT::Offset
        )
    }

    /// The fixed length of a unit below a day, in nanoseconds; `None` for calendar units and
    /// parts.
    pub(crate) fn fixed_nanos(self) -> Option<i64> {
        match self {
            DT::Nanosecond => Some(1),
            DT::Microsecond => Some(1_000),
            DT::Millisecond => Some(1_000_000),
            DT::Second => Some(1_000_000_000),
            DT::Minute => Some(60_000_000_000),
            DT::Hour => Some(3_600_000_000_000),
            DT::Day
            | DT::Week
            | DT::Month
            | DT::Quarter
            | DT::Year
            | DT::Decade
            | DT::Century
            | DT::Millennium
            | DT::DayOfWeek
            | DT::IsoDayOfWeek
            | DT::DayOfYear
            | DT::IsoYear
            | DT::Epoch
            | DT::Offset => None,
        }
    }

    /// The number of calendar days of a day unit (`day` 1, `week` 7); `None` otherwise.
    pub(crate) fn calendar_days(self) -> Option<i64> {
        match self {
            DT::Day => Some(1),
            DT::Week => Some(7),
            DT::Nanosecond
            | DT::Microsecond
            | DT::Millisecond
            | DT::Second
            | DT::Minute
            | DT::Hour
            | DT::Month
            | DT::Quarter
            | DT::Year
            | DT::Decade
            | DT::Century
            | DT::Millennium
            | DT::DayOfWeek
            | DT::IsoDayOfWeek
            | DT::DayOfYear
            | DT::IsoYear
            | DT::Epoch
            | DT::Offset => None,
        }
    }

    /// The number of months of a month unit (`month` 1 .. `millennium` 12000); `None` otherwise.
    pub(crate) fn calendar_months(self) -> Option<i64> {
        match self {
            DT::Month => Some(1),
            DT::Quarter => Some(3),
            DT::Year => Some(12),
            DT::Decade => Some(120),
            DT::Century => Some(1_200),
            DT::Millennium => Some(12_000),
            DT::Nanosecond
            | DT::Microsecond
            | DT::Millisecond
            | DT::Second
            | DT::Minute
            | DT::Hour
            | DT::Day
            | DT::Week
            | DT::DayOfWeek
            | DT::IsoDayOfWeek
            | DT::DayOfYear
            | DT::IsoYear
            | DT::Epoch
            | DT::Offset => None,
        }
    }
}

impl fmt::Display for DT {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::DT;

    #[test]
    fn names_round_trip_in_both_spellings() {
        for &dt in DT::ALL {
            assert_eq!(DT::from_name(dt.name()), Some(dt));
            assert_eq!(DT::from_name(dt.camel_name()), Some(dt));
            assert_eq!(dt.to_string(), dt.name());
        }
        assert_eq!(DT::DayOfWeek.camel_name(), "dayOfWeek");
        assert_eq!(DT::from_name("Second"), None);
    }

    #[test]
    fn units_and_parts() {
        assert!(DT::Second.is_unit());
        assert!(DT::Millennium.is_unit());
        assert!(!DT::Epoch.is_unit());
        assert_eq!(DT::Hour.fixed_nanos(), Some(3_600_000_000_000));
        assert_eq!(DT::Nanosecond.fixed_nanos(), Some(1));
        assert!(DT::Nanosecond.is_unit());
        assert_eq!(DT::Week.calendar_days(), Some(7));
        assert_eq!(DT::Millennium.calendar_months(), Some(12_000));
    }
}
