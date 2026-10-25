//! Dates and times in scripts: the six temporal value types carried natively (`date`,
//! `localdatetime`, `datetime`, `time`, `offsettime`, `duration`), their constructors (`datetime("..")`,
//! TinkerPop's `DateTime("..")`, ...), `now()`/`today()`, getters and operators, and the `DT`
//! token (`DT.day`/`DT::day`, the bare `second`/`minute`/`hour`/`day` of the TinkerPop scenarios).

use crate::script::*;
use crate::value::dt::DT;
use crate::value::temporal::{
    Date, DateTime, Duration, LocalDateTime, OffsetTime, Temporal, TemporalError, Time, arith,
    format, zone::Zone,
};

/// A temporal value as a script value: one Rhai type per temporal type (`type_of()` is
/// `datetime`, `date`, ...), so operators and getters are resolved by type.
pub(crate) fn temporal_to_dynamic(value: Temporal) -> Dynamic {
    match value {
        Temporal::Date(v) => Dynamic::from(v),
        Temporal::LocalDateTime(v) => Dynamic::from(v),
        Temporal::DateTime(v) => Dynamic::from(v),
        Temporal::Time(v) => Dynamic::from(v),
        Temporal::Duration(v) => Dynamic::from(v),
        Temporal::OffsetTime(v) => Dynamic::from(v),
    }
}

/// The temporal value a script value holds, if any.
pub(crate) fn dynamic_temporal(arg: &Dynamic) -> Option<Temporal> {
    if let Some(v) = arg.read_lock::<DateTime>() {
        return Some(Temporal::DateTime(*v));
    }
    if let Some(v) = arg.read_lock::<Date>() {
        return Some(Temporal::Date(*v));
    }
    if let Some(v) = arg.read_lock::<LocalDateTime>() {
        return Some(Temporal::LocalDateTime(*v));
    }
    if let Some(v) = arg.read_lock::<Time>() {
        return Some(Temporal::Time(*v));
    }
    if let Some(v) = arg.read_lock::<Duration>() {
        return Some(Temporal::Duration(*v));
    }
    if let Some(v) = arg.read_lock::<OffsetTime>() {
        return Some(Temporal::OffsetTime(*v));
    }
    // A host may hand the enum itself to a script.
    arg.read_lock::<Temporal>().map(|v| *v)
}

/// Whether a script value is a temporal value.
pub(crate) fn is_temporal(arg: &Dynamic) -> bool {
    arg.is::<DateTime>()
        || arg.is::<Date>()
        || arg.is::<LocalDateTime>()
        || arg.is::<Time>()
        || arg.is::<Duration>()
        || arg.is::<OffsetTime>()
        || arg.is::<Temporal>()
}

/// The script error of a failed temporal operation of `name` (a constructor, an operator).
fn temporal_failure(name: &'static str, error: TemporalError) -> Box<rhai::EvalAltResult> {
    script_error(Box::new(crate::error::TraverserError::Value(
        crate::error::ValueError::Temporal { step: name, error },
    )))
}

type Rhai<T> = Result<T, Box<rhai::EvalAltResult>>;

/// `datetime(text)`: an ISO 8601 / RFC 3339 date-time (TinkerPop's `datetime()` literal:
/// without an offset the text is UTC, a date alone is midnight UTC).
pub(crate) fn script_datetime(text: &str) -> Rhai<DateTime> {
    text.parse()
        .map_err(|error| temporal_failure("datetime", error))
}

fn script_date(text: &str) -> Rhai<Date> {
    text.parse()
        .map_err(|error| temporal_failure("date", error))
}

fn script_local(text: &str) -> Rhai<LocalDateTime> {
    text.parse()
        .map_err(|error| temporal_failure("localdatetime", error))
}

fn script_time(text: &str) -> Rhai<Time> {
    text.parse()
        .map_err(|error| temporal_failure("time", error))
}

fn script_offset_time(text: &str) -> Rhai<OffsetTime> {
    text.parse()
        .map_err(|error| temporal_failure("offsettime", error))
}

