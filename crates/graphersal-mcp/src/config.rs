//! The operator's configuration of the endpoint (`--mcp-config FILE`): which tools and
//! resources are offered, their descriptions, the instructions and the saved-query tools.
//! Nothing here is a named built-in profile: without a configuration the endpoint offers every
//! tool of its mode with its built-in description and the built-in instructions.
//!
//! The host reads the file; [`McpConfig::parse`] checks its JSON (a key starting with `_` is a
//! comment, every other unknown key an error with a did-you-mean) and reads an instructions
//! `file` through the host's callback (relative paths are the host's to resolve).

use std::fmt;

use serde_json::{Map, Value};

use crate::catalog::{Kind, all_tools};
use crate::resources::RESOURCES;

/// The longest instructions text and the longest description override, in characters.
pub const MAX_INSTRUCTIONS_CHARS: usize = 20_000;

/// The smallest answer limit in characters (room for a status line, some results and notes).
pub const MIN_ANSWER_CHARS: usize = 1000;

/// The longest `server_name`.
const MAX_SERVER_NAME_CHARS: usize = 100;

/// What the endpoint answers `initialize` as its instructions.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum Instructions {
    /// The built-in text (+ the read-only note).
    #[default]
    Builtin,
    /// The built-in text followed by the operator's notes (`--mcp-instructions`).
    Append(String),
    /// The operator's text only.
    Replace(String),
}

/// Which saved queries are tools of their own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SavedQueryTools {
    /// Folders (each with its subfolders); `None`: every folder unless `names` is given.
    folders: Option<Vec<String>>,
    /// Query names; `None`: no selection by name.
    names: Option<Vec<String>>,
    /// Query names never offered as tools.
    exclude: Vec<String>,
    /// Whether the tools take `offset`/`max_rows`/`max_items` (default no: the defaults apply
    /// and a call may still pass them).
    paging_params: bool,
}

impl SavedQueryTools {
    /// Every saved query (`--mcp-query-tools`).
    pub(crate) fn all() -> Self {
        Self {
            folders: None,
            names: None,
            exclude: Vec::new(),
            paging_params: false,
        }
    }

    /// Whether the saved query `name` in `folder` is a tool.
    pub(crate) fn selects(&self, name: &str, folder: Option<&str>) -> bool {
        if self.exclude.iter().any(|n| n == name) {
            return false;
        }
        if self.folders.is_none() && self.names.is_none() {
            return true;
        }
        let by_name = self
            .names
            .as_ref()
            .is_some_and(|names| names.iter().any(|n| n == name));
        let by_folder = self.folders.as_ref().is_some_and(|folders| {
            folder.is_some_and(|folder| {
                folders.iter().any(|f| {
                    folder == f
                        || folder
                            .strip_prefix(f.as_str())
                            .is_some_and(|rest| rest.starts_with('/'))
                })
            })
        });
        by_name || by_folder
    }
}

/// The operator's configuration of the MCP endpoint ([`crate::Mcp::with_config`]). The
/// default is the endpoint without a configuration file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct McpConfig {
    pub(crate) server_name: Option<String>,
    pub(crate) instructions: Instructions,
    /// Built-in tools offered (`None`: all of the mode).
    pub(crate) include: Option<Vec<String>>,
    pub(crate) exclude: Vec<String>,
    /// Tool name -> its description instead of the built-in one.
    pub(crate) overrides: Vec<(String, String)>,
    pub(crate) saved_query_tools: Option<SavedQueryTools>,
    /// Resource names listed (`None`: the default list).
    pub(crate) resources: Option<Vec<String>>,
    pub(crate) answer_limit_chars: Option<usize>,
}

/// A configuration that cannot be used: the message names the key (never echoes a secret; the
/// file holds none).
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct McpConfigError {
    /// What is wrong, naming the key.
    pub message: String,
}

impl McpConfigError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for McpConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for McpConfigError {}

/// The resources listed without a `resources` key (`graphersal://gql-reference` holds the GQL
/// support list the `gql` description points to).
pub(crate) const DEFAULT_RESOURCES: [&str; 6] = [
    "schema",
    "statistics",
    "dsl-reference",
    "examples",
    "saved-queries",
    "gql-reference",
];

