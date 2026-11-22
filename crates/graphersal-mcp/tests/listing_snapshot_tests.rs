//! The single-graph endpoint's `initialize` instructions and `tools/list` stay exactly as they
//! were before directory mode existed (`Mcp::handle_directory`): a snapshot of both, for a
//! writable and a read-only endpoint. `GRAPHERSAL_UPDATE_MCP_SNAPSHOT=1` rewrites it (review the
//! diff: every agent sees it).

use std::path::Path;

use graphersal_mcp::{ClientRef, Executor, Host, Job, Mcp, McpError, McpRequest};
use serde_json::{Value, json};

/// A host that runs no job: the default listing reads no graph.
struct NoJobs;

impl Executor for NoJobs {
    fn execute(&self, _client: &ClientRef, _job: Job) -> Result<(), McpError> {
        Err(McpError::Unavailable("no session in this test".into()))
    }
}

impl Host for NoJobs {
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

fn post(mcp: &Mcp, body: &Value) -> Value {
    let text = body.to_string();
    let request = McpRequest::new("POST", text.as_bytes())
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream");
    let response = mcp.handle(&NoJobs, request);
    assert_eq!(response.status(), 200);
    serde_json::from_slice(response.body()).expect("a JSON body")
}

fn endpoint_listing(mcp: &Mcp) -> Value {
    let initialized = post(
        mcp,
        &json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": {"name": "snapshot", "version": "1"}}}),
    );
    let listed = post(
        mcp,
        &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    );
    json!({
        "instructions": initialized["result"]["instructions"],
        "tools": listed["result"]["tools"],
    })
}

#[test]
fn the_single_graph_listing_is_unchanged() {
    let now = json!({
        "writable": endpoint_listing(&Mcp::new(false, None, false)),
        "read_only": endpoint_listing(&Mcp::new(true, None, false)),
    });
    let text = serde_json::to_string_pretty(&now).expect("JSON") + "\n";
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("snapshots")
        .join("single_graph_listing.json");
    if std::env::var_os("GRAPHERSAL_UPDATE_MCP_SNAPSHOT").is_some() {
        std::fs::write(&path, &text).expect("the snapshot is written");
        return;
    }
    let before = std::fs::read_to_string(&path).expect("the snapshot exists");
    assert!(
        before == text,
        "the single-graph initialize/tools/list changed; rerun with \
         GRAPHERSAL_UPDATE_MCP_SNAPSHOT=1 only when that is intended"
    );
}