/// `d.timezone`: the zone name of a `datetime` written with one (`Europe/Berlin`), else its
/// offset text (`+02:00`, `Z`), as GQL's `x.timezone` gives it.
fn timezone_text(value: &DateTime) -> String {
    match value.zone_name() {
        Some(name) => name.to_string(),
        None => offset_text(value.offset_seconds()),
    }
}

/// An offset as its text (`+02:00`, `Z`).
fn offset_text(offset_seconds: i32) -> String {
    let mut text = String::new();
    // Writing into a `String` never fails.
    let _ = format::write_offset(&mut text, offset_seconds);
    text
}

fn script_duration(text: &str) -> Rhai<Duration> {
    text.parse()
        .map_err(|error| temporal_failure("duration", error))
}

/// `d.micros`: the time part of a duration in whole microseconds (the nanoseconds below them
/// cut toward zero); beyond an integer (about 292,000 years) a range error.
fn duration_micros(duration: &Duration) -> Rhai<i64> {
    duration.micros().ok_or_else(|| {
        temporal_failure(
            "micros",
            TemporalError::out_of_range("the microseconds of the duration"),
        )
    })
}

/// `d.nanos`: the time part of a duration in nanoseconds; beyond an integer (about 292 years)
/// a range error.
fn duration_nanos(duration: &Duration) -> Rhai<i64> {
    i64::try_from(duration.nanos()).map_err(|_| {
        temporal_failure(
            "nanos",
            TemporalError::out_of_range("the nanoseconds of the duration"),
        )
    })
}

/// `today()`: the date of `now()` in UTC (PostgreSQL `current_date` in a UTC session);
/// `today(zone)`: its date in that zone (an offset such as `+02:00` or an IANA name).
fn script_today(ctx: &NativeCallContext, zone: Option<&str>) -> Rhai<Date> {
    let now = access::now_of(ctx);
    let at = match zone {
        None => now.with_offset(0),
        Some(name) => Zone::parse(name).and_then(|zone| zone.at(now)),
    };
    let at = at.map_err(|error| temporal_failure("today", error))?;
    Ok(at.local().date())
}

/// `a - b` of two values of one type (PostgreSQL): two `datetime`s or `localdatetime`s give the
/// elapsed time as days of 24 hours and a time part, two `date`s the days between them, two
/// `time`s the elapsed time, two `offsettime`s the elapsed time between their UTC times of day.
fn difference(a: Temporal, b: Temporal) -> Result<Duration, TemporalError> {
    let elapsed: i128 = match (a, b) {
        (Temporal::DateTime(a), Temporal::DateTime(b)) => a.epoch_nanos() - b.epoch_nanos(),
        (Temporal::LocalDateTime(a), Temporal::LocalDateTime(b)) => a.nanos() - b.nanos(),
        (Temporal::Date(a), Temporal::Date(b)) => {
            let days = a.days_since_epoch() as i64 - b.days_since_epoch() as i64;
            return Ok(Duration::new(0, days as i32, 0));
        }
        (Temporal::Time(a), Temporal::Time(b)) => {
            return Duration::from_nanos(a.nanos_of_day() as i128 - b.nanos_of_day() as i128);
        }
        (Temporal::OffsetTime(a), Temporal::OffsetTime(b)) => {
            return Duration::from_nanos(a.utc_nanos() as i128 - b.utc_nanos() as i128);
        }
        (Temporal::Duration(a), Temporal::Duration(b)) => {
            return b.checked_neg().and_then(|b| a.checked_add(b));
        }
        _ => {
            return Err(TemporalError::unsupported(
                format!("subtracting a {}", b.kind()),
                a.kind(),
            ));
        }
    };
    const DAY: i128 = 86_400_000_000_000;
    let days = elapsed / DAY;
    let nanos = elapsed % DAY;
    match i32::try_from(days) {
        Ok(days) => Duration::with_nanos(0, days, nanos),
        Err(_) => Err(TemporalError::out_of_range("the result")),
    }
}

/// `value + duration`.
fn plus(value: Temporal, duration: Duration) -> Rhai<Dynamic> {
    arith::add_duration(value, duration, None)
        .map(temporal_to_dynamic)
        .map_err(|error| temporal_failure("+", error))
}

