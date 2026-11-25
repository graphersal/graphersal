//! Tests of the per-request access level ([`Request::mark_read_only`], plan
//! `ai/graphersal-server-access/PLAN.md` A3): ONE context, two requests for each route class,
//! one marked read-only (by the test, or by a request filter) and one not. The marked request
//! reads, runs its scripts and saved queries under the reading authorizer, and gets `409` from
//! every route that writes; the unmarked one writes as before. `GET /api/info` and
//! `GET /api/changes` answer `readOnly` per request; `/mcp` serves a marked request under the
//! context's profile made read-only. Real session threads, Stores in memory or in the temp
//! directory, no HTTP library.

use std::io::Cursor;
use std::path::PathBuf;
use std::sync::Arc;

use graphersal::persist::{MemDir, Store, StoreOptions};
use graphersal::prelude::TraversalGraph;
use graphersal_session::{Session, SessionPresets};
use serde_json::{Value, json};

use crate::{
    Context, ContextBuilder, Exposure, HostCheck, HttpError, Method, OpenStore, READ_ONLY_HEADLINE,
    READ_ONLY_REQUEST_HELP, Request, RequestFilter, Response, ServedSource, handle, launch,
    open_store,
};

const HOST: &str = "127.0.0.1:8080";

fn check() -> HostCheck {
    HostCheck::new(Exposure::Loopback, 8080, &format!("http://{HOST}/"))
}

fn get<'a>(url: &str) -> Request<'a> {
    Request::new(Method::Get, url, Cursor::new(&b""[..]))
        .with_header("Host", HOST)
        .with_header("X-Graphersal-Client", "tab-1")
}

fn post<'a>(url: &str, body: &'a [u8]) -> Request<'a> {
    Request::new(Method::Post, url, Cursor::new(body))
        .with_header("Host", HOST)
        .with_header("X-Graphersal-Client", "tab-1")
        .with_header("Content-Type", "application/json")
        .with_body_length(Some(body.len()))
}

/// The request marked read-only, as a host's router would.
fn marked(mut request: Request<'_>) -> Request<'_> {
    request.mark_read_only();
    request
}

fn text(response: &Response) -> String {
    String::from_utf8_lossy(response.body()).into_owned()
}

fn body_json(response: &Response) -> Value {
    serde_json::from_slice(response.body()).expect("a JSON answer")
}

/// The `409` text of a write on a marked request.
fn refusal() -> String {
    format!("{READ_ONLY_HEADLINE}. {READ_ONLY_REQUEST_HELP}")
}

/// Asserts the read-only refusal of a marked request.
fn assert_refused(answer: &Response, what: &str) {
    assert_eq!(answer.status(), 409, "{what}: {}", text(answer));
    assert_eq!(body_json(answer)["error"], refusal(), "{what}");
}

/// Asserts that a route answered (not the read-only refusal).
fn assert_answered(answer: &Response, what: &str) {
    assert_eq!(answer.status(), 200, "{what}: {}", text(answer));
}

fn execute_body(script: &str) -> Vec<u8> {
    json!({ "script": script }).to_string().into_bytes()
}

/// The number of vertices, read over the HTTP API on a marked request.
fn vertices(context: &Arc<Context>) -> i64 {
    let body = execute_body("g.v().count().next()");
    let answer = handle(context, marked(post("/api/execute", &body)));
    assert_answered(&answer, "count");
    body_json(&answer)["execution"]["results"][0]
        .as_i64()
        .unwrap_or_else(|| panic!("a count: {}", text(&answer)))
}

/// Creates the modern sample as a Store in `dir` and opens it.
fn open_modern(dir: impl graphersal::persist::IntoStoreDir + Copy) -> Arc<Store> {
    Store::create_with(dir, TraversalGraph::tinkerpop_modern(), StoreOptions::new())
        .expect("the store is created")
        .close()
        .expect("the store closes");
    open_store(dir, OpenStore::new())
        .expect("the store opens")
        .store
}

/// The builder of a context serving `store`.
fn context_of(store: Arc<Store>) -> ContextBuilder {
    let (launched, ()) = launch(
        ServedSource::Store(store),
        SessionPresets::new(),
        |_: &mut Session| Ok::<_, String>(()),
        None,
    )
    .expect("the launch");
    launched.context_builder(check())
}

/// A context over the modern sample in a Store in memory.
fn mem_context(name: &str) -> ContextBuilder {
    let dir = MemDir::named(name);
    context_of(open_modern(&dir))
}

/// A fresh directory of this test process in the temp directory.
fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "graphersal_serve_read_mark_{}_{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("the temporary directory");
    dir
}

