//! Directory mode (`Mcp::handle_directory`): one endpoint over several graphs, each a test host
//! with its own session thread; the directory is the host's registry (names, aliases, states,
//! per-graph profiles), changed by the test while sessions are open.

use std::collections::BTreeMap;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};

use graphersal_mcp::{
    ClientRef, Executor, GraphDirectory, GraphEntry, GraphKind, GraphState, Host, Job, Mcp,
    McpError, McpRequest, McpResponse, Profile, SessionKey,
};
use graphersal_session::Session;
use serde_json::{Value, json};

/// A host over one session on its own thread (the modern graph).
struct TestHost {
    jobs: Mutex<Sender<Job>>,
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

    fn add_graph_info(&self, _info: &mut Value) -> Result<(), McpError> {
        Ok(())
    }

    fn examples_json(&self) -> Option<Vec<u8>> {
        None
    }
}

/// One graph of the registry: its host when loaded, its profile.
struct Graph {
    host: Option<Arc<TestHost>>,
    read_only: bool,
    kind: GraphKind,
}

/// The test registry: graphs and aliases, changed while sessions are open.
#[derive(Default)]
struct Registry {
    graphs: Mutex<BTreeMap<String, Graph>>,
    aliases: Mutex<BTreeMap<String, String>>,
}

impl Registry {
    /// `a` and `b` loaded, `c` unloaded, `r` loaded and read-only, `prod` an alias of `a`.
    fn standard() -> Self {
        let registry = Self::default();
        {
            let mut graphs = registry.graphs.lock().unwrap();
            for (name, loaded, read_only, kind) in [
                ("a", true, false, GraphKind::Plain),
                ("b", true, false, GraphKind::Layer),
                ("c", false, false, GraphKind::Plain),
                ("r", true, true, GraphKind::Base),
            ] {
                graphs.insert(
                    name.to_string(),
                    Graph {
                        host: loaded.then(TestHost::modern),
                        read_only,
                        kind,
                    },
                );
            }
        }
        registry.set_alias("prod", "a");
        registry
    }

    fn remove_alias(&self, alias: &str) {
        self.aliases.lock().unwrap().remove(alias);
    }

    /// Renames an alias the way a host does: the registry, then the endpoint's selections.
    fn rename_alias(&self, mcp: &Mcp, old: &str, new: &str) -> usize {
        let target = self.aliases.lock().unwrap().remove(old).unwrap();
        self.set_alias(new, &target);
        mcp.rename_selection(old, new)
    }

    fn set_alias(&self, alias: &str, target: &str) {
        self.aliases
            .lock()
            .unwrap()
            .insert(alias.to_string(), target.to_string());
    }
}

impl GraphDirectory for Registry {
    fn list(&self) -> Result<Vec<GraphEntry>, McpError> {
        let graphs = self.graphs.lock().unwrap();
        let state = |graph: &Graph| {
            if graph.host.is_some() {
                GraphState::Loaded
            } else {
                GraphState::Unloaded
            }
        };
        let mut entries: Vec<GraphEntry> = graphs
            .iter()
            .map(|(name, graph)| {
                GraphEntry::new(name, state(graph))
                    .with_kind(graph.kind)
                    .with_read_only(graph.read_only)
            })
            .collect();
        for (alias, target) in self.aliases.lock().unwrap().iter() {
            let graph = &graphs[target];
            entries.push(GraphEntry::new(alias, state(graph)).with_alias_of(target));
        }
        entries.push(GraphEntry::new(
            "broken",
            GraphState::Failed("snapshot is damaged".into()),
        ));
        Ok(entries)
    }

    fn resolve(&self, name: &str) -> Result<String, McpError> {
        if let Some(target) = self.aliases.lock().unwrap().get(name) {
            return Ok(target.clone());
        }
        if self.graphs.lock().unwrap().contains_key(name) {
            return Ok(name.to_string());
        }
        Err(McpError::Failed(format!("no database '{name}'")))
    }