/// `value - duration`.
fn minus(value: Temporal, duration: Duration) -> Rhai<Dynamic> {
    let negated = duration
        .checked_neg()
        .map_err(|error| temporal_failure("-", error))?;
    arith::add_duration(value, negated, None)
        .map(temporal_to_dynamic)
        .map_err(|error| temporal_failure("-", error))
}

/// Registers the six types with their printing, comparison, arithmetic and getters.
macro_rules! register_temporal_type {
    ($registry:expr, $ty:ty, $name:literal, $variant:ident) => {{
        $registry
            .register_type_with_name::<$ty>($name)
            .register_fn_as(&["to_string", "to_debug"], |v: &mut $ty| {
                crate::value::literal::rhai_temporal(&Temporal::$variant(*v))
            })
            // The canonical text (`2023-08-02`), what the `as_string()` step gives.
            .register_fn_as(&["as_string", "asString"], |v: &mut $ty| {
                Temporal::$variant(*v).to_string()
            })
            .register_fn("==", |a: $ty, b: $ty| a == b)
            .register_fn("!=", |a: $ty, b: $ty| a != b)
            .register_fn("<", |a: $ty, b: $ty| a < b)
            .register_fn("<=", |a: $ty, b: $ty| a <= b)
            .register_fn(">", |a: $ty, b: $ty| a > b)
            .register_fn(">=", |a: $ty, b: $ty| a >= b)
            .register_fn("-", |a: $ty, b: $ty| {
                difference(Temporal::$variant(a), Temporal::$variant(b))
                    .map_err(|error| temporal_failure("-", error))
            })
            // A temporal value never equals a value of another type (a string, a number).
            .register_fn("==", |_: $ty, _: rhai::ImmutableString| false)
            .register_fn("!=", |_: $ty, _: rhai::ImmutableString| true);
    }};
}

