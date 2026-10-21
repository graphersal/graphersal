#[cfg(feature = "display")]
use std::collections::BTreeSet;

use crate::error::TraverserResult;
use crate::graph::schema::visualize::VisualizeFormat;
use crate::traversal::render::{CUT_MARKER, ItemLimit, RenderOptions, cut_nested};
use crate::value::ElementProperty;
#[cfg(feature = "display")]
use crate::value::PropertyMap;

/// The components a table cell shows of a vector.
pub(crate) const VECTOR_PREVIEW_COMPONENTS: usize = 4;

/// Formats a table cell, cutting every nested collection to the item limit and marking the cut
/// with `…` (for example `[1, 2, 3, …]`).
pub(crate) fn format_cell(prop: &ElementProperty, limit: &ItemLimit) -> String {
    match prop {
        ElementProperty::Null => "-".to_string(),
        ElementProperty::String(s) => s.to_string(),
        ElementProperty::Compressed(text) => text.text().into_string(),
        ElementProperty::Deduped(text) => text.as_str().to_string(),
        ElementProperty::Number(n) => format!("{}", n),
        ElementProperty::Boolean(b) => {
            if *b {
                "true".to_string()
            } else {
                "false".to_string()
            }
        }
        ElementProperty::Uuid(u) => crate::value::uuid::format_canonical(*u).to_string(),
        ElementProperty::Temporal(t) => t.to_string(),
        // `vector[768 f32] [0.1, -0.2, 0.3, …]`: the kind, then the first components.
        ElementProperty::Vector(v) => v.preview(VECTOR_PREVIEW_COMPONENTS),
        ElementProperty::Array(arr) => {
            let keep = limit.keep(arr.len());
            let mut items: Vec<String> = arr
                .iter()
                .take(keep)
                .map(|item| format_cell(item, limit))
                .collect();
            if keep < arr.len() {
                items.push(CUT_MARKER.to_string());
            }
            format!("[{}]", items.join(", "))
        }
        ElementProperty::Object(obj) => {
            let keep = limit.keep(obj.len());
            let mut items: Vec<String> = obj
                .iter()
                .take(keep)
                .map(|(k, v)| format!("{}: {}", k, format_cell(v, limit)))
                .collect();
            if keep < obj.len() {
                items.push(CUT_MARKER.to_string());
            }
            format!("{{{}}}", items.join(", "))
        }
        ElementProperty::Map(map) => {
            let keep = limit.keep(map.len());
            let mut items: Vec<String> = map
                .iter()
                .take(keep)
                .map(|(k, v)| {
                    format!(
                        "{}: {}",
                        crate::value::physical::to_display_text(k),
                        format_cell(v, limit)
                    )
                })
                .collect();
            if keep < map.len() {
                items.push(CUT_MARKER.to_string());
            }
            format!("{{{}}}", items.join(", "))
        }
        ElementProperty::Vertex(v) => format!("v[{}]", v.id()),
        ElementProperty::Edge(e) => format!(
            "e[{}][{}-{}->{}]",
            e.id(),
            e.out_v().id(),
            e.label().unwrap_or("-"),
            e.in_v().id()
        ),
    }
}

/// Resolves the final headers to display based on discovered columns and user options.
///
/// Without an explicit inclusion list, more than `max_items` columns (a `group()` row with 100k
/// keys) are cut to `max_items`, followed by a `…` column.
#[cfg(feature = "display")]
fn resolve_columns(
    discovered: Vec<String>,
    options: &RenderOptions,
    limit: &ItemLimit,
) -> Vec<String> {
    let mut explicit = false;
    let mut cols = match &options.columns {
        None => discovered,
        Some(col_specs) => {
            if col_specs.is_empty() {
                discovered
            } else {
                let mut exclusions = BTreeSet::new();
                let mut inclusions = Vec::new();

                for spec in col_specs {
                    let trimmed = spec.trim();
                    if let Some(stripped) = trimmed
                        .strip_prefix('!')
                        .or_else(|| trimmed.strip_prefix('-'))
                    {
                        exclusions.insert(stripped.trim().to_string());
                    } else {
                        inclusions.push(trimmed.to_string());
                    }
                }

                if inclusions.is_empty() {
                    // Only exclusions were specified -> filter discovered columns
                    discovered
                        .into_iter()
                        .filter(|col| !exclusions.contains(col))
                        .collect()
                } else {
                    // Explicit inclusion list -> filter out any explicit exclusions
                    explicit = true;
                    inclusions
                        .into_iter()
                        .filter(|col| !exclusions.contains(col))
                        .collect()
                }
            }
        }
    };

    if !explicit {
        let keep = limit.keep(cols.len());
        if keep < cols.len() {
            cols.truncate(keep);
            cols.push(CUT_MARKER.to_string());
        }
    }

    if options.row_numbers && !cols.iter().any(|c| c == "#") {
        cols.insert(0, "#".to_string());
    }

    cols
}

