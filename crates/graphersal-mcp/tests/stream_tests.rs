//! The message level: a [`Connection`] served over MCP's stdio framing ([`serve_lines`]) on
//! in-memory pipes, its pushes (`notifications/tools/list_changed`, `notifications/progress`),
//! cancellation, and the same answers as the HTTP adapter byte for byte.

use std::io::{BufRead, BufReader, Write};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use graphersal::exec::CancelToken;
use graphersal_mcp::{
    ClientRef, Connection, Executor, Host, Job, Mcp, McpError, McpRequest, serve_lines,
};
use graphersal_session::Session;
use serde_json::{Value, json};

/// How long a test waits for a line before it fails.
const WAIT: Duration = Duration::from_secs(20);

/// A query that runs until it is cancelled.
const ENDLESS: &str = "let n = 0; loop { n += 1; }";

/// A host over one session on its own thread (the modern graph) with a runs registry like
/// graphersal-serve's: a run is cancelled by its client key and request id.
struct TestHost {
    jobs: Mutex<Sender<Job>>,
    runs: Mutex<Vec<(u64, ClientRef, CancelToken)>>,
    next: Mutex<u64>,
}

impl TestHost {
    fn modern() -> Arc<Self> {
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
        Arc::new(Self {
            jobs: Mutex::new(jobs),
            runs: Mutex::new(Vec::new()),
            next: Mutex::new(0),
        })
    }
}

impl Executor for TestHost {
    fn execute(&self, _client: &ClientRef, job: Job) -> Result<(), McpError> {
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

    fn add_graph_info(&self, info: &mut Value) -> Result<(), McpError> {
        info["host"] = json!("test");
        Ok(())
    }

    fn examples_json(&self) -> Option<Vec<u8>> {
        None
    }

    fn run_started(&self, client: &ClientRef, cancel: &CancelToken) -> u64 {
        let mut next = self.next.lock().expect("next");
        *next += 1;
        self.runs
            .lock()
            .expect("runs")
            .push((*next, client.clone(), cancel.clone()));
        *next
    }

    fn run_ended(&self, run: u64) {
        self.runs
            .lock()
            .expect("runs")
            .retain(|(id, ..)| *id != run);
    }

    fn cancel_request(&self, client: &ClientRef, request: &str) -> bool {
        let runs = self.runs.lock().expect("runs");
        let mut found = false;
        for (_, owner, cancel) in runs.iter() {
            if owner.key() == client.key() && owner.request() == Some(request) {
                cancel.cancel();
                found = true;
            }
        }
        found
    }
}

/// A client on in-memory pipes: it writes lines to the served connection and reads its lines
/// through a thread (so a test waits with a timeout).
struct Client {
    to_server: Option<std::io::PipeWriter>,
    lines: Receiver<String>,
    server: Option<std::thread::JoinHandle<Result<(), McpError>>>,
    /// Lines read while waiting for another one.
    seen: Vec<Value>,
}

impl Client {
    fn serve(connection: Connection) -> Self {
        let (server_in, to_server) = std::io::pipe().expect("a pipe");
        let (from_server, server_out) = std::io::pipe().expect("a pipe");
        let server = std::thread::spawn(move || {
            serve_lines(&connection, BufReader::new(server_in), server_out)
        });
        let (lines_tx, lines) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(from_server).lines() {
                let Ok(line) = line else { break };
                if lines_tx.send(line).is_err() {
                    break;
                }
            }
        });
        Self {
            to_server: Some(to_server),
            lines,
            server: Some(server),
            seen: Vec::new(),
        }
    }

    fn send(&mut self, line: &str) {
        let to_server = self.to_server.as_mut().expect("open");
        to_server.write_all(line.as_bytes()).expect("written");
        to_server.write_all(b"\n").expect("written");
        to_server.flush().expect("flushed");
    }

    /// The next line, as JSON.
    fn next(&mut self) -> Value {
        let line = self.lines.recv_timeout(WAIT).expect("a line in time");
        assert!(!line.contains('\n'));
        serde_json::from_str(&line).unwrap_or_else(|err| panic!("{err}: {line}"))
    }

    /// The raw text of the answer to `id`; other lines are kept in `seen`.
    fn answer_text(&mut self, id: &Value) -> String {
        loop {
            let line = self.lines.recv_timeout(WAIT).expect("an answer in time");
            let message: Value = serde_json::from_str(&line).expect("JSON");
            if message.get("id") == Some(id) && message.get("method").is_none() {
                return line;
            }
            self.seen.push(message);
        }
    }

    fn answer(&mut self, id: &Value) -> Value {
        serde_json::from_str(&self.answer_text(id)).expect("JSON")
    }

    fn request(&mut self, id: i64, method: &str, params: Value) -> Value {
        self.send(
            &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string(),
        );
        self.answer(&json!(id))
    }

    /// Closes the input and waits for the server to end; the lines left.
    fn close(mut self) -> (Result<(), McpError>, Vec<Value>) {
        drop(self.to_server.take());
        let result = self
            .server
            .take()
            .expect("serving")
            .join()
            .expect("the server thread");
        let mut rest = std::mem::take(&mut self.seen);
        while let Ok(line) = self.lines.recv_timeout(WAIT) {
            rest.push(serde_json::from_str(&line).expect("JSON"));
        }
        (result, rest)
    }
}

