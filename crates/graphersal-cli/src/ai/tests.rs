//! The provider adapters against a local mock HTTP server (no network): recorded
//! request/response fixtures (`fixtures/<provider>.json`) for a text answer, a tool call, a
//! tool result round and an error body; the exact JSON and headers we send are asserted.

use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::{Value, json};

use super::client::{
    AiErrorKind, AssistantTurn, Client, Conversation, Message, StopReason, ToolCall,
    ToolDefinition, ToolResult, Usage,
};
use super::config::AiConfig;

const OPENAI_FIXTURES: &str = include_str!("fixtures/openai.json");
const ANTHROPIC_FIXTURES: &str = include_str!("fixtures/anthropic.json");
const TEST_KEY: &str = "sk-test-key-1234";

/// One request the mock received.
#[derive(Debug)]
struct Recorded {
    method: String,
    url: String,
    /// Lower-case header names with their values.
    headers: Vec<(String, String)>,
    body: Value,
}

impl Recorded {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// A mock provider on 127.0.0.1:<free port> answering the given (status, body) pairs in order,
/// after `delay`.
struct Mock {
    base_url: String,
    requests: mpsc::Receiver<Recorded>,
    thread: Option<JoinHandle<()>>,
}

impl Mock {
    fn start(answers: Vec<(u16, String)>, delay: Duration) -> Self {
        let server = tiny_http::Server::http("127.0.0.1:0").expect("mock server");
        let port = server.server_addr().to_ip().expect("ip address").port();
        let (sender, requests) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            for (status, body) in answers {
                let Ok(mut request) = server.recv() else {
                    return;
                };
                let mut text = String::new();
                let _ = request.as_reader().read_to_string(&mut text);
                let recorded = Recorded {
                    method: request.method().to_string(),
                    url: request.url().to_string(),
                    headers: request
                        .headers()
                        .iter()
                        .map(|h| {
                            (
                                h.field.as_str().as_str().to_ascii_lowercase(),
                                h.value.as_str().to_string(),
                            )
                        })
                        .collect(),
                    body: serde_json::from_str(&text).unwrap_or(Value::String(text)),
                };
                let _ = sender.send(recorded);
                std::thread::sleep(delay);
                let header =
                    tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
                        .expect("header");
                let _ = request.respond(
                    tiny_http::Response::from_string(body)
                        .with_status_code(status)
                        .with_header(header),
                );
            }
        });
        Self {
            base_url: format!("http://127.0.0.1:{port}"),
            requests,
            thread: Some(thread),
        }
    }

    fn next_request(&self) -> Recorded {
        self.requests
            .recv_timeout(Duration::from_secs(10))
            .expect("the mock received a request")
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn fixtures(text: &str) -> Value {
    serde_json::from_str(text).expect("fixture JSON")
}

fn answer(fixture: &Value, case: &str) -> (u16, String) {
    let response = &fixture[case]["response"];
    let status = response["status"].as_u64().expect("status") as u16;
    (status, response["body"].to_string())
}

fn config(provider: &str, model: &str, base_url: &str) -> AiConfig {
    let text = format!(
        r#"{{"provider": "{provider}", "model": "{model}", "base_url": "{base_url}",
            "api_key_env": "AI_TEST_KEY", "max_tokens": 512}}"#
    );
    AiConfig::parse(&text, |name| {
        (name == "AI_TEST_KEY").then(|| TEST_KEY.to_string())
    })
    .expect("config")
    .config
}

fn query_tool() -> ToolDefinition {
    ToolDefinition {
        name: "query".to_string(),
        description: "Runs a Graphersal script and returns its result.".to_string(),
        input_schema: json!({"type": "object", "properties": {"script": {"type": "string"}},
                             "required": ["script"]}),
    }
}

const SYSTEM: &str = "You answer questions about a graph.";
const QUESTION: &str = "How many people are there?";

fn text_conversation() -> Conversation {
    Conversation {
        system: SYSTEM.to_string(),
        tools: vec![],
        messages: vec![Message::User("Hello?".to_string())],
        answer_only: false,
    }
}

fn tool_conversation() -> Conversation {
    Conversation {
        system: SYSTEM.to_string(),
        tools: vec![query_tool()],
        messages: vec![Message::User(QUESTION.to_string())],
        answer_only: false,
    }
}

