//! Answer shaping: compact JSON, paging ([`Page`], [`page_of`]) and fitting an answer into the
//! caller's character limit without breaking its JSON.

use serde_json::{Value, json};

#[cfg(test)]
use crate::protocol::MAX_TEXT_CHARS;
use crate::tools::ToolCaller;

/// Characters kept free for the notes when the results of an answer are cut to the limit.
pub(crate) const NOTE_RESERVE: usize = 400;

/// The paging arguments of `query`, `gql`, `run_saved_query` and a saved query's own tool.
pub(crate) const PAGING_ARGS: [&str; 3] = ["offset", "max_rows", "max_items"];

/// Default display limits of the `query` tool (the UI's defaults).
pub(crate) const DEFAULT_MAX_ROWS: usize = 100;
pub(crate) const DEFAULT_MAX_ITEMS: usize = 100;

/// [`fit`] to [`MAX_TEXT_CHARS`].
#[cfg(test)]
pub(crate) fn cut(text: String) -> String {
    fit(text, MAX_TEXT_CHARS)
}

/// Fits a tool answer into `max_chars` characters without breaking its JSON. The query tools
/// fit their results themselves ([`query_text`], with the page's counts and the next offset);
/// this is the rule for every other answer and the safety net: the longest line that is a JSON
/// list or object (answers are compact JSON, one line) keeps the leading whole items (list
/// items, object entries) that fit, followed by a note `shown K of N items`; an answer without
/// such a line keeps its leading whole lines (a longer first line is cut by characters), with
/// a note `cut at N characters`.
pub(crate) fn fit(text: String, max_chars: usize) -> String {
    let total = text.chars().count();
    if total <= max_chars {
        return text;
    }
    let lines: Vec<&str> = text.split('\n').collect();
    let json_line = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.starts_with(['[', '{']))
        .max_by_key(|(_, line)| line.len())
        .filter(|(_, line)| serde_json::from_str::<Value>(line).is_ok())
        .map(|(at, _)| at);
    if let Some(at) = json_line
        && let Some((open, items)) = json_items(lines[at])
    {
        let others = total - lines[at].chars().count();
        if let Some(budget) = max_chars.checked_sub(others + NOTE_RESERVE) {
            let (kept_text, kept) = json_prefix(open, &items, budget);
            let what = if open == '[' { "items" } else { "entries" };
            let mut out: Vec<String> = lines.iter().map(|line| line.to_string()).collect();
            out[at] = kept_text;
            out.insert(
                at + 1,
                format!(
                    "note: shown {kept} of {} {what} of the JSON above (the answer is limited to \
                     {max_chars} characters); narrow the request to see the rest",
                    items.len()
                ),
            );
            let fitted = out.join("\n");
            if fitted.chars().count() <= max_chars {
                return fitted;
            }
        }
    }
    let note = format!(
        "… (cut at {max_chars} characters; narrow the query, e.g. with limit(), values(..), a \
         smaller max_rows or max_items, or page with offset)"
    );
    let budget = max_chars.saturating_sub(note.chars().count() + 1);
    let mut kept = String::new();
    let mut used = 0;
    for line in &lines {
        let len = line.chars().count() + usize::from(used > 0);
        if used + len > budget {
            if used == 0 {
                kept = line.chars().take(budget).collect();
            }
            break;
        }
        if used > 0 {
            kept.push('\n');
        }
        kept.push_str(line);
        used += len;
    }
    format!("{kept}\n{note}")
}

/// `text` as compact JSON (whitespace between tokens removed, strings and the key order kept
/// as written), or `None` when it is no JSON.
pub(crate) fn compact_json(text: &str) -> Option<String> {
    let text = text.trim();
    serde_json::from_str::<Value>(text).ok()?;
    let mut out = String::with_capacity(text.len());
    let (mut in_string, mut escaped) = (false, false);
    for c in text.chars() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
        } else if c == '"' {
            in_string = true;
            out.push(c);
        } else if !matches!(c, ' ' | '\n' | '\r' | '\t') {
            out.push(c);
        }
    }
    Some(out)
}

