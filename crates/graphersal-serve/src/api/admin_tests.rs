//! The run administration over the HTTP boundary (`ContextBuilder::admin_runs`, issue #32): off
//! by default (404, no `adminRuns` in `/api/info`), the list sees other clients' queued and
//! running runs, the kill cancels one by id, the token, the allow-list mode, the mount path.

use std::io::Cursor;
use std::sync::Arc;
use std::time::{Duration, Instant};

use graphersal::auth::AccessPolicy;
use graphersal::exec::CancelToken;
use graphersal_session::SessionPresets;
use graphersal_session::queries::SavedQuerySelection;
use serde_json::{Value, json};

use crate::context::{Context, ContextBuilder, Runner, spawn_session};
use crate::host_check::{Exposure, HostCheck};
use crate::http::{Method, Request, Response};
use crate::runs::{RunDetails, Runs};
use crate::{RUN_TEXT_HEAD_CHARS, handle};

const PORT: u16 = 8080;
const HOST: &str = "127.0.0.1:8080";
const ENDLESS: &str = "g.inject(1).repeat(__.identity()).times(100000000).count().next()";

fn builder(runs: Arc<Runs>) -> ContextBuilder {
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
    Context::builder(
        Runner::Session(jobs),
        HostCheck::new(Exposure::Loopback, PORT, &format!("http://{HOST}/")),
    )
    .runs(runs)
}

fn request<'a>(method: Method, url: &str, body: &'a [u8]) -> Request<'a> {
    Request::new(method, url, Cursor::new(body)).with_header("Host", HOST)
}

fn json_request<'a>(url: &str, body: &'a [u8]) -> Request<'a> {
    request(Method::Post, url, body)
        .with_header("Content-Type", "application/json")
        .with_body_length(Some(body.len()))
}

fn body_json(response: &Response) -> Value {
    serde_json::from_slice(response.body()).expect("a JSON answer")
}

/// `POST /api/execute` of `script` by the page `client` on a thread of its own.
fn execute(
    context: &Arc<Context>,
    client: &'static str,
    script: String,
) -> std::thread::JoinHandle<Response> {
    let context = context.clone();
    std::thread::spawn(move || {
        let body = json!({ "script": script, "runId": "r1" }).to_string();
        handle(
            &context,
            json_request("/api/execute", body.as_bytes())
                .with_header("X-Graphersal-Client", client),
        )
    })
}

/// `GET /api/admin/runs` with `token` (if any), as the admin page `admin`.
fn list(context: &Arc<Context>, token: Option<&str>) -> Response {
    let mut req =
        request(Method::Get, "/api/admin/runs", b"").with_header("X-Graphersal-Client", "admin");
    if let Some(token) = token {
        req = req.with_header("X-Graphersal-Admin-Token", token);
    }
    handle(context, req)
}

fn kill(context: &Arc<Context>, id: u64) -> Response {
    let body = json!({ "id": id }).to_string();
    handle(
        context,
        json_request("/api/admin/runs/cancel", body.as_bytes())
            .with_header("X-Graphersal-Client", "admin"),
    )
}

