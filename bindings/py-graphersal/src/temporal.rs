//! The six date and time types (book "Dates and Times") as Python values, both ways:
//!
//! | Graphersal | Python | with digits below the microsecond (needs `pandas`) |
//! |---|---|---|
//! | `datetime` | aware `datetime.datetime`, `tzinfo` = `datetime.timezone(timedelta(seconds=offset))` | aware `pandas.Timestamp`, the same `tzinfo` |
//! | `datetime` with a zone name | aware `datetime.datetime`, `tzinfo` = `zoneinfo.ZoneInfo(name)` (a `ZoneInfo` datetime comes back with its name) | aware `pandas.Timestamp` in the `ZoneInfo` |
//! | `localdatetime` | naive `datetime.datetime` | naive `pandas.Timestamp` |
//! | `date` | `datetime.date` | - |
//! | `time` | naive `datetime.time` | refused (`ValueError`): no Python type holds a time of day in nanoseconds |
//! | `offsettime` | `datetime.time` with a fixed `tzinfo` (`datetime.timezone`); a `time` whose `tzinfo` has no fixed offset (a `ZoneInfo`) is refused | refused (`ValueError`) |
//! | `duration` | `datetime.timedelta` when it has no months and fits one (at most 999999999 days), else [`Interval`] | `pandas.Timedelta` when it has no months and fits one (an int64 of nanoseconds, about 292 years), else [`Interval`] |
//!
//! Python's `datetime` counts in microseconds, the engine in nanoseconds. A value with digits
//! below the microsecond becomes the `pandas` type (subclasses of `datetime` and `timedelta`
//! that hold nanoseconds) when the optional extra `graphersal[pandas]` is installed, else it
//! raises `ImportError` naming the extra: the type depends on the value only, never on what is
//! installed, and a value is never rounded. A `pandas.Timestamp` holds nanoseconds only from
//! 1677-09-21 to 2262-04-11; outside it such a value raises. Parameters take `pandas.Timestamp`
//! and `pandas.Timedelta` with their nanoseconds (as the `datetime` / `timedelta` they are), and
//! a `pytz` zone's name as a `ZoneInfo`'s. The Python objects are built and read through the
//! modules' own API (the datetime C API is not part of the stable ABI this extension is built
//! for).

use graphersal::temporal::{Date, DateTime, Duration, LocalDateTime, OffsetTime, Temporal, Time};
use pyo3::exceptions::{PyImportError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyTuple};

const MICROS_PER_SECOND: i64 = 1_000_000;
const NANOS_PER_DAY: i128 = 86_400_000_000_000;
const MICROS_PER_DAY: i64 = 86_400 * MICROS_PER_SECOND;
/// `datetime.timedelta`'s day limit.
const MAX_TIMEDELTA_DAYS: i64 = 999_999_999;

/// A `duration` with months (a PostgreSQL `interval` such as `P1M`), which `datetime.timedelta`
/// cannot hold: a month has no fixed length. Immutable; equal when all three fields are equal
/// (`P1M` is not `P30D`, as in the engine).
///
/// ``Interval(months, days=0, micros=0)`` or ``Interval("P1Y2M3DT4H")`` (ISO 8601 text, up to
/// nine fraction digits).
#[pyclass(module = "graphersal", frozen, eq, hash, skip_from_py_object)]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Interval {
    duration: Duration,
}

#[pymethods]
impl Interval {
    #[new]
    #[pyo3(signature = (months, days=0, micros=0))]
    fn py_new(months: &Bound<'_, PyAny>, days: i32, micros: i64) -> PyResult<Self> {
        if let Ok(text) = months.extract::<String>() {
            if days != 0 || micros != 0 {
                return Err(PyValueError::new_err(
                    "Interval(text) takes no days or micros: they are part of the text",
                ));
            }
            return match text.parse::<Duration>() {
                Ok(duration) => Ok(Interval { duration }),
                Err(err) => Err(PyValueError::new_err(format!(
                    "not an ISO 8601 duration: {err}"
                ))),
            };
        }
        let months: i32 = months.extract().map_err(|_| {
            PyValueError::new_err("Interval(months, days, micros) takes int32 months and days")
        })?;
        Ok(Interval {
            duration: Duration::new(months, days, micros),
        })
    }

    /// The months (a year is 12).
    #[getter]
    fn months(&self) -> i32 {
        self.duration.months()
    }

    /// The days (a day is not always 24 hours in a time zone).
    #[getter]
    fn days(&self) -> i32 {
        self.duration.days()
    }

