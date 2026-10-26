//! The `anthropic` adapter: the Messages API (`POST {base_url}/v1/messages`) with tool use.
//!
//! The system prompt is one text block with `cache_control` (ephemeral): tools and system are
//! the static prefix of every call of a chat, so later calls read them from the prompt cache.
//! An earlier assistant turn is replayed with the provider's own content blocks (thinking blocks
//! included, which must come back unchanged); consecutive user-side messages (tool results,
//! then the person's next text) are merged into one `user` message, tool results first.

use serde_json::{Map, Value, json};

use super::client::{
    AiError, AiErrorKind, AssistantTurn, Conversation, Message, ProviderRequest, StopReason,
    ToolCall, Usage,
};
use super::config::{AiConfig, Provider};

/// The `anthropic-version` header.
pub(crate) const API_VERSION: &str = "2023-06-01";

/// Builds the request of a conversation.
pub(crate) fn request(config: &AiConfig, conversation: &Conversation) -> ProviderRequest {
    let mut messages: Vec<Value> = Vec::new();
    for message in &conversation.messages {
        let (role, blocks) = match message {
            Message::User(text) => ("user", vec![json!({"type": "text", "text": text})]),
            Message::Assistant(turn) => ("assistant", assistant_blocks(turn)),
            Message::ToolResults(results) => (
                "user",
                results
                    .iter()
                    .map(|result| {
                        let mut block = json!({"type": "tool_result",
                            "tool_use_id": result.call_id, "content": result.content});
                        if result.is_error {
                            block["is_error"] = json!(true);
                        }
                        block
                    })
                    .collect(),
            ),
        };
        match messages.last_mut() {
            Some(last) if last["role"] == role => {
                if let Some(content) = last["content"].as_array_mut() {
                    content.extend(blocks);
                }
            }
            _ => messages.push(json!({"role": role, "content": blocks})),
        }
    }
    let mut body = Map::new();
    body.insert("model".into(), json!(config.model));
    body.insert("max_tokens".into(), json!(config.max_tokens));
    if !conversation.system.is_empty() {
        body.insert(
            "system".into(),
            json!([{"type": "text", "text": conversation.system,
                    "cache_control": {"type": "ephemeral"}}]),
        );
    }
    if !conversation.tools.is_empty() {
        let tools = conversation
            .tools
            .iter()
            .map(|tool| {
                json!({"name": tool.name, "description": tool.description,
                       "input_schema": tool.input_schema})
            })
            .collect();
        body.insert("tools".into(), Value::Array(tools));
        if conversation.answer_only {
            body.insert("tool_choice".into(), json!({"type": "none"}));
        }
    }
    body.insert("messages".into(), Value::Array(messages));
    let mut headers = vec![("anthropic-version", API_VERSION.to_string())];
    if let Some(key) = config.api_key() {
        headers.push(("x-api-key", key.to_string()));
    }
    ProviderRequest {
        headers,
        body: Value::Object(body),
    }
}

/// The content blocks of an earlier assistant turn.
fn assistant_blocks(turn: &AssistantTurn) -> Vec<Value> {
    if let Some((Provider::Anthropic, Value::Array(blocks))) = &turn.raw {
        return blocks.clone();
    }
    let mut blocks = Vec::new();
    if !turn.text.is_empty() {
        blocks.push(json!({"type": "text", "text": turn.text}));
    }
    for call in &turn.tool_calls {
        let input = match &call.arguments {
            Value::Object(_) => call.arguments.clone(),
            _ => json!({}),
        };
        blocks.push(json!({"type": "tool_use", "id": call.id, "name": call.name,
                           "input": input}));
    }
    if blocks.is_empty() {
        // The API refuses an empty assistant message.
        blocks.push(json!({"type": "text", "text": "(no answer)"}));
    }
    blocks
}

/// Reads the answer of a successful call.
pub(crate) fn response(config: &AiConfig, value: Value) -> Result<AssistantTurn, AiError> {
    let invalid = |what: &str| {
        AiError::new(
            config,
            AiErrorKind::InvalidResponse,
            Some(200),
            format!("The provider's answer has no {what}"),
        )
    };
    let Some(content) = value.get("content").and_then(Value::as_array) else {
        return Err(invalid("\"content\""));
    };
    let mut texts = Vec::new();
    let mut tool_calls = Vec::new();
    for block in content {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    texts.push(text);
                }
            }
            Some("tool_use") => {
                let field = |key: &str| block.get(key).and_then(Value::as_str);
                let (Some(id), Some(name)) = (field("id"), field("name")) else {
                    return Err(invalid("tool_use id or name"));
                };
                tool_calls.push(ToolCall {
                    id: id.to_string(),
                    name: name.to_string(),
                    arguments: block.get("input").cloned().unwrap_or_else(|| json!({})),
                });
            }
            // Thinking and other blocks are kept in `raw` only.
            _ => {}
        }
    }
    let stop = match value.get("stop_reason").and_then(Value::as_str) {
        Some("end_turn" | "stop_sequence") | None => StopReason::EndTurn,
        Some("tool_use") => StopReason::ToolUse,
        Some("max_tokens") => StopReason::MaxTokens,
        Some("refusal") => StopReason::Refusal,
        Some(other) => StopReason::Other(other.to_string()),
    };
    let count = |key: &str| {
        value
            .get("usage")
            .and_then(|usage| usage.get(key))
            .and_then(Value::as_u64)
            .unwrap_or(0)
    };
    let usage = Usage {
        input_tokens: count("input_tokens"),
        output_tokens: count("output_tokens"),
        cache_read_tokens: count("cache_read_input_tokens"),
        cache_write_tokens: count("cache_creation_input_tokens"),
    };
    let text = texts.join("\n\n");
    let raw = Some((Provider::Anthropic, Value::Array(content.clone())));
    Ok(AssistantTurn {
        text,
        tool_calls,
        stop,
        usage,
        raw,
    })
}
