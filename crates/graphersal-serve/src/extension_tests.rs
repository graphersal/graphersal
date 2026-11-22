//! Unit tests of the extension points (crate docs, "Extension points"): a test host that
//! resolves two graphs, observes and cancels runs, refuses requests in its filter and attaches
//! an identity, serves an extra route, sees the lifecycle (ready, stopping, activity), and runs
//! the jobs on a queue of its own; plus the lifecycle's probe answers while loading, ready and
//! stopping; and the reference multi-graph resolver over layers (module `reference_resolver`).
//! Real session threads on the sample graphs, no HTTP library.

use std::io::Cursor;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use graphersal_session::SessionPresets;
use serde_json::{Value, json};

use crate::{
    Busy, BusyReason, Context, Exposure, GraphResolver, Hooks, HostCheck, HttpError, Job, JobQueue,
    Lifecycle, LifecycleHooks, Method, Request, RequestFilter, Response, RunHandle, RunInfo,
    RunKind, RunObserver, Runner, Runs, SCHEDULED_JOBS_REFUSAL, Server, spawn_session,
};

const HOST: &str = "127.0.0.1:8080";

fn check() -> HostCheck {
    HostCheck::new(Exposure::Loopback, 8080, &format!("http://{HOST}/"))
}

/// A session thread with the sample `sample` loaded.
fn session_on(sample: &'static str) -> Runner {
    let (jobs, ()) = spawn_session(
        SessionPresets::new(),
        move |session| {
            session
                .load_sample(sample)
                .map(|_| ())
                .map_err(|err| err.to_string())
        },
        None,
    )
    .expect("the session thread starts");
    Runner::Session(jobs)
}

fn get<'a>(url: &str) -> Request<'a> {
    Request::new(Method::Get, url, Cursor::new(&b""[..])).with_header("Host", HOST)
}

fn post<'a>(url: &str, body: &'a [u8]) -> Request<'a> {
    Request::new(Method::Post, url, Cursor::new(body))
        .with_header("Host", HOST)
        .with_header("Content-Type", "application/json")
        .with_body_length(Some(body.len()))
}

fn body_json(response: &Response) -> Value {
    serde_json::from_slice(response.body()).expect("a JSON answer")
}

fn has_header(response: &Response, name: &str) -> bool {
    response
        .headers()
        .iter()
        .any(|(field, _)| field.eq_ignore_ascii_case(name))
}

/// The answer text of a `200` request.
fn execute(server: &Server, request: Request<'_>) -> String {
    let answer = server.handle(request);
    assert_eq!(
        answer.status(),
        200,
        "{}",
        String::from_utf8_lossy(answer.body())
    );
    String::from_utf8_lossy(answer.body()).into_owned()
}