/// Runs the three recorded rounds (text, tool call, tool result) against the mock and checks
/// each request body against the fixture; returns the requests and the three answers.
fn three_rounds(
    provider: &str,
    model: &str,
    fixture: &Value,
) -> (Vec<Recorded>, Vec<AssistantTurn>) {
    let mock = Mock::start(
        vec![
            answer(fixture, "text"),
            answer(fixture, "tool_call"),
            answer(fixture, "tool_result"),
        ],
        Duration::ZERO,
    );
    let client = Client::new(config(provider, model, &mock.base_url));
    let mut requests = Vec::new();
    let mut turns = Vec::new();

    let text = client.complete(&text_conversation()).expect("text answer");
    requests.push(mock.next_request());
    turns.push(text);

    let mut conversation = tool_conversation();
    let call = client.complete(&conversation).expect("tool call");
    requests.push(mock.next_request());
    turns.push(call.clone());

    conversation.messages.push(Message::Assistant(call.clone()));
    conversation
        .messages
        .push(Message::ToolResults(vec![ToolResult {
            call_id: call.tool_calls[0].id.clone(),
            content: "[4]".to_string(),
            is_error: false,
        }]));
    let last = client.complete(&conversation).expect("final answer");
    requests.push(mock.next_request());
    turns.push(last);

    for (request, case) in requests.iter().zip(["text", "tool_call", "tool_result"]) {
        assert_eq!(request.method, "POST");
        assert_eq!(
            request.body, fixture[case]["request"],
            "{provider} {case}: the request body differs from the fixture"
        );
        assert_eq!(request.header("content-type"), Some("application/json"));
    }
    (requests, turns)
}

#[test]
fn openai_text_tool_call_and_tool_result_rounds() {
    let fixture = fixtures(OPENAI_FIXTURES);
    let (requests, turns) = three_rounds("openai", "gpt-test", &fixture);
    for request in &requests {
        assert_eq!(request.url, "/chat/completions");
        assert_eq!(
            request.header("authorization"),
            Some(format!("Bearer {TEST_KEY}").as_str())
        );
        assert_eq!(request.header("x-api-key"), None);
    }
    assert_eq!(turns[0].text, "Hello! Ask me about the graph.");
    assert_eq!(turns[0].stop, StopReason::EndTurn);
    assert!(turns[0].tool_calls.is_empty());
    assert_eq!(
        turns[0].usage,
        Usage {
            input_tokens: 21,
            output_tokens: 8,
            cache_read_tokens: 16,
            cache_write_tokens: 0
        }
    );
    assert_eq!(turns[1].stop, StopReason::ToolUse);
    assert_eq!(turns[1].text, "");
    assert_eq!(
        turns[1].tool_calls,
        vec![ToolCall {
            id: "call_abc".to_string(),
            name: "query".to_string(),
            arguments: json!({"script": "g.v().has_label(\"person\").count()"}),
        }]
    );
    assert_eq!(turns[2].text, "There are 4 people.");
    assert_eq!(turns[2].stop, StopReason::EndTurn);
}

#[test]
fn anthropic_text_tool_call_and_tool_result_rounds() {
    let fixture = fixtures(ANTHROPIC_FIXTURES);
    let (requests, turns) = three_rounds("anthropic", "claude-test", &fixture);
    for request in &requests {
        assert_eq!(request.url, "/v1/messages");
        assert_eq!(request.header("anthropic-version"), Some("2023-06-01"));
        assert_eq!(request.header("x-api-key"), Some(TEST_KEY));
        assert_eq!(request.header("authorization"), None);
    }
    assert_eq!(turns[0].text, "Hello! Ask me about the graph.");
    assert_eq!(turns[0].usage.cache_read_tokens, 1200);
    assert_eq!(turns[1].stop, StopReason::ToolUse);
    assert_eq!(turns[1].text, "Let me count them.");
    assert_eq!(turns[1].tool_calls[0].id, "toolu_1");
    assert_eq!(
        turns[1].tool_calls[0].arguments,
        json!({"script": "g.v().has_label(\"person\").count()"})
    );
    assert_eq!(turns[1].usage.cache_write_tokens, 1200);
    assert_eq!(turns[2].text, "There are 4 people.");
}

