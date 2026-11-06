//! Usage statistics of the endpoint (`Mcp::stats`) after a known sequence of requests, in both
//! session modes, and the opt-in tool `usage_statistics`: JSON through `Mcp::handle`, without
//! HTTP.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, channel};

use graphersal_mcp::{
    ClientRef, Executor, Host, Job, Mcp, McpConfig, McpError, McpRequest, McpResponse, Refusal,
    SessionKey, ToolCaller, tool_listings,
};
use graphersal_session::Session;
use serde_json::{Value, json};

/// A host over one modern-graph session on a thread of its own (like the dev server's session
/// thread), or refusing every job as busy.
struct TestHost {
    jobs: Mutex<Sender<Job>>,
    busy: AtomicBool,
}

impl TestHost {
    fn modern() -> Self {
        let (jobs, queue) = channel::<Job>();
        std::thread::Builder::new()
            .stack_size(64 * 1024 * 1024)
            .spawn(move || {
                let mut session = Session::new();
                session.load_sample("modern").expect("modern");
                for job in queue {
                    job(&mut session);
                }
            })
            .expect("the session thread starts");
        Self {
            jobs: Mutex::new(jobs),
            busy: AtomicBool::new(false),
        }
    }
}

impl Executor for TestHost {
    fn execute(&self, _client: &ClientRef, job: Job) -> Result<(), McpError> {
        if self.busy.load(Ordering::SeqCst) {
            return Err(McpError::Busy("the server is busy; retry".into()));
        }
        let (done, wait) = channel();
        self.jobs
            .lock()
            .expect("jobs")
            .send(Box::new(move |session| {
                job(session);
                let _ = done.send(());
            }))
            .map_err(|_| McpError::Unavailable("the session is not running".into()))?;
        let _ = wait.recv();
        Ok(())
    }
}

impl Host for TestHost {
    fn view(&self) -> Value {
        Value::Null
    }
    fn add_process_statistics(&self, statistics: Value) -> Value {
        statistics
    }
    fn add_graph_info(&self, _info: &mut Value) -> Result<(), McpError> {
        Ok(())
    }
    fn examples_json(&self) -> Option<Vec<u8>> {
        None
    }
}

fn post(mcp: &Mcp, host: &TestHost, body: &Value, session: Option<&str>) -> McpResponse {
    let text = body.to_string();
    let mut request = McpRequest::new("POST", text.as_bytes())
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream");
    if let Some(session) = session {
        request = request.header("Mcp-Session-Id", session);
    }
    mcp.handle(host, request)
}

fn json_of(response: &McpResponse) -> Value {
    serde_json::from_slice(response.body()).expect("a JSON body")
}

fn initialize(mcp: &Mcp, host: &TestHost, client: &str) -> String {
    let response = post(
        mcp,
        host,
        &json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": {"name": client, "version": "1"}}}),
        None,
    );
    assert_eq!(response.status(), 200);
    response
        .header("mcp-session-id")
        .expect("an id")
        .to_string()
}

fn call(mcp: &Mcp, host: &TestHost, session: &str, name: &str, args: Value) -> McpResponse {
    post(
        mcp,
        host,
        &json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call",
            "params": {"name": name, "arguments": args}}),
        Some(session),
    )
}

fn entry<'a>(list: &'a Value, name: &str) -> &'a Value {
    list.as_array()
        .expect("a list")
        .iter()
        .find(|e| e["name"] == name)
        .unwrap_or_else(|| panic!("no entry {name} in {list}"))
}

fn config(text: &str) -> McpConfig {
    McpConfig::parse(text, &mut |name| Err(format!("no file {name}"))).expect("a configuration")
}