    /// The time part in microseconds: a Python `int`, exact over the whole range (an `i64` of
    /// seconds, beyond an `i64` of microseconds). `ValueError` when the time part has digits
    /// below the microsecond (read `nanos`): never cut.
    #[getter]
    fn micros(&self) -> PyResult<i128> {
        let nanos = self.duration.nanos();
        if nanos % 1000 != 0 {
            return Err(PyValueError::new_err(format!(
                "the Interval {} has digits below the microsecond; read nanos",
                self.duration
            )));
        }
        Ok(nanos / 1000)
    }

    /// The time part in nanoseconds: a Python `int`, exact over the whole range.
    #[getter]
    fn nanos(&self) -> i128 {
        self.duration.nanos()
    }

    /// The ISO 8601 text (`P1Y2M3DT4H`), the engine's canonical form.
    #[allow(clippy::wrong_self_convention)] // a #[pymethods] receiver cannot be `self`
    fn to_iso(&self) -> String {
        self.duration.to_string()
    }

    fn __str__(&self) -> String {
        self.to_iso()
    }

    fn __repr__(&self) -> String {
        format!("Interval('{}')", self.duration)
    }
}

/// The `datetime` module.
fn datetime_module(py: Python<'_>) -> PyResult<Bound<'_, PyModule>> {
    py.import("datetime")
}

/// The error of a time of day with a part below the microsecond: no Python type holds one
/// (never rounded silently).
fn sub_micros_error(value: &Temporal) -> PyErr {
    PyValueError::new_err(format!(
        "the {} {value} has a part below the microsecond, which no Python time type holds \
         (datetime.time counts microseconds; pandas has no time of day); truncate it first, for \
         example with date_trunc(DT.microsecond), or read it with as_string()",
        value.kind()
    ))
}

/// The `pandas` module, for a value with digits below the microsecond; `ImportError` naming the
/// optional extra when it is not installed.
fn pandas_module<'py>(py: Python<'py>, value: &Temporal) -> PyResult<Bound<'py, PyModule>> {
    py.import("pandas").map_err(|_| {
        PyImportError::new_err(format!(
            "the {} {value} has digits below the microsecond: Python's datetime types count \
             microseconds, pandas.Timestamp / pandas.Timedelta nanoseconds; install the extra \
             with pip install graphersal[pandas], or truncate the value first with \
             date_trunc(DT.microsecond)",
            value.kind()
        ))
    })
}

/// A temporal value as a Python object (see the module docs).
pub(crate) fn temporal_to_pyobject(py: Python<'_>, value: Temporal) -> PyResult<Py<PyAny>> {
    let module = datetime_module(py)?;
    if value.has_sub_micros() {
        return sub_micros_to_pyobject(&module, value).map(Bound::unbind);
    }
    let object = match value {
        Temporal::Date(date) => date_object(&module, date)?,
        Temporal::Time(time) => time_object(&module, time, None)?,
        Temporal::LocalDateTime(local) => datetime_object(&module, local, None)?,
        Temporal::DateTime(value) => match value.zone_name() {
            Some(name) => zoned_datetime_object(&module, value, name)?,
            None => {
                let zone = fixed_zone(&module, value.offset_seconds())?;
                datetime_object(&module, value.local(), Some(zone))?
            }
        },
        Temporal::OffsetTime(value) => {
            let zone = fixed_zone(&module, value.offset_seconds())?;
            time_object(&module, value.time(), Some(zone))?
        }
        Temporal::Duration(duration) => {
            // The whole length in microseconds (exact: no part below the microsecond here).
            let micros = duration.nanos() / 1000;
            let total = i128::from(duration.days()) * i128::from(MICROS_PER_DAY) + micros;
            let fits = (-i128::from(MAX_TIMEDELTA_DAYS) * i128::from(MICROS_PER_DAY)
                ..(i128::from(MAX_TIMEDELTA_DAYS) + 1) * i128::from(MICROS_PER_DAY))
                .contains(&total);
            if duration.months() == 0 && fits {
                let kwargs = PyDict::new(py);
                kwargs.set_item("days", duration.days())?;
                kwargs.set_item("microseconds", micros)?;
                module.getattr("timedelta")?.call((), Some(&kwargs))?
            } else {
                Bound::new(py, Interval { duration })?.into_any()
            }
        }
        // A later kind is reported, never guessed (CLAUDE.md rule 14).
        _ => {
            return Err(crate::error::GraphersalError::new_err(format!(
                "unsupported date or time kind {}: py-graphersal does not convert it",
                value.kind()
            )));
        }
    };
    Ok(object.unbind())
}

