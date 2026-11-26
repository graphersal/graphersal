//! Tests of the session presets changed at run time ([`Context::set_presets`]): a run already
//! running keeps its presets, the next run gets the new ones (limits and ceilings), `GET
//! /api/info` and the statistics answer them, every worker of a host queue follows, and an open
//! MCP session's next tool call runs under them. Real session threads on the `modern` sample,
//! no HTTP library.

use std::io::Cursor;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use graphersal::prelude::ElementProperty;
use graphersal_session::{RunOptions, Session, SessionPresets};
use serde_json::{Value, json};

use crate::{
    Busy, BusyReason, Context, Exposure, HostCheck, Job, JobQueue, Method, Request, Response,
    Runner, ServedSource, handle, launch, spawn_session,
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

fn body_json(response: &Response) -> Value {
    serde_json::from_slice(response.body()).expect("a JSON answer")
}

/// The answer of `POST /api/execute` of `script` (status 200, the session's JSON).
fn execute(context: &Arc<Context>, script: &str) -> Value {
    let body = json!({ "script": script }).to_string();
    let answer = handle(context, post("/api/execute", body.as_bytes()));
    assert_eq!(
        answer.status(),
        200,
        "{}",
        String::from_utf8_lossy(answer.body())
    );
    body_json(&answer)
}

/// The diagnostic of a refused run, `None` when it ran.
fn refusal(answer: &Value) -> Option<String> {
    answer["execution"]["error"]["diagnostic"]
        .as_str()
        .map(str::to_string)
}

/// At most `n` traversers, as a ceiling a query may lower but not raise.
fn capped(n: i64) -> SessionPresets {
    SessionPresets::new()
        .with_option("traversal.max_traversers", ElementProperty::from(n))
        .with_ceilings(true)
}

/// Six vertices: refused under [`capped`] with less than 6.
const ALL_VERTICES: &str = "g.v().to_list()";

#[test]
fn a_running_run_keeps_its_presets_and_the_next_run_gets_the_new_ones() {
    let (launched, ()) = launch(
        ServedSource::Sample("modern".to_string()),
        SessionPresets::new(),
        |_: &mut Session| Ok::<_, String>(()),
        None,
    )
    .expect("the launch");
    let context = launched.context_builder(check()).build();
    let before = body_json(&handle(&context, get("/api/info")))["presets"].clone();
    assert_eq!(before, SessionPresets::new().to_value());
    assert_eq!(context.presets().unwrap(), before);
    assert!(refusal(&execute(&context, ALL_VERTICES)).is_none());

    // A run that has started (it waits inside the session for the release) ...
    let (started, running) = channel::<()>();
    let (release, released) = channel::<()>();
    let long = {
        let context = context.clone();
        std::thread::spawn(move || {
            context
                .call(move |session| {
                    let _ = started.send(());
                    let _ = released.recv();
                    session.execute(ALL_VERTICES, &RunOptions::default())
                })
                .expect("the session answers")
        })
    };
    running
        .recv_timeout(Duration::from_secs(10))
        .expect("the long run starts");
    // ... the host lowers the limit meanwhile; the call does not wait for the running run.
    context.set_presets(capped(3)).expect("the presets change");
    let info = body_json(&handle(&context, get("/api/info")));
    assert_eq!(info["presets"], capped(3).to_value(), "{info}");
    assert_eq!(info["presets"]["ceilings"], true);
    assert_eq!(context.presets().unwrap(), capped(3).to_value());
    release.send(()).expect("the long run is released");
    let kept: Value =
        serde_json::from_str(&long.join().expect("the long run")).expect("a JSON answer");
    assert!(refusal(&kept).is_none(), "the old presets: {kept}");
    assert_eq!(
        kept["execution"]["results"].as_array().map(Vec::len),
        Some(6)
    );

    // The next run: refused by the new limit, which is a ceiling now.
    let refused = refusal(&execute(&context, ALL_VERTICES)).expect("refused");
    assert!(refused.contains("traversal.max_traversers"), "{refused}");
    let raised = execute(
        &context,
        r#"g.with("traversal.max_traversers", 100).v().to_list()"#,
    );
    let raised = refusal(&raised).expect("a raise is refused");
    assert!(raised.contains("capped at"), "{raised}");
    let lowered = execute(
        &context,
        r#"g.with("traversal.max_traversers", 2).v().has_label("software").to_list()"#,
    );
    assert!(
        refusal(&lowered).is_none(),
        "lowering is allowed: {lowered}"
    );
    // The statistics show the session's new presets.
    let statistics = body_json(&handle(&context, get("/api/statistics")));
    assert_eq!(
        statistics["limits"]["presets"],
        capped(3).to_value(),
        "{statistics}"
    );

    // Raised again (no ceiling): the query runs and may raise the preset itself.
    let roomy = SessionPresets::new()
        .with_option("traversal.max_traversers", ElementProperty::from(1_000_i64));
    context
        .set_presets(roomy.clone())
        .expect("the presets change");
    assert!(refusal(&execute(&context, ALL_VERTICES)).is_none());
    assert_eq!(
        body_json(&handle(&context, get("/api/info")))["presets"],
        roomy.to_value()
    );
}

/// A host queue over two sessions of one graph (the session thread taken over, plus a second
/// worker from the session's template), taking turns.
struct TwoWorkers {
    workers: [Mutex<Sender<Job>>; 2],
    next: AtomicUsize,
}

impl JobQueue for TwoWorkers {
    fn submit(&self, _key: &str, job: Job) -> Result<(), Busy> {
        let worker = self.next.fetch_add(1, Ordering::SeqCst) % 2;
        self.workers[worker]
            .lock()
            .map_err(|_| Busy::new(BusyReason::Stopping, "gone"))?
            .send(job)
            .map_err(|_| Busy::new(BusyReason::Stopping, "gone"))
    }

    fn status(&self) -> Value {
        json!({ "workers": 2 })
    }
}

#[test]
fn every_worker_of_a_host_queue_runs_under_the_new_presets() {
    let (first, first_jobs) = channel::<Job>();
    let (template_out, template_in) = channel();
    let takeover: Job = Box::new(move |session| {
        let _ = template_out.send(session.template().expect("a template"));
        for job in first_jobs {
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
    let template = template_in
        .recv_timeout(Duration::from_secs(10))
        .expect("the template");
    let (second, second_jobs) = channel::<Job>();
    std::thread::spawn(move || {
        let mut session = template.session().expect("a worker session");
        for job in second_jobs {
            job(&mut session);
        }
    });
    let queue = Arc::new(TwoWorkers {
        workers: [Mutex::new(first), Mutex::new(second)],
        next: AtomicUsize::new(0),
    });
    let context = Context::builder(Runner::Queue(queue.clone()), check()).build();
    for _ in 0..2 {
        assert!(refusal(&execute(&context, ALL_VERTICES)).is_none());
    }
    context.set_presets(capped(3)).expect("the presets change");
    for turn in 0..4 {
        let answer = execute(&context, ALL_VERTICES);
        let refused = refusal(&answer).unwrap_or_else(|| panic!("turn {turn}: {answer}"));
        assert!(refused.contains("traversal.max_traversers"), "{refused}");
    }
    assert_eq!(
        queue.next.load(Ordering::SeqCst),
        6,
        "both workers took turns"
    );
}

#[cfg(feature = "mcp")]
#[test]
fn an_open_mcp_session_runs_its_next_call_under_the_new_presets() {
    let (launched, ()) = launch(
        ServedSource::Sample("modern".to_string()),
        capped(100),
        |_: &mut Session| Ok::<_, String>(()),
        None,
    )
    .expect("the launch");
    let context = launched
        .context_builder(check())
        .mcp(Some(graphersal_mcp::Mcp::new(true, None, false)))
        .mcp_profile(graphersal_mcp::Profile::new(true))
        .build();
    let mcp = |body: Value, session: Option<&str>| {
        let text = body.to_string();
        let mut request = post("/mcp", text.as_bytes()).with_header("Accept", "application/json");
        if let Some(session) = session {
            request = request.with_header("Mcp-Session-Id", session);
        }
        let answer = handle(&context, request);
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
                          "clientInfo": {"name": "presets-test", "version": "1"}}}),
        None,
    );
    let session = initialized
        .headers()
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("Mcp-Session-Id"))
        .map(|(_, value)| value.clone())
        .expect("a session id");
    let query = |id: u64| {
        let answer = body_json(&mcp(
            json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
                   "params": {"name": "query", "arguments": {"script": ALL_VERTICES}}}),
            Some(&session),
        ));
        let result = &answer["result"];
        (
            result["content"][0]["text"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            result["isError"].as_bool().unwrap_or(false),
        )
    };
    let (text, is_error) = query(2);
    assert!(!is_error, "{text}");
    context.set_presets(capped(3)).expect("the presets change");
    let (text, is_error) = query(3);
    assert!(is_error, "{text}");
    assert!(text.contains("traversal.max_traversers"), "{text}");
    // The profile of the context (what a directory host serves the graph with) is unchanged.
    assert!(context.mcp_profile().is_some());
}
