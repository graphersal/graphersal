//! The stored history of one AI Chat conversation, compressed per finished turn.
//!
//! A conversation keeps two lists: the [`Message`]s sent to the provider and the events shown
//! to the page (JSON values). The running turn appends to a plain tail; when the turn ends,
//! [`History::seal`] packs the tail into one block: the messages and events as JSON text,
//! compressed with LZ4 (tool results are JSON and compress well, and a whole turn in one block
//! shares its repetitions). A provider call decodes the blocks again ([`History::messages`]);
//! LZ4 decodes at memory speed, far below the cost of the call itself.

use serde_json::{Map, Value, json};

use super::client::{AssistantTurn, Message, StopReason, ToolCall, ToolResult, Usage};
use super::config::Provider;

/// The history: sealed blocks (finished turns), oldest first, then the plain tail.
#[derive(Default)]
pub struct History {
    blocks: Vec<Block>,
    /// Events in the blocks: the first tail event has the index `sealed_events`.
    sealed_events: usize,
    messages: Vec<Message>,
    events: Vec<Value>,
}

/// One sealed turn.
struct Block {
    /// The LZ4 frame (size prepended) of `{"m": [messages], "e": [events]}`.
    data: Vec<u8>,
    /// The length of the JSON text before compression.
    raw_len: usize,
}

impl History {
    /// Appends a message to the tail.
    pub fn push_message(&mut self, message: Message) {
        self.messages.push(message);
    }

    /// Appends an event to the tail.
    pub fn push_event(&mut self, event: Value) {
        self.events.push(event);
    }

    /// The messages of the running turn (the tail), oldest first.
    pub fn tail_messages(&self) -> &[Message] {
        &self.messages
    }

    /// The number of events.
    pub fn event_count(&self) -> usize {
        self.sealed_events + self.events.len()
    }

    /// Every message, oldest first.
    pub fn messages(&self) -> Result<Vec<Message>, String> {
        let mut out = Vec::new();
        for block in &self.blocks {
            let (messages, _) = block.decode()?;
            for message in messages {
                out.push(decode_message(&message)?);
            }
        }
        out.extend(self.messages.iter().cloned());
        Ok(out)
    }

    /// The events after the first `after`, oldest first.
    pub fn events_after(&self, after: usize) -> Result<Vec<Value>, String> {
        let mut out = Vec::new();
        if after < self.sealed_events {
            let mut seen = 0;
            for block in &self.blocks {
                let (_, events) = block.decode()?;
                let count = events.len();
                if seen + count > after {
                    out.extend(events.into_iter().skip(after.saturating_sub(seen)));
                }
                seen += count;
            }
        }
        let skip = after.saturating_sub(self.sealed_events);
        out.extend(self.events.iter().skip(skip).cloned());
        Ok(out)
    }

    /// Packs the tail into a block (nothing when it is empty).
    pub fn seal(&mut self) {
        if self.messages.is_empty() && self.events.is_empty() {
            return;
        }
        let messages: Vec<Value> = self.messages.iter().map(encode_message).collect();
        let text = json!({"m": messages, "e": self.events}).to_string();
        self.blocks.push(Block {
            data: lz4_flex::compress_prepend_size(text.as_bytes()),
            raw_len: text.len(),
        });
        self.sealed_events += self.events.len();
        self.messages.clear();
        self.events.clear();
    }

    /// `(stored, uncompressed)`: the bytes the history holds (the blocks compressed, the tail
    /// as JSON text) and the bytes it would take as plain JSON text.
    pub fn bytes(&self) -> (usize, usize) {
        let tail: usize = self
            .messages
            .iter()
            .map(|m| encode_message(m).to_string().len())
            .chain(self.events.iter().map(|e| e.to_string().len()))
            .sum();
        let (stored, raw) = self
            .blocks
            .iter()
            .fold((0, 0), |(s, r), b| (s + b.data.len(), r + b.raw_len));
        (stored + tail, raw + tail)
    }
}

