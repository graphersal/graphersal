//! Profiles and a host per connection: two profiles on one [`Mcp`] (a `use`-like profile whose
//! access policy allows only the inferred effects of one saved query, and a `develop`-like one
//! that allows everything), two graphs behind two connections of one endpoint, a host's later
//! profile change that leaves the open connections as they were, the identity a host attaches,
//! and the schema summary without descriptions by default (issue #34).

use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};

use graphersal::auth::{AccessPolicy, ActionKind, ResourceKind};
use graphersal::exec::CancelToken;
use graphersal_mcp::{
    ClientRef, Connection, Executor, Host, Job, Mcp, McpError, McpRequest, Profile,
};
use graphersal_session::Session;
use serde_json::{Value, json};

/// A host over one session on its own thread, loaded with a sample graph; it records the
/// identity of every run it is told about.
struct TestHost {
    jobs: Mutex<Sender<Job>>,
    identities: Mutex<Vec<Option<String>>>,
}

impl TestHost {
    fn on(sample: &'static str) -> Arc<Self> {
        let (jobs, queue) = channel::<Job>();
        std::thread::Builder::new()
            .stack_size(64 * 1024 * 1024)
            .spawn(move || {
                let mut session = Session::new();
                session.load_sample(sample).expect("the sample loads");
                for job in queue {
                    job(&mut session);
                }
            })
            .expect("the session thread starts");
        Arc::new(Self {
            jobs: Mutex::new(jobs),
            identities: Mutex::new(Vec::new()),
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

    fn run_started(&self, client: &ClientRef, _cancel: &CancelToken) -> u64 {
        self.identities
            .lock()
            .expect("identities")
            .push(client.identity().map(str::to_string));
        1
    }
}

/// One JSON-RPC request to a connection: its answer.
fn ask(connection: &Connection, id: u64, method: &str, params: Value) -> Value {
    let message = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
    let lines = connection.handle(&message.to_string());
    let first = lines.first().expect("an answer");
    serde_json::from_str(first).expect("JSON")
}

/// `tools/call` of `name` with `arguments`: the result object (or the error answer).
fn call(connection: &Connection, name: &str, arguments: Value) -> Value {
    let answer = ask(
        connection,
        7,
        "tools/call",
        json!({"name": name, "arguments": arguments}),
    );
    answer.get("result").cloned().unwrap_or(answer)
}

fn text(result: &Value) -> &str {
    result["content"][0]["text"].as_str().unwrap_or_default()
}

fn ok(result: &Value) -> bool {
    result["isError"] == json!(false)
}

fn initialized(connection: Connection) -> Connection {
    let answer = ask(
        &connection,
        1,
        "initialize",
        json!({"protocolVersion": "2025-06-18", "clientInfo": {"name": "test", "version": "1"}}),
    );
    assert!(answer["result"].is_object(), "{answer}");
    connection
}

fn tool_names(connection: &Connection) -> Vec<String> {
    let answer = ask(connection, 2, "tools/list", json!({}));
    answer["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .filter_map(|tool| tool["name"].as_str().map(str::to_string))
        .collect()
}

fn vertex_count(connection: &Connection) -> i64 {
    let result = call(
        connection,
        "gql",
        json!({"query": "MATCH (n) RETURN count(n) AS c"}),
    );
    assert!(ok(&result), "{result}");
    let line = text(&result)
        .lines()
        .find(|line| line.contains("\"c\""))
        .unwrap_or_default();
    let row: Value = serde_json::from_str(line.trim_end_matches(',')).unwrap_or(Value::Null);
    row["c"]
        .as_i64()
        .or_else(|| row[0]["c"].as_i64())
        .unwrap_or_else(|| panic!("no count in {}", text(&result)))
}

/// The access policy of a `use`-like profile: reading, plus exactly the data writes the saved
/// query `name` is known to make (its inferred effects, as `list_saved_queries` names them).
fn policy_of_saved_query(connection: &Connection, name: &str) -> AccessPolicy {
    let listed = call(connection, "list_saved_queries", json!({"search": name}));
    assert!(ok(&listed), "{listed}");
    let listing = text(&listed);
    let json_start = listing.find('[').expect("a JSON list");
    let entries: Value = serde_json::from_str(&listing[json_start..]).expect("JSON");
    let entry = entries
        .as_array()
        .and_then(|entries| entries.iter().find(|entry| entry["name"] == name))
        .expect("the saved query is listed");
    assert_eq!(entry["writes"], json!(true), "{entry}");
    let mut policy = AccessPolicy::read_only();
    for effect in entry["effects"].as_array().expect("effects") {
        let effect = effect.as_str().expect("an effect text");
        let action = match effect.split(' ').next() {
            Some("Create") => ActionKind::Create,
            Some("Update") => ActionKind::Update,
            Some("Delete") => ActionKind::Delete,
            other => panic!("an effect that does not write data: {other:?} ({effect})"),
        };
        assert!(effect.contains(" Data("), "{effect}");
        policy = policy.allow(action, ResourceKind::Data);
    }
    policy
}

const ADD_PERSON: &str = "g.define_query(#{name: \"add_person\", params: #{name: \"string\"}, \
    body: \"fn add_person(name) { g.add_v(\\\"person\\\").property(\\\"name\\\", name) }\"}); 0";

#[test]
fn a_use_profile_runs_its_saved_query_and_no_other_write_and_a_develop_profile_writes() {
    let host = TestHost::on("modern");
    let mcp = Arc::new(Mcp::new(false, None, false));
    let develop = initialized(Connection::new(mcp.clone(), host.clone()));
    let defined = call(&develop, "query", json!({"script": ADD_PERSON}));
    assert!(ok(&defined), "{defined}");
    let policy = policy_of_saved_query(&develop, "add_person");
    assert!(policy.allows(ActionKind::Create, ResourceKind::Data));
    assert!(!policy.allows(ActionKind::Delete, ResourceKind::Data));

    let using = initialized(
        Connection::new(mcp.clone(), host.clone())
            .with_profile(Profile::new(false).with_access_policy(policy.clone())),
    );
    assert_eq!(using.profile().access_policy(), Some(&policy));
    let before = vertex_count(&using);

    // The saved query's own effects are allowed.
    let ran = call(
        &using,
        "run_saved_query",
        json!({"name": "add_person", "params": {"name": "zoe"}}),
    );
    assert!(ok(&ran), "{ran}");
    assert_eq!(vertex_count(&using), before + 1);

    // Every write outside those effects is refused by the engine before it runs, and changes
    // nothing. An `AccessPolicy` matches action and resource KINDS (not labels): the policy
    // allows what the saved query's effects name on `Data` (here `Create`, and the `Update` of
    // its `property` step), so a delete or a catalog write is what stays refused.
    for (tool, arguments) in [
        (
            "query",
            json!({"script": "g.v().has_label(\"software\").drop().iterate()"}),
        ),
        ("query", json!({"script": "g.e().drop().iterate()"})),
        (
            "gql",
            json!({"query": "MATCH (n:software) DETACH DELETE n"}),
        ),
        (
            "query",
            json!({"script": "g.define_query(#{name: \"x\", body: \"fn x() { 1 }\"}); 0"}),
        ),
    ] {
        let refused = call(&using, tool, arguments.clone());
        assert!(!ok(&refused), "{tool} {arguments}: {refused}");
        assert!(
            text(&refused).contains("Permission denied for "),
            "{tool} {arguments}: {}",
            text(&refused)
        );
    }
    assert_eq!(vertex_count(&using), before + 1);

    // The session's own writing tools follow the policy too: not listed, refused.
    let names = tool_names(&using);
    for tool in [
        "set_schema",
        "patch_schema",
        "create_index",
        "define_compression",
    ] {
        assert!(
            !names.iter().any(|name| name == tool),
            "{tool} in {names:?}"
        );
    }
    assert!(names.iter().any(|name| name == "run_saved_query"));
    let refused = ask(
        &using,
        9,
        "tools/call",
        json!({"name": "create_index", "arguments": {"label": "person", "key": "name"}}),
    );
    assert_eq!(refused["error"]["code"], json!(-32602), "{refused}");
    assert!(
        refused["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("not allowed by this connection's access policy"),
        "{refused}"
    );

    // The develop profile (the endpoint's default) writes; its listing is unchanged.
    let dropped = call(
        &develop,
        "query",
        json!({"script": "g.v().has(\"name\", \"zoe\").drop().iterate()"}),
    );
    assert!(ok(&dropped), "{dropped}");
    assert_eq!(vertex_count(&develop), before);
    let names = tool_names(&develop);
    assert!(names.iter().any(|name| name == "set_schema"), "{names:?}");
}

#[test]
fn an_http_request_runs_under_the_profile_the_host_chose() {
    let host = TestHost::on("modern");
    let mcp = Mcp::new(false, None, false);
    let post = |profile: &Profile, body: Value| -> Value {
        let body = body.to_string();
        let request = McpRequest::new("POST", body.as_bytes())
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream");
        let answer = mcp.handle_with_profile(host.as_ref(), profile, request);
        assert_eq!(answer.status(), 200);
        serde_json::from_slice(answer.body()).expect("JSON")
    };
    let drop = json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {
        "name": "query", "arguments": {"script": "g.v().has_label(\"software\").drop().iterate()"}}});
    let reading = Profile::new(false).with_access_policy(AccessPolicy::read_only());
    let refused = post(&reading, drop.clone());
    assert_eq!(refused["result"]["isError"], json!(true), "{refused}");
    assert!(
        text(&refused["result"]).contains("Permission denied for 'drop'"),
        "{refused}"
    );
    let allowed = post(mcp.profile(), drop);
    assert_eq!(allowed["result"]["isError"], json!(false), "{allowed}");
    // The read-only profile's instructions carry the read-only note; the default's do not.
    let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}});
    let read_only = post(&Profile::new(true), init.clone());
    let default = post(mcp.profile(), init);
    let note = "This server is READ-ONLY for MCP";
    assert!(
        read_only["result"]["instructions"]
            .as_str()
            .unwrap_or_default()
            .contains(note)
    );
    assert!(
        !default["result"]["instructions"]
            .as_str()
            .unwrap_or_default()
            .contains(note)
    );
}