/// Waits for `reached` (the test follows the server, not the clock).
fn wait_until(what: &str, reached: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !reached() {
        assert!(Instant::now() < deadline, "never reached: {what}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Two hosted graphs: `X-Graph: b` picks the second, no header the first, another name is a
/// `404`.
struct TwoGraphs {
    a: Arc<Context>,
    b: Arc<Context>,
}

impl GraphResolver for TwoGraphs {
    fn resolve(&self, request: &Request<'_>) -> Result<Arc<Context>, HttpError> {
        match request.header("X-Graph") {
            None | Some("a") => Ok(self.a.clone()),
            Some("b") => Ok(self.b.clone()),
            Some(other) => Err(HttpError::new(404, format!("unknown graph {other}"))),
        }
    }
}

#[test]
fn a_graph_resolver_serves_two_graphs() {
    let server = Server::new();
    server
        .ready_with(Arc::new(TwoGraphs {
            a: Context::builder(session_on("modern"), check()).build(),
            b: Context::builder(session_on("empty"), check()).build(),
        }))
        .expect("ready once");
    let count = |graph: &str| -> Value {
        let answer = execute(
            &server,
            post("/api/execute", br#"{"script": "g.v().count().next()"}"#)
                .with_header("X-Graph", graph),
        );
        serde_json::from_str::<Value>(&answer).expect("JSON")["execution"]["results"][0].clone()
    };
    assert_eq!(count("a"), 6);
    assert_eq!(count("b"), 0);
    // A change on one graph is not seen on the other.
    execute(
        &server,
        post("/api/execute", br#"{"script": "g.add_v(\"x\").iterate()"}"#)
            .with_header("X-Graph", "b"),
    );
    assert_eq!(count("a"), 6);
    assert_eq!(count("b"), 1);
    let unknown = server.handle(get("/api/info").with_header("X-Graph", "c"));
    assert_eq!(unknown.status(), 404);
    assert_eq!(body_json(&unknown)["error"], "unknown graph c");
}

/// Records every run's start (with its handle) and end.
#[derive(Default)]
struct Recorder {
    started: Mutex<Vec<(RunInfo, RunHandle)>>,
    ended: Mutex<Vec<(u64, Duration)>>,
}

impl Recorder {
    fn started(&self) -> Vec<(RunInfo, RunHandle)> {
        self.started.lock().unwrap().clone()
    }

    fn ended(&self) -> Vec<(u64, Duration)> {
        self.ended.lock().unwrap().clone()
    }
}

impl RunObserver for Recorder {
    fn run_started(&self, run: &RunInfo, handle: RunHandle) {
        self.started.lock().unwrap().push((run.clone(), handle));
    }

    fn run_ended(&self, run: &RunInfo, duration: Duration) {
        self.ended.lock().unwrap().push((run.id(), duration));
    }
}

#[test]
fn a_run_observer_sees_every_run_and_cancels_one() {
    const ENDLESS: &str = "g.inject(1).repeat(__.identity()).times(100000000).count().next()";
    let recorder = Arc::new(Recorder::default());
    let context = Context::builder(session_on("modern"), check())
        .runs(Arc::new(Runs::with_observer(recorder.clone())))
        .build();
    let server = Arc::new(Server::new());
    server.ready(context).expect("ready once");
    // A script longer than the head the observer is shown.
    let script = format!("{ENDLESS} // {}", "x".repeat(300));
    let body = json!({ "script": script, "runId": "r1" }).to_string();
    let running = {
        let server = server.clone();
        std::thread::spawn(move || {
            server.handle(
                post("/api/execute", body.as_bytes()).with_header("X-Graphersal-Client", "tab-a"),
            )
        })
    };
    wait_until("the run started", || recorder.started().len() == 1);
    let (info, handle) = recorder.started().remove(0);
    assert_eq!(info.kind(), RunKind::Query);
    assert_eq!(info.client(), "page:tab-a");
    assert_eq!(info.alias(), Some("r1"));
    assert_eq!(info.identity(), None);
    let head = info.text().expect("the query text");
    assert_eq!(head.chars().count(), crate::RUN_TEXT_HEAD_CHARS);
    assert!(head.starts_with(ENDLESS), "{head}");
    assert!(recorder.ended().is_empty());
    // The observer's handle cancels the run (no endpoint of the core does this for it).
    assert!(!handle.is_cancelled());
    handle.cancel();
    let answer = running.join().unwrap();
    assert_eq!(answer.status(), 200);
    assert!(
        String::from_utf8_lossy(answer.body())
            .to_lowercase()
            .contains("cancel")
    );
    wait_until("the run ended", || recorder.ended().len() == 1);
    assert_eq!(recorder.ended()[0].0, info.id());
    assert!(recorder.ended()[0].1 > Duration::ZERO);

    // A layout slice is a run of its own kind, without a text.
    let layout = server.handle(post(
        "/api/layout",
        br#"{"vertexCount": 2, "edgeSource": [0], "edgeTarget": [1], "iterations": 5}"#,
    ));
    assert_eq!(
        layout.status(),
        200,
        "{}",
        String::from_utf8_lossy(layout.body())
    );
    let started = recorder.started();
    assert_eq!(started.len(), 2);
    assert_eq!(started[1].0.kind(), RunKind::Layout);
    assert_eq!(started[1].0.text(), None);
    assert_eq!(recorder.ended().len(), 2);
}

/// A filter that needs `X-Token: secret` and attaches the identity `alice` (the `Host`/`Origin`
/// checks run before it anyway).
struct TokenFilter;

impl RequestFilter for TokenFilter {
    fn filter(&self, request: &mut Request<'_>) -> Result<(), HttpError> {
        if request.header("X-Token") != Some("secret") {
            return Err(HttpError::new(401, "a token is needed"));
        }
        request.set_identity("alice");
        Ok(())
    }
}

/// An extra route: `GET /api/whoami` answers the request's identity and client.
struct WhoAmI;

impl Hooks for WhoAmI {
    fn route(
        &self,
        _context: &Arc<Context>,
        request: &mut Request<'_>,
        endpoint: &str,
        _query: &str,
        client: &str,
    ) -> Option<Result<Response, HttpError>> {
        (endpoint == "whoami").then(|| {
            Ok(Response::json(
                200,
                &json!({ "identity": request.identity(), "client": client }),
            ))
        })
    }
}

#[test]
fn a_request_filter_refuses_requests_and_attaches_an_identity() {
    let recorder = Arc::new(Recorder::default());
    let context = Context::builder(session_on("modern"), check())
        .filter(Arc::new(TokenFilter))
        .hooks(Arc::new(WhoAmI))
        .runs(Arc::new(Runs::with_observer(recorder.clone())))
        .build();
    let server = Server::new();
    server.ready(context).expect("ready once");
    let refused = server.handle(get("/api/info"));
    assert_eq!(refused.status(), 401);
    assert_eq!(body_json(&refused)["error"], "a token is needed");
    // The checks run before the filter: a foreign Host is a 403 even with the token.
    let foreign = server.handle(
        Request::new(Method::Get, "/api/info", Cursor::new(&b""[..]))
            .with_header("Host", "evil.example:8080")
            .with_header("X-Token", "secret"),
    );
    assert_eq!(foreign.status(), 403);
    // Accepted: the extra route reads the identity the filter attached.
    let whoami = server.handle(
        get("/api/whoami")
            .with_header("X-Token", "secret")
            .with_header("X-Graphersal-Client", "tab-w"),
    );
    assert_eq!(whoami.status(), 200);
    assert_eq!(
        body_json(&whoami),
        json!({ "identity": "alice", "client": "tab-w" })
    );
    // A run of the request carries the identity to the observer.
    execute(
        &server,
        post("/api/execute", br#"{"script": "g.v().count().next()"}"#)
            .with_header("X-Token", "secret"),
    );
    let started = recorder.started();
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].0.identity(), Some("alice"));
    // The probes come before the filter.
    assert_eq!(server.handle(get("/healthz")).status(), 200);
}

/// A filter that would accept every request.
struct AcceptAll;

impl RequestFilter for AcceptAll {
    fn filter(&self, request: &mut Request<'_>) -> Result<(), HttpError> {
        request.set_identity("anyone");
        Ok(())
    }
}

#[test]
fn the_host_checks_run_even_with_a_filter_that_accepts_everything() {
    let server = Server::new();
    server
        .ready(
            Context::builder(session_on("modern"), check())
                .filter(Arc::new(AcceptAll))
                .build(),
        )
        .expect("ready once");
    let foreign = server.handle(
        Request::new(Method::Get, "/api/info", Cursor::new(&b""[..]))
            .with_header("Host", "evil.example:8080"),
    );
    assert_eq!(foreign.status(), 403);
    assert!(
        body_json(&foreign)["error"]
            .as_str()
            .unwrap()
            .starts_with("unknown Host"),
    );
    let no_host = server.handle(Request::new(
        Method::Get,
        "/api/info",
        Cursor::new(&b""[..]),
    ));
    assert_eq!(no_host.status(), 403);
    let cross_site = server.handle(get("/api/info").with_header("Origin", "http://evil.example"));
    assert_eq!(cross_site.status(), 403);
    assert_eq!(server.handle(get("/api/info")).status(), 200);
}

/// Answers `GET /api/hello`; every other unknown endpoint stays the core's 404.
struct Hello;

impl Hooks for Hello {
    fn route(
        &self,
        _context: &Arc<Context>,
        _request: &mut Request<'_>,
        endpoint: &str,
        _query: &str,
        _client: &str,
    ) -> Option<Result<Response, HttpError>> {
        (endpoint == "hello").then(|| Ok(Response::json(200, &json!({ "hello": "host" }))))
    }
}

#[test]
fn extra_routes_answer_the_endpoints_the_core_does_not_know() {
    let server = Server::new();
    server
        .ready(
            Context::builder(session_on("modern"), check())
                .hooks(Arc::new(Hello))
                .build(),
        )
        .expect("ready once");
    let hello = server.handle(get("/api/hello"));
    assert_eq!(hello.status(), 200);
    assert_eq!(body_json(&hello)["hello"], "host");
    // The core's endpoints stay the core's; the rest is the core's 404.
    assert_eq!(
        body_json(&server.handle(get("/api/info")))["server"],
        crate::DEFAULT_SERVER_NAME
    );
    let unknown = server.handle(get("/api/goodbye"));
    assert_eq!(unknown.status(), 404);
    assert_eq!(
        body_json(&unknown)["error"],
        "unknown endpoint /api/goodbye"
    );
}

/// Counts the lifecycle hooks.
#[derive(Default)]
struct Counts {
    ready: AtomicUsize,
    stopping: AtomicUsize,
    activity: AtomicUsize,
}

impl LifecycleHooks for Counts {
    fn ready(&self) {
        self.ready.fetch_add(1, Ordering::SeqCst);
    }

    fn stopping(&self) {
        self.stopping.fetch_add(1, Ordering::SeqCst);
    }

    fn activity(&self) {
        self.activity.fetch_add(1, Ordering::SeqCst);
    }
}

/// A request without any header (a probe needs none).
fn bare<'a>(method: Method, url: &str) -> Request<'a> {
    Request::new(method, url, Cursor::new(&b""[..]))
}

#[test]
fn the_lifecycle_answers_probes_and_503s_and_tells_the_hooks() {
    let counts = Arc::new(Counts::default());
    // The server exists before any graph: the probes answer while it loads.
    let server = Server::new().with_hooks(counts.clone());
    assert_eq!(server.state(), Lifecycle::Loading);
    // The probes need no Host header and ignore the query string.
    let live = server.handle(bare(Method::Get, "/healthz"));
    assert_eq!(
        (live.status(), body_json(&live)),
        (200, json!({ "status": "ok" }))
    );
    assert_eq!(server.handle(bare(Method::Head, "/healthz")).status(), 200);
    let loading = server.handle(bare(Method::Get, "/readyz?x=1"));
    assert_eq!(
        (loading.status(), body_json(&loading)),
        (503, json!({ "status": "loading" }))
    );
    let wrong = server.handle(bare(Method::Post, "/readyz"));
    assert_eq!(wrong.status(), 405);
    assert_eq!(
        body_json(&wrong)["error"],
        "method POST is not allowed on /readyz"
    );
    assert!(server.probe(&Method::Get, "/api/info").is_none());
    let early = server.handle(get("/api/info"));
    assert_eq!(early.status(), 503);
    assert_eq!(
        body_json(&early)["error"],
        "the server is still loading its graph; GET /readyz answers 200 when it serves queries"
    );
    assert!(!has_header(&early, "Retry-After"));
    // Probes and 503s are no activity.
    assert_eq!(counts.activity.load(Ordering::SeqCst), 0);

    server
        .ready(Context::builder(session_on("modern"), check()).build())
        .expect("ready once");
    assert_eq!(counts.ready.load(Ordering::SeqCst), 1);
    assert_eq!(server.state(), Lifecycle::Ready);
    assert!(
        server
            .ready(Context::builder(session_on("empty"), check()).build())
            .is_err()
    );
    let ready = server.handle(bare(Method::Get, "/readyz"));
    assert_eq!(
        (ready.status(), body_json(&ready)),
        (200, json!({ "status": "ready" }))
    );
    assert_eq!(counts.activity.load(Ordering::SeqCst), 0);
    // A request is activity (when it reaches its context and when it is answered).
    std::thread::sleep(Duration::from_millis(30));
    assert!(server.idle_for() >= Duration::from_millis(30));
    assert_eq!(server.handle(get("/api/info")).status(), 200);
    assert_eq!(counts.activity.load(Ordering::SeqCst), 2);
    assert!(server.idle_for() < Duration::from_millis(30));
    // The request filter is the context's: without Host the API refuses, the probes do not.
    assert_eq!(server.handle(bare(Method::Get, "/api/info")).status(), 403);
    assert_eq!(counts.activity.load(Ordering::SeqCst), 4);

    server.stop();
    server.stop();
    assert_eq!(counts.stopping.load(Ordering::SeqCst), 1);
    assert_eq!(server.state(), Lifecycle::Stopping);
    let stopping = server.handle(bare(Method::Get, "/readyz"));
    assert_eq!(
        (stopping.status(), body_json(&stopping)),
        (503, json!({ "status": "stopping" }))
    );
    assert_eq!(server.handle(bare(Method::Get, "/healthz")).status(), 200);
    let refused = server.handle(get("/api/info"));
    assert_eq!(refused.status(), 503);
    assert_eq!(body_json(&refused)["error"], "the server is stopping");
    assert!(!has_header(&refused, "Retry-After"));
    assert_eq!(counts.activity.load(Ordering::SeqCst), 4);
}

#[test]
fn a_stop_while_loading_answers_readyz_stopping_and_the_api_still_loading() {
    let server = Server::new();
    server.stop();
    let ready = server.handle(bare(Method::Get, "/readyz"));
    assert_eq!(body_json(&ready)["status"], "stopping");
    // As the dev server did: no graph yet, so the API answers "still loading".
    let api = server.handle(get("/api/info"));
    assert_eq!(api.status(), 503);
    assert!(
        body_json(&api)["error"]
            .as_str()
            .unwrap()
            .contains("still loading")
    );
    // Made ready after the stop: it stays stopping.
    server
        .ready(Context::builder(session_on("empty"), check()).build())
        .expect("ready once");
    assert_eq!(server.state(), Lifecycle::Stopping);
}

/// An executor policy of the host: its own worker (the session thread taken over), counting
/// the jobs, with its own refusal of scheduled jobs.
struct OwnQueue {
    jobs: Mutex<Sender<Job>>,
    submitted: AtomicUsize,
}

impl JobQueue for OwnQueue {
    fn submit(&self, _key: &str, job: Job) -> Result<(), Busy> {
        self.submitted.fetch_add(1, Ordering::SeqCst);
        self.jobs
            .lock()
            .unwrap()
            .send(job)
            .map_err(|_| Busy::new(BusyReason::Stopping, "gone"))
    }

    fn status(&self) -> Value {
        json!({ "submitted": self.submitted.load(Ordering::SeqCst) })
    }

    fn scheduled_jobs_refusal(&self) -> String {
        "no scheduled jobs on this host".to_string()
    }
}

/// A host queue without texts of its own that refuses every job.
struct Quiet;

impl JobQueue for Quiet {
    fn submit(&self, _key: &str, _job: Job) -> Result<(), Busy> {
        Err(Busy::new(BusyReason::QueueFull, "full"))
    }

    fn status(&self) -> Value {
        Value::Null
    }
}

#[test]
fn a_host_queue_is_the_executor_policy() {
    let (jobs, queued) = channel::<Job>();
    // The host queue's worker takes over the session thread once the graph is loaded.
    let takeover: Job = Box::new(move |session| {
        for job in queued {
            job(session);
        }
    });
    let (_unused, ()) = spawn_session(
        SessionPresets::new(),
        |session| {
            session
                .load_sample("modern")
                .map(|_| ())
                .map_err(|err| err.to_string())
        },
        Some(takeover),
    )
    .expect("the session thread starts");
    let queue = Arc::new(OwnQueue {
        jobs: Mutex::new(jobs),
        submitted: AtomicUsize::new(0),
    });
    let server = Server::new();
    server
        .ready(
            Context::builder(Runner::Queue(queue.clone()), check())
                .memory_tracked(true)
                .build(),
        )
        .expect("ready once");
    let count = execute(
        &server,
        post("/api/execute", br#"{"script": "g.v().count().next()"}"#),
    );
    let count: Value = serde_json::from_str(&count).expect("JSON");
    assert_eq!(count["execution"]["results"][0], 6);
    assert_eq!(queue.submitted.load(Ordering::SeqCst), 1);
    let info = body_json(&server.handle(get("/api/info")));
    assert_eq!(info["serving"]["submitted"], 1);
    assert_eq!(info["memoryTracked"], true);
    let jobs = server.handle(post("/api/jobs/run", br#"{"name": "checkpoint"}"#));
    assert_eq!(jobs.status(), 409);
    assert_eq!(body_json(&jobs)["error"], "no scheduled jobs on this host");

    // The defaults: one session thread (no `serving`, no memory tracking claimed), and a host
    // queue without its own text refuses scheduled jobs with the core's.
    let plain = Context::builder(session_on("modern"), check()).build();
    let info = body_json(&crate::handle(&plain, get("/api/info")));
    assert_eq!(info.get("serving"), None);
    assert_eq!(info["memoryTracked"], false);
    let quiet = Context::builder(Runner::Queue(Arc::new(Quiet)), check()).build();
    let refused = crate::handle(&quiet, post("/api/jobs/drop", br#"{"name": "x"}"#));
    assert_eq!(refused.status(), 409);
    assert_eq!(body_json(&refused)["error"], SCHEDULED_JOBS_REFUSAL);
}

#[cfg(feature = "mcp")]
#[test]
fn a_run_observer_sees_the_mcp_tool_runs_with_their_text() {
    let recorder = Arc::new(Recorder::default());
    let context = Context::builder(session_on("modern"), check())
        .runs(Arc::new(Runs::with_observer(recorder.clone())))
        .mcp(Some(graphersal_mcp::Mcp::new(true, None, false)))
        .build();
    let server = Server::new();
    server.ready(context).expect("ready once");
    let mcp = |body: Value, session: Option<&str>| {
        let text = body.to_string();
        let mut request = post("/mcp", text.as_bytes()).with_header("Accept", "application/json");
        if let Some(session) = session {
            request = request.with_header("Mcp-Session-Id", session);
        }
        let answer = server.handle(request);
        assert_eq!(
            answer.status(),
            200,
            "{}",
            String::from_utf8_lossy(answer.body())
        );
        answer
    };
    let initialized = mcp(
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
               "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                          "clientInfo": {"name": "observer-test", "version": "1"}}}),
        None,
    );
    let session = initialized
        .headers()
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("Mcp-Session-Id"))
        .map(|(_, value)| value.clone())
        .expect("a session id");
    mcp(
        json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call",
               "params": {"name": "query", "arguments": {"script": "g.v().count().next()"}}}),
        Some(&session),
    );
    let started = recorder.started();
    assert_eq!(started.len(), 1);
    let info = &started[0].0;
    assert_eq!(info.kind(), RunKind::Tool);
    assert_eq!(info.text(), Some("g.v().count().next()"));
    assert_eq!(info.client(), format!("mcp:{session}"));
    assert_eq!(info.alias(), Some("7"));
    assert_eq!(recorder.ended().len(), 1);
}

/// `POST /mcp` of `body` on `server` (with `headers`): the JSON answer of a `200`.
#[cfg(feature = "mcp")]
fn mcp_post(server: &Server, body: Value, headers: &[(&str, &str)]) -> Value {
    let text = body.to_string();
    let mut request = post("/mcp", text.as_bytes()).with_header("Accept", "application/json");
    for (name, value) in headers {
        request = request.with_header(name, value);
    }
    let answer = server.handle(request);
    assert_eq!(
        answer.status(),
        200,
        "{}",
        String::from_utf8_lossy(answer.body())
    );
    body_json(&answer)
}

/// A `tools/call` of `name` with `arguments` as the JSON-RPC body.
#[cfg(feature = "mcp")]
fn tool_call(name: &str, arguments: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
           "params": {"name": name, "arguments": arguments}})
}

/// The text of a tool answer.
#[cfg(feature = "mcp")]
fn tool_text(answer: &Value) -> String {
    answer["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

#[cfg(feature = "mcp")]
#[test]
fn one_mcp_endpoint_serves_two_graphs_behind_the_resolver() {
    let mcp = Arc::new(graphersal_mcp::Mcp::new(false, None, false));
    let server = Server::new();
    server
        .ready_with(Arc::new(TwoGraphs {
            a: Context::builder(session_on("modern"), check())
                .shared_mcp(mcp.clone())
                .build(),
            b: Context::builder(session_on("empty"), check())
                .shared_mcp(mcp.clone())
                .build(),
        }))
        .expect("ready once");
    let count = |graph: &str| -> String {
        let answer = mcp_post(
            &server,
            tool_call("gql", json!({"query": "MATCH (n) RETURN count(n) AS c"})),
            &[("X-Graph", graph)],
        );
        assert_eq!(answer["result"]["isError"], false, "{answer}");
        tool_text(&answer)
    };
    let (a, b) = (count("a"), count("b"));
    assert!(a.contains("\"c\":6"), "{a}");
    assert!(b.contains("\"c\":0"), "{b}");
    // The endpoint's statistics count the calls of both graphs.
    let stats = mcp.stats().to_json().expect("statistics");
    let gql = stats["tools"]
        .as_array()
        .and_then(|tools| tools.iter().find(|tool| tool["name"] == "gql"))
        .cloned()
        .unwrap_or(Value::Null);
    assert_eq!(gql["calls"], 2, "{stats}");
}

#[cfg(feature = "mcp")]
#[test]
fn two_contexts_over_one_session_serve_two_mcp_profiles() {
    use graphersal::auth::AccessPolicy;
    use graphersal_mcp::Profile;

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
    let mcp = Arc::new(graphersal_mcp::Mcp::new(false, None, false));
    // `X-Graph: b` is the "use" side (reads only, by its access policy), no header "develop".
    let server = Server::new();
    server
        .ready_with(Arc::new(TwoGraphs {
            a: Context::builder(Runner::Session(jobs.clone()), check())
                .shared_mcp(mcp.clone())
                .build(),
            b: Context::builder(Runner::Session(jobs), check())
                .shared_mcp(mcp.clone())
                .mcp_profile(Profile::new(true).with_access_policy(AccessPolicy::read_only()))
                .build(),
        }))
        .expect("ready once");
    let write = tool_call("query", json!({"script": "g.add_v(\"thing\").iterate()"}));
    let refused = mcp_post(&server, write.clone(), &[("X-Graph", "b")]);
    assert_eq!(refused["result"]["isError"], true, "{refused}");
    assert!(
        tool_text(&refused).contains("Permission denied"),
        "{refused}"
    );
    let allowed = mcp_post(&server, write, &[]);
    assert_eq!(allowed["result"]["isError"], false, "{allowed}");
    // Both see the one graph (the write of the develop side).
    let count = tool_call(
        "gql",
        json!({"query": "MATCH (n:thing) RETURN count(n) AS c"}),
    );
    assert!(tool_text(&mcp_post(&server, count, &[("X-Graph", "b")])).contains("\"c\":1"));
    // What each side reports of itself follows its profile.
    let info = mcp_post(
        &server,
        tool_call("graph_info", json!({})),
        &[("X-Graph", "b")],
    );
    assert!(tool_text(&info).contains("\"mcpReadOnly\":true"), "{info}");
    let info = mcp_post(&server, tool_call("graph_info", json!({})), &[]);
    assert!(tool_text(&info).contains("\"mcpReadOnly\":false"), "{info}");
    let api =
        |graph: &str| body_json(&server.handle(get("/api/info").with_header("X-Graph", graph)));
    assert_eq!(api("b")["mcp"]["readOnly"], true);
    assert_eq!(api("a")["mcp"]["readOnly"], false);
}

#[cfg(feature = "mcp")]
#[test]
fn an_mcp_connection_of_a_context_runs_there_with_its_identity() {
    let recorder = Arc::new(Recorder::default());
    let context = Context::builder(session_on("modern"), check())
        .runs(Arc::new(Runs::with_observer(recorder.clone())))
        .mcp(Some(graphersal_mcp::Mcp::new(false, None, false)))
        .build();
    let connection = context
        .mcp_connection()
        .expect("the context has an endpoint")
        .with_identity("carol");
    let lines = connection
        .handle(&tool_call("query", json!({"script": "g.v().count().next()"})).to_string());
    let answer: Value = serde_json::from_str(&lines[0]).expect("JSON");
    assert!(tool_text(&answer).contains("\n6"), "{answer}");
    let started = recorder.started();
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].0.kind(), RunKind::Tool);
    assert_eq!(started[0].0.identity(), Some("carol"));
    assert_eq!(started[0].0.client(), format!("mcp:{}", connection.key()));
    // Without an endpoint there is no connection.
    let plain = Context::builder(session_on("empty"), check()).build();
    let missing = plain
        .mcp_connection()
        .expect_err("no endpoint, no connection");
    assert!(missing.to_string().contains("no MCP endpoint"), "{missing}");
}

#[cfg(feature = "mcp")]
#[test]
fn the_identity_of_the_request_filter_reaches_the_mcp_runs() {
    let recorder = Arc::new(Recorder::default());
    let context = Context::builder(session_on("modern"), check())
        .filter(Arc::new(TokenFilter))
        .runs(Arc::new(Runs::with_observer(recorder.clone())))
        .mcp(Some(graphersal_mcp::Mcp::new(false, None, false)))
        .build();
    let server = Server::new();
    server.ready(context).expect("ready once");
    let answer = mcp_post(
        &server,
        tool_call("query", json!({"script": "1"})),
        &[("X-Token", "secret")],
    );
    assert_eq!(answer["result"]["isError"], false, "{answer}");
    let started = recorder.started();
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].0.identity(), Some("alice"));
}

#[test]
fn a_time_zone_the_session_refuses_is_an_error_of_the_job_ticker() {
    let context = Context::builder(session_on("empty"), check()).build();
    let err = crate::start_job_ticker(&context, Some("Nowhere/Land".to_string()))
        .expect_err("an unknown zone is refused");
    assert!(matches!(err, crate::ServeError::Session(_)), "{err:?}");
    assert!(err.to_string().contains("Nowhere/Land"), "{err}");
    // A zone the session knows starts the ticker as before.
    crate::start_job_ticker(&context, Some("Europe/Berlin".to_string()))
        .expect("a known zone is taken");
}

#[test]
fn a_setup_failure_keeps_the_hosts_error_as_its_source() {
    let err = spawn_session(
        SessionPresets::new(),
        |_session| -> Result<(), std::io::Error> {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Error: no such graph file",
            ))
        },
        None,
    )
    .expect_err("the setup fails");
    assert!(matches!(err, crate::ServeError::Setup(_)), "{err:?}");
    assert_eq!(err.to_string(), "Error: no such graph file");
    let source = std::error::Error::source(&err).expect("the host's error is the source");
    let io = source
        .downcast_ref::<std::io::Error>()
        .expect("the source is the host's own error type");
    assert_eq!(io.kind(), std::io::ErrorKind::NotFound);
    // A `String` diagnostic is shown as it is.
    let err = spawn_session(
        SessionPresets::new(),
        |_session| -> Result<(), String> { Err("Error: cannot load".to_string()) },
        None,
    )
    .expect_err("the setup fails");
    assert_eq!(err.to_string(), "Error: cannot load");
}

