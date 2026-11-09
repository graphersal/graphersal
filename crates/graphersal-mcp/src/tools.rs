//! The tool handlers: [`call_tool`] runs one tool for a [`ToolCaller`], every tool a thin
//! wrapper of one session operation; saved queries as tools; the `query` and `gql` runs.

use graphersal::exec::CancelToken;
use graphersal_session::{RunOptions, Session, playground_authorizer, read_only_authorizer};
use serde_json::{Value, json};

use crate::answer::{PAGING_ARGS, Page, compact, fit, page_of, paging_properties, query_text};
use std::sync::Arc;

use crate::catalog::{all_tools, offered_tools};
use crate::config::McpConfig;
use crate::host::{ClientRef, Ctx, Host, McpError, RunGuard};
use crate::protocol::MAX_TEXT_CHARS;
use crate::stats::McpStats;

/// Who calls a tool through [`call_tool`]: the MCP endpoint or an AI Chat conversation.
#[derive(Clone, Debug)]
pub struct ToolCaller {
    /// Only reading tools; `query` runs under the read-only authorizer.
    pub(crate) read_only: bool,
    /// Every saved query is a tool of its own (`--mcp-query-tools`).
    pub(crate) query_tools: bool,
    /// The change-feed client of the calls: `mcp`, or `ai` with the conversation id.
    pub(crate) client: ClientRef,
    /// The caller's own cancellation token of the queries it runs (an AI Chat turn's Stop),
    /// instead of a new token per query (registered with the host: `Host::run_started`).
    pub(crate) cancel: Option<CancelToken>,
    /// The largest answer text in characters (`--mcp-max-chars`, [`MAX_TEXT_CHARS`]).
    pub(crate) max_chars: usize,
    /// The operator's configuration (`--mcp-config`); the default offers every tool of the
    /// mode with its full description.
    pub(crate) config: Arc<McpConfig>,
    /// The endpoint's usage statistics (the `usage_statistics` tool); none for the AI Chat.
    pub(crate) stats: Option<Arc<McpStats>>,
}

impl ToolCaller {
    /// A caller whose session jobs run for `client`, offered only the reading tools when
    /// `read_only`; no saved query as a tool, a new cancellation token per query, answers of
    /// at most [`MAX_TEXT_CHARS`] characters.
    pub fn new(client: ClientRef, read_only: bool) -> Self {
        Self {
            read_only,
            query_tools: false,
            client,
            cancel: None,
            max_chars: MAX_TEXT_CHARS,
            config: Arc::new(McpConfig::default()),
            stats: None,
        }
    }

    /// Answers `usage_statistics` from the endpoint's `stats`.
    pub(crate) fn with_stats(mut self, stats: Arc<McpStats>) -> Self {
        self.stats = Some(stats);
        self
    }

    /// Offers the tools, descriptions and saved-query tools of the operator's `config`
    /// (its saved-query selection wins over [`ToolCaller::with_query_tools`]).
    pub fn with_config(mut self, config: Arc<McpConfig>) -> Self {
        self.config = config;
        self
    }

    /// Whether saved queries are tools of their own (query tools or the configuration).
    fn has_query_tools(&self) -> bool {
        self.query_tools || self.config.has_saved_query_tools()
    }

    /// Lists every saved query as a tool of its own (`--mcp-query-tools`).
    pub fn with_query_tools(mut self, query_tools: bool) -> Self {
        self.query_tools = query_tools;
        self
    }

    /// Runs the caller's queries under its own cancellation token (an AI Chat turn's Stop)
    /// instead of a new token per query.
    pub fn with_cancel(mut self, cancel: CancelToken) -> Self {
        self.cancel = Some(cancel);
        self
    }

    /// The largest answer text in characters.
    pub fn with_max_chars(mut self, max_chars: usize) -> Self {
        self.max_chars = max_chars;
        self
    }
}

/// The answer of a tool: its text (data for the model, fitted to the caller's `max_chars`) and
/// whether it is a tool error.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct ToolOutcome {
    /// The answer text.
    pub text: String,
    /// Whether the tool failed (the text is the diagnostic).
    pub is_error: bool,
}

/// A tool call refused before it ran: an unknown tool or arguments that are not an object
/// (MCP: the JSON-RPC error "invalid params"), or a failure of the session (`Internal`).
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum CallRefused {
    /// An unknown tool, or arguments that are not an object.
    Params(String),
    /// The host could not run a session job.
    Internal(String),
}