/// The top-level items of a compact JSON list (`[a,b]`: `a`, `b`) or object (`{"k":v}`:
/// `"k":v`) with its opening bracket; `None` for a scalar.
pub(crate) fn json_items(text: &str) -> Option<(char, Vec<&str>)> {
    let open = text.chars().next()?;
    let close = match open {
        '[' => ']',
        '{' => '}',
        _ => return None,
    };
    let inner = text.strip_prefix(open)?.strip_suffix(close)?;
    let mut items = Vec::new();
    if inner.is_empty() {
        return Some((open, items));
    }
    let (mut depth, mut in_string, mut escaped, mut start) = (0usize, false, false, 0);
    for (at, c) in inner.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '[' | '{' => depth += 1,
            ']' | '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                items.push(&inner[start..at]);
                start = at + 1;
            }
            _ => {}
        }
    }
    items.push(&inner[start..]);
    Some((open, items))
}

/// The JSON of the leading `items` (of a list `[` or an object `{`) that fit into `budget`
/// characters, and how many they are.
pub(crate) fn json_prefix(open: char, items: &[&str], budget: usize) -> (String, usize) {
    let close = if open == '[' { ']' } else { '}' };
    let mut out = String::from(open);
    let (mut used, mut kept) = (2, 0);
    for item in items {
        let len = item.chars().count() + usize::from(kept > 0);
        if used + len > budget {
            break;
        }
        if kept > 0 {
            out.push(',');
        }
        out.push_str(item);
        used += len;
        kept += 1;
    }
    out.push(close);
    (out, kept)
}

/// A tool answer's JSON: compact (no indentation), on one line.
pub(crate) fn compact(value: &Value) -> String {
    value.to_string()
}

/// The display window of a query answer: which results it shows (`offset`, `max_rows`), how
/// many items of every nested collection (`max_items`), and the answer limit in characters.
#[derive(Clone, Copy)]
pub(crate) struct Page {
    pub(crate) offset: usize,
    pub(crate) max_rows: usize,
    pub(crate) max_items: usize,
    pub(crate) max_chars: usize,
}

/// A count argument: an integer >= 0 (a JSON number, or a string holding one), `None` when
/// it is absent or null.
pub(crate) fn count_arg(args: &Value, key: &str) -> Result<Option<usize>, String> {
    let value = match args.get(key) {
        None | Some(Value::Null) => return Ok(None),
        Some(value) => value,
    };
    let number = match value {
        Value::String(text) => text.trim().parse::<u64>().ok(),
        Value::Number(n) => n.as_u64().or_else(|| {
            n.as_f64()
                .filter(|f| *f >= 0.0 && f.fract() == 0.0 && *f <= u32::MAX as f64)
                .map(|f| f as u64)
        }),
        _ => None,
    };
    number
        .map(|n| Some(usize::try_from(n).unwrap_or(usize::MAX)))
        .ok_or_else(|| format!("argument \"{key}\" must be an integer >= 0, not {value}"))
}

/// The [`Page`] of a query tool's arguments (`offset` default 0, `max_rows` and `max_items`
/// default 100, at least 1) for `caller`.
pub(crate) fn page_of(args: &Value, caller: &ToolCaller) -> Result<Page, String> {
    Ok(Page {
        offset: count_arg(args, "offset")?.unwrap_or(0),
        max_rows: count_arg(args, "max_rows")?
            .unwrap_or(DEFAULT_MAX_ROWS)
            .max(1),
        max_items: count_arg(args, "max_items")?
            .unwrap_or(DEFAULT_MAX_ITEMS)
            .max(1),
        max_chars: caller.max_chars,
    })
}