thread_local! {
    /// Dropped when its thread ends: tells a test that the session thread is gone.
    static ENDED: std::cell::RefCell<Option<Sender<()>>> = const { std::cell::RefCell::new(None) };
}

/// A session thread on the `modern` sample whose end the returned receiver sees (its channel
/// closes), behind a queue that counts the jobs ([`OwnQueue`]).
fn counted_session() -> (Arc<OwnQueue>, std::sync::mpsc::Receiver<()>) {
    let (ended, session_ended) = channel::<()>();
    let (jobs, ()) = spawn_session(
        SessionPresets::new(),
        move |session| {
            ENDED.with(|slot| *slot.borrow_mut() = Some(ended));
            session
                .load_sample("modern")
                .map(|_| ())
                .map_err(|err| err.to_string())
        },
        None,
    )
    .expect("the session thread starts");
    let queue = Arc::new(OwnQueue {
        jobs: Mutex::new(jobs),
        submitted: AtomicUsize::new(0),
    });
    (queue, session_ended)
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
fn a_stopped_job_ticker_lets_the_context_and_the_session_go() {
    let (queue, session_ended) = counted_session();
    let context = Context::builder(Runner::Queue(queue.clone()), check()).build();
    let ticker = crate::context::start_ticker(&context, Duration::from_millis(5))
        .expect("the ticker starts");
    wait_for("two ticks", || queue.submitted.load(Ordering::SeqCst) >= 2);
    ticker.stop().expect("the ticker stops");
    let ticks = queue.submitted.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(
        queue.submitted.load(Ordering::SeqCst),
        ticks,
        "no tick after stop"
    );
    // The ticker holds nothing: the host's context is the last one, and with it the job queue
    // goes, so the session thread ends.
    let weak = Arc::downgrade(&context);
    drop(context);
    assert!(
        weak.upgrade().is_none(),
        "the stopped ticker keeps no context"
    );
    drop(queue);
    session_ended
        .recv_timeout(Duration::from_secs(10))
        .expect_err("the session thread ends");
}

#[test]
fn a_dropped_job_ticker_ticks_until_the_context_is_gone() {
    let (queue, session_ended) = counted_session();
    let context = Context::builder(Runner::Queue(queue.clone()), check()).build();
    let ticker = crate::context::start_ticker(&context, Duration::from_millis(5))
        .expect("the ticker starts");
    drop(ticker);
    // The handle is gone, the ticker is not (the CLI's dev server keeps no handle).
    let ticks = queue.submitted.load(Ordering::SeqCst);
    wait_for("ticks after the handle is dropped", || {
        queue.submitted.load(Ordering::SeqCst) >= ticks + 2
    });
    // It holds the context only during a tick: it ends once the host dropped its context.
    let weak = Arc::downgrade(&context);
    drop(context);
    wait_for("the context to go", || weak.upgrade().is_none());
    drop(queue);
    session_ended
        .recv_timeout(Duration::from_secs(10))
        .expect_err("the session thread ends");
}

#[test]
fn background_runs_count_the_job_runs_until_their_result_is_back() {
    let (jobs, ()) = spawn_session(
        SessionPresets::new(),
        |session| -> Result<(), String> {
            // A store in memory: the scheduled jobs belong to a store.
            session.set_memory_stores(true);
            let query = session.define_query(&json!({
                "name": "slow",
                "body": "fn slow() { let n = 0; for i in 0..100000000 { n += g.v().count().next(); } n }",
            }));
            assert_eq!(query["ok"], true, "{query}");
            let job = session.define_job(&json!({
                "name": "slow_job", "schedule": "0 3 1 1 *", "action": "query",
                "query": "slow", "timeout_ms": 300,
            }));
            assert_eq!(job["ok"], true, "{job}");
            Ok(())
        },
        None,
    )
    .expect("the session thread starts");
    let context = Context::builder(Runner::Session(jobs), check()).build();
    assert_eq!(context.background_runs().expect("counted"), 0);
    let (before, run) = context
        .call_fallible(|s| {
            let before = crate::context::position(s).ok();
            s.start_job_now("slow_job").map(|run| (before, run))
        })
        .expect("the job starts");
    let (done, finished) = channel();
    crate::job_workers::start(&context, before, run, Some(done));
    // Running on its worker (its timeout ends it): counted, the session answers meanwhile.
    assert_eq!(context.background_runs().expect("counted"), 1);
    assert_eq!(context.call(|_| 1).expect("the session answers"), 1);
    finished
        .recv_timeout(Duration::from_secs(30))
        .expect("the run finishes");
    // Its result is back in the session before the caller is told.
    assert_eq!(context.background_runs().expect("counted"), 0);
    let entry = context
        .call_fallible(|s| s.job_entry("slow_job"))
        .expect("the job's entry");
    assert_eq!(entry["running"], false, "{entry}");
    // The worker holds its context weakly: dropping the host's context lets it go.
    let weak = Arc::downgrade(&context);
    drop(context);
    assert!(weak.upgrade().is_none(), "the job worker keeps no context");
}

#[test]
fn the_runtime_errors_of_call_answer_the_http_status_of_before() {
    // A refusing host queue: `Busy`, a 503 with the queue's text.
    let full = Context::builder(Runner::Queue(Arc::new(Quiet)), check()).build();
    let err = full.call(|_| ()).expect_err("refused");
    let crate::ServeError::Busy(busy) = &err else {
        panic!("expected Busy, got {err:?}");
    };
    let refusal = busy.message().to_string();
    assert_eq!(err.to_string(), refusal);
    assert_eq!(HttpError::from(err), HttpError::new(503, refusal));
    // A session method's diagnostic: `Failed`, a 422.
    let context = Context::builder(session_on("modern"), check()).build();
    let err = context
        .call_fallible(|_| Err::<(), _>("no such thing"))
        .expect_err("the method fails");
    assert!(matches!(err, crate::ServeError::Failed(_)), "{err:?}");
    assert_eq!(HttpError::from(err), HttpError::new(422, "no such thing"));
    // A session that is not running: `Session`, a 500 with the text of before.
    let (jobs, queue) = channel::<Job>();
    drop(queue);
    let gone = Context::builder(Runner::Session(jobs), check()).build();
    let err = gone.call(|_| ()).expect_err("no session");
    assert!(matches!(err, crate::ServeError::Session(_)), "{err:?}");
    assert_eq!(
        HttpError::from(err),
        HttpError::new(500, "the session is not running")
    );
    // A job that panics answers nothing: `Session`, a 500.
    let err = context
        .call(|_| -> () { panic!("a bug in a job") })
        .expect_err("the job panicked");
    assert_eq!(
        HttpError::from(err),
        HttpError::new(500, "the request failed inside the session")
    );
}

#[cfg(feature = "ui-assets")]
/// A host that serves the files of its own UI profile before the core (graphersal-memory's
/// `ui` process): the path below `/memory/` is its own, every other request goes to
/// [`Server::handle`], whose [`crate::Ui`] answers the playground's files. Public API only:
/// [`HostCheck::check`] keeps the same `Host`/`Origin` rules for the host's own files.
fn host_with_profile_files(server: &Server, check: &HostCheck, request: Request<'_>) -> Response {
    if let Some(name) = request.url().strip_prefix("/memory/") {
        if let Err(err) = check.check(&request) {
            return Response::text(err.status(), err.message().to_string());
        }
        return match name {
            "overview.js" => {
                Response::new(200, b"export default { template: '<p>hi</p>' };".to_vec())
                    .with_header("Content-Type", "text/javascript; charset=utf-8")
                    .with_header("X-Content-Type-Options", "nosniff")
            }
            _ => Response::text(404, "not found".to_string()),
        };
    }
    server.handle(request)
}

#[cfg(feature = "ui-assets")]
#[test]
fn a_host_serves_its_profile_files_first_and_the_core_the_rest() {
    let profile = json!({"name": "memory", "title": "Memory", "startTab": "overview",
        "can": {"theme": true}, "tabs": [{"id": "overview", "title": "Overview",
        "module": "overview.js"}], "files": "/memory/"});
    let server = Server::new();
    server
        .ready(
            Context::builder(session_on("empty"), check())
                .ui_profile(profile.clone())
                .build(),
        )
        .expect("ready once");
    let check = check();
    // The profile reaches the page through /api/info.
    let info = host_with_profile_files(&server, &check, get("/api/info"));
    assert_eq!(body_json(&info)["uiProfile"], profile);
    // The host's own module.
    let module = host_with_profile_files(&server, &check, get("/memory/overview.js"));
    assert_eq!(module.status(), 200);
    assert!(String::from_utf8_lossy(module.body()).starts_with("export default"));
    // The playground's files still come from the core's UI.
    let page = host_with_profile_files(&server, &check, get("/"));
    assert_eq!(page.status(), 200, "the embedded or directory index.html");
    let model = host_with_profile_files(&server, &check, get("/js/ui-profiles.js"));
    assert_eq!(
        model.status(),
        200,
        "the profile model is part of the playground"
    );
    // The host's files keep the core's Host check.
    let foreign = Request::new(Method::Get, "/memory/overview.js", Cursor::new(&b""[..]))
        .with_header("Host", "evil.example:8080");
    assert_eq!(
        host_with_profile_files(&server, &check, foreign).status(),
        403
    );
}

/// Two versions of one graph behind an alias (`acme` -> `acme@1` or `acme@2`): a request whose
/// `Mcp-Session-Id` carries a route tag goes to that version, every other one to the alias's
/// current version (S5.2, `ContextBuilder::mcp_session_route`).
#[cfg(feature = "mcp")]
struct PinnedVersions {
    versions: Vec<(&'static str, Arc<Context>)>,
    alias: Mutex<&'static str>,
}

#[cfg(feature = "mcp")]
impl GraphResolver for PinnedVersions {
    fn resolve(&self, request: &Request<'_>) -> Result<Arc<Context>, HttpError> {
        let route = request
            .header("Mcp-Session-Id")
            .and_then(graphersal_mcp::session_route);
        let name = match route {
            Some(route) => route,
            None => *self.alias.lock().expect("alias"),
        };
        self.versions
            .iter()
            .find(|(version, _)| *version == name)
            .map(|(_, context)| context.clone())
            .ok_or_else(|| HttpError::new(404, format!("unknown graph version {name}")))
    }
}

/// `POST /mcp` of `body` on `server` with an optional session id: the status, the minted
/// session id (of an `initialize`) and the JSON answer.
#[cfg(feature = "mcp")]
fn mcp_exchange(
    server: &Server,
    body: &Value,
    session: Option<&str>,
) -> (u16, Option<String>, Value) {
    let text = body.to_string();
    let mut request = post("/mcp", text.as_bytes()).with_header("Accept", "application/json");
    if let Some(session) = session {
        request = request.with_header("Mcp-Session-Id", session);
    }
    let answer = server.handle(request);
    let id = answer
        .headers()
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("mcp-session-id"))
        .map(|(_, value)| value.clone());
    let json = serde_json::from_slice(answer.body()).unwrap_or(Value::Null);
    (answer.status(), id, json)
}

#[cfg(feature = "mcp")]
#[test]
fn a_resolver_reading_the_session_route_keeps_a_session_on_its_version() {
    let initialize = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {},
        "clientInfo": {"name": "tester", "version": "1"}}});
    let count = tool_call("gql", json!({"query": "MATCH (n) RETURN count(n) AS c"}));
    let key = graphersal_mcp::SessionKey::new(b"a shared secret of the replicas, 32+ bytes")
        .expect("a valid key");
    for signed in [false, true] {
        let mut mcp = graphersal_mcp::Mcp::new(true, None, false);
        if signed {
            mcp = mcp.with_session_key(key.clone());
        }
        let mcp = Arc::new(mcp);
        let version = |sample: &'static str, route: &str| {
            Context::builder(session_on(sample), check())
                .shared_mcp(mcp.clone())
                .mcp_session_route(Some(route.to_string()))
                .build()
        };
        let resolver = Arc::new(PinnedVersions {
            versions: vec![
                ("acme@1", version("modern", "acme@1")),
                ("acme@2", version("empty", "acme@2")),
            ],
            alias: Mutex::new("acme@1"),
        });
        let server = Server::new();
        server.ready_with(resolver.clone()).expect("ready once");
        let (status, old, _) = mcp_exchange(&server, &initialize, None);
        assert_eq!(status, 200);
        let old = old.expect("a session id");
        assert_eq!(graphersal_mcp::session_route(&old), Some("acme@1"), "{old}");
        // The release flips the alias: the open session finishes on its version, a new one
        // sees the new version.
        *resolver.alias.lock().unwrap() = "acme@2";
        let (_, _, answer) = mcp_exchange(&server, &count, Some(&old));
        assert!(
            tool_text(&answer).contains("\"c\":6"),
            "signed {signed}: {answer}"
        );
        let (_, new, _) = mcp_exchange(&server, &initialize, None);
        let new = new.expect("a session id");
        assert_eq!(graphersal_mcp::session_route(&new), Some("acme@2"), "{new}");
        let (_, _, answer) = mcp_exchange(&server, &count, Some(&new));
        assert!(
            tool_text(&answer).contains("\"c\":0"),
            "signed {signed}: {answer}"
        );
        // Editing the route reaches the other version's context, whose endpoint refuses the id.
        let forged = old.replacen("acme@1~", "acme@2~", 1);
        let (status, _, _) = mcp_exchange(&server, &count, Some(&forged));
        assert_eq!(status, 404, "signed {signed}");
        assert_eq!(mcp.verified_session_route(&forged), None);
        assert_eq!(mcp.verified_session_route(&old), Some("acme@1"));
    }
}

