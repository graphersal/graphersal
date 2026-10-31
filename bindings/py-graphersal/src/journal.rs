//! Reading the system journal from Python (book page "System Journal"): `Graph.journal`,
//! `Store.journal` and the closed-store `Store.journal_dir` share the keyword filters and the
//! page form here. A page is the dict of `JournalPage::to_property` (`{"records", "next_after",
//! "truncated", "more", "notes"}`), each record the dict of `JournalRecord::to_property`: the
//! keys of `graphersal store journal --json` and `GET /api/journal`.

use std::sync::Arc;

use graphersal::auth::{AccessRequest, Action, Authorizer, Resource};
use graphersal::system_journal::{JournalPage, JournalQuery, Level};
use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyString;

use crate::policy::AccessPolicy;

/// The keyword filters every journal method takes, as Python passed them.
pub(crate) struct JournalArgs<'py> {
    pub after: Option<u64>,
    pub since: Option<Bound<'py, PyAny>>,
    pub until: Option<Bound<'py, PyAny>>,
    pub level: Option<String>,
    pub source: Option<String>,
    pub kind: Option<String>,
    pub op: Option<u64>,
    pub origin: Option<String>,
    pub limit: Option<usize>,
}

impl JournalArgs<'_> {
    /// The engine query of these filters; a bad value raises `ValueError` / `TypeError`.
    pub(crate) fn to_query(&self) -> PyResult<JournalQuery> {
        let mut query = JournalQuery::new();
        if let Some(after) = self.after {
            query = query.after(after);
        }
        if let Some(name) = &self.level {
            let level = Level::from_name(name).ok_or_else(|| {
                PyValueError::new_err(format!(
                    "unknown level '{name}': use error, warn, notice or info"
                ))
            })?;
            query = query.min_level(level);
        }
        if let Some(since) = &self.since {
            query = query.since(time_micros("since", since)?);
        }
        if let Some(until) = &self.until {
            query = query.until(time_micros("until", until)?);
        }
        if let Some(source) = &self.source {
            query = query.source_prefix(source.clone());
        }
        if let Some(kind) = &self.kind {
            query = query.kind(kind.clone());
        }
        if let Some(op) = self.op {
            query = query.op(op);
        }
        if let Some(origin) = &self.origin {
            let id = uuid::Uuid::parse_str(origin.trim()).map_err(|_| {
                PyValueError::new_err(format!(
                    "'origin' is an instance id (a UUID), not {origin:?}"
                ))
            })?;
            query = query.origin(id.as_u128());
        }
        if let Some(limit) = self.limit {
            if limit == 0 {
                return Err(PyValueError::new_err(
                    "'limit' is a positive number of records",
                ));
            }
            query = query.limit(limit);
        }
        Ok(query)
    }
}

/// Microseconds since the Unix epoch of a `since`/`until` argument: an `int` (microseconds),
/// a timezone-aware `datetime.datetime`, or an ISO 8601 / RFC 3339 `str` with a zone
/// (`2026-10-31T09:00:00Z`, `...+02:00`).
fn time_micros(name: &str, value: &Bound<'_, PyAny>) -> PyResult<i64> {
    let py = value.py();
    if value.is_instance_of::<pyo3::types::PyBool>() {
        return Err(PyTypeError::new_err(format!(
            "'{name}' is microseconds since the Unix epoch (int), a timezone-aware datetime or \
             an ISO 8601 str"
        )));
    }
    if let Ok(micros) = value.extract::<i64>() {
        return Ok(micros);
    }
    let datetime = py.import("datetime")?;
    let moment = if value.is_instance_of::<PyString>() {
        let text: String = value.extract()?;
        // `fromisoformat` of Python 3.10 does not read a trailing `Z`.
        let text = match text.trim().strip_suffix(['Z', 'z']) {
            Some(head) => format!("{head}+00:00"),
            None => text.trim().to_string(),
        };
        datetime
            .getattr("datetime")?
            .call_method1("fromisoformat", (text.as_str(),))
            .map_err(|_| {
                PyValueError::new_err(format!(
                    "'{name}' is not a date-time: {text:?} (expected ISO 8601 with a zone, like \
                     2026-10-31T09:00:00Z or 2026-10-31T11:00:00+02:00)"
                ))
            })?
    } else if value.is_instance(&datetime.getattr("datetime")?)? {
        value.clone()
    } else {
        return Err(PyTypeError::new_err(format!(
            "'{name}' is microseconds since the Unix epoch (int), a timezone-aware datetime or \
             an ISO 8601 str, not {}",
            value.get_type().name()?
        )));
    };
    if moment.getattr("tzinfo")?.is_none() || moment.call_method0("utcoffset")?.is_none() {
        return Err(PyValueError::new_err(format!(
            "'{name}' needs a time zone (a naive datetime is ambiguous): use \
             datetime(..., tzinfo=timezone.utc) or a text with Z or an offset"
        )));
    }
    let timezone = datetime.getattr("timezone")?.getattr("utc")?;
    let epoch = datetime
        .getattr("datetime")?
        .call1((1970, 1, 1, 0, 0, 0, 0, timezone))?;
    let delta = moment.call_method1("__sub__", (epoch,))?;
    let days: i64 = delta.getattr("days")?.extract()?;
    let seconds: i64 = delta.getattr("seconds")?.extract()?;
    let micros: i64 = delta.getattr("microseconds")?.extract()?;
    Ok(days * 86_400_000_000 + seconds * 1_000_000 + micros)
}

/// Asks `policy` (everything allowed without one, like a script) for `Read` on the journal;
/// a denial is the rendered diagnostic `PermissionDeniedError` is made from.
pub(crate) fn authorize(policy: Option<&AccessPolicy>) -> Result<(), String> {
    let Some(policy) = policy else {
        return Ok(());
    };
    let authorizer: Arc<dyn Authorizer> = Arc::new(policy.to_engine());
    let request = AccessRequest::new(Action::Read, Resource::Journal);
    authorizer.authorize(&request).map_err(|denied| {
        format!(
            "Error: Permission denied for 'journal' ({request}): {}\nHelp: reading the system \
             journal needs ('read', 'journal'); AccessPolicy.read_only() allows it, or add it \
             with policy.allow(\"read\", \"journal\").",
            denied.reason()
        )
    })
}

/// A page as the Python dict.
pub(crate) fn page_to_py(py: Python<'_>, page: &JournalPage) -> PyResult<Py<PyAny>> {
    crate::convert::property_to_pyobject(py, &page.to_property())
}
