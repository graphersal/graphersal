//! Unit tests of the HTTP boundary without an HTTP library: a [`Context`] on a real session
//! thread (the `modern` sample), requests built as [`Request`] values and answered by
//! [`handle`]. Routing, the body limit, the `Host`/`Origin` checks, cancellation, the host's
//! hooks and a refusing job queue.

use std::io::Cursor;
use std::sync::Arc;
use std::time::{Duration, Instant};

use graphersal_session::SessionPresets;
use serde_json::{Value, json};

use crate::context::{Busy, BusyReason, Context, Hooks, Job, JobQueue, Runner, spawn_session};
use crate::host_check::{Exposure, HostCheck, HostRules};
use crate::http::{HttpError, MAX_BODY_BYTES, Method, Request, Response};
use crate::ui::Ui;
use crate::{handle, runs::Runs};

const PORT: u16 = 8080;
const HOST: &str = "127.0.0.1:8080";

/// A loopback server's checks on [`PORT`].
fn check() -> HostCheck {
    HostCheck::new(Exposure::Loopback, PORT, &format!("http://{HOST}/"))
}

/// A session thread with the `modern` sample loaded.
fn session_runner() -> Runner {
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

/// A context over a fresh session thread, with `runs` shared.
fn context_with(runs: Arc<Runs>, hooks: Option<Arc<dyn Hooks>>) -> Arc<Context> {
    let builder = Context::builder(session_runner(), check()).runs(runs);
    match hooks {
        Some(hooks) => builder.hooks(hooks).build(),
        None => builder.build(),
    }
}

fn context() -> Arc<Context> {
    context_with(Arc::new(Runs::default()), None)
}

/// A request with `Host` set to this server.
fn request<'a>(method: Method, url: &str, body: &'a [u8]) -> Request<'a> {
    Request::new(method, url, Cursor::new(body)).with_header("Host", HOST)
}

/// A JSON request with `Host`, `Content-Type` and the declared length.
fn json_request<'a>(url: &str, body: &'a [u8]) -> Request<'a> {
    request(Method::Post, url, body)
        .with_header("Content-Type", "application/json")
        .with_body_length(Some(body.len()))
}

fn body_json(response: &Response) -> Value {
    serde_json::from_slice(response.body()).expect("a JSON answer")
}

