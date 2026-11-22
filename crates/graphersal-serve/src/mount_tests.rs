//! Mounting a context under a URL prefix (`ContextBuilder::mount_path`, plan
//! `graphersal-server` H3, decision Q3) and the streaming upload helper
//! (`Request::copy_body_to`, decision Q5).
//!
//! The test host serves two contexts under `/db/a/` and `/db/b/` the way a multi-graph host
//! does: its listener strips `/db/<name>` from the request target, drops a client's own
//! `X-Graphersal-Graph` and sets it to the name; its graph resolver picks the context by that
//! header. The core never sees the prefix. Real session threads on the `modern` sample, no
//! HTTP library.

use std::collections::BTreeMap;
use std::io::{self, Cursor, Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use graphersal_session::SessionPresets;
use serde_json::{Value, json};

use crate::{
    Context, Exposure, GraphResolver, HostCheck, HttpError, MAX_BODY_BYTES, Method, Request,
    Response, Runner, Server, spawn_session,
};

const HOST: &str = "127.0.0.1:8080";
const GRAPH_HEADER: &str = "X-Graphersal-Graph";
const HINT: &str = "use http://127.0.0.1:8080/ or add the name to allowed_hosts in server.toml";

/// The listener's checks, shared by every mount, with the host's own advice.
fn check() -> HostCheck {
    HostCheck::new(Exposure::Loopback, 8080, &format!("http://{HOST}/")).with_hint(HINT)
}

/// A session thread with the `modern` sample loaded.
fn modern() -> Runner {
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

/// The test host's resolver: the context named by [`GRAPH_HEADER`], which the host's listener
/// set (a missing or unknown name is a `404` of the host).
struct Mounts {
    contexts: BTreeMap<String, Arc<Context>>,
}

impl GraphResolver for Mounts {
    fn resolve(&self, request: &Request<'_>) -> Result<Arc<Context>, HttpError> {
        let name = request.header(GRAPH_HEADER).unwrap_or_default();
        self.contexts
            .get(name)
            .cloned()
            .ok_or_else(|| HttpError::new(404, format!("no graph {name}")))
    }
}

/// Two contexts mounted at `/db/a` and `/db/b`, each built by `build` from its builder.
fn two_mounts(
    build: impl Fn(crate::ContextBuilder) -> crate::ContextBuilder,
) -> (Server, Arc<Context>, Arc<Context>) {
    let a = build(Context::builder(modern(), check()).mount_path(Some("/db/a".into()))).build();
    let b = build(Context::builder(modern(), check()).mount_path(Some("/db/b/".into()))).build();
    let server = Server::new();
    server
        .ready_with(Arc::new(Mounts {
            contexts: BTreeMap::from([("a".to_string(), a.clone()), ("b".to_string(), b.clone())]),
        }))
        .expect("ready once");
    (server, a, b)
}

/// The host's listener side of one client request: `/db/<name>/<rest>` becomes `/<rest>` with
/// `name` in [`GRAPH_HEADER`] (the client's copy dropped); any other path is the host's 404
/// (its own redirect `/db/x` -> `/db/x/`, landing page and admin routes are out of scope).
fn host(
    server: &Server,
    method: Method,
    url: &str,
    body: &[u8],
    client_headers: &[(&str, &str)],
) -> Response {
    let Some((name, rest)) = url
        .strip_prefix("/db/")
        .and_then(|tail| tail.split_once('/'))
    else {
        return Response::text(404, json!({"error": "no such page"}).to_string());
    };
    let mut request = Request::new(method, &format!("/{rest}"), Cursor::new(body))
        .with_body_length(Some(body.len()));
    let mut has_host = false;
    for (field, value) in client_headers {
        has_host |= field.eq_ignore_ascii_case("Host");
        if !field.eq_ignore_ascii_case(GRAPH_HEADER) {
            request = request.with_header(field, value);
        }
    }
    if !has_host {
        request = request.with_header("Host", HOST);
    }
    server.handle(request.with_header(GRAPH_HEADER, name))
}

fn get(server: &Server, url: &str) -> Response {
    host(server, Method::Get, url, b"", &[])
}

fn post_json(server: &Server, url: &str, body: &Value) -> Response {
    host(
        server,
        Method::Post,
        url,
        body.to_string().as_bytes(),
        &[("Content-Type", "application/json")],
    )
}

fn body_json(response: &Response) -> Value {
    serde_json::from_slice(response.body()).unwrap_or(Value::Null)
}

fn header<'r>(response: &'r Response, name: &str) -> Option<&'r str> {
    response
        .headers()
        .iter()
        .find(|(field, _)| field.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

/// The number of vertices of the mount `name`, through `POST /db/<name>/api/execute`.
fn vertex_count(server: &Server, name: &str) -> Value {
    let answer = post_json(
        server,
        &format!("/db/{name}/api/execute"),
        &json!({"script": "g.v().count().next()"}),
    );
    assert_eq!(
        answer.status(),
        200,
        "{}",
        String::from_utf8_lossy(answer.body())
    );
    body_json(&answer)["execution"]["results"][0].clone()
}

#[test]
fn two_mounts_serve_their_own_graph_without_cross_talk() {
    let (server, _a, _b) = two_mounts(|builder| builder);
    assert_eq!(vertex_count(&server, "a"), 6);
    assert_eq!(vertex_count(&server, "b"), 6);
    // A write in a is not seen in b.
    let write = post_json(
        &server,
        "/db/a/api/execute",
        &json!({"script": "g.add_v(\"x\").iterate()"}),
    );
    assert_eq!(
        write.status(),
        200,
        "{}",
        String::from_utf8_lossy(write.body())
    );
    assert_eq!(vertex_count(&server, "a"), 7);
    assert_eq!(vertex_count(&server, "b"), 6);
    // A client cannot steer the request to the other graph with its own header: the host
    // drops it and sets the name of the path.
    let answer = host(
        &server,
        Method::Post,
        "/db/b/api/execute",
        br#"{"script": "g.v().count().next()"}"#,
        &[("Content-Type", "application/json"), (GRAPH_HEADER, "a")],
    );
    assert_eq!(body_json(&answer)["execution"]["results"][0], 6);
}

#[test]
fn api_info_names_each_mount() {
    let (server, a, b) = two_mounts(|builder| builder);
    assert_eq!(a.mount_path(), Some("/db/a"));
    assert_eq!(b.mount_path(), Some("/db/b"), "the trailing / is dropped");
    for name in ["a", "b"] {
        let info = body_json(&get(&server, &format!("/db/{name}/api/info")));
        assert_eq!(info["mountPath"], format!("/db/{name}"), "{info}");
        assert_eq!(
            info["openapi"],
            format!("/db/{name}/openapi.json"),
            "{info}"
        );
        // The OpenAPI description is served under the mount too.
        let spec = get(&server, &format!("/db/{name}/openapi.json"));
        assert_eq!(spec.status(), 200);
        assert!(body_json(&spec)["paths"].is_object());
    }
}

#[test]
fn a_context_at_the_root_answers_as_before() {
    let server = Server::new();
    let context = Context::builder(modern(), check()).build();
    assert_eq!(context.mount_path(), None);
    server.ready(context).expect("ready once");
    let request =
        Request::new(Method::Get, "/api/info", Cursor::new(&b""[..])).with_header("Host", HOST);
    let info = body_json(&server.handle(request));
    assert!(info.get("mountPath").is_none(), "{info}");
    assert_eq!(info["openapi"], "/openapi.json");
}

#[test]
fn a_mount_path_is_normalised() {
    let mount = |path: Option<&str>| {
        Context::builder(modern(), check())
            .mount_path(path.map(str::to_string))
            .build()
            .mount_path()
            .map(str::to_string)
    };
    assert_eq!(mount(Some("db/x/")), Some("/db/x".to_string()));
    assert_eq!(mount(Some("//db/x//")), Some("/db/x".to_string()));
    assert_eq!(mount(Some("/db/x")), Some("/db/x".to_string()));
    assert_eq!(mount(Some("/")), None);
    assert_eq!(mount(Some("")), None);
    assert_eq!(mount(None), None);
}

#[test]
fn the_host_checks_run_on_every_mount_with_the_hosts_hint() {
    let (server, _a, _b) = two_mounts(|builder| builder);
    for name in ["a", "b"] {
        let url = format!("/db/{name}/api/info");
        let foreign_origin = host(
            &server,
            Method::Get,
            &url,
            b"",
            &[("Origin", "http://evil.example")],
        );
        assert_eq!(foreign_origin.status(), 403);
        assert_eq!(
            body_json(&foreign_origin)["error"],
            format!("cross-origin request from http://evil.example refused ({HINT})")
        );
        let foreign_host = host(
            &server,
            Method::Get,
            &url,
            b"",
            &[("Host", "evil.example:8080")],
        );
        assert_eq!(foreign_host.status(), 403);
        assert_eq!(
            body_json(&foreign_host)["error"],
            format!("unknown Host: {HINT}")
        );
        // A write is refused as well, before it reaches the session.
        let write = host(
            &server,
            Method::Post,
            &format!("/db/{name}/api/execute"),
            br#"{"script": "g.add_v(\"x\").iterate()"}"#,
            &[
                ("Content-Type", "application/json"),
                ("Origin", "http://evil.example"),
            ],
        );
        assert_eq!(write.status(), 403);
        // The page's own origin is accepted.
        let own = host(
            &server,
            Method::Get,
            &url,
            b"",
            &[("Origin", "http://127.0.0.1:8080")],
        );
        assert_eq!(own.status(), 200);
    }
    assert_eq!(vertex_count(&server, "a"), 6);
    assert_eq!(vertex_count(&server, "b"), 6);
}

#[test]
fn without_a_hint_the_host_checks_keep_the_cli_texts() {
    let check = HostCheck::new(Exposure::Loopback, 8080, &format!("http://{HOST}/"));
    let foreign = Request::new(Method::Get, "/api/info", Cursor::new(&b""[..]))
        .with_header("Host", "evil.example:8080");
    let err = check.check(&foreign).expect_err("refused");
    assert_eq!(
        err.message(),
        "unknown Host: use http://127.0.0.1:8080/ (a public name behind a reverse proxy: start \
         the server with --allowed-host <that name>)"
    );
    let cross = Request::new(Method::Get, "/api/info", Cursor::new(&b""[..]))
        .with_header("Host", HOST)
        .with_header("Origin", "http://evil.example");
    let err = check.check(&cross).expect_err("refused");
    assert_eq!(
        err.message(),
        "cross-origin request from http://evil.example refused"
    );
    // The network exposure takes the hint too.
    let network = HostCheck::new(Exposure::Network, 8080, "http://10.0.0.1:8080/").with_hint("x");
    let foreign = Request::new(Method::Get, "/", Cursor::new(&b""[..]))
        .with_header("Host", "evil.example.com");
    assert_eq!(
        network.check(&foreign).expect_err("refused").message(),
        "unknown Host: x"
    );
}

#[cfg(feature = "ui-assets")]
#[test]
fn the_ui_is_served_under_each_mount_with_relative_urls() {
    let (server, _a, _b) = two_mounts(|builder| builder);
    for name in ["a", "b"] {
        let page = get(&server, &format!("/db/{name}/"));
        assert_eq!(page.status(), 200);
        assert_eq!(
            header(&page, "Content-Type"),
            Some("text/html; charset=utf-8")
        );
        assert!(header(&page, "Location").is_none());
        let html = String::from_utf8_lossy(page.body());
        // Every URL of the page is relative, so it resolves below the mount.
        for absolute in ["src=\"/", "href=\"/", "action=\"/"] {
            assert!(!html.contains(absolute), "{absolute} in index.html");
        }
        let asset = get(&server, &format!("/db/{name}/js/engine/http-engine.js"));
        assert_eq!(asset.status(), 200);
        assert_eq!(
            header(&asset, "Content-Type"),
            Some("text/javascript; charset=utf-8")
        );
        let reset = get(&server, &format!("/db/{name}/reset/"));
        assert_eq!(reset.status(), 200);
        let missing = get(&server, &format!("/db/{name}/no-such-file.js"));
        assert_eq!(missing.status(), 404);
        assert_eq!(body_json(&missing)["error"], "not found: /no-such-file.js");
    }
}

#[cfg(feature = "mcp")]
#[test]
fn the_mcp_endpoint_is_named_and_served_under_each_mount() {
    let mcp = Arc::new(graphersal_mcp::Mcp::new(false, None, false));
    let (server, _a, _b) = two_mounts(|builder| builder.shared_mcp(mcp.clone()));
    // A write in a, through its endpoint under the mount.
    let call = |name: &str, query: &str| -> Value {
        let body = json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {"name": "gql", "arguments": {"query": query}}});
        let answer = host(
            &server,
            Method::Post,
            &format!("/db/{name}/mcp"),
            body.to_string().as_bytes(),
            &[
                ("Content-Type", "application/json"),
                ("Accept", "application/json"),
            ],
        );
        assert_eq!(
            answer.status(),
            200,
            "{}",
            String::from_utf8_lossy(answer.body())
        );
        body_json(&answer)
    };
    let created = call("a", "INSERT (:x)");
    assert_eq!(created["result"]["isError"], false, "{created}");
    for (name, count) in [("a", 7), ("b", 6)] {
        let info = body_json(&get(&server, &format!("/db/{name}/api/info")));
        assert_eq!(info["mcp"]["endpoint"], format!("/db/{name}/mcp"), "{info}");
        let changes = body_json(&get(&server, &format!("/db/{name}/api/changes")));
        assert_eq!(
            changes["mcp"]["endpoint"],
            format!("/db/{name}/mcp"),
            "{changes}"
        );
        let answer = call(name, "MATCH (n) RETURN count(n) AS c");
        let text = answer["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(text.contains(&format!("\"c\":{count}")), "{name}: {text}");
    }
    // At the root the endpoint stays /mcp.
    let root = Context::builder(modern(), check())
        .shared_mcp(mcp.clone())
        .build();
    let root_server = Server::new();
    root_server.ready(root).expect("ready once");
    let request =
        Request::new(Method::Get, "/api/info", Cursor::new(&b""[..])).with_header("Host", HOST);
    assert_eq!(
        body_json(&root_server.handle(request))["mcp"]["endpoint"],
        "/mcp"
    );
}

