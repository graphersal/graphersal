//! The provider-neutral conversation model and the one call [`Client::complete`].
//!
//! A [`Conversation`] is the system prompt (the static prefix: instructions, DSL reference),
//! the tool definitions (JSON schemas) and the messages so far; [`Client::complete`] sends it to
//! the configured provider in one non-streaming HTTPS request and returns the
//! [`AssistantTurn`]: its text, the tool calls it asks for and why it stopped. The caller runs
//! the tools, appends the turn and a [`Message::ToolResults`] and calls again.
//!
//! The HTTP client is `ureq` (synchronous, rustls): no async runtime, matching the synchronous
//! dev server. Every failure is an [`AiError`] with a help text; the API key never appears in
//! one (a provider message echoing it is scrubbed).

use std::time::Duration;

use serde_json::Value;

use super::config::{AiConfig, Provider};
use super::{anthropic, openai};

/// Connect timeout of a provider call.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Whole-call timeout of a provider call: a non-streaming answer of a large model can take
/// minutes.
const CALL_TIMEOUT: Duration = Duration::from_secs(300);
/// The largest response body read from a provider.
const MAX_RESPONSE_BYTES: u64 = 16 * 1024 * 1024;

/// A conversation sent to [`Client::complete`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Conversation {
    /// The system prompt. It is the static prefix of every call of a chat (Anthropic caches it).
    pub system: String,
    /// The tools the model may call.
    pub tools: Vec<ToolDefinition>,
    /// The messages, oldest first.
    pub messages: Vec<Message>,
    /// The model must answer without a tool call (`tool_choice` "none"): the last call of a turn
    /// that reached its tool-call limit. The tools stay defined, since the history holds calls
    /// of them (Anthropic refuses tool use blocks without the tool definitions).
    pub answer_only: bool,
}

/// A tool the model may call.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolDefinition {
    /// The tool name.
    pub name: String,
    /// What the tool does, for the model.
    pub description: String,
    /// The JSON Schema of the arguments (an `object` schema).
    pub input_schema: Value,
}

/// One message of a [`Conversation`].
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// A message of the person.
    User(String),
    /// An earlier answer of the model, as [`Client::complete`] returned it.
    Assistant(AssistantTurn),
    /// The results of the tool calls of the preceding assistant turn.
    ToolResults(Vec<ToolResult>),
}

/// One answer of the model.
#[derive(Debug, Clone, PartialEq)]
pub struct AssistantTurn {
    /// The text of the answer (the text blocks joined), possibly empty.
    pub text: String,
    /// The tool calls the model asks for, in its order.
    pub tool_calls: Vec<ToolCall>,
    /// Why the model stopped.
    pub stop: StopReason,
    /// Token usage of the call, as far as the provider reports it.
    pub usage: Usage,
    /// The provider's own content of the answer, replayed unchanged when the turn is sent back
    /// to the same provider (Anthropic: the content blocks, keeps thinking blocks valid; OpenAI:
    /// the answer's `message`, whose tool-call extras such as Gemini's thought signatures are
    /// copied back, see `openai::replay_extras`).
    pub(crate) raw: Option<(Provider, Value)>,
}

impl AssistantTurn {
    /// A turn built by hand (for tests): no provider content to keep.
    #[cfg(test)]
    pub fn new(text: impl Into<String>, tool_calls: Vec<ToolCall>, stop: StopReason) -> Self {
        Self {
            text: text.into(),
            tool_calls,
            stop,
            usage: Usage::default(),
            raw: None,
        }
    }
}

/// A tool call the model asks for.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    /// The provider's id of the call; the [`ToolResult`] names it.
    pub id: String,
    /// The tool name.
    pub name: String,
    /// The arguments. Normally an object; arguments that were not valid JSON stay a string
    /// (the tool then reports the error to the model).
    pub arguments: Value,
}

/// The result of one tool call.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolResult {
    /// The [`ToolCall::id`] it answers.
    pub call_id: String,
    /// The result text (JSON or a message). Data for the model, never instructions.
    pub content: String,
    /// Whether the tool failed.
    pub is_error: bool,
}

/// Why the model stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    /// The answer is complete.
    EndTurn,
    /// The model waits for the results of its tool calls.
    ToolUse,
    /// `max_tokens` cut the answer.
    MaxTokens,
    /// The provider declined to answer.
    Refusal,
    /// Another reason, as the provider named it.
    Other(String),
}

/// Token usage of one call.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    /// Input tokens (Anthropic: not counting the cached ones).
    pub input_tokens: u64,
    /// Output tokens.
    pub output_tokens: u64,
    /// Input tokens read from the prompt cache.
    pub cache_read_tokens: u64,
    /// Input tokens written to the prompt cache.
    pub cache_write_tokens: u64,
}