#[test]
fn two_graphs_behind_two_connections_of_one_endpoint() {
    let mcp = Arc::new(Mcp::new(false, None, false));
    let modern = initialized(Connection::new(mcp.clone(), TestHost::on("modern")));
    let empty = initialized(Connection::new(mcp.clone(), TestHost::on("empty")));
    assert_eq!(vertex_count(&modern), 6);
    assert_eq!(vertex_count(&empty), 0);
    // A write on one graph leaves the other alone.
    let added = call(
        &empty,
        "gql",
        json!({"query": "INSERT (:thing {name: 'one'})"}),
    );
    assert!(ok(&added), "{added}");
    assert_eq!(vertex_count(&empty), 1);
    assert_eq!(vertex_count(&modern), 6);
    // The statistics are the endpoint's: both connections counted.
    let stats = mcp.stats().to_json().expect("statistics");
    let gql = stats["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .find(|tool| tool["name"] == "gql")
        .cloned()
        .unwrap_or(Value::Null);
    assert_eq!(gql["calls"], json!(5), "{stats}");
}

#[test]
fn a_profile_change_does_not_affect_open_connections() {
    let host = TestHost::on("modern");
    let mcp = Arc::new(Mcp::new(false, None, false));
    // The host's current choice for new connections: read-only at first.
    let current = Mutex::new(Profile::new(true));
    let connect = || {
        let profile = current.lock().expect("profile").clone();
        initialized(Connection::new(mcp.clone(), host.clone()).with_profile(profile))
    };
    let early = connect();
    *current.lock().expect("profile") = Profile::new(false);
    let late = connect();

    assert!(early.profile().read_only());
    assert!(!late.profile().read_only());
    let write = json!({"script": "g.add_v(\"thing\").iterate()"});
    let refused = call(&early, "query", write.clone());
    assert!(!ok(&refused), "{refused}");
    assert!(!tool_names(&early).iter().any(|name| name == "set_schema"));
    let allowed = call(&late, "query", write);
    assert!(ok(&allowed), "{allowed}");
    assert!(tool_names(&late).iter().any(|name| name == "set_schema"));
    // The endpoint's own default stays what it was built with.
    assert!(!mcp.profile().read_only());
}

#[test]
fn the_identity_a_host_attaches_reaches_the_runs() {
    let host = TestHost::on("modern");
    let mcp = Arc::new(Mcp::new(false, None, false));
    let connection = initialized(Connection::new(mcp.clone(), host.clone()).with_identity("alice"));
    let ran = call(&connection, "query", json!({"script": "1 + 1"}));
    assert!(ok(&ran), "{ran}");
    let body = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
        "name": "query", "arguments": {"script": "2"}}})
    .to_string();
    let request = McpRequest::new("POST", body.as_bytes())
        .header("Content-Type", "application/json")
        .with_identity("bob");
    assert_eq!(request.identity(), Some("bob"));
    assert_eq!(mcp.handle(host.as_ref(), request).status(), 200);
    let body = body.replace("\"2\"", "\"3\"");
    let request =
        McpRequest::new("POST", body.as_bytes()).header("Content-Type", "application/json");
    assert_eq!(mcp.handle(host.as_ref(), request).status(), 200);
    assert_eq!(
        *host.identities.lock().expect("identities"),
        [Some("alice".to_string()), Some("bob".to_string()), None]
    );
}