impl CallRefused {
    /// The message.
    pub fn message(&self) -> &str {
        match self {
            Self::Params(message) | Self::Internal(message) => message,
        }
    }
}

/// The tool catalog of a caller, as MCP `tools/list` entries (`name`, `title`, `description`,
/// `inputSchema`, `annotations`): every tool, only the reading ones (and `query`) when
/// read-only, plus every saved query as a tool of its own with the caller's query tools. The
/// MCP endpoint and the AI Chat agent list the same catalog.
pub fn tool_listings(host: &dyn Host, caller: &ToolCaller) -> Result<Vec<Value>, McpError> {
    let read_only = caller.read_only;
    let mut listed: Vec<Value> = offered_tools(read_only, &caller.config)
        .into_iter()
        .map(|tool| tool.listing(read_only))
        .collect();
    if caller.has_query_tools() {
        let context = Ctx {
            host,
            client: &caller.client,
        };
        listed.extend(
            query_tool_listings(&context, read_only, &caller.config).map_err(McpError::Failed)?,
        );
    }
    Ok(listed)
}

/// Runs the tool `name` with `args` for `caller` (the MCP endpoint's `tools/call` and the AI
/// Chat agent): the session jobs run through `host` for `caller`'s client (the dev server
/// queues them on its session thread like the browser's, and its change feed records their
/// commits with that client). A failing tool is a [`ToolOutcome`] with `is_error` (its text
/// the diagnostic, with the read-only view note).
pub fn call_tool(
    host: &dyn Host,
    caller: &ToolCaller,
    name: &str,
    args: &Value,
) -> Result<ToolOutcome, CallRefused> {
    let context = Ctx {
        host,
        client: &caller.client,
    };
    call_tool_with(&context, caller, name, args)
}