/// The cells of a map row: each entry's column header (the key's display text) and value.
///
/// Two keys of one map can print the same text (`"29"` and `29`). They stay separate cells: the
/// string key's header is then shown quoted (`"29"`), so no entry is lost.
#[cfg(feature = "display")]
fn map_cells(map: &crate::value::ValueMap) -> Vec<(String, &ElementProperty)> {
    let shown: Vec<String> = map
        .keys()
        .map(|key| crate::value::physical::to_display_text(key).to_string())
        .collect();
    map.iter()
        .zip(shown.iter())
        .map(|((key, value), text)| {
            let collides = shown.iter().filter(|other| *other == text).count() > 1;
            match (key, collides) {
                (ElementProperty::String(_), true) => (format!("\"{text}\""), value),
                _ => (text.clone(), value),
            }
        })
        .collect()
}

/// The columns of a vertex table that are the vertex's own, before its properties.
#[cfg(feature = "display")]
const VERTEX_COLUMNS: &[&str] = &["id", "label"];
/// The columns of an edge table that are the edge's own (GraphSON's names), before its properties.
#[cfg(feature = "display")]
const EDGE_COLUMNS: &[&str] = &["id", "label", "outV", "inV"];
/// The header suffix of a property whose name is one of the element's own columns.
#[cfg(feature = "display")]
const PROPERTY_SUFFIX: &str = " (property)";

/// The column header of the property `key`: the key itself, or `key (property)` when the
/// element's own columns already use that name (a property named `id` or `label`).
#[cfg(feature = "display")]
fn property_column(fixed: &[&str], key: &str) -> String {
    if fixed.contains(&key) {
        format!("{key}{PROPERTY_SUFFIX}")
    } else {
        key.to_string()
    }
}

/// The cell of a property column `col` (see [`property_column`]).
#[cfg(feature = "display")]
fn property_cell(fixed: &[&str], properties: &PropertyMap, col: &str, limit: &ItemLimit) -> String {
    let key = col
        .strip_suffix(PROPERTY_SUFFIX)
        .filter(|key| fixed.contains(key))
        .unwrap_or(col);
    properties
        .get(key)
        .map_or_else(|| "-".to_string(), |val| format_cell(val, limit))
}

/// Discovers the ordered column schema for a slice of `ElementProperty` items.
#[cfg(feature = "display")]
fn discover_columns(elements: &[ElementProperty]) -> Vec<String> {
    if elements.is_empty() {
        return Vec::new();
    }

    // Check kind of elements
    let is_vertex = elements
        .iter()
        .any(|e| matches!(e, ElementProperty::Vertex(_)));
    let is_edge = elements
        .iter()
        .any(|e| matches!(e, ElementProperty::Edge(_)));
    let is_object = elements
        .iter()
        .any(|e| matches!(e, ElementProperty::Object(_)));
    let is_map = elements
        .iter()
        .any(|e| matches!(e, ElementProperty::Map(_)));

    if is_vertex || is_edge {
        let fixed: &[&str] = if is_vertex {
            VERTEX_COLUMNS
        } else {
            EDGE_COLUMNS
        };
        let mut prop_keys = BTreeSet::new();
        for el in elements {
            let properties = match el {
                ElementProperty::Vertex(v) if is_vertex => v.properties(),
                ElementProperty::Edge(e) if !is_vertex => e.properties(),
                _ => continue,
            };
            for key in properties.keys() {
                prop_keys.insert(property_column(fixed, key));
            }
        }
        let mut headers: Vec<String> = fixed.iter().map(|c| c.to_string()).collect();
        headers.extend(prop_keys);
        headers
    } else if is_object {
        let mut prop_keys = BTreeSet::new();
        for el in elements {
            if let ElementProperty::Object(obj) = el {
                for key in obj.keys() {
                    prop_keys.insert(key.to_string());
                }
            }
        }
        prop_keys.into_iter().collect()
    } else if is_map {
        // A map with a non-string key: one column per key, headed by the key's display text, in the
        // order the keys first appear. Rows are never merged by position.
        let mut columns: Vec<String> = Vec::new();
        for el in elements {
            if let ElementProperty::Map(map) = el {
                for (header, _) in map_cells(map) {
                    if !columns.contains(&header) {
                        columns.push(header);
                    }
                }
            }
        }
        columns
    } else {
        // Scalar value
        vec!["value".to_string()]
    }
}