    fn host(&self, route: &str) -> Result<(Arc<dyn Host>, Profile), McpError> {
        let graphs = self.graphs.lock().unwrap();
        let graph = graphs
            .get(route)
            .ok_or_else(|| McpError::Failed(format!("no database '{route}'")))?;
        let host = graph.host.clone().ok_or_else(|| {
            McpError::Unavailable(format!(
                "database '{route}' is not loaded; ask an operator to load it"
            ))
        })?;
        Ok((host, Profile::new(graph.read_only)))
    }
}

fn request(body: &Value, session: Option<&str>) -> McpRequest<'static> {
    let text = std::io::Cursor::new(body.to_string().into_bytes());
    let request = McpRequest::new("POST", text)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream");
    match session {
        Some(id) => request
            .header("Mcp-Session-Id", id)
            .header("MCP-Protocol-Version", "2025-06-18"),
        None => request,
    }
}

fn json_of(response: &McpResponse) -> Value {
    serde_json::from_slice(response.body()).expect("a JSON body")
}

fn initialize(mcp: &Mcp, registry: &Registry) -> (String, Value) {
    let response = mcp.handle_directory(
        registry,
        request(
            &json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
                "protocolVersion": "2025-06-18", "capabilities": {},
                "clientInfo": {"name": "tester", "version": "1"}}}),
            None,
        ),
    );
    assert_eq!(response.status(), 200);
    let id = response
        .header("mcp-session-id")
        .expect("a session id")
        .to_string();
    (id, json_of(&response)["result"].clone())
}

/// `tools/call`: `(text, isError)`.
fn call(mcp: &Mcp, registry: &Registry, session: &str, name: &str, args: Value) -> (String, bool) {
    let response = mcp.handle_directory(
        registry,
        request(
            &json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call",
                "params": {"name": name, "arguments": args}}),
            Some(session),
        ),
    );
    assert_eq!(response.status(), 200);
    let answer = json_of(&response);
    let result = &answer["result"];
    assert!(result.is_object(), "{answer}");
    (
        result["content"][0]["text"]
            .as_str()
            .expect("text")
            .to_string(),
        result["isError"].as_bool().expect("isError"),
    )
}