/// Registers every temporal script function.
pub(crate) fn register_temporal(registry: &mut crate::script::register::Registry) {
    register_temporal_type!(registry, DateTime, "datetime", DateTime);
    register_temporal_type!(registry, Date, "date", Date);
    register_temporal_type!(registry, LocalDateTime, "localdatetime", LocalDateTime);
    register_temporal_type!(registry, Time, "time", Time);
    register_temporal_type!(registry, Duration, "duration", Duration);
    register_temporal_type!(registry, OffsetTime, "offsettime", OffsetTime);

    // `+`/`-` with a duration (PostgreSQL `value + interval`); duration + duration.
    registry
        .register_fn("+", |a: DateTime, d: Duration| {
            plus(Temporal::DateTime(a), d)
        })
        .register_fn("+", |a: Date, d: Duration| plus(Temporal::Date(a), d))
        .register_fn("+", |a: LocalDateTime, d: Duration| {
            plus(Temporal::LocalDateTime(a), d)
        })
        .register_fn("+", |a: Time, d: Duration| plus(Temporal::Time(a), d))
        .register_fn("+", |a: OffsetTime, d: Duration| {
            plus(Temporal::OffsetTime(a), d)
        })
        .register_fn("+", |a: Duration, d: Duration| {
            plus(Temporal::Duration(a), d)
        })
        .register_fn("-", |a: DateTime, d: Duration| {
            minus(Temporal::DateTime(a), d)
        })
        .register_fn("-", |a: Date, d: Duration| minus(Temporal::Date(a), d))
        .register_fn("-", |a: LocalDateTime, d: Duration| {
            minus(Temporal::LocalDateTime(a), d)
        })
        .register_fn("-", |a: Time, d: Duration| minus(Temporal::Time(a), d))
        .register_fn("-", |a: OffsetTime, d: Duration| {
            minus(Temporal::OffsetTime(a), d)
        })
        .register_fn("-", |d: Duration| {
            d.checked_neg()
                .map_err(|error| temporal_failure("-", error))
        });

    // Constructors: TinkerPop's `datetime(..)`/`DateTime(..)` and one per type.
    registry
        .register_fn_as(&["datetime", "DateTime"], |text: &str| {
            script_datetime(text)
        })
        .register_fn("date", |text: &str| script_date(text))
        .register_fn_as(&["localdatetime"], |text: &str| script_local(text))
        .register_fn("time", |text: &str| script_time(text))
        .register_fn_as(&["offsettime"], |text: &str| script_offset_time(text))
        .register_fn("duration", |text: &str| script_duration(text))
        .register_fn("now", |ctx: NativeCallContext| access::now_of(&ctx))
        .register_fn("today", |ctx: NativeCallContext| script_today(&ctx, None))
        .register_fn("today", |ctx: NativeCallContext, zone: &str| {
            script_today(&ctx, Some(zone))
        });

    // Getters (both spellings where they differ).
    registry
        .register_get("year", |v: &mut DateTime| v.local().date().year() as i64)
        .register_get("month", |v: &mut DateTime| v.local().date().month() as i64)
        .register_get("day", |v: &mut DateTime| v.local().date().day() as i64)
        .register_get("hour", |v: &mut DateTime| v.local().time().hour() as i64)
        .register_get("minute", |v: &mut DateTime| {
            v.local().time().minute() as i64
        })
        .register_get("second", |v: &mut DateTime| {
            v.local().time().second() as i64
        })
        .register_get("microsecond", |v: &mut DateTime| {
            v.local().time().microsecond() as i64
        })
        .register_get("nanosecond", |v: &mut DateTime| {
            v.local().time().nanosecond() as i64
        })
        .register_get("offset", |v: &mut DateTime| v.offset_seconds() as i64)
        .register_get("timezone", |v: &mut DateTime| timezone_text(v))
        .register_get("epoch_millis", |v: &mut DateTime| v.epoch_millis())
        .register_get("epochMillis", |v: &mut DateTime| v.epoch_millis())
        .register_get("year", |v: &mut LocalDateTime| v.date().year() as i64)
        .register_get("month", |v: &mut LocalDateTime| v.date().month() as i64)
        .register_get("day", |v: &mut LocalDateTime| v.date().day() as i64)
        .register_get("hour", |v: &mut LocalDateTime| v.time().hour() as i64)
        .register_get("minute", |v: &mut LocalDateTime| v.time().minute() as i64)
        .register_get("second", |v: &mut LocalDateTime| v.time().second() as i64)
        .register_get("microsecond", |v: &mut LocalDateTime| {
            v.time().microsecond() as i64
        })
        .register_get("nanosecond", |v: &mut LocalDateTime| {
            v.time().nanosecond() as i64
        })
        .register_get("year", |v: &mut Date| v.year() as i64)
        .register_get("month", |v: &mut Date| v.month() as i64)
        .register_get("day", |v: &mut Date| v.day() as i64)
        .register_get("hour", |v: &mut Time| v.hour() as i64)
        .register_get("minute", |v: &mut Time| v.minute() as i64)
        .register_get("second", |v: &mut Time| v.second() as i64)
        .register_get("microsecond", |v: &mut Time| v.microsecond() as i64)
        .register_get("nanosecond", |v: &mut Time| v.nanosecond() as i64)
        .register_get("hour", |v: &mut OffsetTime| v.time().hour() as i64)
        .register_get("minute", |v: &mut OffsetTime| v.time().minute() as i64)
        .register_get("second", |v: &mut OffsetTime| v.time().second() as i64)
        .register_get("microsecond", |v: &mut OffsetTime| {
            v.time().microsecond() as i64
        })
        .register_get("nanosecond", |v: &mut OffsetTime| {
            v.time().nanosecond() as i64
        })
        .register_get("offset", |v: &mut OffsetTime| v.offset_seconds() as i64)
        .register_get("timezone", |v: &mut OffsetTime| {
            offset_text(v.offset_seconds())
        })
        .register_get("months", |v: &mut Duration| v.months() as i64)
        .register_get("days", |v: &mut Duration| v.days() as i64)
        .register_get("micros", |v: &mut Duration| duration_micros(v))
        .register_get("nanos", |v: &mut Duration| duration_nanos(v));

    // The `DT` token: `DT.day` (dot, on the scope marker; both spellings of the multi-word
    // names) and `DT::day` (static module); the bare `second`/`minute`/`hour`/`day` are scope
    // constants (`graph_scope_with_options`).
    registry
        .register_type_with_name::<DT>("DT")
        .register_type_with_name::<ScriptDT>("DTTokens");
    for &dt in DT::ALL {
        registry.register_get(dt.name(), move |_: &mut ScriptDT| dt);
        if dt.camel_name() != dt.name() {
            registry.register_get(dt.camel_name(), move |_: &mut ScriptDT| dt);
        }
    }
    registry.register_static_module("DT", exported_module!(DTModule).into());
}

