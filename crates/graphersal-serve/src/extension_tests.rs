//! Unit tests of the extension points (crate docs, "Extension points"): a test host that
//! resolves two graphs, observes and cancels runs, refuses requests in its filter and attaches
//! an identity, serves an extra route, sees the lifecycle (ready, stopping, activity), and runs
//! the jobs on a queue of its own; plus the lifecycle's probe answers while loading, ready and
//! stopping. Real session threads on the sample graphs, no HTTP library.

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
        crate::SERVER_ID
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
    assert!(plain.mcp_connection().is_none());
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