fn call_tool_with(
    context: &Ctx<'_>,
    caller: &ToolCaller,
    name: &str,
    args: &Value,
) -> Result<ToolOutcome, CallRefused> {
    let outcome = |result: Result<String, String>| match result {
        Ok(text) => ToolOutcome {
            text: fit(text, caller.max_chars),
            is_error: false,
        },
        Err(text) => ToolOutcome {
            text: fit(with_view_note(context, text), caller.max_chars),
            is_error: true,
        },
    };
    let Some(tool) = offered_tools(caller.read_only, &caller.config)
        .into_iter()
        .find(|t| t.name == name)
    else {
        if caller.has_query_tools() && !all_tools().iter().any(|t| t.name == name) {
            let wanted = name.to_string();
            let entry = context
                .call(move |s| s.get_query(&wanted))
                .map_err(CallRefused::Internal)?
                .map_err(|err| CallRefused::Internal(err.to_string()))?;
            let selected = caller
                .config
                .saved_query_tools
                .as_ref()
                .is_none_or(|s| s.selects(name, entry["folder"].as_str()));
            if !entry.is_null() && selected {
                let Some(given) = args.as_object() else {
                    return Err(CallRefused::Params("arguments must be an object".into()));
                };
                // The paging arguments, unless the query declares a parameter of the name.
                let declared = |key: &str| {
                    entry["params"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .any(|param| param["name"] == key)
                };
                let mut params = serde_json::Map::new();
                let mut call = json!({"name": name});
                for (key, value) in given {
                    if PAGING_ARGS.contains(&key.as_str()) && !declared(key) {
                        call[key] = value.clone();
                    } else {
                        params.insert(key.clone(), value.clone());
                    }
                }
                call["params"] = Value::Object(params);
                return Ok(outcome(run_saved_query(context, caller, &call)));
            }
        }
        let why = if !all_tools().iter().any(|t| t.name == name) {
            ""
        } else if !caller.config.offers(name) {
            " (not offered by this server's configuration)"
        } else {
            " (read-only: only reading tools)"
        };
        return Err(CallRefused::Params(format!("unknown tool {name:?}{why}")));
    };
    if !args.is_object() {
        return Err(CallRefused::Params("arguments must be an object".into()));
    }
    Ok(outcome(run_tool(context, caller, tool.name, args)))
}

/// The note an agent gets while the user has a read-only view of a past state open in the
/// browser (Store menu): what it serves and why writes fail. MCP cannot close the view (the
/// endpoint has no persistence tools); `None` on the live graph.
pub(crate) fn view_note(view: &Value) -> Option<String> {
    if view.is_null() {
        return None;
    }
    let commit = view["commitSeq"].as_u64().unwrap_or(0);
    // The target when it says more than the commit (a mark, a time).
    let target = view["target"].as_str().unwrap_or("");
    let target = if target.is_empty() || target.starts_with("commit ") {
        String::new()
    } else {
        format!(" ({target})")
    };
    Some(format!(
        "the served state is a read-only view at commit {commit}{target}, opened by the user in \
         the browser's Store menu: queries read that state, every write is refused until the \
         user goes back to the live graph"
    ))
}

/// A tool error's text, with the [`view_note`] when a read-only view is open.
pub(crate) fn with_view_note(context: &Ctx<'_>, text: String) -> String {
    match view_note(&context.host.view()) {
        Some(note) => format!("{text}\nnote: {note}."),
        None => text,
    }
}

pub(crate) fn tool_result(text: String, is_error: bool) -> Value {
    json!({
        "content": [{"type": "text", "text": text}],
        "isError": is_error,
    })
}

/// A schema or patch argument as JSON text: an object, or a string holding JSON. `mode` set on
/// a schema object replaces its own.
pub(crate) fn json_arg(args: &Value, key: &str, mode: Option<&str>) -> Result<String, String> {
    let mut value = match args.get(key) {
        None | Some(Value::Null) => return Err(format!("missing argument {key:?}")),
        Some(Value::String(text)) => serde_json::from_str::<Value>(text)
            .map_err(|err| format!("argument {key:?} is not JSON: {err}"))?,
        Some(other) => other.clone(),
    };
    if let (Some(mode), Some(map)) = (mode, value.as_object_mut()) {
        map.insert("mode".into(), json!(mode));
    }
    Ok(value.to_string())
}

/// The `confirm_lock` argument of `set_schema` / `patch_schema`.
pub(crate) fn confirm_lock_arg() -> Value {
    json!({"type": "boolean", "description": "Confirms a vector fill above the threshold (an estimate of 1 s or more, or 10,000 elements without an estimate): the whole database is locked while it runs. Default false: such a change is refused with the warning text and nothing is applied."})
}

pub(crate) fn confirm_lock(args: &Value) -> bool {
    args.get("confirm_lock")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// What the schema preview's planned vector fill means for a schema tool.
pub(crate) enum FillGate {
    /// Above the threshold without `confirm_lock`: the answer (nothing applied).
    Refused(String),
    /// Applied; the note before the answer (empty without a fill).
    Note(String),
}

/// The fill gate of a schema tool from the preview's `fill`/`fill_warning` (book page
/// "Vector Rules"): the warning text is the session's, never the tool's own.
pub(crate) fn fill_gate(validation: &Value, confirmed: bool) -> FillGate {
    let Some(warning) = validation.get("fill_warning").and_then(Value::as_str) else {
        return FillGate::Note(String::new());
    };
    let needs = validation
        .get("fill")
        .and_then(Value::as_array)
        .is_some_and(|fills| {
            fills
                .iter()
                .any(|f| f.get("needs_confirmation").and_then(Value::as_bool) == Some(true))
        });
    if needs && !confirmed {
        FillGate::Refused(format!(
            "Nothing was applied.\n{warning}\nRepeat with confirm_lock: true."
        ))
    } else {
        FillGate::Note(format!("{warning}\n"))
    }
}

pub(crate) fn str_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// `get_schema` / `infer_schema`: the schema summary by default (the same text for both: the
/// observed graph with the stored schema laid over it), the schema JSON with `format: "json"`
/// (the stored or the inferred one); `labels` narrows either to some labels.
fn schema_answer(
    context: &Ctx<'_>,
    caller: &ToolCaller,
    name: &str,
    args: &Value,
) -> Result<String, String> {
    let labels: Vec<String> = match args.get("labels") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::String(label)) => vec![label.clone()],
        Some(Value::Array(labels)) => labels
            .iter()
            .map(|label| {
                label
                    .as_str()
                    .map(str::to_string)
                    .ok_or_else(|| format!("labels holds label names, not {label}"))
            })
            .collect::<Result<_, _>>()?,
        Some(other) => return Err(format!("labels is a list of label names, not {other}")),
    };
    match str_arg(args, "format").unwrap_or("summary") {
        "summary" => {
            let spec = if labels.is_empty() {
                Value::Null
            } else {
                serde_json::json!({ "labels": labels })
            };
            Ok(context.call(move |s| s.schema_summary_text(&spec))??)
        }
        "json" => {
            let inferred = name == "infer_schema";
            let text = context.call(move |s| s.schema_text_of(inferred, &labels))??;
            if text == "null" {
                return Ok(
                    "null: the graph stores no schema (mode none). infer_schema shows \
                           what the data has; set_schema stores one."
                        .to_string(),
                );
            }
            if text.chars().count() > caller.max_chars {
                return Ok(format!(
                    "note: the schema JSON is larger than the answer limit; pass labels: \
                     [\"<label>\", ...] for some labels, or read the summary (no format)\n{text}"
                ));
            }
            Ok(text)
        }
        other => Err(format!("format is \"summary\" or \"json\", not {other:?}")),
    }
}

