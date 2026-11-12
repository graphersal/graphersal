//! Tests of `POST /api/query` and the allow-list mode of the HTTP API
//! ([`crate::ContextBuilder::api_allow_list`]) at the boundary, without an HTTP library: a
//! [`Context`] on a real session thread (the `modern` sample plus a few saved queries), requests
//! built as [`Request`] values and answered by [`handle`].

use std::io::Cursor;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use graphersal::auth::{AccessPolicy, ActionKind, ResourceKind};
use graphersal_session::queries::SavedQuerySelection;
use graphersal_session::{RunOptions, SessionPresets};
use serde_json::{Value, json};

use crate::context::{Context, ContextBuilder, Hooks, RequestFilter, Runner, spawn_session};
use crate::handle;
use crate::host_check::{Exposure, HostCheck};
use crate::http::{HttpError, Method, Request, Response};

const PORT: u16 = 8080;
const HOST: &str = "127.0.0.1:8080";

/// The saved queries of the tests: `memory/add_note` creates a vertex, `memory/sub/count_notes`
/// reads, `memory/forget_all` deletes vertices, `other/people` reads, `ui_status` (no folder)
/// reads.
const QUERIES: &str = r#"
g.define_query(#{name: "add_note", folder: "memory", params: #{text: "string", project: "string"},
    body: "fn add_note(text, project) { g.add_v(\"note\").property(\"text\", text).property(\"project\", project) }"});
g.define_query(#{name: "count_notes", folder: "memory/sub", params: #{project: "string"},
    body: "fn count_notes(project) { g.v().has_label(\"note\").has(\"project\", project).count() }"});
g.define_query(#{name: "forget_all", folder: "memory",
    body: "fn forget_all() { g.v().has_label(\"note\").drop() }"});
g.define_query(#{name: "people", folder: "other",
    body: "fn people() { g.v().has_label(\"person\").values(\"name\") }"});
g.define_query(#{name: "ui_status", body: "fn ui_status() { g.v().count() }"});
0
"#;

/// A session thread with the `modern` sample and [`QUERIES`].
fn session_runner() -> Runner {
    let (jobs, ()) = spawn_session(
        SessionPresets::new(),
        |session| {
            session
                .load_sample("modern")
                .map_err(|err| err.to_string())?;
            let answer: Value =
                serde_json::from_str(&session.execute(QUERIES, &RunOptions::default()))
                    .map_err(|err| err.to_string())?;
            if answer["ok"] == true {
                Ok(())
            } else {
                Err(format!("the saved queries are not defined: {answer}"))
            }
        },
        None,
    )
    .expect("the session thread starts");
    Runner::Session(jobs)
}

fn builder() -> ContextBuilder {
    Context::builder(
        session_runner(),
        HostCheck::new(Exposure::Loopback, PORT, &format!("http://{HOST}/")),
    )
}

/// The selection of the tests: the folder `memory` (with `memory/sub`) and `ui_status`.
fn selection() -> SavedQuerySelection {
    SavedQuerySelection::of(["memory"], ["ui_status"])
}

/// What `memory/add_note` needs (its inferred effects): reading plus creating and updating
/// data. Deleting data is not allowed.
fn policy() -> AccessPolicy {
    AccessPolicy::read_only()
        .allow(ActionKind::Create, ResourceKind::Data)
        .allow(ActionKind::Update, ResourceKind::Data)
}

fn allow_list_context() -> Arc<Context> {
    builder().api_allow_list(selection(), policy()).build()
}

fn request<'a>(method: Method, url: &str, body: &'a [u8]) -> Request<'a> {
    Request::new(method, url, Cursor::new(body)).with_header("Host", HOST)
}

fn json_request<'a>(url: &str, body: &'a [u8]) -> Request<'a> {
    request(Method::Post, url, body)
        .with_header("Content-Type", "application/json")
        .with_body_length(Some(body.len()))
}

fn query(context: &Arc<Context>, body: Value) -> Response {
    let body = body.to_string();
    handle(context, json_request("/api/query", body.as_bytes()))
}

