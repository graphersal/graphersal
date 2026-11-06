//! Stateless MCP session ids (`Mcp::with_session_key`) and a busy host (`McpError::Busy`):
//! JSON through `Mcp::handle`, without HTTP.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use graphersal_mcp::{
    ClientRef, Executor, Host, Job, Mcp, McpError, McpRequest, McpResponse, SessionKey,
};
use graphersal_session::Session;
use serde_json::{Value, json};

/// A host that runs every job on a fresh modern-graph session on the calling thread, or
/// refuses it as busy; records the client keys.
#[derive(Default)]
struct PoolHost {
    busy: AtomicBool,
    keys: Mutex<Vec<Option<String>>>,
}

impl Executor for PoolHost {
    fn execute(&self, client: &ClientRef, job: Job) -> Result<(), McpError> {
        self.keys
            .lock()
            .expect("keys")
            .push(client.key().map(str::to_string));
        if self.busy.load(Ordering::SeqCst) {
            return Err(McpError::Busy("the server is busy; retry".into()));
        }
        let mut session = Session::new();
        session.load_sample("modern").expect("modern");
        job(&mut session);
        Ok(())
    }
}

impl Host for PoolHost {
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

fn post(mcp: &Mcp, host: &PoolHost, body: &Value, session: Option<&str>) -> McpResponse {
    let text = body.to_string();
    let mut request = McpRequest::new("POST", text.as_bytes())
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream");
    if let Some(session) = session {
        request = request.header("Mcp-Session-Id", session);
    }
    mcp.handle(host, request)
}

fn initialize(mcp: &Mcp, host: &PoolHost) -> String {
    let response = post(
        mcp,
        host,
        &json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": {"name": "tester", "version": "1"}}}),
        None,
    );
    assert_eq!(response.status(), 200);
    response
        .header("mcp-session-id")
        .expect("an id")
        .to_string()
}

fn key() -> SessionKey {
    SessionKey::new(b"a shared secret of the replicas, 32+ bytes").expect("a valid key")
}

const PING: &str = r#"{"jsonrpc": "2.0", "id": 2, "method": "ping"}"#;

#[test]
fn an_id_of_one_endpoint_is_valid_on_another_with_the_same_key() {
    let host = PoolHost::default();
    let one = Mcp::new(true, None, false).with_session_key(key());
    let two = Mcp::new(true, None, false).with_session_key(key());
    let other = Mcp::new(true, None, false).with_session_key(SessionKey::random());
    assert!(one.stateless_sessions());
    assert!(!Mcp::new(true, None, false).stateless_sessions());
    let ping: Value = serde_json::from_str(PING).unwrap();
    let id = initialize(&one, &host);
    assert_eq!(post(&two, &host, &ping, Some(&id)).status(), 200);
    assert_eq!(post(&other, &host, &ping, Some(&id)).status(), 404);
    // No table: hundreds of sessions evict none.
    for _ in 0..200 {
        initialize(&one, &host);
    }
    assert_eq!(post(&one, &host, &ping, Some(&id)).status(), 200);
    let (names, _) = one.memory_bytes();
    assert_eq!(names, 1, "one client name kept for the statistics");
    // The session id is the key of the client's jobs.
    let call = json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": {"name": "query", "arguments": {"script": "g.v().count()"}}});
    assert_eq!(post(&two, &host, &call, Some(&id)).status(), 200);
    assert_eq!(
        host.keys
            .lock()
            .unwrap()
            .last()
            .cloned()
            .flatten()
            .as_deref(),
        Some(id.as_str())
    );
}

#[test]
fn a_busy_host_makes_the_request_503_with_retry_after() {
    let host = PoolHost::default();
    let mcp = Mcp::new(true, None, false).with_session_key(key());
    let id = initialize(&mcp, &host);
    host.busy.store(true, Ordering::SeqCst);
    let call = json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": {"name": "query", "arguments": {"script": "g.v().count()"}}});
    let response = post(&mcp, &host, &call, Some(&id));
    assert_eq!(response.status(), 503);
    assert_eq!(response.header("retry-after"), Some("1"));
    let body: Value = serde_json::from_slice(response.body()).unwrap();
    assert_eq!(body["id"], 3, "{body}");
    assert!(
        body["error"]["message"].as_str().unwrap().contains("busy"),
        "{body}"
    );
    // A request that runs no job is still answered.
    let ping: Value = serde_json::from_str(PING).unwrap();
    assert_eq!(post(&mcp, &host, &ping, Some(&id)).status(), 200);
    host.busy.store(false, Ordering::SeqCst);
    assert_eq!(post(&mcp, &host, &call, Some(&id)).status(), 200);
}