/// The counters after a known sequence: three initializes of two clients, a listing, tool
/// calls (a saved query by `run_saved_query` and by its own tool, a failing GQL query, an
/// unknown tool), a notification, an unknown session, a busy host and a host's own refusal.
fn known_sequence(mcp: &Mcp) -> Value {
    let host = TestHost::modern();
    let a = initialize(mcp, &host, "claude-code");
    let _again = initialize(mcp, &host, "claude-code");
    let b = initialize(mcp, &host, "opencode");
    let listed = post(
        mcp,
        &host,
        &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        Some(&a),
    );
    assert_eq!(listed.status(), 200);
    let defined = call(
        mcp,
        &host,
        &a,
        "query",
        json!({"script": "g.define_query(#{name: \"older\", body: \"fn older() { g.v().has(\\\"age\\\", P.gt(30)) }\"})"}),
    );
    assert_eq!(json_of(&defined)["result"]["isError"], false, "{defined:?}");
    let ran = call(mcp, &host, &a, "run_saved_query", json!({"name": "older"}));
    assert_eq!(json_of(&ran)["result"]["isError"], false);
    let own = call(mcp, &host, &b, "older", json!({}));
    assert_eq!(
        json_of(&own)["result"]["isError"],
        false,
        "{:?}",
        json_of(&own)
    );
    let failed = call(mcp, &host, &b, "gql", json!({"query": "MATCH (n RETURN n"}));
    assert_eq!(json_of(&failed)["result"]["isError"], true);
    let unknown = call(mcp, &host, &b, "nope", json!({}));
    assert_eq!(json_of(&unknown)["error"]["code"], -32602);
    let notified = post(
        mcp,
        &host,
        &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        Some(&a),
    );
    assert_eq!(notified.status(), 202);
    let lost = post(
        mcp,
        &host,
        &json!({"jsonrpc": "2.0", "id": 3, "method": "ping"}),
        Some("0123"),
    );
    assert_eq!(lost.status(), 404);
    host.busy.store(true, Ordering::SeqCst);
    let busy = call(mcp, &host, &a, "statistics", json!({}));
    assert_eq!(busy.status(), 503);
    mcp.stats().record_refusal(Refusal::QueueFull);
    mcp.stats().to_json().expect("the statistics")
}

fn check_known_sequence(stats: &Value) {
    assert_eq!(stats["sessions"]["initialized"], 3, "{stats}");
    assert_eq!(stats["sessions"]["activeLast5Min"], 3, "{stats}");
    // 3 initialize + tools/list + 6 tools/call + 1 notification; the 404 is not dispatched.
    assert_eq!(stats["requests"]["total"], 11, "{stats}");
    assert_eq!(stats["requests"]["withoutSession"], 3);
    assert_eq!(stats["requests"]["busy503"], 1);
    assert_eq!(stats["methods"]["initialize"], 3);
    assert_eq!(stats["methods"]["tools/list"], 1);
    assert_eq!(stats["methods"]["tools/call"], 6);
    assert_eq!(stats["methods"]["(notification)"], 1);
    assert_eq!(stats["refusals"]["unknownSession"], 1);
    assert_eq!(stats["refusals"]["queueFull"], 1);
    assert_eq!(stats["refusals"]["invalidRequest"], 0);
    let tools = &stats["tools"];
    assert_eq!(entry(tools, "query")["calls"], 1);
    assert_eq!(entry(tools, "run_saved_query")["calls"], 1);
    assert_eq!(entry(tools, "older")["calls"], 1);
    let gql = entry(tools, "gql");
    assert_eq!(
        (gql["calls"].as_u64(), gql["errors"].as_u64()),
        (Some(1), Some(1))
    );
    assert!(gql["answerChars"].as_u64().unwrap() > 0);
    let histogram: u64 = gql["histogram"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n.as_u64().unwrap())
        .sum();
    assert_eq!(histogram, 1);
    assert_eq!(entry(tools, "(unknown)")["errors"], 1);
    // The busy call counts as an error of its tool.
    assert_eq!(entry(tools, "statistics")["errors"], 1);
    // Both ways of running the saved query count for it.
    assert_eq!(entry(&stats["savedQueries"], "older")["calls"], 2);
    assert_eq!(stats["savedQueries"].as_array().unwrap().len(), 1);
    let clients = &stats["clients"];
    let claude = entry(clients, "claude-code");
    assert_eq!(claude["initialized"], 2);
    // tools/list, 3 tool calls (one busy), the notification.
    assert_eq!(claude["requests"], 5, "{clients}");
    assert_eq!(claude["toolCalls"], 3);
    let opencode = entry(clients, "opencode");
    assert_eq!(
        (
            opencode["initialized"].as_u64(),
            opencode["toolCalls"].as_u64()
        ),
        (Some(1), Some(3))
    );
    assert_eq!(stats["clientsKept"], 2);
    assert_eq!(stats["latencyBucketsMs"], json!([1, 10, 100, 1000, 10000]));
}