/// A value with digits below the microsecond (see the module docs): a `pandas.Timestamp` built
/// from the `datetime` of its microseconds plus `nanosecond=` (so a repeated wall-clock time
/// keeps its `fold`), a `pandas.Timedelta` of its nanoseconds, an [`Interval`], or an error.
fn sub_micros_to_pyobject<'py>(
    module: &Bound<'py, PyModule>,
    value: Temporal,
) -> PyResult<Bound<'py, PyAny>> {
    let py = module.py();
    let timestamp = |base: Bound<'py, PyAny>, nanosecond: u32| -> PyResult<Bound<'py, PyAny>> {
        let pandas = pandas_module(py, &value)?;
        let kwargs = PyDict::new(py);
        kwargs.set_item("nanosecond", nanosecond % 1000)?;
        pandas
            .getattr("Timestamp")?
            .call((base,), Some(&kwargs))
            .map_err(|err| {
                crate::error::GraphersalError::new_err(format!(
                    "the {} {value} is outside the years pandas.Timestamp holds nanoseconds for \
                     (1677-09-21 to 2262-04-11): {err}; read it with as_string(), or truncate it \
                     with date_trunc(DT.microsecond)",
                    value.kind()
                ))
            })
    };
    match value {
        Temporal::LocalDateTime(local) => {
            let base = datetime_object(module, local, None)?;
            timestamp(base, local.time().nanosecond())
        }
        Temporal::DateTime(instant) => {
            let base = match instant.zone_name() {
                Some(name) => zoned_datetime_object(module, instant, name)?,
                None => {
                    let zone = fixed_zone(module, instant.offset_seconds())?;
                    datetime_object(module, instant.local(), Some(zone))?
                }
            };
            timestamp(base, instant.local().time().nanosecond())
        }
        Temporal::Duration(duration) => {
            let total = i128::from(duration.days()) * NANOS_PER_DAY + duration.nanos();
            match i64::try_from(total) {
                Ok(total) if duration.months() == 0 => {
                    let pandas = pandas_module(py, &value)?;
                    let kwargs = PyDict::new(py);
                    kwargs.set_item("unit", "ns")?;
                    pandas.getattr("Timedelta")?.call((total,), Some(&kwargs))
                }
                _ => Ok(Bound::new(py, Interval { duration })?.into_any()),
            }
        }
        _ => Err(sub_micros_error(&value)),
    }
}

/// `datetime.timezone(timedelta(seconds=offset))`.
fn fixed_zone<'py>(module: &Bound<'py, PyModule>, offset: i32) -> PyResult<Bound<'py, PyAny>> {
    let delta = module.getattr("timedelta")?.call((0, offset), None)?;
    module.getattr("timezone")?.call1((delta,))
}

/// A `datetime` with a zone name as an aware `datetime.datetime` in `zoneinfo.ZoneInfo(name)`:
/// the instant moved into the zone (`astimezone`), so a time the zone repeats gets the right
/// `fold`. A zone the Python installation does not know (no `tzdata` on some systems) raises,
/// never drops the name silently.
fn zoned_datetime_object<'py>(
    module: &Bound<'py, PyModule>,
    value: DateTime,
    name: &str,
) -> PyResult<Bound<'py, PyAny>> {
    let py = module.py();
    let zone = py
        .import("zoneinfo")
        .and_then(|zoneinfo| zoneinfo.getattr("ZoneInfo")?.call1((name,)))
        .map_err(|err| {
            crate::error::GraphersalError::new_err(format!(
                "the datetime {value} names the zone {name}, which this Python installation \
                 does not know ({err}); install the tzdata package, or read the value without \
                 its zone name (datetime(x.as_string()) of the offset form)"
            ))
        })?;
    let utc = fixed_zone(module, 0)?;
    let at_utc = datetime_object(
        module,
        value
            .with_offset(0)
            .map_err(|err| crate::error::GraphersalError::new_err(err.to_string()))?
            .local(),
        Some(utc),
    )?;
    at_utc
        .call_method1("astimezone", (zone,))
        .map_err(|_| out_of_python_range(&value.to_string()))
}

fn date_object<'py>(module: &Bound<'py, PyModule>, date: Date) -> PyResult<Bound<'py, PyAny>> {
    module
        .getattr("date")?
        .call1((date.year(), date.month(), date.day()))
        .map_err(|_| out_of_python_range(&date.to_string()))
}