impl Usage {
    /// Every input token of the call, cached ones included: OpenAI's `prompt_tokens` already
    /// counts the cached ones, Anthropic's `input_tokens` does not.
    pub fn request_tokens(&self, provider: Provider) -> u64 {
        match provider {
            Provider::OpenAi => self.input_tokens,
            Provider::Anthropic => self
                .input_tokens
                .saturating_add(self.cache_read_tokens)
                .saturating_add(self.cache_write_tokens),
        }
    }
}

/// What went wrong in a provider call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiErrorKind {
    /// The base URL could not be reached (DNS, refused connection, TLS).
    Unreachable,
    /// The call took longer than the timeout.
    Timeout,
    /// The provider refused the API key (401) or its permissions (403).
    Authentication,
    /// Too many requests or tokens (429).
    RateLimited,
    /// The model does not exist or the key may not use it.
    ModelNotFound,
    /// The endpoint does not exist under the base URL (404 not about a model).
    EndpointNotFound,
    /// The provider is overloaded or failed (5xx, 529).
    ProviderUnavailable,
    /// The provider rejected the request (400, 413, 422, ...).
    BadRequest,
    /// The answer could not be read.
    InvalidResponse,
}

/// A failed provider call: what happened, the provider's message, and a help text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiError {
    /// The kind of failure.
    pub kind: AiErrorKind,
    /// The HTTP status, when the provider answered.
    pub status: Option<u16>,
    /// What happened, with the provider's own message when there is one.
    pub message: String,
    /// The provider name (`openai`/`anthropic`) and endpoint, for the help text.
    provider: Provider,
    endpoint: String,
}

impl AiError {
    pub(crate) fn new(
        config: &AiConfig,
        kind: AiErrorKind,
        status: Option<u16>,
        message: impl Into<String>,
    ) -> Self {
        let mut message = message.into();
        if let Some(key) = config.api_key().filter(|key| key.len() >= 4) {
            message = message.replace(key, "<redacted>");
        }
        Self {
            kind,
            status,
            message,
            provider: config.provider,
            endpoint: config.endpoint(),
        }
    }

    /// What to do about it.
    pub fn help(&self) -> String {
        let provider = self.provider.name();
        match self.kind {
            AiErrorKind::Unreachable => format!(
                "Cannot reach {}. Check \"base_url\" in the --ai file and the network; for a \
                 local server, check that it runs (Ollama: `ollama serve`, base_url \
                 \"http://localhost:11434/v1\").",
                self.endpoint
            ),
            AiErrorKind::Timeout => format!(
                "The {provider} call did not finish within {} seconds. Try again, ask for less \
                 at once, or lower \"max_tokens\" in the --ai file.",
                CALL_TIMEOUT.as_secs()
            ),
            AiErrorKind::Authentication => format!(
                "The {provider} provider refused the API key. Check the key in the environment \
                 variable named by \"api_key_env\" in the --ai file (restart graphersal after \
                 changing it) and that the key may use this model."
            ),
            AiErrorKind::RateLimited => format!(
                "The {provider} provider limits the requests or tokens of this key. Wait a \
                 moment and try again, or lower \"max_tool_calls_per_turn\"/\"max_tokens\" in \
                 the --ai file."
            ),
            AiErrorKind::ModelNotFound => format!(
                "The model named by \"model\" in the --ai file is not known to {} (or the key \
                 may not use it). Check its exact id in the provider's model list (Ollama: \
                 `ollama pull <model>` first).",
                self.endpoint
            ),
            AiErrorKind::EndpointNotFound => format!(
                "{} does not exist. Check \"base_url\" in the --ai file: for \"openai\" it is \
                 the URL before /chat/completions and usually ends in /v1 (e.g. \
                 \"http://localhost:11434/v1\"); for \"anthropic\" it is the URL before \
                 /v1/messages.",
                self.endpoint
            ),
            AiErrorKind::ProviderUnavailable => {
                format!("The {provider} provider is overloaded or failed. Try again in a moment.")
            }
            AiErrorKind::BadRequest => format!(
                "The {provider} provider rejected the request. Check \"model\" and \
                 \"max_tokens\" in the --ai file (the model's output limit) and the provider's \
                 message above."
            ),
            AiErrorKind::InvalidResponse => format!(
                "The answer of {} is not a {provider} answer. Check \"provider\" and \
                 \"base_url\" in the --ai file.",
                self.endpoint
            ),
        }
    }

    /// Maps an HTTP error answer to an error, with the provider's message from the body.
    pub(crate) fn from_status(config: &AiConfig, status: u16, body: &str) -> Self {
        let (message, code) = provider_message(body);
        let about_model = code.as_deref().is_some_and(|code| code.contains("model"))
            || message.to_ascii_lowercase().contains("model");
        let kind = match status {
            401 | 403 => AiErrorKind::Authentication,
            429 => AiErrorKind::RateLimited,
            404 if about_model => AiErrorKind::ModelNotFound,
            404 => AiErrorKind::EndpointNotFound,
            400 if code.as_deref() == Some("model_not_found") => AiErrorKind::ModelNotFound,
            500..=599 => AiErrorKind::ProviderUnavailable,
            _ => AiErrorKind::BadRequest,
        };
        let detail = if message.is_empty() {
            String::new()
        } else {
            format!(": {message}")
        };
        Self::new(
            config,
            kind,
            Some(status),
            format!(
                "The {} provider answered HTTP {status}{detail}",
                config.provider.name()
            ),
        )
    }
}