const TOP_KEYS: [&str; 7] = [
    "server_name",
    "instructions",
    "descriptions",
    "tools",
    "saved_query_tools",
    "resources",
    "answer_limit_chars",
];

impl McpConfig {
    /// Parses the JSON `text` of a configuration file. `read_file` reads the instructions
    /// `file` the configuration names (the host resolves a relative path against the
    /// configuration file); its error is shown with the key.
    pub fn parse(
        text: &str,
        read_file: &mut dyn FnMut(&str) -> Result<String, String>,
    ) -> Result<Self, McpConfigError> {
        let value: Value = serde_json::from_str(text)
            .map_err(|err| McpConfigError::new(format!("not valid JSON: {err}")))?;
        let Value::Object(top) = value else {
            return Err(McpConfigError::new(
                "the configuration must be a JSON object",
            ));
        };
        check_keys(&top, &TOP_KEYS, "")?;
        let mut config = McpConfig::default();
        if let Some(value) = top.get("server_name") {
            let name = string(value, "server_name")?;
            let chars = name.chars().count();
            if name.trim().is_empty()
                || chars > MAX_SERVER_NAME_CHARS
                || name.chars().any(char::is_control)
            {
                return Err(McpConfigError::new(format!(
                    "\"server_name\" must be 1..{MAX_SERVER_NAME_CHARS} characters without \
                     control characters"
                )));
            }
            config.server_name = Some(name);
        }
        if let Some(value) = top.get("instructions") {
            config.instructions = parse_instructions(value, read_file)?;
        }
        if let Some(value) = top.get("descriptions") {
            // The tool descriptions are the short ones only: "short" is accepted so that
            // existing files keep working, "full" was removed (book: dev server, MCP).
            match string(value, "descriptions")?.as_str() {
                "short" => {}
                "full" => {
                    return Err(McpConfigError::new(
                        "\"descriptions\": \"full\" was removed: every tool has one short \
                         description now, and the long GQL reference is the resource \
                         graphersal://gql-reference. Remove the key (or write \"short\")",
                    ));
                }
                other => {
                    return Err(McpConfigError::new(format!(
                        "\"descriptions\" is {other:?}: the only value is \"short\" (the \
                         default; the key can be left out)"
                    )));
                }
            }
        }
        if let Some(value) = top.get("tools") {
            parse_tools(&mut config, value)?;
        }
        if let Some(value) = top.get("saved_query_tools") {
            config.saved_query_tools = Some(parse_saved_query_tools(value)?);
        }
        if let Some(value) = top.get("resources") {
            let names = strings(value, "resources")?;
            let known: Vec<&str> = RESOURCES.iter().map(|r| r.name).collect();
            for name in &names {
                if !known.contains(&name.as_str()) {
                    return Err(unknown("resources", name, &known));
                }
            }
            config.resources = Some(names);
        }
        if let Some(value) = top.get("answer_limit_chars") {
            let n = value
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .filter(|n| *n >= MIN_ANSWER_CHARS)
                .ok_or_else(|| {
                    McpConfigError::new(format!(
                        "\"answer_limit_chars\" must be a whole number of characters, at least \
                         {MIN_ANSWER_CHARS}"
                    ))
                })?;
            config.answer_limit_chars = Some(n);
        }
        Ok(config)
    }

    /// Checks the configuration against the server's mode: on a `read_only` server a writing
    /// tool named in `tools.include` or `tools.overrides` is an error (read-only always wins,
    /// it never drops a tool silently).
    pub fn check(&self, read_only: bool) -> Result<(), McpConfigError> {
        if !read_only {
            return Ok(());
        }
        let writing: Vec<&str> = all_tools()
            .iter()
            .filter(|t| !offered_read_only(t.name, t.kind))
            .map(|t| t.name)
            .collect();
        let named = self
            .include
            .iter()
            .flatten()
            .map(|n| ("tools.include", n.as_str()))
            .chain(
                self.overrides
                    .iter()
                    .map(|(n, _)| ("tools.overrides", n.as_str())),
            );
        for (key, name) in named {
            if writing.contains(&name) {
                return Err(McpConfigError::new(format!(
                    "\"{key}\" names the writing tool {name:?}, but the server is read-only \
                     (--mcp-read-only or --read-only): remove it from the configuration"
                )));
            }
        }
        Ok(())
    }