/// Extracts a cell string value for a given row element and column header.
#[cfg(feature = "display")]
fn extract_cell_value(
    row_idx: usize,
    col: &str,
    element: &ElementProperty,
    limit: &ItemLimit,
) -> String {
    if col == "#" {
        return (row_idx + 1).to_string();
    }
    if col == CUT_MARKER {
        return CUT_MARKER.to_string();
    }

    match element {
        ElementProperty::Vertex(v) => match col {
            "id" => v.id().to_string(),
            "label" => v.label().unwrap_or("-").to_string(),
            _ => property_cell(VERTEX_COLUMNS, v.properties(), col, limit),
        },
        ElementProperty::Edge(e) => match col {
            "id" => e.id().to_string(),
            "label" => e.label().unwrap_or("-").to_string(),
            "outV" => e.out_v().id().to_string(),
            "inV" => e.in_v().id().to_string(),
            _ => property_cell(EDGE_COLUMNS, e.properties(), col, limit),
        },
        ElementProperty::Object(obj) => {
            if let Some(val) = obj.get(col) {
                format_cell(val, limit)
            } else {
                "-".to_string()
            }
        }
        ElementProperty::Map(map) => map_cells(map)
            .into_iter()
            .find(|(header, _)| header == col)
            .map_or_else(|| "-".to_string(), |(_, val)| format_cell(val, limit)),
        scalar => {
            if col == "value" {
                format_cell(scalar, limit)
            } else {
                "-".to_string()
            }
        }
    }
}

/// Formats traversal elements as a table or markdown according to options.
///
/// The caller has already cut the rows to `max_rows`; nested collections are cut to `limit`
/// here, with a `…` marker in the cell.
pub(crate) fn format_elements_as_table(
    elements: &[ElementProperty],
    format: VisualizeFormat,
    options: &RenderOptions,
    limit: &ItemLimit,
) -> TraverserResult<String> {
    if elements.is_empty() {
        return Ok(String::new());
    }

    #[cfg(feature = "display")]
    {
        use tabled::builder::Builder;
        use tabled::settings::Modify;
        use tabled::settings::Style;
        use tabled::settings::Width;
        use tabled::settings::object::Segment;

        let discovered = discover_columns(elements);
        let columns = resolve_columns(discovered, options, limit);

        if columns.is_empty() {
            return Ok(String::new());
        }

        let mut builder = Builder::default();
        builder.push_record(columns.iter().map(|c| c.as_str()));

        for (idx, el) in elements.iter().enumerate() {
            let row: Vec<String> = columns
                .iter()
                .map(|col| extract_cell_value(idx, col, el, limit))
                .collect();
            builder.push_record(row);
        }

        let mut table = builder.build();

        match format {
            VisualizeFormat::Markdown => {
                table.with(Style::markdown());
            }
            _ => {
                table.with(Style::rounded());
            }
        }

        if let Some(max_width) = options.max_width {
            table.with(Modify::new(Segment::all()).with(Width::wrap(max_width)));
        }

        if let Some(truncate) = options.truncate {
            table.with(Modify::new(Segment::all()).with(Width::truncate(truncate).suffix("...")));
        }

        Ok(table.to_string())
    }

    #[cfg(not(feature = "display"))]
    {
        let _ = (format, options);
        Ok(format_elements_as_lines(elements, limit))
    }
}

/// Renders one element per line as compact JSON text, for output without a table renderer.
///
/// Human-facing, not an exchange format: a value holding a non-finite float, which JSON cannot
/// represent, is printed as that float (`NaN`, `inf`, `-inf`) instead of failing the output.
/// Nested collections are cut to `limit` without a marker, as in JSON output.
pub(crate) fn format_elements_as_lines(elements: &[ElementProperty], limit: &ItemLimit) -> String {
    let mut buf = String::new();
    for (i, el) in elements.iter().enumerate() {
        if i > 0 {
            buf.push('\n');
        }
        let cut;
        let el = if limit.is_limited() {
            cut = cut_nested(el, limit);
            &cut
        } else {
            el
        };
        match crate::value::physical::to_json_text(el) {
            // A non-finite float is GraphSON's typed `g:Double`, like in every value JSON.
            Ok(text) => buf.push_str(&text),
            Err(err) => buf.push_str(&err.to_string()),
        }
    }
    buf
}
