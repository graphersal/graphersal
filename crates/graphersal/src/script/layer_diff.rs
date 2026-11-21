//! `g.layer_diff([#{..}])`: what a layered graph changed against its frozen base, a plain call
//! on the source answering [`LayerDiff::to_property`]'s map (an empty diff on a graph without
//! layers).
//!
//! [`LayerDiff::to_property`]: crate::graph::LayerDiff::to_property

use super::saved_query::{fail, graph_fail};
use super::*;
use crate::error::ScriptError;
use crate::graph::{AccessRequest, Action, ElementKind, LayerDiffRequest, Resource};

/// The keys an options map may have.
const DIFF_KEYS: [&str; 3] = ["kinds", "labels", "limit"];

/// The expected shape of the argument, as the diagnostics name it.
const DIFF_SHAPE: &str = "a map #{kinds?, labels?, limit?}";

impl ScriptGraphTraversalSource {
    /// `g.layer_diff()`: the whole diff, both element kinds, every label, no limit.
    pub fn layer_diff(&mut self) -> Result<Dynamic, Box<EvalAltResult>> {
        self.layer_diff_with(Dynamic::from_map(rhai::Map::new()))
    }

    /// `g.layer_diff(#{kinds?, labels?, limit?})`: what the layer changed against its base
    /// (a map, see [`crate::graph::LayerDiff::to_property`]), or `()` when the source has no
    /// graph. `kinds` is `"vertex"`, `"edge"` or a list of them (default both), `labels` a
    /// label or a list of labels (any label of an element's base or layer label set matches),
    /// `limit` a positive number of entries across the whole diff (`truncated` says whether it
    /// cut entries). Read-only; asks for `Read` on `Data`.
    pub fn layer_diff_with(&mut self, options: Dynamic) -> Result<Dynamic, Box<EvalAltResult>> {
        let request = layer_diff_request(options)?;
        self.require(
            "layer_diff",
            AccessRequest::new(Action::Read, Resource::data(None, None)),
        )?;
        let Some(g) = self.g.as_ref() else {
            return Ok(Dynamic::UNIT);
        };
        let diff = g.layer_diff(&request).map_err(graph_fail)?;
        Ok(element_property_to_dynamic(diff.to_property()))
    }
}

/// The [`ScriptError::ArgumentMismatch`] of `layer_diff` (its `help()` lists the keys).
fn mismatch(expected: impl Into<String>, actual: impl Into<String>) -> Box<EvalAltResult> {
    fail(ScriptError::ArgumentMismatch {
        step_name: "layer_diff".to_string(),
        expected: expected.into(),
        actual: actual.into(),
    })
}

/// The values of a key that takes one string or a list of strings; `None` for a missing key
/// or `()`.
fn strings(key: &str, value: Option<Dynamic>) -> Result<Option<Vec<String>>, Box<EvalAltResult>> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_unit() {
        return Ok(None);
    }
    let expected = || format!("{key} as a string or a list of strings");
    if value.is_string() {
        return Ok(Some(vec![value.into_string().map_err(|_| {
            mismatch(expected(), "a string that cannot be read")
        })?]));
    }
    let text = dynamic_text(&value);
    let Some(items) = value.try_cast::<rhai::Array>() else {
        return Err(mismatch(expected(), text));
    };
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let item_text = dynamic_text(&item);
        match item.into_string() {
            Ok(s) => out.push(s),
            Err(_) => return Err(mismatch(expected(), format!("the item {item_text}"))),
        }
    }
    Ok(Some(out))
}

/// The request an options map asks for.
fn layer_diff_request(options: Dynamic) -> Result<LayerDiffRequest, Box<EvalAltResult>> {
    let actual = type_name_of(&options);
    let Some(mut options) = options.try_cast::<rhai::Map>() else {
        return Err(mismatch(DIFF_SHAPE, actual));
    };
    if let Some(unknown) = options
        .keys()
        .find(|key| !DIFF_KEYS.contains(&key.as_str()))
    {
        return Err(mismatch(
            format!("only the keys {}", DIFF_KEYS.join(", ")),
            format!("the key '{unknown}'"),
        ));
    }
    let mut request = LayerDiffRequest::new();
    if let Some(kinds) = strings("kinds", options.remove("kinds"))? {
        let (mut vertex, mut edge) = (false, false);
        for kind in &kinds {
            match kind.as_str() {
                "vertex" => vertex = true,
                "edge" => edge = true,
                other => {
                    return Err(mismatch(
                        "kinds as \"vertex\", \"edge\" or a list of them",
                        format!("\"{other}\""),
                    ));
                }
            }
        }
        match (vertex, edge) {
            (true, false) => request = request.kind(ElementKind::Vertex),
            (false, true) => request = request.kind(ElementKind::Edge),
            (true, true) => {}
            (false, false) => {
                return Err(mismatch(
                    "kinds as \"vertex\", \"edge\" or a list of them",
                    "an empty list",
                ));
            }
        }
    }
    if let Some(labels) = strings("labels", options.remove("labels"))? {
        request = request.labels(labels);
    }
    if let Some(value) = options.remove("limit")
        && !value.is_unit()
    {
        match value.as_int().ok().and_then(|n| usize::try_from(n).ok()) {
            Some(n) if n > 0 => request = request.limit(n),
            _ => {
                return Err(mismatch(
                    "limit as a positive integer",
                    dynamic_text(&value),
                ));
            }
        }
    }
    Ok(request)
}
