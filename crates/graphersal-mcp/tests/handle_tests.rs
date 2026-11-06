//! The transport entry point `Mcp::handle` driven with JSON, without HTTP: a test host runs the
//! session jobs on a thread of its own (like the dev server's session thread) and records the
//! client of every job.

use std::sync::Mutex;
use std::sync::mpsc::{Sender, channel};

use graphersal_mcp::{
    ClientRef, Executor, Host, Job, MAX_TEXT_CHARS, Mcp, McpError, McpRequest, McpResponse,
    PROTOCOL_VERSIONS, ToolCaller, call_tool, tool_listings,
};
use graphersal_session::Session;
use serde_json::{Value, json};

/// A host over one session on its own thread (the modern graph), or a host that cannot run
/// jobs (`jobs: None`).
struct TestHost {
    jobs: Option<Mutex<Sender<Job>>>,
    /// The client of every job, in order.
    clients: Mutex<Vec<ClientRef>>,
}

impl TestHost {
    fn modern() -> Self {
        let (jobs, queue) = channel::<Job>();
        std::thread::Builder::new()
            .stack_size(64 * 1024 * 1024)
            .spawn(move || {
                let mut session = Session::new();
                session
                    .load_sample("modern")
                    .expect("the modern graph loads");
                for job in queue {
                    job(&mut session);
                }
            })
            .expect("the session thread starts");
        Self {
            jobs: Some(Mutex::new(jobs)),
            clients: Mutex::new(Vec::new()),
        }
    }

    fn broken() -> Self {
        Self {
            jobs: None,
            clients: Mutex::new(Vec::new()),
        }
    }

    fn clients(&self) -> Vec<ClientRef> {
        self.clients.lock().expect("clients").clone()
    }
}

impl Executor for TestHost {
    fn execute(&self, client: &ClientRef, job: Job) -> Result<(), McpError> {
        self.clients.lock().expect("clients").push(client.clone());
        let Some(jobs) = &self.jobs else {
            return Err(McpError::Unavailable("the session is not running".into()));
        };
        let (done, wait) = channel();
        jobs.lock()
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

    fn add_graph_info(&self, info: &mut Value) -> Result<(), McpError> {
        info["host"] = json!("test");
        Ok(())
    }

    fn examples_json(&self) -> Option<Vec<u8>> {
        Some(br#"[{"group": "Basics", "graph": "modern", "title": "Count", "script": "g.v().count()"}]"#.to_vec())
    }
}

/// A `POST` of `body` with the headers an MCP client sends, plus `extra`.
fn post(mcp: &Mcp, host: &TestHost, body: &Value, extra: &[(&str, &str)]) -> McpResponse {
    let text = body.to_string();
    let mut request = McpRequest::new("POST", text.as_bytes())
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream");
    for (name, value) in extra {
        request = request.header(name, value);
    }
    mcp.handle(host, request)
}

fn json_of(response: &McpResponse) -> Value {
    serde_json::from_slice(response.body()).expect("a JSON body")
}

/// `initialize`, returning the session id.
fn initialize(mcp: &Mcp, host: &TestHost) -> String {
    let response = post(
        mcp,
        host,
        &json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": {"name": "tester", "version": "1"}}}),
        &[],
    );
    assert_eq!(response.status(), 200);
    response
        .header("mcp-session-id")
        .expect("a session id")
        .to_string()
}

fn call(mcp: &Mcp, host: &TestHost, session: &str, name: &str, args: Value) -> Value {
    let response = post(
        mcp,
        host,
        &json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call",
            "params": {"name": name, "arguments": args}}),
        &[
            ("Mcp-Session-Id", session),
            ("MCP-Protocol-Version", "2025-06-18"),
        ],
    );
    assert_eq!(response.status(), 200);
    json_of(&response)
}