/// The reference multi-graph resolver (layered-graph plan S5.3; book page "Serving Layers" for
/// developers): how a host serves many versioned graphs behind one MCP endpoint with this
/// repository's pieces, without a base pool or alias catalog in this crate (one graph per
/// process is its rule; the full multi-graph server is a future server's).
///
/// - a **base pool** keyed by [`BaseRef`]: one [`LayerBase`] per base commit, loaded read-only
///   from a versioned base store at a mark, shared by every layer over it;
/// - a **catalog**: versions (`acme@2026.3` = a layer over `product@2026.3`), aliases (`acme`
///   -> its current version) and explicit pins the host keeps;
/// - the host's **path rewrite** (`/mcp/acme` -> `/mcp` + the name in a header the client
///   cannot set);
/// - one context per version, built with `.layered(true)`,
///   `.mcp_session_route(Some("<name>@<version>"))`, the one shared `Arc<Mcp>` and a profile
///   of its own;
/// - the **name check**: a session's route ([`graphersal_mcp::Mcp::verified_session_route`])
///   must be a version of the name in the path;
/// - the **retirement** of versions no alias, pin or recent request needs, and the eviction of
///   bases no layer reads any more.
#[cfg(feature = "mcp")]
mod reference_resolver {
    use std::collections::{HashMap, HashSet};
    use std::sync::Weak;

