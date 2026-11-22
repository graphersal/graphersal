//! Tests of the runtime read-only toggle ([`crate::ContextBuilder::runtime_read_only`],
//! [`Context::set_read_only`]): a browser-style client over the HTTP boundary and an open MCP
//! session see their writes refused while it is on (the commit hook for the units, `409` for
//! the writes that are no commit) and their reads answered; turned off, writes work again; a
//! write queued before the flip commits; the job ticker pauses; `GET /api/info` and
//! `GET /api/changes` answer the flag. Real session threads, a Store in memory, no HTTP library.

use std::io::Cursor;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use graphersal::persist::{MemDir, Store, StoreOptions};
use graphersal::prelude::TraversalGraph;
use graphersal_session::{RunOptions, Session, SessionPresets};
use serde_json::{Value, json};

use crate::{
    Busy, Context, ContextBuilder, Exposure, HostCheck, Job, JobQueue, Method, OpenStore,
    READ_ONLY_HEADLINE, READ_ONLY_HELP, Request, Response, Runner, ServeError, ServedSource,
    handle, launch, open_store, spawn_session,
};

const HOST: &str = "127.0.0.1:8080";

fn check() -> HostCheck {
    HostCheck::new(Exposure::Loopback, 8080, &format!("http://{HOST}/"))
}

/// A request of a browser tab (`X-Graphersal-Client`) with `Host` set to this server.
fn get<'a>(url: &str) -> Request<'a> {
    Request::new(Method::Get, url, Cursor::new(&b""[..]))
        .with_header("Host", HOST)
        .with_header("X-Graphersal-Client", "tab-1")
}

/// A JSON `POST` of a browser tab.
fn post<'a>(url: &str, body: &'a [u8]) -> Request<'a> {
    Request::new(Method::Post, url, Cursor::new(body))
        .with_header("Host", HOST)
        .with_header("X-Graphersal-Client", "tab-1")
        .with_header("Content-Type", "application/json")
        .with_body_length(Some(body.len()))
}

fn text(response: &Response) -> String {
    String::from_utf8_lossy(response.body()).into_owned()
}

fn body_json(response: &Response) -> Value {
    serde_json::from_slice(response.body()).expect("a JSON answer")
}

/// The answer of `POST /api/execute` of `script` (status 200, the session's JSON).
fn execute(context: &Arc<Context>, script: &str) -> Value {
    let body = json!({ "script": script }).to_string();
    let answer = handle(context, post("/api/execute", body.as_bytes()));
    assert_eq!(answer.status(), 200, "{}", text(&answer));
    body_json(&answer)
}

/// The number of vertices, read over the HTTP API.
fn vertices(context: &Arc<Context>) -> i64 {
    let answer = execute(context, "g.v().count().next()");
    answer["execution"]["results"][0]
        .as_i64()
        .unwrap_or_else(|| panic!("a count: {answer}"))
}

/// The `409` text of the toggle's refusals with the default help.
fn refusal() -> String {
    format!("{READ_ONLY_HEADLINE}. {READ_ONLY_HELP}")
}

/// A Store in memory holding the modern sample, opened.
fn modern_store(name: &str) -> Arc<Store> {
    let dir = MemDir::named(name);
    Store::create_with(
        &dir,
        TraversalGraph::tinkerpop_modern(),
        StoreOptions::new(),
    )
    .expect("the store is created")
    .close()
    .expect("the store closes");
    open_store(&dir, OpenStore::new())
        .expect("the store opens")
        .store
}

/// The builder of a context serving `store` (the launch's pre-filled builder).
fn store_context(name: &str) -> ContextBuilder {
    let (launched, ()) = launch(
        ServedSource::Store(modern_store(name)),
        SessionPresets::new(),
        |_: &mut Session| Ok::<_, String>(()),
        None,
    )
    .expect("the launch");
    launched.context_builder(check())
}

/// A session thread with the `modern` sample loaded.
fn modern_session() -> Sender<Job> {
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
    jobs
}