/// The number of `person` vertices of the graph the call reaches.
fn persons(mcp: &Mcp, registry: &Registry, session: &str, graph: Option<&str>) -> u64 {
    let mut args = json!({"query": "MATCH (p:person) RETURN count(p) AS n"});
    if let Some(graph) = graph {
        args["graph"] = json!(graph);
    }
    let (text, is_error) = call(mcp, registry, session, "gql", args);
    assert!(!is_error, "{text}");
    let at = text.find("\"n\":").expect("a count") + 4;
    text[at..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .expect("a number")
}

fn insert_person(mcp: &Mcp, registry: &Registry, session: &str) -> (String, bool) {
    call(
        mcp,
        registry,
        session,
        "gql",
        json!({"query": "INSERT (:person {name: 'zed'})"}),
    )
}

#[test]
fn the_directory_lists_its_tools_and_instructions() {
    let registry = Registry::standard();
    // A default profile with saved-query tools: none of them is listed in directory mode.
    let mcp = Mcp::new(false, None, true);
    let (_, initialized) = initialize(&mcp, &registry);
    let instructions = initialized["instructions"].as_str().unwrap();
    assert!(instructions.starts_with("This MCP endpoint serves SEVERAL graphs."));
    assert!(instructions.contains("after a reconnect"));
    assert!(instructions.contains("`list_graphs`") && instructions.contains("`use_graph`"));
    let listed = json_of(&mcp.handle_directory(
        &registry,
        request(
            &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
            None,
        ),
    ));
    let tools = listed["result"]["tools"].as_array().unwrap();
    assert_eq!(tools[0]["name"], "list_graphs");
    assert_eq!(tools[1]["name"], "use_graph");
    // Every data tool takes an optional `graph`; the default profile's built-in tools follow.
    let single = Mcp::new(false, None, false)
        .tool_listings(TestHost::modern().as_ref())
        .unwrap();
    assert_eq!(tools.len(), single.len() + 2);
    for (tool, plain) in tools[2..].iter().zip(&single) {
        assert_eq!(tool["name"], plain["name"]);
        assert_eq!(tool["inputSchema"]["properties"]["graph"]["type"], "string");
        let required = tool["inputSchema"]["required"].as_array();
        assert!(!required.is_some_and(|r| r.contains(&json!("graph"))));
    }
    assert_eq!(mcp.directory_tool_listings(), tools.clone());
    // The status names the host's path.
    let mcp = Mcp::new(false, None, false).with_endpoint("/agents/mcp");
    assert_eq!(mcp.status()["endpoint"], "/agents/mcp");
    assert_eq!(Mcp::new(false, None, false).status()["endpoint"], "/mcp");
}

#[test]
fn select_write_switch_and_override_per_call() {
    let registry = Registry::standard();
    let mcp = Mcp::new(false, None, false);
    let (session, _) = initialize(&mcp, &registry);

    // list_graphs with states, kinds, aliases and the failure reason.
    let (text, is_error) = call(&mcp, &registry, &session, "list_graphs", json!({}));
    assert!(!is_error, "{text}");
    let listed: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(listed["selected"], Value::Null);
    let by_name = |name: &str| {
        listed["graphs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|g| g["name"] == name)
            .cloned()
            .unwrap()
    };
    assert_eq!(by_name("a")["state"], "loaded");
    assert_eq!(by_name("b")["kind"], "layer");
    assert_eq!(by_name("c")["state"], "unloaded");
    assert_eq!(by_name("r")["readOnly"], true);
    assert_eq!(by_name("prod")["aliasOf"], "a");
    assert_eq!(by_name("broken")["state"], "failed");
    assert_eq!(by_name("broken")["reason"], "snapshot is damaged");
    assert!(text.starts_with("{\"graphs\":[{\"name\":\"a\",\"kind\":\"plain\""));

    // use_graph a, write and read there.
    let (text, is_error) = call(&mcp, &registry, &session, "use_graph", json!({"name": "a"}));
    assert!(!is_error, "{text}");
    let selected: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(selected["selected"], "a");
    assert_eq!(selected["state"], "loaded");
    assert_eq!(selected["readOnly"], false);
    let (text, is_error) = insert_person(&mcp, &registry, &session);
    assert!(!is_error, "{text}");
    assert_eq!(persons(&mcp, &registry, &session, None), 5);

    // Switch to b: the write stayed in a.
    let (text, is_error) = call(&mcp, &registry, &session, "use_graph", json!({"name": "b"}));
    assert!(!is_error, "{text}");
    assert_eq!(persons(&mcp, &registry, &session, None), 4);
    // `graph` reaches a for one call; the selection stays b.
    assert_eq!(persons(&mcp, &registry, &session, Some("a")), 5);
    assert_eq!(persons(&mcp, &registry, &session, None), 4);
    let (text, _) = call(&mcp, &registry, &session, "list_graphs", json!({}));
    assert!(
        text.ends_with("\"selected\":\"b\",\"selectedGraph\":\"b\"}"),
        "{text}"
    );
    // A `graph` that names nothing, or is no string.
    let (text, is_error) = call(
        &mcp,
        &registry,
        &session,
        "statistics",
        json!({"graph": "nope"}),
    );
    assert!(is_error && text.contains("no database 'nope'") && text.contains("list_graphs"));
    let (text, is_error) = call(&mcp, &registry, &session, "statistics", json!({"graph": 3}));
    assert!(
        is_error && text.contains("must be the name of a graph"),
        "{text}"
    );

    // A resource reads the selected graph (or the one `graph` names).
    let read = |graph: Option<&str>| {
        let mut params = json!({"uri": "graphersal://statistics"});
        if let Some(graph) = graph {
            params["graph"] = json!(graph);
        }
        json_of(&mcp.handle_directory(
            &registry,
            request(
                &json!({"jsonrpc": "2.0", "id": 9, "method": "resources/read", "params": params}),
                Some(&session),
            ),
        ))
    };
    let on_b = read(None);
    let on_a = read(Some("a"));
    assert!(on_b["result"]["contents"][0]["text"].is_string(), "{on_b}");
    assert_ne!(
        on_b["result"]["contents"][0]["text"],
        on_a["result"]["contents"][0]["text"]
    );

    // Saved queries are not tools of their own here.
    let response = mcp.handle_directory(
        &registry,
        request(
            &json!({"jsonrpc": "2.0", "id": 8, "method": "tools/call",
                "params": {"name": "my_saved_query", "arguments": {}}}),
            Some(&session),
        ),
    );
    let answer = json_of(&response);
    assert_eq!(answer["error"]["code"], -32602);
    assert!(
        answer["error"]["message"]
            .as_str()
            .unwrap()
            .contains("run_saved_query")
    );
}

#[test]
fn without_a_selection_the_error_names_list_graphs_and_use_graph() {
    let registry = Registry::standard();
    let mcp = Mcp::new(false, None, false);
    let (session, _) = initialize(&mcp, &registry);
    let (text, is_error) = call(&mcp, &registry, &session, "statistics", json!({}));
    assert!(is_error);
    assert!(
        text.contains("list_graphs") && text.contains("use_graph"),
        "{text}"
    );
    assert!(text.contains("reconnect"), "{text}");

    // A reconnect is a new session: the selection of the old one does not carry over.
    let (text, is_error) = call(&mcp, &registry, &session, "use_graph", json!({"name": "a"}));
    assert!(!is_error, "{text}");
    assert_eq!(persons(&mcp, &registry, &session, None), 4);
    let (again, _) = initialize(&mcp, &registry);
    let (text, is_error) = call(&mcp, &registry, &again, "graph_info", json!({}));
    assert!(is_error && text.contains("use_graph") && text.contains("reconnect"));
    // A resource needs a graph too, except the GQL reference.
    let answer = json_of(&mcp.handle_directory(
        &registry,
        request(
            &json!({"jsonrpc": "2.0", "id": 3, "method": "resources/read",
                "params": {"uri": "graphersal://schema"}}),
            Some(&again),
        ),
    ));
    assert!(
        answer["error"]["message"]
            .as_str()
            .unwrap()
            .contains("use_graph")
    );
    let answer = json_of(&mcp.handle_directory(
        &registry,
        request(
            &json!({"jsonrpc": "2.0", "id": 3, "method": "resources/read",
                "params": {"uri": "graphersal://gql-reference"}}),
            Some(&again),
        ),
    ));
    assert!(answer["result"]["contents"][0]["text"].is_string());

    // A request without a session has no selection to keep.
    let response = mcp.handle_directory(
        &registry,
        request(
            &json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call",
                "params": {"name": "use_graph", "arguments": {"name": "a"}}}),
            None,
        ),
    );
    let text = json_of(&response)["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(text.contains("no Mcp-Session-Id"), "{text}");
}

#[test]
fn an_alias_flip_moves_an_open_session_with_its_name() {
    let registry = Registry::standard();
    let mcp = Mcp::new(false, None, false);
    let (old, _) = initialize(&mcp, &registry);
    let (text, is_error) = call(&mcp, &registry, &old, "use_graph", json!({"name": "prod"}));
    assert!(!is_error, "{text}");
    let selected: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(selected["selected"], "prod");
    assert_eq!(selected["graph"], "a");
    let (text, is_error) = insert_person(&mcp, &registry, &old);
    assert!(!is_error, "{text}");

    // The operator moves `prod` to b: the open session follows it, without a note.
    registry.set_alias("prod", "b");
    let (text, is_error) = call(&mcp, &registry, &old, "graph_info", json!({}));
    assert!(!is_error && !text.starts_with("note:"), "{text}");
    assert_eq!(persons(&mcp, &registry, &old, None), 4);
    let (text, _) = call(&mcp, &registry, &old, "list_graphs", json!({}));
    assert!(
        text.ends_with("\"selected\":\"prod\",\"selectedGraph\":\"b\"}"),
        "{text}"
    );
    // A new session selecting `prod` gets b too; `graph` reaches a for one call.
    let (new, _) = initialize(&mcp, &registry);
    let (text, _) = call(&mcp, &registry, &new, "use_graph", json!({"name": "prod"}));
    assert!(
        text.contains("\"selected\":\"prod\",\"graph\":\"b\""),
        "{text}"
    );
    assert_eq!(persons(&mcp, &registry, &new, None), 4);
    assert_eq!(persons(&mcp, &registry, &old, Some("a")), 5);
    // Back to a: followed again.
    registry.set_alias("prod", "a");
    assert_eq!(persons(&mcp, &registry, &old, None), 5);
}

#[test]
fn an_unloaded_graph_is_refused_and_the_selection_stays() {
    let registry = Registry::standard();
    let mcp = Mcp::new(false, None, false);
    let (session, _) = initialize(&mcp, &registry);
    call(&mcp, &registry, &session, "use_graph", json!({"name": "b"}));
    let (text, is_error) = call(&mcp, &registry, &session, "use_graph", json!({"name": "c"}));
    assert!(is_error);
    assert_eq!(
        text,
        "database 'c' is not loaded; ask an operator to load it"
    );
    let (text, _) = call(&mcp, &registry, &session, "list_graphs", json!({}));
    assert!(
        text.ends_with("\"selected\":\"b\",\"selectedGraph\":\"b\"}"),
        "{text}"
    );
    // Per call too.
    let (text, is_error) = call(
        &mcp,
        &registry,
        &session,
        "graph_info",
        json!({"graph": "c"}),
    );
    assert!(
        is_error && text.contains("not loaded; ask an operator"),
        "{text}"
    );
    // Unloading the selected graph refuses its calls, with the same text.
    registry.graphs.lock().unwrap().get_mut("b").unwrap().host = None;
    let (text, is_error) = call(&mcp, &registry, &session, "graph_info", json!({}));
    assert!(
        is_error && text.contains("database 'b' is not loaded"),
        "{text}"
    );
}

#[test]
fn a_read_only_graph_refuses_writes() {
    let registry = Registry::standard();
    let mcp = Mcp::new(false, None, false);
    let (session, _) = initialize(&mcp, &registry);
    let (text, _) = call(&mcp, &registry, &session, "use_graph", json!({"name": "r"}));
    assert!(text.contains("\"readOnly\":true"), "{text}");
    let (text, is_error) = insert_person(&mcp, &registry, &session);
    assert!(is_error, "{text}");
    assert_eq!(persons(&mcp, &registry, &session, None), 4);
    // A writing tool is refused by the graph's profile, naming the graph.
    let response = mcp.handle_directory(
        &registry,
        request(
            &json!({"jsonrpc": "2.0", "id": 5, "method": "tools/call",
                "params": {"name": "create_index", "arguments": {"label": "person", "key": "name"}}}),
            Some(&session),
        ),
    );
    let message = json_of(&response)["error"]["message"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(message.contains("graph \"r\" is read-only"), "{message}");
    // The same write reaches a writable graph by `graph`.
    let (text, is_error) = call(
        &mcp,
        &registry,
        &session,
        "gql",
        json!({"query": "INSERT (:person {name: 'zed'})", "graph": "a"}),
    );
    assert!(!is_error, "{text}");
    assert_eq!(persons(&mcp, &registry, &session, Some("a")), 5);
}

#[test]
fn directory_mode_refuses_signed_sessions() {
    let registry = Registry::standard();
    let mcp = Mcp::new(false, None, false).with_session_key(SessionKey::random());
    let response = mcp.handle_directory(
        &registry,
        request(
            &json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
            None,
        ),
    );
    assert_eq!(response.status(), 500);
    let message = json_of(&response)["error"]["message"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(message.contains("signed session ids"), "{message}");
}

#[test]
fn a_forgotten_session_is_404_and_starts_again_without_a_selection() {
    let registry = Registry::standard();
    let mcp = Mcp::new(false, None, false).with_max_sessions(1);
    let (first, _) = initialize(&mcp, &registry);
    call(&mcp, &registry, &first, "use_graph", json!({"name": "a"}));
    let (second, _) = initialize(&mcp, &registry);
    let response = mcp.handle_directory(
        &registry,
        request(
            &json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call",
                "params": {"name": "graph_info", "arguments": {}}}),
            Some(&first),
        ),
    );
    assert_eq!(response.status(), 404);
    let (text, is_error) = call(&mcp, &registry, &second, "graph_info", json!({}));
    assert!(is_error && text.contains("use_graph"));
    // The token applies like on a single-graph endpoint; DELETE ends a session.
    let mcp = Mcp::new(false, Some("secret".into()), false);
    let response = mcp.handle_directory(
        &registry,
        request(&json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}), None),
    );
    assert_eq!(response.status(), 401);
    let mcp = Mcp::new(false, None, false);
    let (session, _) = initialize(&mcp, &registry);
    let delete = McpRequest::new("DELETE", &b""[..]).header("Mcp-Session-Id", &session);
    assert_eq!(mcp.handle_directory(&registry, delete).status(), 200);
}