    use graphersal::auth::AccessPolicy;
    use graphersal::persist::{MemDir, RecoveryTarget, Store, StoreOptions};
    use graphersal::prelude::*;
    use graphersal::storage::{BaseRef, LayerBase, LayeredGraph};
    use graphersal_mcp::{Mcp, Profile, SessionKey};

    use super::*;

    /// The header carrying the graph name of the path. The host sets it from the path and drops
    /// any copy the client sent, so only the path names the graph.
    const GRAPH_HEADER: &str = "X-Graphersal-Graph";

    /// One served version: its context, the base it reads and when a request last used it.
    struct Served {
        context: Arc<Context>,
        base: BaseRef,
        last_used: Instant,
    }

    /// Versions, aliases and the explicit pins the host keeps (never retired).
    #[derive(Default)]
    struct Catalog {
        versions: HashMap<String, Served>,
        aliases: HashMap<String, String>,
        kept: HashSet<String>,
    }

    /// The reference resolver: base pool + catalog + the shared MCP endpoint.
    struct LayerHost {
        mcp: Arc<Mcp>,
        pool: Mutex<HashMap<BaseRef, LayerBase>>,
        catalog: Mutex<Catalog>,
    }

    /// `acme@2026.3` -> (`acme`, `2026.3`).
    fn split(version: &str) -> Option<(&str, &str)> {
        version.split_once('@')
    }