/// Runs one tool; `Err` is a tool error (its text is the diagnostic).
pub(crate) fn run_tool(
    context: &Ctx<'_>,
    caller: &ToolCaller,
    name: &str,
    args: &Value,
) -> Result<String, String> {
    match name {
        "query" => query(context, caller, args),
        "gql" => gql(context, caller, args),
        "get_schema" | "infer_schema" => schema_answer(context, caller, name, args),
        "validate_schema" => {
            let schema = json_arg(args, "schema", str_arg(args, "mode"))?;
            fallible(context, move |s| s.validate_schema(&schema))
        }
        "validate_schema_patch" => {
            let patch = json_arg(args, "patch", None)?;
            fallible(context, move |s| s.validate_schema_patch(&patch))
        }
        "diff_schema" => {
            let schema = json_arg(args, "schema", None)?;
            fallible(context, move |s| s.diff_schema(&schema))
        }
        "set_schema" => {
            let schema = json_arg(args, "schema", None)?;
            let mode = str_arg(args, "mode").map(str::to_string);
            let note = if schema.contains("\"from\"") {
                let proposed = json_arg(args, "schema", str_arg(args, "mode"))?;
                let validation = context
                    .call(move |s| s.validate_schema(&proposed))?
                    .map_err(|err| err.to_string())?;
                match fill_gate(&validation, confirm_lock(args)) {
                    FillGate::Refused(text) => return Err(text),
                    FillGate::Note(note) => note,
                }
            } else {
                String::new()
            };
            let info = context.call(move |s| s.set_schema(&schema, mode.as_deref()))??;
            Ok(format!("{note}Schema stored.\n{}", compact(&info)))
        }
        "patch_schema" => {
            let patch = json_arg(args, "patch", None)?;
            let note = if patch.contains("\"from\"") || patch.contains("\"embedder\"") {
                let proposed = patch.clone();
                let validation = context
                    .call(move |s| s.validate_schema_patch(&proposed))?
                    .map_err(|err| err.to_string())?;
                match fill_gate(&validation, confirm_lock(args)) {
                    FillGate::Refused(text) => return Err(text),
                    FillGate::Note(note) => note,
                }
            } else {
                String::new()
            };
            let text = context.call(move |s| s.patch_schema_text(&patch))??;
            Ok(format!(
                "{note}Schema patched; the stored schema is now:\n{}",
                text
            ))
        }
        "statistics" => {
            Ok(compact(&context.host.add_process_statistics(
                context.call(|s| s.statistics_with_process())??,
            )))
        }
        "workload_statistics" => {
            let max_rows = args
                .get("max_rows")
                .and_then(Value::as_u64)
                .unwrap_or(50)
                .max(1) as usize;
            let mut report = match args.get("history").and_then(Value::as_str) {
                None => context.call(|s| s.workload_report())??,
                Some(tier) => {
                    let tier = tier.to_string();
                    context.call(move |s| s.workload_report_with_history(&tier))??
                }
            };
            if let Some(map) = report.as_object_mut() {
                for list in ["queries", "properties", "locks", "jobs"] {
                    let omitted = match map.get_mut(list).and_then(Value::as_array_mut) {
                        Some(rows) if rows.len() > max_rows => {
                            let omitted = rows.len() - max_rows;
                            rows.truncate(max_rows);
                            omitted
                        }
                        _ => continue,
                    };
                    map.insert(format!("{list}_omitted"), json!(omitted));
                }
            }
            Ok(format!(
                "Workload statistics (times in microseconds, rows sorted by total time):\n{}",
                compact(&report)
            ))
        }
        "journal" => {
            let mut spec = serde_json::Map::new();
            for key in [
                "after",
                "before",
                "newest_first",
                "level",
                "since",
                "source",
                "kind",
                "limit",
            ] {
                if let Some(value) = args.get(key).filter(|v| !v.is_null()) {
                    spec.insert(key.to_string(), value.clone());
                }
            }
            spec.entry("limit").or_insert(json!(100));
            let spec = Value::Object(spec);
            let page = context.call(move |s| s.journal(&spec))??;
            Ok(format!(
                "System journal (times in microseconds since the Unix epoch, UTC):\n{}",
                compact(&page)
            ))
        }
        "advise" => {
            let spec = json!({"everything": args.get("everything").and_then(Value::as_bool).unwrap_or(false)});
            let answer = context.call(move |s| s.advise(&spec))?;
            Ok(format!(
                "Storage advice (memory_bytes: freed, negative = taken):\n{}",
                compact(&answer)
            ))
        }
        "apply_recommendations" => {
            let ids: Vec<String> = args
                .get("ids")
                .and_then(Value::as_array)
                .and_then(|ids| {
                    ids.iter()
                        .map(|id| id.as_str().map(str::to_string))
                        .collect::<Option<Vec<_>>>()
                })
                .ok_or("missing argument \"ids\" (a list of recommendation ids from advise)")?;
            let spec = json!({"everything": args.get("everything").and_then(Value::as_bool).unwrap_or(false)});
            let answer = context.call(move |s| s.apply_recommendations(&ids, &spec))?;
            if answer["ok"] == json!(true) {
                Ok(format!(
                    "Applied in one commit (memory_bytes: freed, negative = taken):\n{}",
                    compact(&answer)
                ))
            } else {
                Err(format!(
                    "Nothing was applied:\n{}",
                    answer["error"].as_str().unwrap_or_default()
                ))
            }
        }
        "list_jobs" => Ok(format!(
            "Scheduled jobs (times in microseconds since the Unix epoch):\n{}",
            compact(&context.call(|s| s.jobs_view())??)
        )),
        "memory_report" => {
            let mut spec = match args {
                Value::Object(map) => Value::Object(map.clone()),
                _ => json!({}),
            };
            // A bounded answer by default: the 50 property and label rows with the most bytes
            // (`max_rows`/`max_labels` still win over the limit).
            if let Some(map) = spec.as_object_mut() {
                map.entry("limit").or_insert(json!(50));
            }
            let answer = context.call(move |s| s.memory_report(&spec))?;
            if answer["ok"] == json!(true) {
                Ok(format!(
                    "Memory report (the graph was not changed; row bytes are estimates, rows \
                     sorted by bytes; components say which process figures are exact):\n{}",
                    compact(&answer["report"])
                ))
            } else {
                Err(format!(
                    "The memory report was refused:\n{}",
                    answer["error"].as_str().unwrap_or_default()
                ))
            }
        }
        "list_saved_queries" => list_saved_queries(context, caller.read_only, args),
        "run_saved_query" => run_saved_query(context, caller, args),
        "list_compressions" => {
            let list = context.call(|s| s.list_compressions())??;
            Ok(if list.as_array().is_some_and(Vec::is_empty) {
                "The graph has no compression rules (define_compression adds one).".to_string()
            } else {
                compact(&list)
            })
        }
        "estimate_compression" => {
            let mut spec = args.clone();
            // `min_bytes` (the DSL's spelling) is accepted too.
            if let Some(map) = spec.as_object_mut()
                && let Some(value) = map.remove("min_bytes")
            {
                map.entry("minBytes").or_insert(value);
            }
            let answer = context.call(move |s| s.estimate_compression(&spec))?;
            if answer["ok"] == json!(true) {
                Ok(format!(
                    "Estimate (the graph was not changed; best is the index of the candidate \
                     that saves the most, savedBytes = plainBytes - storedBytes - \
                     dictionaryBytes):\n{}",
                    compact(&answer["estimate"])
                ))
            } else {
                Err(format!(
                    "The estimate was refused:\n{}",
                    compact(&answer["errors"])
                ))
            }
        }
        "define_compression" => {
            let mut spec = args.clone();
            // `min_bytes` (the DSL's spelling) is accepted too.
            if let Some(map) = spec.as_object_mut()
                && let Some(value) = map.remove("min_bytes")
            {
                map.entry("minBytes").or_insert(value);
            }
            let answer = context.call(move |s| s.define_compression(&spec))?;
            if answer["ok"] == json!(true) {
                Ok(format!("Rule stored.\n{}", compact(&answer["rule"])))
            } else {
                Err(format!(
                    "The rule was refused:\n{}",
                    compact(&answer["errors"])
                ))
            }
        }
        "drop_compression" => {
            let name = str_arg(args, "name")
                .ok_or("missing argument \"name\" (list_compressions)")?
                .to_string();
            let dropped = context.call(move |s| s.drop_compression(&name))??;
            Ok(if dropped {
                "Rule dropped; its values are plain again.".to_string()
            } else {
                "There is no rule of that name (list_compressions).".to_string()
            })
        }
        "recompress" => {
            let name = str_arg(args, "name")
                .ok_or("missing argument \"name\" (list_compressions)")?
                .to_string();
            fallible(context, move |s| s.recompress(&name))
        }
        "list_indexes" => {
            let list = context.call(|s| s.list_indexes())??;
            Ok(if list.as_array().is_some_and(Vec::is_empty) {
                "The graph has no property indexes (create_index adds one).".to_string()
            } else {
                compact(&list)
            })
        }
        "create_index" => {
            let spec = args.clone();
            let answer = context.call(move |s| s.create_index(&spec))?;
            if answer["ok"] == json!(true) {
                Ok(format!("Index created.\n{}", compact(&answer["index"])))
            } else {
                Err(format!(
                    "The index was refused:\n{}",
                    compact(&answer["errors"])
                ))
            }
        }
        "drop_index" => {
            let name = str_arg(args, "name")
                .ok_or("missing argument \"name\" (list_indexes)")?
                .to_string();
            let dropped = context.call(move |s| s.drop_index(&name))??;
            Ok(if dropped {
                "Index dropped.".to_string()
            } else {
                "There is no index of that name (list_indexes).".to_string()
            })
        }
        "graph_info" => {
            let mut info = context.call(|s| s.info())??;
            // The host's own fields: the source, the store, damage found on disk, ...
            context
                .host
                .add_graph_info(&mut info)
                .map_err(|err| err.to_string())?;
            let view = context.host.view();
            if let Some(note) = view_note(&view) {
                info["note"] = json!(note);
            }
            info["view"] = view;
            info["readOnly"] = json!(caller.read_only);
            info["answerLimitChars"] = json!(caller.max_chars);
            Ok(compact(&info))
        }
        "usage_statistics" => {
            let Some(stats) = &caller.stats else {
                return Err("this caller has no usage statistics (the MCP endpoint has)".into());
            };
            let mut answer = stats.to_json().map_err(|err| err.to_string())?;
            let max_rows = args
                .get("max_rows")
                .and_then(Value::as_u64)
                .map_or(USAGE_ROWS, |n| n.max(1) as usize);
            for list in ["tools", "savedQueries", "clients"] {
                if let Some(rows) = answer.get_mut(list).and_then(Value::as_array_mut)
                    && rows.len() > max_rows
                {
                    let omitted = rows.len() - max_rows;
                    rows.truncate(max_rows);
                    answer[format!("{list}_omitted")] = json!(omitted);
                }
            }
            Ok(compact(&answer))
        }
        other => Err(format!("unknown tool {other:?}")),
    }
}