/// Waits until the list holds `n` runs and `ready` holds of it: the test follows the server.
fn wait_for(context: &Arc<Context>, n: usize, ready: impl Fn(&[Value]) -> bool) -> Vec<Value> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let runs = body_json(&list(context, None))["runs"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if runs.len() == n && ready(&runs) {
            return runs;
        }
        assert!(
            Instant::now() < deadline,
            "the runs never became ready: {runs:?}"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn the_run_administration_is_off_by_default() {
    let context = builder(Arc::new(Runs::default())).build();
    assert!(!context.admin_runs_enabled());
    let info = body_json(&handle(&context, request(Method::Get, "/api/info", b"")));
    assert!(info.get("adminRuns").is_none(), "{info}");
    let listed = list(&context, None);
    assert_eq!(listed.status(), 404);
    assert_eq!(
        body_json(&listed)["error"],
        "the run administration is off: start the server with --admin-runs"
    );
    assert_eq!(kill(&context, 1).status(), 404);
    // A wrong method of an off endpoint says the same (nothing tells it is there).
    let wrong = handle(&context, request(Method::Delete, "/api/admin/runs", b""));
    assert_eq!(wrong.status(), 404);
}

#[test]
fn the_list_sees_other_clients_runs_and_the_kill_cancels_them() {
    let runs = Arc::new(Runs::default());
    let context = builder(runs.clone()).admin_runs(None).build();
    let info = body_json(&handle(&context, request(Method::Get, "/api/info", b"")));
    assert_eq!(info["adminRuns"], json!({ "tokenRequired": false }));

    // One running query of tab-a, and one of tab-c queued behind it (one session thread), with a
    // text longer than the head the list shows.
    let running = execute(&context, "tab-a", ENDLESS.to_string());
    let first = wait_for(&context, 1, |runs| runs[0]["state"] == "running");
    let long = format!("{ENDLESS} // {}", "x".repeat(400));
    let queued = execute(&context, "tab-c", long.clone());
    let listed = wait_for(&context, 2, |_| true);
    assert_eq!(listed[0]["id"], first[0]["id"]);
    assert_eq!(listed[0]["client"], "page:tab-a");
    assert_eq!(listed[0]["alias"], "r1");
    assert_eq!(listed[0]["kind"], "query");
    assert_eq!(listed[0]["state"], "running");
    assert_eq!(listed[0]["text"], ENDLESS);
    assert_eq!(listed[0]["cancelled"], false);
    assert_eq!(listed[0]["mount"], Value::Null);
    assert!(listed[0]["startedAtMs"].as_u64().unwrap() > 0);
    assert!(listed[0]["durationMs"].is_u64());
    assert_eq!(listed[1]["client"], "page:tab-c");
    assert_eq!(listed[1]["state"], "queued");
    let head: String = long.chars().take(RUN_TEXT_HEAD_CHARS).collect();
    assert_eq!(listed[1]["text"], head.as_str());
    assert_eq!(
        body_json(&list(&context, None))["textHeadChars"],
        RUN_TEXT_HEAD_CHARS
    );

    // The admin (another client) kills the queued run first: it stays listed, cancelled, until
    // its job starts and stops at once.
    let id_queued = listed[1]["id"].as_u64().unwrap();
    let id_running = listed[0]["id"].as_u64().unwrap();
    assert_eq!(body_json(&kill(&context, id_queued))["cancelled"], true);
    let after = wait_for(&context, 2, |_| true);
    assert_eq!(after[1]["cancelled"], true);
    assert_eq!(after[1]["state"], "queued");
    // A client's own Stop of the admin page reaches neither run.
    let own = handle(
        &context,
        request(Method::Post, "/api/cancel", b"").with_header("X-Graphersal-Client", "admin"),
    );
    assert_eq!(body_json(&own)["cancelled"], false);
    assert_eq!(body_json(&kill(&context, id_running))["cancelled"], true);
    for answer in [running.join().unwrap(), queued.join().unwrap()] {
        assert_eq!(answer.status(), 200);
        let text = String::from_utf8_lossy(answer.body()).to_lowercase();
        assert!(text.contains("cancel"), "{text}");
    }
    wait_for(&context, 0, |_| true);
    // An ended run is not killed again; a body without an id is refused.
    assert_eq!(body_json(&kill(&context, id_running))["cancelled"], false);
    let bad = handle(
        &context,
        json_request("/api/admin/runs/cancel", br#"{"id": "one"}"#),
    );
    assert_eq!(bad.status(), 400);
    let wrong = handle(&context, request(Method::Delete, "/api/admin/runs", b""));
    assert_eq!(wrong.status(), 405);
}

#[test]
fn the_token_is_checked() {
    let context = builder(Arc::new(Runs::default()))
        .admin_runs(Some("s3cret".to_string()))
        .build();
    let info = body_json(&handle(&context, request(Method::Get, "/api/info", b"")));
    assert_eq!(info["adminRuns"], json!({ "tokenRequired": true }));
    let missing = list(&context, None);
    assert_eq!(missing.status(), 403);
    assert_eq!(
        body_json(&missing)["error"],
        "the run administration needs its token: send the header X-Graphersal-Admin-Token"
    );
    let wrong = list(&context, Some("s3cre7"));
    assert_eq!(wrong.status(), 403);
    assert!(!String::from_utf8_lossy(wrong.body()).contains("s3cre"));
    assert_eq!(list(&context, Some("s3cret")).status(), 200);
    // The kill needs it too.
    assert_eq!(kill(&context, 1).status(), 403);
    let body = br#"{"id": 1}"#;
    let killed = handle(
        &context,
        json_request("/api/admin/runs/cancel", body)
            .with_header("X-Graphersal-Admin-Token", "s3cret"),
    );
    assert_eq!(killed.status(), 200);
    assert_eq!(body_json(&killed)["cancelled"], false);
}

#[test]
fn the_allow_list_mode_refuses_the_run_administration() {
    let context = builder(Arc::new(Runs::default()))
        .admin_runs(None)
        .api_allow_list(
            SavedQuerySelection::of(["memory"], ["ui_status"]),
            AccessPolicy::read_only(),
        )
        .build();
    assert_eq!(list(&context, None).status(), 403);
    assert_eq!(kill(&context, 1).status(), 403);
}

#[test]
fn a_mounted_context_lists_and_kills_only_its_own_runs() {
    let runs = Arc::new(Runs::default());
    let context = builder(runs.clone())
        .mount_path(Some("/db/a".to_string()))
        .admin_runs(None)
        .build();
    let other = CancelToken::new();
    let other_id = runs
        .start_with(
            "page:x",
            None,
            other.clone(),
            RunDetails {
                mount: Some("/db/b".to_string()),
                ..RunDetails::default()
            },
        )
        .unwrap();
    let own = CancelToken::new();
    let own_id = runs
        .start_with(
            "page:y",
            None,
            own.clone(),
            RunDetails {
                mount: Some("/db/a".to_string()),
                ..RunDetails::default()
            },
        )
        .unwrap();
    let listed = body_json(&list(&context, None));
    let ids: Vec<u64> = listed["runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|run| run["id"].as_u64().unwrap())
        .collect();
    assert_eq!(ids, [own_id]);
    assert_eq!(listed["runs"][0]["mount"], "/db/a");
    assert_eq!(body_json(&kill(&context, other_id))["cancelled"], false);
    assert!(!other.is_cancelled());
    assert_eq!(body_json(&kill(&context, own_id))["cancelled"], true);
    assert!(own.is_cancelled());
    // The host's own view sees both.
    assert_eq!(runs.list().unwrap().len(), 2);
}