#[test]
fn error_bodies_become_errors_with_help_and_no_key() {
    let openai = fixtures(OPENAI_FIXTURES);
    let mock = Mock::start(vec![answer(&openai, "error")], Duration::ZERO);
    let client = Client::new(config("openai", "gpt-test", &mock.base_url));
    let err = client.complete(&text_conversation()).unwrap_err();
    mock.next_request();
    assert_eq!(err.kind, AiErrorKind::Authentication);
    assert_eq!(err.status, Some(401));
    assert!(err.message.contains("Incorrect API key provided"), "{err}");
    assert!(err.help().contains("api_key_env"), "{err}");
    assert!(!err.to_string().contains(TEST_KEY));

    let anthropic = fixtures(ANTHROPIC_FIXTURES);
    let mock = Mock::start(vec![answer(&anthropic, "error")], Duration::ZERO);
    let client = Client::new(config("anthropic", "claude-test", &mock.base_url));
    let err = client.complete(&text_conversation()).unwrap_err();
    mock.next_request();
    assert_eq!(err.kind, AiErrorKind::RateLimited);
    assert!(err.message.contains("rate limit"), "{err}");
    assert!(err.to_string().contains("\nHelp: "), "{err}");
}

#[test]
fn statuses_map_to_kinds_and_a_key_in_a_message_is_scrubbed() {
    let cases = [
        (
            404,
            json!({"type": "error", "error": {"type": "not_found_error", "message": "model: nope"}}),
            AiErrorKind::ModelNotFound,
        ),
        (
            404,
            json!({"error": {"message": "The model `x` does not exist", "code": "model_not_found"}}),
            AiErrorKind::ModelNotFound,
        ),
        (
            404,
            json!("404 page not found"),
            AiErrorKind::EndpointNotFound,
        ),
        (
            529,
            json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}}),
            AiErrorKind::ProviderUnavailable,
        ),
        (
            400,
            json!({"error": {"message": format!("bad key {TEST_KEY} in request")}}),
            AiErrorKind::BadRequest,
        ),
        (
            403,
            json!({"error": "forbidden"}),
            AiErrorKind::Authentication,
        ),
    ];
    let answers = cases
        .iter()
        .map(|(status, body, _)| match body {
            Value::String(text) => (*status, text.clone()),
            other => (*status, other.to_string()),
        })
        .collect::<Vec<_>>();
    let mock = Mock::start(answers, Duration::ZERO);
    let client = Client::new(config("anthropic", "claude-test", &mock.base_url));
    for (status, _, kind) in &cases {
        let err = client.complete(&text_conversation()).unwrap_err();
        mock.next_request();
        assert_eq!((err.kind, err.status), (*kind, Some(*status)), "{err}");
        assert!(!err.to_string().contains(TEST_KEY), "{err}");
    }
}

#[test]
fn invalid_answers_unreachable_hosts_and_timeouts() {
    // A 200 that is not a provider answer.
    let mock = Mock::start(
        vec![
            (200, "not json".to_string()),
            (200, "{\"x\": 1}".to_string()),
        ],
        Duration::ZERO,
    );
    let client = Client::new(config("openai", "m", &mock.base_url));
    for _ in 0..2 {
        let err = client.complete(&text_conversation()).unwrap_err();
        mock.next_request();
        assert_eq!(err.kind, AiErrorKind::InvalidResponse, "{err}");
    }
    drop(mock);

    // Nobody listens on a port just released.
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
        listener.local_addr().expect("address").port()
    };
    let client = Client::new(config(
        "openai",
        "m",
        &format!("http://127.0.0.1:{port}/v1"),
    ));
    let err = client.complete(&text_conversation()).unwrap_err();
    assert_eq!(err.kind, AiErrorKind::Unreachable, "{err}");
    assert!(err.help().contains("base_url"), "{err}");

    // An answer slower than the timeout.
    let fixture = fixtures(OPENAI_FIXTURES);
    let mock = Mock::start(vec![answer(&fixture, "text")], Duration::from_millis(1500));
    let client = Client::with_timeout(
        config("openai", "m", &mock.base_url),
        Duration::from_millis(300),
    );
    let err = client.complete(&text_conversation()).unwrap_err();
    mock.next_request();
    assert_eq!(err.kind, AiErrorKind::Timeout, "{err}");
}