/// Rows per list of `usage_statistics` unless `max_rows` says otherwise.
const USAGE_ROWS: usize = 50;

pub(crate) fn fallible<E: std::fmt::Display + Send + 'static>(
    context: &Ctx<'_>,
    job: impl FnOnce(&mut Session) -> Result<Value, E> + Send + 'static,
) -> Result<String, String> {
    Ok(compact(&context.call(job)?.map_err(|err| err.to_string())?))
}

/// The saved queries as the agent sees them: the session's entries (`folder` prefix, `search`
/// in name or description), compact.
/// The saved queries as compact entries (no `null` fields, `effects` as their texts, no empty
/// `denied_writes`), `folder` and `search` as `list_saved_queries` takes them; `read_only`
/// leaves out every query that writes.
pub(crate) fn saved_queries(
    context: &Ctx<'_>,
    folder: Option<String>,
    search: Option<String>,
    read_only: bool,
) -> Result<Vec<Value>, String> {
    let list = context.call(move |s| s.list_queries(folder.as_deref()))??;
    let search = search.map(|s| s.to_lowercase());
    Ok(list
        .as_array()
        .into_iter()
        .flatten()
        .filter(|entry| !(read_only && entry["writes"].as_bool().unwrap_or(false)))
        .filter(|entry| {
            search.as_deref().is_none_or(|needle| {
                ["name", "description"].iter().any(|key| {
                    entry[*key]
                        .as_str()
                        .is_some_and(|text| text.to_lowercase().contains(needle))
                })
            })
        })
        .map(|entry| {
            let mut compact = serde_json::Map::new();
            for (key, value) in entry.as_object().into_iter().flatten() {
                let value = match key.as_str() {
                    // The effect texts are enough for an agent (`Create Data(vertex "person")`).
                    "effects" | "denied_writes" => Value::Array(
                        value
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|effect| effect.get("text").cloned())
                            .collect(),
                    ),
                    _ => value.clone(),
                };
                let empty = value.as_array().is_some_and(Vec::is_empty);
                if !value.is_null() && !(empty && key != "params") {
                    compact.insert(key.clone(), value);
                }
            }
            Value::Object(compact)
        })
        .collect())
}

