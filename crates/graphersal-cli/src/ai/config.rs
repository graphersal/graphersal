//! The `--ai FILE` configuration: which LLM provider the AI Chat calls, with which model and
//! key, and its limits.
//!
//! The file is JSON: every value has its type checked, a key starting with `_` (a comment) is
//! ignored, any other unknown key is a startup warning, except one within edit distance 2 of a
//! known key, which is an error with a "did you mean" hint (a typo must not silently drop a
//! safety setting such as `read_only`). Example:
//!
//! ```json
//! {"provider": "anthropic", "model": "<model id>", "base_url": null,
//!  "api_key_env": "ANTHROPIC_API_KEY", "read_only": false,
//!  "max_tool_calls_per_turn": 20, "max_tokens": 4096}
//! ```
//!
//! There is no default model: the file names it. The API key comes from the environment
//! variable `api_key_env` names; an inline `api_key` is accepted with a warning (the file may be
//! shared or committed). The key is never logged and never shown: [`AiConfig`]'s `Debug`
//! redacts it.

use serde_json::{Map, Value};

use crate::args_file::edit_distance;

/// Default `max_tool_calls_per_turn`.
pub const DEFAULT_MAX_TOOL_CALLS_PER_TURN: u32 = 20;
/// Default `max_tokens` (the answer length bound of one provider call).
pub const DEFAULT_MAX_TOKENS: u32 = 4096;
/// The keys an `--ai` file may hold.
const KNOWN_KEYS: &[&str] = &[
    "provider",
    "model",
    "base_url",
    "api_key_env",
    "api_key",
    "read_only",
    "max_tool_calls_per_turn",
    "max_tokens",
    "system_prompt",
    "system_prompt_file",
];
/// The most characters of the custom system prompt text.
pub const MAX_SYSTEM_PROMPT_CHARS: usize = 20_000;

/// The provider API an [`AiConfig`] talks to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    /// OpenAI Chat Completions with `tools` (OpenAI, Azure, OpenRouter, Groq, Ollama, LM Studio,
    /// vLLM, ... through `base_url`).
    OpenAi,
    /// Anthropic Messages API with tool use.
    Anthropic,
}

impl Provider {
    /// The name in the `--ai` file.
    pub fn name(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::Anthropic => "anthropic",
        }
    }

    /// The API this provider speaks, as the server's start line names it: `openai` is a format
    /// that many providers offer (Gemini, Ollama, vLLM, ...), not the provider itself.
    pub fn api_name(self) -> &'static str {
        match self {
            Self::OpenAi => "the OpenAI-compatible API",
            Self::Anthropic => "the Anthropic API",
        }
    }

    /// The base URL used when the file gives none.
    pub fn default_base_url(self) -> &'static str {
        match self {
            Self::OpenAi => "https://api.openai.com/v1",
            Self::Anthropic => "https://api.anthropic.com",
        }
    }

    /// The path of the one endpoint the adapter calls, appended to the base URL.
    fn endpoint_path(self) -> &'static str {
        match self {
            Self::OpenAi => "/chat/completions",
            Self::Anthropic => "/v1/messages",
        }
    }
}

/// A validated `--ai` configuration.
#[derive(Clone, PartialEq, Eq)]
pub struct AiConfig {
    /// The provider API.
    pub provider: Provider,
    /// The model id, passed to the provider as is.
    pub model: String,
    /// The base URL without a trailing `/`; [`Provider::default_base_url`] when the file has none.
    pub base_url: String,
    /// Whether the file set `base_url` (for the safety text: a default URL is the provider's).
    pub custom_base_url: bool,
    /// The API key, if any (a local OpenAI-compatible server may need none). Never logged.
    pub(crate) api_key: Option<String>,
    /// Where the key came from, for diagnostics: the variable name, or `"api_key"`.
    pub key_source: Option<String>,
    /// The agent may only read (the session's read-only authorizer, as `--mcp-read-only`).
    pub read_only: bool,
    /// The most tool calls one chat turn may make.
    pub max_tool_calls_per_turn: u32,
    /// The answer length bound of one provider call.
    pub max_tokens: u32,
    /// The operator's own system prompt text (`system_prompt` or `system_prompt_file`), if any.
    pub system_prompt: Option<String>,
}