#[test]
fn a_removed_alias_fails_only_while_its_name_is_unknown() {
    let registry = Registry::standard();
    let mcp = Mcp::new(false, None, false);
    let (session, _) = initialize(&mcp, &registry);
    call(
        &mcp,
        &registry,
        &session,
        "use_graph",
        json!({"name": "prod"}),
    );
    registry.remove_alias("prod");
    let (text, is_error) = call(&mcp, &registry, &session, "graph_info", json!({}));
    assert!(is_error, "{text}");
    assert!(
        text.contains("pick another graph: list_graphs, then use_graph")
            && text.contains("\"prod\""),
        "{text}"
    );
    // The selection stays: list_graphs shows the name without a graph.
    let (text, _) = call(&mcp, &registry, &session, "list_graphs", json!({}));
    assert!(
        text.ends_with("\"selected\":\"prod\",\"selectedGraph\":null}"),
        "{text}"
    );
    // Another call names its graph and works; the selection is still the missing name.
    assert_eq!(persons(&mcp, &registry, &session, Some("b")), 4);
    let (text, is_error) = call(&mcp, &registry, &session, "statistics", json!({}));
    assert!(is_error && text.contains("pick another graph"), "{text}");
    // The alias is created again (on another graph): the session works on, no tombstone.
    registry.set_alias("prod", "b");
    let (text, is_error) = insert_person(&mcp, &registry, &session);
    assert!(!is_error && !text.starts_with("note:"), "{text}");
    assert_eq!(persons(&mcp, &registry, &session, Some("b")), 5);
    assert_eq!(persons(&mcp, &registry, &session, Some("a")), 4);
    // A resource follows the same rule.
    registry.remove_alias("prod");
    let answer = json_of(&mcp.handle_directory(
        &registry,
        request(
            &json!({"jsonrpc": "2.0", "id": 3, "method": "resources/read",
                "params": {"uri": "graphersal://schema"}}),
            Some(&session),
        ),
    ));
    assert!(
        answer["error"]["message"]
            .as_str()
            .unwrap()
            .contains("pick another graph"),
        "{answer}"
    );
}