/// The input schema entries of the paging arguments (`offset`, `max_rows`, `max_items`).
pub(crate) fn paging_properties(what: &str) -> [(&'static str, Value); 3] {
    [
        (
            "offset",
            json!({"type": "integer", "minimum": 0, "description": format!("Paging: skip the first offset {what} (default 0); a cut answer's note names the next page's offset.")}),
        ),
        (
            "max_rows",
            json!({"type": "integer", "minimum": 1, "description": format!("Most {what} returned per page (default {DEFAULT_MAX_ROWS}).")}),
        ),
        (
            "max_items",
            json!({"type": "integer", "minimum": 1, "description": format!("Most items shown of every nested list or map (default {DEFAULT_MAX_ITEMS}); a cut list ends with \"… N more\".")}),
        ),
    ]
}

/// An input schema of `required` and `properties` plus the paging arguments.
pub(crate) fn with_paging(mut schema: Value, what: &str) -> Value {
    if let Some(properties) = schema["properties"].as_object_mut() {
        for (key, value) in paging_properties(what) {
            properties.entry(key).or_insert(value);
        }
    }
    schema
}

/// The text of a `query` answer: the `head` line (the call of a saved query), a status line,
/// the results (compact JSON within the display limits, on one line) or the error's
/// diagnostic, the notes, and the profile table when asked.
///
/// The answer fits into `page.max_chars` without breaking the JSON: a list of results keeps
/// the leading WHOLE results that fit (never part of one), and a note names the results shown
/// (`shown results 101-160 of 443`) and the next page's `offset`; a single value that is a list
/// or an object keeps its leading whole items or entries (offset pages only a list of results);
/// a result that alone exceeds the limit is left out with a note.
pub(crate) fn query_text(
    response: &Value,
    commit_seq: u64,
    profile: bool,
    page: &Page,
    head: Option<String>,
) -> (bool, String) {
    let ok = response["ok"].as_bool().unwrap_or(false);
    let execution = &response["execution"];
    let elapsed = response["elapsed_ms"].as_f64().unwrap_or(0.0);
    let mut out: Vec<String> = head.into_iter().collect();
    let profile_table =
        (profile && execution["profile"].is_object()).then(|| profile_text(&execution["profile"]));
    if ok {
        let changes = &response["changes"];
        let changed = match (
            changes["data"].as_bool().unwrap_or(false),
            changes["schema"].as_bool().unwrap_or(false),
        ) {
            (true, true) => "changed the data and the schema",
            (true, false) => "changed the data",
            (false, true) => "changed the schema",
            (false, false) => "",
        };
        let display = &response["display"];
        let total = display["total_rows"].as_u64();
        let mut status = match total {
            Some(n) => format!("ok: {n} result{}", if n == 1 { "" } else { "s" }),
            None => "ok".to_string(),
        };
        status.push_str(&format!(" ({elapsed:.1} ms)"));
        if response["mutated"].as_bool().unwrap_or(false) {
            let what = if changed.is_empty() {
                "changed the graph"
            } else {
                changed
            };
            status.push_str(&format!("; {what}, committed as commit {commit_seq}"));
            if let Some(summary) = response["change_summary"]["text"]
                .as_str()
                .filter(|text| !text.is_empty())
            {
                status.push_str(&format!(" ({summary})"));
            }
        }
        out.push(status);
        let raw_view = response["preferred_view"] == "raw" || response["terminal"] == "profile";
        let shown = if raw_view {
            display["raw"].as_str().unwrap_or_default()
        } else {
            display["json"].as_str().unwrap_or_default()
        };
        let json = if raw_view { None } else { compact_json(shown) };
        let mut notes = Vec::new();
        match json {
            Some(json) => {
                // Everything but the results: the lines so far, the profile, the line breaks.
                let fixed = out
                    .iter()
                    .map(|line| line.chars().count() + 1)
                    .sum::<usize>()
                    + profile_table
                        .as_ref()
                        .map_or(0, |table| table.chars().count() + 1);
                let (body, page_notes) = fit_results(&json, display, page, fixed);
                if let Some(body) = body {
                    out.push(body);
                }
                notes.extend(page_notes);
                if display["items_cut"].as_bool().unwrap_or(false) {
                    notes.push(format!(
                        "note: nested lists and maps longer than max_items ({}) were cut: a cut \
                         list ends with the item \"… N more\", a cut map with the key \"…\"; pass \
                         a larger max_items to see more",
                        page.max_items
                    ));
                } else if display["total_rows"].is_null()
                    && let Some(notice) = display["notice"].as_str()
                {
                    notes.push(format!(
                        "note: {notice} (pass max_rows or max_items, or narrow the query)"
                    ));
                }
            }
            None => {
                if !shown.is_empty() {
                    out.push(shown.to_string());
                }
                if let Some(notice) = display["notice"].as_str() {
                    notes.push(format!(
                        "note: {notice} (pass max_rows or max_items, or narrow the query)"
                    ));
                }
            }
        }
        out.extend(notes);
    } else {
        out.push(format!(
            "error ({elapsed:.1} ms): the script failed and was rolled back; the graph is \
             unchanged."
        ));
        let error = &execution["error"];
        let text = error["diagnostic"]
            .as_str()
            .or_else(|| error["message"].as_str())
            .map(str::to_string)
            .unwrap_or_else(|| error.to_string());
        out.push(text);
    }
    out.extend(profile_table);
    (ok, out.join("\n"))
}

/// The results of a query answer (compact JSON `json` of the session's `display`) fitted into
/// `page.max_chars` minus the `fixed` characters of the rest of the answer, and the notes on
/// what is shown: see [`query_text`]. `None`: no result fits.
pub(crate) fn fit_results(
    json: &str,
    display: &Value,
    page: &Page,
    fixed: usize,
) -> (Option<String>, Vec<String>) {
    let max_chars = page.max_chars;
    let length = json.chars().count();
    let fits = fixed + length + NOTE_RESERVE <= max_chars;
    let budget = max_chars.saturating_sub(fixed + NOTE_RESERVE);
    let mut notes = Vec::new();
    let Some(total) = display["total_rows"].as_u64().map(|n| n as usize) else {
        // A single value: its leading whole items or entries.
        if fits {
            return (Some(json.to_string()), notes);
        }
        return match json_items(json) {
            Some((open, items)) => {
                let (body, kept) = json_prefix(open, &items, budget);
                notes.push(format!(
                    "note: shown {kept} of {} {} of the result (cut at a whole item to fit the \
                     answer limit of {max_chars} characters); offset pages only a list of \
                     results: return the items as results (e.g. without fold(), or with \
                     unfold()), or narrow the query",
                    items.len(),
                    if open == '[' { "items" } else { "entries" },
                ));
                (Some(body), notes)
            }
            None => {
                notes.push(format!(
                    "note: the result is one value of {length} characters, longer than the \
                     answer limit of {max_chars} characters: narrow the query"
                ));
                (None, notes)
            }
        };
    };
    let offset = display["offset"].as_u64().map_or(0, |n| n as usize);
    let shown = display["shown_rows"].as_u64().map_or(0, |n| n as usize);
    let (body, kept) = match json_items(json) {
        Some(('[', items)) if !fits => json_prefix('[', &items, budget),
        _ => (json.to_string(), shown),
    };
    let end = offset + kept;
    if total > 0 && offset >= total {
        notes.push(format!(
            "note: offset {offset} is past the last result ({total} in all)"
        ));
    } else if kept == 0 && shown > 0 {
        notes.push(format!(
            "note: the result at offset {offset} alone is longer than the answer limit of \
             {max_chars} characters and is left out: narrow it (values(..), select(..), a smaller \
             max_items), or skip it with offset {}",
            offset + 1
        ));
    } else if kept < shown {
        notes.push(format!(
            "note: shown results {}-{end} of {total}, cut at a whole result to fit the answer \
             limit of {max_chars} characters (a smaller max_items or a narrower query fits more \
             per page); more results follow: next page offset {end}",
            offset + 1
        ));
    } else if end < total {
        notes.push(format!(
            "note: shown results {}-{end} of {total} (max_rows sets the page size); more results \
             follow: next page offset {end}",
            offset + 1
        ));
    } else if offset > 0 && kept > 0 {
        notes.push(format!(
            "note: shown results {}-{end} of {total}; no more results",
            offset + 1
        ));
    }
    (Some(body), notes)
}

/// The per-step metrics as a compact text table (the layout of the CLI's "Traversal Metrics").
pub(crate) fn profile_text(profile: &Value) -> String {
    fn walk(list: &Value, depth: usize, out: &mut Vec<String>) {
        for m in list.as_array().into_iter().flatten() {
            let mut name = format!(
                "{}{}",
                "  ".repeat(depth),
                m["name"].as_str().unwrap_or("?")
            );
            if let Some(path) = m["path_recording"].as_str() {
                name.push_str(&format!(" [path: {path}]"));
            }
            out.push(format!(
                "{:<8} {name}  in {} out {} {:.3} ms {:.1}%",
                m["id"].as_str().unwrap_or(""),
                m["count_in"],
                m["counts"]["traverser_count"],
                m["duration_ns"].as_f64().unwrap_or(0.0) / 1e6,
                m["percent_duration"].as_f64().unwrap_or(0.0),
            ));
            walk(&m["nested"], depth + 1, out);
        }
    }
    let mut out = vec!["Traversal metrics (step id, step, traversers in/out, time):".to_string()];
    walk(&profile["metrics"], 0, &mut out);
    out.push(format!(
        "total {:.3} ms",
        profile["duration_ns"].as_f64().unwrap_or(0.0) / 1e6
    ));
    if let Some(rules) = profile["optimizer_rules_applied"].as_array()
        && !rules.is_empty()
    {
        let names: Vec<&str> = rules.iter().filter_map(Value::as_str).collect();
        out.push(format!("Optimizer rules applied: {}", names.join(", ")));
    }
    out.join("\n")
}
