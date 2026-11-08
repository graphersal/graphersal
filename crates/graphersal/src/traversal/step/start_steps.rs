mod step_e;
mod step_v;

use crate::value::literal::{rhai_string_array, rhai_strings};

/// The canonical rendering of a `v()`/`e()` source step, with what the optimizer folded into it:
/// `v("1", "2")`, `v(labels: ["person"])`, `v(ids: ["1"], labels: ["person"])`, then each folded
/// `has(key, value)` as `.has("title", "CEO")`, and a fused `.count()` /
/// `.group_count().by(T.label)`. An empty id list (an empty intersection of
/// `has_id()` filters) renders as `v([])`, so it does not look like a full scan.
pub(crate) fn render_source(
    name: &str,
    ids: &Option<Vec<String>>,
    labels: &Option<Vec<String>>,
    has: &[crate::traversal::step::PropertyFilter],
    count_only: bool,
    group_count_by_label: bool,
    index: Option<&crate::traversal::step::IndexAccess>,
) -> String {
    let args = match (ids, labels) {
        (Some(ids), Some(labels)) => format!(
            "ids: {}, labels: {}",
            rhai_string_array(ids),
            rhai_string_array(labels)
        ),
        (Some(ids), None) if ids.is_empty() => "[]".to_string(),
        (Some(ids), None) => rhai_strings(ids),
        (None, Some(labels)) => format!("labels: {}", rhai_string_array(labels)),
        (None, None) => String::new(),
    };
    let suffix = if count_only {
        ".count()"
    } else if group_count_by_label {
        ".group_count().by(T.label)"
    } else {
        ""
    };
    let mut filters = String::new();
    for filter in has {
        filters.push_str(&format!(
            ".has({}, {})",
            crate::value::literal::rhai_string(&filter.key),
            crate::value::literal::rhai_literal(&filter.value)
        ));
    }
    let index = match index {
        Some(index) if index.label_less || labels.is_some() => index.render(
            if name == "e" { "edge" } else { "vertex" },
            labels.as_deref(),
            count_only,
        ),
        _ => String::new(),
    };
    format!("{name}({args}){filters}{suffix}{index}")
}