impl std::fmt::Debug for AiConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AiConfig")
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("key_source", &self.key_source)
            .field("read_only", &self.read_only)
            .field("max_tool_calls_per_turn", &self.max_tool_calls_per_turn)
            .field("max_tokens", &self.max_tokens)
            .field(
                "system_prompt_chars",
                &self.system_prompt.as_ref().map(|t| t.chars().count()),
            )
            .finish()
    }
}

/// A configuration together with the warnings to print at startup.
#[derive(Debug)]
pub struct LoadedConfig {
    /// The configuration.
    pub config: AiConfig,
    /// Warnings (an inline key, a key sent over plain HTTP); never contain the key.
    pub warnings: Vec<String>,
}

impl AiConfig {
    /// Reads and validates the `--ai` file; the key is read from the process environment.
    pub fn load(path: &str) -> Result<LoadedConfig, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|err| format!("Error: cannot read the --ai file '{path}': {err}"))?;
        let base = std::path::Path::new(path).parent();
        Self::parse_in(&text, base, |name| std::env::var(name).ok())
            .map_err(|err| format!("Error: invalid --ai file '{path}': {err}"))
    }

    /// Parses and validates the text of an `--ai` file; `env` looks up an environment variable.
    #[cfg(test)]
    pub fn parse(text: &str, env: impl Fn(&str) -> Option<String>) -> Result<LoadedConfig, String> {
        Self::parse_in(text, None, env)
    }

    /// Like [`AiConfig::parse`]; a relative `system_prompt_file` is read from `base` (the
    /// directory of the config file; the current directory when `None`).
    pub fn parse_in(
        text: &str,
        base: Option<&std::path::Path>,
        env: impl Fn(&str) -> Option<String>,
    ) -> Result<LoadedConfig, String> {
        let value: Value = serde_json::from_str(text).map_err(|err| {
            format!("not valid JSON ({err}). Help: the file is one JSON object, e.g. {EXAMPLE}")
        })?;
        let Value::Object(map) = value else {
            return Err(format!("expected a JSON object, e.g. {EXAMPLE}"));
        };
        let mut warnings = Vec::new();
        for key in map.keys() {
            if key.starts_with('_') || KNOWN_KEYS.contains(&key.as_str()) {
                continue;
            }
            if let Some(near) = KNOWN_KEYS
                .iter()
                .find(|known| edit_distance(key, known) <= 2)
            {
                return Err(format!(
                    "unknown key \"{key}\": did you mean \"{near}\"? The keys are: {}",
                    KNOWN_KEYS.join(", ")
                ));
            }
            warnings.push(format!(
                "warning: the --ai file: unknown key \"{key}\" ignored; the keys are: {} \
                 (a key starting with \"_\" is a comment and never warns)",
                KNOWN_KEYS.join(", ")
            ));
        }

        let provider = match required_string(&map, "provider")?.as_str() {
            "openai" => Provider::OpenAi,
            "anthropic" => Provider::Anthropic,
            other => {
                return Err(format!(
                    "unknown provider \"{other}\": use \"openai\" (Chat Completions: OpenAI, \
                     Azure, OpenRouter, Groq, Ollama, LM Studio, vLLM, ... with base_url) or \
                     \"anthropic\""
                ));
            }
        };
        let model = required_string(&map, "model")?;
        let base_url = optional_string(&map, "base_url")?;
        let custom_base_url = base_url.is_some();
        let base_url = match base_url {
            Some(url) => check_base_url(&url)?,
            None => provider.default_base_url().to_string(),
        };

        let api_key_env = optional_string(&map, "api_key_env")?;
        let inline_key = optional_string(&map, "api_key")?;
        let (api_key, key_source) = match (api_key_env, inline_key) {
            (Some(_), Some(_)) => {
                return Err(
                    "both \"api_key_env\" and \"api_key\" are set: keep only \"api_key_env\" \
                     (the name of the environment variable holding the key)"
                        .to_string(),
                );
            }
            (Some(name), None) => match env(&name).filter(|key| !key.trim().is_empty()) {
                Some(key) => (Some(key.trim().to_string()), Some(name)),
                None => {
                    return Err(format!(
                        "the environment variable {name} (api_key_env) is not set or empty. \
                         Help: export {name}=<your key> before starting graphersal"
                    ));
                }
            },
            (None, Some(key)) => {
                warnings.push(
                    "warning: the --ai file holds the API key inline (\"api_key\"); prefer \
                     \"api_key_env\" with the name of an environment variable, so the key does \
                     not end up in a shared or committed file"
                        .to_string(),
                );
                (Some(key), Some("api_key".to_string()))
            }
            (None, None) => (None, None),
        };
        if api_key.is_none() && (provider == Provider::Anthropic || !custom_base_url) {
            return Err(format!(
                "no API key: set \"api_key_env\" to the name of the environment variable that \
                 holds it (e.g. \"{}\"); only a custom \"base_url\" of an OpenAI-compatible \
                 local server may go without a key",
                match provider {
                    Provider::OpenAi => "OPENAI_API_KEY",
                    Provider::Anthropic => "ANTHROPIC_API_KEY",
                }
            ));
        }

        let read_only = match map.get("read_only") {
            None | Some(Value::Null) => false,
            Some(Value::Bool(flag)) => *flag,
            Some(other) => return Err(wrong_type("read_only", "true or false", other)),
        };
        let max_tool_calls_per_turn =
            positive(&map, "max_tool_calls_per_turn")?.unwrap_or(DEFAULT_MAX_TOOL_CALLS_PER_TURN);
        let max_tokens = positive(&map, "max_tokens")?.unwrap_or(DEFAULT_MAX_TOKENS);

        let system_prompt = system_prompt(&map, base)?;

        let config = AiConfig {
            provider,
            model,
            base_url,
            custom_base_url,
            api_key,
            key_source,
            read_only,
            max_tool_calls_per_turn,
            max_tokens,
            system_prompt,
        };
        if config.api_key.is_some() && config.base_url.starts_with("http://") && !config.is_local()
        {
            warnings.push(format!(
                "warning: base_url {} uses plain http to a host that is not this machine: the \
                 API key and the data you query travel unencrypted",
                config.base_url
            ));
        }
        Ok(LoadedConfig { config, warnings })
    }

    /// The URL of the provider endpoint the client posts to.
    pub fn endpoint(&self) -> String {
        format!("{}{}", self.base_url, self.provider.endpoint_path())
    }

    /// The host (with port, if any) of `base_url`, for the safety text.
    pub fn host(&self) -> &str {
        url_host(&self.base_url)
    }

    /// Whether `base_url` points at this machine (`localhost`, `127.*`, `[::1]`): then no data
    /// leaves it and the safety text is not shown.
    pub fn is_local(&self) -> bool {
        let host = self.host();
        let name = match host.strip_prefix('[') {
            Some(rest) => rest.split(']').next().unwrap_or(rest),
            None => host.split(':').next().unwrap_or(host),
        };
        name.eq_ignore_ascii_case("localhost")
            || name.starts_with("127.")
            || name == "::1"
            || name.to_ascii_lowercase().ends_with(".localhost")
    }

    /// The API key, if any. Never log or display it.
    pub(crate) fn api_key(&self) -> Option<&str> {
        self.api_key.as_deref()
    }
}