#[test]
fn hand_built_turns_and_failed_tool_results_are_sent_in_each_dialect() {
    let call = ToolCall {
        id: "c1".to_string(),
        name: "query".to_string(),
        arguments: json!({"script": "g.v().count()"}),
    };
    let mut conversation = tool_conversation();
    conversation
        .messages
        .push(Message::Assistant(AssistantTurn::new(
            "",
            vec![call],
            StopReason::ToolUse,
        )));
    conversation
        .messages
        .push(Message::ToolResults(vec![ToolResult {
            call_id: "c1".to_string(),
            content: "syntax error".to_string(),
            is_error: true,
        }]));
    conversation
        .messages
        .push(Message::User("Try again".to_string()));

    let anthropic = super::anthropic::request(
        &config("anthropic", "claude-test", "http://localhost:1"),
        &conversation,
    );
    assert_eq!(
        anthropic.body["messages"],
        json!([
            {"role": "user", "content": [{"type": "text", "text": QUESTION}]},
            {"role": "assistant", "content": [
                {"type": "tool_use", "id": "c1", "name": "query",
                 "input": {"script": "g.v().count()"}}]},
            {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "c1", "content": "syntax error",
                 "is_error": true},
                {"type": "text", "text": "Try again"}]}
        ])
    );

    let openai = super::openai::request(
        &config("openai", "gpt-test", "http://localhost:1/v1"),
        &conversation,
    );
    let messages = openai.body["messages"].as_array().expect("messages");
    assert_eq!(
        messages[3],
        json!({"role": "tool", "tool_call_id": "c1", "content": "Error: syntax error"})
    );
    assert_eq!(messages[4], json!({"role": "user", "content": "Try again"}));
    assert_eq!(openai.body["max_tokens"], json!(512));

    // OpenAI's own host gets max_completion_tokens.
    let own = super::openai::request(
        &config("openai", "gpt-test", "https://api.openai.com/v1"),
        &conversation,
    );
    assert_eq!(own.body["max_completion_tokens"], json!(512));
    assert!(own.body.get("max_tokens").is_none());
}

const GEMINI_FIXTURES: &str = include_str!("fixtures/gemini.json");

/// Asks the mock once (it answers `answer`) and returns the turn and its assistant message as
/// the next request sends it back, after the tool results of every call.
fn gemini_round_trip(answer: &Value) -> (AssistantTurn, Value) {
    let mock = Mock::start(vec![(200, answer.to_string())], Duration::ZERO);
    let config = config("openai", "gemini-test", &mock.base_url);
    let turn = Client::new(config.clone())
        .complete(&tool_conversation())
        .expect("gemini answer");
    mock.next_request();
    let mut conversation = tool_conversation();
    conversation.messages.push(Message::Assistant(turn.clone()));
    if !turn.tool_calls.is_empty() {
        conversation
            .messages
            .push(Message::ToolResults(results(&turn, "[1]", false)));
    }
    let request = super::openai::request(&config, &conversation);
    let sent = request.body["messages"][2].clone();
    (turn, sent)
}

fn results(turn: &AssistantTurn, content: &str, is_error: bool) -> Vec<ToolResult> {
    turn.tool_calls
        .iter()
        .map(|call| ToolResult {
            call_id: call.id.clone(),
            content: content.to_string(),
            is_error,
        })
        .collect()
}

#[test]
fn gemini_thought_signatures_are_sent_back_on_their_calls() {
    let fixture = fixtures(GEMINI_FIXTURES);

    // One call: its signature and the message's extra_content come back, reasoning_content not.
    let (turn, sent) = gemini_round_trip(&fixture["one_call"]);
    assert_eq!(turn.tool_calls[0].id, "function-call-1");
    assert_eq!(
        sent,
        json!({"role": "assistant", "content": null,
               "extra_content": {"google": {"thought_signature": "CsIBAVSoXO_msg"}},
               "tool_calls": [{"id": "function-call-1", "type": "function",
                   "function": {"name": "list_saved_queries", "arguments": "{}"},
                   "extra_content": {"google": {"thought_signature": "CsIBAVSoXO_one"}}}]})
    );
    assert!(sent.get("reasoning_content").is_none());

    // Two parallel calls: only the first carries a signature, and only it gets one back.
    let (_, sent) = gemini_round_trip(&fixture["two_calls"]);
    assert_eq!(
        sent,
        json!({"role": "assistant", "content": "Let me check both.",
               "tool_calls": [
                   {"id": "function-call-2", "type": "function",
                    "function": {"name": "query", "arguments": "{\"script\":\"g.v().count()\"}"},
                    "extra_content": {"google": {"thought_signature": "CsIBAVSoXO_first"}}},
                   {"id": "function-call-3", "type": "function",
                    "function": {"name": "query", "arguments": "{\"script\":\"g.e().count()\"}"}}]})
    );

    // A text answer keeps the signature on the message.
    let (_, sent) = gemini_round_trip(&fixture["text"]);
    assert_eq!(
        sent,
        json!({"role": "assistant", "content": "There are 6 vertices and 6 edges.",
               "extra_content": {"google": {"thought_signature": "CsIBAVSoXO_text"}}})
    );
}