/// Waits until `done` holds (at most 10 s).
fn wait_for(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn the_toggle_refuses_every_write_and_answers_reads_until_it_is_off() {
    let context = store_context("read-only-toggle")
        .runtime_read_only(None)
        .build();
    // A scheduled job to run later (defined while writes are allowed).
    let defined = handle(
        &context,
        post(
            "/api/jobs/define",
            br#"{"name": "cp", "schedule": "0 3 1 1 *", "action": "checkpoint"}"#,
        ),
    );
    assert_eq!(defined.status(), 200, "{}", text(&defined));
    let mark = handle(&context, post("/api/store/mark", br#"{"name": "start"}"#));
    assert_eq!(mark.status(), 200, "{}", text(&mark));
    assert!(!context.is_read_only());
    assert_eq!(
        body_json(&handle(&context, get("/api/info")))["readOnly"],
        false
    );
    assert_eq!(
        body_json(&handle(&context, get("/api/changes")))["readOnly"],
        false
    );

    context.set_read_only(true).expect("read-only turns on");
    assert!(context.is_read_only());
    assert_eq!(
        body_json(&handle(&context, get("/api/info")))["readOnly"],
        true
    );
    let feed = body_json(&handle(&context, get("/api/changes")));
    assert_eq!(feed["readOnly"], true, "{feed}");

    // A writing query: the commit hook's one diagnostic, nothing written.
    let refused = execute(&context, "g.add_v(\"thing\").iterate()");
    assert!(
        refused.to_string().contains(READ_ONLY_HEADLINE),
        "{refused}"
    );
    assert!(refused.to_string().contains(READ_ONLY_HELP), "{refused}");
    // The element editor and the schema: the same diagnostic.
    let set = json!([["age", {"type": "int64", "value": "30"}]]);
    let body = json!({"kind": "vertex", "id": "1", "set": set}).to_string();
    let edit = handle(&context, post("/api/element/update", body.as_bytes()));
    assert!(text(&edit).contains(READ_ONLY_HEADLINE), "{}", text(&edit));
    let body =
        json!({"kind": "vertex", "labels": ["person"], "properties": [["name", {"type": "string", "value": "ann"}]]})
            .to_string();
    let create = handle(&context, post("/api/element/create", body.as_bytes()));
    assert!(
        text(&create).contains(READ_ONLY_HEADLINE),
        "{}",
        text(&create)
    );
    let schema = handle(
        &context,
        post(
            "/api/schema/set",
            br#"{"mode": "open", "vertices": {"person": {"schema": {"type": "object"}}}}"#,
        ),
    );
    assert_ne!(schema.status(), 200, "{}", text(&schema));
    assert!(
        text(&schema).contains(READ_ONLY_HEADLINE),
        "{}",
        text(&schema)
    );
    let saved_query = handle(
        &context,
        post(
            "/api/queries/define",
            br#"{"name": "people", "body": "fn people() { g.v().has_label(\"person\").to_list() }"}"#,
        ),
    );
    assert!(
        text(&saved_query).contains(READ_ONLY_HEADLINE),
        "{}",
        text(&saved_query)
    );
    // The writes that are no commit: 409 with the same text.
    for (url, body) in [
        ("/api/jobs/run", &br#"{"name": "cp"}"#[..]),
        (
            "/api/jobs/define",
            br#"{"name": "cp2", "schedule": "0 3 1 1 *", "action": "checkpoint"}"#,
        ),
        ("/api/jobs/drop", br#"{"name": "cp"}"#),
        ("/api/jobs/enable", br#"{"name": "cp", "enabled": false}"#),
        (
            "/api/jobs/schedule",
            br#"{"name": "cp", "schedule": "0 4 1 1 *"}"#,
        ),
        ("/api/store/checkpoint", br#"{"name": "x"}"#),
        ("/api/store/mark", br#"{"name": "m"}"#),
        ("/api/store/rollback", br#"{"commit": 0}"#),
        ("/api/store/attic/restore", br#"{"id": "a"}"#),
        ("/api/store/attic/remove", br#"{"id": "a"}"#),
        ("/api/store/compact", br#"{}"#),
    ] {
        let answer = handle(&context, post(url, body));
        assert_eq!(answer.status(), 409, "{url}: {}", text(&answer));
        assert_eq!(body_json(&answer)["error"], refusal(), "{url}");
    }
    // Reads, views and verify stay allowed; nothing changed.
    assert_eq!(vertices(&context), 6);
    assert_eq!(handle(&context, get("/api/graph")).status(), 200);
    assert_eq!(handle(&context, get("/api/jobs")).status(), 200);
    let verify = handle(&context, post("/api/store/verify", b"{}"));
    assert_eq!(verify.status(), 200, "{}", text(&verify));
    let view = handle(
        &context,
        post("/api/store/view", br#"{"target": {"mark": "start"}}"#),
    );
    assert_eq!(view.status(), 200, "{}", text(&view));
    let live = handle(&context, post("/api/store/live", b"{}"));
    assert_eq!(live.status(), 200, "{}", text(&live));
    // Back on the live graph after a view: still refused.
    let refused = execute(&context, "g.add_v(\"thing\").iterate()");
    assert!(
        refused.to_string().contains(READ_ONLY_HEADLINE),
        "{refused}"
    );
    let feed = body_json(&handle(&context, get("/api/changes")));
    assert_eq!(vertices(&context), 6);

    context.set_read_only(false).expect("read-only turns off");
    assert!(!context.is_read_only());
    assert_eq!(
        body_json(&handle(&context, get("/api/info")))["readOnly"],
        false
    );
    let written = execute(&context, "g.add_v(\"thing\").iterate()");
    assert!(written["error"].is_null(), "{written}");
    assert_eq!(vertices(&context), 7);
    assert_ne!(
        body_json(&handle(&context, get("/api/changes")))["version"],
        feed["version"]
    );
    let edit = handle(
        &context,
        post(
            "/api/element/update",
            json!({"kind": "vertex", "id": "1", "set": set})
                .to_string()
                .as_bytes(),
        ),
    );
    assert_eq!(body_json(&edit)["changed"], true, "{}", text(&edit));
    let checkpoint = handle(&context, post("/api/store/checkpoint", br#"{"name": "x"}"#));
    assert_eq!(checkpoint.status(), 200, "{}", text(&checkpoint));
    let run = handle(&context, post("/api/jobs/run", br#"{"name": "cp"}"#));
    assert_eq!(run.status(), 200, "{}", text(&run));
    // On again: the hook registered by the first turn is reused.
    context
        .set_read_only(true)
        .expect("read-only turns on again");
    let refused = execute(&context, "g.add_v(\"thing\").iterate()");
    assert!(
        refused.to_string().contains(READ_ONLY_HEADLINE),
        "{refused}"
    );
    assert_eq!(vertices(&context), 7);
}

#[test]
fn a_write_queued_before_the_flip_commits_and_one_after_is_refused() {
    let context = Context::builder(Runner::Session(modern_session()), check())
        .runtime_read_only(None)
        .build();
    let (started, running) = channel::<()>();
    let (release, released) = channel::<()>();
    let writer = {
        let context = context.clone();
        std::thread::spawn(move || {
            context
                .call(move |session| {
                    let _ = started.send(());
                    let _ = released.recv();
                    session.execute("g.add_v(\"early\").iterate()", &RunOptions::default())
                })
                .expect("the session answers")
        })
    };
    running
        .recv_timeout(Duration::from_secs(10))
        .expect("the write runs");
    let flipper = {
        let context = context.clone();
        std::thread::spawn(move || context.set_read_only(true))
    };
    // The flip waits behind the running write.
    std::thread::sleep(Duration::from_millis(50));
    assert!(
        !context.is_read_only(),
        "the flip waits for the running job"
    );
    release.send(()).expect("the write is released");
    let written: Value =
        serde_json::from_str(&writer.join().expect("the writer")).expect("a JSON answer");
    assert!(written["error"].is_null(), "{written}");
    flipper
        .join()
        .expect("the flipper")
        .expect("read-only turns on");
    assert!(context.is_read_only());
    assert_eq!(vertices(&context), 7, "the early write committed");
    let refused = execute(&context, "g.add_v(\"late\").iterate()");
    assert!(
        refused.to_string().contains(READ_ONLY_HEADLINE),
        "{refused}"
    );
    assert_eq!(vertices(&context), 7);
}

#[test]
fn a_context_built_read_only_refuses_from_its_first_job_with_the_hosts_help() {
    let help = "The database is frozen for the release; writes return tomorrow.";
    let context = Context::builder(Runner::Session(modern_session()), check())
        .read_only(true)
        .runtime_read_only(Some(help.to_string()))
        .build();
    assert!(context.is_read_only());
    assert_eq!(
        body_json(&handle(&context, get("/api/info")))["readOnly"],
        true
    );
    let refused = execute(&context, "g.add_v(\"thing\").iterate()");
    assert!(
        refused.to_string().contains(READ_ONLY_HEADLINE),
        "{refused}"
    );
    assert!(refused.to_string().contains(help), "{refused}");
    let answer = handle(&context, post("/api/jobs/define", br#"{"name": "x"}"#));
    assert_eq!(answer.status(), 409);
    assert_eq!(
        body_json(&answer)["error"],
        format!("{READ_ONLY_HEADLINE}. {help}")
    );
    // Save to file keeps its own refusal.
    let save = handle(&context, post("/api/save", b"{}"));
    assert_eq!(save.status(), 409);
    assert_eq!(
        body_json(&save)["error"],
        "the graph is served read-only: nothing is saved"
    );
    context.set_read_only(false).expect("read-only turns off");
    let written = execute(&context, "g.add_v(\"thing\").iterate()");
    assert!(written["error"].is_null(), "{written}");
    assert_eq!(vertices(&context), 7);
}

#[test]
fn without_the_toggle_read_only_is_fixed_and_cannot_be_set() {
    let context = Context::builder(Runner::Session(modern_session()), check())
        .read_only(true)
        .build();
    let err = context.set_read_only(false).expect_err("no toggle");
    assert!(matches!(err, ServeError::Unsupported(_)), "{err:?}");
    assert!(err.to_string().contains("runtime_read_only"), "{err}");
    assert!(context.is_read_only());
    // The host refuses the writes itself (the CLI's hook); the core only answers the flag.
    let feed = body_json(&handle(&context, get("/api/changes")));
    assert_eq!(feed["readOnly"], true, "{feed}");
    let written = execute(&context, "g.add_v(\"thing\").iterate()");
    assert!(written["error"].is_null(), "{written}");
    // The scheduled jobs are not refused by a toggle it does not have.
    let answer = handle(&context, post("/api/jobs/define", br#"{"name": "x"}"#));
    assert_ne!(answer.status(), 409, "{}", text(&answer));
}

/// A host queue that hands every job to one session thread and counts them.
struct Counted {
    jobs: Mutex<Sender<Job>>,
    submitted: AtomicUsize,
}

impl JobQueue for Counted {
    fn submit(&self, _client: &str, job: Job) -> Result<(), Busy> {
        self.submitted.fetch_add(1, Ordering::SeqCst);
        let _ = self.jobs.lock().expect("the sender").send(job);
        Ok(())
    }

    fn status(&self) -> Value {
        Value::Null
    }
}

#[test]
fn the_job_ticker_pauses_while_read_only() {
    let queue = Arc::new(Counted {
        jobs: Mutex::new(modern_session()),
        submitted: AtomicUsize::new(0),
    });
    let context = Context::builder(Runner::Queue(queue.clone()), check())
        .runtime_read_only(None)
        .build();
    let ticker = crate::context::start_ticker(&context, Duration::from_millis(5))
        .expect("the ticker starts");
    wait_for("two ticks", || queue.submitted.load(Ordering::SeqCst) >= 2);
    context.set_read_only(true).expect("read-only turns on");
    let paused = queue.submitted.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(60));
    assert_eq!(
        queue.submitted.load(Ordering::SeqCst),
        paused,
        "no tick reaches the session while read-only"
    );
    context.set_read_only(false).expect("read-only turns off");
    wait_for("ticks again", || {
        queue.submitted.load(Ordering::SeqCst) >= paused + 3
    });
    ticker.stop().expect("the ticker stops");
}

#[cfg(feature = "mcp")]
mod mcp {
    use super::*;
    use crate::{OneGraph, Server};

    /// `POST /mcp` of `body` with an optional session id: the minted session id (of an
    /// `initialize`) and the JSON answer of a `200`.
    fn exchange(server: &Server, body: &Value, session: Option<&str>) -> (Option<String>, Value) {
        let text = body.to_string();
        let mut request = post("/mcp", text.as_bytes()).with_header("Accept", "application/json");
        if let Some(session) = session {
            request = request.with_header("Mcp-Session-Id", session);
        }
        let answer = server.handle(request);
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
    fn an_open_mcp_session_and_a_page_see_the_toggle_without_reconnecting() {
        let context = store_context("read-only-mcp")
            .mcp(Some(graphersal_mcp::Mcp::new(false, None, false)))
            .runtime_read_only(None)
            .build();
        let server = Server::new();
        server
            .ready_with(Arc::new(OneGraph::new(context.clone())))
            .expect("ready once");
        let initialize = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": {"name": "tester", "version": "1"}}});
        let (session, _) = exchange(&server, &initialize, None);
        let session = session.expect("a session id");
        let write = tool_call("query", json!({"script": "g.add_v(\"agent\").iterate()"}));
        let count = tool_call("gql", json!({"query": "MATCH (n) RETURN count(n) AS c"}));
        let (_, answer) = exchange(&server, &write, Some(&session));
        assert_eq!(answer["result"]["isError"], false, "{answer}");
        let info = |server: &Server| body_json(&server.handle(get("/api/info")));
        assert_eq!(info(&server)["mcp"]["readOnly"], false);

        context.set_read_only(true).expect("read-only turns on");
        // The same MCP session: its write is refused with the one diagnostic, its read answers.
        let (_, refused) = exchange(&server, &write, Some(&session));
        assert_eq!(refused["result"]["isError"], true, "{refused}");
        assert!(
            tool_text(&refused).contains(READ_ONLY_HEADLINE),
            "{refused}"
        );
        let (_, answer) = exchange(&server, &count, Some(&session));
        assert!(tool_text(&answer).contains("\"c\":7"), "{answer}");
        // A page's write is refused too, its read answers.
        let page = execute(&context, "g.add_v(\"page\").iterate()");
        assert!(page.to_string().contains(READ_ONLY_HEADLINE), "{page}");
        assert_eq!(vertices(&context), 7);
        // What the endpoint reports follows the toggle (the profile stays as it is).
        assert_eq!(info(&server)["readOnly"], true);
        assert_eq!(info(&server)["mcp"]["readOnly"], true);
        let changes = body_json(&server.handle(get("/api/changes")));
        assert_eq!(changes["mcp"]["readOnly"], true, "{changes}");
        let stats = body_json(&server.handle(get("/api/mcp/stats")));
        assert_eq!(stats["readOnly"], true, "{stats}");
        let (_, graph_info) =
            exchange(&server, &tool_call("graph_info", json!({})), Some(&session));
        assert!(
            tool_text(&graph_info).contains("\"mcpReadOnly\":true"),
            "{graph_info}"
        );

        context.set_read_only(false).expect("read-only turns off");
        let (_, answer) = exchange(&server, &write, Some(&session));
        assert_eq!(answer["result"]["isError"], false, "{answer}");
        assert_eq!(vertices(&context), 8);
        assert_eq!(info(&server)["mcp"]["readOnly"], false);
    }
}