    /// Adds the shorthand flags of the host: `notes` (`--mcp-instructions`: instructions
    /// mode append), `query_tools` (`--mcp-query-tools`: every saved query a tool) and
    /// `max_chars` (`--mcp-max-chars`). A flag and the configuration key of the same setting
    /// together are an error (no precedence to remember).
    pub fn with_flags(
        mut self,
        notes: Option<String>,
        query_tools: bool,
        max_chars: Option<usize>,
    ) -> Result<Self, McpConfigError> {
        let both = |flag: &str, key: &str| {
            McpConfigError::new(format!(
                "{flag} and the configuration key \"{key}\" set the same thing: use one of them"
            ))
        };
        if let Some(notes) = notes {
            if self.instructions != Instructions::Builtin {
                return Err(both("--mcp-instructions", "instructions"));
            }
            self.instructions = Instructions::Append(notes);
        }
        if query_tools {
            if self.saved_query_tools.is_some() {
                return Err(both("--mcp-query-tools", "saved_query_tools"));
            }
            self.saved_query_tools = Some(SavedQueryTools::all());
        }
        if let Some(max_chars) = max_chars {
            if self.answer_limit_chars.is_some() {
                return Err(both("--mcp-max-chars", "answer_limit_chars"));
            }
            self.answer_limit_chars = Some(max_chars);
        }
        Ok(self)
    }

    /// Whether saved queries are tools of their own.
    pub fn has_saved_query_tools(&self) -> bool {
        self.saved_query_tools.is_some()
    }

    /// The answer limit in characters, if the configuration sets one.
    pub fn answer_limit_chars(&self) -> Option<usize> {
        self.answer_limit_chars
    }

    /// Whether the built-in tool `name` is offered by this configuration (before the mode).
    pub(crate) fn offers(&self, name: &str) -> bool {
        let included = match &self.include {
            Some(include) => include.iter().any(|n| n == name),
            // Without `tools.include` every tool but the opt-in ones.
            None => !OPT_IN_TOOLS.contains(&name),
        };
        included && !self.exclude.iter().any(|n| n == name)
    }

    /// The operator's description of the tool `name`, if any.
    pub(crate) fn override_of(&self, name: &str) -> Option<&str> {
        self.overrides
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, d)| d.as_str())
    }

    /// Whether the resource `name` is listed.
    pub(crate) fn lists_resource(&self, name: &str) -> bool {
        match &self.resources {
            Some(names) => names.iter().any(|n| n == name),
            None => DEFAULT_RESOURCES.contains(&name),
        }
    }

    /// Whether saved-query tools take the paging arguments.
    pub(crate) fn saved_query_paging(&self) -> bool {
        self.saved_query_tools
            .as_ref()
            .is_some_and(|s| s.paging_params)
    }
}

/// The built-in tools offered only when `tools.include` names them (never in the default
/// listing, never to the AI Chat).
pub(crate) const OPT_IN_TOOLS: [&str; 1] = ["usage_statistics"];

/// Whether a tool of `kind` named `name` is offered on a read-only server (`query`, `gql` and
/// `run_saved_query` run there under the read-only authorizer).
pub(crate) fn offered_read_only(name: &str, kind: Kind) -> bool {
    kind == Kind::Read || matches!(name, "query" | "gql" | "run_saved_query")
}