/// The example shown in parse errors.
const EXAMPLE: &str =
    r#"{"provider": "anthropic", "model": "<model id>", "api_key_env": "ANTHROPIC_API_KEY"}"#;

/// The host part of an `http(s)://host[:port]/path` URL (userinfo dropped).
fn url_host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    authority.rsplit('@').next().unwrap_or(authority)
}

/// Checks a `base_url` and strips trailing slashes.
fn check_base_url(url: &str) -> Result<String, String> {
    let trimmed = url.trim().trim_end_matches('/');
    let scheme_ok = trimmed.starts_with("https://") || trimmed.starts_with("http://");
    if !scheme_ok || url_host(trimmed).is_empty() || trimmed.contains(char::is_whitespace) {
        return Err(format!(
            "invalid base_url \"{url}\": expected an http:// or https:// URL, e.g. \
             \"http://localhost:11434/v1\" (Ollama) or \"https://openrouter.ai/api/v1\""
        ));
    }
    Ok(trimmed.to_string())
}

/// The custom system prompt text from `system_prompt` or `system_prompt_file`.
fn system_prompt(
    map: &Map<String, Value>,
    base: Option<&std::path::Path>,
) -> Result<Option<String>, String> {
    let inline = optional_string(map, "system_prompt")?;
    let file = optional_string(map, "system_prompt_file")?;
    let (key, text) = match (inline, file) {
        (Some(_), Some(_)) => {
            return Err(
                "both \"system_prompt\" and \"system_prompt_file\" are set: keep one".to_string(),
            );
        }
        (None, None) => return Ok(None),
        (Some(text), None) => ("system_prompt", text),
        (None, Some(name)) => {
            let path = match base {
                Some(dir) => dir.join(&name),
                None => std::path::PathBuf::from(&name),
            };
            let bytes = std::fs::read(&path).map_err(|err| {
                format!(
                    "cannot read \"system_prompt_file\" {}: {err}",
                    path.display()
                )
            })?;
            let text = String::from_utf8(bytes).map_err(|_| {
                format!(
                    "\"system_prompt_file\" {} is not UTF-8 text",
                    path.display()
                )
            })?;
            let text = text.trim().to_string();
            if text.is_empty() {
                return Err(format!(
                    "\"system_prompt_file\" {} is empty",
                    path.display()
                ));
            }
            ("system_prompt_file", text)
        }
    };
    let chars = text.chars().count();
    if chars > MAX_SYSTEM_PROMPT_CHARS {
        return Err(format!(
            "\"{key}\" is {chars} characters; the most is {MAX_SYSTEM_PROMPT_CHARS}"
        ));
    }
    Ok(Some(text))
}