#[test]
fn a_marked_execute_reads_and_cannot_write_the_other_request_writes() {
    let context = mem_context("read-mark-execute").build();
    let write = execute_body("g.add_v(\"thing\").iterate()");
    let refused = handle(&context, marked(post("/api/execute", &write)));
    assert_answered(&refused, "marked execute");
    assert!(
        text(&refused).contains("Permission denied"),
        "{}",
        text(&refused)
    );
    assert_eq!(vertices(&context), 6);
    let written = handle(&context, post("/api/execute", &write));
    assert_answered(&written, "execute");
    assert!(body_json(&written)["error"].is_null(), "{}", text(&written));
    assert_eq!(vertices(&context), 7);
}

#[test]
fn a_marked_saved_query_runs_under_the_reading_authorizer() {
    let context = mem_context("read-mark-query").build();
    let defined = handle(
        &context,
        post(
            "/api/queries/define",
            br#"{"name": "add_one", "body": "fn add_one() { g.add_v(\"thing\").iterate() }"}"#,
        ),
    );
    assert_answered(&defined, "define");
    let call = br#"{"name": "add_one"}"#;
    let refused = handle(&context, marked(post("/api/query", call)));
    assert_answered(&refused, "marked query");
    let answer = body_json(&refused);
    assert_eq!(answer["ok"], false, "{answer}");
    assert!(answer.to_string().contains("Permission denied"), "{answer}");
    assert_eq!(vertices(&context), 6);
    let ran = handle(&context, post("/api/query", call));
    assert_answered(&ran, "query");
    assert_eq!(body_json(&ran)["ok"], true, "{}", text(&ran));
    assert_eq!(vertices(&context), 7);
}

#[test]
fn commit_writes_are_refused_on_a_marked_request_only() {
    let context = mem_context("read-mark-commit").build();
    let element = json!({"kind": "vertex", "labels": ["person"],
        "properties": [["name", {"type": "string", "value": "ann"}]]})
    .to_string();
    let schema = br#"{"mode": "open", "vertices": {"person": {"schema": {"type": "object"}}}}"#;
    let index = br#"{"name": "by_name", "label": "person", "key": "name"}"#;
    let routes: [(&str, &[u8]); 3] = [
        ("/api/element/create", element.as_bytes()),
        ("/api/schema/set", schema),
        (
            "/api/queries/define",
            br#"{"name": "q", "body": "fn q() { 1 }"}"#,
        ),
    ];
    for (url, body) in routes {
        assert_refused(&handle(&context, marked(post(url, body))), url);
    }
    let refused = handle(&context, marked(post("/api/indexes/create", index)));
    assert_refused(&refused, "indexes/create");
    assert_eq!(vertices(&context), 6);
    for (url, body) in routes {
        assert_answered(&handle(&context, post(url, body)), url);
    }
    assert_eq!(vertices(&context), 7);
}