fn header<'a>(response: &'a Response, name: &str) -> Option<&'a str> {
    response
        .headers()
        .iter()
        .find(|(field, _)| field.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

#[test]
fn methods_parse_case_sensitively_and_print_their_names() {
    assert_eq!(Method::parse("GET"), Method::Get);
    assert_eq!(Method::parse("DELETE"), Method::Delete);
    assert_eq!(Method::parse("get"), Method::Other("get".into()));
    assert_eq!(Method::parse("BREW").to_string(), "BREW");
    assert_eq!(Method::Post.as_str(), "POST");
}

#[test]
fn the_api_routes_known_endpoints_and_refuses_the_rest() {
    let context = context();
    let info = handle(&context, request(Method::Get, "/api/info", b""));
    assert_eq!(info.status(), 200);
    assert_eq!(body_json(&info)["server"], crate::SERVER_ID);
    assert_eq!(body_json(&info)["readOnly"], false);
    assert_eq!(body_json(&info)["mcp"], Value::Null);
    assert_eq!(
        header(&info, "Content-Type"),
        Some("application/json; charset=utf-8")
    );
    assert_eq!(header(&info, "Cache-Control"), Some("no-store"));

    let executed = handle(
        &context,
        json_request(
            "/api/execute",
            br#"{"script": "g.v().has_label(\"person\").count().next()"}"#,
        ),
    );
    assert_eq!(executed.status(), 200);
    assert!(
        String::from_utf8_lossy(executed.body()).contains('4'),
        "{}",
        String::from_utf8_lossy(executed.body())
    );

    let unknown = handle(&context, request(Method::Get, "/api/nowhere", b""));
    assert_eq!(unknown.status(), 404);
    assert_eq!(
        body_json(&unknown)["error"],
        "unknown endpoint /api/nowhere"
    );

    let wrong_method = handle(&context, request(Method::Delete, "/api/info", b""));
    assert_eq!(wrong_method.status(), 405);
    assert_eq!(
        body_json(&wrong_method)["error"],
        "method DELETE is not allowed on /api/info"
    );
    let static_post = handle(&context, request(Method::Post, "/index.html", b""));
    assert_eq!(static_post.status(), 405);

    let openapi = handle(&context, request(Method::Get, "/openapi.json", b""));
    assert_eq!(openapi.status(), 200);
    assert_eq!(
        body_json(&openapi)["info"]["version"],
        env!("CARGO_PKG_VERSION")
    );

    // Without the MCP endpoint `/mcp` answers 404, like a server started without `--mcp`.
    let mcp = handle(&context, json_request("/mcp", b"{}"));
    assert_eq!(mcp.status(), 404);
    assert_eq!(
        body_json(&mcp)["error"],
        "the MCP endpoint is off: start the server with --mcp"
    );
}

#[test]
fn static_files_come_from_the_ui_directory_and_stay_inside_it() {
    let dir = std::env::temp_dir().join(format!("graphersal_serve_ui_{}", std::process::id()));
    std::fs::create_dir_all(dir.join("js")).unwrap();
    std::fs::write(dir.join("index.html"), "<p>ui</p>").unwrap();
    std::fs::write(dir.join("js/x.js"), "export {}").unwrap();
    let ui = Ui::resolve(dir.to_str(), "unused").unwrap();
    let context = Context::builder(session_runner(), check()).ui(ui).build();
    let index = handle(&context, request(Method::Get, "/", b""));
    assert_eq!(index.status(), 200);
    assert_eq!(index.body(), b"<p>ui</p>");
    assert_eq!(
        header(&index, "Content-Type"),
        Some("text/html; charset=utf-8")
    );
    let module = handle(&context, request(Method::Get, "/js/x.js", b""));
    assert_eq!(module.status(), 200);
    assert_eq!(
        header(&module, "Content-Type"),
        Some("text/javascript; charset=utf-8")
    );
    for escape in [
        "/../Cargo.toml",
        "/js/%2e%2e/%2e%2e/etc/passwd",
        "/missing.js",
    ] {
        assert_eq!(
            handle(&context, request(Method::Get, escape, b"")).status(),
            404,
            "{escape}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
    let missing = Ui::resolve(Some("/no/such/ui/dir"), "unused")
        .err()
        .expect("a missing directory is refused");
    assert!(
        missing.to_string().contains("(--ui-dir) is not usable"),
        "{missing}"
    );
}

#[test]
fn bodies_are_json_and_limited() {
    let context = context();
    // Not JSON: 415.
    let text = handle(
        &context,
        request(Method::Post, "/api/execute", b"{}").with_header("Content-Type", "text/plain"),
    );
    assert_eq!(text.status(), 415);
    // A declared body beyond the limit: 413 before anything is read.
    let declared = handle(
        &context,
        request(Method::Post, "/api/execute", b"{}")
            .with_header("Content-Type", "application/json")
            .with_body_length(Some(MAX_BODY_BYTES + 1)),
    );
    assert_eq!(declared.status(), 413);
    // A body longer than the limit without a declared length: 413 after reading one byte too
    // many.
    let big = vec![b' '; MAX_BODY_BYTES + 10];
    let undeclared = handle(
        &context,
        request(Method::Post, "/api/execute", &big).with_header("Content-Type", "application/json"),
    );
    assert_eq!(undeclared.status(), 413);
    assert_eq!(
        body_json(&undeclared)["error"],
        format!("the body is larger than {MAX_BODY_BYTES} bytes")
    );
    // Malformed JSON, a body that is no object, a missing field: 400.
    for body in [&b"{"[..], b"[1]", b"{}"] {
        let answer = handle(&context, json_request("/api/execute", body));
        assert_eq!(
            answer.status(),
            400,
            "{}",
            String::from_utf8_lossy(answer.body())
        );
    }
}

#[test]
fn foreign_hosts_and_origins_are_refused() {
    let context = context();
    let no_host = handle(
        &context,
        Request::new(Method::Get, "/api/info", Cursor::new(&b""[..])),
    );
    assert_eq!(no_host.status(), 403);
    let rebinding = handle(
        &context,
        Request::new(Method::Get, "/api/info", Cursor::new(&b""[..]))
            .with_header("Host", "evil.example:8080"),
    );
    assert_eq!(rebinding.status(), 403);
    assert!(
        body_json(&rebinding)["error"]
            .as_str()
            .unwrap()
            .starts_with("unknown Host: use http://127.0.0.1:8080/"),
        "{}",
        body_json(&rebinding)
    );
    let cross_site = handle(
        &context,
        request(Method::Get, "/api/info", b"").with_header("Origin", "http://evil.example"),
    );
    assert_eq!(cross_site.status(), 403);
    assert_eq!(
        body_json(&cross_site)["error"],
        "cross-origin request from http://evil.example refused"
    );
    let same_site = handle(
        &context,
        request(Method::Get, "/api/info", b"").with_header("Origin", "http://localhost:8080"),
    );
    assert_eq!(same_site.status(), 200);
    // The header name is case-insensitive.
    let lower = handle(
        &context,
        Request::new(Method::Get, "/api/info", Cursor::new(&b""[..])).with_header("host", HOST),
    );
    assert_eq!(lower.status(), 200);

    // The network rules: addresses and dotless names on any port, a fully qualified name only
    // when allowed; the origin must be the request's own.
    let allowed = vec!["kb.example.com".to_string()];
    let network = HostRules::new(Exposure::Network, None, &allowed);
    assert!(network.is_own_host("192.168.1.5:9000", PORT));
    assert!(network.is_own_host("graphersal", PORT));
    assert!(!network.is_own_host("rebind.attacker.com", PORT));
    assert!(network.is_own_host("KB.example.com:443", PORT));
    assert!(network.is_own_origin("https://kb.example.com", "localhost:8080", PORT));
    assert!(!network.is_own_origin("http://10.0.0.9:8080", "10.0.0.1:8080", PORT));
    let message =
        HostCheck::new(Exposure::Network, PORT, "http://127.0.0.1:8080/").unknown_host_message();
    assert!(message.contains("--allowed-host"), "{message}");
}

/// Waits until `runs` holds `n` runs: the test follows the server, not the clock.
fn wait_for_runs(runs: &Runs, n: usize) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while runs.len() != n {
        assert!(Instant::now() < deadline, "the runs never reached {n}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn a_cancel_reaches_only_the_clients_own_run() {
    const ENDLESS: &str = "g.inject(1).repeat(__.identity()).times(100000000).count().next()";
    let runs = Arc::new(Runs::default());
    let context = context_with(runs.clone(), None);
    let body = json!({ "script": ENDLESS, "runId": "r1" }).to_string();
    let running = {
        let context = context.clone();
        std::thread::spawn(move || {
            handle(
                &context,
                json_request("/api/execute", body.as_bytes())
                    .with_header("X-Graphersal-Client", "tab-a"),
            )
        })
    };
    wait_for_runs(&runs, 1);
    // Another client's Stop and another run id of the same client reach nothing.
    let other = handle(
        &context,
        json_request("/api/cancel", br#"{"runId": "r1"}"#)
            .with_header("X-Graphersal-Client", "tab-b"),
    );
    assert_eq!(body_json(&other)["cancelled"], false);
    let wrong_id = handle(
        &context,
        json_request("/api/cancel", br#"{"runId": "r2"}"#)
            .with_header("X-Graphersal-Client", "tab-a"),
    );
    assert_eq!(body_json(&wrong_id)["cancelled"], false);
    let own = handle(
        &context,
        json_request("/api/cancel", br#"{"runId": "r1"}"#)
            .with_header("X-Graphersal-Client", "tab-a"),
    );
    assert_eq!(body_json(&own)["cancelled"], true);
    let answer = running.join().unwrap();
    assert_eq!(answer.status(), 200);
    let text = String::from_utf8_lossy(answer.body()).to_lowercase();
    assert!(text.contains("cancel"), "{text}");
    // The run ended: nothing left to cancel; a body-less Stop of the client answers false.
    wait_for_runs(&runs, 0);
    let stop = handle(
        &context,
        request(Method::Post, "/api/cancel", b"").with_header("X-Graphersal-Client", "tab-a"),
    );
    assert_eq!(body_json(&stop)["cancelled"], false);
    // A runId that is not a short string is refused.
    let bad = handle(&context, json_request("/api/cancel", br#"{"runId": 7}"#));
    assert_eq!(bad.status(), 400);
}

/// A host adding an `/api/info` field, a process statistic and an endpoint of its own.
struct TestHooks;

impl Hooks for TestHooks {
    fn add_info(&self, _context: &Context, info: &mut Value) {
        info["host"] = json!("test");
    }

    fn add_process_statistics(&self, _context: &Context, statistics: &mut Value) {
        statistics["process"]["test"] = json!(true);
    }

    fn route(
        &self,
        _context: &Arc<Context>,
        request: &mut Request<'_>,
        endpoint: &str,
        _query: &str,
        client: &str,
    ) -> Option<Result<Response, HttpError>> {
        (endpoint == "extra").then(|| match request.method() {
            Method::Get => Ok(Response::json(200, &json!({ "client": client }))),
            _ => Err(HttpError::new(405, "GET only")),
        })
    }
}

#[test]
fn the_hosts_hooks_add_fields_and_endpoints() {
    let context = context_with(Arc::new(Runs::default()), Some(Arc::new(TestHooks)));
    let info = handle(&context, request(Method::Get, "/api/info", b""));
    assert_eq!(body_json(&info)["host"], "test");
    let statistics = handle(&context, request(Method::Get, "/api/statistics", b""));
    assert_eq!(statistics.status(), 200);
    assert_eq!(body_json(&statistics)["process"]["test"], true);
    let extra = handle(
        &context,
        request(Method::Get, "/api/extra", b"").with_header("X-Graphersal-Client", "tab-x"),
    );
    assert_eq!(extra.status(), 200);
    assert_eq!(body_json(&extra)["client"], "tab-x");
    let refused = handle(&context, request(Method::Post, "/api/extra", b""));
    assert_eq!(refused.status(), 405);
    assert_eq!(body_json(&refused)["error"], "GET only");
    // An endpoint neither knows: 404.
    let unknown = handle(&context, request(Method::Get, "/api/other", b""));
    assert_eq!(unknown.status(), 404);
}

/// A queue that refuses every job (the read-only serving mode's pool when full).
struct FullQueue;

impl JobQueue for FullQueue {
    fn submit(&self, _key: &str, _job: Job) -> Result<(), Busy> {
        Err(Busy::new(BusyReason::QueueFull, "the queue is full"))
    }

    fn status(&self) -> Value {
        json!({ "workers": 0 })
    }
}

#[test]
fn a_refusing_job_queue_answers_503_with_retry_after() {
    let context = Context::builder(Runner::Queue(Arc::new(FullQueue)), check()).build();
    let busy = handle(&context, request(Method::Get, "/api/graph", b""));
    assert_eq!(busy.status(), 503);
    assert_eq!(header(&busy, "Retry-After"), Some("1"));
    assert_eq!(body_json(&busy)["error"], "the queue is full");
    // The read-only serving mode: `serving` in `/api/info` (answered without the queue), and no
    // scheduled jobs.
    let info = handle(&context, request(Method::Get, "/api/info", b""));
    assert_eq!(info.status(), 200);
    assert_eq!(body_json(&info)["serving"]["workers"], 0);
    let jobs = handle(
        &context,
        json_request("/api/jobs/run", br#"{"name": "checkpoint"}"#),
    );
    assert_eq!(jobs.status(), 409);
}

#[cfg(feature = "mcp")]
#[test]
fn the_mcp_endpoint_answers_through_the_boundary() {
    let mcp = graphersal_mcp::Mcp::new(true, None, false);
    let context = Context::builder(session_runner(), check())
        .mcp(Some(mcp))
        .build();
    let initialize = json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                   "clientInfo": {"name": "boundary-test", "version": "1"}},
    })
    .to_string();
    let answer = handle(
        &context,
        json_request("/mcp", initialize.as_bytes()).with_header("Accept", "application/json"),
    );
    assert_eq!(
        answer.status(),
        200,
        "{}",
        String::from_utf8_lossy(answer.body())
    );
    let session = header(&answer, "Mcp-Session-Id")
        .expect("a session id")
        .to_string();
    assert_eq!(body_json(&answer)["id"], 1);
    // The endpoint reads the graph through the context's session thread.
    let call = json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "query", "arguments": {"script": "g.v().count().next()"}},
    })
    .to_string();
    let answer = handle(
        &context,
        json_request("/mcp", call.as_bytes())
            .with_header("Accept", "application/json")
            .with_header("Mcp-Session-Id", &session),
    );
    assert_eq!(answer.status(), 200);
    let text = body_json(&answer)["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(text.contains('6'), "{text}");
    // `/api/info` names the endpoint, read-only.
    let info = handle(&context, request(Method::Get, "/api/info", b""));
    assert_eq!(body_json(&info)["mcp"]["readOnly"], true);
}