/// The `list_saved_queries` tool.
pub(crate) fn list_saved_queries(
    context: &Ctx<'_>,
    read_only: bool,
    args: &Value,
) -> Result<String, String> {
    let folder = str_arg(args, "folder").map(str::to_string);
    let search = str_arg(args, "search").map(str::to_string);
    let filtered = folder.is_some() || search.is_some();
    let list = saved_queries(context, folder, search, read_only)?;
    if list.is_empty() {
        return Ok(if filtered {
            "No saved query matches (call list_saved_queries without arguments to see all).".into()
        } else {
            "The graph has no saved queries: write the query with the query tool.".into()
        });
    }
    Ok(format!(
        "{} saved quer{} (run one with run_saved_query {{name, params}}; status \"error\" means \
         it cannot run, see \"error\"; writes: true means it changes data, see \"effects\": run \
         it only when the user asked for that change):\n{}",
        list.len(),
        if list.len() == 1 { "y" } else { "ies" },
        compact(&Value::Array(list))
    ))
}

/// The `run_saved_query` tool (and a saved query's own tool with `--mcp-query-tools`): the
/// call text `g.query(name, #{..})` run like the `query` tool.
pub(crate) fn run_saved_query(
    context: &Ctx<'_>,
    caller: &ToolCaller,
    args: &Value,
) -> Result<String, String> {
    let name = str_arg(args, "name").ok_or("missing argument \"name\" (list_saved_queries)")?;
    let params = match args.get("params") {
        None | Some(Value::Null) => Value::Null,
        Some(Value::String(text)) => serde_json::from_str(text)
            .map_err(|err| format!("argument \"params\" is not a JSON object: {err}"))?,
        Some(other) => other.clone(),
    };
    let script = graphersal_session::queries::query_call_text(name, &params)?;
    if caller.read_only {
        let wanted = name.to_string();
        let entry = context.call(move |s| s.get_query(&wanted))??;
        if entry["writes"].as_bool().unwrap_or(false) {
            return Err(format!(
                "the saved query \"{name}\" changes data, and this server is read-only for MCP"
            ));
        }
    }
    let page = page_of(args, caller)?;
    let head = format!("call: {script}");
    run_result(run_script(
        context,
        caller,
        script,
        false,
        page,
        Some(head),
    )?)
}