/// Scope marker that makes the dotted spelling `DT.day` resolve (`DT.dayOfWeek` too).
///
/// Mirrors [`ScriptPop`]: the tokens are properties of this marker (dot form) and constants of
/// the static module `DT::day`; the graph scope also binds the bare constants `second`,
/// `minute`, `hour` and `day` (the Gremlin console's `import static DT.*`, which the TinkerPop
/// scenarios use: `dateAdd(hour, 2)`). A script variable of the same name shadows a bare
/// constant for the rest of the scope. The token itself is [`DT`], registered with Rhai as `DT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScriptDT;

#[export_module]
#[allow(non_snake_case, non_upper_case_globals)]
pub(crate) mod DTModule {
    use super::DT;
    use crate::value::literal::TokenLiteral;

    /// `DT.microsecond`.
    pub const microsecond: DT = DT::Microsecond;
    /// `DT.millisecond`.
    pub const millisecond: DT = DT::Millisecond;
    /// `DT.second`.
    pub const second: DT = DT::Second;
    /// `DT.minute`.
    pub const minute: DT = DT::Minute;
    /// `DT.hour`.
    pub const hour: DT = DT::Hour;
    /// `DT.day`.
    pub const day: DT = DT::Day;
    /// `DT.week`.
    pub const week: DT = DT::Week;
    /// `DT.month`.
    pub const month: DT = DT::Month;
    /// `DT.quarter`.
    pub const quarter: DT = DT::Quarter;
    /// `DT.year`.
    pub const year: DT = DT::Year;
    /// `DT.decade`.
    pub const decade: DT = DT::Decade;
    /// `DT.century`.
    pub const century: DT = DT::Century;
    /// `DT.millennium`.
    pub const millennium: DT = DT::Millennium;
    /// `DT.day_of_week`.
    pub const day_of_week: DT = DT::DayOfWeek;
    /// `DT.iso_day_of_week`.
    pub const iso_day_of_week: DT = DT::IsoDayOfWeek;
    /// `DT.day_of_year`.
    pub const day_of_year: DT = DT::DayOfYear;
    /// `DT.iso_year`.
    pub const iso_year: DT = DT::IsoYear;
    /// `DT.epoch`.
    pub const epoch: DT = DT::Epoch;
    /// `DT.offset`.
    pub const offset: DT = DT::Offset;
    /// camelCase alias of `DT::day_of_week`.
    pub const dayOfWeek: DT = DT::DayOfWeek;
    /// camelCase alias of `DT::iso_day_of_week`.
    pub const isoDayOfWeek: DT = DT::IsoDayOfWeek;
    /// camelCase alias of `DT::day_of_year`.
    pub const dayOfYear: DT = DT::DayOfYear;
    /// camelCase alias of `DT::iso_year`.
    pub const isoYear: DT = DT::IsoYear;

    /// Printing
    #[rhai_fn(global, name = "to_string", name = "to_debug", pure)]
    pub fn to_string(dt: &mut DT) -> String {
        dt.token_literal()
    }

    /// Equality of two `DT` tokens.
    #[rhai_fn(global, name = "==", pure)]
    pub fn eq(left: &mut DT, right: DT) -> bool {
        *left == right
    }

    /// Inequality of two `DT` tokens.
    #[rhai_fn(global, name = "!=", pure)]
    pub fn ne(left: &mut DT, right: DT) -> bool {
        *left != right
    }
}