#[test]
fn initialize_assigns_a_session_and_negotiates_the_version() {
    let host = TestHost::modern();
    let mcp = Mcp::new(false, None, false).with_operator_notes(Some("Be kind.".into()));
    let response = post(
        &mcp,
        &host,
        &json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "clientInfo": {"name": "tester", "version": "2"}}}),
        &[],
    );
    assert_eq!(response.status(), 200);
    assert_eq!(response.header("Content-Type"), Some("application/json"));
    assert_eq!(response.header("Cache-Control"), Some("no-store"));
    let session = response.header("Mcp-Session-Id").expect("a session id");
    assert_eq!(session.len(), 32);
    assert!(session.chars().all(|c| c.is_ascii_hexdigit()));
    let answer = json_of(&response);
    assert_eq!(answer["id"], 1);
    assert_eq!(answer["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(answer["result"]["serverInfo"]["name"], "graphersal-dev");
    let instructions = answer["result"]["instructions"].as_str().expect("text");
    assert!(instructions.starts_with(graphersal_mcp::instructions()));
    assert!(
        instructions
            .ends_with("Notes from the operator of this server about this graph:\n\nBe kind.")
    );
    assert!(!instructions.contains("READ-ONLY"));
    assert_eq!(
        mcp.status(),
        json!({"endpoint": "/mcp", "readOnly": false,
            "client": {"name": "tester", "version": "2"}})
    );
    // An unknown revision gets the newest; initialize runs no session job.
    let response = post(
        &mcp,
        &host,
        &json!({"jsonrpc": "2.0", "id": 2, "method": "initialize",
            "params": {"protocolVersion": "1999-01-01"}}),
        &[],
    );
    assert_eq!(
        json_of(&response)["result"]["protocolVersion"],
        PROTOCOL_VERSIONS[0]
    );
    assert!(host.clients().is_empty());
}

#[test]
fn tools_run_on_the_host_as_the_client_mcp() {
    let host = TestHost::modern();
    let mcp = Mcp::new(false, None, false);
    let session = initialize(&mcp, &host);
    let answer = call(
        &mcp,
        &host,
        &session,
        "query",
        json!({"script": "g.v().has_label(\"person\").values(\"name\")"}),
    );
    let text = answer["result"]["content"][0]["text"]
        .as_str()
        .expect("text");
    assert!(text.starts_with("ok: 4 results ("), "{text}");
    assert!(
        text.ends_with(")\n[\"marko\",\"vadas\",\"josh\",\"peter\"]"),
        "{text}"
    );
    assert_eq!(answer["result"]["isError"], false);
    let answer = call(
        &mcp,
        &host,
        &session,
        "gql",
        json!({"query": "MATCH (p:person) WHERE p.age > $min RETURN p.name AS n ORDER BY n",
            "params": {"min": 30}, "max_rows": 1}),
    );
    let text = answer["result"]["content"][0]["text"]
        .as_str()
        .expect("text");
    assert!(text.contains("\n[{\"n\":\"josh\"}]\n"), "{text}");
    assert!(text.ends_with("next page offset 1"), "{text}");
    let info = call(&mcp, &host, &session, "graph_info", json!({}));
    let info: Value =
        serde_json::from_str(info["result"]["content"][0]["text"].as_str().expect("text"))
            .expect("graph_info is JSON");
    assert_eq!(info["host"], "test");
    assert_eq!(info["readOnly"], false);
    assert_eq!(info["answerLimitChars"], MAX_TEXT_CHARS);
    assert!(info["view"].is_null() && info.get("note").is_none());
    let clients = host.clients();
    assert!(!clients.is_empty());
    assert!(
        clients
            .iter()
            .all(|c| c.name() == "mcp" && c.detail().is_none())
    );
    // The examples resource comes from the host.
    let response = post(
        &mcp,
        &host,
        &json!({"jsonrpc": "2.0", "id": 9, "method": "resources/read",
            "params": {"uri": "graphersal://examples"}}),
        &[("Mcp-Session-Id", &session)],
    );
    let text = json_of(&response)["result"]["contents"][0]["text"]
        .as_str()
        .expect("text")
        .to_string();
    assert!(
        text.contains("\n## Basics (graph: modern)\n\nCount:\n```\ng.v().count()\n```\n"),
        "{text}"
    );
}

#[test]
fn read_only_lists_reading_tools_and_refuses_writes() {
    let host = TestHost::modern();
    let mcp = Mcp::new(true, None, false);
    let session = initialize(&mcp, &host);
    let response = post(
        &mcp,
        &host,
        &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        &[("Mcp-Session-Id", &session)],
    );
    let tools = json_of(&response)["result"]["tools"].clone();
    let names: Vec<&str> = tools
        .as_array()
        .expect("a list")
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert!(names.contains(&"query") && names.contains(&"gql"));
    assert!(!names.contains(&"set_schema") && !names.contains(&"create_index"));
    let answer = call(&mcp, &host, &session, "set_schema", json!({"schema": {}}));
    assert_eq!(answer["error"]["code"], -32602);
    assert_eq!(
        answer["error"]["message"],
        "unknown tool \"set_schema\" (the server runs with --mcp-read-only: only reading tools)"
    );
    let answer = call(
        &mcp,
        &host,
        &session,
        "query",
        json!({"script": "g.add_v(\"person\")"}),
    );
    assert_eq!(answer["result"]["isError"], true);
    // The listing is the same through `tool_listings` (the AI Chat's way in).
    let caller = ToolCaller::new(ClientRef::new("ai", Some("c1")), true);
    let listed = tool_listings(&host, &caller).expect("listed");
    assert_eq!(Value::Array(listed), tools);
}

#[test]
fn a_caller_runs_its_jobs_as_its_own_client() {
    let host = TestHost::modern();
    let caller = ToolCaller::new(ClientRef::new("ai", Some("c1")), false).with_max_chars(1000);
    let outcome = call_tool(&host, &caller, "list_indexes", &json!({})).expect("the tool runs");
    assert!(!outcome.is_error);
    assert_eq!(
        outcome.text,
        "The graph has no property indexes (create_index adds one)."
    );
    assert_eq!(host.clients(), vec![ClientRef::new("ai", Some("c1"))]);
    let refused = call_tool(&host, &caller, "nope", &json!({})).expect_err("unknown");
    assert_eq!(refused.message(), "unknown tool \"nope\"");
    let refused = call_tool(&host, &caller, "query", &json!(3)).expect_err("not an object");
    assert_eq!(refused.message(), "arguments must be an object");
}

#[test]
fn a_host_that_cannot_run_jobs_answers_tool_errors() {
    let host = TestHost::broken();
    let mcp = Mcp::new(false, None, false);
    let session = initialize(&mcp, &host);
    let answer = call(&mcp, &host, &session, "query", json!({"script": "1"}));
    assert_eq!(answer["result"]["isError"], true);
    assert_eq!(
        answer["result"]["content"][0]["text"],
        "the session is not running"
    );
    // A failure of a resource is a JSON-RPC internal error.
    let response = post(
        &mcp,
        &host,
        &json!({"jsonrpc": "2.0", "id": 3, "method": "resources/read",
            "params": {"uri": "graphersal://schema"}}),
        &[("Mcp-Session-Id", &session)],
    );
    assert_eq!(
        json_of(&response)["error"],
        json!({"code": -32603, "message": "the session is not running"})
    );
}

#[test]
fn the_transport_answers_without_a_session_job() {
    let host = TestHost::broken();
    let mcp = Mcp::new(false, None, false);
    let ping = json!({"jsonrpc": "2.0", "id": 1, "method": "ping"});
    // Unknown session, unsupported revision, wrong Accept and Content-Type.
    let response = post(&mcp, &host, &ping, &[("Mcp-Session-Id", "abc")]);
    assert_eq!(response.status(), 404);
    assert_eq!(json_of(&response)["error"]["code"], -32600);
    let response = post(
        &mcp,
        &host,
        &ping,
        &[("MCP-Protocol-Version", "1999-01-01")],
    );
    assert_eq!(response.status(), 400);
    let ping_text = ping.to_string();
    let request = McpRequest::new("POST", ping_text.as_bytes())
        .header("Content-Type", "application/json")
        .header("Accept", "text/html");
    assert_eq!(mcp.handle(&host, request).status(), 406);
    let request =
        McpRequest::new("POST", ping_text.as_bytes()).header("content-type", "text/plain");
    let response = mcp.handle(&host, request);
    assert_eq!(response.status(), 415);
    assert_eq!(
        json_of(&response)["error"]["message"],
        "send the body as Content-Type: application/json"
    );
    // Malformed JSON, a batch, a notification, a response.
    let request =
        McpRequest::new("POST", &b"{\"jsonrpc\":"[..]).header("Content-Type", "application/json");
    let response = mcp.handle(&host, request);
    assert_eq!(response.status(), 400);
    assert_eq!(json_of(&response)["error"]["code"], -32700);
    let response = post(&mcp, &host, &json!([ping]), &[]);
    assert_eq!(response.status(), 400);
    let response = post(
        &mcp,
        &host,
        &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        &[],
    );
    assert_eq!(response.status(), 202);
    assert!(response.body().is_empty());
    let response = post(
        &mcp,
        &host,
        &json!({"jsonrpc": "2.0", "id": 4, "result": {}}),
        &[],
    );
    assert_eq!(response.status(), 202);
    // A method that does not exist; ping without a session.
    let answer = json_of(&post(
        &mcp,
        &host,
        &json!({"jsonrpc": "2.0", "id": 5, "method": "nope"}),
        &[],
    ));
    assert_eq!(answer["error"]["code"], -32601);
    let response = post(&mcp, &host, &ping, &[]);
    assert_eq!(response.status(), 200);
    assert_eq!(json_of(&response)["result"], json!({}));
    // GET has no stream; DELETE ends a session once.
    let response = mcp.handle(&host, McpRequest::new("GET", std::io::empty()));
    assert_eq!(response.status(), 405);
    assert_eq!(response.header("Allow"), Some("POST, DELETE"));
    let response = mcp.handle(&host, McpRequest::new("DELETE", std::io::empty()));
    assert_eq!(response.status(), 400);
    let session = initialize(&mcp, &host);
    let delete = || {
        mcp.handle(
            &host,
            McpRequest::new("DELETE", std::io::empty()).header("Mcp-Session-Id", &session),
        )
    };
    assert_eq!(delete().status(), 200);
    assert_eq!(delete().status(), 404);
    assert!(host.clients().is_empty());
}

#[test]
fn a_token_is_required_when_set() {
    let host = TestHost::broken();
    let mcp = Mcp::new(false, Some("s3cret".into()), false);
    let ping = json!({"jsonrpc": "2.0", "id": 1, "method": "ping"});
    for given in [None, Some("Bearer wrong"), Some("Basic s3cret")] {
        let headers: Vec<(&str, &str)> = given.map(|v| ("Authorization", v)).into_iter().collect();
        let response = post(&mcp, &host, &ping, &headers);
        assert_eq!(response.status(), 401, "{given:?}");
        assert_eq!(response.header("WWW-Authenticate"), Some("Bearer"));
    }
    let response = post(&mcp, &host, &ping, &[("Authorization", "Bearer s3cret")]);
    assert_eq!(response.status(), 200);
}

#[test]
fn a_body_over_the_limit_is_refused() {
    let host = TestHost::broken();
    let mcp = Mcp::new(false, None, false);
    let request = McpRequest::new(
        "POST",
        std::io::repeat(b' ').take_bytes(16 * 1024 * 1024 + 1),
    )
    .header("Content-Type", "application/json");
    let response = mcp.handle(&host, request);
    assert_eq!(response.status(), 413);
    let request = McpRequest::new("POST", std::io::empty())
        .header("Content-Type", "application/json")
        .header("Content-Length", "99999999999");
    assert_eq!(mcp.handle(&host, request).status(), 413);
    let request =
        McpRequest::new("POST", &[0xff_u8, 0xfe][..]).header("Content-Type", "application/json");
    let response = mcp.handle(&host, request);
    assert_eq!(response.status(), 400);
    assert_eq!(
        json_of(&response)["error"]["message"],
        "the body is not UTF-8"
    );
}

/// `Read::take` under a name that does not clash with the iterator's.
trait TakeBytes: std::io::Read + Sized {
    fn take_bytes(self, n: u64) -> std::io::Take<Self> {
        std::io::Read::take(self, n)
    }
}

impl<R: std::io::Read> TakeBytes for R {}
