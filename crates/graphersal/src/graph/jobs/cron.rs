//! The standard 5-field cron schedule of a job ([`CronSchedule`]): `minute hour day-of-month
//! month day-of-week`, each field `*`, a number, a range `a-b`, a step `*/n` or `a-b/n`, or a
//! comma list of those; months and weekdays also by name (`jan`..`dec`, `sun`..`sat`; 0 and 7
//! are both Sunday). As in Vixie cron, when both day fields are restricted a day matches if
//! EITHER does. Times are wall-clock times in the scheduler's time zone.

use std::fmt;

/// A parsed 5-field cron schedule.
///
/// ```
/// use graphersal::jobs::CronSchedule;
///
/// let every_quarter_hour: CronSchedule = "*/15 * * * *".parse().unwrap();
/// assert_eq!(every_quarter_hour.to_string(), "*/15 * * * *");
/// assert!("61 * * * *".parse::<CronSchedule>().is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronSchedule {
    text: String,
    minutes: u64,
    hours: u32,
    days: u32,
    months: u16,
    weekdays: u8,
    /// Whether the day-of-month field was `*` (then only the weekday restricts the day).
    any_day: bool,
    /// Whether the day-of-week field was `*`.
    any_weekday: bool,
}

/// Why a cron text is not a schedule: the field and what is wrong with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronError {
    /// The schedule text.
    pub text: String,
    /// What is wrong (names the field).
    pub reason: String,
}

impl fmt::Display for CronError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid cron schedule '{}': {} (the format is 'minute hour day-of-month month \
             day-of-week', e.g. '*/5 * * * *' for every five minutes)",
            self.text, self.reason
        )
    }
}

impl std::error::Error for CronError {}

const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];
const WEEKDAYS: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

/// Parses one field into a bit set of the values `min..=max`.
fn field(
    text: &str,
    name: &str,
    min: u32,
    max: u32,
    names: &[&str],
    names_from: u32,
) -> Result<u64, String> {
    let value = |part: &str| -> Result<u32, String> {
        let lower = part.to_ascii_lowercase();
        if let Some(i) = names.iter().position(|n| *n == lower) {
            return Ok(i as u32 + names_from);
        }
        part.parse::<u32>()
            .map_err(|_| format!("{name}: '{part}' is not a number"))
    };
    let mut bits = 0u64;
    for item in text.split(',') {
        if item.is_empty() {
            return Err(format!("{name}: an empty list item"));
        }
        let (range, step) = match item.split_once('/') {
            Some((range, step)) => {
                let step: u32 = step
                    .parse()
                    .map_err(|_| format!("{name}: the step '{step}' is not a number"))?;
                if step == 0 {
                    return Err(format!("{name}: a step of 0"));
                }
                (range, step)
            }
            None => (item, 1),
        };
        let (from, to) = if range == "*" {
            (min, max)
        } else if let Some((a, b)) = range.split_once('-') {
            (value(a)?, value(b)?)
        } else {
            let single = value(range)?;
            // `5/10` means from 5 to the end in steps of 10.
            (single, if item.contains('/') { max } else { single })
        };
        if from < min || to > max || from > to {
            return Err(format!(
                "{name}: '{range}' is outside {min}-{max} or reversed"
            ));
        }
        let mut v = from;
        while v <= to {
            bits |= 1 << v;
            v += step;
        }
    }
    Ok(bits)
}

impl std::str::FromStr for CronSchedule {
    type Err = CronError;