fn parse_instructions(
    value: &Value,
    read_file: &mut dyn FnMut(&str) -> Result<String, String>,
) -> Result<Instructions, McpConfigError> {
    let Value::Object(map) = value else {
        return Err(McpConfigError::new(
            "\"instructions\" must be an object: {\"mode\": \"builtin\" | \"append\" | \
             \"replace\", \"text\": \"...\"} or {\"mode\": ..., \"file\": \"notes.md\"}",
        ));
    };
    check_keys(map, &["mode", "text", "file"], "instructions.")?;
    let mode = match map.get("mode") {
        Some(value) => string(value, "instructions.mode")?,
        None => "builtin".to_string(),
    };
    let text = match (map.get("text"), map.get("file")) {
        (Some(_), Some(_)) => {
            return Err(McpConfigError::new(
                "\"instructions\" takes \"text\" or \"file\", not both",
            ));
        }
        (Some(text), None) => Some(string(text, "instructions.text")?),
        (None, Some(file)) => {
            let path = string(file, "instructions.file")?;
            Some(read_file(&path).map_err(|err| {
                McpConfigError::new(format!("\"instructions.file\" {path}: {err}"))
            })?)
        }
        (None, None) => None,
    };
    let text = match text {
        None => None,
        Some(text) => {
            let text = text.trim().to_string();
            let chars = text.chars().count();
            if text.is_empty() || chars > MAX_INSTRUCTIONS_CHARS {
                return Err(McpConfigError::new(format!(
                    "the instructions text has {chars} characters: expected 1..\
                     {MAX_INSTRUCTIONS_CHARS}"
                )));
            }
            Some(text)
        }
    };
    match (mode.as_str(), text) {
        ("builtin", None) => Ok(Instructions::Builtin),
        ("builtin", Some(_)) => Err(McpConfigError::new(
            "\"instructions.mode\" builtin takes no text: use \"append\" (after the built-in \
             text) or \"replace\" (instead of it)",
        )),
        ("append", Some(text)) => Ok(Instructions::Append(text)),
        ("replace", Some(text)) => Ok(Instructions::Replace(text)),
        ("append" | "replace", None) => Err(McpConfigError::new(format!(
            "\"instructions.mode\" {mode} needs \"text\" or \"file\""
        ))),
        (other, _) => Err(McpConfigError::new(format!(
            "\"instructions.mode\" is {other:?}: expected \"builtin\", \"append\" or \"replace\""
        ))),
    }
}

fn parse_tools(config: &mut McpConfig, value: &Value) -> Result<(), McpConfigError> {
    let Value::Object(map) = value else {
        return Err(McpConfigError::new(
            "\"tools\" must be an object: {\"include\": [..], \"exclude\": [..], \"overrides\": \
             {\"<tool>\": {\"description\": \"..\"}}}",
        ));
    };
    check_keys(map, &["include", "exclude", "overrides"], "tools.")?;
    let tools = all_tools();
    let known: Vec<&str> = tools.iter().map(|t| t.name).collect();
    let checked = |key: &str, names: Vec<String>| -> Result<Vec<String>, McpConfigError> {
        for name in &names {
            if !known.contains(&name.as_str()) {
                return Err(unknown(key, name, &known));
            }
        }
        Ok(names)
    };
    if let Some(value) = map.get("include") {
        config.include = Some(checked("tools.include", strings(value, "tools.include")?)?);
    }
    if let Some(value) = map.get("exclude") {
        config.exclude = checked("tools.exclude", strings(value, "tools.exclude")?)?;
    }
    if let Some(name) = config.exclude.iter().find(|name| {
        config
            .include
            .as_ref()
            .is_some_and(|include| include.contains(name))
    }) {
        return Err(McpConfigError::new(format!(
            "the tool {name:?} is in both \"tools.include\" and \"tools.exclude\""
        )));
    }
    if let Some(value) = map.get("overrides") {
        let Value::Object(overrides) = value else {
            return Err(McpConfigError::new(
                "\"tools.overrides\" must be an object: {\"<tool>\": {\"description\": \"..\"}}",
            ));
        };
        for (name, entry) in overrides {
            if name.starts_with('_') {
                continue;
            }
            if !known.contains(&name.as_str()) {
                return Err(unknown("tools.overrides", name, &known));
            }
            let key = format!("tools.overrides.{name}");
            let Value::Object(entry) = entry else {
                return Err(McpConfigError::new(format!(
                    "\"{key}\" must be an object: {{\"description\": \"..\"}}"
                )));
            };
            check_keys(entry, &["description"], &format!("{key}."))?;
            let Some(description) = entry.get("description") else {
                return Err(McpConfigError::new(format!(
                    "\"{key}\" needs \"description\""
                )));
            };
            let description = string(description, &format!("{key}.description"))?;
            let chars = description.trim().chars().count();
            if chars == 0 || chars > MAX_INSTRUCTIONS_CHARS {
                return Err(McpConfigError::new(format!(
                    "\"{key}.description\" has {chars} characters: expected 1..\
                     {MAX_INSTRUCTIONS_CHARS}"
                )));
            }
            if !config.offers(name) {
                return Err(McpConfigError::new(format!(
                    "\"{key}\": the tool {name:?} is not offered (tools.include / \
                     tools.exclude)"
                )));
            }
            config
                .overrides
                .push((name.clone(), description.trim().to_string()));
        }
    }
    Ok(())
}

