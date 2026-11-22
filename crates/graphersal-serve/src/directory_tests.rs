//! One MCP endpoint over two served contexts (`graphersal_mcp::Mcp::handle_directory`, plan
//! `graphersal-server` H4, decision Q4): the test host's registry implements
//! `graphersal_mcp::GraphDirectory` over real [`Context`]s the way a multi-graph host does
//! (each context is the `Host` of its graph, its MCP profile the graph's profile), with real
//! session threads on the `modern` sample and the runtime read-only toggle of one of them.

use std::collections::BTreeMap;
use std::io::Cursor;
use std::sync::{Arc, Mutex};

use graphersal_mcp::{
    GraphDirectory, GraphEntry, GraphState, Host, Mcp, McpError, McpRequest, McpResponse, Profile,
};
use graphersal_session::SessionPresets;
use serde_json::{Value, json};

use crate::{Context, Exposure, HostCheck, Runner, spawn_session};

fn check() -> HostCheck {
    HostCheck::new(Exposure::Loopback, 8080, "http://127.0.0.1:8080/")
}

fn modern() -> Runner {
    let (jobs, ()) = spawn_session(
        SessionPresets::new(),
        |session| {
            session
                .load_sample("modern")
                .map(|_| ())
                .map_err(|err| err.to_string())
        },
        None,
    )
    .expect("the session thread starts");
    Runner::Session(jobs)
}

/// The host's registry: contexts by name (unloaded = `None`) and aliases.
struct Registry {
    contexts: BTreeMap<String, Option<Arc<Context>>>,
    aliases: Mutex<BTreeMap<String, String>>,
}

impl GraphDirectory for Registry {
    fn list(&self) -> Result<Vec<GraphEntry>, McpError> {
        let mut entries: Vec<GraphEntry> =
            self.contexts
                .iter()
                .map(|(name, context)| match context {
                    Some(context) => GraphEntry::new(name, GraphState::Loaded)
                        .with_read_only(context.is_read_only()),
                    None => GraphEntry::new(name, GraphState::Unloaded),
                })
                .collect();
        let aliases = self
            .aliases
            .lock()
            .map_err(|_| McpError::Failed("poisoned".into()))?;
        entries.extend(aliases.iter().map(|(alias, target)| {
            GraphEntry::new(alias, GraphState::Loaded).with_alias_of(target)
        }));
        Ok(entries)
    }

    fn resolve(&self, name: &str) -> Result<String, McpError> {
        let aliases = self
            .aliases
            .lock()
            .map_err(|_| McpError::Failed("poisoned".into()))?;
        match aliases.get(name) {
            Some(target) => Ok(target.clone()),
            None if self.contexts.contains_key(name) => Ok(name.to_string()),
            None => Err(McpError::Failed(format!("no database '{name}'"))),
        }
    }

    fn host(&self, route: &str) -> Result<(Arc<dyn Host>, Profile), McpError> {
        match self.contexts.get(route) {
            Some(Some(context)) => {
                let profile = context.mcp_profile().cloned().unwrap_or_default();
                let host: Arc<dyn Host> = context.clone();
                Ok((host, profile))
            }
            Some(None) => Err(McpError::Unavailable(format!(
                "database '{route}' is not loaded; ask an operator to load it"
            ))),
            None => Err(McpError::Failed(format!("no database '{route}'"))),
        }
    }
}

fn post(mcp: &Mcp, registry: &Registry, body: Value, session: Option<&str>) -> McpResponse {
    let mut request = McpRequest::new("POST", Cursor::new(body.to_string().into_bytes()))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream");
    if let Some(session) = session {
        request = request.header("Mcp-Session-Id", session);
    }
    mcp.handle_directory(registry, request)
}

fn tool(mcp: &Mcp, registry: &Registry, session: &str, name: &str, args: Value) -> (String, bool) {
    let response = post(
        mcp,
        registry,
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {"name": name, "arguments": args}}),
        Some(session),
    );
    assert_eq!(response.status(), 200);
    let answer: Value = serde_json::from_slice(response.body()).expect("JSON");
    let result = &answer["result"];
    (
        result["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        result["isError"].as_bool().unwrap_or(true),
    )
}

fn persons(mcp: &Mcp, registry: &Registry, session: &str, graph: Option<&str>) -> String {
    let mut args = json!({"query": "MATCH (p:person) RETURN count(p) AS n"});
    if let Some(graph) = graph {
        args["graph"] = json!(graph);
    }
    let (text, is_error) = tool(mcp, registry, session, "gql", args);
    assert!(!is_error, "{text}");
    let at = text.find("\"n\":").expect("a count") + 4;
    text[at..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect()
}

#[test]
fn one_endpoint_serves_two_contexts() {
    let mcp = Arc::new(Mcp::new(false, None, false).with_endpoint("/mcp"));
    let a = Context::builder(modern(), check())
        .shared_mcp(mcp.clone())
        .runtime_read_only(None)
        .build();
    let b = Context::builder(modern(), check())
        .shared_mcp(mcp.clone())
        .build();
    let registry = Registry {
        contexts: BTreeMap::from([
            ("a".to_string(), Some(a.clone())),
            ("b".to_string(), Some(b)),
            ("c".to_string(), None),
        ]),
        aliases: Mutex::new(BTreeMap::from([("prod".to_string(), "a".to_string())])),
    };
    let response = post(
        &mcp,
        &registry,
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "clientInfo": {"name": "t", "version": "1"}}}),
        None,
    );
    let session = response
        .header("mcp-session-id")
        .expect("an id")
        .to_string();

    let (text, is_error) = tool(
        &mcp,
        &registry,
        &session,
        "use_graph",
        json!({"name": "prod"}),
    );
    assert!(!is_error && text.contains("\"selected\":\"a\""), "{text}");
    let (text, is_error) = tool(
        &mcp,
        &registry,
        &session,
        "gql",
        json!({"query": "INSERT (:person {name: 'zed'})"}),
    );
    assert!(!is_error, "{text}");
    assert_eq!(persons(&mcp, &registry, &session, None), "5");
    assert_eq!(persons(&mcp, &registry, &session, Some("b")), "4");

    // Alias flip: the open session stays on a.
    registry
        .aliases
        .lock()
        .unwrap()
        .insert("prod".into(), "b".into());
    assert_eq!(persons(&mcp, &registry, &session, None), "5");

    // The runtime read-only toggle of a refuses the write; b takes it.
    a.set_read_only(true).expect("read-only turns on");
    let insert = json!({"query": "INSERT (:person {name: 'yan'})"});
    let (text, is_error) = tool(&mcp, &registry, &session, "gql", insert.clone());
    assert!(is_error, "{text}");
    let mut on_b = insert;
    on_b["graph"] = json!("b");
    let (text, is_error) = tool(&mcp, &registry, &session, "gql", on_b);
    assert!(!is_error, "{text}");
    assert_eq!(persons(&mcp, &registry, &session, Some("b")), "5");
    let (text, _) = tool(&mcp, &registry, &session, "list_graphs", json!({}));
    assert!(
        text.contains("{\"name\":\"a\",\"kind\":\"plain\",\"state\":\"loaded\",\"readOnly\":true}"),
        "{text}"
    );

    // An unloaded graph: the host's 409-style text.
    let (text, is_error) = tool(&mcp, &registry, &session, "use_graph", json!({"name": "c"}));
    assert!(
        is_error && text.contains("not loaded; ask an operator"),
        "{text}"
    );
}