// --- Request::copy_body_to ---

/// A sink that counts the bytes it is given and keeps none.
#[derive(Default)]
struct Counting {
    bytes: u64,
}

impl Write for Counting {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.bytes += buf.len() as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A body that counts how many bytes were read from it.
struct Watched<R> {
    inner: R,
    read: Arc<AtomicU64>,
}

impl<R: Read> Read for Watched<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buf)?;
        self.read.fetch_add(read as u64, Ordering::SeqCst);
        Ok(read)
    }
}

fn upload<'a>(body: impl Read + 'a, declared: Option<usize>) -> Request<'a> {
    Request::new(Method::Post, "/api/upload", body)
        .with_header("Host", HOST)
        .with_body_length(declared)
}

#[test]
fn a_body_larger_than_the_json_limit_is_copied_with_a_large_max() {
    let size = MAX_BODY_BYTES as u64 + 3 * 1024 * 1024 + 7;
    // Declared.
    let mut request = upload(io::repeat(7).take(size), Some(size as usize));
    let mut sink = Counting::default();
    assert_eq!(request.copy_body_to(&mut sink, 64 << 20), Ok(size));
    assert_eq!(sink.bytes, size);
    // Undeclared (chunked), max exactly the size.
    let mut request = upload(io::repeat(7).take(size), None);
    let mut sink = Counting::default();
    assert_eq!(request.copy_body_to(&mut sink, size), Ok(size));
    assert_eq!(sink.bytes, size);
    // The bytes arrive as sent.
    let data: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    let mut request = upload(Cursor::new(data.clone()), Some(data.len()));
    let mut out = Vec::new();
    assert_eq!(
        request.copy_body_to(&mut out, data.len() as u64),
        Ok(200_000)
    );
    assert_eq!(out, data);
}