#[test]
fn a_stopped_turn_keeps_the_signatures_of_its_calls() {
    let fixture = fixtures(GEMINI_FIXTURES);
    let mock = Mock::start(
        vec![(200, fixture["two_calls"].to_string())],
        Duration::ZERO,
    );
    let config = config("openai", "gemini-test", &mock.base_url);
    let turn = Client::new(config.clone())
        .complete(&tool_conversation())
        .expect("gemini answer");
    mock.next_request();
    // As the dev server's Stop leaves the history: every call answered "stopped", then the
    // person's next question.
    let mut conversation = tool_conversation();
    conversation.messages.push(Message::Assistant(turn.clone()));
    conversation.messages.push(Message::ToolResults(results(
        &turn,
        "Not completed: the user stopped the turn.",
        true,
    )));
    conversation
        .messages
        .push(Message::User("Only the vertices, please.".to_string()));
    let body = super::openai::request(&config, &conversation).body;
    assert_eq!(
        body["messages"][2]["tool_calls"][0]["extra_content"],
        json!({"google": {"thought_signature": "CsIBAVSoXO_first"}})
    );
    assert!(
        body["messages"][2]["tool_calls"][1]
            .get("extra_content")
            .is_none()
    );
}

#[test]
fn a_raw_call_without_an_id_is_matched_by_position() {
    let answer = json!({"choices": [{"message": {"role": "assistant", "content": null,
        "tool_calls": [{"type": "function", "function": {"name": "query", "arguments": "{}"},
                        "extra_content": {"google": {"thought_signature": "sig"}}}]},
        "finish_reason": "tool_calls"}]});
    let (turn, sent) = gemini_round_trip(&answer);
    assert_eq!(turn.tool_calls[0].id, "call_0");
    assert_eq!(
        sent["tool_calls"][0],
        json!({"id": "call_0", "type": "function",
               "function": {"name": "query", "arguments": "{}"},
               "extra_content": {"google": {"thought_signature": "sig"}}})
    );
}

#[test]
fn an_answer_only_call_keeps_the_tools_and_forbids_calling_them() {
    let mut conversation = tool_conversation();
    let openai_config = config("openai", "gpt-test", "http://127.0.0.1:1/v1");
    let anthropic_config = config("anthropic", "claude-test", "http://127.0.0.1:1");
    let plain = super::openai::request(&openai_config, &conversation).body;
    assert!(plain.get("tool_choice").is_none(), "{plain}");
    let plain = super::anthropic::request(&anthropic_config, &conversation).body;
    assert!(plain.get("tool_choice").is_none(), "{plain}");
    conversation.answer_only = true;
    let body = super::openai::request(&openai_config, &conversation).body;
    assert_eq!(body["tool_choice"], "none", "{body}");
    assert_eq!(body["tools"].as_array().map(Vec::len), Some(1), "{body}");
    let body = super::anthropic::request(&anthropic_config, &conversation).body;
    assert_eq!(body["tool_choice"], json!({"type": "none"}), "{body}");
    assert_eq!(body["tools"].as_array().map(Vec::len), Some(1), "{body}");
}

#[test]
fn request_tokens_count_the_cached_ones_in_each_dialect() {
    use super::config::Provider;
    let usage = Usage {
        input_tokens: 100,
        output_tokens: 7,
        cache_read_tokens: 60,
        cache_write_tokens: 5,
    };
    assert_eq!(usage.request_tokens(Provider::OpenAi), 100);
    assert_eq!(usage.request_tokens(Provider::Anthropic), 165);
}

#[test]
fn openai_usage_is_read_in_the_responses_spelling_too() {
    let config = config("openai", "gpt-test", "http://localhost:1/v1");
    let answer = |usage: Value| {
        json!({"choices": [{"message": {"role": "assistant", "content": "ok"},
                            "finish_reason": "stop"}],
               "usage": usage})
    };
    let chat = super::openai::response(
        &config,
        &answer(json!({"prompt_tokens": 30, "completion_tokens": 4,
                       "prompt_tokens_details": {"cached_tokens": 10}})),
    )
    .expect("chat completions usage");
    let responses = super::openai::response(
        &config,
        &answer(json!({"input_tokens": 30, "output_tokens": 4,
                       "input_tokens_details": {"cached_tokens": 10}})),
    )
    .expect("responses usage");
    let expected = Usage {
        input_tokens: 30,
        output_tokens: 4,
        cache_read_tokens: 10,
        cache_write_tokens: 0,
    };
    assert_eq!(chat.usage, expected);
    assert_eq!(responses.usage, expected);
    let none = super::openai::response(
        &config,
        &json!({"choices": [{"message": {"role": "assistant", "content": "ok"}}]}),
    )
    .expect("no usage");
    assert_eq!(none.usage, Usage::default());
}