    fn from_str(text: &str) -> Result<Self, CronError> {
        let fail = |reason: String| CronError {
            text: text.to_string(),
            reason,
        };
        let fields: Vec<&str> = text.split_whitespace().collect();
        let [minute, hour, day, month, weekday] = fields.as_slice() else {
            return Err(fail(format!("{} fields instead of 5", fields.len())));
        };
        let minutes = field(minute, "minute", 0, 59, &[], 0).map_err(fail)?;
        let hours = field(hour, "hour", 0, 23, &[], 0).map_err(fail)? as u32;
        let days = field(day, "day of month", 1, 31, &[], 0).map_err(fail)? as u32;
        let months = field(month, "month", 1, 12, &MONTHS, 1).map_err(fail)? as u16;
        let mut weekdays = field(weekday, "day of week", 0, 7, &WEEKDAYS, 0).map_err(fail)?;
        // 7 is Sunday too.
        if weekdays & (1 << 7) != 0 {
            weekdays = (weekdays & 0x7f) | 1;
        }
        let schedule = CronSchedule {
            text: fields.join(" "),
            minutes,
            hours,
            days,
            months,
            weekdays: weekdays as u8,
            any_day: *day == "*",
            any_weekday: *weekday == "*",
        };
        // A schedule that never fires (31 February) is refused rather than silently idle.
        if !(1..=12).any(|m| schedule.month_has_day(m)) {
            return Err(fail("no day of the year matches".to_string()));
        }
        Ok(schedule)
    }
}

impl fmt::Display for CronSchedule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

impl CronSchedule {
    /// Whether some day of month `m` (in a leap year) can match the day-of-month field.
    fn month_has_day(&self, m: u32) -> bool {
        if self.months & (1 << m) == 0 {
            return false;
        }
        if self.any_day || !self.any_weekday {
            // `*`, or the weekday field can match any month.
            return true;
        }
        let last = match m {
            2 => 29,
            4 | 6 | 9 | 11 => 30,
            _ => 31,
        };
        (1..=last).any(|d| self.days & (1 << d) != 0)
    }

    /// Whether the calendar day matches (month, then the day fields with cron's OR rule).
    fn day_matches(&self, month: u32, day: u32, weekday: u32) -> bool {
        if self.months & (1 << month) == 0 {
            return false;
        }
        let by_day = self.days & (1 << day) != 0;
        let by_weekday = self.weekdays & (1 << weekday) != 0;
        match (self.any_day, self.any_weekday) {
            (true, true) => true,
            (true, false) => by_weekday,
            (false, true) => by_day,
            (false, false) => by_day || by_weekday,
        }
    }