impl Block {
    fn decode(&self) -> Result<(Vec<Value>, Vec<Value>), String> {
        let bytes = lz4_flex::decompress_size_prepended(&self.data)
            .map_err(|err| format!("a block of the chat history cannot be read: {err}"))?;
        let mut value: Value = serde_json::from_slice(&bytes)
            .map_err(|err| format!("a block of the chat history is not JSON: {err}"))?;
        let mut list = |key: &str| match value.get_mut(key).map(Value::take) {
            Some(Value::Array(items)) => Ok(items),
            _ => Err(format!("a block of the chat history has no {key:?} list")),
        };
        Ok((list("m")?, list("e")?))
    }
}

fn encode_message(message: &Message) -> Value {
    match message {
        Message::User(text) => json!({ "user": text }),
        Message::Assistant(turn) => {
            let calls: Vec<Value> = turn
                .tool_calls
                .iter()
                .map(|c| json!({"id": c.id, "name": c.name, "args": c.arguments}))
                .collect();
            let stop = match &turn.stop {
                StopReason::EndTurn => json!("end_turn"),
                StopReason::ToolUse => json!("tool_use"),
                StopReason::MaxTokens => json!("max_tokens"),
                StopReason::Refusal => json!("refusal"),
                StopReason::Other(other) => json!({ "other": other }),
            };
            let u = &turn.usage;
            let raw = match &turn.raw {
                Some((provider, raw)) => json!([provider.name(), raw]),
                None => Value::Null,
            };
            json!({"assistant": {
                "text": turn.text,
                "calls": calls,
                "stop": stop,
                "usage": [u.input_tokens, u.output_tokens, u.cache_read_tokens,
                          u.cache_write_tokens],
                "raw": raw,
            }})
        }
        Message::ToolResults(results) => {
            let results: Vec<Value> = results
                .iter()
                .map(|r| json!({"id": r.call_id, "content": r.content, "error": r.is_error}))
                .collect();
            json!({ "results": results })
        }
    }
}

