//! The endpoint on a layered graph (layered-graph plan S5.4): `graph_info` carries the session's
//! `layer` block (base name and identity, change counts, own and shared bytes) only on a
//! layer, and `initialize` adds the note on the layer steps to the built-in instructions only
//! when the host serves a layer (`Host::serves_layer`): a plain graph's instructions stay as
//! they were, an operator's replacing text is answered as it is.

use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};

use graphersal::prelude::{Graph, TraversalGraph};
use graphersal::storage::LayerBase;
use graphersal_mcp::{ClientRef, Executor, Host, Job, Mcp, McpConfig, McpError, McpRequest};
use graphersal_session::Session;
use serde_json::{Value, json};

/// A host over one session on its own thread: the modern graph, or an empty layer over it
/// (the CLI's `--layer`). It knows which it serves without a job, as a host should.
struct TestHost {
    jobs: Mutex<Sender<Job>>,
    layer: bool,
}

impl TestHost {
    fn new(layer: bool) -> Self {
        let (jobs, queue) = channel::<Job>();
        std::thread::Builder::new()
            .stack_size(64 * 1024 * 1024)
            .spawn(move || {
                let mut session = Session::new();
                if layer {
                    let base =
                        LayerBase::from_graph(Graph::new(TraversalGraph::tinkerpop_modern()))
                            .expect("a base")
                            .with_name("modern");
                    let layer = base.new_layer().expect("a layer");
                    session
                        .load_shared(Arc::new(layer), "modern", "layer")
                        .expect("the layer loads");
                } else {
                    session
                        .load_sample("modern")
                        .expect("the modern graph loads");
                }
                for job in queue {
                    job(&mut session);
                }
            })
            .expect("the session thread starts");
        Self {
            jobs: Mutex::new(jobs),
            layer,
        }
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

    fn add_graph_info(&self, _info: &mut Value) -> Result<(), McpError> {
        Ok(())
    }

    fn examples_json(&self) -> Option<Vec<u8>> {
        None
    }

    fn serves_layer(&self) -> bool {
        self.layer
    }
}

fn post(mcp: &Mcp, host: &TestHost, body: &Value, session: Option<&str>) -> Value {
    let text = body.to_string();
    let mut request = McpRequest::new("POST", text.as_bytes())
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream");
    if let Some(session) = session {
        request = request
            .header("Mcp-Session-Id", session)
            .header("MCP-Protocol-Version", "2025-06-18");
    }
    let response = mcp.handle(host, request);
    assert_eq!(response.status(), 200);
    let mut answer: Value = serde_json::from_slice(response.body()).expect("a JSON body");
    if let Some(id) = response.header("mcp-session-id") {
        answer["session"] = json!(id);
    }
    answer
}

/// `initialize`: the instructions and the session id.
fn initialize(mcp: &Mcp, host: &TestHost) -> (String, String) {
    let answer = post(
        mcp,
        host,
        &json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": {"name": "tester", "version": "1"}}}),
        None,
    );
    (
        answer["result"]["instructions"]
            .as_str()
            .expect("instructions")
            .to_string(),
        answer["session"]
            .as_str()
            .expect("a session id")
            .to_string(),
    )
}

/// A tool's text answer.
fn call(mcp: &Mcp, host: &TestHost, session: &str, name: &str, args: Value) -> String {
    let answer = post(
        mcp,
        host,
        &json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call",
            "params": {"name": name, "arguments": args}}),
        Some(session),
    );
    assert_eq!(answer["result"]["isError"], false, "{answer}");
    answer["result"]["content"][0]["text"]
        .as_str()
        .expect("text")
        .to_string()
}

fn graph_info(mcp: &Mcp, host: &TestHost, session: &str) -> Value {
    serde_json::from_str(&call(mcp, host, session, "graph_info", json!({}))).expect("JSON")
}

const LAYER_STEPS: [&str; 3] = ["`layer()`", "`has_layer(", "`g.layer_diff()`"];

#[test]
fn a_layer_shows_its_block_and_the_layer_steps() {
    let host = TestHost::new(true);
    let mcp = Mcp::new(false, None, false);
    let (instructions, session) = initialize(&mcp, &host);
    assert!(instructions.starts_with(graphersal_mcp::instructions()));
    for step in LAYER_STEPS {
        assert!(instructions.contains(step), "{step} in {instructions}");
    }
    assert!(instructions.contains("This graph is a LAYER"));
    // The endpoint's own description of its instructions is the plain graph's.
    assert_eq!(mcp.instructions_text(), graphersal_mcp::instructions());

    let info = graph_info(&mcp, &host, &session);
    let layer = &info["layer"];
    assert_eq!(layer["base"], "modern", "{info}");
    assert!(layer["baseRef"]["graphId"].is_string(), "{info}");
    assert_eq!(layer["added"], json!({"vertices": 0, "edges": 0}));
    assert!(layer["sharedBytes"].as_u64().unwrap_or(0) > 0, "{info}");

    // A write goes to the layer: the counts follow, the layer steps answer.
    call(
        &mcp,
        &host,
        &session,
        "query",
        json!({"script": "g.add_v(\"project\").iterate(); g.v(1).property(\"age\", 30).iterate()"}),
    );
    let info = graph_info(&mcp, &host, &session);
    assert_eq!(info["layer"]["added"], json!({"vertices": 1, "edges": 0}));
    assert_eq!(
        info["layer"]["modified"],
        json!({"vertices": 1, "edges": 0})
    );
    assert!(
        info["layer"]["ownBytes"].as_u64().unwrap_or(0) > 0,
        "{info}"
    );
    let text = call(
        &mcp,
        &host,
        &session,
        "query",
        json!({"script": "g.v().has_layer(\"added\", \"modified\").layer().to_list()"}),
    );
    assert!(text.ends_with("[\"modified\",\"added\"]"), "{text}");
}

#[test]
fn a_plain_graph_keeps_its_instructions_and_info() {
    let host = TestHost::new(false);
    let mcp = Mcp::new(false, None, false);
    let (instructions, session) = initialize(&mcp, &host);
    assert_eq!(instructions, graphersal_mcp::instructions());
    for step in LAYER_STEPS {
        assert!(!instructions.contains(step), "{step}");
    }
    let info = graph_info(&mcp, &host, &session);
    assert!(info.get("layer").is_none(), "{info}");
    assert_eq!(info["vertexCount"], 6);
}

/// The layer note comes before the read-only note and the operator's notes; an operator's
/// replacing text is answered as it is.
#[test]
fn the_layer_note_keeps_the_order_and_the_operators_text() {
    let host = TestHost::new(true);
    let mcp = Mcp::new(true, None, false).with_operator_notes(Some("Be kind.".into()));
    let (instructions, _) = initialize(&mcp, &host);
    let layer = instructions
        .find("This graph is a LAYER")
        .expect("layer note");
    let read_only = instructions.find("READ-ONLY").expect("read-only note");
    let notes = instructions.find("Be kind.").expect("operator notes");
    assert!(layer < read_only && read_only < notes, "{instructions}");

    let config = McpConfig::parse(
        r#"{"instructions": {"mode": "replace", "text": "Only this."}}"#,
        &mut |_| Err("no files".into()),
    )
    .expect("a configuration");
    let mcp = Mcp::new(false, None, false).with_config(config);
    let (instructions, _) = initialize(&mcp, &host);
    assert_eq!(instructions, "Only this.");
}