#[test]
fn a_renamed_alias_moves_the_sessions_with_one_note() {
    let registry = Registry::standard();
    let mcp = Mcp::new(false, None, false);
    let (session, _) = initialize(&mcp, &registry);
    let (on_a, _) = initialize(&mcp, &registry);
    // The name is spelled as the directory lists it.
    let (text, is_error) = call(
        &mcp,
        &registry,
        &session,
        "use_graph",
        json!({"name": "PROD"}),
    );
    assert!(!is_error, "{text}");
    assert!(
        text.starts_with("{\"selected\":\"prod\",\"graph\":\"a\""),
        "{text}"
    );
    call(&mcp, &registry, &on_a, "use_graph", json!({"name": "a"}));

    // One rename: the next answer carries the note once.
    assert_eq!(registry.rename_alias(&mcp, "prod", "live"), 1);
    let (text, is_error) = call(&mcp, &registry, &session, "graph_info", json!({}));
    assert!(!is_error, "{text}");
    assert!(
        text.starts_with("note: alias \"prod\" was renamed to \"live\"\n"),
        "{text}"
    );
    let (text, _) = call(&mcp, &registry, &session, "graph_info", json!({}));
    assert!(!text.contains("renamed"), "{text}");
    let (text, _) = call(&mcp, &registry, &session, "list_graphs", json!({}));
    assert!(
        text.ends_with("\"selected\":\"live\",\"selectedGraph\":\"a\"}"),
        "{text}"
    );

    // Two renames before the next call: one note, first name -> current name.
    assert_eq!(registry.rename_alias(&mcp, "live", "main"), 1);
    assert_eq!(registry.rename_alias(&mcp, "main", "stable"), 1);
    let (text, is_error) = insert_person(&mcp, &registry, &session);
    assert!(!is_error, "{text}");
    let notes = text.matches("note: alias").count();
    assert_eq!(notes, 1, "{text}");
    assert!(
        text.starts_with("note: alias \"live\" was renamed to \"stable\"\n"),
        "{text}"
    );
    assert_eq!(persons(&mcp, &registry, &session, None), 5);
    // The note rides on an error answer too.
    assert_eq!(registry.rename_alias(&mcp, "stable", "prod"), 1);
    let (text, is_error) = call(
        &mcp,
        &registry,
        &session,
        "statistics",
        json!({"graph": "nope"}),
    );
    assert!(is_error, "{text}");
    assert!(
        text.starts_with("note: alias \"stable\" was renamed to \"prod\"\n"),
        "{text}"
    );
    // The session selecting the real name was not moved and gets no note.
    let (text, _) = call(&mcp, &registry, &on_a, "graph_info", json!({}));
    assert!(!text.contains("renamed"), "{text}");
    assert_eq!(persons(&mcp, &registry, &on_a, None), 5);
}