fn initialize_params() -> Value {
    json!({"protocolVersion": "2025-06-18", "capabilities": {"roots": {}},
        "clientInfo": {"name": "tester", "version": "1"}})
}

fn tool_names(answer: &Value) -> Vec<String> {
    answer["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .filter_map(|tool| tool["name"].as_str().map(str::to_string))
        .collect()
}

#[test]
fn a_stdio_round_trip() {
    let host = TestHost::modern();
    let mcp = Arc::new(Mcp::new(false, None, true));
    let connection = Connection::new(mcp.clone(), host.clone())
        .with_progress_interval(Duration::from_millis(20));
    let mut client = Client::serve(connection);

    // initialize: no session id, the tool list may change (saved queries are tools).
    let answer = client.request(1, "initialize", initialize_params());
    assert_eq!(answer["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(
        answer["result"]["capabilities"],
        json!({"tools": {"listChanged": true}, "resources": {"listChanged": false}})
    );
    client.send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);

    // tools/list and tools/call.
    let answer = client.request(2, "tools/list", json!({}));
    let names = tool_names(&answer);
    assert!(names.contains(&"query".to_string()) && !names.contains(&"people".to_string()));
    let answer = client.request(
        3,
        "tools/call",
        json!({"name": "query", "arguments": {"script": "g.v().has_label(\"person\").values(\"name\")"}}),
    );
    let text = answer["result"]["content"][0]["text"]
        .as_str()
        .expect("text");
    assert!(
        text.ends_with("\n[\"marko\",\"vadas\",\"josh\",\"peter\"]"),
        "{text}"
    );

    // A malformed line, a batch and an empty line: errors with id null, the connection goes on.
    client.send("{\"jsonrpc\":");
    let error = client.next();
    assert_eq!(error["id"], Value::Null);
    assert_eq!(error["error"]["code"], -32700);
    client.send("");
    client.send(r#"[{"jsonrpc":"2.0","id":4,"method":"ping"}]"#);
    let error = client.next();
    assert_eq!(error["error"]["code"], -32600);
    assert_eq!(client.request(5, "ping", Value::Null)["result"], json!({}));

    // Defining a saved query changes the listing: list_changed comes after the call's answer.
    let answer = client.request(
        6,
        "tools/call",
        json!({"name": "query", "arguments": {"script":
            "g.define_query(#{name: \"people\", body: \"fn people() { g.v().has_label(\\\"person\\\").values(\\\"name\\\") }\"})"}}),
    );
    assert_eq!(answer["result"]["isError"], false, "{answer}");
    assert_eq!(
        client.next(),
        json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"})
    );
    let names = tool_names(&client.request(7, "tools/list", json!({})));
    assert!(names.contains(&"people".to_string()), "{names:?}");
    // A call that changes nothing announces nothing.
    client.request(
        8,
        "tools/call",
        json!({"name": "list_indexes", "arguments": {}}),
    );
    assert_eq!(client.request(9, "ping", Value::Null)["result"], json!({}));
    assert!(client.seen.is_empty(), "{:?}", client.seen);

    // A long call with a progress token: progress while it runs; a ping is answered meanwhile;
    // the client's cancel ends it.
    client.send(
        &json!({"jsonrpc": "2.0", "id": 41, "method": "tools/call", "params": {
            "name": "query", "arguments": {"script": ENDLESS},
            "_meta": {"progressToken": "p41"}}})
        .to_string(),
    );
    let mut progress = Vec::new();
    while progress.len() < 2 {
        let message = client.next();
        assert_eq!(message["method"], "notifications/progress", "{message}");
        progress.push(message);
    }
    assert_eq!(progress[0]["params"]["progressToken"], "p41");
    assert_eq!(progress[0]["params"]["progress"], 1);
    assert_eq!(progress[1]["params"]["progress"], 2);
    assert_eq!(client.request(42, "ping", Value::Null)["result"], json!({}));
    // A cancel of another id does nothing.
    client.send(
        r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":"41"}}"#,
    );
    assert_eq!(client.request(43, "ping", Value::Null)["result"], json!({}));
    client.send(r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":41,"reason":"test"}}"#);
    let answer = client.answer(&json!(41));
    assert_eq!(answer["result"]["isError"], true, "{answer}");
    let text = answer["result"]["content"][0]["text"]
        .as_str()
        .expect("text");
    assert!(text.contains("cancelled"), "{text}");
    assert!(
        client
            .seen
            .iter()
            .all(|message| message["method"] == "notifications/progress"),
        "{:?}",
        client.seen
    );

    // EOF ends the connection.
    let (result, rest) = client.close();
    assert!(result.is_ok(), "{result:?}");
    assert!(
        rest.iter()
            .all(|message| message["method"] == "notifications/progress"),
        "{rest:?}"
    );
    assert!(host.runs.lock().expect("runs").is_empty());
    // The stream's requests count in the endpoint's statistics.
    let stats = mcp.stats().to_json().expect("statistics");
    assert!(stats.to_string().contains("tester"), "{stats}");
}

#[test]
fn the_end_of_the_stream_cancels_running_requests() {
    let host = TestHost::modern();
    let mcp = Arc::new(Mcp::new(false, None, false));
    let mut client = Client::serve(Connection::new(mcp, host.clone()));
    client.request(1, "initialize", initialize_params());
    client.send(
        &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": {"name": "query", "arguments": {"script": ENDLESS}}})
        .to_string(),
    );
    // Wait until it runs, then close the input.
    while host.runs.lock().expect("runs").is_empty() {
        std::thread::sleep(Duration::from_millis(10));
    }
    let (result, rest) = client.close();
    assert!(result.is_ok(), "{result:?}");
    // Its answer is still written; no progress without a token.
    assert_eq!(rest.len(), 1, "{rest:?}");
    assert_eq!(rest[0]["id"], 2);
    let text = rest[0]["result"]["content"][0]["text"]
        .as_str()
        .expect("text");
    assert!(text.contains("cancelled"), "{text}");
}

#[test]
fn a_connection_keeps_what_initialize_negotiated() {
    let host = TestHost::modern();
    let mcp = Arc::new(Mcp::new(true, None, false));
    let connection = Connection::new(mcp, host);
    assert!(!connection.is_initialized());
    assert_eq!(connection.protocol_version(), None);
    assert_eq!(connection.key().len(), 32);
    // A notification has no answer; text that is no JSON is an error.
    assert!(
        connection
            .handle(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .is_empty()
    );
    let lines = connection.handle("nope");
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("-32700"), "{lines:?}");
    let lines = connection.handle(
        &json!({"jsonrpc": "2.0", "id": "a", "method": "initialize", "params": {
            "protocolVersion": "1999-01-01", "capabilities": {"sampling": {}},
            "clientInfo": {"name": "tester", "version": "2"}}})
        .to_string(),
    );
    assert_eq!(lines.len(), 1);
    let answer: Value = serde_json::from_str(&lines[0]).expect("JSON");
    assert_eq!(answer["id"], "a");
    assert_eq!(
        answer["result"]["protocolVersion"],
        graphersal_mcp::PROTOCOL_VERSIONS[0]
    );
    // Without saved-query tools the list cannot change.
    assert_eq!(
        answer["result"]["capabilities"]["tools"]["listChanged"],
        false
    );
    assert!(
        answer["result"]["instructions"]
            .as_str()
            .expect("text")
            .contains("READ-ONLY")
    );
    assert!(connection.is_initialized());
    assert_eq!(
        connection.protocol_version(),
        Some(graphersal_mcp::PROTOCOL_VERSIONS[0])
    );
    assert_eq!(
        connection.client_info(),
        Some(json!({"name": "tester", "version": "2"}))
    );
    assert_eq!(
        connection.client_capabilities(),
        Some(json!({"sampling": {}}))
    );
    assert!(format!("{connection:?}").starts_with("Connection {"));
}

/// The HTTP adapter and a stream connection answer the same calls with the same bytes (each
/// over its own host on its own modern graph, so the writes match too). Left out: `statistics`
/// (the process's resident memory changes between two calls) and the run time of queries.
#[test]
fn http_and_stdio_answer_the_same_bytes() {
    let http_host = TestHost::modern();
    let stream_host = TestHost::modern();
    let mcp = Arc::new(Mcp::new(false, None, true));
    let session = {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": initialize_params()})
        .to_string();
        let request = McpRequest::new("POST", body.as_bytes())
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream");
        let response = mcp.handle(http_host.as_ref(), request);
        assert_eq!(response.status(), 200);
        response
            .header("Mcp-Session-Id")
            .expect("a session")
            .to_string()
    };
    let mut client = Client::serve(Connection::new(mcp.clone(), stream_host));
    client.request(1, "initialize", initialize_params());
    let calls = [
        json!({"method": "tools/list"}),
        json!({"method": "tools/call", "params": {"name": "graph_info", "arguments": {}}}),
        json!({"method": "tools/call", "params": {"name": "get_schema", "arguments": {}}}),
        json!({"method": "tools/call", "params": {"name": "list_saved_queries", "arguments": {}}}),
        json!({"method": "tools/call", "params": {"name": "query", "arguments": {"script": "1 +"}}}),
        json!({"method": "tools/call", "params": {"name": "nope", "arguments": {}}}),
        json!({"method": "tools/call", "params": {"name": "query", "arguments": {"script":
            "g.define_query(#{name: \"people\", body: \"fn people() { g.v().has_label(\\\"person\\\").values(\\\"name\\\") }\"})"}}}),
        json!({"method": "tools/list"}),
        json!({"method": "tools/call", "params": {"name": "people", "arguments": {}}}),
        json!({"method": "tools/call", "params": {"name": "gql", "arguments": {
            "query": "MATCH (p:person) WHERE p.age > $min RETURN p.name AS n ORDER BY n",
            "params": {"min": 30}}}}),
        json!({"method": "resources/list"}),
        json!({"method": "resources/read", "params": {"uri": "graphersal://schema-summary"}}),
        json!({"method": "resources/read", "params": {"uri": "graphersal://nope"}}),
        json!({"method": "prompts/list"}),
        json!({"method": "nope"}),
    ];
    for (n, call) in calls.iter().enumerate() {
        let id = 100 + n as i64;
        let mut message = call.clone();
        message["jsonrpc"] = json!("2.0");
        message["id"] = json!(id);
        let body = message.to_string();
        let request = McpRequest::new("POST", body.as_bytes())
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .header("Mcp-Session-Id", &session)
            .header("MCP-Protocol-Version", "2025-06-18");
        let response = mcp.handle(http_host.as_ref(), request);
        assert_eq!(response.status(), 200, "{call}");
        let http = String::from_utf8(response.body().to_vec()).expect("UTF-8");
        client.send(&body);
        let stream = client.answer_text(&json!(id));
        assert_eq!(without_timing(&http), without_timing(&stream), "{call}");
    }
    let (result, _) = client.close();
    assert!(result.is_ok());
}

/// The answer with the run time of a query (`ok: 4 results (0.3 ms)`) left out.
fn without_timing(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find(" ms)") {
        let head = &rest[..at];
        match head.rfind('(') {
            Some(open) => {
                out.push_str(&head[..open]);
                out.push_str("(T");
            }
            None => out.push_str(head),
        }
        rest = &rest[at..];
        out.push_str(" ms)");
        rest = &rest[" ms)".len()..];
    }
    out.push_str(rest);
    out
}

/// A tool call over HTTP that changes the saved queries reaches a stream client that listed
/// the tools; HTTP's own `initialize` stays as it was (no push over HTTP).
#[test]
fn a_change_made_over_http_is_pushed_to_stream_clients() {
    let host = TestHost::modern();
    let mcp = Arc::new(Mcp::new(false, None, true));
    let post = |body: Value| {
        let body = body.to_string();
        let request = McpRequest::new("POST", body.as_bytes())
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream");
        mcp.handle(host.as_ref(), request)
    };
    let response = post(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": initialize_params()}));
    let answer: Value = serde_json::from_slice(response.body()).expect("JSON");
    assert_eq!(
        answer["result"]["capabilities"]["tools"]["listChanged"],
        false
    );

    let (outbound, pushed) = channel();
    let listening = Connection::new(mcp.clone(), host.clone()).with_outbound(outbound);
    let silent = Connection::new(mcp.clone(), host.clone());
    for connection in [&listening, &silent] {
        connection.handle(
            &json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": initialize_params()})
            .to_string(),
        );
    }
    // Only a connection that listed the tools hears of a change.
    listening.handle(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#);
    let response = post(
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {
        "name": "query", "arguments": {"script":
            "g.define_query(#{name: \"people\", body: \"fn people() { 1 }\"})"}}}),
    );
    assert_eq!(response.status(), 200);
    assert_eq!(
        pushed.recv_timeout(WAIT).expect("a push"),
        r#"{"jsonrpc":"2.0","method":"notifications/tools/list_changed"}"#
    );
    // Once told, a further change is announced again; a host's own change is found by
    // `check_tools_changed`.
    let define = "g.define_query(#{name: \"places\", body: \"fn places() { 2 }\"})";
    let changed = Arc::new(Mutex::new(None));
    let slot = changed.clone();
    host.execute(
        &ClientRef::new("browser", None),
        Box::new(move |session| {
            *slot.lock().expect("slot") =
                Some(session.execute(define, &graphersal_session::RunOptions::default()));
        }),
    )
    .expect("the job runs");
    assert!(changed.lock().expect("slot").is_some());
    mcp.check_tools_changed().expect("checked");
    assert_eq!(
        pushed.recv_timeout(WAIT).expect("a push"),
        r#"{"jsonrpc":"2.0","method":"notifications/tools/list_changed"}"#
    );
    // Nothing changed since: nothing more.
    mcp.check_tools_changed().expect("checked");
    assert!(pushed.recv_timeout(Duration::from_millis(200)).is_err());
    drop(silent);
}

#[test]
fn a_line_over_the_limit_is_refused_and_the_connection_goes_on() {
    let host = TestHost::modern();
    let mcp = Arc::new(Mcp::new(false, None, false));
    let connection = Connection::new(mcp, host);
    let mut input = vec![b' '; 16 * 1024 * 1024 + 1];
    input.extend_from_slice(b"\n\xff\xfe\n{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}");
    let mut output = Vec::new();
    serve_lines(&connection, std::io::Cursor::new(input), &mut output).expect("served");
    let lines: Vec<Value> = String::from_utf8(output)
        .expect("UTF-8")
        .lines()
        .map(|line| serde_json::from_str(line).expect("JSON"))
        .collect();
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert_eq!(lines[0]["error"]["code"], -32600);
    assert_eq!(
        lines[0]["error"]["message"],
        "the line is longer than 16777216 bytes"
    );
    assert_eq!(lines[1]["error"]["code"], -32700);
    assert_eq!(lines[1]["error"]["message"], "the line is not UTF-8");
    // The last line needs no line end.
    assert_eq!(lines[2], json!({"jsonrpc": "2.0", "id": 1, "result": {}}));
}