fn body_json(response: &Response) -> Value {
    serde_json::from_slice(response.body()).expect("a JSON answer")
}

/// The number of `note` vertices, counted through the full API's `execute`.
fn notes(context: &Arc<Context>) -> i64 {
    let answer = handle(
        context,
        json_request(
            "/api/execute",
            br#"{"script": "g.v().has_label(\"note\").count().next()"}"#,
        ),
    );
    body_json(&answer)["execution"]["results"][0]
        .as_i64()
        .expect("a count")
}

#[test]
fn the_query_endpoint_runs_any_saved_query_in_full_mode() {
    let context = builder().build();
    let info = body_json(&handle(&context, request(Method::Get, "/api/info", b"")));
    assert_eq!(
        info.get("apiMode"),
        None,
        "a full server's info is unchanged"
    );

    let people = query(&context, json!({"name": "people"}));
    assert_eq!(people.status(), 200);
    let answer = body_json(&people);
    assert_eq!(answer["ok"], true, "{answer}");
    let mut names: Vec<&str> = answer["results"]
        .as_array()
        .expect("results")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    names.sort_unstable();
    assert_eq!(names, ["josh", "marko", "peter", "vadas"]);
    assert_eq!(answer["mutated"], false);
    assert_eq!(answer["changes"]["data"], false);

    // A writing query writes (the playground's authorizer) and says so.
    let added = body_json(&query(
        &context,
        json!({"name": "add_note", "params": {"text": "hi", "project": "p"}}),
    ));
    assert_eq!(added["ok"], true, "{added}");
    assert_eq!(added["mutated"], true);
    assert_eq!(added["changes"]["data"], true);
    assert_eq!(notes(&context), 1);
    let counted = body_json(&query(
        &context,
        json!({"name": "count_notes", "params": {"project": "p"}, "runId": "r1"}),
    ));
    assert_eq!(counted["results"], json!([1]), "{counted}");

    // The engine's refusals are answers (200, `ok: false`) with the error JSON of a run.
    let unknown = query(&context, json!({"name": "nope"}));
    assert_eq!(unknown.status(), 200);
    let unknown = body_json(&unknown);
    assert_eq!(unknown["ok"], false);
    assert!(
        unknown["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("nope")),
        "{unknown}"
    );
    let missing = body_json(&query(&context, json!({"name": "count_notes"})));
    assert_eq!(missing["ok"], false, "a missing parameter: {missing}");

    // Malformed calls are 400; another method is 405.
    for (body, text) in [
        (json!({}), "expected {\"name\""),
        (json!({"name": ""}), "expected {\"name\""),
        (json!({"name": 3}), "expected {\"name\""),
        (
            json!({"name": "people", "params": [1]}),
            "\"params\" is a JSON object",
        ),
        (
            json!({"name": "people", "runId": 5}),
            "runId must be a string",
        ),
    ] {
        let refused = query(&context, body.clone());
        assert_eq!(refused.status(), 400, "{body}");
        let error = body_json(&refused)["error"]
            .as_str()
            .unwrap_or("")
            .to_string();
        assert!(error.contains(text), "{body}: {error}");
    }
    assert_eq!(
        handle(&context, request(Method::Get, "/api/query", b"")).status(),
        405
    );
}

#[test]
fn allow_list_mode_answers_info_changes_and_query_and_refuses_every_other_api_route() {
    let context = allow_list_context();
    assert!(context.is_allow_list());
    let info = handle(&context, request(Method::Get, "/api/info", b""));
    assert_eq!(info.status(), 200);
    let info = body_json(&info);
    assert_eq!(info["apiMode"], "allow-list");
    assert_eq!(info["server"], crate::SERVER_ID);
    assert_eq!(info["mcp"], Value::Null);
    let changes = handle(&context, request(Method::Get, "/api/changes", b""));
    assert_eq!(changes.status(), 200);
    assert_eq!(body_json(&changes)["mcp"], Value::Null);

    let refusal = |method: &str, path: &str| {
        format!(
            "{method} {path} is not offered by this server: it answers only GET /api/info, \
             GET /api/changes and POST /api/query (allow-list mode)"
        )
    };
    for (method, url) in [
        (Method::Get, "/api/graph"),
        (Method::Get, "/api/schema"),
        (Method::Get, "/api/queries"),
        (Method::Get, "/api/queries/get?name=add_note"),
        (Method::Get, "/api/statistics"),
        (Method::Get, "/api/journal"),
        (Method::Get, "/api/store"),
        (Method::Get, "/api/completions"),
        (Method::Get, "/api/token-values"),
        (Method::Get, "/api/mcp/stats"),
        (Method::Get, "/api/openapi.json"),
        (Method::Get, "/api/no-such-endpoint"),
        (Method::Get, "/api/query"),
        (Method::Post, "/api/info"),
        (Method::Head, "/api/info"),
        (Method::Post, "/api/changes"),
        (Method::Post, "/api/execute"),
        (Method::Post, "/api/cancel"),
        (Method::Post, "/api/queries/define"),
        (Method::Post, "/api/queries/call-text"),
        (Method::Post, "/api/schema/set"),
        (Method::Post, "/api/save"),
        (Method::Post, "/api/layout"),
        (Method::Post, "/mcp"),
        (Method::Get, "/mcp"),
    ] {
        let path = url.split('?').next().unwrap_or(url);
        let body = br#"{"script": "g.v().drop()"}"#;
        let answer = handle(
            &context,
            request(method.clone(), url, body)
                .with_header("Content-Type", "application/json")
                .with_body_length(Some(body.len())),
        );
        assert_eq!(answer.status(), 403, "{method} {url}");
        assert_eq!(
            body_json(&answer)["error"],
            refusal(method.as_str(), path),
            "{method} {url}"
        );
    }
    // Nothing of that ran: the graph still has its six vertices.
    let status = body_json(&query(&context, json!({"name": "ui_status"})));
    assert_eq!(status["results"], json!([6]), "{status}");

    // The static UI and the OpenAPI description carry no data and stay served.
    assert_eq!(
        handle(&context, request(Method::Get, "/openapi.json", b"")).status(),
        200
    );
    assert_ne!(
        handle(&context, request(Method::Get, "/index.html", b"")).status(),
        403
    );
}

#[test]
fn allow_list_mode_runs_a_selected_query_under_the_policy_and_refuses_the_rest() {
    let context = allow_list_context();
    let feed_before = body_json(&handle(&context, request(Method::Get, "/api/changes", b"")));

    // A selected writing query whose effect the policy allows writes; the caller passes every
    // parameter (`project` included: the server binds none).
    let added = query(
        &context,
        json!({"name": "add_note", "params": {"text": "hello", "project": "graphersal"}}),
    );
    assert_eq!(added.status(), 200);
    let added = body_json(&added);
    assert_eq!(added["ok"], true, "{added}");
    assert_eq!(added["mutated"], true);
    let feed_after = body_json(&handle(&context, request(Method::Get, "/api/changes", b"")));
    assert_ne!(
        feed_before["version"], feed_after["version"],
        "the commit is in the change feed"
    );
    // A selected query in a subfolder, and one selected by name.
    let counted = body_json(&query(
        &context,
        json!({"name": "count_notes", "params": {"project": "graphersal"}}),
    ));
    assert_eq!(counted["results"], json!([1]), "{counted}");
    let status = body_json(&query(&context, json!({"name": "ui_status"})));
    assert_eq!(status["results"], json!([7]), "{status}");

    // A query outside the selection, and one that does not exist: the same 403.
    for name in ["people", "nope"] {
        let refused = query(&context, json!({"name": name}));
        assert_eq!(refused.status(), 403, "{name}");
        assert_eq!(
            body_json(&refused)["error"],
            format!("the saved query \"{name}\" is not offered by this server")
        );
    }

    // A selected query whose effect the policy does not allow is refused by the engine before
    // it runs: an answer with the engine's error, nothing changed.
    let forbidden = query(&context, json!({"name": "forget_all"}));
    assert_eq!(forbidden.status(), 200);
    let forbidden = body_json(&forbidden);
    assert_eq!(forbidden["ok"], false, "{forbidden}");
    assert_eq!(forbidden["mutated"], false);
    let message = forbidden["error"]["message"].as_str().unwrap_or("");
    assert!(message.contains("Permission denied"), "{forbidden}");
    let counted = body_json(&query(
        &context,
        json!({"name": "count_notes", "params": {"project": "graphersal"}}),
    ));
    assert_eq!(counted["results"], json!([1]), "{counted}");
}

/// A request filter that counts the requests it sees and refuses those without a token.
struct TokenFilter(AtomicUsize);

impl RequestFilter for TokenFilter {
    fn filter(&self, request: &mut Request<'_>) -> Result<(), HttpError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        if request.header("Authorization") == Some("Bearer t") {
            request.set_identity("ui");
            Ok(())
        } else {
            Err(HttpError::new(401, "missing token"))
        }
    }
}