    impl LayerHost {
        fn new(mcp: Arc<Mcp>) -> Self {
            Self {
                mcp,
                pool: Mutex::new(HashMap::new()),
                catalog: Mutex::new(Catalog::default()),
            }
        }

        /// The base of the store `dir` at `mark`, from the pool when that commit is loaded
        /// already (same `BaseRef`), else loaded read-only and pooled.
        fn base(&self, dir: &MemDir, mark: &str) -> LayerBase {
            let loaded = LayerBase::from_store_read_only(dir, RecoveryTarget::Mark(mark.into()))
                .expect("the base store opens at the mark")
                .with_name(format!("product@{mark}"));
            let key = *loaded.base_ref().expect("a base identity");
            self.pool
                .lock()
                .unwrap()
                .entry(key)
                .or_insert(loaded)
                .clone()
        }

        /// Publishes `version` (`<name>@<version>`) as a new layer over `base` that `setup`
        /// fills, served by a context of its own with `profile`. Returns the layer (to watch it
        /// go away).
        fn publish(
            &self,
            version: &str,
            base: &LayerBase,
            profile: Profile,
            setup: impl FnOnce(&Graph<LayeredGraph>),
        ) -> Weak<Graph<LayeredGraph>> {
            let layer = Arc::new(base.new_layer().expect("a layer over the base"));
            setup(&layer);
            let watched = Arc::downgrade(&layer);
            let name = version.to_string();
            let (jobs, ()) = spawn_session(
                SessionPresets::new(),
                move |session| {
                    session
                        .load_shared(layer, &name, "layer")
                        .map(|_| ())
                        .map_err(|err| err.to_string())
                },
                None,
            )
            .expect("the session thread starts");
            let context = Context::builder(Runner::Session(jobs), check())
                .layered(true)
                .shared_mcp(self.mcp.clone())
                .mcp_profile(profile)
                .mcp_session_route(Some(version.to_string()))
                .build();
            let served = Served {
                context,
                base: *base.base_ref().expect("a base identity"),
                last_used: Instant::now(),
            };
            self.catalog
                .lock()
                .unwrap()
                .versions
                .insert(version.to_string(), served);
            watched
        }

        /// The release: `alias` names `version` for every new session from now on.
        fn flip(&self, alias: &str, version: &str) {
            let mut catalog = self.catalog.lock().unwrap();
            assert!(catalog.versions.contains_key(version), "{version}");
            catalog.aliases.insert(alias.into(), version.into());
        }

