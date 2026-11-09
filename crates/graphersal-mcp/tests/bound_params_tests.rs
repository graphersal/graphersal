//! Bound saved-query parameters (`Profile::with_bound_params`): a host binds a parameter (the
//! project of a connection) that the agent neither sees in `tools/list` nor can change in
//! `tools/call`, over a stream connection and over HTTP (`Mcp::handle_with_profile`).

use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};

use graphersal_mcp::{
    ClientRef, Connection, Executor, Host, Job, Mcp, McpConfig, McpError, McpRequest, Profile,
};
use graphersal_session::Session;
use serde_json::{Value, json};

/// A host over one session on its own thread: the modern graph with three saved queries of the
/// folder `mem`: `scoped(project, name = "x")` (reads), `remember(project, name)` (writes),
/// `unscoped(name)` (declares no `project`).
struct TestHost {
    jobs: Mutex<Sender<Job>>,
}

impl TestHost {
    fn new() -> Arc<Self> {
        let (jobs, queue) = channel::<Job>();
        std::thread::Builder::new()
            .stack_size(64 * 1024 * 1024)
            .spawn(move || {
                let mut session = Session::new();
                session.load_sample("modern").expect("the sample loads");
                for job in queue {
                    job(&mut session);
                }
            })
            .expect("the session thread starts");
        let host = Arc::new(Self {
            jobs: Mutex::new(jobs),
        });
        let script = "g.define_query(#{name: \"scoped\", folder: \"mem\", description: \"Notes \
            of a project.\", params: #{project: #{schema: #{type: \"string\"}}, name: #{schema: \
            #{type: \"string\"}, default: \"x\"}}, body: \"fn scoped(project, name) { project + \
            \\\"/\\\" + name }\"}); \
            g.define_query(#{name: \"remember\", folder: \"mem\", params: #{project: #{schema: \
            #{type: \"string\"}}, name: #{schema: #{type: \"string\"}}}, body: \"fn \
            remember(project, name) { g.add_v(\\\"note\\\").property(\\\"project\\\", project)\
            .property(\\\"name\\\", name).values(\\\"project\\\") }\"}); \
            g.define_query(#{name: \"unscoped\", folder: \"mem\", params: #{name: #{schema: \
            #{type: \"string\"}}}, body: \"fn unscoped(name) { name }\"}); 0";
        let setup = Connection::new(Arc::new(Mcp::new(false, None, false)), host.clone());
        let answer = call(&setup, "query", json!({"script": script}));
        assert!(ok(&answer), "{answer}");
        host
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

fn ask(connection: &Connection, method: &str, params: Value) -> Value {
    let message = json!({"jsonrpc": "2.0", "id": 5, "method": method, "params": params});
    let lines = connection.handle(&message.to_string());
    serde_json::from_str(lines.first().expect("an answer")).expect("JSON")
}

fn call(connection: &Connection, name: &str, arguments: Value) -> Value {
    let answer = ask(
        connection,
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

/// The `call:` line of an answer.
fn call_line(result: &Value) -> &str {
    text(result)
        .lines()
        .find(|line| line.starts_with("call: "))
        .unwrap_or_default()
}

fn config() -> McpConfig {
    McpConfig::parse(
        r#"{"saved_query_tools": {"folders": ["mem"]}}"#,
        &mut |_| Err("no files".into()),
    )
    .expect("parses")
}

fn bound_profile() -> Profile {
    Profile::new(false)
        .with_config(config())
        .with_bound_params([("project".to_string(), json!("/work/a"))])
}

fn connection(host: &Arc<TestHost>, profile: Profile) -> Connection {
    Connection::new(Arc::new(Mcp::new(false, None, false)), host.clone()).with_profile(profile)
}

fn tool<'a>(listed: &'a Value, name: &str) -> &'a Value {
    listed["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .find(|tool| tool["name"] == name)
        .unwrap_or_else(|| panic!("{name} listed: {listed}"))
}

#[test]
fn bound_params_accumulate_and_belong_to_the_profile() {
    let profile = Profile::new(false)
        .with_bound_params([("project".to_string(), json!("/a"))])
        .with_bound_params([
            ("project".to_string(), json!("/b")),
            ("user".to_string(), json!(7)),
        ]);
    assert_eq!(profile.bound_params()["project"], json!("/b"));
    assert_eq!(profile.bound_params()["user"], json!(7));
    assert!(Profile::new(false).bound_params().is_empty());
    assert_ne!(profile, Profile::new(false));
}

#[test]
fn a_bound_parameter_is_left_out_of_the_saved_query_tools_schemas() {
    let host = TestHost::new();
    let unbound = ask(
        &connection(&host, Profile::new(false).with_config(config())),
        "tools/list",
        json!({}),
    );
    let schema = &tool(&unbound, "scoped")["inputSchema"];
    assert!(schema["properties"].get("project").is_some(), "{schema}");
    assert_eq!(schema["required"], json!(["project"]), "{schema}");

    let bound = ask(&connection(&host, bound_profile()), "tools/list", json!({}));
    let schema = &tool(&bound, "scoped")["inputSchema"];
    assert_eq!(
        schema["properties"],
        json!({"name": {"type": "string", "default": "x"}}),
        "{schema}"
    );
    assert_eq!(schema["required"], json!([]), "{schema}");
    let schema = &tool(&bound, "remember")["inputSchema"];
    assert!(schema["properties"].get("project").is_none(), "{schema}");
    assert_eq!(schema["required"], json!(["name"]), "{schema}");
    // A query without the parameter and the built-in tools are listed unchanged.
    assert_eq!(
        tool(&bound, "unscoped"),
        tool(
            &ask(
                &connection(&host, Profile::new(false).with_config(config())),
                "tools/list",
                json!({})
            ),
            "unscoped"
        )
    );
    assert_eq!(
        tool(&bound, "run_saved_query"),
        tool(&unbound, "run_saved_query")
    );
}

#[test]
fn a_bound_parameter_overrides_the_client_in_both_call_paths() {
    let host = TestHost::new();
    let bound = connection(&host, bound_profile());
    // The saved query's own tool: the client's project is replaced, the call line hides it.
    let answer = call(&bound, "scoped", json!({"project": "/evil", "name": "n"}));
    assert!(ok(&answer), "{answer}");
    assert!(text(&answer).contains("/work/a/n"), "{answer}");
    assert!(!text(&answer).contains("/evil"), "{answer}");
    assert!(call_line(&answer).contains("\"scoped\""), "{answer}");
    assert!(!call_line(&answer).contains("project"), "{answer}");
    // Without arguments: the bound value and the defaults.
    let answer = call(&bound, "scoped", json!({}));
    assert!(text(&answer).contains("/work/a/x"), "{answer}");
    // run_saved_query: with params, without params, with params as a JSON string.
    for args in [
        json!({"name": "scoped", "params": {"project": "/evil", "name": "n"}}),
        json!({"name": "scoped", "params": "{\"name\": \"n\"}"}),
    ] {
        let answer = call(&bound, "run_saved_query", args);
        assert!(ok(&answer), "{answer}");
        assert!(text(&answer).contains("/work/a/n"), "{answer}");
        assert!(!call_line(&answer).contains("project"), "{answer}");
    }
    let answer = call(&bound, "run_saved_query", json!({"name": "scoped"}));
    assert!(text(&answer).contains("/work/a/x"), "{answer}");
    // A writing query gets the bound value too.
    let answer = call(&bound, "remember", json!({"name": "first"}));
    assert!(ok(&answer), "{answer}");
    assert!(text(&answer).contains("/work/a"), "{answer}");
    // A query that does not declare the parameter is called unchanged.
    let answer = call(
        &bound,
        "run_saved_query",
        json!({"name": "unscoped", "params": {"name": "q"}}),
    );
    assert!(ok(&answer), "{answer}");
    assert_eq!(
        call_line(&answer),
        call_line(&call(
            &connection(&host, Profile::new(false).with_config(config())),
            "run_saved_query",
            json!({"name": "unscoped", "params": {"name": "q"}}),
        ))
    );
    // An unknown query still fails as before.
    let answer = call(&bound, "run_saved_query", json!({"name": "nope"}));
    assert_eq!(answer["isError"], json!(true), "{answer}");
    // Without a binding the client's value is used.
    let free = connection(&host, Profile::new(false).with_config(config()));
    let answer = call(&free, "scoped", json!({"project": "/mine", "name": "n"}));
    assert!(text(&answer).contains("/mine/n"), "{answer}");
}

#[test]
fn a_read_only_profile_still_refuses_a_writing_query_with_a_binding() {
    let host = TestHost::new();
    let profile = Profile::new(true)
        .with_config(config())
        .with_bound_params([("project".to_string(), json!("/work/a"))]);
    let bound = connection(&host, profile);
    let answer = call(
        &bound,
        "run_saved_query",
        json!({"name": "remember", "params": {"name": "n"}}),
    );
    assert_eq!(answer["isError"], json!(true), "{answer}");
    assert!(text(&answer).contains("read-only"), "{answer}");
    let answer = call(&bound, "scoped", json!({"name": "n"}));
    assert!(text(&answer).contains("/work/a/n"), "{answer}");
}

#[test]
fn an_http_request_gets_the_binding_of_its_profile() {
    let host = TestHost::new();
    let mcp = Mcp::new(false, None, false);
    let profile = bound_profile();
    let post = |body: Value| -> Value {
        let body = body.to_string();
        let request = McpRequest::new("POST", body.as_bytes())
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream");
        let answer = mcp.handle_with_profile(host.as_ref(), &profile, request);
        assert_eq!(answer.status(), 200);
        serde_json::from_slice(answer.body()).expect("JSON")
    };
    let listed = post(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}));
    assert!(
        tool(&listed, "scoped")["inputSchema"]["properties"]
            .get("project")
            .is_none(),
        "{listed}"
    );
    let answer = post(
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
        "name": "run_saved_query",
        "arguments": {"name": "scoped", "params": {"project": "/evil", "name": "h"}}}}),
    );
    assert!(text(&answer["result"]).contains("/work/a/h"), "{answer}");
    let answer = post(
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {
        "name": "scoped", "arguments": {"project": "/evil"}}}),
    );
    assert!(text(&answer["result"]).contains("/work/a/x"), "{answer}");
}
