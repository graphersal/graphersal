//! The `openai` adapter: Chat Completions (`POST {base_url}/chat/completions`) with `tools`
//! and `tool_calls`. Spoken by OpenAI and most other providers and local servers (Azure,
//! OpenRouter, Groq, Ollama, LM Studio, vLLM, ...), so it uses only the common subset: the
//! system prompt as the first `system` message, tool results as `tool` messages (a failed one
//! starts with `Error: `, the API has no error flag), the answer length as `max_tokens`
//! (`max_completion_tokens` for OpenAI's own hosts, whose reasoning models refuse
//! `max_tokens`).
//!
//! Provider extras of an earlier answer are sent back (see [`replay_extras`]): Google Gemini's
//! OpenAI-compatible endpoint puts a thought signature on a tool call
//! (`"extra_content": {"google": {"thought_signature": ".."}}`; with parallel calls only on the
//! first one) and possibly on a text answer, and refuses the next request when a signature of
//! the current turn's calls is missing.

use serde_json::{Map, Value, json};

use super::client::{
    AiError, AiErrorKind, AssistantTurn, Conversation, Message, ProviderRequest, StopReason,
    ToolCall, Usage,
};
use super::config::{AiConfig, Provider};

/// The tool-call fields the adapter rebuilds itself; every OTHER field of a provider's tool call
/// (e.g. Gemini's `extra_content`) is copied back unchanged onto the rebuilt call (deny rule:
/// tool calls are request input in the OpenAI format, so extras there are meant to round-trip).
const REBUILT_CALL_FIELDS: &[&str] = &["id", "type", "function"];

/// The message-level fields copied back from a provider's assistant message (allow rule: other
/// fields such as `reasoning_content` of DeepSeek-style servers or `refusal` must NOT be sent
/// back; some compatible servers reject unknown input fields).
const REPLAYED_MESSAGE_FIELDS: &[&str] = &["extra_content"];

/// Builds the request of a conversation.
pub(crate) fn request(config: &AiConfig, conversation: &Conversation) -> ProviderRequest {
    let mut messages = Vec::new();
    if !conversation.system.is_empty() {
        messages.push(json!({"role": "system", "content": conversation.system}));
    }
    for message in &conversation.messages {
        match message {
            Message::User(text) => messages.push(json!({"role": "user", "content": text})),
            Message::Assistant(turn) => {
                let mut entry = Map::new();
                entry.insert("role".into(), json!("assistant"));
                entry.insert(
                    "content".into(),
                    if turn.text.is_empty() {
                        Value::Null
                    } else {
                        json!(turn.text)
                    },
                );
                if !turn.tool_calls.is_empty() {
                    let calls = turn
                        .tool_calls
                        .iter()
                        .map(|call| {
                            let arguments = match &call.arguments {
                                Value::String(raw) => raw.clone(),
                                other => other.to_string(),
                            };
                            json!({"id": call.id, "type": "function",
                                   "function": {"name": call.name, "arguments": arguments}})
                        })
                        .collect();
                    entry.insert("tool_calls".into(), Value::Array(calls));
                }
                if let Some((Provider::OpenAi, Value::Object(raw))) = &turn.raw {
                    replay_extras(&mut entry, raw);
                }
                messages.push(Value::Object(entry));
            }
            Message::ToolResults(results) => {
                for result in results {
                    let content = if result.is_error {
                        format!("Error: {}", result.content)
                    } else {
                        result.content.clone()
                    };
                    messages.push(json!({"role": "tool", "tool_call_id": result.call_id,
                                         "content": content}));
                }
            }
        }
    }
    let mut body = Map::new();
    body.insert("model".into(), json!(config.model));
    body.insert("messages".into(), Value::Array(messages));
    if !conversation.tools.is_empty() {
        let tools = conversation
            .tools
            .iter()
            .map(|tool| {
                json!({"type": "function", "function": {"name": tool.name,
                       "description": tool.description, "parameters": tool.input_schema}})
            })
            .collect();
        body.insert("tools".into(), Value::Array(tools));
        if conversation.answer_only {
            body.insert("tool_choice".into(), json!("none"));
        }
    }
    let length_key = if openai_host(config) {
        "max_completion_tokens"
    } else {
        "max_tokens"
    };
    body.insert(length_key.into(), json!(config.max_tokens));
    let headers = config
        .api_key()
        .map(|key| vec![("authorization", format!("Bearer {key}"))])
        .unwrap_or_default();
    ProviderRequest {
        headers,
        body: Value::Object(body),
    }
}