fn time_object<'py>(
    module: &Bound<'py, PyModule>,
    time: Time,
    zone: Option<Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    let args = (
        time.hour(),
        time.minute(),
        time.second(),
        time.microsecond(),
        zone,
    );
    module.getattr("time")?.call1(args)
}

fn datetime_object<'py>(
    module: &Bound<'py, PyModule>,
    local: LocalDateTime,
    zone: Option<Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    let (date, time) = (local.date(), local.time());
    let args = PyTuple::new(
        module.py(),
        [
            date.year().into_pyobject(module.py())?.into_any(),
            date.month().into_pyobject(module.py())?.into_any(),
            date.day().into_pyobject(module.py())?.into_any(),
            time.hour().into_pyobject(module.py())?.into_any(),
            time.minute().into_pyobject(module.py())?.into_any(),
            time.second().into_pyobject(module.py())?.into_any(),
            time.microsecond().into_pyobject(module.py())?.into_any(),
        ],
    )?;
    let kwargs = PyDict::new(module.py());
    if let Some(zone) = zone {
        kwargs.set_item("tzinfo", zone)?;
    }
    module
        .getattr("datetime")?
        .call(args, Some(&kwargs))
        .map_err(|_| out_of_python_range(&local.to_string()))
}

/// Python's `datetime` holds the years 1 to 9999; Graphersal goes down to -9999.
fn out_of_python_range(text: &str) -> PyErr {
    crate::error::GraphersalError::new_err(format!(
        "the date {text} is outside the years 1 to 9999 that Python's datetime holds; read it \
         as text with as_string()"
    ))
}