fn decode_message(value: &Value) -> Result<Message, String> {
    let bad = || format!("a message of the chat history cannot be read: {value}");
    let text = |v: &Value, key: &str| v.get(key).and_then(Value::as_str).map(str::to_string);
    let object = value.as_object().ok_or_else(bad)?;
    if let Some(user) = object.get("user").and_then(Value::as_str) {
        return Ok(Message::User(user.to_string()));
    }
    if let Some(results) = object.get("results").and_then(Value::as_array) {
        return results
            .iter()
            .map(|r| {
                Ok(ToolResult {
                    call_id: text(r, "id").ok_or_else(bad)?,
                    content: text(r, "content").ok_or_else(bad)?,
                    is_error: r.get("error").and_then(Value::as_bool).ok_or_else(bad)?,
                })
            })
            .collect::<Result<_, String>>()
            .map(Message::ToolResults);
    }
    let turn: &Map<String, Value> = object
        .get("assistant")
        .and_then(Value::as_object)
        .ok_or_else(bad)?;
    let tool_calls = turn
        .get("calls")
        .and_then(Value::as_array)
        .ok_or_else(bad)?
        .iter()
        .map(|c| {
            Ok(ToolCall {
                id: text(c, "id").ok_or_else(bad)?,
                name: text(c, "name").ok_or_else(bad)?,
                arguments: c.get("args").cloned().unwrap_or(Value::Null),
            })
        })
        .collect::<Result<_, String>>()?;
    let stop = match turn.get("stop") {
        Some(Value::String(s)) => match s.as_str() {
            "end_turn" => StopReason::EndTurn,
            "tool_use" => StopReason::ToolUse,
            "max_tokens" => StopReason::MaxTokens,
            "refusal" => StopReason::Refusal,
            _ => return Err(bad()),
        },
        Some(other) => StopReason::Other(text(other, "other").ok_or_else(bad)?),
        None => return Err(bad()),
    };
    let count = |i: usize| {
        turn.get("usage")
            .and_then(|usage| usage.get(i))
            .and_then(Value::as_u64)
            .unwrap_or(0)
    };
    let usage = Usage {
        input_tokens: count(0),
        output_tokens: count(1),
        cache_read_tokens: count(2),
        cache_write_tokens: count(3),
    };
    let raw = match turn.get("raw") {
        None | Some(Value::Null) => None,
        Some(pair) => {
            let provider = match pair.get(0).and_then(Value::as_str) {
                Some("openai") => Provider::OpenAi,
                Some("anthropic") => Provider::Anthropic,
                _ => return Err(bad()),
            };
            Some((provider, pair.get(1).cloned().unwrap_or(Value::Null)))
        }
    };
    Ok(Message::Assistant(AssistantTurn {
        text: turn
            .get("text")
            .and_then(Value::as_str)
            .ok_or_else(bad)?
            .to_string(),
        tool_calls,
        stop,
        usage,
        raw,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<Message> {
        let mut turn = AssistantTurn::new(
            "looking",
            vec![ToolCall {
                id: "c1".into(),
                name: "query".into(),
                arguments: json!({"script": "g.v().count()"}),
            }],
            StopReason::ToolUse,
        );
        turn.usage = Usage {
            input_tokens: 100,
            output_tokens: 20,
            cache_read_tokens: 80,
            cache_write_tokens: 5,
        };
        turn.raw = Some((
            Provider::Anthropic,
            json!([{"type": "thinking", "thinking": "x"}]),
        ));
        let mut other = AssistantTurn::new("", vec![], StopReason::Other("pause".into()));
        other.raw = Some((Provider::OpenAi, json!({"role": "assistant"})));
        vec![
            Message::User("how many?".into()),
            Message::Assistant(turn),
            Message::ToolResults(vec![ToolResult {
                call_id: "c1".into(),
                content: "ok: 1 result\n[6]".into(),
                is_error: false,
            }]),
            Message::Assistant(other),
            Message::Assistant(AssistantTurn::new("six", vec![], StopReason::MaxTokens)),
            Message::ToolResults(vec![ToolResult {
                call_id: "c2".into(),
                content: "denied".into(),
                is_error: true,
            }]),
        ]
    }

    #[test]
    fn sealed_turns_read_back_unchanged() {
        let mut history = History::default();
        let messages = sample();
        for (n, message) in messages.iter().enumerate() {
            history.push_message(message.clone());
            history.push_event(json!({"seq": n + 1}));
            if n == 2 {
                history.seal();
            }
        }
        assert_eq!(history.tail_messages().len(), 3);
        assert_eq!(history.messages().unwrap(), messages);
        history.seal();
        history.seal();
        assert!(history.tail_messages().is_empty());
        assert_eq!(history.messages().unwrap(), messages);
        assert_eq!(history.event_count(), 6);
        for after in 0..=7 {
            let seqs: Vec<u64> = history
                .events_after(after)
                .unwrap()
                .iter()
                .map(|e| e["seq"].as_u64().unwrap())
                .collect();
            let expected: Vec<u64> = (after as u64 + 1..=6).collect();
            assert_eq!(seqs, expected, "after {after}");
        }
    }

    #[test]
    fn a_sealed_turn_takes_less_than_its_text() {
        let mut history = History::default();
        let rows: Vec<Value> = (0..200)
            .map(|i| json!({"name": format!("person {i}"), "label": "person", "age": i}))
            .collect();
        history.push_message(Message::User("list everybody".into()));
        history.push_message(Message::ToolResults(vec![ToolResult {
            call_id: "c1".into(),
            content: Value::Array(rows).to_string(),
            is_error: false,
        }]));
        let (before, raw_before) = history.bytes();
        assert_eq!(before, raw_before, "the tail is stored as it is");
        history.seal();
        let (stored, raw) = history.bytes();
        assert!(raw >= raw_before, "{raw} of {raw_before}");
        assert!(stored * 3 < raw, "{stored} of {raw}");
    }
}
