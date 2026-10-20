//! Integration tests of `graphersal --server` (the DEV server): the binary is started on a free
//! port (`--port 0`, the address is the first stdout line) and driven with a minimal HTTP/1.1
//! client over `TcpStream`, the way the playground's `http-engine.js` calls it.
#![cfg(feature = "dev-server")]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use serde_json::Value;

/// A running server; killed when dropped.
struct DevServer {
    child: Child,
    port: u16,
}

impl Drop for DevServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A scratch directory with a minimal UI (`index.html`, `js/app.js`) and a secret file next to
/// it that must stay unreachable.
fn ui_dir(name: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!("graphersal_srv_{}_{name}", std::process::id()));
    let ui = base.join("ui");
    std::fs::create_dir_all(ui.join("js")).unwrap();
    std::fs::write(ui.join("index.html"), "<!doctype html><title>ui</title>").unwrap();
    std::fs::write(ui.join("js/app.js"), "export const x = 1;").unwrap();
    std::fs::write(base.join("secret.txt"), "top secret").unwrap();
    ui
}

fn start(args: &[&str]) -> DevServer {
    let mut child = Command::new(env!("CARGO_BIN_EXE_graphersal"))
        .args(args)
        .args(["--server", "--port", "0"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("graphersal starts");
    let stdout = child.stdout.take().expect("stdout");
    let mut line = String::new();
    BufReader::new(stdout).read_line(&mut line).unwrap();
    let port = line
        .trim()
        .trim_end_matches('/')
        .rsplit(':')
        .next()
        .and_then(|p| p.parse().ok())
        .unwrap_or_else(|| panic!("no address on stdout: {line:?}"));
    DevServer { child, port }
}

struct Answer {
    status: u16,
    headers: String,
    body: String,
}

impl Answer {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|e| panic!("{e}: {}", self.body))
    }
}

/// One request with `Connection: close`; `extra` are additional header lines.
/// Connects to the server; retried for a while when the client runs out of local ports (a long
/// fuzz run leaves thousands of connections in TIME_WAIT).
fn connect(port: u16) -> std::io::Result<TcpStream> {
    let mut attempt = 0;
    loop {
        match TcpStream::connect(("127.0.0.1", port)) {
            Err(err)
                if attempt < 200
                    && matches!(
                        err.kind(),
                        std::io::ErrorKind::AddrNotAvailable
                            | std::io::ErrorKind::ConnectionRefused
                    ) =>
            {
                attempt += 1;
                std::thread::sleep(Duration::from_millis(100));
            }
            other => return other,
        }
    }
}

fn request(port: u16, method: &str, path: &str, body: Option<&str>, extra: &[&str]) -> Answer {
    let mut stream = connect(port).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    let mut head = format!("{method} {path} HTTP/1.1\r\nConnection: close\r\n");
    if !extra.iter().any(|h| h.starts_with("Host:")) {
        head.push_str(&format!("Host: 127.0.0.1:{port}\r\n"));
    }
    for h in extra {
        head.push_str(h);
        head.push_str("\r\n");
    }
    let body = body.unwrap_or("");
    head.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    stream.write_all(head.as_bytes()).unwrap();
    stream.write_all(body.as_bytes()).unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .unwrap_or(raw.len());
    let headers = String::from_utf8_lossy(&raw[..split]).to_string();
    let body = raw.get(split + 4..).unwrap_or_default();
    let status = headers
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    // tiny_http sends a body above 32 KiB with chunked transfer encoding.
    let body = if headers
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        dechunk(body)
    } else {
        body.to_vec()
    };
    Answer {
        status,
        headers,
        body: String::from_utf8_lossy(&body).to_string(),
    }
}

/// The body bytes of a chunked HTTP/1.1 response.
fn dechunk(mut rest: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    while let Some(eol) = rest.windows(2).position(|w| w == b"\r\n") {
        let size = std::str::from_utf8(&rest[..eol])
            .ok()
            .and_then(|s| usize::from_str_radix(s.trim(), 16).ok())
            .unwrap_or(0);
        let data = &rest[eol + 2..];
        if size == 0 || data.len() < size {
            break;
        }
        body.extend_from_slice(&data[..size]);
        rest = data[size..].strip_prefix(b"\r\n").unwrap_or(&data[size..]);
    }
    body
}

const JSON: &str = "Content-Type: application/json";

fn get(port: u16, path: &str) -> Answer {
    request(port, "GET", path, None, &[])
}

fn post(port: u16, path: &str, body: &str) -> Answer {
    request(port, "POST", path, Some(body), &[JSON])
}

fn execute(port: u16, script: &str) -> Value {
    let body = serde_json::json!({ "script": script, "options": { "profile": true } });
    let answer = post(port, "/api/execute", &body.to_string());
    assert_eq!(answer.status, 200, "{}", answer.body);
    answer.json()
}

#[test]
fn graph_view_is_columnar_and_element_fetches_the_properties() {
    let ui = ui_dir("graph-view");
    let server = start(&["--graph", "modern", "--ui-dir", ui.to_str().unwrap()]);
    let port = server.port;

    let view = get(port, "/api/graph-view?maxNodes=100&maxEdges=100").json();
    assert_eq!(view["vertexIds"].as_array().unwrap().len(), 6, "{view}");
    assert_eq!(view["edgeSource"].as_array().unwrap().len(), 6);
    assert_eq!(view["totalNodes"], 6);
    assert_eq!(view["truncated"], false);
    assert!(
        view.get("nodes").is_none(),
        "no properties in the view: {view}"
    );

    let marko = get(port, "/api/element?kind=vertex&id=1").json();
    assert_eq!(marko["kind"], "vertex");
    assert_eq!(
        marko["properties"],
        serde_json::json!({"name": "marko", "age": 29})
    );
    let edge = get(port, "/api/element?kind=edge&id=0").json();
    assert_eq!(edge["label"], "knows");
    assert_eq!(edge["properties"]["weight"], 0.5);
    // A dropped (or unknown) element is null; a bad kind or a missing id is refused.
    assert_eq!(
        get(port, "/api/element?kind=vertex&id=42").json(),
        Value::Null
    );
    let bad = get(port, "/api/element?kind=graph&id=1");
    assert_eq!(bad.status, 422, "{}", bad.body);
    assert!(bad.body.contains("unknown element kind"), "{}", bad.body);
    assert_eq!(get(port, "/api/element?kind=vertex").status, 400);
    // A result's subgraph has the same columns.
    let r = execute(port, "g.v(1).out_e()");
    assert_eq!(r["subgraph"]["edgeIds"].as_array().unwrap().len(), 3, "{r}");
}

/// The Graph panel's property editor over HTTP: the typed edit view, a dry run (no commit), a
/// save (one commit, in the change feed) and a refused value (nothing written).
#[test]
fn element_editing_over_http() {
    let ui = ui_dir("element-edit");
    let server = start(&["--graph", "modern", "--ui-dir", ui.to_str().unwrap()]);
    let port = server.port;

    let view = get(port, "/api/element/edit?kind=edge&id=0").json();
    assert_eq!(view["properties"][0][1]["type"], "float64", "{view}");
    assert_eq!(view["readOnly"], Value::Null);
    assert_eq!(get(port, "/api/element/edit?kind=vertex").status, 400);
    let version = get(port, "/api/changes").json()["version"].clone();

    let set = serde_json::json!([["age", {"type": "int64", "value": "30"}]]);
    let checked = post(
        port,
        "/api/element/update",
        &serde_json::json!({"kind": "vertex", "id": "1", "set": set, "validateOnly": true})
            .to_string(),
    )
    .json();
    assert_eq!(checked["ok"], true, "{checked}");
    assert_eq!(get(port, "/api/changes").json()["version"], version);

    let saved = post(
        port,
        "/api/element/update",
        &serde_json::json!({"kind": "vertex", "id": "1", "set": set, "remove": ["name"]})
            .to_string(),
    )
    .json();
    assert_eq!(saved["changed"], true, "{saved}");
    assert_ne!(get(port, "/api/changes").json()["version"], version);
    let marko = get(port, "/api/element?kind=vertex&id=1").json();
    assert_eq!(marko["properties"], serde_json::json!({"age": 30}));

    let bad = post(
        port,
        "/api/element/update",
        r#"{"kind": "vertex", "id": "1", "set": [["age", {"type": "int64", "value": "x"}]]}"#,
    )
    .json();
    assert_eq!(bad["ok"], false);
    assert_eq!(bad["errors"][0]["field"], "age", "{bad}");
}

/// The Graph panel's edit mode over HTTP: label choices (from the schema's connections once a
/// closed schema is set), a dry run (no commit), creating a vertex and an edge, deleting a vertex
/// with its edges; every write is one commit in the change feed with the page's client id.
#[test]
fn creating_and_deleting_elements_over_http() {
    let ui = ui_dir("element-create");
    let server = start(&["--graph", "modern", "--ui-dir", ui.to_str().unwrap()]);
    let port = server.port;
    let as_page = |path: &str, body: Value| {
        let r = request(
            port,
            "POST",
            path,
            Some(&body.to_string()),
            &[JSON, "X-Graphersal-Client: tab-7"],
        );
        assert_eq!(r.status, 200, "{}", r.body);
        r.json()
    };

    let choices = as_page("/api/element/labels", serde_json::json!({"kind": "vertex"}));
    assert_eq!(choices["labels"][0]["label"], "person", "{choices}");
    assert_eq!(
        post(
            port,
            "/api/element/labels",
            r#"{"kind": "edge", "from": "1"}"#
        )
        .status,
        422
    );
    assert_eq!(get(port, "/api/element/create").status, 405);
    let version = changes(port)["version"].clone();

    let vertex = serde_json::json!({
        "kind": "vertex", "labels": ["person"],
        "properties": [["name", {"type": "string", "value": "ann"}]],
    });
    let mut dry = vertex.clone();
    dry["validateOnly"] = Value::Bool(true);
    let checked = as_page("/api/element/create", dry);
    assert_eq!(checked["ok"], true, "{checked}");
    assert_eq!(changes(port)["version"], version);

    let created = as_page("/api/element/create", vertex);
    assert_eq!(created["created"], true, "{created}");
    let id = created["drawn"]["id"].as_str().unwrap().to_string();
    assert_eq!(created["drawn"]["caption"], "ann");
    let feed = changes(port);
    assert_ne!(feed["version"], version, "{feed}");
    let last = feed["recent"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(last["by"], "tab-7", "{feed}");

    let edge = as_page(
        "/api/element/create",
        serde_json::json!({"kind": "edge", "label": "knows", "from": "1", "to": id}),
    );
    assert_eq!(edge["drawn"]["target"], Value::String(id.clone()), "{edge}");

    let preview = as_page(
        "/api/element/delete",
        serde_json::json!({"kind": "vertex", "id": id, "validateOnly": true}),
    );
    assert_eq!(preview["edges"], 1, "{preview}");
    let deleted = as_page(
        "/api/element/delete",
        serde_json::json!({"kind": "vertex", "id": id}),
    );
    assert_eq!(deleted["deleted"], true, "{deleted}");
    assert_eq!(get(port, "/api/graph").json()["vertexCount"], 6);

    // A closed schema: only the labels its connections allow, refusals at their fields.
    let schema = r#"{"mode": "closed",
        "vertices": {"person": {"schema": {"type": "object", "additionalProperties": true}},
                     "software": {"schema": {"type": "object", "additionalProperties": true}}},
        "edges": {"created": {"connections": [{"from": "person", "to": "software"}],
                              "schema": {"type": "object", "additionalProperties": true}},
                  "knows": {"connections": [{"from": "person", "to": "person"}],
                            "schema": {"type": "object", "additionalProperties": true}}}}"#;
    let set = post(port, "/api/schema/set", schema);
    assert_eq!(set.status, 200, "{}", set.body);
    let allowed = as_page(
        "/api/element/labels",
        serde_json::json!({"kind": "edge", "from": "1", "to": "3"}),
    );
    assert_eq!(allowed["free"], false, "{allowed}");
    assert_eq!(allowed["labels"].as_array().unwrap().len(), 1, "{allowed}");
    assert_eq!(allowed["labels"][0]["label"], "created");
    let refused = as_page(
        "/api/element/create",
        serde_json::json!({"kind": "edge", "label": "knows", "from": "1", "to": "3"}),
    );
    assert_eq!(refused["errors"][0]["field"], "label", "{refused}");
}