        /// Drops every version that no alias names, no explicit pin keeps and no request used
        /// for `grace`: its context goes, so its session thread ends and its layer is dropped;
        /// a session still routed to it gets `404` and starts again on the current version.
        /// Returns the retired versions.
        fn retire_idle(&self, grace: Duration) -> Vec<String> {
            let mut catalog = self.catalog.lock().unwrap();
            let needed: HashSet<String> = catalog
                .aliases
                .values()
                .chain(catalog.kept.iter())
                .cloned()
                .collect();
            let retired: Vec<String> = catalog
                .versions
                .iter()
                .filter(|(version, served)| {
                    !needed.contains(*version) && served.last_used.elapsed() >= grace
                })
                .map(|(version, _)| version.clone())
                .collect();
            for version in &retired {
                catalog.versions.remove(version);
            }
            retired
        }

        /// Drops the pooled bases no served version reads any more. Returns how many.
        fn evict_unused_bases(&self) -> usize {
            let read: HashSet<BaseRef> = {
                let catalog = self.catalog.lock().unwrap();
                catalog
                    .versions
                    .values()
                    .map(|served| served.base)
                    .collect()
            };
            let mut pool = self.pool.lock().unwrap();
            let before = pool.len();
            pool.retain(|key, _| read.contains(key));
            before - pool.len()
        }
    }

    impl GraphResolver for LayerHost {
        fn resolve(&self, request: &Request<'_>) -> Result<Arc<Context>, HttpError> {
            let name = request
                .header(GRAPH_HEADER)
                .ok_or_else(|| HttpError::new(404, "no graph name"))?;
            let routed = match request.header("Mcp-Session-Id") {
                Some(id) => {
                    // Known to the endpoint (a signed id: its signature; table mode: in the
                    // table), and a version of the name in the path.
                    let route = self
                        .mcp
                        .verified_session_route(id)
                        .ok_or_else(|| HttpError::new(404, "unknown MCP session"))?;
                    let belongs = if name.contains('@') {
                        route == name
                    } else {
                        split(route).is_some_and(|(of, _)| of == name)
                    };
                    if !belongs {
                        return Err(HttpError::new(404, "unknown MCP session"));
                    }
                    Some(route)
                }
                None => None,
            };
            let mut catalog = self.catalog.lock().unwrap();
            let version = match routed {
                Some(route) => route.to_string(),
                None if name.contains('@') => name.to_string(),
                None => catalog
                    .aliases
                    .get(name)
                    .cloned()
                    .ok_or_else(|| HttpError::new(404, format!("unknown graph {name}")))?,
            };
            let served = catalog
                .versions
                .get_mut(&version)
                .ok_or_else(|| HttpError::new(404, format!("unknown graph {name}")))?;
            served.last_used = Instant::now();
            Ok(served.context.clone())
        }
    }