/// Copies the provider extras of the raw assistant message `raw` onto the rebuilt `entry`: the
/// message fields of [`REPLAYED_MESSAGE_FIELDS`] (allow rule) and, for each rebuilt tool call,
/// every field of the raw call with the same id that is not in [`REBUILT_CALL_FIELDS`] (deny
/// rule; a raw call without an id, whose id we generated, is matched by position). A call kept
/// in a stopped turn keeps its extras too: the turn's calls all stay in the history.
fn replay_extras(entry: &mut Map<String, Value>, raw: &Map<String, Value>) {
    for key in REPLAYED_MESSAGE_FIELDS {
        if let Some(value) = raw.get(*key).filter(|value| !value.is_null()) {
            entry.insert((*key).to_string(), value.clone());
        }
    }
    let raw_calls = raw
        .get("tool_calls")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let Some(Value::Array(calls)) = entry.get_mut("tool_calls") else {
        return;
    };
    for (index, call) in calls.iter_mut().enumerate() {
        let id = call.get("id").and_then(Value::as_str);
        let source = raw_calls
            .iter()
            .find(|raw_call| raw_call.get("id").and_then(Value::as_str) == id)
            .or_else(|| {
                raw_calls
                    .get(index)
                    .filter(|raw_call| raw_call.get("id").is_none())
            });
        let (Some(Value::Object(source)), Value::Object(call)) = (source, call) else {
            continue;
        };
        for (key, value) in source {
            if !REBUILT_CALL_FIELDS.contains(&key.as_str()) {
                call.insert(key.clone(), value.clone());
            }
        }
    }
}

/// Whether the base URL is OpenAI's own (api.openai.com, Azure OpenAI).
fn openai_host(config: &AiConfig) -> bool {
    let host = config.host().to_ascii_lowercase();
    let name = host.split(':').next().unwrap_or(&host);
    name == "api.openai.com" || name.ends_with(".openai.azure.com")
}

/// Reads the answer of a successful call.
pub(crate) fn response(config: &AiConfig, value: &Value) -> Result<AssistantTurn, AiError> {
    let invalid = |what: &str| {
        AiError::new(
            config,
            AiErrorKind::InvalidResponse,
            Some(200),
            format!("The provider's answer has no {what}"),
        )
    };
    // Some gateways answer an error with HTTP 200.
    if let Some(error) = value.get("error").filter(|error| !error.is_null()) {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .map_or_else(|| error.to_string(), str::to_string);
        return Err(AiError::new(
            config,
            AiErrorKind::BadRequest,
            Some(200),
            format!("The openai provider answered with an error: {message}"),
        ));
    }
    let choice = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .ok_or_else(|| invalid("\"choices\""))?;
    let message = choice
        .get("message")
        .ok_or_else(|| invalid("\"message\""))?;
    let text = message
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let mut tool_calls = Vec::new();
    for call in message
        .get("tool_calls")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        let function = call
            .get("function")
            .ok_or_else(|| invalid("tool \"function\""))?;
        let name = function
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("tool name"))?;
        let arguments = match function.get("arguments") {
            Some(Value::String(raw)) if raw.trim().is_empty() => json!({}),
            Some(Value::String(raw)) => {
                serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.clone()))
            }
            // Some local servers send the arguments as an object.
            Some(other @ Value::Object(_)) => other.clone(),
            _ => json!({}),
        };
        let id = call
            .get("id")
            .and_then(Value::as_str)
            .map_or_else(|| format!("call_{}", tool_calls.len()), str::to_string);
        tool_calls.push(ToolCall {
            id,
            name: name.to_string(),
            arguments,
        });
    }
    let stop = match choice.get("finish_reason").and_then(Value::as_str) {
        // Some local servers say "stop" although they ask for tools.
        _ if !tool_calls.is_empty() => StopReason::ToolUse,
        Some("stop") | None => StopReason::EndTurn,
        Some("tool_calls") => StopReason::ToolUse,
        Some("length") => StopReason::MaxTokens,
        Some("content_filter") => StopReason::Refusal,
        Some(other) => StopReason::Other(other.to_string()),
    };
    // Chat Completions names the counts `prompt_tokens`/`completion_tokens`; some compatible
    // servers and gateways use the Responses API's `input_tokens`/`output_tokens` instead.
    let count = |paths: &[&[&str]]| {
        paths
            .iter()
            .find_map(|path| {
                let mut node = value.get("usage");
                for key in *path {
                    node = node.and_then(|n| n.get(*key));
                }
                node.and_then(Value::as_u64)
            })
            .unwrap_or(0)
    };
    Ok(AssistantTurn {
        text,
        tool_calls,
        stop,
        usage: Usage {
            input_tokens: count(&[&["prompt_tokens"], &["input_tokens"]]),
            output_tokens: count(&[&["completion_tokens"], &["output_tokens"]]),
            cache_read_tokens: count(&[
                &["prompt_tokens_details", "cached_tokens"],
                &["input_tokens_details", "cached_tokens"],
            ]),
            cache_write_tokens: 0,
        },
        raw: Some((Provider::OpenAi, message.clone())),
    })
}