fn parse_saved_query_tools(value: &Value) -> Result<SavedQueryTools, McpConfigError> {
    let Value::Object(map) = value else {
        return Err(McpConfigError::new(
            "\"saved_query_tools\" must be an object: {\"folders\": [..], \"names\": [..], \
             \"exclude\": [..], \"paging_params\": false} ({} = every saved query)",
        ));
    };
    check_keys(
        map,
        &["folders", "names", "exclude", "paging_params"],
        "saved_query_tools.",
    )?;
    let list = |key: &str| -> Result<Option<Vec<String>>, McpConfigError> {
        map.get(key)
            .map(|value| strings(value, &format!("saved_query_tools.{key}")))
            .transpose()
    };
    let paging_params = match map.get("paging_params") {
        None => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => {
            return Err(McpConfigError::new(
                "\"saved_query_tools.paging_params\" must be true or false",
            ));
        }
    };
    Ok(SavedQueryTools {
        folders: list("folders")?,
        names: list("names")?,
        exclude: list("exclude")?.unwrap_or_default(),
        paging_params,
    })
}

/// Every key of `map` is in `known` or starts with `_`; `prefix` names the object.
fn check_keys(
    map: &Map<String, Value>,
    known: &[&str],
    prefix: &str,
) -> Result<(), McpConfigError> {
    for key in map.keys() {
        if key.starts_with('_') || known.contains(&key.as_str()) {
            continue;
        }
        let hint = closest(key, known)
            .map(|k| format!(" (did you mean \"{prefix}{k}\"?)"))
            .unwrap_or_default();
        return Err(McpConfigError::new(format!(
            "unknown key \"{prefix}{key}\"{hint}; known: {} (a key starting with \"_\" is a \
             comment)",
            known.join(", ")
        )));
    }
    Ok(())
}

fn unknown(key: &str, name: &str, known: &[&str]) -> McpConfigError {
    let hint = closest(name, known)
        .map(|k| format!(" (did you mean {k:?}?)"))
        .unwrap_or_default();
    McpConfigError::new(format!(
        "\"{key}\" names {name:?}, which does not exist{hint}; known: {}",
        known.join(", ")
    ))
}

fn string(value: &Value, key: &str) -> Result<String, McpConfigError> {
    value
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| McpConfigError::new(format!("\"{key}\" must be a string")))
}

fn strings(value: &Value, key: &str) -> Result<Vec<String>, McpConfigError> {
    let error = || McpConfigError::new(format!("\"{key}\" must be a list of strings"));
    value
        .as_array()
        .ok_or_else(error)?
        .iter()
        .map(|v| v.as_str().map(str::to_string).ok_or_else(error))
        .collect()
}

/// The known name nearest to `given` (edit distance at most a third of its length, at least 2).
fn closest<'a>(given: &str, known: &[&'a str]) -> Option<&'a str> {
    let limit = (given.chars().count() / 3).max(2);
    known
        .iter()
        .map(|k| (distance(given, k), *k))
        .filter(|(d, _)| *d <= limit)
        .min_by_key(|(d, _)| *d)
        .map(|(_, k)| k)
}

fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cur = row[j + 1];
            row[j + 1] = if ca == *cb {
                prev
            } else {
                1 + prev.min(cur).min(row[j])
            };
            prev = cur;
        }
    }
    row[b.len()]
}