#[test]
fn a_declared_body_above_max_is_refused_before_reading() {
    let read = Arc::new(AtomicU64::new(0));
    let body = Watched {
        inner: io::repeat(1).take(10_000),
        read: read.clone(),
    };
    let mut request = upload(body, Some(10_000));
    let mut sink = Counting::default();
    let err = request.copy_body_to(&mut sink, 9_999).expect_err("refused");
    assert_eq!(err.status(), 413);
    assert_eq!(err.message(), "the body is larger than 9999 bytes");
    assert_eq!(read.load(Ordering::SeqCst), 0, "nothing read");
    assert_eq!(sink.bytes, 0);
}

#[test]
fn an_undeclared_body_passing_max_is_refused_while_reading() {
    let max = 100_000u64;
    let mut request = upload(io::repeat(1).take(5 * max), None);
    let mut out = Vec::new();
    let err = request.copy_body_to(&mut out, max).expect_err("refused");
    assert_eq!(err.status(), 413);
    assert_eq!(err.message(), "the body is larger than 100000 bytes");
    assert!(out.len() as u64 <= max, "never more than max written");
    // A declared length smaller than the body sent: the read still stops at max.
    let mut request = upload(io::repeat(1).take(5 * max), Some(10));
    let mut sink = Counting::default();
    let err = request.copy_body_to(&mut sink, max).expect_err("refused");
    assert_eq!(err.status(), 413);
    assert!(sink.bytes <= max);
}

#[test]
fn read_and_write_failures_of_an_upload_are_400_and_500() {
    struct Broken;
    impl Read for Broken {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("connection reset"))
        }
    }
    struct Full;
    impl Write for Full {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("disk full"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let err = upload(Broken, None)
        .copy_body_to(&mut Counting::default(), 10)
        .expect_err("refused");
    assert_eq!(err.status(), 400);
    assert_eq!(err.message(), "cannot read the body: connection reset");
    let err = upload(Cursor::new(&b"abc"[..]), Some(3))
        .copy_body_to(&mut Full, 10)
        .expect_err("refused");
    assert_eq!(err.status(), 500);
    assert_eq!(err.message(), "cannot store the body: disk full");
    // An empty body copies nothing.
    let mut out = Vec::new();
    assert_eq!(
        upload(io::empty(), Some(0)).copy_body_to(&mut out, 0),
        Ok(0)
    );
}