/// An extra route of the host: refused in allow-list mode like every other `/api/` route.
struct ExtraRoute;

impl Hooks for ExtraRoute {
    fn route(
        &self,
        _context: &Arc<Context>,
        _request: &mut Request<'_>,
        endpoint: &str,
        _query: &str,
        _client: &str,
    ) -> Option<Result<Response, HttpError>> {
        (endpoint == "extra").then(|| Ok(Response::json(200, &json!({"extra": true}))))
    }
}

#[test]
fn in_allow_list_mode_the_host_check_runs_first_and_the_request_filter_still_sees_requests() {
    let filter = Arc::new(TokenFilter(AtomicUsize::new(0)));
    let context = builder()
        .api_allow_list(selection(), policy())
        .filter(filter.clone())
        .hooks(Arc::new(ExtraRoute))
        .build();
    let with_token = |request: Request<'static>| request.with_header("Authorization", "Bearer t");

    // A foreign Host: the HostCheck's 403, before the filter and the allow list.
    let foreign = handle(
        &context,
        Request::new(Method::Get, "/api/info", Cursor::new(&b""[..]))
            .with_header("Host", "evil.example:8080"),
    );
    assert_eq!(foreign.status(), 403);
    let error = body_json(&foreign)["error"]
        .as_str()
        .unwrap_or("")
        .to_string();
    assert!(!error.contains("allow-list"), "{error}");
    assert_eq!(
        filter.0.load(Ordering::SeqCst),
        0,
        "the filter never saw it"
    );

    // The filter sees every request the checks accepted, allowed or not: its refusal comes
    // before the allow list's.
    let no_token = handle(&context, request(Method::Get, "/api/graph", b""));
    assert_eq!(no_token.status(), 401);
    assert_eq!(filter.0.load(Ordering::SeqCst), 1);
    let refused = handle(
        &context,
        with_token(request(Method::Get, "/api/graph", b"")),
    );
    assert_eq!(refused.status(), 403);
    assert_eq!(filter.0.load(Ordering::SeqCst), 2);
    let extra = handle(
        &context,
        with_token(request(Method::Get, "/api/extra", b"")),
    );
    assert_eq!(
        extra.status(),
        403,
        "the host's extra routes are refused too"
    );
    let info = handle(&context, with_token(request(Method::Get, "/api/info", b"")));
    assert_eq!(info.status(), 200);
    let body = br#"{"name": "ui_status"}"#;
    let ran = handle(
        &context,
        with_token(
            request(Method::Post, "/api/query", body)
                .with_header("Content-Type", "application/json")
                .with_body_length(Some(body.len())),
        ),
    );
    assert_eq!(ran.status(), 200);
    assert_eq!(body_json(&ran)["results"], json!([6]));
    assert_eq!(filter.0.load(Ordering::SeqCst), 5);

    // Without the allow list the same host's extra route answers.
    let full = builder().hooks(Arc::new(ExtraRoute)).build();
    assert_eq!(
        handle(&full, request(Method::Get, "/api/extra", b"")).status(),
        200
    );
}