fn required_string(map: &Map<String, Value>, key: &str) -> Result<String, String> {
    match optional_string(map, key)? {
        Some(text) => Ok(text),
        None => Err(format!(
            "missing \"{key}\" (a non-empty string), e.g. {EXAMPLE}"
        )),
    }
}

fn optional_string(map: &Map<String, Value>, key: &str) -> Result<Option<String>, String> {
    match map.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) if text.trim().is_empty() => Err(format!("\"{key}\" is empty")),
        Some(Value::String(text)) => Ok(Some(text.trim().to_string())),
        Some(other) => Err(wrong_type(key, "a string", other)),
    }
}

fn positive(map: &Map<String, Value>, key: &str) -> Result<Option<u32>, String> {
    match map.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(number)) => match number.as_u64().and_then(|n| u32::try_from(n).ok()) {
            Some(n) if n > 0 => Ok(Some(n)),
            _ => Err(format!(
                "\"{key}\" must be a whole number from 1 to {}, not {number}",
                u32::MAX
            )),
        },
        Some(other) => Err(wrong_type(key, "a whole number", other)),
    }
}

fn wrong_type(key: &str, expected: &str, got: &Value) -> String {
    let kind = match got {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    };
    format!("\"{key}\" must be {expected}, not {kind}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(name: &str) -> Option<String> {
        match name {
            "TEST_KEY" => Some("sk-test-123".to_string()),
            "EMPTY_KEY" => Some("  ".to_string()),
            _ => None,
        }
    }

    fn parse(text: &str) -> Result<LoadedConfig, String> {
        AiConfig::parse(text, env)
    }

    #[test]
    fn reads_a_full_file_and_the_key_from_the_environment() {
        let loaded = parse(
            r#"{"provider": "anthropic", "model": "m-1", "base_url": null,
                "api_key_env": "TEST_KEY", "read_only": true,
                "max_tool_calls_per_turn": 5, "max_tokens": 1000}"#,
        )
        .unwrap();
        assert!(loaded.warnings.is_empty());
        let config = loaded.config;
        assert_eq!(config.provider, Provider::Anthropic);
        assert_eq!(config.model, "m-1");
        assert_eq!(config.base_url, "https://api.anthropic.com");
        assert_eq!(config.endpoint(), "https://api.anthropic.com/v1/messages");
        assert_eq!(config.api_key(), Some("sk-test-123"));
        assert_eq!(config.key_source.as_deref(), Some("TEST_KEY"));
        assert!(config.read_only && !config.custom_base_url && !config.is_local());
        assert_eq!(
            (config.max_tool_calls_per_turn, config.max_tokens),
            (5, 1000)
        );
        assert_eq!(config.host(), "api.anthropic.com");
        // The key never shows in Debug.
        let debug = format!("{config:?}");
        assert!(
            !debug.contains("sk-test-123") && debug.contains("<redacted>"),
            "{debug}"
        );
    }

    #[test]
    fn defaults_and_local_servers_without_a_key() {
        let config = parse(
            r#"{"provider": "openai", "model": "llama3", "base_url": "http://localhost:11434/v1/"}"#,
        )
        .unwrap()
        .config;
        assert_eq!(
            config.endpoint(),
            "http://localhost:11434/v1/chat/completions"
        );
        assert_eq!(config.api_key(), None);
        assert!(config.is_local() && config.custom_base_url && !config.read_only);
        assert_eq!(
            config.max_tool_calls_per_turn,
            DEFAULT_MAX_TOOL_CALLS_PER_TURN
        );
        assert_eq!(config.max_tokens, DEFAULT_MAX_TOKENS);
        for url in [
            "http://127.0.0.1:8000/v1",
            "http://[::1]:1234/v1",
            "http://user@localhost",
        ] {
            let text = format!(r#"{{"provider": "openai", "model": "m", "base_url": "{url}"}}"#);
            assert!(parse(&text).unwrap().config.is_local(), "{url}");
        }
        let remote = parse(
            r#"{"provider": "openai", "model": "m", "base_url": "https://openrouter.ai/api/v1",
                "api_key_env": "TEST_KEY"}"#,
        )
        .unwrap()
        .config;
        assert!(!remote.is_local());
        assert_eq!(remote.host(), "openrouter.ai");
    }

    /// The example files of `examples/` (named in the book) stay valid and hold no key.
    #[test]
    fn the_example_files_are_valid() {
        let examples = [
            (
                include_str!("../../examples/ai-anthropic.json"),
                Provider::Anthropic,
                false,
            ),
            (
                include_str!("../../examples/ai-openai.json"),
                Provider::OpenAi,
                false,
            ),
            (
                include_str!("../../examples/ai-google.json"),
                Provider::OpenAi,
                false,
            ),
            (
                include_str!("../../examples/ai-ollama.json"),
                Provider::OpenAi,
                true,
            ),
        ];
        let env = |name: &str| {
            matches!(
                name,
                "ANTHROPIC_API_KEY" | "OPENAI_API_KEY" | "GEMINI_API_KEY"
            )
            .then(|| "k".to_string())
        };
        for (text, provider, local) in examples {
            assert!(!text.contains("\"api_key\""), "{text}");
            let loaded = AiConfig::parse(text, env).unwrap();
            assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
            assert_eq!(loaded.config.provider, provider);
            assert_eq!(loaded.config.is_local(), local);
            assert_eq!(loaded.config.api_key().is_none(), local);
        }
    }

    #[test]
    fn rejects_unknown_keys_and_wrong_values() {
        let cases = [
            (
                r#"{"provider": "openai", "model": "m", "readonly": true}"#,
                "did you mean \"read_only\"",
            ),
            (r#"[1]"#, "JSON object"),
            (
                r#"{"provider": "x", "model": "m"}"#,
                "unknown provider \"x\"",
            ),
            (r#"{"provider": "openai"}"#, "missing \"model\""),
            (
                r#"{"provider": "openai", "model": ""}"#,
                "\"model\" is empty",
            ),
            (
                r#"{"provider": "openai", "model": 3}"#,
                "\"model\" must be a string",
            ),
            (
                r#"{"provider": "openai", "model": "m", "base_url": "ftp://x"}"#,
                "invalid base_url",
            ),
            (
                r#"{"provider": "anthropic", "model": "m", "api_key_env": "MISSING"}"#,
                "MISSING (api_key_env) is not set",
            ),
            (
                r#"{"provider": "anthropic", "model": "m", "api_key_env": "EMPTY_KEY"}"#,
                "EMPTY_KEY",
            ),
            (r#"{"provider": "anthropic", "model": "m"}"#, "no API key"),
            (r#"{"provider": "openai", "model": "m"}"#, "no API key"),
            (
                r#"{"provider": "openai", "model": "m", "api_key": "k", "api_key_env": "TEST_KEY"}"#,
                "both",
            ),
            (
                r#"{"provider": "openai", "model": "m", "api_key": "k", "read_only": "yes"}"#,
                "\"read_only\" must be true or false",
            ),
            (
                r#"{"provider": "openai", "model": "m", "api_key": "k", "max_tokens": 0}"#,
                "\"max_tokens\" must be a whole number from 1",
            ),
            (
                r#"{"provider": "openai", "model": "m", "api_key": "k", "max_tool_calls_per_turn": 1.5}"#,
                "max_tool_calls_per_turn",
            ),
            ("{", "not valid JSON"),
        ];
        for (text, expected) in cases {
            let err = parse(text).unwrap_err();
            assert!(err.contains(expected), "{text}: {err}");
        }
    }

    #[test]
    fn warns_about_an_inline_key_and_plain_http_without_echoing_the_key() {
        let loaded = parse(
            r#"{"provider": "openai", "model": "m", "api_key": "sk-inline-999",
                "base_url": "http://gpu-box.lan:8000/v1"}"#,
        )
        .unwrap();
        assert_eq!(loaded.config.api_key(), Some("sk-inline-999"));
        assert_eq!(loaded.warnings.len(), 2, "{:?}", loaded.warnings);
        assert!(loaded.warnings[0].contains("api_key_env"));
        assert!(loaded.warnings[1].contains("unencrypted"));
        assert!(loaded.warnings.iter().all(|w| !w.contains("sk-inline-999")));
    }

    const MIN: &str = r#""provider": "openai", "model": "m", "api_key": "k""#;

    #[test]
    fn underscore_keys_are_silent_and_other_unknown_keys_warn() {
        let loaded = parse(&format!(r#"{{{MIN}, "_comment": "x", "temperature": 1}}"#)).unwrap();
        assert!(loaded.warnings.iter().all(|w| !w.contains("_comment")));
        let unknown: Vec<_> = loaded
            .warnings
            .iter()
            .filter(|w| w.contains("temperature"))
            .collect();
        assert_eq!(unknown.len(), 1, "{:?}", loaded.warnings);
        assert!(unknown[0].contains("unknown key \"temperature\" ignored"));
    }

    #[test]
    fn a_typo_of_a_known_key_is_an_error_with_a_hint() {
        for (typo, near) in [("readonly", "read_only"), ("max_token", "max_tokens")] {
            let err = parse(&format!(r#"{{{MIN}, "{typo}": 1}}"#)).unwrap_err();
            assert!(err.contains(&format!("did you mean \"{near}\"?")), "{err}");
        }
        assert_eq!(edit_distance("kitten", "sitting"), 3);
        assert_eq!(edit_distance("", "ab"), 2);
    }

    #[test]
    fn the_system_prompt_inline_or_from_a_file_next_to_the_config() {
        let inline = parse(&format!(r#"{{{MIN}, "system_prompt": "  Be brief.  "}}"#)).unwrap();
        assert_eq!(inline.config.system_prompt.as_deref(), Some("Be brief."));
        assert!(
            parse(&format!("{{{MIN}}}"))
                .unwrap()
                .config
                .system_prompt
                .is_none()
        );

        let dir = std::env::temp_dir().join(format!("graphersal-ai-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("p.md"), "From file.\n").unwrap();
        std::fs::write(dir.join("bin.md"), [0xff, 0xfe]).unwrap();
        let text = format!(r#"{{{MIN}, "system_prompt_file": "p.md"}}"#);
        let loaded = AiConfig::parse_in(&text, Some(&dir), env).unwrap();
        assert_eq!(loaded.config.system_prompt.as_deref(), Some("From file."));
        let abs = dir.join("p.md").display().to_string().replace('\\', "/");
        let text = format!(r#"{{{MIN}, "system_prompt_file": "{abs}"}}"#);
        assert!(AiConfig::parse_in(&text, None, env).is_ok());
        let text = format!(r#"{{{MIN}, "system_prompt_file": "bin.md"}}"#);
        let err = AiConfig::parse_in(&text, Some(&dir), env).unwrap_err();
        assert!(
            err.contains("system_prompt_file") && err.contains("UTF-8"),
            "{err}"
        );
        let text = format!(r#"{{{MIN}, "system_prompt_file": "nope.md"}}"#);
        let err = AiConfig::parse_in(&text, Some(&dir), env).unwrap_err();
        assert!(
            err.contains("system_prompt_file") && err.contains("cannot read"),
            "{err}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_system_prompt_errors() {
        let err = parse(&format!(
            r#"{{{MIN}, "system_prompt": "a", "system_prompt_file": "b"}}"#
        ))
        .unwrap_err();
        assert!(err.contains("both"), "{err}");
        let long = "x".repeat(MAX_SYSTEM_PROMPT_CHARS + 1);
        let err = parse(&format!(r#"{{{MIN}, "system_prompt": "{long}"}}"#)).unwrap_err();
        assert!(
            err.contains("\"system_prompt\" is 20001 characters"),
            "{err}"
        );
        let ok = "x".repeat(MAX_SYSTEM_PROMPT_CHARS);
        assert!(parse(&format!(r#"{{{MIN}, "system_prompt": "{ok}"}}"#)).is_ok());
    }
}