impl std::fmt::Display for AiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}\nHelp: {}", self.message, self.help())
    }
}

impl std::error::Error for AiError {}

/// The message (and the error code or type) of a provider error body: `{"error": {"message",
/// "code"|"type"}}` (OpenAI, Anthropic), `{"error": "text"}` (Ollama), else the body's start.
fn provider_message(body: &str) -> (String, Option<String>) {
    if let Ok(value) = serde_json::from_str::<Value>(body) {
        match value.get("error") {
            Some(Value::Object(error)) => {
                let message = error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let code = ["code", "type"]
                    .iter()
                    .find_map(|key| error.get(*key).and_then(Value::as_str))
                    .map(str::to_string);
                return (message, code);
            }
            Some(Value::String(message)) => return (message.clone(), None),
            _ => {}
        }
    }
    let trimmed = body.trim();
    let cut: String = trimmed.chars().take(300).collect();
    if cut.len() < trimmed.len() {
        (format!("{cut}…"), None)
    } else {
        (cut, None)
    }
}

/// An HTTP request a provider adapter builds.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ProviderRequest {
    /// The headers besides `Content-Type: application/json` (the key header included).
    pub headers: Vec<(&'static str, String)>,
    /// The JSON body.
    pub body: Value,
}

/// The client of one configured provider.
#[derive(Debug, Clone)]
pub struct Client {
    config: AiConfig,
    agent: ureq::Agent,
    timeout: Duration,
}

impl Client {
    /// A client for the configuration.
    pub fn new(config: AiConfig) -> Self {
        Self::with_timeout(config, CALL_TIMEOUT)
    }

    /// A client with another whole-call timeout.
    pub(crate) fn with_timeout(config: AiConfig, timeout: Duration) -> Self {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_connect(Some(CONNECT_TIMEOUT.min(timeout)))
            .timeout_global(Some(timeout))
            .user_agent(concat!("graphersal/", env!("CARGO_PKG_VERSION")))
            .build()
            .new_agent();
        Self {
            config,
            agent,
            timeout,
        }
    }

    /// The configuration.
    pub fn config(&self) -> &AiConfig {
        &self.config
    }

    /// Sends the conversation to the provider and returns its answer.
    pub fn complete(&self, conversation: &Conversation) -> Result<AssistantTurn, AiError> {
        let request = match self.config.provider {
            Provider::OpenAi => openai::request(&self.config, conversation),
            Provider::Anthropic => anthropic::request(&self.config, conversation),
        };
        let (status, body) = self.post(&request)?;
        if !(200..300).contains(&status) {
            return Err(AiError::from_status(&self.config, status, &body));
        }
        let value: Value = serde_json::from_str(&body).map_err(|err| {
            AiError::new(
                &self.config,
                AiErrorKind::InvalidResponse,
                Some(status),
                format!("The provider's answer is not JSON: {err}"),
            )
        })?;
        match self.config.provider {
            Provider::OpenAi => openai::response(&self.config, &value),
            Provider::Anthropic => anthropic::response(&self.config, value),
        }
    }

    /// Posts the request; returns the status and the body text.
    fn post(&self, request: &ProviderRequest) -> Result<(u16, String), AiError> {
        let url = self.config.endpoint();
        let mut builder = self
            .agent
            .post(&url)
            .header("content-type", "application/json");
        for (name, value) in &request.headers {
            builder = builder.header(*name, value.as_str());
        }
        let body = request.body.to_string();
        let mut response = builder
            .send(body.as_bytes())
            .map_err(|err| self.transport_error(err))?;
        let status = response.status().as_u16();
        let text = response
            .body_mut()
            .with_config()
            .limit(MAX_RESPONSE_BYTES)
            .read_to_string()
            .map_err(|err| self.transport_error(err))?;
        Ok((status, text))
    }

    fn transport_error(&self, err: ureq::Error) -> AiError {
        let kind = match &err {
            ureq::Error::Timeout(_) => AiErrorKind::Timeout,
            ureq::Error::Io(io) if io.kind() == std::io::ErrorKind::TimedOut => {
                AiErrorKind::Timeout
            }
            ureq::Error::BodyExceedsLimit(_) | ureq::Error::Protocol(_) => {
                AiErrorKind::InvalidResponse
            }
            _ => AiErrorKind::Unreachable,
        };
        let message = match kind {
            AiErrorKind::Timeout => format!(
                "The {} call timed out after {} seconds",
                self.config.provider.name(),
                self.timeout.as_secs_f64()
            ),
            _ => format!("The call to {} failed: {err}", self.config.endpoint()),
        };
        AiError::new(&self.config, kind, None, message)
    }
}