/// With `--mcp-query-tools`: every saved query as a tool listing (`inputSchema` from its
/// parameters); a query named like a built-in tool is left out (`run_saved_query` reaches it).
pub(crate) fn query_tool_listings(
    context: &Ctx<'_>,
    read_only: bool,
    config: &McpConfig,
) -> Result<Vec<Value>, String> {
    let list = saved_queries(context, None, None, read_only)?;
    let paging = config.saved_query_paging();
    Ok(list
        .iter()
        .filter_map(|entry| {
            let name = entry["name"].as_str()?;
            if all_tools().iter().any(|t| t.name == name) {
                return None;
            }
            if let Some(selection) = &config.saved_query_tools
                && !selection.selects(name, entry["folder"].as_str())
            {
                return None;
            }
            let mut properties = serde_json::Map::new();
            let mut required = Vec::new();
            for param in entry["params"].as_array().into_iter().flatten() {
                let Some(param_name) = param["name"].as_str() else {
                    continue;
                };
                let mut schema = param["schema"].clone();
                if let (Some(default), Some(map)) = (param.get("default"), schema.as_object_mut()) {
                    map.insert("default".into(), default.clone());
                }
                properties.insert(param_name.to_string(), schema);
                if param["required"].as_bool().unwrap_or(false) {
                    required.push(json!(param_name));
                }
            }
            // Paging like run_saved_query's, unless a parameter has the name (left out with
            // `paging_params: false`, the defaults apply).
            if paging {
                for (key, value) in paging_properties("results") {
                    properties.entry(key).or_insert(value);
                }
            }
            let mut description = entry["description"]
                .as_str()
                .map_or_else(|| format!("The saved query {name}."), str::to_string);
            if !description.trim_end().ends_with(['.', '!', '?']) {
                description.push('.');
            }
            let writes = entry["writes"].as_bool().unwrap_or(false);
            let effects: Vec<&str> = entry["effects"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            if writes {
                description.push_str(&format!(
                    " Saved query that CHANGES DATA ({}); run it only when the user asked for \
                     that change. The same as run_saved_query.",
                    effects.join(", ")
                ));
            }
            if let Some(error) = entry["error"].as_str() {
                description.push_str(&format!(" It cannot run: {error}"));
            }
            let destructive = effects
                .iter()
                .any(|effect| effect.starts_with("Update") || effect.starts_with("Delete"));
            Some(json!({
                "name": name,
                "title": name,
                "description": description,
                "inputSchema": {
                    "type": "object",
                    "properties": properties,
                    "required": required,
                    "additionalProperties": false,
                },
                "annotations": {
                    "title": name,
                    "readOnlyHint": !writes,
                    "destructiveHint": writes && destructive,
                    "idempotentHint": !writes,
                    "openWorldHint": false,
                },
            }))
        })
        .collect())
}

/// The `gql` tool: the call text `g.gql(query, #{params})` (the query one Rhai string literal,
/// the parameters Rhai literals) run like the `query` tool: a GQL write commits like a script,
/// and a read-only caller runs it under the read-only authorizer, which refuses the write.
pub(crate) fn gql(context: &Ctx<'_>, caller: &ToolCaller, args: &Value) -> Result<String, String> {
    let text = str_arg(args, "query").ok_or("missing argument \"query\" (the GQL query to run)")?;
    let params = match args.get("params") {
        None | Some(Value::Null) => Value::Null,
        Some(Value::String(text)) => serde_json::from_str(text)
            .map_err(|err| format!("argument \"params\" is not a JSON object: {err}"))?,
        Some(other) => other.clone(),
    };
    let script = graphersal_session::queries::gql_call_text(text, &params)?;
    let profile = args
        .get("profile")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let page = page_of(args, caller)?;
    run_result(run_script(context, caller, script, profile, page, None)?)
}

/// The `query` tool: one run of the session (like the browser's Run), the answer as text.
pub(crate) fn query(
    context: &Ctx<'_>,
    caller: &ToolCaller,
    args: &Value,
) -> Result<String, String> {
    let script = str_arg(args, "script")
        .ok_or("missing argument \"script\" (the Graphersal script to run)")?
        .to_string();
    let profile = args
        .get("profile")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let page = page_of(args, caller)?;
    run_result(run_script(context, caller, script, profile, page, None)?)
}

/// Runs `script` on the session (under the read-only authorizer for a read-only caller; with
/// the caller's own cancellation token when it has one): whether it succeeded and the answer's
/// text.
pub(crate) fn run_script(
    context: &Ctx<'_>,
    caller: &ToolCaller,
    script: String,
    profile: bool,
    page: Page,
    head: Option<String>,
) -> Result<(bool, String), String> {
    let options = RunOptions {
        profile,
        max_rows: Some(page.max_rows),
        max_items: Some(page.max_items),
        offset: Some(page.offset),
        ..RunOptions::default()
    };
    let authorizer = if caller.read_only {
        read_only_authorizer()
    } else {
        playground_authorizer()
    };
    // A token of this run alone (the caller's own: an AI Chat turn's), registered with the host
    // before the job is queued, so a cancel of it is never lost and reaches no other run.
    let cancel = caller.cancel.clone().unwrap_or_default();
    let _run = RunGuard {
        host: context.host,
        run: context
            .host
            .run_started_with_text(context.client, &cancel, &script),
    };
    let answer = context.call(move |s| {
        let text = s.execute_with_cancel(&script, &options, authorizer, &cancel);
        (text, s.graph().last_commit_seq())
    });
    let (text, commit_seq) = answer?;
    let commit_seq = commit_seq.map_err(|err| err.to_string())?;
    let response: Value =
        serde_json::from_str(&text).map_err(|err| format!("malformed engine answer: {err}"))?;
    Ok(query_text(&response, commit_seq, profile, &page, head))
}

/// `Ok` for a successful run, `Err` (a tool error) for a failed one.
pub(crate) fn run_result((ok, text): (bool, String)) -> Result<String, String> {
    if ok { Ok(text) } else { Err(text) }
}