/// A Python date or time value (`datetime`, `date`, `time`, `timedelta`, [`Interval`]) as a
/// temporal value; `Ok(None)` for any other object.
pub(crate) fn python_to_temporal(value: &Bound<'_, PyAny>) -> Result<Option<Temporal>, String> {
    if let Ok(interval) = value.cast::<Interval>() {
        return Ok(Some(Temporal::Duration(interval.get().duration)));
    }
    let py = value.py();
    let module = datetime_module(py).map_err(|err| err.to_string())?;
    let is = |class: &str| -> bool {
        module
            .getattr(class)
            .and_then(|class| value.is_instance(&class))
            .unwrap_or(false)
    };
    let int = |name: &str| -> Result<i64, String> {
        value
            .getattr(name)
            .and_then(|v| v.extract::<i64>())
            .map_err(|err| err.to_string())
    };
    // The digits below the microsecond of a `pandas.Timestamp` (`nanosecond`) or a
    // `pandas.Timedelta` (`nanoseconds`), 0 for the standard library's types.
    let sub_micros = |name: &str| -> Result<i64, String> {
        match value.getattr(name) {
            Ok(field) => field.extract::<i64>().map_err(|err| err.to_string()),
            Err(_) => Ok(0),
        }
    };
    let failed = |err: graphersal::temporal::TemporalError| err.to_string();
    // `pandas.NaT` is an instance of `datetime` (and passes as one) but has no value.
    let type_name = value
        .get_type()
        .name()
        .map(|name| name.to_string())
        .unwrap_or_default();
    if type_name == "NaTType" {
        return Err("pandas.NaT (not a time) has no value; pass None for no value".to_string());
    }
    // `datetime` is a subclass of `date`: checked first.
    if is("datetime") {
        let date = Date::from_ymd(int("year")? as i32, int("month")? as u8, int("day")? as u8)
            .map_err(failed)?;
        let nano = int("microsecond")? * 1000 + sub_micros("nanosecond")?;
        let time = Time::from_hms_nano(
            int("hour")? as u8,
            int("minute")? as u8,
            int("second")? as u8,
            u32::try_from(nano).map_err(|_| "the datetime's fraction is out of range")?,
        )
        .map_err(failed)?;
        let local = LocalDateTime::new(date, time);
        let offset = value
            .call_method0("utcoffset")
            .map_err(|err| err.to_string())?;
        if offset.is_none() {
            return Ok(Some(Temporal::LocalDateTime(local)));
        }
        let micros = timedelta_micros(&offset)?;
        if micros % MICROS_PER_SECOND != 0 {
            return Err(
                "the datetime's UTC offset has a fraction of a second; Graphersal keeps whole \
                 seconds"
                    .to_string(),
            );
        }
        let offset = i32::try_from(micros / MICROS_PER_SECOND)
            .map_err(|_| "the datetime's UTC offset is out of range".to_string())?;
        let instant = DateTime::from_local(local, offset).map_err(failed)?;
        // A `zoneinfo.ZoneInfo` (its `key` is the IANA name) or a `pytz` zone (its `zone`, what
        // pandas 2 uses for a zone name) keeps its name, as the engine's zoned datetime; a name
        // the bundled database does not know is an error, never dropped.
        let key = value.getattr("tzinfo").ok().and_then(|zone| {
            let name = |attr: &str| {
                zone.getattr(attr)
                    .ok()
                    .and_then(|key| key.extract::<String>().ok())
            };
            let pytz = zone
                .get_type()
                .module()
                .is_ok_and(|module| module.to_string().starts_with("pytz"));
            name("key").or_else(|| if pytz { name("zone") } else { None })
        });
        return match key {
            Some(name) => instant
                .in_zone(&name)
                .map(|v| Some(Temporal::DateTime(v)))
                .map_err(failed),
            None => Ok(Some(Temporal::DateTime(instant))),
        };
    }
    if is("date") {
        return Date::from_ymd(int("year")? as i32, int("month")? as u8, int("day")? as u8)
            .map(|v| Some(Temporal::Date(v)))
            .map_err(failed);
    }
    if is("time") {
        let time = Time::from_hms_micro(
            int("hour")? as u8,
            int("minute")? as u8,
            int("second")? as u8,
            int("microsecond")? as u32,
        )
        .map_err(failed)?;
        let zone = value.getattr("tzinfo").map_err(|err| err.to_string())?;
        if zone.is_none() {
            return Ok(Some(Temporal::Time(time)));
        }
        // A fixed offset (`datetime.timezone`) makes an offsettime; a zone with rules (a
        // `ZoneInfo`) has no offset without a date.
        let offset = value
            .call_method0("utcoffset")
            .map_err(|err| err.to_string())?;
        if offset.is_none() {
            return Err(
                "an aware datetime.time whose tzinfo has no fixed offset (a ZoneInfo) has no \
                 date to find its offset at: pass a datetime.timezone offset, or an aware \
                 datetime.datetime"
                    .to_string(),
            );
        }
        let micros = timedelta_micros(&offset)?;
        if micros % MICROS_PER_SECOND != 0 {
            return Err(
                "the time's UTC offset has a fraction of a second; Graphersal keeps whole seconds"
                    .to_string(),
            );
        }
        let offset = i32::try_from(micros / MICROS_PER_SECOND)
            .map_err(|_| "the time's UTC offset is out of range".to_string())?;
        return OffsetTime::new(time, offset)
            .map(|v| Some(Temporal::OffsetTime(v)))
            .map_err(failed);
    }
    if is("timedelta") {
        // Whole days and the rest, both with the sign of the whole (timedelta(hours=-1) is
        // PT-1H, not Python's own days=-1, seconds=82800).
        // Python keeps 0 <= seconds < 86400 and 0 <= microseconds < 10^6.
        // A `pandas.Timedelta` adds 0 <= nanoseconds < 1000.
        let (mut days, mut rest) = (
            int("days")?,
            (i128::from(int("seconds")?) * i128::from(MICROS_PER_SECOND)
                + i128::from(int("microseconds")?))
                * 1000
                + i128::from(sub_micros("nanoseconds")?),
        );
        if days < 0 && rest > 0 {
            days += 1;
            rest -= NANOS_PER_DAY;
        }
        let days = i32::try_from(days).map_err(|_| "the timedelta is out of range".to_string())?;
        return Duration::with_nanos(0, days, rest)
            .map(|v| Some(Temporal::Duration(v)))
            .map_err(failed);
    }
    Ok(None)
}

/// A `timedelta` in microseconds (exact: days, seconds and microseconds are integers).
fn timedelta_micros(delta: &Bound<'_, PyAny>) -> Result<i64, String> {
    let field = |name: &str| -> Result<i64, String> {
        delta
            .getattr(name)
            .and_then(|v| v.extract::<i64>())
            .map_err(|err| err.to_string())
    };
    let (days, seconds, micros) = (field("days")?, field("seconds")?, field("microseconds")?);
    days.checked_mul(MICROS_PER_DAY)
        .and_then(|total| total.checked_add(seconds * MICROS_PER_SECOND + micros))
        .ok_or_else(|| "the timedelta is out of range".to_string())
}

/// The canonical text of a Python date or time value, `None` for any other object (a saved
/// query's arguments travel as JSON text; the parameter's schema reads the text back).
pub(crate) fn python_temporal_text(value: &Bound<'_, PyAny>) -> Option<String> {
    python_to_temporal(value)
        .ok()
        .flatten()
        .map(|value| value.to_string())
}
