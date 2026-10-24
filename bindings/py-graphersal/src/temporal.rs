//! The five date and time types (book "Dates and Times") as Python values, both ways:
//!
//! | Graphersal | Python |
//! |---|---|
//! | `datetime` | aware `datetime.datetime`, `tzinfo` = `datetime.timezone(timedelta(seconds=offset))` |
//! | `localdatetime` | naive `datetime.datetime` |
//! | `date` | `datetime.date` |
//! | `time` | naive `datetime.time` (an aware `time` has no instant and is refused) |
//! | `duration` | `datetime.timedelta` when it has no months, else [`Interval`] |
//!
//! Python's `datetime` counts in microseconds, the engine in nanoseconds: a value with a part
//! below the microsecond raises `ValueError` rather than being rounded (truncate it first with
//! `date_trunc(DT.microsecond)`); every other value converts exactly. The Python objects are
//! built and read through the `datetime` module's own API (the datetime C API is not part of the
//! stable ABI this extension is built for).

use graphersal::temporal::{Date, DateTime, Duration, LocalDateTime, Temporal, Time};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyTuple};

const MICROS_PER_SECOND: i64 = 1_000_000;
const MICROS_PER_DAY: i64 = 86_400 * MICROS_PER_SECOND;
/// `datetime.timedelta`'s day limit.
const MAX_TIMEDELTA_DAYS: i64 = 999_999_999;

/// A `duration` with months (a PostgreSQL `interval` such as `P1M`), which `datetime.timedelta`
/// cannot hold: a month has no fixed length. Immutable; equal when all three fields are equal
/// (`P1M` is not `P30D`, as in the engine).
///
/// ``Interval(months, days=0, micros=0)`` or ``Interval("P1Y2M3DT4H")`` (ISO 8601 text).
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
                Ok(duration) if Temporal::Duration(duration).has_sub_micros() => {
                    Err(sub_micros_error(&Temporal::Duration(duration)))
                }
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

    /// The time part in microseconds.
    #[getter]
    fn micros(&self) -> i64 {
        self.duration.micros()
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

/// The error of a value with a part below the microsecond, which Python's `datetime` types
/// cannot hold (never rounded silently).
fn sub_micros_error(value: &Temporal) -> PyErr {
    PyValueError::new_err(format!(
        "the {} {value} has a part below the microsecond, which Python's datetime types cannot \
         hold; truncate it first, for example with date_trunc(DT.microsecond)",
        value.kind()
    ))
}

/// A temporal value as a Python object (see the module docs).
pub(crate) fn temporal_to_pyobject(py: Python<'_>, value: Temporal) -> PyResult<Py<PyAny>> {
    if value.has_sub_micros() {
        return Err(sub_micros_error(&value));
    }
    let module = datetime_module(py)?;
    let object = match value {
        Temporal::Date(date) => date_object(&module, date)?,
        Temporal::Time(time) => time_object(&module, time, None)?,
        Temporal::LocalDateTime(local) => datetime_object(&module, local, None)?,
        Temporal::DateTime(value) => {
            let delta = module
                .getattr("timedelta")?
                .call((0, value.offset_seconds()), None)?;
            let zone = module.getattr("timezone")?.call1((delta,))?;
            datetime_object(&module, value.local(), Some(zone))?
        }
        Temporal::Duration(duration) => {
            if duration.months() == 0 && i64::from(duration.days()).abs() <= MAX_TIMEDELTA_DAYS {
                let kwargs = PyDict::new(py);
                kwargs.set_item("days", duration.days())?;
                kwargs.set_item("microseconds", duration.micros())?;
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
    let failed = |err: graphersal::temporal::TemporalError| err.to_string();
    // `datetime` is a subclass of `date`: checked first.
    if is("datetime") {
        let date = Date::from_ymd(int("year")? as i32, int("month")? as u8, int("day")? as u8)
            .map_err(failed)?;
        let time = Time::from_hms_micro(
            int("hour")? as u8,
            int("minute")? as u8,
            int("second")? as u8,
            int("microsecond")? as u32,
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
        return DateTime::from_local(local, offset)
            .map(|v| Some(Temporal::DateTime(v)))
            .map_err(failed);
    }
    if is("date") {
        return Date::from_ymd(int("year")? as i32, int("month")? as u8, int("day")? as u8)
            .map(|v| Some(Temporal::Date(v)))
            .map_err(failed);
    }
    if is("time") {
        let zone = value.getattr("tzinfo").map_err(|err| err.to_string())?;
        if !zone.is_none() {
            return Err(
                "an aware datetime.time (with tzinfo) has no date, so no instant: pass a naive \
                 time (time.replace(tzinfo=None)) or an aware datetime.datetime"
                    .to_string(),
            );
        }
        return Time::from_hms_micro(
            int("hour")? as u8,
            int("minute")? as u8,
            int("second")? as u8,
            int("microsecond")? as u32,
        )
        .map(|v| Some(Temporal::Time(v)))
        .map_err(failed);
    }
    if is("timedelta") {
        // Whole days and the rest, both with the sign of the whole (timedelta(hours=-1) is
        // PT-1H, not Python's own days=-1, seconds=82800).
        // Python keeps 0 <= seconds < 86400 and 0 <= microseconds < 10^6.
        let (mut days, mut rest) = (
            int("days")?,
            int("seconds")? * MICROS_PER_SECOND + int("microseconds")?,
        );
        if days < 0 && rest > 0 {
            days += 1;
            rest -= MICROS_PER_DAY;
        }
        let days = i32::try_from(days).map_err(|_| "the timedelta is out of range".to_string())?;
        return Ok(Some(Temporal::Duration(Duration::new(0, days, rest))));
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