/// The Graph panel's selections over HTTP: the neighbourhood (expanded with `keep`), a label
/// sample, the label meta-graph, the result subgraph limit; bad requests are refused.
#[test]
fn neighbourhood_sample_and_meta_graph_over_http() {
    let ui = ui_dir("selection");
    let server = start(&["--graph", "modern", "--ui-dir", ui.to_str().unwrap()]);
    let port = server.port;

    let n = post(port, "/api/neighbourhood", r#"{"ids": ["2"], "hops": 1}"#).json();
    assert_eq!(n["vertexIds"].as_array().unwrap().len(), 2, "{n}");
    assert_eq!(n["hiddenEdges"], 2);
    let grown = post(
        port,
        "/api/neighbourhood",
        r#"{"ids": ["1"], "keep": ["1", "2"], "maxNodes": 3}"#,
    )
    .json();
    assert_eq!(grown["vertexIds"].as_array().unwrap().len(), 3, "{grown}");
    assert_eq!(grown["truncated"], true);
    // Depth and direction from the same centre; a cut result says how much is left out.
    let inn = post(
        port,
        "/api/neighbourhood",
        r#"{"ids": ["3"], "hops": 1, "direction": "in"}"#,
    )
    .json();
    assert_eq!(inn["vertexIds"].as_array().unwrap().len(), 4, "{inn}");
    let deep = post(
        port,
        "/api/neighbourhood",
        r#"{"ids": ["2"], "hops": 3, "direction": "both", "maxNodes": 3}"#,
    )
    .json();
    assert_eq!(deep["leftOut"]["vertices"], 3, "{deep}");
    let bad = post(
        port,
        "/api/neighbourhood",
        r#"{"ids": ["2"], "direction": "up"}"#,
    );
    assert_eq!(bad.status, 422, "{}", bad.body);
    let unknown = post(port, "/api/neighbourhood", r#"{"ids": ["nope"]}"#);
    assert_eq!(unknown.status, 422, "{}", unknown.body);
    assert!(unknown.body.contains("no vertex"), "{}", unknown.body);
    assert_eq!(post(port, "/api/neighbourhood", "[1]").status, 422);

    let sample = post(
        port,
        "/api/sample",
        r#"{"edge": {"label": "knows"}, "maxEdges": 1}"#,
    )
    .json();
    assert_eq!(sample["sample"]["kind"], "edges", "{sample}");
    assert_eq!(sample["sample"]["total"], 2);
    assert_eq!(sample["edgeIds"].as_array().unwrap().len(), 1);

    let meta = get(port, "/api/meta-graph").json();
    assert_eq!(meta["labels"].as_array().unwrap().len(), 2, "{meta}");
    assert_eq!(meta["edgeCount"], 6);

    let body = serde_json::json!({ "script": "g.v()", "options": { "subgraph_max_elements": 3 } });
    let r = post(port, "/api/execute", &body.to_string()).json();
    assert_eq!(
        r["subgraph"]["vertexIds"].as_array().unwrap().len(),
        3,
        "{r}"
    );
    assert_eq!(r["subgraph"]["stoppedBy"], "elements");
}

/// The engine's force layout over HTTP: slices, positions with the final status, the cache,
/// cancel, and refused inputs.
#[test]
fn the_force_layout_runs_in_slices_over_http() {
    use serde_json::json;
    let ui = ui_dir("layout");
    let server = start(&["--graph", "modern", "--ui-dir", ui.to_str().unwrap()]);
    let port = server.port;
    let view = get(port, "/api/graph-view?maxNodes=100&maxEdges=100").json();
    let body = json!({
        "vertexCount": 6,
        "edgeSource": view["edgeSource"],
        "edgeTarget": view["edgeTarget"],
        "budgetMs": 0,
    });
    let mut status = post(port, "/api/layout", &body.to_string()).json();
    assert_eq!(status["state"], "running", "{status}");
    assert_eq!(status["iterations"], 1);
    assert!(
        status.get("positions").is_none(),
        "a running job has none yet"
    );
    let job = status["job"].as_u64().unwrap();
    while status["state"] == "running" {
        let step = post(
            port,
            "/api/layout/step",
            &json!({"job": job, "budgetMs": 50}).to_string(),
        );
        assert_eq!(step.status, 200, "{}", step.body);
        status = step.json();
    }
    assert_eq!(status["state"], "done");
    let positions = status["positions"].as_array().unwrap().clone();
    assert_eq!(positions.len(), 12);
    assert!(positions.iter().all(|v| v.as_f64().unwrap().is_finite()));
    // The job is forgotten once its positions were answered; the same drawing comes from the
    // cache with the same positions.
    let gone = post(port, "/api/layout/step", &json!({"job": job}).to_string());
    assert_eq!(gone.status, 422, "{}", gone.body);
    let cached = post(port, "/api/layout", &body.to_string()).json();
    assert_eq!(cached["state"], "done");
    assert_eq!(cached["cached"], true);
    assert_eq!(cached["positions"].as_array().unwrap(), &positions);
    // Cancel keeps the positions reached so far.
    let other = post(
        port,
        "/api/layout",
        &json!({"vertexCount": 3, "edgeSource": [0], "edgeTarget": [1], "seed": 4, "budgetMs": 0})
            .to_string(),
    )
    .json();
    let cancelled = post(
        port,
        "/api/layout/cancel",
        &json!({"job": other["job"]}).to_string(),
    )
    .json();
    assert_eq!(cancelled["state"], "cancelled", "{cancelled}");
    assert_eq!(cancelled["positions"].as_array().unwrap().len(), 6);
    // Refused: an edge to a vertex that is not there, a malformed body, a missing job, GET.
    let bad = post(
        port,
        "/api/layout",
        &json!({"vertexCount": 2, "edgeSource": [0], "edgeTarget": [7]}).to_string(),
    );
    assert_eq!(bad.status, 422, "{}", bad.body);
    assert!(bad.body.contains("not a vertex index"), "{}", bad.body);
    assert_eq!(
        post(port, "/api/layout", r#"{"vertexCount": -1}"#).status,
        400
    );
    assert_eq!(post(port, "/api/layout/step", "{}").status, 400);
    assert_eq!(get(port, "/api/layout").status, 405);
}

#[test]
fn info_execute_graph_and_static_files() {
    let ui = ui_dir("basic");
    let server = start(&["--graph", "modern", "--ui-dir", ui.to_str().unwrap()]);
    let port = server.port;

    let info = get(port, "/api/info").json();
    assert_eq!(info["server"], "graphersal-dev");
    assert_eq!(info["dev"], true);
    assert_eq!(info["saveable"], false);

    let r = execute(port, r#"g.v().has_label("person").values("name")"#);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(r["terminal"], "execute");
    assert_eq!(r["execution"]["results"].as_array().unwrap().len(), 4);
    assert!(r["execution"]["profile"].is_object());

    // A mutation is kept; the graph info follows it.
    let r = execute(port, r#"g.addV("x").next()"#);
    assert_eq!(r["mutated"], true, "{r}");
    assert_eq!(get(port, "/api/graph").json()["vertexCount"], 7);

    let index = get(port, "/");
    assert_eq!(index.status, 200);
    assert!(
        index
            .headers
            .to_ascii_lowercase()
            .contains("content-type: text/html")
    );
    assert!(index.body.contains("<title>ui</title>"));
    let js = get(port, "/js/app.js");
    assert_eq!(js.status, 200);
    assert!(js.headers.to_ascii_lowercase().contains("text/javascript"));
    assert_eq!(get(port, "/missing.js").status, 404);

    for path in [
        "/../secret.txt",
        "/%2e%2e/secret.txt",
        "/js/../../secret.txt",
    ] {
        let answer = get(port, path);
        assert!(
            matches!(answer.status, 403 | 404),
            "{path}: {}",
            answer.status
        );
        assert!(!answer.body.contains("top secret"), "{path}");
    }
}

#[test]
fn malformed_and_foreign_requests_are_4xx_json() {
    let ui = ui_dir("malformed");
    let server = start(&["--graph", "empty", "--ui-dir", ui.to_str().unwrap()]);
    let port = server.port;
    let cases = [
        (post(port, "/api/execute", "{not json"), 400),
        (post(port, "/api/execute", r#"{"options": {}}"#), 400),
        (
            post(
                port,
                "/api/execute",
                r#"{"script": "1", "options": {"bogus": 1}}"#,
            ),
            400,
        ),
        (
            request(port, "POST", "/api/execute", Some(r#"{"script":"1"}"#), &[]),
            415,
        ),
        (get(port, "/api/execute"), 405),
        (get(port, "/api/nope"), 404),
        (post(port, "/api/save", ""), 409),
        (post(port, "/api/schema/set", r#"{"mode": "bogus"}"#), 422),
        (
            request(port, "GET", "/api/info", None, &["Host: evil.example:80"]),
            403,
        ),
        (
            request(
                port,
                "GET",
                "/api/info",
                None,
                &["Origin: http://evil.example"],
            ),
            403,
        ),
    ];
    for (i, (answer, status)) in cases.iter().enumerate() {
        assert_eq!(answer.status, *status, "case {i}: {}", answer.body);
        assert!(
            answer.json()["error"].is_string(),
            "case {i}: {}",
            answer.body
        );
    }
    // The server still answers.
    assert_eq!(execute(port, "1 + 1")["ok"], true);
}

/// The Schema panel draws what its editor shows: the views of a given schema (a draft), with
/// the graph's label counts, whatever the graph stores.
#[test]
fn schema_view_of_a_given_schema_over_http() {
    let ui = ui_dir("schema-view");
    let server = start(&["--graph", "modern", "--ui-dir", ui.to_str().unwrap()]);
    let port = server.port;
    let draft = r#"{"mode": "none", "vertices": {"city": {"schema": {"type": "object",
        "properties": {"name": {"type": "string"}}}}, "person": {"schema": {"type": "object"}}},
        "edges": {"lives in": {"connections": [{"from": "person", "to": "city"}]}}}"#;
    let view = post(port, "/api/schema/view", draft);
    assert_eq!(view.status, 200, "{}", view.body);
    let view = view.json();
    assert_eq!(view["source"], "given");
    assert_eq!(view["mode"], "none");
    let mermaid = view["mermaid"].as_str().unwrap();
    assert!(mermaid.contains("city {"), "{mermaid}");
    assert!(
        mermaid.contains(r#"person ||--o{ city : "lives in""#),
        "{mermaid}"
    );
    let nodes = view["graph"]["nodes"].as_array().unwrap();
    let person = nodes.iter().find(|n| n["label"] == "person").unwrap();
    assert_eq!(person["count"], 4, "{person}");
    // nothing stored
    assert_eq!(get(port, "/api/schema/stored").body, "null");
    let bad = post(port, "/api/schema/view", r#"{"vertices": 1}"#);
    assert_eq!(bad.status, 422, "{}", bad.body);
}

#[test]
fn schema_methods_over_http() {
    let ui = ui_dir("schema");
    let server = start(&["--graph", "modern", "--ui-dir", ui.to_str().unwrap()]);
    let port = server.port;
    assert_eq!(get(port, "/api/schema/stored").body, "null");
    let inferred = get(port, "/api/schema/infer").json();
    assert!(inferred["vertices"]["person"].is_object(), "{inferred}");
    let info = post(port, "/api/schema/set", &inferred.to_string());
    assert_eq!(info.status, 200, "{}", info.body);
    let patched = post(port, "/api/schema/patch", r#"{"mode": "open"}"#);
    assert_eq!(patched.status, 200, "{}", patched.body);
    assert_eq!(patched.json()["mode"], "open");
    assert_eq!(get(port, "/api/graph").json()["schemaMode"], "open");
    let view = get(port, "/api/schema").json();
    assert_eq!(view["source"], "stored");
    let validation = post(port, "/api/schema/validate-patch", r#"{"mode": "closed"}"#).json();
    assert!(validation["violations"].is_object(), "{validation}");
    // Freeze: the inferred schema with a mode (the query parameter replaces the schema's own).
    let frozen = post(port, "/api/schema/set?mode=closed", &inferred.to_string());
    assert_eq!(frozen.status, 200, "{}", frozen.body);
    assert_eq!(frozen.json()["schemaMode"], "closed");
    let bad = post(port, "/api/schema/set?mode=Closed", &inferred.to_string());
    assert_eq!(bad.status, 422, "{}", bad.body);
    assert!(bad.body.contains("unknown mode"), "{}", bad.body);
}

/// The completion's token values come from the engine, as in the WebAssembly build.
#[test]
fn token_values_come_from_the_engine() {
    let ui = ui_dir("token-values");
    let server = start(&["--graph", "modern", "--ui-dir", ui.to_str().unwrap()]);
    let tokens = get(server.port, "/api/token-values").json();
    assert_eq!(
        tokens["Order"],
        serde_json::json!(["asc", "desc", "shuffle"])
    );
    assert!(
        tokens["GType"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m == "LONG"),
        "{tokens}"
    );
    assert!(tokens.get("Execution").is_none(), "{tokens}");
}

#[test]
fn statistics_report_counts_memory_presets_and_defaults() {
    let ui = ui_dir("statistics");
    let server = start(&[
        "--graph",
        "modern",
        "--ui-dir",
        ui.to_str().unwrap(),
        "--max-traversers",
        "1000",
    ]);
    let port = server.port;
    let stats = get(port, "/api/statistics").json();
    assert_eq!(stats["statistics"]["vertex_count"], 6, "{stats}");
    assert_eq!(stats["statistics"]["edge_count"], 6, "{stats}");
    assert_eq!(
        stats["statistics"]["vertex_labels"][0]["label"], "person",
        "{stats}"
    );
    assert!(stats["memory"]["total_bytes"].is_number(), "{stats}");
    let limits = &stats["limits"];
    assert_eq!(
        limits["presets"]["options"]["traversal.max_traversers"], 1000,
        "{stats}"
    );
    // The defaults list only what no preset sets.
    assert!(
        limits["defaults"].get("traversal.max_traversers").is_none(),
        "{stats}"
    );
    assert_eq!(limits["defaults"]["repeat.max_loops"], 10000, "{stats}");
    // The same presets as /api/info.
    assert_eq!(get(port, "/api/info").json()["presets"], limits["presets"]);
    // A write is counted on the next call.
    execute(port, r#"g.addV("person").next()"#);
    let stats = get(port, "/api/statistics").json();
    assert_eq!(stats["statistics"]["vertex_count"], 7, "{stats}");
}

#[test]
fn cancel_stops_a_running_query_and_rolls_it_back() {
    let ui = ui_dir("cancel");
    let server = start(&["--graph", "modern", "--ui-dir", ui.to_str().unwrap()]);
    let port = server.port;
    // Nothing runs: nothing to cancel.
    assert_eq!(post(port, "/api/cancel", "").json()["cancelled"], false);
    for script in [
        r#"g.inject(1).addV("ghost").repeat(__.identity()).times(1000000000).count()"#,
        "let n = 0; loop { n += 1; }",
    ] {
        let script = script.to_string();
        let running = std::thread::spawn(move || execute(port, &script));
        let mut cancelled = false;
        for _ in 0..100 {
            std::thread::sleep(Duration::from_millis(50));
            if post(port, "/api/cancel", "").json()["cancelled"] == true {
                cancelled = true;
                break;
            }
        }
        assert!(cancelled);
        let r = running.join().unwrap();
        assert_eq!(r["ok"], false, "{r}");
        let message = r["execution"]["error"]["diagnostic"].as_str().unwrap();
        assert!(message.contains("cancelled"), "{message}");
        assert_eq!(get(port, "/api/graph").json()["vertexCount"], 6);
    }
    // The next query runs normally.
    assert_eq!(execute(port, "g.v().count().next()")["ok"], true);
}

#[test]
fn save_writes_the_graph_back_to_its_file() {
    let ui = ui_dir("save");
    let file = ui.parent().unwrap().join("graph.json");
    let status = Command::new(env!("CARGO_BIN_EXE_graphersal"))
        .args(["--graph", "modern", "-e"])
        .arg(format!("g.export_graphson({:?})", file.to_str().unwrap()))
        .status()
        .unwrap();
    assert!(status.success());
    let server = start(&[
        "--graph",
        file.to_str().unwrap(),
        "--ui-dir",
        ui.to_str().unwrap(),
    ]);
    let port = server.port;
    let info = get(port, "/api/info").json();
    assert_eq!(info["saveable"], true);
    assert_eq!(execute(port, r#"g.addV("saved").next()"#)["ok"], true);
    let saved = post(port, "/api/save", "");
    assert_eq!(saved.status, 200, "{}", saved.body);
    assert_eq!(saved.json()["format"], "graphson");
    // The file now holds 7 vertices, and no temporary file is left behind.
    let out = Command::new(env!("CARGO_BIN_EXE_graphersal"))
        .args([
            "--graph",
            file.to_str().unwrap(),
            "-e",
            "g.v().count().next()",
        ])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "7");
    let leftovers: Vec<_> = std::fs::read_dir(file.parent().unwrap())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty());
    // The export endpoint returns the same structure as the wasm `saveGraph`.
    let export = get(port, "/api/export?format=graphml").json();
    assert_eq!(export["format"], "graphml");
    assert!(export["text"].as_str().unwrap().contains("<graphml"));
}

#[test]
fn a_packed_snapshot_is_served_and_saved_back_as_a_snapshot() {
    let ui = ui_dir("gsnap");
    let file = ui.parent().unwrap().join("graph.gsnap");
    let status = Command::new(env!("CARGO_BIN_EXE_graphersal"))
        .args(["--graph", "modern", "-e"])
        .arg(format!("g.export_snapshot({:?})", file.to_str().unwrap()))
        .status()
        .unwrap();
    assert!(status.success());
    let server = start(&[
        "--graph",
        file.to_str().unwrap(),
        "--ui-dir",
        ui.to_str().unwrap(),
    ]);
    let port = server.port;
    assert_eq!(get(port, "/api/graph").json()["vertexCount"], 6);
    assert_eq!(get(port, "/api/info").json()["saveable"], true);
    assert_eq!(execute(port, r#"g.addV("saved").next()"#)["ok"], true);
    let saved = post(port, "/api/save", "");
    assert_eq!(saved.status, 200, "{}", saved.body);
    assert_eq!(saved.json()["format"], "gsnap");
    // The file is still a snapshot, now with 7 vertices.
    let out = Command::new(env!("CARGO_BIN_EXE_graphersal"))
        .args([
            "--graph",
            file.to_str().unwrap(),
            "-e",
            "g.v().count().next()",
        ])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "7");
}

#[test]
fn the_start_banner_prints_the_load_time_and_the_statistics() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_graphersal"))
        .args(["--graph", "modern", "--server", "--port", "0"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("graphersal starts");
    let stderr = child.stderr.take().expect("stderr");
    let mut banner = Vec::new();
    let mut lines = BufReader::new(stderr).lines();
    for line in lines.by_ref() {
        let line = line.unwrap();
        let last = line.starts_with("Limits");
        banner.push(line);
        if last {
            break;
        }
    }
    // Keep reading: the server writes more banner lines, and a closed pipe would fail them.
    std::thread::spawn(move || lines.for_each(drop));
    let mut stdout = child.stdout.take().expect("stdout");
    let mut first = String::new();
    BufReader::new(&mut stdout).read_line(&mut first).unwrap();
    let _ = child.kill();
    let _ = child.wait();
    let mut rest = String::new();
    stdout.read_to_string(&mut rest).unwrap();
    // stdout keeps only the address line.
    assert!(
        first.starts_with("Graphersal dev server: http://127.0.0.1:"),
        "{first}"
    );
    assert_eq!(rest, "");
    let banner = banner.join("\n");
    let loaded = banner
        .lines()
        .find(|line| line.starts_with("Loaded in "))
        .unwrap_or_else(|| panic!("no load line: {banner}"));
    assert!(
        loaded.ends_with(" ms: 6 vertices, 6 edges") || loaded.ends_with(" s: 6 vertices, 6 edges"),
        "{loaded}"
    );
    assert!(
        banner.contains("\nVertex labels (2): person 4, software 2\n"),
        "{banner}"
    );
    assert!(
        banner.contains("\nEdge labels (2): created 4, knows 2\n"),
        "{banner}"
    );
    assert!(banner.contains("\nMemory: graph "), "{banner}");
}

#[test]
fn a_missing_ui_dir_is_a_usage_error_with_a_hint() {
    let out = Command::new(env!("CARGO_BIN_EXE_graphersal"))
        .args([
            "--server",
            "--port",
            "0",
            "--ui-dir",
            "/nonexistent/graphersal-ui",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--ui-dir"), "{stderr}");
}

/// Starts the server in the directory `cwd` and returns it with its `UI: ...` banner line.
fn start_in(cwd: &std::path::Path, args: &[&str]) -> (DevServer, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_graphersal"))
        .current_dir(cwd)
        .args(args)
        .args(["--server", "--port", "0"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("graphersal starts");
    let stderr = child.stderr.take().expect("stderr");
    let mut banner = String::new();
    let mut lines = BufReader::new(stderr).lines();
    for line in lines.by_ref() {
        let line = line.unwrap();
        if line.starts_with("UI: ") {
            banner = line;
            break;
        }
    }
    // Keep reading: the server writes more banner lines, and a closed pipe would fail them.
    std::thread::spawn(move || lines.for_each(drop));
    let stdout = child.stdout.take().expect("stdout");
    let mut line = String::new();
    BufReader::new(stdout).read_line(&mut line).unwrap();
    let port = line
        .trim()
        .trim_end_matches('/')
        .rsplit(':')
        .next()
        .and_then(|p| p.parse().ok())
        .unwrap_or_else(|| panic!("no address on stdout: {line:?}"));
    (DevServer { child, port }, banner)
}

/// A file of the repository's playground (the source of the embedded UI).
fn playground_file(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../playground")
        .join(name);
    std::fs::read_to_string(path).unwrap()
}

#[test]
fn without_a_ui_directory_the_embedded_ui_is_served() {
    // A working directory without ./playground: the default silently falls back.
    let cwd = std::env::temp_dir().join(format!("graphersal_srv_{}_embedded", std::process::id()));
    std::fs::create_dir_all(&cwd).unwrap();
    let (server, banner) = start_in(&cwd, &["--graph", "modern"]);
    let port = server.port;
    assert_eq!(banner, "UI: embedded");

    let index = get(port, "/");
    assert_eq!(index.status, 200);
    let headers = index.headers.to_ascii_lowercase();
    assert!(headers.contains("content-type: text/html"), "{headers}");
    assert!(headers.contains("cache-control: no-store"), "{headers}");
    assert_eq!(index.body, playground_file("index.html"));
    let module = get(port, "/js/main.js");
    assert_eq!(module.status, 200);
    assert!(
        module
            .headers
            .to_ascii_lowercase()
            .contains("text/javascript")
    );
    assert_eq!(module.body, playground_file("js/main.js"));
    let css = get(port, "/css/app.css");
    assert!(css.headers.to_ascii_lowercase().contains("text/css"));
    assert_eq!(
        get(port, "/reset/").body,
        playground_file("reset/index.html")
    );
    assert_eq!(
        get(port, "/reset").body,
        playground_file("reset/index.html")
    );
    assert!(
        get(port, "/assets/favicon.svg")
            .headers
            .contains("image/svg+xml")
    );
    // Not part of the embedded UI.
    for path in ["/README.md", "/build.sh", "/serve.py", "/tests/x.test.mjs"] {
        assert_eq!(get(port, path).status, 404, "{path}");
    }
    for path in [
        "/../index.html",
        "/%2e%2e/playground/index.html",
        "/js/../../playground/index.html",
        "/js/%2e%2e/index.html",
        "/./index.html",
    ] {
        assert_eq!(get(port, path).status, 404, "{path}");
    }
    drop(server);
    let _ = std::fs::remove_dir_all(&cwd);
}

#[test]
fn a_file_of_the_ui_directory_wins_over_the_embedded_copy() {
    let ui = ui_dir("overlay");
    let (server, banner) = start_in(
        &std::env::temp_dir(),
        &["--graph", "modern", "--ui-dir", ui.to_str().unwrap()],
    );
    let port = server.port;
    assert!(banner.ends_with("(embedded fallback)"), "{banner}");
    assert!(banner.contains("overlay"), "{banner}");
    // The directory's changed index.html wins ...
    assert!(get(port, "/").body.contains("<title>ui</title>"));
    assert_eq!(get(port, "/js/app.js").body, "export const x = 1;");
    // ... a file missing there comes from the embedded copy.
    assert_eq!(get(port, "/js/main.js").body, playground_file("js/main.js"));
    assert_eq!(
        get(port, "/css/app.css").body,
        playground_file("css/app.css")
    );
    assert_eq!(get(port, "/missing.js").status, 404);
    for path in [
        "/../secret.txt",
        "/%2e%2e/secret.txt",
        "/js/../../secret.txt",
    ] {
        let answer = get(port, path);
        assert_eq!(answer.status, 404, "{path}");
        assert!(!answer.body.contains("top secret"), "{path}");
    }
}

/// The diagnostic of a failed `/api/execute` answer.
fn diagnostic(answer: &Value) -> String {
    assert_eq!(answer["ok"], false, "{answer}");
    answer["execution"]["error"]["diagnostic"]
        .as_str()
        .unwrap_or_else(|| panic!("no diagnostic: {answer}"))
        .to_string()
}

#[test]
fn the_limit_options_are_session_presets() {
    let ui = ui_dir("limits");
    let server = start(&[
        "--graph",
        "empty",
        "--ui-dir",
        ui.to_str().unwrap(),
        "--timeout",
        "200",
        "--memory-limit",
        "64000000",
        "--memory-headroom",
        "1000000",
        "--max-operations",
        "100000000",
        "--max-traversers",
        "3",
    ]);
    let port = server.port;
    // /api/info reports what applies.
    let presets = get(port, "/api/info").json()["presets"].clone();
    assert_eq!(presets["options"]["evaluationTimeout"], 200, "{presets}");
    assert_eq!(presets["options"]["memory.limit"], 64000000, "{presets}");
    assert_eq!(presets["options"]["memory.headroom"], 1000000, "{presets}");
    assert_eq!(
        presets["options"]["traversal.max_traversers"], 3,
        "{presets}"
    );
    assert_eq!(
        presets["scriptLimits"]["max_operations"], 100000000,
        "{presets}"
    );
    assert_eq!(presets["scriptTimeoutMs"], 200, "{presets}");

    // A small query fits every limit.
    assert_eq!(execute(port, "g.v().count().next()")["ok"], true);
    // --timeout: the evaluationTimeout of every traversal ...
    let message = diagnostic(&execute(
        port,
        "g.inject(1).repeat(__.identity()).times(1000000000).count()",
    ));
    assert!(message.contains("evaluationTimeout"), "{message}");
    // ... which a query overrides ...
    let own = execute(
        port,
        r#"g.with("evaluationTimeout", 0).inject(1).repeat(__.identity()).times(10).count().next()"#,
    );
    assert_eq!(own["ok"], true, "{own}");
    // ... and the budget of a run as a whole: a loop between traversals stops too.
    let message = diagnostic(&execute(port, "let n = 0; loop { n += 1; }"));
    assert!(
        message.contains("Script exceeded evaluationTimeout of 200 ms"),
        "{message}"
    );
    // --max-traversers
    let message = diagnostic(&execute(port, "g.inject(1, 2, 3, 4, 5).to_list()"));
    assert!(message.contains("traversal.max_traversers"), "{message}");
    // --memory-limit: a run that grows past it fails and rolls back.
    let message = diagnostic(&execute(
        port,
        r#"g.addV("x").inject(1).repeat(__.path()).times(40).count().next()"#,
    ));
    assert!(
        message.contains("Resource limit 'memory.limit' exceeded (limit 64000000"),
        "{message}"
    );
    assert_eq!(get(port, "/api/graph").json()["vertexCount"], 0);
    // The server serves on.
    assert_eq!(execute(port, "g.v().count().next()")["ok"], true);
}

#[test]
fn the_script_limits_bound_a_run() {
    let ui = ui_dir("script_limits");
    let server = start(&[
        "--graph",
        "empty",
        "--ui-dir",
        ui.to_str().unwrap(),
        "--max-operations",
        "10000",
    ]);
    let port = server.port;
    let message = diagnostic(&execute(port, "let n = 0; loop { n += 1; }"));
    assert!(message.contains("Resource limit"), "{message}");
    assert_eq!(execute(port, "1 + 1")["ok"], true);
}

#[test]
fn without_limit_options_the_server_runs_like_the_playground() {
    let ui = ui_dir("no_limits");
    let server = start(&["--graph", "empty", "--ui-dir", ui.to_str().unwrap()]);
    let presets = get(server.port, "/api/info").json()["presets"].clone();
    assert_eq!(
        presets,
        serde_json::json!({"options": {}, "scriptLimits": {}, "scriptTimeoutMs": null})
    );
}

#[test]
fn display_options_are_rejected_with_server() {
    for flag in [
        &["--max-rows", "5"][..],
        &["--no-limit"][..],
        &["--format", "json"][..],
        &["--spelling", "camel"][..],
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_graphersal"))
            .args(["--server", "--port", "0"])
            .args(flag)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{flag:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains(&format!("{} does not apply to --server", flag[0])),
            "{stderr}"
        );
    }
}

/// The dev server on a Store directory: commits survive a restart (the first server is killed,
/// not closed), the store endpoints list, checkpoint, mark and download snapshots.
#[test]
fn a_server_on_a_store_directory_keeps_its_writes_across_restarts() {
    let ui = ui_dir("store");
    let data = ui.parent().unwrap().join("data");
    let _ = std::fs::remove_dir_all(&data);
    let data_arg = data.to_str().unwrap().to_string();
    let ui_arg = ui.to_str().unwrap().to_string();
    let server = start(&["--graph", &data_arg, "--ui-dir", &ui_arg, "--create-store"]);
    let port = server.port;
    let info = get(port, "/api/info").json();
    assert!(info["store"].as_str().unwrap().ends_with("data"), "{info}");
    let r = execute(
        port,
        r#"g.add_v("person").property("name", "ann").to_list()"#,
    );
    assert_eq!(r["ok"], true, "{r}");
    let snapshot = post(port, "/api/store/checkpoint", r#"{"name": "first"}"#).json();
    assert_eq!(snapshot["snapshot"]["commitSeq"], 1, "{snapshot}");
    assert_eq!(snapshot["snapshot"]["name"], "first");
    let mark = post(port, "/api/store/mark", r#"{"name": "after-ann"}"#).json();
    assert_eq!(mark["mark"]["commitSeq"], 1, "{mark}");
    let dup = post(port, "/api/store/mark", r#"{"name": "after-ann"}"#);
    assert_eq!(dup.status, 422, "{}", dup.body);
    execute(
        port,
        r#"g.add_v("person").property("name", "bob").to_list()"#,
    );
    let store = get(port, "/api/store").json();
    assert_eq!(store["store"]["commitSeq"], 2, "{store}");
    assert_eq!(store["store"]["snapshots"].as_array().unwrap().len(), 2);
    assert_eq!(store["store"]["marks"][0]["name"], "after-ann");
    let download = get(port, "/api/store/snapshot?commit=1");
    assert_eq!(download.status, 200);
    assert!(
        download.headers.contains("graph-1.gsnap"),
        "{}",
        download.headers
    );
    assert!(download.body.starts_with("GRSLPACK"));
    let current = get(port, "/api/export?format=gsnap");
    assert_eq!(current.status, 200);
    assert!(current.body.starts_with("GRSLPACK"));
    assert_eq!(get(port, "/api/store/snapshot?commit=99").status, 404);
    drop(server); // killed: no clean close

    let server = start(&["--graph", &data_arg, "--ui-dir", &ui_arg]);
    let r = execute(server.port, r#"g.v().values("name").order().to_list()"#);
    assert_eq!(
        r["execution"]["results"],
        serde_json::json!(["ann", "bob"]),
        "{r}"
    );
    drop(server);

    // Without a store: no store endpoints.
    let server = start(&["--graph", "modern", "--ui-dir", &ui_arg]);
    assert_eq!(get(server.port, "/api/store").json()["store"], Value::Null);
    assert_eq!(post(server.port, "/api/store/checkpoint", "{}").status, 409);
}

/// Ctrl+C (SIGINT) stops the server and closes its store cleanly; a damaged store is served
/// read-only in maintenance mode with its damage report.
#[cfg(unix)]
#[test]
fn ctrl_c_closes_the_store_cleanly_and_a_damaged_store_is_served_read_only() {
    let ui = ui_dir("store-signal");
    let data = ui.parent().unwrap().join("data-signal");
    let _ = std::fs::remove_dir_all(&data);
    let data_arg = data.to_str().unwrap().to_string();
    let ui_arg = ui.to_str().unwrap().to_string();
    let mut server = start(&["--graph", &data_arg, "--ui-dir", &ui_arg, "--create-store"]);
    let r = execute(
        server.port,
        r#"g.add_v("person").property("name", "ann").to_list()"#,
    );
    assert_eq!(r["ok"], true, "{r}");
    let info = get(server.port, "/api/info").json();
    assert_eq!(info["maintenance"], Value::Null, "{info}");
    let status = Command::new("kill")
        .args(["-INT", &server.child.id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let exit = loop {
        if let Some(exit) = server.child.try_wait().unwrap() {
            break exit;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the server did not stop"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(exit.success(), "{exit:?}");
    let info = Command::new(env!("CARGO_BIN_EXE_graphersal"))
        .args(["store", "info", &data_arg])
        .output()
        .unwrap();
    let info = String::from_utf8_lossy(&info.stdout);
    assert!(info.contains("closed cleanly"), "{info}");

    // A damaged WAL record: maintenance mode, read-only, the report in the API.
    let wal = std::fs::read_dir(data.join("wal"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .max()
        .unwrap();
    let mut bytes = std::fs::read(&wal).unwrap();
    let last = bytes.len() - 3;
    bytes[last] ^= 0x40;
    std::fs::write(&wal, bytes).unwrap();
    let server = start(&["--graph", &data_arg, "--ui-dir", &ui_arg]);
    let info = get(server.port, "/api/info").json();
    assert_eq!(info["maintenance"]["recoverable"], false, "{info}");
    let store = get(server.port, "/api/store").json();
    let damage = store["store"]["maintenance"]["damage"].as_array().unwrap();
    assert_eq!(damage[0]["kind"], "wal_record", "{store}");
    assert_eq!(damage[0]["lost"], true, "{store}");
    let r = execute(server.port, r#"g.v().count().next()"#);
    assert_eq!(r["execution"]["results"], serde_json::json!([0]), "{r}");
    let r = execute(
        server.port,
        r#"g.add_v("person").property("name", "bob").to_list()"#,
    );
    assert_eq!(r["ok"], false, "{r}");
    assert!(r.to_string().contains("maintenance mode"), "{r}");
    let checkpoint = post(server.port, "/api/store/checkpoint", "{}");
    assert_eq!(checkpoint.status, 422, "{}", checkpoint.body);
    drop(server);
    let _ = std::fs::remove_dir_all(&data);
}

/// A backup directory is served READ-ONLY: `/api/info` and `/api/store` carry the marker for
/// the banner, queries read it, writes, checkpoints and marks are refused, a read-only view of
/// an older commit it holds works.
#[test]
fn a_backup_directory_is_served_read_only() {
    let ui = ui_dir("backup");
    let base = ui.parent().unwrap().to_path_buf();
    let data = base.join("data-backup-src");
    let backup = base.join("data-backup");
    for path in [&data, &backup] {
        let _ = std::fs::remove_dir_all(path);
    }
    let graphersal = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_graphersal"))
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    let (d, b) = (data.to_str().unwrap(), backup.to_str().unwrap());
    graphersal(&["store", "create", d, "--from", "modern"]);
    graphersal(&[
        "--graph",
        d,
        "-e",
        r#"g.add_v("person").property("name", "ann").to_list()"#,
    ]);
    graphersal(&["store", "backup", d, b]);
    let ui_arg = ui.to_str().unwrap().to_string();
    let server = start(&["--graph", b, "--ui-dir", &ui_arg]);
    let info = get(server.port, "/api/info").json();
    assert_eq!(info["backup"]["commitSeq"], 1, "{info}");
    assert!(
        info["backup"]["restore"]
            .as_str()
            .unwrap()
            .contains("store restore"),
        "{info}"
    );
    let store = get(server.port, "/api/store").json();
    assert_eq!(store["store"]["backup"]["commitSeq"], 1, "{store}");
    let r = execute(server.port, r#"g.v().count().next()"#);
    assert_eq!(r["execution"]["results"], serde_json::json!([7]), "{r}");
    let r = execute(server.port, r#"g.add_v("x").to_list()"#);
    assert_eq!(r["ok"], false, "{r}");
    assert!(r.to_string().contains("is a backup"), "{r}");
    for path in ["/api/store/checkpoint", "/api/store/mark"] {
        let answer = post(server.port, path, r#"{"name": "x"}"#);
        assert_eq!(answer.status, 422, "{path}: {}", answer.body);
    }
    let view = ok_json(post(
        server.port,
        "/api/store/view",
        r#"{"target": {"commit": 0}}"#,
    ));
    assert_eq!(view["view"]["commitSeq"], 0, "{view}");
    assert_eq!(names(server.port).as_array().unwrap().len(), 6);
    drop(server);
    for path in [&data, &backup] {
        let _ = std::fs::remove_dir_all(path);
    }
}

fn names(port: u16) -> Value {
    let r = execute(port, r#"g.v().values("name").order().to_list()"#);
    assert_eq!(r["ok"], true, "{r}");
    r["execution"]["results"].clone()
}

fn ok_json(answer: Answer) -> Value {
    assert_eq!(answer.status, 200, "{}", answer.body);
    answer.json()
}

/// The Store menu's time travel over HTTP: a read-only view at a mark, a commit and a time
/// (writes refused, back to live), a fork next to the store, a rollback (that cancels the
/// running query) with the attic: changes, restore, the restore refused after a new commit,
/// fork and remove of an entry.
#[test]
fn store_views_forks_rollbacks_and_the_attic_over_http() {
    let ui = ui_dir("store-travel");
    let base = ui.parent().unwrap().to_path_buf();
    let data = base.join("data");
    for entry in std::fs::read_dir(&base).unwrap().flatten() {
        if entry.file_name().to_string_lossy().starts_with("data") {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
    let data_arg = data.to_str().unwrap().to_string();
    let ui_arg = ui.to_str().unwrap().to_string();
    let server = start(&["--graph", &data_arg, "--ui-dir", &ui_arg, "--create-store"]);
    let port = server.port;
    let add = |name: &str| {
        let r = execute(
            port,
            &format!(r#"g.add_v("person").property("name", "{name}").to_list()"#),
        );
        assert_eq!(r["ok"], true, "{r}");
    };
    add("ann"); // commit 1
    ok_json(post(port, "/api/store/mark", r#"{"name": "m1"}"#));
    add("bob"); // commit 2
    ok_json(post(port, "/api/store/checkpoint", r#"{"name": "two"}"#));
    add("cy"); // commit 3
    let store = ok_json(get(port, "/api/store"))["store"].clone();
    assert_eq!(store["commitSeq"], 3, "{store}");
    assert_eq!(store["view"], Value::Null, "{store}");
    let mark_time = store["marks"][0]["time"].as_i64().unwrap();

    // A read-only view at the mark: queries read it, writes are refused, Checkpoint too.
    let view = ok_json(post(
        port,
        "/api/store/view",
        r#"{"target": {"mark": "m1"}}"#,
    ));
    assert_eq!(view["view"]["commitSeq"], 1, "{view}");
    assert!(
        view["graph"]["name"]
            .as_str()
            .unwrap()
            .contains("Read-only view of commit 1"),
        "{view}"
    );
    assert_eq!(names(port), serde_json::json!(["ann"]));
    assert_eq!(get(port, "/api/info").json()["view"]["commitSeq"], 1);
    let r = execute(port, r#"g.add_v("person").property("name", "x").to_list()"#);
    assert_eq!(r["ok"], false, "{r}");
    assert!(
        r.to_string()
            .contains("the served state is a read-only view at commit 1 (mark"),
        "{r}"
    );
    assert!(r.to_string().contains("Back to the live graph"), "{r}");
    assert!(!r.to_string().contains("write-ahead"), "{r}");
    assert_eq!(names(port), serde_json::json!(["ann"]));
    let refused = post(port, "/api/store/checkpoint", "{}");
    assert_eq!(refused.status, 409, "{}", refused.body);
    // By commit, by time (microseconds and RFC 3339).
    let view = ok_json(post(
        port,
        "/api/store/view",
        r#"{"target": {"commit": 2}}"#,
    ));
    assert_eq!(view["view"]["commitSeq"], 2, "{view}");
    assert_eq!(names(port), serde_json::json!(["ann", "bob"]));
    let body = format!(r#"{{"target": {{"time": {mark_time}}}}}"#);
    let view = ok_json(post(port, "/api/store/view", &body));
    assert_eq!(view["view"]["commitSeq"], 1, "{view}");
    let view = ok_json(post(
        port,
        "/api/store/view",
        r#"{"target": {"time": "2000-01-01T00:00Z"}}"#,
    ));
    assert_eq!(view["view"]["commitSeq"], 0, "{view}");
    assert_eq!(names(port), serde_json::json!([]));
    assert_eq!(
        post(port, "/api/store/view", r#"{"target": {"mark": "nope"}}"#).status,
        422
    );
    assert_eq!(post(port, "/api/store/view", r#"{}"#).status, 400);
    assert_eq!(
        post(port, "/api/store/view", r#"{"target": {"time": "noon"}}"#).status,
        400
    );
    let live = ok_json(post(port, "/api/store/live", "{}"));
    assert_eq!(live["view"], Value::Null);
    assert_eq!(names(port), serde_json::json!(["ann", "bob", "cy"]));
    assert_eq!(get(port, "/api/info").json()["view"], Value::Null);

    // A fork next to the store (the default directory), then a taken directory.
    let fork = ok_json(post(
        port,
        "/api/store/fork",
        r#"{"target": {"commit": 2}}"#,
    ));
    let fork_dir = fork["fork"]["dir"].as_str().unwrap().to_string();
    assert!(fork_dir.ends_with("data-fork-commit-2"), "{fork}");
    assert!(
        fork["fork"]["open"].as_str().unwrap().contains("--server"),
        "{fork}"
    );
    let count = Command::new(env!("CARGO_BIN_EXE_graphersal"))
        .args(["--graph", &fork_dir, "-e", "g.v().count().next()"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&count.stdout).trim(), "2");
    let taken = post(
        port,
        "/api/store/fork",
        r#"{"target": {"commit": 2}, "dir": "data-fork-commit-2"}"#,
    );
    assert_eq!(taken.status, 409, "{}", taken.body);
    let again = ok_json(post(
        port,
        "/api/store/fork",
        r#"{"target": {"mark": "m1"}}"#,
    ));
    assert!(
        again["fork"]["dir"]
            .as_str()
            .unwrap()
            .ends_with("data-fork-mark-m1"),
        "{again}"
    );

    // A rollback from inside a view cancels the running query and lands on the live graph.
    // (Opening a view cancels the running query too: the graph changes under it.)
    let running = std::thread::spawn(move || execute(port, "let n = 0; loop { n += 1; }"));
    std::thread::sleep(Duration::from_millis(500));
    ok_json(post(
        port,
        "/api/store/view",
        r#"{"target": {"commit": 2}}"#,
    ));
    assert_eq!(running.join().unwrap()["ok"], false);
    let running = std::thread::spawn(move || execute(port, "let n = 0; loop { n += 1; }"));
    std::thread::sleep(Duration::from_millis(500));
    let rolled = ok_json(post(
        port,
        "/api/store/rollback",
        r#"{"target": {"mark": "m1"}}"#,
    ));
    let r = running.join().unwrap();
    assert_eq!(r["ok"], false, "the running query was cancelled: {r}");
    assert_eq!(rolled["rollback"]["target"], 1, "{rolled}");
    assert_eq!(rolled["rollback"]["previousCommitSeq"], 3, "{rolled}");
    assert!(
        rolled["rollback"]["undo"]
            .as_str()
            .unwrap()
            .contains("restore"),
        "{rolled}"
    );
    assert_eq!(rolled["store"]["view"], Value::Null, "{rolled}");
    assert_eq!(rolled["graph"]["vertexCount"], 1, "{rolled}");
    assert_eq!(names(port), serde_json::json!(["ann"]));
    let id = rolled["rollback"]["attic"].as_str().unwrap().to_string();
    let entry = &rolled["store"]["attic"][0];
    assert_eq!(entry["id"], id.as_str(), "{rolled}");
    assert_eq!(entry["restorable"], true, "{rolled}");
    assert_eq!(entry["firstCommitSeq"], 2, "{rolled}");
    assert!(entry["bytes"].as_u64().unwrap() > 0, "{rolled}");
    let changes = ok_json(get(port, &format!("/api/store/attic/changes?id={id}")));
    assert_eq!(changes["commitCount"], 2, "{changes}");
    assert_eq!(changes["totals"]["addVertex"], 2, "{changes}");
    assert!(
        changes["commits"][0]["samples"][0]
            .as_str()
            .unwrap()
            .starts_with("add vertex"),
        "{changes}"
    );
    // Page by page: the second commit alone, the totals still over all.
    let page = ok_json(get(
        port,
        &format!("/api/store/attic/changes?id={id}&offset=1&limit=1"),
    ));
    assert_eq!(page["commits"].as_array().unwrap().len(), 1, "{page}");
    assert_eq!(page["commits"][0]["commitSeq"], 3, "{page}");
    assert_eq!(page["totals"]["addVertex"], 2, "{page}");
    assert_eq!(page["hasMore"], false, "{page}");
    assert_eq!(changes["limit"], 100, "{changes}");
    assert_eq!(
        get(port, &format!("/api/store/attic/changes?id={id}&offset=x")).status,
        400
    );
    assert_eq!(get(port, "/api/store/attic/changes").status, 400);
    assert_eq!(get(port, "/api/store/attic/changes?id=nope").status, 422);
    let verified = ok_json(post(port, "/api/store/verify", "{}"));
    assert_eq!(verified["verify"]["ok"], true, "{verified}");

    // Restore: back at commit 3 with everything.
    let restored = ok_json(post(
        port,
        "/api/store/attic/restore",
        &format!(r#"{{"id": "{id}"}}"#),
    ));
    assert_eq!(restored["restored"]["commitSeq"], 3, "{restored}");
    assert_eq!(restored["graph"]["vertexCount"], 3, "{restored}");
    assert_eq!(names(port), serde_json::json!(["ann", "bob", "cy"]));
    assert_eq!(
        restored["store"]["attic"].as_array().unwrap().len(),
        0,
        "{restored}"
    );

    // After a new commit the entry can only be forked or removed.
    let rolled = ok_json(post(
        port,
        "/api/store/rollback",
        r#"{"target": {"commit": 1}}"#,
    ));
    let id = rolled["rollback"]["attic"].as_str().unwrap().to_string();
    add("eve");
    let store = ok_json(get(port, "/api/store"))["store"].clone();
    assert_eq!(store["attic"][0]["restorable"], false, "{store}");
    assert!(
        store["attic"][0]["restoreBlocked"]
            .as_str()
            .unwrap()
            .contains("new commits"),
        "{store}"
    );
    let refused = post(
        port,
        "/api/store/attic/restore",
        &format!(r#"{{"id": "{id}"}}"#),
    );
    assert_eq!(refused.status, 422, "{}", refused.body);
    let forked = ok_json(post(
        port,
        "/api/store/attic/fork",
        &format!(r#"{{"id": "{id}"}}"#),
    ));
    let attic_fork = forked["fork"]["dir"].as_str().unwrap().to_string();
    let count = Command::new(env!("CARGO_BIN_EXE_graphersal"))
        .args(["--graph", &attic_fork, "-e", "g.v().count().next()"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&count.stdout).trim(), "3");
    ok_json(post(
        port,
        "/api/store/attic/remove",
        &format!(r#"{{"id": "{id}"}}"#),
    ));
    let store = ok_json(get(port, "/api/store"))["store"].clone();
    assert_eq!(store["attic"].as_array().unwrap().len(), 0, "{store}");

    // Delete mode: no attic entry; a target at or after the position is refused.
    let rolled = ok_json(post(
        port,
        "/api/store/rollback",
        r#"{"target": {"commit": 0}, "delete": true}"#,
    ));
    assert_eq!(rolled["rollback"]["attic"], Value::Null, "{rolled}");
    assert_eq!(names(port), serde_json::json!([]));
    let nothing = post(port, "/api/store/rollback", r#"{"target": {"commit": 5}}"#);
    assert_eq!(nothing.status, 422, "{}", nothing.body);
    assert_eq!(get(port, "/api/store/rollback").status, 405);
    drop(server);

    // Without a store: 409.
    let server = start(&["--graph", "modern", "--ui-dir", &ui_arg]);
    assert_eq!(
        post(
            server.port,
            "/api/store/view",
            r#"{"target": {"commit": 0}}"#
        )
        .status,
        409
    );
    drop(server);
    for entry in std::fs::read_dir(&base).unwrap().flatten() {
        if entry.file_name().to_string_lossy().starts_with("data") {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Compact never makes a mark unreachable without `dropMarks`: 409 with the list, nothing
/// changed; confirmed, it runs and names them.
#[test]
fn compact_answers_409_with_the_marks_it_would_make_unreachable() {
    let ui = ui_dir("store-compact-marks");
    let base = ui.parent().unwrap().to_path_buf();
    let data = base.join("data");
    for entry in std::fs::read_dir(&base).unwrap().flatten() {
        if entry.file_name().to_string_lossy().starts_with("data") {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
    let data_arg = data.to_str().unwrap().to_string();
    let ui_arg = ui.to_str().unwrap().to_string();
    let server = start(&["--graph", &data_arg, "--ui-dir", &ui_arg, "--create-store"]);
    let port = server.port;
    for round in 0..4 {
        let r = execute(
            port,
            &format!(r#"g.add_v("person").property("name", "p{round}").to_list()"#),
        );
        assert_eq!(r["ok"], true, "{r}");
        if round < 2 {
            let body = format!(r#"{{"name": "m{round}"}}"#);
            ok_json(post(port, "/api/store/mark", &body));
        }
        ok_json(post(port, "/api/store/checkpoint", "{}"));
    }
    let store = ok_json(get(port, "/api/store"))["store"].clone();
    assert_eq!(
        store["compaction"]["pruneMarksUnreachable"],
        serde_json::json!(["m0", "m1"]),
        "{store}"
    );
    for body in ["{}", "", r#"{"dropMarks": false}"#] {
        let refused = post(port, "/api/store/compact", body);
        assert_eq!(refused.status, 409, "{}", refused.body);
        let answer = refused.json();
        assert_eq!(
            answer["marksUnreachable"],
            serde_json::json!(["m0", "m1"]),
            "{answer}"
        );
        let error = answer["error"].as_str().unwrap();
        assert!(error.contains("\"m0\", \"m1\""), "{error}");
        assert!(error.contains("dropMarks"), "{error}");
    }
    let after = ok_json(get(port, "/api/store"))["store"].clone();
    assert_eq!(after["marks"].as_array().unwrap().len(), 2, "{after}");
    assert_eq!(after["snapshots"], store["snapshots"], "nothing removed");
    let done = ok_json(post(port, "/api/store/compact", r#"{"dropMarks": true}"#));
    assert_eq!(
        done["compact"]["marksUnreachable"],
        serde_json::json!(["m0", "m1"]),
        "{done}"
    );
    let after = ok_json(get(port, "/api/store"))["store"].clone();
    assert!(after["marks"].as_array().unwrap().is_empty(), "{after}");
    // No mark affected any more: no confirmation needed.
    ok_json(post(port, "/api/store/compact", "{}"));
    drop(server);
    for entry in std::fs::read_dir(&base).unwrap().flatten() {
        if entry.file_name().to_string_lossy().starts_with("data") {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

#[test]
fn a_single_file_store_is_served_with_the_compaction_advice_and_compact() {
    let ui = ui_dir("store-file");
    let base = ui.parent().unwrap().to_path_buf();
    for entry in std::fs::read_dir(&base).unwrap().flatten() {
        if entry.file_name().to_string_lossy().starts_with("data") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    let data = base.join("data.gstore");
    let data_arg = data.to_str().unwrap().to_string();
    let ui_arg = ui.to_str().unwrap().to_string();
    let server = start(&["--graph", &data_arg, "--ui-dir", &ui_arg, "--create-store"]);
    let port = server.port;
    assert!(data.is_file(), "one file");
    for round in 0..3 {
        let r = execute(
            port,
            &format!(
                r#"g.v().drop().to_list(); g.add_v("person").property("name", "p{round}").to_list()"#
            ),
        );
        assert_eq!(r["ok"], true, "{r}");
        ok_json(post(port, "/api/store/checkpoint", "{}"));
    }
    let store = ok_json(get(port, "/api/store"))["store"].clone();
    assert_eq!(store["kind"], "file", "{store}");
    assert!(
        store["space"]["totalBytes"].as_u64().unwrap() > 0,
        "{store}"
    );
    let advice = &store["compaction"];
    assert_eq!(advice["compactable"], true, "{advice}");
    assert_eq!(advice["recommended"], false, "small: {advice}");
    assert_eq!(advice["decidedBy"], "total_bytes", "{advice}");
    assert_eq!(advice["pruneFirst"], true, "{advice}");
    let polled = ok_json(get(port, "/api/store/compaction"));
    assert_eq!(polled["compaction"]["totalBytes"], advice["totalBytes"]);
    // Compact (the menu enables it only when recommended; the endpoint always runs it).
    let size = std::fs::metadata(&data).unwrap().len();
    let compacted = ok_json(post(port, "/api/store/compact", "{}"));
    assert_eq!(compacted["compact"]["compacted"], true, "{compacted}");
    assert!(
        !compacted["compact"]["snapshotsRemoved"]
            .as_array()
            .unwrap()
            .is_empty(),
        "{compacted}"
    );
    assert!(std::fs::metadata(&data).unwrap().len() < size);
    assert_eq!(compacted["compaction"]["pruneFirst"], false, "{compacted}");
    assert_eq!(names(port), serde_json::json!(["p2"]));
    let r = execute(
        port,
        r#"g.add_v("person").property("name", "after").to_list()"#,
    );
    assert_eq!(r["ok"], true, "{r}");
    // A fork of a single-file store is a single file next to it.
    ok_json(post(port, "/api/store/mark", r#"{"name": "here"}"#));
    let fork = ok_json(post(
        port,
        "/api/store/fork",
        r#"{"target": {"mark": "here"}}"#,
    ));
    let fork_path = fork["fork"]["dir"].as_str().unwrap().to_string();
    assert!(
        fork_path.contains("data-fork-") && fork_path.ends_with(".gstore"),
        "{fork}"
    );
    assert!(std::path::Path::new(&fork_path).is_file());
    assert_eq!(get(port, "/api/store/compact").status, 405);
    drop(server);
    for entry in std::fs::read_dir(&base).unwrap().flatten() {
        if entry.file_name().to_string_lossy().starts_with("data") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

// ------------------------------------------------------------------ MCP (`--server --mcp`)

const MCP_ACCEPT: &str = "Accept: application/json, text/event-stream";

/// One MCP client: `initialize`d, its session id sent on every later request.
struct McpClient {
    port: u16,
    session: String,
    extra: Vec<String>,
    next_id: u64,
}

impl McpClient {
    /// `initialize` (protocol 2025-06-18) and `notifications/initialized`; `extra` are header
    /// lines sent on every request (a token).
    fn connect(port: u16, extra: &[&str]) -> Self {
        let extra: Vec<String> = extra.iter().map(|h| h.to_string()).collect();
        let answer = mcp_post(
            port,
            &extra,
            r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test-agent","version":"9"}}}"#,
        );
        assert_eq!(answer.status, 200, "{}", answer.body);
        let init = answer.json();
        assert_eq!(init["result"]["protocolVersion"], "2025-06-18", "{init}");
        assert_eq!(init["result"]["serverInfo"]["name"], "graphersal-dev");
        assert!(init["result"]["capabilities"]["tools"].is_object());
        assert!(init["result"]["capabilities"]["resources"].is_object());
        let session = answer
            .headers
            .lines()
            .find_map(|l| {
                let (k, v) = l.split_once(':')?;
                k.eq_ignore_ascii_case("mcp-session-id")
                    .then(|| v.trim().to_string())
            })
            .expect("Mcp-Session-Id");
        let client = Self {
            port,
            session,
            extra,
            next_id: 1,
        };
        let note = client.send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
        assert_eq!(note.status, 202, "{}", note.body);
        assert!(note.body.is_empty());
        client
    }

    fn headers(&self) -> Vec<String> {
        let mut headers = self.extra.clone();
        headers.push(format!("Mcp-Session-Id: {}", self.session));
        headers.push("MCP-Protocol-Version: 2025-06-18".to_string());
        headers
    }

    fn send(&self, body: &str) -> Answer {
        mcp_post(self.port, &self.headers(), body)
    }

    /// A request; the JSON-RPC answer (status 200).
    fn rpc(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let body = serde_json::json!({
            "jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params,
        });
        let answer = self.send(&body.to_string());
        assert_eq!(answer.status, 200, "{method}: {}", answer.body);
        let value = answer.json();
        assert_eq!(value["id"], self.next_id, "{value}");
        value
    }

    /// `tools/call`: `(is_error, text)`.
    fn call(&mut self, tool: &str, arguments: Value) -> (bool, String) {
        let answer = self.rpc(
            "tools/call",
            serde_json::json!({"name": tool, "arguments": arguments}),
        );
        let result = &answer["result"];
        assert!(result.is_object(), "{tool}: {answer}");
        (
            result["isError"].as_bool().unwrap(),
            result["content"][0]["text"].as_str().unwrap().to_string(),
        )
    }

    fn tool_names(&mut self) -> Vec<String> {
        self.rpc("tools/list", serde_json::json!({}))["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect()
    }
}

fn mcp_post(port: u16, extra: &[String], body: &str) -> Answer {
    let mut headers: Vec<&str> = vec![JSON, MCP_ACCEPT];
    headers.extend(extra.iter().map(String::as_str));
    request(port, "POST", "/mcp", Some(body), &headers)
}

fn changes(port: u16) -> Value {
    get(port, "/api/changes").json()
}

#[test]
fn mcp_lifecycle_tools_resources_and_the_change_feed() {
    let ui = ui_dir("mcp");
    std::fs::write(
        ui.join("js/examples.json"),
        r#"[{"group": "Modern graph", "graph": "modern", "title": "Count", "script": "g.v().count()"}]"#,
    )
    .unwrap();
    let server = start(&[
        "--graph",
        "modern",
        "--ui-dir",
        ui.to_str().unwrap(),
        "--mcp",
    ]);
    let port = server.port;
    let info = get(port, "/api/info").json();
    assert_eq!(info["mcp"]["readOnly"], false, "{info}");
    assert_eq!(info["mcp"]["client"], Value::Null, "{info}");
    assert_eq!(changes(port)["version"], 0);

    let mut mcp = McpClient::connect(port, &[]);
    assert_eq!(
        get(port, "/api/info").json()["mcp"]["client"]["name"],
        "test-agent"
    );
    assert_eq!(
        mcp.rpc("ping", serde_json::json!({}))["result"],
        serde_json::json!({})
    );

    let names = mcp.tool_names();
    for name in [
        "query",
        "get_schema",
        "infer_schema",
        "validate_schema",
        "validate_schema_patch",
        "diff_schema",
        "set_schema",
        "patch_schema",
        "statistics",
        "graph_info",
    ] {
        assert!(names.iter().any(|n| n == name), "{name}: {names:?}");
    }
    // The graph only: no persistence tools.
    assert!(!names.iter().any(|n| n == "rollback"), "{names:?}");
    let tools = mcp.rpc("tools/list", serde_json::json!({}));
    let set_schema = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "set_schema")
        .unwrap()
        .clone();
    assert_eq!(set_schema["annotations"]["destructiveHint"], true);
    assert!(set_schema["inputSchema"]["required"][0] == "schema");

    // query: reads, a profile, a write recorded in the change feed as "mcp", an error.
    let (error, text) = mcp.call(
        "query",
        serde_json::json!({"script": r#"g.v().has_label("person").values("name")"#, "profile": true}),
    );
    assert!(!error, "{text}");
    assert!(text.starts_with("ok: 4 results"), "{text}");
    assert!(text.contains("\"marko\""), "{text}");
    assert!(text.contains("Traversal metrics"), "{text}");
    let (error, text) = mcp.call(
        "query",
        serde_json::json!({"script": r#"g.v().values("name")"#, "max_rows": 2}),
    );
    assert!(!error && text.contains("note:"), "{text}");
    let (error, text) = mcp.call(
        "query",
        serde_json::json!({"script": r#"g.add_v("person").property("name", "ada")"#}),
    );
    assert!(!error, "{text}");
    assert!(text.contains("committed as commit 1"), "{text}");
    let feed = changes(port);
    assert_eq!(feed["version"], 1, "{feed}");
    assert_eq!(feed["commitSeq"], 1, "{feed}");
    assert_eq!(feed["recent"][0]["by"], "mcp", "{feed}");
    let (error, text) = mcp.call(
        "query",
        serde_json::json!({"script": r#"g.add_v("x").next(); g.v().bogus()"#}),
    );
    assert!(error, "{text}");
    assert!(
        text.contains("rolled back") && text.contains("bogus"),
        "{text}"
    );
    assert_eq!(get(port, "/api/graph").json()["vertexCount"], 7);
    let (error, text) = mcp.call("query", serde_json::json!({}));
    assert!(error && text.contains("script"), "{text}");

    // A browser tab's change is recorded with its client id.
    let r = request(
        port,
        "POST",
        "/api/execute",
        Some(r#"{"script": "g.add_v(\"tab\").next()"}"#),
        &[JSON, "X-Graphersal-Client: tab-1"],
    );
    assert_eq!(r.status, 200, "{}", r.body);
    let feed = changes(port);
    assert_eq!(feed["version"], 2, "{feed}");
    assert_eq!(feed["recent"][1]["by"], "tab-1", "{feed}");

    // The schema tools.
    let (error, text) = mcp.call("get_schema", serde_json::json!({}));
    assert!(!error && text.starts_with("null"), "{text}");
    let (error, inferred) = mcp.call("infer_schema", serde_json::json!({}));
    assert!(!error, "{inferred}");
    let inferred: Value = serde_json::from_str(&inferred).unwrap();
    let (error, text) = mcp.call(
        "validate_schema",
        serde_json::json!({"schema": inferred, "mode": "closed"}),
    );
    assert!(!error && text.contains("violations"), "{text}");
    let (error, text) = mcp.call("diff_schema", serde_json::json!({"schema": inferred}));
    assert!(!error && text.contains("compatible"), "{text}");
    let (error, text) = mcp.call(
        "set_schema",
        serde_json::json!({"schema": inferred.to_string(), "mode": "open"}),
    );
    assert!(
        !error && text.contains("\"schemaMode\": \"open\""),
        "{text}"
    );
    let patch = serde_json::json!({"vertices": {"person": {"schema": {"properties": {"email": {"type": "string"}}}}}});
    let (error, text) = mcp.call("validate_schema_patch", serde_json::json!({"patch": patch}));
    assert!(!error && text.contains("violations"), "{text}");
    let (error, text) = mcp.call("patch_schema", serde_json::json!({"patch": patch}));
    assert!(!error && text.contains("email"), "{text}");
    let (error, text) = mcp.call(
        "patch_schema",
        serde_json::json!({"patch": {"vertices": {"person": {"schema": 7}}}}),
    );
    assert!(error && text.contains("Help:"), "{text}");
    let (error, text) = mcp.call("set_schema", serde_json::json!({}));
    assert!(error && text.contains("schema"), "{text}");

    let (error, text) = mcp.call("statistics", serde_json::json!({}));
    assert!(!error && text.contains("limits"), "{text}");
    let (error, text) = mcp.call("graph_info", serde_json::json!({}));
    assert!(!error && text.contains("\"mcpReadOnly\": false"), "{text}");

    // Unknown tools and methods are JSON-RPC errors.
    let unknown = mcp.rpc(
        "tools/call",
        serde_json::json!({"name": "rollback", "arguments": {}}),
    );
    assert_eq!(unknown["error"]["code"], -32602, "{unknown}");
    assert_eq!(
        unknown["error"]["message"], "unknown tool \"rollback\"",
        "{unknown}"
    );
    assert_eq!(mcp.rpc("nope/nope", Value::Null)["error"]["code"], -32601);

    // Resources.
    let list = mcp.rpc("resources/list", serde_json::json!({}));
    let uris: Vec<&str> = list["result"]["resources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["uri"].as_str().unwrap())
        .collect();
    assert_eq!(
        uris,
        [
            "graphersal://schema",
            "graphersal://statistics",
            "graphersal://dsl-reference",
            "graphersal://examples",
            "graphersal://saved-queries"
        ]
    );
    let read = |mcp: &mut McpClient, uri: &str| -> String {
        let answer = mcp.rpc("resources/read", serde_json::json!({"uri": uri}));
        answer["result"]["contents"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("{uri}: {answer}"))
            .to_string()
    };
    assert!(read(&mut mcp, "graphersal://schema").contains("email"));
    assert!(read(&mut mcp, "graphersal://statistics").contains("statistics"));
    let reference = read(&mut mcp, "graphersal://dsl-reference");
    assert!(reference.contains("has_label") && reference.contains("hasLabel"));
    // Every line carries the function's doc text and example from the engine's doc table.
    assert!(
        reference.contains("[hasLabel]: Keeps elements whose label")
            && reference.contains("Example: `g.v().has_label(\"software\")"),
        "{reference}"
    );
    assert!(
        reference
            .lines()
            .filter(|l| l.starts_with("- "))
            .all(|l| l.contains(" Example: `") || l.contains(": as ")),
        "{reference}"
    );
    assert!(reference.len() < 64 * 1024, "{}", reference.len());
    assert!(read(&mut mcp, "graphersal://examples").contains("g.v().count()"));
    let missing = mcp.rpc(
        "resources/read",
        serde_json::json!({"uri": "graphersal://x"}),
    );
    assert_eq!(missing["error"]["code"], -32002, "{missing}");

    // Transport rules.
    let stale = mcp_post(
        port,
        &["Mcp-Session-Id: 0000".to_string()],
        r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#,
    );
    assert_eq!(stale.status, 404, "{}", stale.body);
    let modern = mcp_post(
        port,
        &["MCP-Protocol-Version: 2099-01-01".to_string()],
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
    );
    assert_eq!(modern.status, 400, "{}", modern.body);
    assert_eq!(modern.json()["error"]["code"], -32600);
    let batch = mcp_post(port, &[], r#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#);
    assert_eq!(batch.status, 400, "{}", batch.body);
    assert_eq!(
        mcp_post(port, &[], "{not json").json()["error"]["code"],
        -32700
    );
    let no_session = mcp_post(port, &[], r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#);
    assert_eq!(no_session.status, 200, "{}", no_session.body);
    assert_eq!(
        request(port, "GET", "/mcp", None, &[MCP_ACCEPT]).status,
        405
    );
    let foreign = request(
        port,
        "POST",
        "/mcp",
        Some(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#),
        &[JSON, MCP_ACCEPT, "Origin: http://evil.example"],
    );
    assert_eq!(foreign.status, 403, "{}", foreign.body);
    let delete = request(
        port,
        "DELETE",
        "/mcp",
        None,
        &mcp.headers().iter().map(String::as_str).collect::<Vec<_>>(),
    );
    assert_eq!(delete.status, 200, "{}", delete.body);
    assert_eq!(
        mcp.send(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#)
            .status,
        404
    );
}

#[test]
fn mcp_read_only_token_and_off() {
    let ui = ui_dir("mcp-ro");
    let ui_arg = ui.to_str().unwrap().to_string();
    let server = start(&[
        "--graph",
        "modern",
        "--ui-dir",
        &ui_arg,
        "--mcp",
        "--mcp-read-only",
        "--mcp-token",
        "s3cret",
    ]);
    let port = server.port;
    let ping = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
    let refused = mcp_post(port, &[], ping);
    assert_eq!(refused.status, 401, "{}", refused.body);
    assert!(
        refused.headers.contains("WWW-Authenticate"),
        "{}",
        refused.headers
    );
    let wrong = mcp_post(port, &["Authorization: Bearer nope".to_string()], ping);
    assert_eq!(wrong.status, 401);
    let mut mcp = McpClient::connect(port, &["Authorization: Bearer s3cret"]);
    assert_eq!(get(port, "/api/info").json()["mcp"]["readOnly"], true);

    let names = mcp.tool_names();
    assert!(names.iter().any(|n| n == "query"), "{names:?}");
    assert!(names.iter().any(|n| n == "validate_schema"), "{names:?}");
    assert!(names.iter().any(|n| n == "list_compressions"), "{names:?}");
    for writing in [
        "set_schema",
        "patch_schema",
        "define_compression",
        "drop_compression",
        "recompress",
    ] {
        assert!(!names.iter().any(|n| n == writing), "{names:?}");
        let refused = mcp.rpc(
            "tools/call",
            serde_json::json!({"name": writing, "arguments": {"schema": {}, "patch": {}}}),
        );
        assert_eq!(refused["error"]["code"], -32602, "{refused}");
        assert!(
            refused["error"]["message"]
                .as_str()
                .unwrap()
                .contains("read-only"),
            "{refused}"
        );
    }
    let (error, text) = mcp.call("query", serde_json::json!({"script": "g.v().count()"}));
    assert!(!error && text.contains('6'), "{text}");
    for script in [
        r#"g.add_v("person").property("name", "eve")"#,
        r#"g.v().drop()"#,
        r#"g.v().property("x", 1)"#,
        r#"g.set_schema(g.infer_schema())"#,
        r#"g.define_compression(#{name: "n", label: "person", path: "name"})"#,
        r#"g.define_query(#{name: "n", body: "fn n() { 1 }"})"#,
    ] {
        let (error, text) = mcp.call("query", serde_json::json!({"script": script}));
        assert!(error, "{script}: {text}");
        assert!(text.to_lowercase().contains("denied"), "{script}: {text}");
    }
    assert_eq!(get(port, "/api/graph").json()["vertexCount"], 6);
    assert_eq!(changes(port)["version"], 0);
    // The browser is not read-only.
    assert_eq!(execute(port, r#"g.add_v("ok").next()"#)["mutated"], true);
    drop(server);

    // Without --mcp the endpoint does not exist.
    let server = start(&["--graph", "modern", "--ui-dir", &ui_arg]);
    let off = mcp_post(server.port, &[], ping);
    assert_eq!(off.status, 404, "{}", off.body);
    assert_eq!(get(server.port, "/api/info").json()["mcp"], Value::Null);
}

/// A saved query's spec as the playground's editor sends it.
const OLDER_THAN: &str = r#"{"name": "older_than", "folder": "people", "description": "People older than an age", "params": [{"name": "age", "schema": {"type": "integer", "minimum": 0, "description": "years"}, "default": 30}], "body": "fn older_than(age) { g.v().has_label(\"person\").has(\"age\", P.gt(age)).values(\"name\") }"}"#;

#[test]
fn saved_queries_over_http() {
    let ui = ui_dir("queries");
    let server = start(&["--graph", "modern", "--ui-dir", ui.to_str().unwrap()]);
    let port = server.port;
    assert_eq!(get(port, "/api/queries").json(), serde_json::json!([]));
    let defined = post(port, "/api/queries/define", OLDER_THAN).json();
    assert_eq!(defined["ok"], true, "{defined}");
    assert_eq!(defined["query"]["params"][0]["default"], 30);
    // A catalog change is a commit: the change feed records it.
    assert_eq!(changes(port)["commitSeq"], 1);
    let refused = post(
        port,
        "/api/queries/define",
        r#"{"name": "x y", "body": "fn x() {}"}"#,
    );
    assert_eq!(refused.status, 200);
    assert_eq!(refused.json()["errors"][0]["field"], "name");

    assert_eq!(
        get(port, "/api/queries?folder=people").json()[0]["name"],
        "older_than"
    );
    assert_eq!(
        get(port, "/api/queries?folder=other").json(),
        serde_json::json!([])
    );
    assert_eq!(
        get(port, "/api/queries/get?name=older_than").json()["status"],
        "ok"
    );
    assert_eq!(get(port, "/api/queries/get?name=none").json(), Value::Null);
    let text = post(
        port,
        "/api/queries/call-text",
        r#"{"name": "older_than", "args": {"age": 29}}"#,
    )
    .json();
    assert_eq!(text["text"], "g.query(\"older_than\", #{age: 29})");
    let run = execute(port, text["text"].as_str().unwrap());
    assert_eq!(
        run["execution"]["results"],
        serde_json::json!(["josh", "peter"])
    );
    let moved = post(
        port,
        "/api/queries/move",
        r#"{"name": "older_than", "folder": "a/b"}"#,
    )
    .json();
    assert_eq!(moved["folder"], "a/b");
    assert_eq!(
        get(port, "/api/queries?folder=a%2Fb").json()[0]["name"],
        "older_than"
    );
    let file = get(port, "/api/catalog/export").body;
    assert!(file.contains("\"definitions\""), "{file}");
    assert_eq!(
        post(port, "/api/queries/drop", r#"{"name": "older_than"}"#).json()["dropped"],
        true
    );
    assert_eq!(get(port, "/api/graph").json()["savedQueries"], 0);
    let loaded = post(port, "/api/catalog/load?replace=true", &file).json();
    assert_eq!(loaded["loaded"], 1, "{loaded}");
    assert_eq!(loaded["graph"]["savedQueries"], 1);
    let bad = post(port, "/api/catalog/load", r#"{"definitions": 1}"#);
    assert_eq!(bad.status, 422, "{}", bad.body);
    assert_eq!(post(port, "/api/queries/drop", "{}").status, 400);
}

#[test]
fn compression_rules_over_http() {
    let ui = ui_dir("compression");
    let server = start(&["--graph", "modern", "--ui-dir", ui.to_str().unwrap()]);
    let port = server.port;
    assert_eq!(get(port, "/api/compression").json(), serde_json::json!([]));
    let defined = post(
        port,
        "/api/compression/define",
        r#"{"name": "names", "label": "person", "path": "name", "minBytes": 1}"#,
    )
    .json();
    assert_eq!(defined["ok"], true, "{defined}");
    assert_eq!(defined["rule"]["path"], "$.name");
    // A rule change is a commit: the change feed records it.
    assert_eq!(changes(port)["commitSeq"], 1);
    let refused = post(
        port,
        "/api/compression/define",
        r#"{"name": "x", "label": "person", "path": "a[0]"}"#,
    );
    assert_eq!(refused.status, 200);
    assert_eq!(refused.json()["errors"][0]["field"], "path");
    let listed = get(port, "/api/compression").json();
    assert_eq!(listed[0]["name"], "names");
    assert!(listed[0]["stats"]["compressedValues"].is_u64());
    let recompressed = post(port, "/api/compression/recompress", r#"{"name": "names"}"#);
    assert_eq!(recompressed.status, 200, "{}", recompressed.body);
    // Reads stay plain.
    let run = execute(
        port,
        r#"g.v().has_label("person").values("name").to_list()"#,
    );
    assert_eq!(
        run["execution"]["results"],
        serde_json::json!(["marko", "vadas", "josh", "peter"])
    );
    assert_eq!(get(port, "/api/graph").json()["compressionRules"], 1);
    let missing = post(port, "/api/compression/recompress", r#"{"name": "nope"}"#);
    assert_eq!(missing.status, 422, "{}", missing.body);
    // The catalog file carries the rule; loading it brings it back.
    let file = get(port, "/api/catalog/export").body;
    assert!(file.contains("\"compression\""), "{file}");
    assert_eq!(
        post(port, "/api/compression/drop", r#"{"name": "names"}"#).json()["dropped"],
        true
    );
    assert_eq!(get(port, "/api/graph").json()["compressionRules"], 0);
    let loaded = post(port, "/api/catalog/load?replace=true", &file).json();
    assert_eq!(loaded["graph"]["compressionRules"], 1, "{loaded}");
    assert_eq!(get(port, "/api/compression/define").status, 405);
}

#[test]
fn property_indexes_over_http() {
    let ui = ui_dir("indexes");
    let server = start(&["--graph", "modern", "--ui-dir", ui.to_str().unwrap()]);
    let port = server.port;
    assert_eq!(get(port, "/api/indexes").json(), serde_json::json!([]));
    let created = post(
        port,
        "/api/indexes/create",
        r#"{"label": "person", "key": "age"}"#,
    )
    .json();
    assert_eq!(created["ok"], true, "{created}");
    assert_eq!(created["index"]["name"], "person_age");
    assert_eq!(created["index"]["stats"]["entries"], 4);
    // Creating an index is a commit: the change feed records it.
    assert_eq!(changes(port)["commitSeq"], 1);
    let refused = post(
        port,
        "/api/indexes/create",
        r#"{"label": "person", "key": "age", "name": "ages"}"#,
    );
    assert_eq!(refused.status, 200);
    assert_eq!(refused.json()["errors"][0]["field"], "key");
    let listed = get(port, "/api/indexes").json();
    assert_eq!(listed[0]["served"], true, "{listed}");
    assert_eq!(get(port, "/api/graph").json()["propertyIndexes"], 1);
    let memory = &get(port, "/api/statistics").json()["memory"];
    assert!(
        memory["property_index_bytes"].as_u64().unwrap() > 0,
        "{memory}"
    );
    let run = execute(
        port,
        r#"g.v().has_label("person").has("age", P.gt(30)).values("name").order().to_list()"#,
    );
    assert_eq!(
        run["execution"]["results"],
        serde_json::json!(["josh", "peter"])
    );
    // The catalog file carries the index; loading it brings it back.
    let file = get(port, "/api/catalog/export").body;
    assert!(file.contains("\"property_index\""), "{file}");
    assert_eq!(
        post(port, "/api/indexes/drop", r#"{"name": "person_age"}"#).json()["dropped"],
        true
    );
    assert_eq!(changes(port)["commitSeq"], 2);
    assert_eq!(get(port, "/api/graph").json()["propertyIndexes"], 0);
    let loaded = post(port, "/api/catalog/load?replace=true", &file).json();
    assert_eq!(loaded["graph"]["propertyIndexes"], 1, "{loaded}");
    assert_eq!(get(port, "/api/indexes/create").status, 405);
}

#[test]
fn mcp_property_index_tools_and_read_only() {
    let ui = ui_dir("mcp-indexes");
    let ui_arg = ui.to_str().unwrap().to_string();
    let server = start(&["--graph", "modern", "--ui-dir", &ui_arg, "--mcp"]);
    let port = server.port;
    let mut mcp = McpClient::connect(port, &[]);
    let names = mcp.tool_names();
    for name in ["list_indexes", "create_index", "drop_index"] {
        assert!(names.iter().any(|n| n == name), "{name}: {names:?}");
    }
    let (error, text) = mcp.call("list_indexes", serde_json::json!({}));
    assert!(!error && text.contains("no property indexes"), "{text}");
    let (error, text) = mcp.call(
        "create_index",
        serde_json::json!({"label": "person", "key": "age"}),
    );
    assert!(!error && text.contains("person_age"), "{text}");
    assert_eq!(changes(port)["commitSeq"], 1);
    let (error, text) = mcp.call("create_index", serde_json::json!({"label": "person"}));
    assert!(error && text.contains("\"key\""), "{text}");
    let (error, text) = mcp.call("list_indexes", serde_json::json!({}));
    assert!(!error && text.contains("\"entries\": 4"), "{text}");
    let (error, text) = mcp.call(
        "query",
        serde_json::json!({"script": r#"g.v().has_label("person").has("age", 29).values("name").profile()"#}),
    );
    assert!(
        !error && text.contains("[index: vertex") && text.contains("property_index"),
        "{text}"
    );
    let (error, text) = mcp.call("drop_index", serde_json::json!({"name": "person_age"}));
    assert!(!error && text.contains("dropped"), "{text}");
    let (error, text) = mcp.call("drop_index", serde_json::json!({"name": "person_age"}));
    assert!(!error && text.contains("no index"), "{text}");
    drop(server);

    // Read-only: list only; the writing tools are not offered and the query tool's authorizer
    // denies the DSL's index writes.
    let server = start(&[
        "--graph",
        "modern",
        "--ui-dir",
        &ui_arg,
        "--mcp",
        "--mcp-read-only",
    ]);
    let port = server.port;
    let mut mcp = McpClient::connect(port, &[]);
    let names = mcp.tool_names();
    assert!(names.iter().any(|n| n == "list_indexes"), "{names:?}");
    for writing in ["create_index", "drop_index"] {
        assert!(!names.iter().any(|n| n == writing), "{names:?}");
        let refused = mcp.rpc(
            "tools/call",
            serde_json::json!({"name": writing, "arguments": {"label": "person", "key": "age", "name": "x"}}),
        );
        assert_eq!(refused["error"]["code"], -32602, "{refused}");
    }
    for script in [
        r#"g.create_index(#{label: "person", key: "age"})"#,
        r#"g.drop_index("person_age")"#,
    ] {
        let (error, text) = mcp.call("query", serde_json::json!({"script": script}));
        assert!(error, "{script}: {text}");
        assert!(text.to_lowercase().contains("denied"), "{script}: {text}");
    }
    let (error, text) = mcp.call("query", serde_json::json!({"script": "g.indexes()"}));
    assert!(!error, "{text}");
    assert_eq!(changes(port)["version"], 0);
}

#[test]
fn property_index_types_over_http_and_mcp() {
    let ui = ui_dir("index-types");
    let ui_arg = ui.to_str().unwrap().to_string();
    let server = start(&["--graph", "modern", "--ui-dir", &ui_arg, "--mcp"]);
    let port = server.port;
    // HTTP: the type is listed, an unknown one is an error of the field `type`.
    let created = post(
        port,
        "/api/indexes/create",
        r#"{"label": "person", "key": "age", "type": "memory-ordered"}"#,
    )
    .json();
    assert_eq!(created["ok"], true, "{created}");
    assert_eq!(created["index"]["type"], "memory-ordered", "{created}");
    let listed = get(port, "/api/indexes").json();
    assert_eq!(listed[0]["type"], "memory-ordered", "{listed}");
    assert_eq!(listed[0]["reason"], Value::Null, "{listed}");
    let refused = post(
        port,
        "/api/indexes/create",
        r#"{"label": "person", "key": "name", "type": "hash"}"#,
    )
    .json();
    assert_eq!(refused["ok"], false, "{refused}");
    assert_eq!(refused["errors"][0]["field"], "type", "{refused}");
    // A key the schema declares as a vector: an error of the field `key`.
    let set = execute(
        port,
        r#"g.set_schema(#{mode: "open", vertices: #{doc: #{schema: #{type: "object",
             properties: #{emb: #{type: "array", format: "vector", element: "f32"}}}}}})"#,
    );
    assert_eq!(set["ok"], true, "{set}");
    let refused = post(
        port,
        "/api/indexes/create",
        r#"{"label": "doc", "key": "emb"}"#,
    )
    .json();
    assert_eq!(refused["errors"][0]["field"], "key", "{refused}");
    assert!(
        refused["errors"][0]["help"]
            .as_str()
            .unwrap()
            .contains("nearest()"),
        "{refused}"
    );

    // MCP: initialize -> tools/list (the type enum) -> tools/call.
    let mut mcp = McpClient::connect(port, &[]);
    let tools = mcp.rpc("tools/list", serde_json::json!({}));
    let create = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "create_index")
        .unwrap()
        .clone();
    assert_eq!(
        create["inputSchema"]["properties"]["type"]["enum"],
        serde_json::json!(["memory-ordered"]),
        "{create}"
    );
    let (error, text) = mcp.call(
        "create_index",
        serde_json::json!({"label": "software", "key": "lang", "type": "memory-ordered"}),
    );
    assert!(
        !error && text.contains("\"type\": \"memory-ordered\""),
        "{text}"
    );
    let (error, text) = mcp.call(
        "create_index",
        serde_json::json!({"label": "software", "key": "name", "type": "hash"}),
    );
    assert!(error && text.contains("\"type\""), "{text}");
    let (error, text) = mcp.call(
        "create_index",
        serde_json::json!({"label": "doc", "key": "emb"}),
    );
    assert!(error && text.contains("vector<f32>"), "{text}");
    let (error, text) = mcp.call("list_indexes", serde_json::json!({}));
    assert!(
        !error && text.matches("\"type\": \"memory-ordered\"").count() == 2,
        "{text}"
    );
}

#[test]
fn mcp_saved_query_tools_and_resource() {
    let ui = ui_dir("mcp-queries");
    let ui_arg = ui.to_str().unwrap().to_string();
    let server = start(&["--graph", "modern", "--ui-dir", &ui_arg, "--mcp"]);
    let port = server.port;
    let mut mcp = McpClient::connect(port, &[]);
    let names = mcp.tool_names();
    for name in ["list_saved_queries", "run_saved_query"] {
        assert!(names.iter().any(|n| n == name), "{name}: {names:?}");
    }
    // The instructions and the query tool point to the saved queries.
    let init = mcp_post(
        port,
        &[],
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"x","version":"1"}}}"#,
    )
    .json();
    assert!(
        init["result"]["instructions"]
            .as_str()
            .unwrap()
            .contains("list_saved_queries")
    );
    let (error, text) = mcp.call("list_saved_queries", serde_json::json!({}));
    assert!(!error && text.contains("no saved queries"), "{text}");

    assert_eq!(
        post(port, "/api/queries/define", OLDER_THAN).json()["ok"],
        true
    );
    let (error, text) = mcp.call("list_saved_queries", serde_json::json!({"search": "OLDER"}));
    assert!(!error, "{text}");
    assert!(
        text.contains("\"older_than\"") && text.contains("years"),
        "{text}"
    );
    let (_, text) = mcp.call(
        "list_saved_queries",
        serde_json::json!({"folder": "nothing"}),
    );
    assert!(text.contains("No saved query matches"), "{text}");
    let (error, text) = mcp.call(
        "run_saved_query",
        serde_json::json!({"name": "older_than", "params": {"age": 29}}),
    );
    assert!(!error, "{text}");
    assert!(text.contains("josh") && text.contains("peter"), "{text}");
    assert!(
        text.starts_with("call: g.query(\"older_than\", #{age: 29})"),
        "{text}"
    );
    // Defaults fill omitted parameters; a wrong argument is an error with its Help line.
    let (error, text) = mcp.call("run_saved_query", serde_json::json!({"name": "older_than"}));
    assert!(!error && text.contains("josh"), "{text}");
    let (error, text) = mcp.call(
        "run_saved_query",
        serde_json::json!({"name": "older_than", "params": {"age": "x"}}),
    );
    assert!(error && text.contains("Help:"), "{text}");
    let (error, text) = mcp.call("run_saved_query", serde_json::json!({"name": "nope"}));
    assert!(error && text.contains("nope"), "{text}");
    let resource = mcp.rpc(
        "resources/read",
        serde_json::json!({"uri": "graphersal://saved-queries"}),
    );
    assert!(
        resource["result"]["contents"][0]["text"]
            .as_str()
            .unwrap()
            .contains("older_than"),
        "{resource}"
    );
    let listed = mcp.rpc("resources/list", serde_json::json!({}));
    assert!(listed.to_string().contains("graphersal://saved-queries"));
    // Without --mcp-query-tools a saved query is no tool of its own.
    assert!(!mcp.tool_names().iter().any(|n| n == "older_than"));
    drop(server);

    // Read-only: both tools stay, a saved query runs.
    let server = start(&[
        "--graph",
        "modern",
        "--ui-dir",
        &ui_arg,
        "--mcp",
        "--mcp-read-only",
        "--mcp-query-tools",
    ]);
    let port = server.port;
    assert_eq!(
        post(port, "/api/queries/define", OLDER_THAN).json()["ok"],
        true
    );
    // A query named like a built-in tool is reached through run_saved_query only.
    let statistics = r#"{"name": "statistics", "body": "fn statistics() { g.v().count() }"}"#;
    assert_eq!(
        post(port, "/api/queries/define", statistics).json()["ok"],
        true
    );
    let mut mcp = McpClient::connect(port, &[]);
    let tools = mcp.rpc("tools/list", serde_json::json!({}));
    let listed = tools["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = listed.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"run_saved_query"), "{names:?}");
    assert!(names.contains(&"older_than"), "{names:?}");
    assert_eq!(names.iter().filter(|n| **n == "statistics").count(), 1);
    let own = listed.iter().find(|t| t["name"] == "older_than").unwrap();
    assert_eq!(own["inputSchema"]["properties"]["age"]["default"], 30);
    assert_eq!(own["inputSchema"]["properties"]["age"]["minimum"], 0);
    assert_eq!(own["annotations"]["readOnlyHint"], true);
    assert!(
        own["description"]
            .as_str()
            .unwrap()
            .starts_with("People older than an age. Folder: people."),
        "{own}"
    );
    let (error, text) = mcp.call("older_than", serde_json::json!({"age": 33}));
    assert!(
        !error && text.contains("peter") && !text.contains("josh"),
        "{text}"
    );
    let (error, text) = mcp.call("run_saved_query", serde_json::json!({"name": "statistics"}));
    assert!(!error && text.contains('6'), "{text}");
    // tools/list follows the catalog.
    post(port, "/api/queries/drop", r#"{"name": "older_than"}"#);
    assert!(!mcp.tool_names().iter().any(|n| n == "older_than"));
    let gone = mcp.rpc(
        "tools/call",
        serde_json::json!({"name": "older_than", "arguments": {}}),
    );
    assert_eq!(gone["error"]["code"], -32602, "{gone}");
}

/// A saved query that writes over HTTP and MCP: its preview, `writes` and `effects` in the
/// listing, tool annotations from its effects, and the read-only mode that neither lists nor
/// runs it.
#[test]
fn saved_queries_that_write_over_http_and_mcp() {
    let ui = ui_dir("mcp-writing-queries");
    let ui_arg = ui.to_str().unwrap().to_string();
    let define = r#"{"name": "add_ghost", "body": "fn add_ghost() { g.add_v(\"ghost\").property(\"n\", 1).next() }"}"#;
    let ghosts = |port: u16| {
        execute(port, "g.v().has_label(\"ghost\").count().next()")["execution"]["results"][0]
            .clone()
    };
    let server = start(&[
        "--graph",
        "modern",
        "--ui-dir",
        &ui_arg,
        "--mcp",
        "--mcp-query-tools",
    ]);
    let port = server.port;
    assert_eq!(post(port, "/api/queries/define", define).json()["ok"], true);
    let preview = post(port, "/api/queries/preview", r#"{"name": "add_ghost"}"#).json();
    assert_eq!(preview["changes"]["vertices_added"], 1, "{preview}");
    assert_eq!(ghosts(port), 0);
    let mut mcp = McpClient::connect(port, &[]);
    let init = mcp_post(
        port,
        &[],
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"x","version":"1"}}}"#,
    )
    .json();
    assert!(
        init["result"]["instructions"]
            .as_str()
            .unwrap()
            .contains("Saved queries marked `writes: true` change data"),
        "{init}"
    );
    let (error, text) = mcp.call("list_saved_queries", serde_json::json!({}));
    assert!(!error, "{text}");
    assert!(text.contains("\"writes\": true"), "{text}");
    assert!(text.contains(r#"Create Data(vertex \"ghost\")"#), "{text}");
    let tools = mcp.rpc("tools/list", serde_json::json!({}));
    let own = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "add_ghost")
        .unwrap()
        .clone();
    assert_eq!(own["annotations"]["readOnlyHint"], false, "{own}");
    assert_eq!(own["annotations"]["destructiveHint"], true, "{own}");
    assert!(
        own["description"]
            .as_str()
            .unwrap()
            .contains("CHANGES DATA"),
        "{own}"
    );
    let (error, text) = mcp.call("run_saved_query", serde_json::json!({"name": "add_ghost"}));
    assert!(!error, "{text}");
    assert_eq!(ghosts(port), 1);
    drop(server);

    let server = start(&[
        "--graph",
        "modern",
        "--ui-dir",
        &ui_arg,
        "--mcp",
        "--mcp-read-only",
        "--mcp-query-tools",
    ]);
    let port = server.port;
    assert_eq!(post(port, "/api/queries/define", define).json()["ok"], true);
    let mut mcp = McpClient::connect(port, &[]);
    let (_, text) = mcp.call("list_saved_queries", serde_json::json!({}));
    assert!(text.contains("no saved queries"), "{text}");
    assert!(!mcp.tool_names().iter().any(|n| n == "add_ghost"));
    let resource = mcp.rpc(
        "resources/read",
        serde_json::json!({"uri": "graphersal://saved-queries"}),
    );
    assert!(!resource.to_string().contains("add_ghost"), "{resource}");
    let (error, text) = mcp.call("run_saved_query", serde_json::json!({"name": "add_ghost"}));
    assert!(error, "{text}");
    assert!(
        text.contains("changes data, and this server is read-only for MCP"),
        "{text}"
    );
    assert_eq!(ghosts(port), 0);
}

/// Saved queries written in GQL: defined over HTTP (the playground's editor) and with a script
/// through the MCP `query` tool, listed with their language and dialect, run with
/// `run_saved_query` (and as a tool of their own); read-only mode hides the one that writes.
#[test]
fn saved_queries_written_in_gql_over_http_and_mcp() {
    let ui = ui_dir("mcp-gql-queries");
    let ui_arg = ui.to_str().unwrap().to_string();
    let older = r#"{"name": "older", "language": "gql", "description": "People older than an age", "params": [{"name": "min", "schema": {"type": "integer"}, "default": 30}], "body": "MATCH (p:person) WHERE p.age > $min RETURN p.name AS name ORDER BY name"}"#;
    let server = start(&[
        "--graph",
        "modern",
        "--ui-dir",
        &ui_arg,
        "--mcp",
        "--mcp-query-tools",
    ]);
    let port = server.port;
    let defined = post(port, "/api/queries/define", older).json();
    assert_eq!(defined["ok"], true, "{defined}");
    assert_eq!(defined["query"]["language"], "gql");
    assert_eq!(defined["query"]["dialect"], "graphersal-gql/1");
    let mut mcp = McpClient::connect(port, &[]);
    let tools = mcp.rpc("tools/list", serde_json::json!({}));
    let listed = tools["result"]["tools"].as_array().unwrap();
    for name in ["list_saved_queries", "run_saved_query"] {
        let tool = listed.iter().find(|t| t["name"] == name).unwrap();
        assert!(
            tool["description"].as_str().unwrap().contains("gql"),
            "{tool}"
        );
    }
    // A writing GQL query defined by a script through the query tool.
    let (error, text) = mcp.call(
        "query",
        serde_json::json!({"script": "g.define_query(#{name: \"add_person\", language: \"gql\", params: #{name: \"string\"}, body: \"INSERT (:person {name: $name})\"})"}),
    );
    assert!(!error, "{text}");
    let (error, text) = mcp.call("list_saved_queries", serde_json::json!({}));
    assert!(!error, "{text}");
    assert!(text.contains("\"language\": \"gql\""), "{text}");
    assert!(text.contains("\"dialect\": \"graphersal-gql/1\""), "{text}");
    assert!(text.contains("\"dialect\": \"graphersal-gql/2\""), "{text}");
    assert!(text.contains(r#"Create Data(vertex \"person\")"#), "{text}");
    let (error, text) = mcp.call(
        "run_saved_query",
        serde_json::json!({"name": "older", "params": {"min": 29}}),
    );
    assert!(!error, "{text}");
    assert!(
        text.contains("\"name\": \"josh\"") && text.contains("\"name\": \"peter\""),
        "{text}"
    );
    let (error, text) = mcp.call("older", serde_json::json!({"min": 33}));
    assert!(
        !error && text.contains("peter") && !text.contains("josh"),
        "{text}"
    );
    let (error, text) = mcp.call(
        "run_saved_query",
        serde_json::json!({"name": "add_person", "params": {"name": "ann"}}),
    );
    assert!(!error, "{text}");
    assert_eq!(
        execute(port, "g.v().has(\"name\", \"ann\").count().next()")["execution"]["results"][0],
        1
    );
    let (error, text) = mcp.call(
        "run_saved_query",
        serde_json::json!({"name": "older", "params": {"min": "x"}}),
    );
    assert!(error && text.contains("Help:"), "{text}");
    drop(server);

    // Read-only: the reading GQL query runs, the writing one is neither listed nor run.
    let server = start(&[
        "--graph",
        "modern",
        "--ui-dir",
        &ui_arg,
        "--mcp",
        "--mcp-read-only",
    ]);
    let port = server.port;
    assert_eq!(post(port, "/api/queries/define", older).json()["ok"], true);
    let add = r#"{"name": "add_person", "language": "gql", "params": [{"name": "name", "schema": {"type": "string"}}], "body": "INSERT (:person {name: $name})"}"#;
    assert_eq!(post(port, "/api/queries/define", add).json()["ok"], true);
    let mut mcp = McpClient::connect(port, &[]);
    let (_, text) = mcp.call("list_saved_queries", serde_json::json!({}));
    assert!(
        text.contains("\"older\"") && !text.contains("add_person"),
        "{text}"
    );
    let (error, text) = mcp.call("run_saved_query", serde_json::json!({"name": "older"}));
    assert!(!error && text.contains("josh"), "{text}");
    let (error, text) = mcp.call(
        "run_saved_query",
        serde_json::json!({"name": "add_person", "params": {"name": "ann"}}),
    );
    assert!(
        error && text.contains("changes data, and this server is read-only for MCP"),
        "{text}"
    );
}

/// MCP works with the graph only, also on a Store: exactly the graph tools, the persistence
/// tools are unknown; a read-only view the browser opens is server-wide: `graph_info` and a
/// refused write say so, the change feed carries it.
#[test]
fn mcp_on_a_store_offers_the_graph_tools_only() {
    let ui = ui_dir("mcp-store");
    let base = ui.parent().unwrap().to_path_buf();
    let data = base.join("data");
    let _ = std::fs::remove_dir_all(&data);
    let data_arg = data.to_str().unwrap().to_string();
    let ui_arg = ui.to_str().unwrap().to_string();
    let server = start(&[
        "--graph",
        &data_arg,
        "--ui-dir",
        &ui_arg,
        "--create-store",
        "--mcp",
    ]);
    let port = server.port;
    let mut mcp = McpClient::connect(port, &[]);
    assert_eq!(
        mcp.tool_names(),
        [
            "query",
            "gql",
            "list_saved_queries",
            "run_saved_query",
            "list_compressions",
            "estimate_compression",
            "define_compression",
            "drop_compression",
            "recompress",
            "list_indexes",
            "create_index",
            "drop_index",
            "get_schema",
            "infer_schema",
            "validate_schema",
            "validate_schema_patch",
            "diff_schema",
            "set_schema",
            "patch_schema",
            "statistics",
            "graph_info",
        ]
    );
    for name in [
        "store_info",
        "checkpoint",
        "mark",
        "open_view",
        "close_view",
        "rollback",
        "attic_list",
        "attic_restore",
        "backup",
        "compact",
    ] {
        let unknown = mcp.rpc(
            "tools/call",
            serde_json::json!({"name": name, "arguments": {}}),
        );
        assert_eq!(unknown["error"]["code"], -32602, "{name}: {unknown}");
        assert_eq!(
            unknown["error"]["message"],
            format!("unknown tool {name:?}"),
            "{unknown}"
        );
    }
    let (error, text) = mcp.call(
        "query",
        serde_json::json!({"script": r#"g.add_v("person").property("name", "ann")"#}),
    );
    assert!(!error, "{text}"); // commit 1
    let (error, text) = mcp.call("graph_info", serde_json::json!({}));
    assert!(!error && text.contains("\"view\": null"), "{text}");
    assert!(!text.contains("\"note\""), "{text}");

    // The browser opens a read-only view: the whole server serves it, MCP too.
    let opened = request(
        port,
        "POST",
        "/api/store/view",
        Some(r#"{"target": {"commit": 0}}"#),
        &[JSON, "X-Graphersal-Client: tab-1"],
    );
    assert_eq!(opened.status, 200, "{}", opened.body);
    let feed = changes(port);
    assert_eq!(feed["view"]["commitSeq"], 0, "{feed}");
    assert_eq!(
        feed["recent"].as_array().unwrap().last().unwrap()["by"],
        "tab-1"
    );
    let (error, text) = mcp.call("graph_info", serde_json::json!({}));
    assert!(!error, "{text}");
    assert!(
        text.contains("the served state is a read-only view at commit 0"),
        "{text}"
    );
    let (error, text) = mcp.call("query", serde_json::json!({"script": "g.v().count()"}));
    assert!(!error && text.contains('0'), "{text}");
    let (error, text) = mcp.call("query", serde_json::json!({"script": r#"g.add_v("x")"#}));
    assert!(error, "{text}");
    assert!(
        text.contains("note: the served state is a read-only view at commit 0"),
        "{text}"
    );
    let (error, text) = mcp.call(
        "set_schema",
        serde_json::json!({"schema": {"vertices": {"x": {"schema": {"type": "object"}}}}, "mode": "open"}),
    );
    assert!(
        error && text.contains("read-only view at commit 0"),
        "{text}"
    );

    // Back to live (browser): no note, writes work again.
    assert_eq!(post(port, "/api/store/live", "{}").status, 200);
    let (error, text) = mcp.call("graph_info", serde_json::json!({}));
    assert!(!error && !text.contains("\"note\""), "{text}");
    let (error, text) = mcp.call("query", serde_json::json!({"script": r#"g.add_v("x")"#}));
    assert!(!error, "{text}");
    drop(server);
    let _ = std::fs::remove_dir_all(&base);
}

// ------------------------------------------------------- robustness of the request bodies (T3)

/// SplitMix64, seeded: the fuzz cases are reproducible.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }
    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

fn fuzz_budget(default: usize) -> usize {
    std::env::var("ROBUSTNESS_INPUT_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// One raw request: the head as given (no defaults), then `body`; the write half is closed
/// after the body, so a server waiting for more body bytes sees the end. `None`: no answer
/// within the deadline (a hang).
fn raw_request(port: u16, head: &str, body: &[u8]) -> Option<Answer> {
    let mut stream = connect(port).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_secs(60)))
        .ok()?;
    // The server may answer (413) and close before it reads a large body: a failed write is
    // fine, the answer is what counts.
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.shutdown(std::net::Shutdown::Write);
    let mut raw = Vec::new();
    if let Err(err) = stream.read_to_end(&mut raw)
        && matches!(
            err.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        )
    {
        return None;
    }
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .unwrap_or(raw.len());
    let headers = String::from_utf8_lossy(&raw[..split]).to_string();
    let rest = raw.get(split + 4..).unwrap_or_default();
    let body = if headers
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        dechunk(rest)
    } else {
        rest.to_vec()
    };
    let status = headers
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    Some(Answer {
        status,
        headers,
        body: String::from_utf8_lossy(&body).to_string(),
    })
}

/// A hostile JSON value: wrong types, extreme numbers, long strings, deep nesting (below and
/// at serde_json's parse limit of 128).
fn hostile_value(rng: &mut Rng) -> Value {
    use serde_json::json;
    let depth = *rng.pick(&[2usize, 60, 120]);
    match rng.below(16) {
        0 => Value::Null,
        1 => json!(true),
        2 => json!(-1),
        3 => json!(u64::MAX),
        4 => json!(i64::MIN),
        5 => json!(1.0e308),
        6 => json!(""),
        7 => json!("x".repeat(100_000)),
        8 => json!([]),
        9 => json!({}),
        10 => {
            let mut v = json!(1);
            for _ in 0..depth {
                v = json!([v]);
            }
            v
        }
        11 => {
            let mut v = json!("leaf");
            for _ in 0..depth {
                v = json!({ "k": v });
            }
            v
        }
        12 => json!("\u{0}\u{feff}\u{10ffff}"),
        13 => json!({"commit": u64::MAX, "mark": "", "time": "9999-99-99T99:99:99Z"}),
        14 => json!(["g.v()", 1, null]),
        _ => json!("g.v().count()"),
    }
}

/// Replaces, removes or adds a key of an object (recursively at random) with hostile values.
fn mutate_value(rng: &mut Rng, value: &mut Value) {
    match value {
        Value::Object(map) if !map.is_empty() && rng.below(4) != 0 => {
            let keys: Vec<String> = map.keys().cloned().collect();
            let key = rng.pick(&keys).clone();
            match rng.below(4) {
                0 => {
                    map.remove(&key);
                }
                1 => {
                    if let Some(inner) = map.get_mut(&key) {
                        mutate_value(rng, inner);
                    }
                }
                _ => {
                    map.insert(key, hostile_value(rng));
                }
            }
        }
        Value::Object(map) => {
            let key = rng
                .pick(&[
                    "bogus", "id", "name", "target", "schema", "patch", "options", "",
                ])
                .to_string();
            map.insert(key, hostile_value(rng));
        }
        Value::Array(items) if !items.is_empty() => {
            let at = rng.below(items.len());
            mutate_value(rng, &mut items[at]);
        }
        _ => *value = hostile_value(rng),
    }
}

/// Byte-level damage of a body text: a cut, an invalid UTF-8 byte, a JSON-breaking character,
/// nesting far past the parse limit, an out-of-range number.
fn mutate_text(rng: &mut Rng, text: &[u8]) -> Vec<u8> {
    let mut out = text.to_vec();
    if out.is_empty() {
        out.push(b'{');
    }
    let at = rng.below(out.len());
    match rng.below(6) {
        0 => out.truncate(at),
        1 => out[at] = *rng.pick(&[0xFF, 0x80, 0xC3, 0x00]),
        2 => out[at] = *rng.pick(b"{}[]\":,\\"),
        3 => {
            out.splice(at..at, "[".repeat(100_000).into_bytes());
        }
        4 => {
            out.splice(at..at, b"{\"a\":".repeat(300));
        }
        _ => {
            out.splice(at..at, b"1e999999999".iter().copied());
        }
    }
    out
}

/// The POST endpoints of the API with a valid body each (the fork endpoints are left out: a
/// fork writes a directory next to the store).
fn api_bodies() -> Vec<(&'static str, Value)> {
    use serde_json::json;
    let schema = json!({"mode": "open", "vertices": {"person": {"properties": {
        "name": {"type": "string"}}}}});
    vec![
        (
            "/api/execute",
            json!({"script": "g.v().count()", "options": {"profile": true}}),
        ),
        (
            "/api/execute",
            json!({"script": "g.add_v(\"fz\").to_list()", "options": {}}),
        ),
        (
            "/api/execute",
            json!({"script": "1 + 1", "options": {"maxRows": 2, "maxItems": 2}}),
        ),
        ("/api/schema/set", schema.clone()),
        ("/api/schema/validate", schema.clone()),
        ("/api/schema/diff", schema.clone()),
        ("/api/schema/patch", json!({"vertices": {"person": null}})),
        (
            "/api/schema/validate-patch",
            json!({"vertices": {"x": {"properties": {}}}}),
        ),
        ("/api/store/checkpoint", json!({"name": "fz"})),
        ("/api/store/mark", json!({"name": "fz"})),
        ("/api/store/view", json!({"target": {"commit": 1}})),
        ("/api/store/live", json!({})),
        (
            "/api/store/rollback",
            json!({"target": {"commit": 1}, "delete": false}),
        ),
        ("/api/store/attic/restore", json!({"id": "nope"})),
        ("/api/store/attic/remove", json!({"id": "nope"})),
        ("/api/store/verify", json!({})),
        ("/api/cancel", json!({})),
        (
            "/api/layout",
            json!({"vertexCount": 3, "edgeSource": [0, 1], "edgeTarget": [1, 2], "budgetMs": 5}),
        ),
        ("/api/layout/step", json!({"job": 1, "budgetMs": 5})),
        ("/api/layout/cancel", json!({"job": 1})),
        ("/api/save", json!({})),
        (
            "/api/queries/define",
            json!({"name": "fz", "params": [{"name": "n", "schema": {"type": "integer"}, "default": 1}],
                   "body": "fn fz(n) { g.v().limit(n) }"}),
        ),
        ("/api/queries/move", json!({"name": "fz", "folder": "a/b"})),
        (
            "/api/queries/call-text",
            json!({"name": "fz", "args": {"n": 2, "s": "x\"y"}}),
        ),
        ("/api/queries/drop", json!({"name": "fz"})),
        (
            "/api/catalog/load",
            json!({"definitions": [{"kind": "query", "name": "fz", "source": "fn fz() { 1 }"}]}),
        ),
        (
            "/api/compression/define",
            json!({"name": "fz", "label": "person", "path": "name", "minBytes": 1}),
        ),
        ("/api/compression/recompress", json!({"name": "fz"})),
        ("/api/compression/drop", json!({"name": "fz"})),
        (
            "/api/indexes/create",
            json!({"label": "person", "key": "name", "name": "fz"}),
        ),
        (
            "/api/indexes/create",
            json!({"label": "", "key": "a\u{0}b", "name": "fz fz", "element": "nope"}),
        ),
        ("/api/indexes/drop", json!({"name": "fz"})),
    ]
}

/// GET paths with hostile query values.
const API_GETS: &[&str] = &[
    "/api/info",
    "/api/changes",
    "/api/graph",
    "/api/schema",
    "/api/schema/stored",
    "/api/schema/infer",
    "/api/graph-view?maxNodes=18446744073709551615&maxEdges=18446744073709551615",
    "/api/graph-view?maxNodes=-1&maxEdges=x",
    "/api/element?kind=vertex&id=%ZZ",
    "/api/element?kind=%00&id=1",
    "/api/element?kind=edge",
    "/api/element?kind=vertex&id=%F0%9F%9A%80",
    "/api/completions",
    "/api/token-values",
    "/api/memory",
    "/api/statistics",
    "/api/export?format=gsnap",
    "/api/export?format=graphml",
    "/api/export?format=%00",
    "/api/store",
    "/api/store/snapshot?commit=18446744073709551615",
    "/api/store/snapshot?commit=-1",
    "/api/store/attic/changes?id=%ZZ&offset=18446744073709551615&limit=18446744073709551615",
    "/api/store/attic/changes?id=nope&offset=x",
    "/api/store/attic/changes?id=%F0%9F%9A%80+%2e%2e",
    "/api/queries?folder=%ZZ",
    "/api/queries?folder=a%2Fb",
    "/api/queries/get?name=%F0%9F%9A%80",
    "/api/catalog/export",
    "/api/indexes",
    "/%FF%FE",
    "/%2",
    "/api/",
    "/api",
];

/// One JSON-RPC body for `/mcp`, valid or not.
fn mcp_body(rng: &mut Rng) -> Vec<u8> {
    use serde_json::json;
    let tools = [
        "query",
        "get_schema",
        "infer_schema",
        "validate_schema",
        "validate_schema_patch",
        "diff_schema",
        "set_schema",
        "patch_schema",
        "statistics",
        "graph_info",
        "store_info",
        "attic_list",
        "checkpoint",
        "mark",
        "open_view",
        "close_view",
        "rollback",
        "attic_restore",
        "nope",
    ];
    let args = [
        json!({"script": "g.v().count()", "max_rows": u64::MAX, "profile": true}),
        json!({"script": "g.add_v(\"m\").to_list()"}),
        json!({"schema": {"mode": "open", "vertices": {}}, "mode": "closed"}),
        json!({"schema": "{\"vertices\": ", "mode": 7}),
        json!({"patch": {"vertices": null}}),
        json!({"patch": "not json"}),
        json!({"name": "m"}),
        json!({"commit": 1}),
        json!({"target": {"mark": "nope"}, "delete": "yes"}),
        json!({"id": "nope"}),
        json!({}),
    ];
    let methods = [
        "initialize",
        "ping",
        "tools/list",
        "tools/call",
        "resources/list",
        "resources/templates/list",
        "resources/read",
        "prompts/list",
        "notifications/initialized",
        "nope/never",
        "",
    ];
    let method = *rng.pick(&methods);
    let params = match method {
        "tools/call" => json!({"name": rng.pick(&tools), "arguments": rng.pick(&args).clone()}),
        "resources/read" => json!({"uri": rng.pick(&[
            "graphersal://dsl-reference",
            "graphersal://examples",
            "graphersal://schema",
            "graphersal://statistics",
            "graphersal://../../etc/passwd",
            "",
        ])}),
        "initialize" => json!({"protocolVersion": "2025-06-18", "clientInfo": {"name": "fz"}}),
        _ => json!({}),
    };
    let ids = [
        json!(1),
        json!(u64::MAX),
        json!(i64::MIN),
        json!(1.5),
        json!("x".repeat(70_000)),
        json!({"a": [1]}),
        json!([1, 2]),
        Value::Null,
    ];
    let mut message = json!({"jsonrpc": "2.0", "id": rng.pick(&ids).clone(),
        "method": method, "params": params});
    match rng.below(6) {
        0 => {}
        1 | 2 => mutate_value(rng, &mut message),
        3 => {
            // A notification or a response without "method".
            if let Some(map) = message.as_object_mut() {
                map.remove(if rng.below(2) == 0 { "id" } else { "method" });
            }
        }
        4 => message = json!([message.clone(), message]),
        _ => return mutate_text(rng, message.to_string().as_bytes()),
    }
    message.to_string().into_bytes()
}

/// The statuses the server may answer; 500 means a request died inside the server.
fn allowed_status(status: u16) -> bool {
    matches!(
        status,
        200 | 202 | 400 | 401 | 403 | 404 | 405 | 406 | 409 | 413 | 415 | 422
    )
}

/// Random API and MCP requests (hostile bodies, headers, query strings, sizes) against one
/// server on a Store directory with `--mcp`: every answer is a clean 2xx/4xx, an error is JSON,
/// nothing hangs, nothing panics (stderr), and the server still serves afterwards.
/// `ROBUSTNESS_INPUT_CASES=<n>` raises the budget.
#[test]
fn hostile_api_and_mcp_requests_get_clean_answers() {
    let ui = ui_dir("fuzz");
    let base = ui.parent().unwrap().to_path_buf();
    let data = base.join("data");
    let _ = std::fs::remove_dir_all(&data);
    let stderr_path = base.join("stderr.txt");
    let stderr = std::fs::File::create(&stderr_path).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_graphersal"))
        .args(["--graph", data.to_str().unwrap(), "--create-store"])
        .args(["--ui-dir", ui.to_str().unwrap()])
        .args(["--timeout", "3000", "--safe-limits", "--mcp"])
        .args(["--server", "--port", "0"])
        .stdout(Stdio::piped())
        .stderr(stderr)
        .spawn()
        .expect("graphersal starts");
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let port: u16 = line
        .trim()
        .trim_end_matches('/')
        .rsplit(':')
        .next()
        .and_then(|p| p.parse().ok())
        .unwrap_or_else(|| panic!("no address on stdout: {line:?}"));
    let server = DevServer { child, port };
    let host = format!("Host: 127.0.0.1:{port}");
    // A first commit, so store targets resolve.
    execute(
        port,
        r#"g.add_v("person").property("name", "ann").to_list()"#,
    );

    let bodies = api_bodies();
    let mut rng = Rng(0x7E57_5E2F);
    let mut fatal = Vec::new();
    let mut statuses = std::collections::BTreeMap::<u16, usize>::new();
    for case in 0..fuzz_budget(250) {
        let (method, path, body, mut headers): (&str, String, Vec<u8>, Vec<String>) =
            match rng.below(10) {
                0..=3 => {
                    let (path, base) = rng.pick(&bodies).clone();
                    let mut value = base;
                    for _ in 0..rng.below(3) {
                        mutate_value(&mut rng, &mut value);
                    }
                    let mut text = value.to_string().into_bytes();
                    if rng.below(3) == 0 {
                        text = mutate_text(&mut rng, &text);
                    }
                    ("POST", path.to_string(), text, vec![JSON.to_string()])
                }
                4 => ("GET", rng.pick(API_GETS).to_string(), Vec::new(), vec![]),
                5..=8 => {
                    let mut headers = vec![JSON.to_string(), MCP_ACCEPT.to_string()];
                    if rng.below(5) == 0 {
                        headers.push(
                            rng.pick(&[
                                "Mcp-Session-Id: 0000",
                                "MCP-Protocol-Version: 1999-01-01",
                                "Accept: text/html",
                                "Authorization: Bearer x",
                            ])
                            .to_string(),
                        );
                    }
                    let method = *rng.pick(&["POST", "POST", "POST", "GET", "DELETE", "PUT"]);
                    (method, "/mcp".to_string(), mcp_body(&mut rng), headers)
                }
                _ => {
                    // Oversized or mislabelled bodies.
                    let (path, _) = rng.pick(&bodies).clone();
                    let (body, header) = match rng.below(3) {
                        0 => (vec![b' '; 16 * 1024 * 1024 + 1], JSON),
                        1 => (b"{}".to_vec(), "Content-Type: text/plain"),
                        _ => (
                            br#"{"script": "1"}"#.to_vec(),
                            "Content-Type: application/x",
                        ),
                    };
                    ("POST", path.to_string(), body, vec![header.to_string()])
                }
            };
        // Sometimes a Content-Length that lies (an absurd one is
        // `an_absurd_content_length_does_not_abort_the_server`).
        let length = match rng.below(12) {
            0 => body.len() + 100,
            1 => body.len() / 2,
            2 => 16 * 1024 * 1024 + 1,
            _ => body.len(),
        };
        headers.push(format!("Content-Length: {length}"));
        let mut head = format!("{method} {path} HTTP/1.1\r\nConnection: close\r\n{host}\r\n");
        for h in &headers {
            head.push_str(h);
            head.push_str("\r\n");
        }
        head.push_str("\r\n");
        let what = format!(
            "case {case}: {method} {path} {headers:?} {}",
            String::from_utf8_lossy(&body[..body.len().min(200)])
        );
        let Some(answer) = raw_request(port, &head, &body) else {
            fatal.push(format!("{what}: no answer within 60 s"));
            continue;
        };
        *statuses.entry(answer.status).or_default() += 1;
        if answer.status == 0 {
            // A connection closed without an answer (tiny_http does that for a request whose
            // head or length it cannot use): the server must still serve.
            if get(port, "/api/info").status != 200 {
                fatal.push(format!("{what}: the server stopped answering"));
                break;
            }
            continue;
        }
        if !allowed_status(answer.status) {
            fatal.push(format!("{what}: status {} {}", answer.status, answer.body));
            continue;
        }
        let is_json = answer
            .headers
            .to_ascii_lowercase()
            .contains("content-type: application/json");
        if is_json && serde_json::from_str::<Value>(&answer.body).is_err() {
            fatal.push(format!("{what}: not JSON: {}", answer.body));
        }
        if answer.status >= 400 && path.starts_with("/api/") && !is_json {
            fatal.push(format!("{what}: error {} without JSON", answer.status));
        }
    }
    // The fuzzer reaches the handlers, not only the request checks.
    for status in [200, 400, 422] {
        assert!(statuses.contains_key(&status), "{statuses:?}");
    }
    // Still serving, and nothing panicked.
    assert_eq!(get(port, "/api/info").status, 200);
    // Back to the live graph if a fuzzed view is open, then a run.
    post(port, "/api/store/live", "{}");
    let r = execute(port, "1 + 1");
    assert_eq!(r["ok"], true, "{r}");
    drop(server);
    let log = std::fs::read_to_string(&stderr_path).unwrap_or_default();
    let panics: Vec<&str> = log.lines().filter(|l| l.contains("panicked")).collect();
    let _ = std::fs::remove_dir_all(&base);
    assert!(
        panics.is_empty(),
        "a panic in the server:\n{}",
        panics.join("\n")
    );
    assert!(
        fatal.is_empty(),
        "{} findings:\n{}",
        fatal.len(),
        fatal.join("\n")
    );
}

/// Regression (T3 fuzzing): a request declaring a body of 2^62 bytes aborted the whole server,
/// because `tiny_http` drains an unread body with one buffer of the declared size. It is now
/// ignored (no answer) and the server keeps serving.
#[test]
fn an_absurd_content_length_does_not_abort_the_server() {
    let ui = ui_dir("content-length");
    let server = start(&["--graph", "modern", "--ui-dir", ui.to_str().unwrap()]);
    let port = server.port;
    for (method, path) in [
        ("GET", "/api/info"),
        ("POST", "/api/execute"),
        ("POST", "/mcp"),
    ] {
        for length in [1u64 << 62, u64::MAX / 2, 1 << 40] {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_millis(500)))
                .unwrap();
            let head = format!(
                "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\
                 {JSON}\r\nContent-Length: {length}\r\n\r\n{{}}"
            );
            stream.write_all(head.as_bytes()).unwrap();
            let _ = stream.shutdown(std::net::Shutdown::Write);
            let mut raw = Vec::new();
            let _ = stream.read_to_end(&mut raw);
            assert_eq!(
                get(port, "/api/info").status,
                200,
                "{method} {path} {length}"
            );
        }
    }
    assert_eq!(execute(port, "1 + 1")["ok"], true);
}

/// Backups over HTTP (`POST /api/store/backup`: full next to the store, then increments) and
/// damage found while the store is served: the backup is refused with the report, the store
/// turns read-only (its damage policy `maintenance`), `/api/info`, `/api/changes`,
/// `/api/store` and the MCP `graph_info` show it, and a backup from memory saves the graph.
#[test]
fn backups_over_http_and_damage_found_while_served() {
    let ui = ui_dir("bk1");
    let base = ui.parent().unwrap().to_path_buf();
    let data = base.join("bk1-data");
    let saved = base.join("bk1-saved");
    let default_backup = base.join("bk1-data-backup");
    for path in [&data, &saved, &default_backup] {
        let _ = std::fs::remove_dir_all(path);
    }
    let data_arg = data.to_str().unwrap().to_string();
    let ui_arg = ui.to_str().unwrap().to_string();
    let server = start(&[
        "--graph",
        &data_arg,
        "--ui-dir",
        &ui_arg,
        "--create-store",
        "--on-damage",
        "maintenance",
        "--mcp",
    ]);
    let port = server.port;
    let r = execute(
        port,
        r#"g.add_v("person").property("name", "ann").to_list()"#,
    );
    assert_eq!(r["ok"], true, "{r}");
    let store = get(port, "/api/store").json();
    assert_eq!(store["store"]["damagePolicy"], "maintenance", "{store}");
    assert_eq!(
        store["store"]["storeId"].as_str().map(str::len),
        Some(36),
        "{store}"
    );
    assert_eq!(store["store"]["frozenByDamage"], false, "{store}");
    let first = ok_json(post(port, "/api/store/backup", "{}"));
    assert_eq!(first["backup"]["incremental"], false, "{first}");
    assert!(default_backup.join("GRAPH").exists());
    let r = execute(
        port,
        r#"g.add_v("person").property("name", "bob").to_list()"#,
    );
    assert_eq!(r["ok"], true, "{r}");
    let second = ok_json(post(port, "/api/store/backup", "{}"));
    assert_eq!(second["backup"]["incremental"], true, "{second}");
    assert_eq!(second["backup"]["commitSeq"], 2, "{second}");
    // Refused while no damage is known.
    let refused = post(
        port,
        "/api/store/backup",
        r#"{"from_memory": true, "dir": "bk1-saved"}"#,
    );
    assert_eq!(refused.status, 422, "{}", refused.body);

    // Damage on disk while served.
    let wal = std::fs::read_dir(data.join("wal"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .max()
        .unwrap();
    let mut bytes = std::fs::read(&wal).unwrap();
    let at = bytes.len() - 5;
    bytes[at] ^= 0x40;
    std::fs::write(&wal, bytes).unwrap();
    let damaged = post(port, "/api/store/backup", r#"{"dir": "bk1-other"}"#);
    assert_eq!(damaged.status, 422, "{}", damaged.body);
    assert!(damaged.body.contains("is damaged"), "{}", damaged.body);
    assert!(damaged.body.contains("store repair"), "{}", damaged.body);
    assert!(!base.join("bk1-other").exists());
    let info = get(port, "/api/info").json();
    assert_eq!(info["damageFound"]["frozen"], true, "{info}");
    assert_eq!(info["damageFound"]["foundBy"], "backup", "{info}");
    assert_eq!(changes(port)["damageFound"]["frozen"], true);
    let store = get(port, "/api/store").json();
    assert_eq!(store["store"]["frozenByDamage"], true, "{store}");
    assert_eq!(store["store"]["backupFromMemory"], true, "{store}");
    assert!(
        store["store"]["damageText"]
            .as_str()
            .unwrap()
            .contains("READ-ONLY"),
        "{store}"
    );
    let r = execute(
        port,
        r#"g.add_v("person").property("name", "cy").to_list()"#,
    );
    assert_eq!(r["ok"], false, "{r}");
    assert!(r.to_string().contains("read-only"), "{r}");
    let mut mcp = McpClient::connect(port, &[]);
    let (error, text) = mcp.call("graph_info", serde_json::json!({}));
    assert!(
        !error && text.contains("damageFound") && text.contains("\"frozen\": true"),
        "{text}"
    );
    // Saved from memory, restored: the live graph.
    let from_memory = ok_json(post(
        port,
        "/api/store/backup",
        r#"{"from_memory": true, "dir": "bk1-saved"}"#,
    ));
    assert_eq!(from_memory["backup"]["fromMemory"], true, "{from_memory}");
    drop(server);
    let restored = Command::new(env!("CARGO_BIN_EXE_graphersal"))
        .args(["store", "restore", saved.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        restored.status.success(),
        "{}",
        String::from_utf8_lossy(&restored.stderr)
    );
    let count = Command::new(env!("CARGO_BIN_EXE_graphersal"))
        .args([
            "--graph",
            saved.to_str().unwrap(),
            "-e",
            "g.v().count().next()",
        ])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&count.stdout).trim(), "2");
    for path in [&data, &saved, &default_backup] {
        let _ = std::fs::remove_dir_all(path);
    }
}

#[test]
fn schema_flag_loads_the_graph_against_the_schema() {
    let ui = ui_dir("schema");
    let base = ui.parent().unwrap();
    let schema = base.join("schema.json");
    std::fs::write(
        &schema,
        r#"{"mode": "open", "vertices": {"thing": {"schema": {"type": "object",
            "properties": {"uid": {"type": "string", "format": "uuid"}}}}}}"#,
    )
    .unwrap();
    let file = base.join("graph.graphml");
    let status = Command::new(env!("CARGO_BIN_EXE_graphersal"))
        .args(["--graph", "empty", "-e"])
        .arg(format!(
            r#"g.add_v("thing").property("uid", "6f1c2d3e-4a5b-4c6d-8e7f-901234567890").next(); g.export_graphml({:?})"#,
            file.to_str().unwrap()
        ))
        .output()
        .unwrap();
    assert!(status.status.success());
    let server = start(&[
        "--graph",
        file.to_str().unwrap(),
        "--schema",
        schema.to_str().unwrap(),
        "--ui-dir",
        ui.to_str().unwrap(),
    ]);
    let port = server.port;
    let info = get(port, "/api/info").json();
    assert_eq!(info["saveable"], true);
    assert_eq!(
        info["schemaSource"],
        std::fs::canonicalize(&schema)
            .unwrap()
            .display()
            .to_string()
    );
    let answer = execute(port, r#"type_of(g.v().values("uid").next())"#);
    assert_eq!(answer["ok"], true, "{answer}");
    assert!(answer.to_string().contains("Uuid"), "{answer}");
    assert_eq!(get(port, "/api/graph").json()["name"], "graph.graphml");
    drop(server);

    // Data that violates the schema stops the start with exit code 1, nothing served.
    let out = Command::new(env!("CARGO_BIN_EXE_graphersal"))
        .args(["--graph", file.to_str().unwrap(), "--schema"])
        .arg(schema.to_str().unwrap())
        .args(["--schema-mode", "closed", "--graph", "modern"])
        .args(["--server", "--port", "0", "--ui-dir", ui.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("person is not declared"), "{stderr}");
}

#[test]
fn the_server_listens_on_the_ipv6_loopback_too() {
    let ui = ui_dir("ipv6");
    let server = start(&["--graph", "modern", "--ui-dir", ui.to_str().unwrap()]);
    let port = server.port;
    // `[::1]` is one of the server's own hosts (Host and Origin), on either listener.
    let host = format!("Host: [::1]:{port}");
    let origin = format!("Origin: http://[::1]:{port}");
    let info = request(port, "GET", "/api/info", None, &[&host, &origin]);
    assert_eq!(info.status, 200, "{}", info.body);
    let other_port = format!("Host: [::1]:{}", port.wrapping_add(1));
    assert_eq!(
        request(port, "GET", "/api/info", None, &[&other_port]).status,
        403
    );
    // A request to [::1] itself, when this machine has an IPv6 loopback.
    if std::net::TcpListener::bind(("::1", 0)).is_err() {
        return;
    }
    let mut stream = TcpStream::connect(("::1", port)).expect("the server listens on [::1]");
    stream
        .set_read_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    write!(
        stream,
        "GET /api/graph HTTP/1.1\r\nConnection: close\r\nHost: [::1]:{port}\r\n\r\n"
    )
    .unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).unwrap();
    assert!(raw.starts_with("HTTP/1.1 200"), "{raw}");
    let body = raw.split("\r\n\r\n").nth(1).unwrap_or_default();
    let graph: Value = serde_json::from_str(body).unwrap();
    assert_eq!(graph["vertexCount"], 6);
}

#[test]
fn save_without_schema_flag_warns_about_a_stored_schema() {
    let ui = ui_dir("save_warn");
    let file = ui.parent().unwrap().join("graph.graphml");
    let status = Command::new(env!("CARGO_BIN_EXE_graphersal"))
        .args(["--graph", "modern", "-e"])
        .arg(format!("g.export_graphml({:?})", file.to_str().unwrap()))
        .status()
        .unwrap();
    assert!(status.success());
    let server = start(&[
        "--graph",
        file.to_str().unwrap(),
        "--ui-dir",
        ui.to_str().unwrap(),
    ]);
    let port = server.port;
    // No schema: no warning.
    let saved = post(port, "/api/save", "").json();
    assert!(saved["warning"].is_null(), "{saved}");
    assert!(saved["schemaPath"].is_null(), "{saved}");
    // A stored schema cannot go into GraphML: the answer says so, and no file is written
    // beside the graph.
    let inferred = get(port, "/api/schema/infer").json();
    let set = post(port, "/api/schema/set?mode=open", &inferred.to_string());
    assert_eq!(set.status, 200, "{}", set.body);
    let saved = post(port, "/api/save", "").json();
    let warning = saved["warning"].as_str().unwrap_or_default();
    assert!(warning.contains("--schema FILE"), "{saved}");
    assert!(warning.contains("Save schema"), "{saved}");
    let files: Vec<_> = std::fs::read_dir(file.parent().unwrap())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        files.iter().all(|name| !name.ends_with(".json")),
        "{files:?}"
    );
}

#[test]
fn save_writes_the_schema_back_to_the_schema_file() {
    let ui = ui_dir("save_schema");
    let base = ui.parent().unwrap();
    let schema = base.join("schema.json");
    std::fs::write(
        &schema,
        r#"{"mode": "open", "vertices": {"thing": {"schema": {"type": "object",
            "properties": {"uid": {"type": "string", "format": "uuid"}}}}}}"#,
    )
    .unwrap();
    let file = base.join("graph.json");
    let out = Command::new(env!("CARGO_BIN_EXE_graphersal"))
        .args(["--graph", "empty", "-e"])
        .arg(format!(
            r#"g.add_v("thing").property("uid", "6f1c2d3e-4a5b-4c6d-8e7f-901234567890").next(); g.export_graphson({:?})"#,
            file.to_str().unwrap()
        ))
        .output()
        .unwrap();
    assert!(out.status.success());
    let server = start(&[
        "--graph",
        file.to_str().unwrap(),
        "--schema",
        schema.to_str().unwrap(),
        "--ui-dir",
        ui.to_str().unwrap(),
    ]);
    let port = server.port;
    // A new label in the schema and a vertex of it in the data.
    let patch = r#"{"vertices": {"gadget": {"schema": {"type": "object",
        "properties": {"size": {"type": "integer"}}}}}}"#;
    let patched = post(port, "/api/schema/patch", patch);
    assert_eq!(patched.status, 200, "{}", patched.body);
    let added = execute(port, r#"g.add_v("gadget").property("size", 3).next()"#);
    assert_eq!(added["ok"], true, "{added}");
    let saved = post(port, "/api/save", "").json();
    assert!(saved["warning"].is_null(), "{saved}");
    assert_eq!(
        saved["schemaPath"],
        std::fs::canonicalize(&schema)
            .unwrap()
            .display()
            .to_string()
    );
    // Both files are written, no temporary file is left behind.
    let written = std::fs::read_to_string(&schema).unwrap();
    assert!(written.contains("gadget"), "{written}");
    assert!(written.contains("uuid"), "{written}");
    let leftovers: Vec<_> = std::fs::read_dir(base)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty());
    drop(server);
    // The saved pair loads back: the schema first, then the data against it.
    let out = Command::new(env!("CARGO_BIN_EXE_graphersal"))
        .args(["--graph", file.to_str().unwrap(), "--schema"])
        .arg(schema.to_str().unwrap())
        .args(["-e", r#"g.v().has_label("gadget").values("size").next()"#])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "3");
}

/// The AI Chat (`--ai FILE`): the agent loop against a local mock of an OpenAI-compatible
/// provider (never a real one), its endpoints and the isolation of conversations.
#[cfg(feature = "ai")]
mod ai_chat {
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use serde_json::{Value, json};

    use super::{JSON, get, request, start, ui_dir};

    const KEY: &str = "sk-test-secret-9876";
    const LONG_QUERY: &str =
        r#"g.addV("ghost").next(); g.inject(1).repeat(__.identity()).times(1000000000).count()"#;

    /// A mock Chat Completions provider on 127.0.0.1. Its answer follows the person's last
    /// message: `RUN <script>` calls `query` with the script and then answers `RESULT: <tool
    /// result>`; `GEMINI <script>` does the same like Google Gemini's OpenAI-compatible endpoint
    /// (the call carries `extra_content.google.thought_signature`, and a request that sends the
    /// call back without it is refused with HTTP 400); `FAIL <script>` calls `query` and then
    /// answers HTTP 400; `LOOP` calls `query` forever; `SLOW` answers after 1.5 s; `SAVED
    /// <name>` calls `run_saved_query` with the name and answers like `RUN`; anything else is
    /// answered `hello`. Every request body is recorded.
    struct MockLlm {
        base_url: String,
        requests: Arc<Mutex<Vec<Value>>>,
        server: Arc<tiny_http::Server>,
    }

    impl Drop for MockLlm {
        fn drop(&mut self) {
            self.server.unblock();
        }
    }

    impl MockLlm {
        fn start() -> Self {
            let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").expect("mock"));
            let port = server.server_addr().to_ip().expect("ip").port();
            let requests = Arc::new(Mutex::new(Vec::new()));
            let (listener, recorded) = (server.clone(), requests.clone());
            std::thread::spawn(move || {
                for mut request in listener.incoming_requests() {
                    let recorded = recorded.clone();
                    std::thread::spawn(move || {
                        let mut text = String::new();
                        let _ = request.as_reader().read_to_string(&mut text);
                        let body: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
                        recorded.lock().unwrap().push(body.clone());
                        let (answer, delay) = brain(&body);
                        std::thread::sleep(delay);
                        let status = if answer.get("error").is_some() {
                            400
                        } else {
                            200
                        };
                        let header = tiny_http::Header::from_bytes(
                            &b"Content-Type"[..],
                            &b"application/json"[..],
                        )
                        .unwrap();
                        let _ = request.respond(
                            tiny_http::Response::from_string(answer.to_string())
                                .with_status_code(status)
                                .with_header(header),
                        );
                    });
                }
            });
            Self {
                base_url: format!("http://127.0.0.1:{port}"),
                requests,
                server,
            }
        }

        fn requests(&self) -> Vec<Value> {
            self.requests.lock().unwrap().clone()
        }
    }

    fn completion(message: Value, finish: &str) -> Value {
        json!({"id": "chatcmpl-mock", "object": "chat.completion",
               "choices": [{"index": 0, "message": message, "finish_reason": finish}],
               "usage": {"prompt_tokens": 10, "completion_tokens": 5}})
    }

    fn text(content: &str) -> Value {
        completion(json!({"role": "assistant", "content": content}), "stop")
    }

    fn tool_call(id: usize, name: &str, args: Value) -> Value {
        completion(
            json!({"role": "assistant", "content": null, "tool_calls": [{
                "id": format!("call_{id}"), "type": "function",
                "function": {"name": name, "arguments": args.to_string()}}]}),
            "tool_calls",
        )
    }

    fn brain(body: &Value) -> (Value, Duration) {
        let messages = body["messages"].as_array().cloned().unwrap_or_default();
        let user = messages
            .iter()
            .rev()
            .find(|m| m["role"] == "user")
            .and_then(|m| m["content"].as_str())
            .unwrap_or("")
            .to_string();
        let last = messages.last().cloned().unwrap_or(Value::Null);
        let after_tool = last["role"] == "tool";
        let id = messages.len();
        if let Some(script) = user.strip_prefix("GEMINI ") {
            if !after_tool {
                let mut answer = tool_call(id, "query", json!({ "script": script }));
                answer["choices"][0]["message"]["tool_calls"][0]["extra_content"] =
                    json!({"google": {"thought_signature": "sig-1"}});
                return (answer, Duration::ZERO);
            }
            let call = &messages[messages.len() - 2]["tool_calls"][0];
            if call["extra_content"]["google"]["thought_signature"] != "sig-1" {
                return (
                    json!({"error": {"code": 400, "status": "INVALID_ARGUMENT",
                        "message": "Function call is missing a thought_signature in \
                                    functionCall parts."}}),
                    Duration::ZERO,
                );
            }
            let result = last["content"].as_str().unwrap_or("");
            return (text(&format!("RESULT: {result}")), Duration::ZERO);
        }
        if let Some(script) = user.strip_prefix("FAIL ") {
            return if after_tool {
                (
                    json!({"error": {"message": "The provider refused the request."}}),
                    Duration::ZERO,
                )
            } else {
                (
                    tool_call(id, "query", json!({ "script": script })),
                    Duration::ZERO,
                )
            };
        }
        if user == "LOOP" {
            return (
                tool_call(id, "query", json!({"script": "g.v().count()"})),
                Duration::ZERO,
            );
        }
        if let Some(name) = user.strip_prefix("SAVED ") {
            return if after_tool {
                let result = last["content"].as_str().unwrap_or("");
                (text(&format!("RESULT: {result}")), Duration::ZERO)
            } else {
                (
                    tool_call(id, "run_saved_query", json!({ "name": name })),
                    Duration::ZERO,
                )
            };
        }
        if let Some(script) = user.strip_prefix("RUN ") {
            return if after_tool {
                let result = last["content"].as_str().unwrap_or("");
                (text(&format!("RESULT: {result}")), Duration::ZERO)
            } else {
                (
                    tool_call(id, "query", json!({ "script": script })),
                    Duration::ZERO,
                )
            };
        }
        if user == "SLOW" {
            return (text("slow answer"), Duration::from_millis(1500));
        }
        (text("hello"), Duration::ZERO)
    }

    /// Starts a server on the modern graph with an `--ai` file for the mock; `extra` are more
    /// keys of the file.
    fn start_ai(name: &str, mock: &MockLlm, extra: &str) -> super::DevServer {
        let ui = ui_dir(name);
        let file = ui.parent().unwrap().join("ai.json");
        std::fs::write(
            &file,
            format!(
                r#"{{"provider": "openai", "model": "mock-model", "base_url": "{}/v1",
                    "api_key": "{KEY}"{extra}}}"#,
                mock.base_url
            ),
        )
        .unwrap();
        start(&[
            "--graph",
            "modern",
            "--ui-dir",
            ui.to_str().unwrap(),
            "--ai",
            file.to_str().unwrap(),
        ])
    }

    fn call(port: u16, method: &str, path: &str, body: Option<&str>, who: &str) -> super::Answer {
        let who = format!("X-Graphersal-Client: {who}");
        let mut headers = vec![who.as_str()];
        if body.is_some() {
            headers.push(JSON);
        }
        request(port, method, path, body, &headers)
    }

    fn create(port: u16, who: &str) -> String {
        let answer = call(port, "POST", "/api/ai/conversations", Some("{}"), who);
        assert_eq!(answer.status, 200, "{}", answer.body);
        let id = answer.json()["id"].as_str().unwrap().to_string();
        assert_eq!(id.len(), 32, "a random 128-bit id: {id}");
        id
    }

    fn send(port: u16, who: &str, id: &str, text: &str) -> super::Answer {
        call(
            port,
            "POST",
            &format!("/api/ai/conversations/{id}/messages"),
            Some(&json!({ "text": text }).to_string()),
            who,
        )
    }

    fn stop(port: u16, who: &str, id: &str) -> Value {
        let path = format!("/api/ai/conversations/{id}/stop");
        let answer = call(port, "POST", &path, Some("{}"), who);
        assert_eq!(answer.status, 200, "{}", answer.body);
        answer.json()
    }

    fn events(port: u16, who: &str, id: &str) -> Value {
        let path = format!("/api/ai/conversations/{id}/events?after=0");
        let answer = call(port, "GET", &path, None, who);
        assert_eq!(answer.status, 200, "{}", answer.body);
        answer.json()
    }

    /// Waits until the conversation's turn has ended; its events.
    fn finished(port: u16, who: &str, id: &str) -> Vec<Value> {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let answer = events(port, who, id);
            if answer["running"] == false {
                return answer["events"].as_array().unwrap().clone();
            }
            assert!(Instant::now() < deadline, "the turn did not end: {answer}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Waits until an event of `kind` appears.
    fn wait_for(port: u16, who: &str, id: &str, kind: &str) {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let answer = events(port, who, id);
            if answer["events"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["type"] == kind)
            {
                return;
            }
            assert!(Instant::now() < deadline, "no {kind} event: {answer}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn of<'a>(events: &'a [Value], kind: &str) -> Vec<&'a Value> {
        events.iter().filter(|e| e["type"] == kind).collect()
    }

    /// A saved query that writes runs only in a turn after the one that asked the user.
    #[test]
    fn a_saved_query_that_writes_waits_for_the_users_confirmation() {
        let mock = MockLlm::start();
        let server = start_ai("ai-confirm", &mock, "");
        let port = server.port;
        let define =
            r#"{"name": "add_ghost", "body": "fn add_ghost() { g.add_v(\"ghost\").next() }"}"#;
        let answer = request(port, "POST", "/api/queries/define", Some(define), &[JSON]);
        assert_eq!(answer.json()["ok"], true, "{}", answer.body);
        let count = || {
            super::execute(port, "g.v().has_label(\"ghost\").count().next()")["execution"]
                ["results"][0]
                .clone()
        };
        let id = create(port, "page");
        assert_eq!(send(port, "page", &id, "SAVED add_ghost").status, 200);
        let events = finished(port, "page", &id);
        let results = of(&events, "tool_result");
        assert_eq!(results[0]["ok"], false, "{events:?}");
        assert!(
            results[0]["text"].as_str().unwrap().contains(
                r#"Not run: the saved query "add_ghost" changes data (Create Data(vertex "ghost"))"#
            ),
            "{events:?}"
        );
        assert_eq!(count(), 0);
        // The user's next message confirms: the call runs, once.
        assert_eq!(send(port, "page", &id, "SAVED add_ghost").status, 200);
        let events = finished(port, "page", &id);
        let results = of(&events, "tool_result");
        assert_eq!(results.last().unwrap()["ok"], true, "{events:?}");
        assert_eq!(count(), 1);
        assert_eq!(send(port, "page", &id, "hi").status, 200);
        finished(port, "page", &id);
        assert_eq!(send(port, "page", &id, "SAVED add_ghost").status, 200);
        let events = finished(port, "page", &id);
        assert_eq!(of(&events, "tool_result").last().unwrap()["ok"], false);
        assert_eq!(count(), 1);
    }

    #[test]
    fn a_question_is_answered_after_a_query_that_ran_on_the_session() {
        let mock = MockLlm::start();
        let server = start_ai("ai-answer", &mock, "");
        let port = server.port;

        let info = get(port, "/api/info");
        assert!(!info.body.contains(KEY), "the key never reaches the page");
        let ai = &info.json()["ai"];
        assert_eq!(ai["provider"], "openai", "{ai}");
        assert_eq!(ai["model"], "mock-model");
        assert_eq!(ai["readOnly"], false);
        assert_eq!(ai["sendsDataTo"], Value::Null, "a local provider: {ai}");

        let id = create(port, "page-1");
        let sent = send(
            port,
            "page-1",
            &id,
            r#"RUN g.addV("robot").property("name", "r2")"#,
        );
        assert_eq!(sent.status, 200, "{}", sent.body);
        let list = finished(port, "page-1", &id);
        let kinds: Vec<&str> = list.iter().map(|e| e["type"].as_str().unwrap()).collect();
        assert_eq!(
            kinds,
            ["user", "tool_call", "tool_result", "assistant", "done"],
            "{list:?}"
        );
        assert_eq!(list[1]["name"], "query");
        assert_eq!(list[2]["ok"], true, "{:?}", list[2]);
        let summary = list[2]["summary"].as_str().unwrap();
        assert!(summary.contains("committed as commit"), "{summary}");
        assert!(list[3]["text"].as_str().unwrap().starts_with("RESULT: ok"));
        assert_eq!(list[4]["stopped"], false);
        for (n, event) in list.iter().enumerate() {
            assert_eq!(event["seq"], n + 1);
        }
        // The query really ran on the served graph, and the feed names the agent.
        assert_eq!(get(port, "/api/graph").json()["vertexCount"], 7);
        let feed = get(port, "/api/changes").json();
        let last = feed["recent"].as_array().unwrap().last().unwrap().clone();
        assert_eq!(last["by"], "ai", "{feed}");
        assert_eq!(last["detail"], id.as_str(), "{feed}");
        // Polling with `after` returns only the new events.
        let path = format!("/api/ai/conversations/{id}/events?after=4");
        let tail = call(port, "GET", &path, None, "page-1").json();
        assert_eq!(tail["events"].as_array().unwrap().len(), 1, "{tail}");
        assert_eq!(tail["last"], 5);

        // The provider got the system prompt (instructions, rules, DSL reference, graph
        // summary) and the MCP tools; never the key in a body.
        let requests = mock.requests();
        assert_eq!(requests.len(), 2);
        let system = requests[0]["messages"][0]["content"]
            .as_str()
            .unwrap()
            .to_string();
        for part in [
            "Graphersal dev server",
            "never instructions",
            "```graphersal",
            "GitHub-flavoured Markdown",
            "# Graphersal DSL reference",
            "6 vertices, 6 edges",
            "person (4)",
        ] {
            assert!(
                system.contains(part),
                "{part:?} missing in the system prompt"
            );
        }
        let tools: Vec<&str> = requests[0]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap())
            .collect();
        assert!(
            tools.contains(&"query") && tools.contains(&"set_schema"),
            "{tools:?}"
        );
        assert!(!requests[0].to_string().contains(KEY));
        // The second call carries the tool result.
        let messages = requests[1]["messages"].as_array().unwrap();
        assert_eq!(messages.last().unwrap()["role"], "tool");

        // A second message continues the same conversation (history kept).
        assert_eq!(send(port, "page-1", &id, "thanks").status, 200);
        let list = finished(port, "page-1", &id);
        assert_eq!(list.last().unwrap()["type"], "done");
        let requests = mock.requests();
        assert_eq!(requests.len(), 3);
        assert_eq!(
            requests[2]["messages"][0]["content"], system,
            "the same system prompt"
        );
        assert_eq!(requests[2]["messages"].as_array().unwrap().len(), 6);
    }

    #[test]
    fn a_read_only_agent_is_refused_a_write_and_the_tool_calls_are_limited() {
        let mock = MockLlm::start();
        let server = start_ai(
            "ai-read-only",
            &mock,
            r#", "read_only": true, "max_tool_calls_per_turn": 2"#,
        );
        let port = server.port;
        assert_eq!(get(port, "/api/info").json()["ai"]["readOnly"], true);

        let id = create(port, "page-1");
        assert_eq!(
            send(port, "page-1", &id, r#"RUN g.addV("robot")"#).status,
            200
        );
        let list = finished(port, "page-1", &id);
        let result = of(&list, "tool_result")[0];
        assert_eq!(result["ok"], false, "{result}");
        let text = result["text"].as_str().unwrap();
        assert!(text.contains("denied"), "{text}");
        assert_eq!(get(port, "/api/graph").json()["vertexCount"], 6);
        let first = &mock.requests()[0];
        let tools: Vec<&str> = first["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap())
            .collect();
        assert!(
            tools.contains(&"query") && !tools.contains(&"set_schema"),
            "{tools:?}"
        );
        let system = first["messages"][0]["content"].as_str().unwrap();
        assert!(system.contains("READ-ONLY"));

        // A model that keeps calling tools is stopped at max_tool_calls_per_turn.
        assert_eq!(send(port, "page-1", &id, "LOOP").status, 200);
        let list = finished(port, "page-1", &id);
        let begin = list.iter().rposition(|e| e["type"] == "user").unwrap();
        let turn = &list[begin..];
        assert_eq!(of(turn, "tool_result").len(), 2, "{turn:?}");
        let error = of(turn, "error")[0];
        let message = error["message"].as_str().unwrap();
        assert!(message.contains("limit of 2 tool calls"), "{error}");
        assert_eq!(turn.last().unwrap()["type"], "done");
        assert_eq!(turn.last().unwrap()["toolCalls"], 2);
    }

    #[test]
    fn conversations_are_isolated_between_clients() {
        let mock = MockLlm::start();
        let server = start_ai("ai-isolation", &mock, "");
        let port = server.port;

        // A conversation needs the page's id.
        let anonymous = request(port, "POST", "/api/ai/conversations", Some("{}"), &[JSON]);
        assert_eq!(anonymous.status, 400, "{}", anonymous.body);
        let mine = create(port, "page-a");
        let other = create(port, "page-b");
        assert_ne!(mine, other);
        // Another client can neither read nor post to it, nor stop or delete it.
        let path = format!("/api/ai/conversations/{mine}/events?after=0");
        assert_eq!(call(port, "GET", &path, None, "page-b").status, 403);
        assert_eq!(send(port, "page-b", &mine, "hi").status, 403);
        let stop_path = format!("/api/ai/conversations/{mine}/stop");
        assert_eq!(
            call(port, "POST", &stop_path, Some("{}"), "page-b").status,
            403
        );
        let conversation = format!("/api/ai/conversations/{mine}");
        assert_eq!(
            call(port, "DELETE", &conversation, None, "page-b").status,
            403
        );
        // Unknown ids are 404.
        let unknown = "/api/ai/conversations/0123456789abcdef0123456789abcdef/events";
        assert_eq!(call(port, "GET", unknown, None, "page-a").status, 404);

        // Concurrent turns of two conversations; a second send while a turn runs is 409.
        let started = Instant::now();
        assert_eq!(send(port, "page-a", &mine, "SLOW").status, 200);
        assert_eq!(send(port, "page-b", &other, "SLOW").status, 200);
        assert_eq!(send(port, "page-a", &mine, "again").status, 409);
        assert_eq!(events(port, "page-a", &mine)["running"], true);
        assert_eq!(events(port, "page-b", &other)["running"], true);
        let a = finished(port, "page-a", &mine);
        let b = finished(port, "page-b", &other);
        assert!(
            started.elapsed() < Duration::from_millis(2900),
            "the two slow answers ran concurrently"
        );
        assert_eq!(of(&a, "assistant")[0]["text"], "slow answer");
        assert_eq!(of(&b, "assistant")[0]["text"], "slow answer");
        // Each has its own history: page-b's conversation never saw page-a's messages.
        assert_eq!(of(&b, "user").len(), 1);
        for body in mock.requests() {
            let users = body["messages"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|m| m["role"] == "user")
                .count();
            assert_eq!(users, 1, "{body}");
        }

        // DELETE, then the conversation is gone.
        assert_eq!(
            call(port, "DELETE", &conversation, None, "page-a").status,
            200
        );
        assert_eq!(call(port, "GET", &path, None, "page-a").status, 404);
        assert_eq!(send(port, "page-a", &mine, "hi").status, 404);
        // Without --ai the endpoints are 404 and /api/info says so.
        let ui = ui_dir("ai-off");
        let plain = start(&["--graph", "modern", "--ui-dir", ui.to_str().unwrap()]);
        assert_eq!(get(plain.port, "/api/info").json()["ai"], Value::Null);
        let off = call(plain.port, "POST", "/api/ai/conversations", Some("{}"), "p");
        assert_eq!(off.status, 404, "{}", off.body);
    }

    #[test]
    fn gemini_thought_signatures_go_back_and_a_provider_error_keeps_the_chat_usable() {
        let mock = MockLlm::start();
        let server = start_ai("ai-gemini", &mock, "");
        let port = server.port;
        let id = create(port, "page-1");

        // The signature of the tool call goes back with the next request (else HTTP 400).
        assert_eq!(
            send(port, "page-1", &id, "GEMINI g.v().count()").status,
            200
        );
        let list = finished(port, "page-1", &id);
        assert!(of(&list, "error").is_empty(), "{list:?}");
        let answer = of(&list, "assistant")[0]["text"].as_str().unwrap();
        assert!(answer.starts_with("RESULT: ok"), "{list:?}");
        // Without the signature the mock answers HTTP 400 (the user's live failure).
        let sent = &mock.requests()[1]["messages"][2]["tool_calls"][0];
        assert_eq!(
            sent["extra_content"]["google"]["thought_signature"], "sig-1",
            "{sent}"
        );

        // A provider error after a tool call ends the turn with the error only, says how
        // far the turn got, and the next message continues with a valid history.
        assert_eq!(send(port, "page-1", &id, "FAIL g.v().count()").status, 200);
        let list = finished(port, "page-1", &id);
        let errors = of(&list, "error");
        assert_eq!(errors.len(), 1, "{list:?}");
        let message = errors[0]["message"].as_str().unwrap();
        assert!(
            message.contains("The provider refused the request."),
            "{message}"
        );
        assert!(
            message.contains("the turn stopped after 1 tool call;"),
            "{message}"
        );
        assert_eq!(list.last().unwrap()["stopped"], false);
        assert_eq!(send(port, "page-1", &id, "hello").status, 200);
        let list = finished(port, "page-1", &id);
        assert_eq!(of(&list, "assistant").last().unwrap()["text"], "hello");
        let last = mock.requests().last().unwrap().clone();
        let roles: Vec<&str> = last["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["role"].as_str().unwrap())
            .collect();
        assert_eq!(
            roles,
            [
                "system",
                "user",
                "assistant",
                "tool",
                "assistant",
                "user",
                "assistant",
                "tool",
                "user"
            ],
            "{last}"
        );
    }

    #[test]
    fn stop_ends_only_its_own_conversations_turn_and_query() {
        let mock = MockLlm::start();
        let server = start_ai("ai-stop", &mock, "");
        let port = server.port;

        // Conversation A runs a long query on the session.
        let a = create(port, "page-a");
        let run = format!("RUN {LONG_QUERY}");
        assert_eq!(send(port, "page-a", &a, &run).status, 200);
        wait_for(port, "page-a", &a, "tool_call");
        // Conversation B (another tab) waits for the session; Stop ends B's turn only.
        let b = create(port, "page-b");
        assert_eq!(send(port, "page-b", &b, "hello").status, 200);
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(stop(port, "page-b", &b)["stopped"], true);
        let b_events = finished(port, "page-b", &b);
        assert_eq!(b_events.last().unwrap()["stopped"], true);
        // A's query keeps running.
        std::thread::sleep(Duration::from_millis(500));
        let a_now = events(port, "page-a", &a);
        assert_eq!(a_now["running"], true, "{a_now}");
        assert!(of(a_now["events"].as_array().unwrap(), "tool_result").is_empty());
        // Stop A: its query is cancelled and rolled back, the session is free again.
        assert_eq!(stop(port, "page-a", &a)["stopped"], true);
        let a_events = finished(port, "page-a", &a);
        assert_eq!(a_events.last().unwrap()["stopped"], true);
        assert_eq!(get(port, "/api/graph").json()["vertexCount"], 6);
        // Nothing runs any more: a second stop stops nothing.
        assert_eq!(stop(port, "page-a", &a)["stopped"], false);
        // The stopped conversation takes a new message; its history stays valid.
        assert_eq!(send(port, "page-a", &a, "hello").status, 200);
        let a_events = finished(port, "page-a", &a);
        assert_eq!(of(&a_events, "assistant").last().unwrap()["text"], "hello");
        let last = mock.requests().last().unwrap().clone();
        let roles: Vec<&str> = last["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["role"].as_str().unwrap())
            .collect();
        assert_eq!(
            roles,
            ["system", "user", "assistant", "tool", "user"],
            "{last}"
        );
    }
}

/// `graphersal --server 2>&1 | head`: the server starts with nobody reading its stderr (the
/// banner, the statistics and every later warning meet a closed pipe) and keeps serving.
#[test]
fn the_server_keeps_serving_with_a_closed_stderr() {
    let (reader, writer) = std::io::pipe().expect("a pipe");
    drop(reader);
    let mut child = Command::new(env!("CARGO_BIN_EXE_graphersal"))
        .args(["--graph", "modern", "--server", "--port", "0"])
        .stdout(Stdio::piped())
        .stderr(writer)
        .spawn()
        .expect("graphersal starts");
    let stdout = child.stdout.take().expect("stdout");
    let mut line = String::new();
    BufReader::new(stdout).read_line(&mut line).unwrap();
    let port = line
        .trim()
        .trim_end_matches('/')
        .rsplit(':')
        .next()
        .and_then(|p| p.parse().ok())
        .unwrap_or_else(|| panic!("no address on stdout: {line:?}"));
    let server = DevServer { child, port };

    let info = get(server.port, "/api/info").json();
    assert_eq!(info["server"], "graphersal-dev");
    let r = execute(server.port, "g.v().fail_here()");
    assert_eq!(r["ok"], false, "{r}");
    assert_eq!(get(server.port, "/api/info").status, 200);
}

#[test]
fn property_index_transforms_over_http_and_mcp() {
    let ui = ui_dir("index-transforms");
    let ui_arg = ui.to_str().unwrap().to_string();
    let server = start(&["--graph", "modern", "--ui-dir", &ui_arg, "--mcp"]);
    let port = server.port;
    // HTTP: the transform is stored, listed and names the default; a plain index beside it.
    let created = post(
        port,
        "/api/indexes/create",
        r#"{"label": "person", "key": "name", "transform": "to_lower"}"#,
    )
    .json();
    assert_eq!(created["ok"], true, "{created}");
    assert_eq!(
        created["index"]["name"], "person_name_to_lower",
        "{created}"
    );
    assert_eq!(created["index"]["transform"], "to_lower", "{created}");
    assert_eq!(created["index"]["stats"]["skipped"], 0, "{created}");
    let plain = post(
        port,
        "/api/indexes/create",
        r#"{"label": "person", "key": "name"}"#,
    )
    .json();
    assert_eq!(plain["index"]["transform"], Value::Null, "{plain}");
    let listed = get(port, "/api/indexes").json();
    assert_eq!(listed.as_array().unwrap().len(), 2, "{listed}");
    let refused = post(
        port,
        "/api/indexes/create",
        r#"{"label": "person", "key": "name", "transform": "upper"}"#,
    )
    .json();
    assert_eq!(refused["errors"][0]["field"], "transform", "{refused}");
    let found = execute(
        port,
        r#"g.V().hasLabel("person").has("name", __.to_lower().is("marko")).values("name").toList()"#,
    );
    assert_eq!(found["ok"], true, "{found}");

    // MCP: initialize -> tools/list (the transform enum) -> tools/call.
    let mut mcp = McpClient::connect(port, &[]);
    let tools = mcp.rpc("tools/list", serde_json::json!({}));
    let create = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "create_index")
        .unwrap()
        .clone();
    assert_eq!(
        create["inputSchema"]["properties"]["transform"]["enum"],
        serde_json::json!(["to_lower"]),
        "{create}"
    );
    let (error, text) = mcp.call(
        "create_index",
        serde_json::json!({"label": "software", "key": "name", "transform": "to_lower"}),
    );
    assert!(
        !error && text.contains("\"name\": \"software_name_to_lower\""),
        "{text}"
    );
    let (error, text) = mcp.call(
        "create_index",
        serde_json::json!({"label": "software", "key": "lang", "transform": "x"}),
    );
    assert!(error && text.contains("\"transform\""), "{text}");
    let (error, text) = mcp.call("list_indexes", serde_json::json!({}));
    assert!(
        !error && text.matches("\"transform\": \"to_lower\"").count() == 2,
        "{text}"
    );
}

/// `--embedder` with `--server`: the model is loaded at start, `GET /api/info` lists the
/// registry and the queries of the session call it (here a tiny synthetic Model2Vec model).
#[cfg(feature = "model2vec")]
#[test]
fn an_embedder_of_the_command_line_serves_the_session() {
    let dir = std::env::temp_dir().join(format!("graphersal_srv_{}_m2v", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("config.json"), r#"{"normalize": true}"#).unwrap();
    std::fs::write(
        dir.join("tokenizer.json"),
        r#"{"version": "1.0", "truncation": null, "padding": null, "added_tokens": [],
            "normalizer": null, "pre_tokenizer": {"type": "Whitespace"},
            "post_processor": null, "decoder": null,
            "model": {"type": "WordLevel", "vocab": {"[UNK]": 0, "lop": 1, "ripple": 2},
                      "unk_token": "[UNK]"}}"#,
    )
    .unwrap();
    let data: Vec<u8> = [0.0f32, 1.0, 1.0, 0.0, 0.5, 0.5]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    let mut header = format!(
        r#"{{"embeddings": {{"dtype": "F32", "shape": [3, 2], "data_offsets": [0, {}]}}}}"#,
        data.len()
    );
    while header.len() % 8 != 0 {
        header.push(' ');
    }
    let mut model = (header.len() as u64).to_le_bytes().to_vec();
    model.extend_from_slice(header.as_bytes());
    model.extend_from_slice(&data);
    std::fs::write(dir.join("model.safetensors"), model).unwrap();

    let spec = format!("tiny={}", dir.display());
    let server = start(&[
        "--graph",
        "modern",
        "--embedder",
        &spec,
        "--default-embedder",
        "tiny",
    ]);
    let port = server.port;
    let info = get(port, "/api/info").json();
    let embedders = info["embedders"].as_array().expect("embedders").clone();
    let tiny = embedders
        .iter()
        .find(|e| e["name"] == "tiny")
        .expect("tiny listed");
    assert_eq!(tiny["dimension"], 2);
    assert_eq!(tiny["default"], true);
    assert!(
        tiny["model"].as_str().unwrap().starts_with("model2vec@"),
        "{tiny}"
    );
    assert!(
        embedders
            .iter()
            .any(|e| e["name"] == "hash-ngram/1" && e["default"] == false)
    );

    let r = execute(
        port,
        r#"g.v().has_label("software").embed("name").to_list().len()"#,
    );
    assert_eq!(r["ok"], true, "{r}");
    drop(server);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Issue #12: `POST /api/compression/estimate` and the MCP tool `estimate_compression` answer
/// the candidates and change nothing; the tool is a reading one, offered and allowed in
/// read-only mode.
#[test]
fn a_compression_estimate_over_http_and_mcp_changes_nothing() {
    let ui = ui_dir("compression-estimate");
    let ui_arg = ui.to_str().unwrap().to_string();
    let server = start(&[
        "--graph",
        "modern",
        "--ui-dir",
        &ui_arg,
        "--mcp",
        "--mcp-read-only",
    ]);
    let port = server.port;
    let answer = post(
        port,
        "/api/compression/estimate",
        r#"{"label": "person", "path": "name"}"#,
    )
    .json();
    assert_eq!(answer["ok"], true, "{answer}");
    assert_eq!(answer["estimate"]["valuesFound"], 4);
    assert_eq!(
        answer["estimate"]["candidates"].as_array().unwrap().len(),
        10
    );
    assert_eq!(
        answer["estimate"]["warnings"][0]["code"],
        "nothing_qualifies"
    );
    let refused = post(
        port,
        "/api/compression/estimate",
        r#"{"label": "person", "path": "a[0]"}"#,
    )
    .json();
    assert_eq!(refused["errors"][0]["field"], "path", "{refused}");
    assert_eq!(get(port, "/api/compression/estimate").status, 405);

    let mut mcp = McpClient::connect(port, &[]);
    let tools = mcp.rpc("tools/list", serde_json::json!({}));
    let tool = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "estimate_compression")
        .cloned()
        .expect("listed in read-only mode");
    assert_eq!(tool["annotations"]["readOnlyHint"], true, "{tool}");
    let (error, text) = mcp.call(
        "estimate_compression",
        serde_json::json!({"label": "person", "path": "name", "min_bytes": 1}),
    );
    assert!(!error, "{text}");
    assert!(text.contains("not changed"), "{text}");
    assert!(text.contains("\"valuesFound\": 4"), "{text}");
    let (error, text) = mcp.call("estimate_compression", serde_json::json!({"path": "name"}));
    assert!(error, "{text}");
    assert!(text.contains("label"), "{text}");
    // The DSL function is allowed in read-only mode too: it reads data.
    let (error, text) = mcp.call(
        "query",
        serde_json::json!({"script": r#"g.estimate_compression(#{label: "person", path: "name"}).values_found"#}),
    );
    assert!(!error && text.contains('4'), "{text}");
    assert_eq!(changes(port)["version"], 0, "nothing was committed");
    assert_eq!(get(port, "/api/compression").json(), serde_json::json!([]));
}

/// GQL over HTTP runs on the dev server's session thread: the deepest nesting the parser accepts
/// answers, one level more is refused with GQLSTATUS 54001 (never a stack overflow), and the
/// rows keep the written column order.
#[test]
fn gql_runs_on_the_dev_server_session_thread() {
    let ui = ui_dir("basic");
    let server = start(&["--graph", "modern", "--ui-dir", ui.to_str().unwrap()]);
    let port = server.port;
    let nested = |n: usize| {
        format!(
            r#"g.gql("MATCH (p:person) WHERE {}p.age > 1{} RETURN p.name AS n, p.age AS a")"#,
            "(".repeat(n),
            ")".repeat(n)
        )
    };
    let body = serde_json::json!({ "script": nested(126), "options": {} });
    let answer = post(port, "/api/execute", &body.to_string());
    assert_eq!(answer.status, 200, "{}", answer.body);
    assert!(
        answer.body.contains(r#"{"n":"marko","a":29}"#),
        "{}",
        answer.body
    );
    let r = answer.json();
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(
        r["execution"]["results"].as_array().unwrap().len(),
        4,
        "{r}"
    );
    let r = execute(port, &nested(127));
    assert_eq!(r["ok"], false, "{r}");
    assert!(diagnostic(&r).contains("54001"), "{r}");
    let ors = (0..510)
        .map(|i| format!("p.age = {i}"))
        .collect::<Vec<_>>()
        .join(" OR ");
    let r = execute(
        port,
        &format!(r#"g.gql("MATCH (p:person) WHERE {ors} RETURN p.name AS n")"#),
    );
    assert_eq!(r["ok"], true, "{r}");
}

/// The MCP tool `gql`: rows as JSON objects in the written column order, parameters passed as
/// values (never spliced into the query), a GQL error with its GQLSTATUS and Help line, the
/// profile, offered and allowed in read-only mode; the examples resource has the GQL group.
#[test]
fn mcp_gql_tool_rows_params_errors_and_read_only() {
    let server = start(&["--graph", "modern", "--mcp", "--mcp-read-only"]);
    let port = server.port;
    let mut mcp = McpClient::connect(port, &[]);
    let tools = mcp.rpc("tools/list", serde_json::json!({}));
    let tool = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "gql")
        .cloned()
        .expect("listed in read-only mode");
    assert_eq!(tool["annotations"]["readOnlyHint"], true, "{tool}");

    // Rows in the written column order (not sorted: `who` before `friend` before `age`).
    let (error, text) = mcp.call(
        "gql",
        serde_json::json!({"query": "MATCH (p:person)-[:knows]->(f:person) RETURN p.name AS who, f.name AS friend, f.age AS age ORDER BY age DESC"}),
    );
    assert!(!error, "{text}");
    assert!(text.starts_with("ok: 2 results"), "{text}");
    let at = |key: &str| text.find(key).unwrap_or_else(|| panic!("{key}: {text}"));
    assert!(
        at("\"who\"") < at("\"friend\"") && at("\"friend\"") < at("\"age\""),
        "{text}"
    );
    assert!(at("\"josh\"") < at("\"vadas\""), "{text}");

    // Parameters are values: a quote in one never reaches the query text.
    let (error, text) = mcp.call(
        "gql",
        serde_json::json!({
            "query": "MATCH (p:person) WHERE p.age > $min AND p.name <> $odd\nRETURN p.name AS name ORDER BY name",
            "params": {"min": 30, "odd": "x' OR 'a' = 'a"},
        }),
    );
    assert!(!error, "{text}");
    assert!(text.starts_with("ok: 2 results"), "{text}");
    assert!(
        text.contains("josh") && text.contains("peter") && !text.contains("marko"),
        "{text}"
    );
    // As a JSON string too, like run_saved_query.
    let (error, text) = mcp.call(
        "gql",
        serde_json::json!({"query": "MATCH (p {name: $n}) RETURN p.age AS age", "params": "{\"n\": \"vadas\"}"}),
    );
    assert!(!error && text.contains("27"), "{text}");

    // max_rows bounds the rows.
    let (error, text) = mcp.call(
        "gql",
        serde_json::json!({"query": "MATCH (v) RETURN v.name AS name", "max_rows": 2}),
    );
    assert!(!error, "{text}");
    assert!(text.contains("note:"), "{text}");

    // A GQL error: GQLSTATUS and a Help line.
    let (error, text) = mcp.call(
        "gql",
        serde_json::json!({"query": "MATCH (p:person) RETRUN p.name"}),
    );
    assert!(error, "{text}");
    assert!(
        text.contains("42001") && text.contains("Help: Did you mean RETURN?"),
        "{text}"
    );
    let (error, text) = mcp.call(
        "gql",
        serde_json::json!({"query": "MATCH (p) RETURN p EXCEPT MATCH (q) RETURN q"}),
    );
    assert!(error && text.contains("not yet supported"), "{text}");
    let (error, text) = mcp.call("gql", serde_json::json!({"params": {}}));
    assert!(
        error && text.contains("missing argument \"query\""),
        "{text}"
    );
    let (error, text) = mcp.call(
        "gql",
        serde_json::json!({"query": "MATCH (p) RETURN p", "params": [1]}),
    );
    assert!(error && text.contains("JSON object"), "{text}");

    // The profile: the Gremlin plan the query runs as.
    let (error, text) = mcp.call(
        "gql",
        serde_json::json!({"query": "MATCH (p:person) WHERE p.age > 30 RETURN p.name AS name", "profile": true}),
    );
    assert!(!error && text.contains("gql:project"), "{text}");
    assert_eq!(changes(port)["version"], 0, "nothing was committed");

    // The embedded playground's examples carry the GQL group.
    let answer = mcp.rpc(
        "resources/read",
        serde_json::json!({"uri": "graphersal://examples"}),
    );
    let examples = answer["result"]["contents"][0]["text"].as_str().unwrap();
    assert!(examples.contains("g.gql("), "{examples}");
}

/// The gql tool writes by default (W41): an INSERT commits, the answer names the commit and the
/// change summary, and a failing write changes nothing; under `--mcp-read-only` the same write
/// is refused before it runs.
#[test]
fn mcp_gql_tool_writes_unless_read_only() {
    let server = start(&["--graph", "modern", "--mcp"]);
    let port = server.port;
    let mut mcp = McpClient::connect(port, &[]);
    let tools = mcp.rpc("tools/list", serde_json::json!({}));
    let tool = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "gql")
        .cloned()
        .expect("listed");
    assert_eq!(tool["annotations"]["readOnlyHint"], false, "{tool}");
    assert_eq!(tool["annotations"]["destructiveHint"], true, "{tool}");

    let (error, text) = mcp.call(
        "gql",
        serde_json::json!({
            "query": "MATCH (p:person {name: $who}) INSERT (p)-[:likes]->(f:fruit {name: 'apple'}) RETURN f.name AS fruit",
            "params": {"who": "marko"},
        }),
    );
    assert!(!error, "{text}");
    assert!(
        text.starts_with("ok: 1 result")
            && text.contains("changed the data, committed as commit 1 (+1 vertex, +1 edge)")
            && text.contains("apple"),
        "{text}"
    );
    let (error, text) = mcp.call(
        "gql",
        serde_json::json!({"query": "INSERT (:tag {n: 1}), (:tag {n: 2})"}),
    );
    assert!(!error, "{text}");
    assert!(text.contains("(+2 vertices)"), "{text}");
    // The last row fails: nothing of the query is kept.
    let (error, text) = mcp.call(
        "gql",
        serde_json::json!({"query": "FOR x IN [1, 0] INSERT (:tag {n: 1 / x})"}),
    );
    assert!(error && text.contains("the graph is unchanged"), "{text}");
    let (error, text) = mcp.call(
        "gql",
        serde_json::json!({"query": "MATCH (t:tag) RETURN count(t) AS tags"}),
    );
    assert!(!error && text.contains("2"), "{text}");
    assert_eq!(changes(port)["version"], 2, "two commits");
    drop(server);

    let server = start(&["--graph", "modern", "--mcp", "--mcp-read-only"]);
    let port = server.port;
    let mut mcp = McpClient::connect(port, &[]);
    let (error, text) = mcp.call("gql", serde_json::json!({"query": "INSERT (:tag {n: 1})"}));
    assert!(
        error && text.contains("Permission denied for 'gql:insert_node'"),
        "{text}"
    );
    let (error, text) = mcp.call(
        "gql",
        serde_json::json!({"query": "MATCH (t:tag) RETURN count(t) AS tags"}),
    );
    assert!(!error && text.contains("\"tags\": 0"), "{text}");
    assert_eq!(changes(port)["version"], 0, "nothing was committed");
}