#[test]
fn a_selection_by_a_real_name_stays_on_that_graph() {
    let registry = Registry::standard();
    let mcp = Mcp::new(false, None, false);
    let (session, _) = initialize(&mcp, &registry);
    let (text, _) = call(&mcp, &registry, &session, "use_graph", json!({"name": "A"}));
    assert!(
        text.starts_with("{\"selected\":\"a\",\"graph\":\"a\""),
        "{text}"
    );
    // Aliases move, are renamed, removed, or one takes the name of another graph: a stays.
    registry.set_alias("prod", "b");
    assert_eq!(registry.rename_alias(&mcp, "prod", "live"), 0);
    registry.remove_alias("live");
    assert_eq!(persons(&mcp, &registry, &session, None), 4);
    let (text, _) = insert_person(&mcp, &registry, &session);
    assert!(!text.starts_with("note:"), "{text}");
    assert_eq!(persons(&mcp, &registry, &session, None), 5);
    assert_eq!(persons(&mcp, &registry, &session, Some("b")), 4);
    let (text, _) = call(&mcp, &registry, &session, "list_graphs", json!({}));
    assert!(
        text.ends_with("\"selected\":\"a\",\"selectedGraph\":\"a\"}"),
        "{text}"
    );
    // A name the directory does not list is kept as given (and must resolve).
    let (text, is_error) = call(
        &mcp,
        &registry,
        &session,
        "use_graph",
        json!({"name": "zz"}),
    );
    assert!(is_error && text.contains("no database 'zz'"), "{text}");
}
