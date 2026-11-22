//! `g.memory_report([#{..}])` / `g.memory_report(..)`: the memory report (book page "Memory
//! Usage"), a plain call on the source answering [`MemoryReport::to_property`]'s map.
//!
//! [`MemoryReport::to_property`]: crate::graph::MemoryReport::to_property

use super::saved_query::{fail, graph_fail};
use super::*;
use crate::error::ScriptError;
use crate::graph::memory_report::MemoryReportScope;
use crate::graph::{AccessRequest, Action, MemoryReportRequest, Resource};

/// The keys an options map may have.
const REPORT_KEYS: [&str; 7] = [
    "sample_every",
    "max_elements",
    "limit",
    "max_rows",
    "max_labels",
    "path_depth",
    "scope",
];

impl ScriptGraphTraversalSource {
    /// `g.memory_report()`: a full walk, the 500 largest property rows and label rows.
    pub fn memory_report(&mut self) -> Result<Dynamic, Box<EvalAltResult>> {
        self.memory_report_with(Dynamic::from_map(rhai::Map::new()))
    }

    /// `g.memory_report(#{sample_every?, max_elements?, limit?, max_rows?, max_labels?,
    /// path_depth?, scope?})` / `g.memory_report(..)`: where the graph's memory goes, per label and per
    /// top-level property (a map, see [`crate::graph::MemoryReport::to_property`]), or `()` when
    /// the source has no graph. `sample_every`/`max_elements` sample (figures extrapolated),
    /// `limit` keeps the property rows and the label rows with the most bytes (default 500;
    /// `max_rows`/`max_labels` set one list each and win over it), `path_depth` is `1`,
    /// `scope` is `"all"` (the default: every element the graph shows) or `"delta"` (a layer's
    /// own elements only; the whole graph for a storage without layers, see
    /// [`MemoryReportScope`]). Read-only; asks for `Read` on `Data`.
    pub fn memory_report_with(&mut self, options: Dynamic) -> Result<Dynamic, Box<EvalAltResult>> {
        let operation = "memory_report";
        let actual = type_name_of(&options);
        let mismatch = |expected: String, actual: String| {
            fail(ScriptError::ArgumentMismatch {
                step_name: operation.to_string(),
                expected,
                actual,
            })
        };
        let Some(mut options) = options.try_cast::<rhai::Map>() else {
            return Err(mismatch(
                "a map #{sample_every?, max_elements?, limit?, max_rows?, max_labels?, path_depth?, \
                 scope?}"
                    .into(),
                actual,
            ));
        };
        if let Some(unknown) = options
            .keys()
            .find(|key| !REPORT_KEYS.contains(&key.as_str()))
        {
            return Err(mismatch(
                format!("only the keys {}", REPORT_KEYS.join(", ")),
                format!("the key '{unknown}'"),
            ));
        }
        let scope = match options.remove("scope") {
            None => None,
            Some(value) if value.is_unit() => None,
            Some(value) => match value
                .clone()
                .into_immutable_string()
                .ok()
                .and_then(|name| MemoryReportScope::from_name(name.as_str()))
            {
                Some(scope) => Some(scope),
                None => {
                    return Err(mismatch(
                        "scope as \"all\" or \"delta\"".into(),
                        dynamic_text(&value),
                    ));
                }
            },
        };
        let mut positive = |key: &str| -> Result<Option<u64>, Box<EvalAltResult>> {
            let Some(value) = options.remove(key) else {
                return Ok(None);
            };
            if value.is_unit() {
                return Ok(None);
            }
            match value.as_int().ok().and_then(|n| u64::try_from(n).ok()) {
                Some(n) if n > 0 => Ok(Some(n)),
                _ => Err(mismatch(
                    format!("{key} as a positive integer"),
                    dynamic_text(&value),
                )),
            }
        };
        let mut request = MemoryReportRequest::new();
        if let Some(k) = positive("sample_every")? {
            request = request.with_sample_every(k);
        }
        if let Some(n) = positive("max_elements")? {
            request = request.with_max_elements(n);
        }
        if let Some(n) = positive("limit")? {
            request = request.with_limit(usize::try_from(n).unwrap_or(usize::MAX));
        }
        if let Some(n) = positive("max_rows")? {
            request = request.with_max_rows(usize::try_from(n).unwrap_or(usize::MAX));
        }
        if let Some(n) = positive("max_labels")? {
            request = request.with_max_labels(usize::try_from(n).unwrap_or(usize::MAX));
        }
        if let Some(depth) = positive("path_depth")? {
            request = request.with_path_depth(u32::try_from(depth).unwrap_or(u32::MAX));
        }
        if let Some(scope) = scope {
            request = request.with_scope(scope);
        }
        request.validate().map_err(graph_fail)?;
        self.require(
            operation,
            AccessRequest::new(Action::Read, Resource::data(None, None)),
        )?;
        let Some(g) = self.g.as_ref() else {
            return Ok(Dynamic::UNIT);
        };
        let report = g.memory_report(&request).map_err(script_error)?;
        Ok(element_property_to_dynamic(report.to_property()))
    }
}