#[test]
fn counters_after_a_known_sequence_with_the_session_table() {
    let mcp = Mcp::new(false, None, true);
    check_known_sequence(&known_sequence(&mcp));
}

#[test]
fn counters_after_a_known_sequence_with_stateless_sessions() {
    let mcp =
        Mcp::new(false, None, true).with_session_key(SessionKey::new(&[3u8; 32]).expect("a key"));
    check_known_sequence(&known_sequence(&mcp));
}

#[test]
fn refusals_before_dispatch_are_counted() {
    let host = TestHost::modern();
    let mcp = Mcp::new(true, Some("s3cret".into()), false);
    let request =
        McpRequest::new("POST", b"{}".as_slice()).header("Content-Type", "application/json");
    assert_eq!(mcp.handle(&host, request).status(), 401);
    let request = McpRequest::new("POST", b"{\"jsonrpc\":".as_slice())
        .header("Content-Type", "application/json")
        .header("Authorization", "Bearer s3cret");
    assert_eq!(mcp.handle(&host, request).status(), 400);
    let stats = mcp.stats().to_json().unwrap();
    assert_eq!(stats["refusals"]["unauthorized"], 1);
    assert_eq!(stats["refusals"]["invalidRequest"], 1);
    assert_eq!(stats["requests"]["total"], 0);
    let summary = mcp.stats().summary();
    assert_eq!(summary["requests"], 0);
    assert_eq!(summary["toolCalls"], 0);
}

fn names(listing: &[Value]) -> Vec<String> {
    listing
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn usage_statistics_is_offered_only_when_the_configuration_names_it() {
    let host = TestHost::modern();
    for read_only in [false, true] {
        // The default listing, a configuration without include, and the AI Chat's caller.
        let plain = Mcp::new(read_only, None, false);
        assert!(!names(&plain.tool_listings(&host).unwrap()).contains(&"usage_statistics".into()));
        let excluding = Mcp::new(read_only, None, false)
            .with_config(config(r#"{"tools": {"exclude": ["journal"]}}"#));
        assert!(
            !names(&excluding.tool_listings(&host).unwrap()).contains(&"usage_statistics".into())
        );
        let chat = ToolCaller::new(ClientRef::new("ai", Some("c1")), read_only);
        assert!(!names(&tool_listings(&host, &chat).unwrap()).contains(&"usage_statistics".into()));
        // A call without the configuration is refused like any tool not offered.
        let session = initialize(&plain, &host, "tester");
        let refused = call(&plain, &host, &session, "usage_statistics", json!({}));
        let message = json_of(&refused)["error"]["message"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(message.contains("not offered"), "{message}");
        // Named in tools.include: listed (reading) and answering the counters.
        let configured = Mcp::new(read_only, None, false).with_config(config(
            r#"{"tools": {"include": ["gql", "usage_statistics"]}}"#,
        ));
        let listing = configured.tool_listings(&host).unwrap();
        assert_eq!(names(&listing), ["gql", "usage_statistics"]);
        let tool = &listing[1];
        assert_eq!(tool["annotations"]["readOnlyHint"], true);
        let session = initialize(&configured, &host, "tester");
        call(
            &configured,
            &host,
            &session,
            "gql",
            json!({"query": "MATCH (p:person) RETURN p.name AS n"}),
        );
        let answer = call(
            &configured,
            &host,
            &session,
            "usage_statistics",
            json!({"max_rows": 1}),
        );
        let result = json_of(&answer)["result"].clone();
        assert_eq!(result["isError"], false, "{result}");
        let text = result["content"][0]["text"].as_str().unwrap();
        let stats: Value = serde_json::from_str(text).expect("compact JSON");
        assert_eq!(stats["tools"].as_array().unwrap().len(), 1, "{stats}");
        assert_eq!(stats["tools"][0]["name"], "gql");
        assert_eq!(stats["sessions"]["initialized"], 1);
        assert_eq!(stats["clients"][0]["name"], "tester");
        // The short description in short mode.
        let short = Mcp::new(read_only, None, false).with_config(config(
            r#"{"descriptions": "short", "tools": {"include": ["usage_statistics"]}}"#,
        ));
        let listing = short.tool_listings(&host).unwrap();
        assert!(listing[0]["description"].as_str().unwrap().len() < 200);
    }
}