    /// The host's listener side: `POST /mcp/<name>` becomes `POST /mcp` with the name in
    /// [`GRAPH_HEADER`] (a client's own copy of that header is dropped); any other path is a
    /// `404` of the host.
    fn rewrite<'a>(
        path: &str,
        body: &'a [u8],
        client_headers: &[(&str, &str)],
    ) -> Result<Request<'a>, HttpError> {
        let name = path
            .strip_prefix("/mcp/")
            .filter(|name| {
                !name.is_empty()
                    && name.len() <= 64
                    && name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._-@".contains(&b))
            })
            .ok_or_else(|| HttpError::new(404, "no such endpoint"))?;
        let mut request = post("/mcp", body).with_header("Accept", "application/json");
        for (field, value) in client_headers {
            if !field.eq_ignore_ascii_case(GRAPH_HEADER) {
                request = request.with_header(field, value);
            }
        }
        Ok(request.with_header(GRAPH_HEADER, name))
    }

    /// One client request on the host: the status, the minted session id and the JSON answer.
    fn exchange(
        server: &Server,
        path: &str,
        body: &Value,
        headers: &[(&str, &str)],
    ) -> (u16, Option<String>, Value) {
        let text = body.to_string();
        let request = match rewrite(path, text.as_bytes(), headers) {
            Ok(request) => request,
            Err(err) => return (err.status(), None, Value::Null),
        };
        let answer = server.handle(request);
        let id = answer
            .headers()
            .iter()
            .find(|(field, _)| field.eq_ignore_ascii_case("mcp-session-id"))
            .map(|(_, value)| value.clone());
        let json = serde_json::from_slice(answer.body()).unwrap_or(Value::Null);
        (answer.status(), id, json)
    }

    fn initialize() -> Value {
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": {"name": "tester", "version": "1"}}})
    }

    /// A new session at `path`: its id (which carries the version's route).
    fn start(server: &Server, path: &str) -> String {
        let (status, id, answer) = exchange(server, path, &initialize(), &[]);
        assert_eq!(status, 200, "{path}: {answer}");
        id.expect("a session id")
    }

    /// `MATCH (n) RETURN count(n)` in `session` at `path`: the status and the count (`None`
    /// when refused).
    fn count(server: &Server, path: &str, session: &str) -> (u16, Option<u64>) {
        let call = tool_call("gql", json!({"query": "MATCH (n) RETURN count(n) AS c"}));
        let (status, _, answer) = exchange(server, path, &call, &[("Mcp-Session-Id", session)]);
        let text = tool_text(&answer);
        let count = text
            .split("\"c\":")
            .nth(1)
            .and_then(|rest| rest.split(|c: char| !c.is_ascii_digit()).next())
            .and_then(|digits| digits.parse().ok());
        (status, count)
    }

    /// The versioned base store `product`: the modern graph at mark `2026.2` (6 vertices), one
    /// vertex more at `2026.3`, two more at `2026.4`.
    fn product_store() -> MemDir {
        let dir = MemDir::named("product");
        let store = Store::create_with(
            &dir,
            TraversalGraph::tinkerpop_modern(),
            StoreOptions::new(),
        )
        .expect("the base store");
        store.mark("2026.2").expect("a mark");
        for mark in ["2026.3", "2026.4"] {
            GraphTraversalSource::new(&mut *store.graph().write())
                .add_v("feature")
                .iterate()
                .expect("a base commit");
            store.mark(mark).expect("a mark");
        }
        store.close().expect("closed");
        dir
    }

    /// `n` vertices labelled `label` into a layer (the customer's own data).
    fn add(label: &'static str, n: usize) -> impl FnOnce(&Graph<LayeredGraph>) {
        move |layer| {
            for _ in 0..n {
                GraphTraversalSource::new(&mut *layer.write())
                    .add_v(label)
                    .iterate()
                    .expect("a layer write");
            }
        }
    }

    /// The writing customer's profile and the read-only one.
    fn writer() -> Profile {
        Profile::new(false)
    }

    fn reader() -> Profile {
        Profile::new(true).with_access_policy(AccessPolicy::read_only())
    }

    /// The catalog of design section 6, on one host:
    ///
    /// ```text
    /// product@2026.2/.3/.4 -> bases (one store, three marks)
    /// acme@2026.2          -> layer over product@2026.2, kept (explicit pin)
    /// acme@2026.3          -> layer over product@2026.3
    /// globex@2026.3        -> another layer over the SAME base product@2026.3 (read-only)
    /// product@2026.3       -> an empty layer over that base: the base itself, read-only
    /// acme@2026.4          -> layer over product@2026.4 (the next release)
    /// acme -> acme@2026.3, globex -> globex@2026.3, product -> product@2026.3
    /// ```
    ///
    /// Vertex counts: acme@2026.2 7, acme@2026.3 8, acme@2026.4 9, globex@2026.3 10,
    /// product@2026.3 7.
    struct Fixture {
        host: Arc<LayerHost>,
        server: Server,
        acme_3: Weak<Graph<LayeredGraph>>,
        globex_3: Weak<Graph<LayeredGraph>>,
    }

    fn fixture(mcp: Mcp) -> Fixture {
        let host = Arc::new(LayerHost::new(Arc::new(mcp)));
        let dir = product_store();
        let base_2 = host.base(&dir, "2026.2");
        host.publish("acme@2026.2", &base_2, writer(), add("acme", 1));
        let acme_3 = host.publish(
            "acme@2026.3",
            &host.base(&dir, "2026.3"),
            writer(),
            add("acme", 1),
        );
        // A second load of the same commit: the pooled base.
        let globex_3 = host.publish(
            "globex@2026.3",
            &host.base(&dir, "2026.3"),
            reader(),
            add("globex", 3),
        );
        host.publish(
            "product@2026.3",
            &host.base(&dir, "2026.3"),
            reader(),
            |_| {},
        );
        host.publish(
            "acme@2026.4",
            &host.base(&dir, "2026.4"),
            writer(),
            add("acme", 1),
        );
        host.catalog
            .lock()
            .unwrap()
            .kept
            .insert("acme@2026.2".into());
        host.flip("acme", "acme@2026.3");
        host.flip("globex", "globex@2026.3");
        host.flip("product", "product@2026.3");
        let server = Server::new();
        server.ready_with(host.clone()).expect("ready once");
        Fixture {
            host,
            server,
            acme_3,
            globex_3,
        }
    }

    /// The endpoint in table mode and with signed ids (a key shared by the replicas).
    fn both_modes() -> [(&'static str, Mcp); 2] {
        let key = SessionKey::new(b"a shared secret of the replicas, 32+ bytes").expect("a key");
        [
            ("table", Mcp::new(false, None, false)),
            ("signed", Mcp::new(false, None, false).with_session_key(key)),
        ]
    }

    #[test]
    fn two_layers_over_one_pooled_base_share_it() {
        let Fixture {
            host,
            acme_3,
            globex_3,
            ..
        } = fixture(Mcp::new(false, None, false));
        // Three bases for five versions: the three loads of mark 2026.3 are one base.
        assert_eq!(host.pool.lock().unwrap().len(), 3);
        let (acme, globex) = (acme_3.upgrade().unwrap(), globex_3.upgrade().unwrap());
        let (acme, globex) = (acme.read(), globex.read());
        assert!(Arc::ptr_eq(acme.base(), globex.base()));
        assert_eq!(acme.base_ref(), globex.base_ref());
        let shared = |layer: &LayeredGraph| {
            layer
                .memory_usage()
                .expect("readable")
                .expect("measured")
                .shared_bytes
        };
        assert!(shared(&acme) > 0);
        assert_eq!(shared(&acme), shared(&globex));
    }

    #[test]
    fn an_alias_flip_keeps_open_sessions_on_their_version() {
        for (mode, mcp) in both_modes() {
            let Fixture { host, server, .. } = fixture(mcp);
            let old = start(&server, "/mcp/acme");
            assert_eq!(graphersal_mcp::session_route(&old), Some("acme@2026.3"));
            assert_eq!(count(&server, "/mcp/acme", &old), (200, Some(8)), "{mode}");
            // The release: new sessions get 2026.4, the open one finishes on 2026.3.
            host.flip("acme", "acme@2026.4");
            assert_eq!(count(&server, "/mcp/acme", &old), (200, Some(8)), "{mode}");
            let new = start(&server, "/mcp/acme");
            assert_eq!(graphersal_mcp::session_route(&new), Some("acme@2026.4"));
            assert_eq!(count(&server, "/mcp/acme", &new), (200, Some(9)), "{mode}");
            // The instructions of a layer name the layer steps (`.layered(true)`).
            let (_, _, answer) = exchange(&server, "/mcp/acme", &initialize(), &[]);
            let instructions = answer["result"]["instructions"].as_str().unwrap_or("");
            assert!(instructions.contains("layer_diff"), "{mode}: {answer}");
        }
    }

    #[test]
    fn an_explicit_pin_reaches_its_version_whatever_the_alias() {
        for (mode, mcp) in both_modes() {
            let Fixture { server, .. } = fixture(mcp);
            let pinned = start(&server, "/mcp/acme@2026.2");
            assert_eq!(graphersal_mcp::session_route(&pinned), Some("acme@2026.2"));
            assert_eq!(
                count(&server, "/mcp/acme@2026.2", &pinned),
                (200, Some(7)),
                "{mode}"
            );
            // The same session also answers at its name's alias path (it is acme's version).
            assert_eq!(
                count(&server, "/mcp/acme", &pinned),
                (200, Some(7)),
                "{mode}"
            );
            // A session of the alias does not answer at the pin of another version.
            let current = start(&server, "/mcp/acme");
            assert_eq!(
                count(&server, "/mcp/acme@2026.2", &current).0,
                404,
                "{mode}"
            );
            // The base itself, read-only, and the per-context profiles.
            let product = start(&server, "/mcp/product");
            assert_eq!(count(&server, "/mcp/product", &product), (200, Some(7)));
            let write = tool_call("query", json!({"script": "g.add_v(\"x\").iterate()"}));
            let globex = start(&server, "/mcp/globex");
            let (_, _, refused) = exchange(
                &server,
                "/mcp/globex",
                &write,
                &[("Mcp-Session-Id", &globex)],
            );
            assert_eq!(refused["result"]["isError"], true, "{mode}: {refused}");
            let (_, _, allowed) = exchange(
                &server,
                "/mcp/acme",
                &write,
                &[("Mcp-Session-Id", &current)],
            );
            assert_eq!(allowed["result"]["isError"], false, "{mode}: {allowed}");
            // acme's write stays in its layer: the shared base and globex do not see it.
            assert_eq!(count(&server, "/mcp/acme", &current), (200, Some(9)));
            assert_eq!(count(&server, "/mcp/globex", &globex), (200, Some(10)));
            assert_eq!(count(&server, "/mcp/product", &product), (200, Some(7)));
        }
    }

    #[test]
    fn a_session_of_another_name_or_a_forged_route_is_refused() {
        for (mode, mcp) in both_modes() {
            let Fixture { server, .. } = fixture(mcp);
            let globex = start(&server, "/mcp/globex");
            assert_eq!(count(&server, "/mcp/globex", &globex), (200, Some(10)));
            // A valid session of globex sent to acme's path: refused by the name check (the
            // endpoint alone would accept it, the id is genuine).
            assert_eq!(count(&server, "/mcp/acme", &globex).0, 404, "{mode}");
            // An edited route (the id of globex relabelled as acme's): unknown to the endpoint.
            let forged = globex.replacen("globex@2026.3~", "acme@2026.3~", 1);
            assert_ne!(forged, globex);
            assert_eq!(count(&server, "/mcp/acme", &forged).0, 404, "{mode}");
            assert_eq!(count(&server, "/mcp/globex", &forged).0, 404, "{mode}");
            // Unknown names and versions, a bad path, and a client's own graph header.
            let (status, _, _) = exchange(&server, "/mcp/initech", &initialize(), &[]);
            assert_eq!(status, 404, "{mode}");
            let (status, _, _) = exchange(&server, "/mcp/acme@2025.1", &initialize(), &[]);
            assert_eq!(status, 404, "{mode}");
            let (status, _, _) = exchange(&server, "/mcp/acme/../globex", &initialize(), &[]);
            assert_eq!(status, 404, "{mode}");
            let smuggled = exchange(
                &server,
                "/mcp/acme",
                &initialize(),
                &[(GRAPH_HEADER, "globex")],
            );
            assert_eq!(
                smuggled
                    .1
                    .as_deref()
                    .and_then(graphersal_mcp::session_route),
                Some("acme@2026.3"),
                "{mode}: the path names the graph, not the client's header"
            );
        }
    }

    #[test]
    fn a_retired_version_drops_its_context_layer_and_finally_its_base() {
        let Fixture {
            host,
            server,
            acme_3,
            globex_3,
        } = fixture(Mcp::new(false, None, false));
        let old = start(&server, "/mcp/acme");
        host.flip("acme", "acme@2026.4");
        assert_eq!(count(&server, "/mcp/acme", &old), (200, Some(8)));
        // Used a moment ago: a grace period keeps it.
        assert!(host.retire_idle(Duration::from_secs(3600)).is_empty());
        let context = Arc::downgrade(&host.catalog.lock().unwrap().versions["acme@2026.3"].context);
        // No grace: the version no alias or pin names goes.
        assert_eq!(host.retire_idle(Duration::ZERO), vec!["acme@2026.3"]);
        wait_until("the retired context is dropped", || {
            context.upgrade().is_none()
        });
        wait_until("its layer is dropped", || acme_3.upgrade().is_none());
        // The open session learns it is gone (a 404 makes an MCP client start again) and the
        // new session is on the current version.
        assert_eq!(count(&server, "/mcp/acme", &old).0, 404);
        let new = start(&server, "/mcp/acme");
        assert_eq!(count(&server, "/mcp/acme", &new), (200, Some(9)));
        // globex and product still read base 2026.3: it stays pooled.
        assert!(globex_3.upgrade().is_some());
        assert_eq!(host.evict_unused_bases(), 0);
        // Unpinning acme@2026.2 lets it go, and then its base, which nothing else reads.
        host.catalog.lock().unwrap().kept.remove("acme@2026.2");
        assert_eq!(host.retire_idle(Duration::ZERO), vec!["acme@2026.2"]);
        assert_eq!(host.evict_unused_bases(), 1);
        assert_eq!(host.pool.lock().unwrap().len(), 2);
    }
}