#[test]
fn store_and_job_writes_are_refused_on_a_marked_request_only() {
    let context = mem_context("read-mark-store").build();
    let job = br#"{"name": "cp", "schedule": "0 3 1 1 *", "action": "checkpoint"}"#;
    let routes: [(&str, &[u8]); 5] = [
        ("/api/jobs/define", job),
        ("/api/jobs/run", br#"{"name": "cp"}"#),
        ("/api/store/checkpoint", br#"{}"#),
        ("/api/store/mark", br#"{"name": "m"}"#),
        ("/api/store/compact", br#"{}"#),
    ];
    for (url, body) in routes {
        assert_refused(&handle(&context, marked(post(url, body))), url);
    }
    for url in [
        "/api/store/rollback",
        "/api/store/attic/restore",
        "/api/store/attic/remove",
        "/api/save",
        "/api/jobs/drop",
    ] {
        assert_refused(&handle(&context, marked(post(url, b"{}"))), url);
    }
    // Nothing was defined by the marked requests.
    let jobs = body_json(&handle(&context, marked(get("/api/jobs"))));
    assert!(!jobs.to_string().contains("\"cp\""), "{jobs}");
    for (url, body) in routes {
        assert_answered(&handle(&context, post(url, body)), url);
    }
    // Reads of the store stay open on a marked request.
    for url in [
        "/api/store",
        "/api/store/summary",
        "/api/jobs",
        "/api/graph",
    ] {
        assert_answered(&handle(&context, marked(get(url))), url);
    }
    let verify = handle(&context, marked(post("/api/store/verify", b"{}")));
    assert_answered(&verify, "store/verify");
}

#[test]
fn fork_backup_and_view_are_refused_on_a_marked_request_only() {
    let root = temp_dir("fork");
    let store_dir = root.join("data");
    let context = context_of(open_modern(&store_dir)).build();
    let mark = handle(&context, post("/api/store/mark", br#"{"name": "start"}"#));
    assert_answered(&mark, "mark");
    let fork = br#"{"target": {"mark": "start"}}"#;
    let attic_fork = br#"{"id": "a"}"#;
    let view = br#"{"target": {"mark": "start"}}"#;
    for (url, body) in [
        ("/api/store/fork", &fork[..]),
        ("/api/store/attic/fork", attic_fork),
        ("/api/store/backup", b"{}"),
        ("/api/store/view", view),
        ("/api/store/live", b"{}"),
    ] {
        assert_refused(&handle(&context, marked(post(url, body))), url);
    }
    assert!(!root.join("data-fork-mark-start").exists());
    assert!(!root.join("data-backup").exists());
    assert!(body_json(&handle(&context, get("/api/info")))["view"].is_null());

    let forked = handle(&context, post("/api/store/fork", fork));
    assert_answered(&forked, "fork");
    assert!(root.join("data-fork-mark-start").is_dir());
    let backup = handle(&context, post("/api/store/backup", b"{}"));
    assert_answered(&backup, "backup");
    assert!(root.join("data-backup").is_dir());
    assert_answered(&handle(&context, post("/api/store/view", view)), "view");
    assert_answered(&handle(&context, post("/api/store/live", b"{}")), "live");

    // Downloads are reads: open on a marked request.
    let gsnap = handle(&context, marked(get("/api/export?format=gsnap")));
    assert_answered(&gsnap, "export gsnap");
    let snapshot = handle(&context, marked(get("/api/store/snapshot")));
    assert_answered(&snapshot, "store/snapshot");
    let graphson = handle(&context, marked(get("/api/export")));
    assert_answered(&graphson, "export");
    drop(context);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn advisor_marks_are_refused_on_a_marked_request_only() {
    let context = mem_context("read-mark-advise").build();
    let advise = handle(&context, marked(post("/api/advise", b"{}")));
    assert_answered(&advise, "advise");
    for url in [
        "/api/advise/dismiss",
        "/api/advise/restore",
        "/api/advise/apply",
    ] {
        assert_refused(&handle(&context, marked(post(url, br#"{"id": "x"}"#))), url);
    }
    for url in ["/api/advise/dismiss", "/api/advise/restore"] {
        assert_answered(&handle(&context, post(url, br#"{"id": "x"}"#)), url);
    }
}

#[test]
fn info_and_changes_answer_read_only_per_request() {
    let context = mem_context("read-mark-info").build();
    for url in ["/api/info", "/api/changes"] {
        assert_eq!(
            body_json(&handle(&context, marked(get(url))))["readOnly"],
            true
        );
        assert_eq!(body_json(&handle(&context, get(url)))["readOnly"], false);
    }
    // The mark lives for one request: the context is not read-only.
    assert!(!context.is_read_only());
}

/// A host's filter that marks the requests of the identity `reader` read-only.
struct ReaderFilter;

impl RequestFilter for ReaderFilter {
    fn filter(&self, request: &mut Request<'_>) -> Result<(), HttpError> {
        let user = request
            .header("X-Test-User")
            .unwrap_or_default()
            .to_string();
        if user == "reader" {
            request.mark_read_only();
        }
        if !user.is_empty() {
            request.set_identity(user);
        }
        Ok(())
    }
}

#[test]
fn a_request_filter_marks_requests_read_only() {
    let context = mem_context("read-mark-filter")
        .filter(Arc::new(ReaderFilter))
        .build();
    let as_user = |request: Request<'static>, user: &str| request.with_header("X-Test-User", user);
    let info = handle(&context, as_user(get("/api/info"), "reader"));
    assert_eq!(body_json(&info)["readOnly"], true);
    let info = handle(&context, as_user(get("/api/info"), "writer"));
    assert_eq!(body_json(&info)["readOnly"], false);
    let body: &'static [u8] = br#"{"name": "x"}"#;
    let refused = handle(&context, as_user(post("/api/store/mark", body), "reader"));
    assert_refused(&refused, "reader mark");
    let answered = handle(&context, as_user(post("/api/store/mark", body), "writer"));
    assert_answered(&answered, "writer mark");
    // A wrong method stays a 405, an unknown endpoint a 404, on a marked request too.
    let wrong = handle(&context, as_user(get("/api/store/checkpoint"), "reader"));
    assert_eq!(wrong.status(), 405, "{}", text(&wrong));
    let unknown = handle(&context, as_user(post("/api/nothing", b"{}"), "reader"));
    assert_eq!(unknown.status(), 404, "{}", text(&unknown));
}

#[cfg(feature = "mcp")]
mod mcp {
    use super::*;

    /// `POST /mcp` of `body` with an optional session id, marked read-only or not: the minted
    /// session id and the JSON answer of a `200`.
    fn exchange(
        context: &Arc<Context>,
        body: &Value,
        session: Option<&str>,
        read_only: bool,
    ) -> (Option<String>, Value) {
        let text = body.to_string();
        let mut request = post("/mcp", text.as_bytes()).with_header("Accept", "application/json");
        if let Some(session) = session {
            request = request.with_header("Mcp-Session-Id", session);
        }
        if read_only {
            request.mark_read_only();
        }
        let answer = handle(context, request);
        assert_eq!(answer.status(), 200, "{}", super::text(&answer));
        let id = answer
            .headers()
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("mcp-session-id"))
            .map(|(_, value)| value.clone());
        (id, body_json(&answer))
    }

    fn tool_call(name: &str, arguments: Value) -> Value {
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
               "params": {"name": name, "arguments": arguments}})
    }

    fn tool_text(answer: &Value) -> String {
        answer["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    #[test]
    fn a_marked_mcp_request_is_served_read_only_the_other_writes() {
        let context = mem_context("read-mark-mcp")
            .mcp(Some(graphersal_mcp::Mcp::new(false, None, false)))
            .build();
        let initialize = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": {"name": "tester", "version": "1"}}});
        let (session, _) = exchange(&context, &initialize, None, false);
        let session = session.expect("a session id");
        let write = tool_call("query", json!({"script": "g.add_v(\"agent\").iterate()"}));
        let schema = tool_call(
            "set_schema",
            json!({"schema": {"mode": "open", "vertices": {}}}),
        );

        // The same session, a marked request: the writing query and the writing tool refused.
        let (_, refused) = exchange(&context, &write, Some(&session), true);
        assert_eq!(refused["result"]["isError"], true, "{refused}");
        let (_, refused_tool) = exchange(&context, &schema, Some(&session), true);
        assert!(
            refused_tool["error"].is_object() || refused_tool["result"]["isError"] == true,
            "{refused_tool}"
        );
        assert_eq!(vertices(&context), 6);
        // Its tool list offers only reading tools.
        let list = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"});
        let (_, marked_tools) = exchange(&context, &list, Some(&session), true);
        let (_, all_tools) = exchange(&context, &list, Some(&session), false);
        let names = |answer: &Value| -> Vec<String> {
            answer["result"]["tools"]
                .as_array()
                .map(|tools| {
                    tools
                        .iter()
                        .filter_map(|t| t["name"].as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default()
        };
        assert!(!names(&marked_tools).contains(&"set_schema".to_string()));
        assert!(names(&all_tools).contains(&"set_schema".to_string()));
        // graph_info and the status say read-only for the marked request only.
        let info = tool_call("graph_info", json!({}));
        let (_, marked_info) = exchange(&context, &info, Some(&session), true);
        assert!(
            tool_text(&marked_info).contains("\"mcpReadOnly\":true"),
            "{marked_info}"
        );
        let (_, open_info) = exchange(&context, &info, Some(&session), false);
        assert!(
            tool_text(&open_info).contains("\"mcpReadOnly\":false"),
            "{open_info}"
        );
        let marked_status = body_json(&handle(&context, marked(get("/api/info"))));
        assert_eq!(marked_status["mcp"]["readOnly"], true, "{marked_status}");
        let status = body_json(&handle(&context, get("/api/info")));
        assert_eq!(status["mcp"]["readOnly"], false, "{status}");
        let stats = body_json(&handle(&context, marked(get("/api/mcp/stats"))));
        assert_eq!(stats["readOnly"], true, "{stats}");

        // The unmarked request on the same session writes.
        let (_, written) = exchange(&context, &write, Some(&session), false);
        assert_eq!(written["result"]["isError"], false, "{written}");
        assert_eq!(vertices(&context), 7);
    }
}