#[test]
fn the_default_schema_summary_leaves_out_descriptions() {
    let host = TestHost::on("modern");
    let mcp = Arc::new(Mcp::new(false, None, false));
    let connection = initialized(Connection::new(mcp, host));
    let schema = json!({"mode": "open", "vertices": {"person": {"schema": {
        "description": "A human being.", "type": "object",
        "properties": {"name": {"type": "string", "description": "The given name."}}}}}});
    let stored = call(&connection, "set_schema", json!({"schema": schema}));
    assert!(ok(&stored), "{stored}");

    for tool in ["get_schema", "infer_schema"] {
        let short = call(&connection, tool, json!({}));
        assert!(ok(&short), "{short}");
        assert!(text(&short).contains("person 4:"), "{}", text(&short));
        assert!(!text(&short).contains("A human being."), "{}", text(&short));
        let full = call(&connection, tool, json!({"descriptions": true}));
        assert!(text(&full).contains("A human being."), "{}", text(&full));
        // The labels' details too.
        let short = call(&connection, tool, json!({"labels": ["person"]}));
        assert!(
            !text(&short).contains("The given name."),
            "{}",
            text(&short)
        );
        let full = call(
            &connection,
            tool,
            json!({"labels": ["person"], "descriptions": true}),
        );
        assert!(text(&full).contains("The given name."), "{}", text(&full));
    }
    let wrong = call(&connection, "get_schema", json!({"descriptions": "yes"}));
    assert!(!ok(&wrong), "{wrong}");
    assert!(
        text(&wrong).contains("descriptions is true or false"),
        "{wrong}"
    );

    // The resource keeps the full summary.
    let read = ask(
        &connection,
        5,
        "resources/read",
        json!({"uri": "graphersal://schema-summary"}),
    );
    let resource = read["result"]["contents"][0]["text"]
        .as_str()
        .unwrap_or_default();
    assert!(resource.contains("A human being."), "{read}");
    let full = call(&connection, "get_schema", json!({"descriptions": true}));
    assert_eq!(resource, text(&full));
}