    /// The first instant strictly after `after` (microseconds since the Unix epoch, UTC) at
    /// which the schedule fires in `zone`, or `None` when none comes within five years.
    /// A wall-clock time skipped by a daylight-saving jump fires at the first valid instant
    /// after the gap; a repeated hour fires once (its first occurrence).
    pub(crate) fn next_after(&self, after: i64, zone: &jiff::tz::TimeZone) -> Option<i64> {
        use jiff::civil::Time;
        use jiff::{Timestamp, tz::Disambiguation};
        let start = Timestamp::from_microsecond(after)
            .ok()?
            .to_zoned(zone.clone());
        let mut date = start.date();
        for _ in 0..(5 * 366) {
            let weekday = date.weekday().to_sunday_zero_offset() as u32;
            if self.day_matches(date.month() as u32, date.day() as u32, weekday) {
                for hour in 0..24u32 {
                    if self.hours & (1 << hour) == 0 {
                        continue;
                    }
                    for minute in 0..60u32 {
                        if self.minutes & (1 << minute) == 0 {
                            continue;
                        }
                        let time = Time::new(hour as i8, minute as i8, 0, 0).ok()?;
                        let civil = date.to_datetime(time);
                        let Ok(zoned) = zone
                            .to_ambiguous_zoned(civil)
                            .disambiguate(Disambiguation::Compatible)
                        else {
                            continue;
                        };
                        let at = zoned.timestamp().as_microsecond();
                        if at > after {
                            return Some(at);
                        }
                    }
                }
            }
            date = date.tomorrow().ok()?;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc() -> jiff::tz::TimeZone {
        jiff::tz::TimeZone::UTC
    }

    /// Microseconds of a UTC wall-clock time.
    fn at(text: &str) -> i64 {
        text.parse::<jiff::Timestamp>().unwrap().as_microsecond()
    }

    fn next(schedule: &str, after: &str, zone: &jiff::tz::TimeZone) -> String {
        let schedule: CronSchedule = schedule.parse().unwrap();
        let next = schedule.next_after(at(after), zone).unwrap();
        jiff::Timestamp::from_microsecond(next).unwrap().to_string()
    }

    #[test]
    fn fields_steps_ranges_lists_and_names() {
        assert_eq!(
            next("* * * * *", "2026-10-25T10:00:30Z", &utc()),
            "2026-10-25T10:01:00Z"
        );
        assert_eq!(
            next("*/15 * * * *", "2026-10-25T10:00:00Z", &utc()),
            "2026-10-25T10:15:00Z"
        );
        assert_eq!(
            next("0 3 * * *", "2026-10-25T10:00:00Z", &utc()),
            "2026-10-26T03:00:00Z"
        );
        assert_eq!(
            next("30 9 1,15 * *", "2026-10-25T10:00:00Z", &utc()),
            "2026-11-01T09:30:00Z"
        );
        // Day of month `*`: only the weekday restricts (the first Monday of January).
        assert_eq!(
            next("0 0 * jan mon", "2026-10-25T10:00:00Z", &utc()),
            "2027-01-04T00:00:00Z"
        );
        // Both restricted: either matches (the 1st, a Friday, comes before the first Monday).
        assert_eq!(
            next("0 0 1 jan mon", "2026-10-25T10:00:00Z", &utc()),
            "2027-01-01T00:00:00Z"
        );
        // 2026-10-25 is a Sunday; 7 is Sunday too.
        assert_eq!(
            next("0 12 * * 7", "2026-10-25T10:00:00Z", &utc()),
            "2026-10-25T12:00:00Z"
        );
        assert_eq!(
            next("0 12 * * mon-fri", "2026-10-25T13:00:00Z", &utc()),
            "2026-10-26T12:00:00Z"
        );
        assert_eq!(
            next("5/20 * * * *", "2026-10-25T10:06:00Z", &utc()),
            "2026-10-25T10:25:00Z"
        );
        assert_eq!(
            next("0 0 29 2 *", "2026-10-25T10:00:00Z", &utc()),
            "2028-02-29T00:00:00Z"
        );
    }

    #[test]
    fn invalid_schedules_are_refused() {
        for text in [
            "* * * *",
            "60 * * * *",
            "* 24 * * *",
            "* * 0 * *",
            "* * * 13 *",
            "*/0 * * * *",
            "5-1 * * * *",
            "x * * * *",
            "0 0 31 2 *",
            "1,,2 * * * *",
        ] {
            let err = text.parse::<CronSchedule>().unwrap_err();
            assert!(err.to_string().contains("invalid cron schedule"), "{text}");
        }
    }

    #[test]
    fn local_time_and_daylight_saving() {
        let berlin = jiff::tz::TimeZone::get("Europe/Berlin").unwrap();
        // 03:00 local in winter (UTC+1) is 02:00 UTC.
        assert_eq!(
            next("0 3 * * *", "2026-12-01T00:00:00Z", &berlin),
            "2026-12-01T02:00:00Z"
        );
        // 2027-03-28: 02:00-03:00 is skipped; a 02:30 job fires right after the gap (03:30
        // CEST = 01:30 UTC).
        assert_eq!(
            next("30 2 * * *", "2027-03-27T12:00:00Z", &berlin),
            "2027-03-28T01:30:00Z"
        );
        // 2026-10-25: 02:00-03:00 is repeated; a 02:30 job fires once, at the first one.
        assert_eq!(
            next("30 2 * * *", "2026-10-24T12:00:00Z", &berlin),
            "2026-10-25T00:30:00Z"
        );
        assert_eq!(
            next("30 2 * * *", "2026-10-25T00:30:00Z", &berlin),
            "2026-10-26T01:30:00Z"
        );
    }
}
