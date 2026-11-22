//! Session route tags (`Host::session_route`, `session_route`, `Mcp::verified_session_route`):
//! a host pins an MCP session to the graph it resolved at `initialize`, in table mode and with
//! signed ids; JSON through `Mcp::handle`, without HTTP.

use std::sync::Mutex;

use graphersal_mcp::{
    ClientRef, Executor, Host, Job, MAX_SESSION_ROUTE_BYTES, Mcp, McpError, McpRequest,
    McpResponse, SessionKey, is_valid_session_route, session_route,
};
use graphersal_session::Session;
use serde_json::{Value, json};

/// A host that runs every job on a fresh modern-graph session and answers the route tag it
/// is told.
#[derive(Default)]
struct RouteHost {
    route: Mutex<Option<String>>,
}

impl RouteHost {
    fn routed(route: &str) -> Self {
        Self {
            route: Mutex::new(Some(route.to_string())),
        }
    }
}

impl Executor for RouteHost {
    fn execute(&self, _client: &ClientRef, job: Job) -> Result<(), McpError> {
        let mut session = Session::new();
        session.load_sample("modern").expect("modern");
        job(&mut session);
        Ok(())
    }
}

impl Host for RouteHost {
    fn view(&self) -> Value {
        Value::Null
    }
    fn add_process_statistics(&self, statistics: Value) -> Value {
        statistics
    }
    fn add_graph_info(&self, _info: &mut Value) -> Result<(), McpError> {
        Ok(())
    }
    fn examples_json(&self) -> Option<Vec<u8>> {
        None
    }
    fn session_route(&self) -> Option<String> {
        self.route.lock().expect("route").clone()
    }
}

fn post(mcp: &Mcp, host: &RouteHost, body: &Value, session: Option<&str>) -> McpResponse {
    let text = body.to_string();
    let mut request = McpRequest::new("POST", text.as_bytes())
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream");
    if let Some(session) = session {
        request = request.header("Mcp-Session-Id", session);
    }
    mcp.handle(host, request)
}

fn initialize_answer(mcp: &Mcp, host: &RouteHost) -> McpResponse {
    post(
        mcp,
        host,
        &json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": {"name": "tester", "version": "1"}}}),
        None,
    )
}

fn initialize(mcp: &Mcp, host: &RouteHost) -> String {
    let response = initialize_answer(mcp, host);
    assert_eq!(response.status(), 200);
    response
        .header("mcp-session-id")
        .expect("an id")
        .to_string()
}

fn ping(mcp: &Mcp, host: &RouteHost, session: &str) -> u16 {
    post(
        mcp,
        host,
        &json!({"jsonrpc": "2.0", "id": 2, "method": "ping"}),
        Some(session),
    )
    .status()
}

fn key() -> SessionKey {
    SessionKey::new(b"a shared secret of the replicas, 32+ bytes").expect("a valid key")
}

/// Both session modes: the table (default) and signed ids.
fn endpoints() -> Vec<(&'static str, Mcp)> {
    vec![
        ("table", Mcp::new(true, None, false)),
        (
            "signed",
            Mcp::new(true, None, false).with_session_key(key()),
        ),
    ]
}

/// The id with one character of its route changed (`acme@2026.3` -> `acme@2026.4`).
fn moved(id: &str) -> String {
    id.replacen("acme@2026.3~", "acme@2026.4~", 1)
}

#[test]
fn a_route_round_trips_in_both_modes() {
    for (mode, mcp) in endpoints() {
        let host = RouteHost::routed("acme@2026.3");
        let id = initialize(&mcp, &host);
        assert!(id.starts_with("acme@2026.3~"), "{mode}: {id}");
        assert_eq!(session_route(&id), Some("acme@2026.3"), "{mode}");
        assert_eq!(
            mcp.verified_session_route(&id),
            Some("acme@2026.3"),
            "{mode}"
        );
        assert_eq!(ping(&mcp, &host, &id), 200, "{mode}");
        // The host's route changes (an alias moved): the open session keeps its own.
        *host.route.lock().unwrap() = Some("acme@2026.4".into());
        assert_eq!(ping(&mcp, &host, &id), 200, "{mode}");
        let newer = initialize(&mcp, &host);
        assert_eq!(session_route(&newer), Some("acme@2026.4"), "{mode}");
        assert_eq!(session_route(&id), Some("acme@2026.3"), "{mode}");
        // A tool call carries the routed id as the client key, as any id.
        let call = json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {"name": "query", "arguments": {"script": "g.v().count()"}}});
        assert_eq!(post(&mcp, &host, &call, Some(&id)).status(), 200, "{mode}");
    }
}

#[test]
fn a_changed_route_is_an_unknown_session() {
    for (mode, mcp) in endpoints() {
        let host = RouteHost::routed("acme@2026.3");
        let id = initialize(&mcp, &host);
        let forged = moved(&id);
        assert_ne!(forged, id);
        // The decoder only reads; the endpoint refuses the forged id.
        assert_eq!(session_route(&forged), Some("acme@2026.4"));
        assert_eq!(mcp.verified_session_route(&forged), None, "{mode}");
        assert_eq!(ping(&mcp, &host, &forged), 404, "{mode}");
        // Stripping the route or adding one to an unrouted id is refused as well.
        let stripped = id.split_once('~').unwrap().1;
        assert_eq!(ping(&mcp, &host, stripped), 404, "{mode}");
        let plain = initialize(&mcp, &RouteHost::default());
        assert_eq!(
            ping(&mcp, &host, &format!("acme@2026.3~{plain}")),
            404,
            "{mode}"
        );
        assert_eq!(ping(&mcp, &host, &id), 200, "{mode}");
    }
}

#[test]
fn a_signed_route_verifies_on_every_replica_with_the_key() {
    let host = RouteHost::routed("acme@2026.3");
    let one = Mcp::new(true, None, false).with_session_key(key());
    let two = Mcp::new(true, None, false).with_session_key(key());
    let other = Mcp::new(true, None, false).with_session_key(SessionKey::random());
    let id = initialize(&one, &host);
    assert_eq!(ping(&two, &host, &id), 200);
    assert_eq!(two.verified_session_route(&id), Some("acme@2026.3"));
    assert_eq!(ping(&other, &host, &id), 404);
    assert_eq!(other.verified_session_route(&id), None);
    assert_eq!(ping(&two, &host, &moved(&id)), 404);
}

#[test]
fn without_a_route_the_id_is_unchanged() {
    let host = RouteHost::default();
    let table = initialize(&Mcp::new(true, None, false), &host);
    assert_eq!(table.len(), 32, "{table}");
    assert!(table.bytes().all(|b| b.is_ascii_hexdigit()), "{table}");
    let mcp = Mcp::new(true, None, false).with_session_key(key());
    let signed = initialize(&mcp, &host);
    assert_eq!(signed.len(), 80, "{signed}");
    assert!(signed.bytes().all(|b| b.is_ascii_hexdigit()), "{signed}");
    assert_eq!(session_route(&table), None);
    assert_eq!(session_route(&signed), None);
    assert_eq!(mcp.verified_session_route(&signed), None);
}

#[test]
fn an_invalid_route_refuses_the_session() {
    let longest = "r".repeat(MAX_SESSION_ROUTE_BYTES);
    assert!(is_valid_session_route(&longest));
    assert!(is_valid_session_route("acme/2026.3"));
    assert!(is_valid_session_route("t0k3n_base64url-AZ=+:@"));
    for bad in [
        String::new(),
        "r".repeat(MAX_SESSION_ROUTE_BYTES + 1),
        "acme~1".into(),
        "acme 1".into(),
        "acme\r\nX-Injected: 1".into(),
        "acmé".into(),
    ] {
        assert!(!is_valid_session_route(&bad), "{bad:?}");
        for (mode, mcp) in endpoints() {
            let response = initialize_answer(&mcp, &RouteHost::routed(&bad));
            assert_eq!(response.header("mcp-session-id"), None, "{mode}: {bad:?}");
            let body: Value = serde_json::from_slice(response.body()).unwrap();
            let message = body["error"]["message"].as_str().expect("an error");
            assert!(message.contains("session route tag"), "{message}");
            assert!(
                bad.is_empty() || !message.contains(&bad),
                "the tag itself is not echoed: {message}"
            );
        }
    }
    let ok = RouteHost::routed(&longest);
    let id = initialize(&Mcp::new(true, None, false).with_session_key(key()), &ok);
    assert_eq!(session_route(&id), Some(longest.as_str()));
    // Text that is no routed id has no route.
    for text in ["", "~", "~abc", "a b~abc", "abc"] {
        assert_eq!(session_route(text), None, "{text:?}");
    }
}

/// A stream connection is its own session: the host is not asked for a route.
#[test]
fn a_stream_connection_is_not_routed() {
    use std::sync::Arc;
    let mcp = Arc::new(Mcp::new(true, None, false));
    let host: Arc<dyn Host> = Arc::new(RouteHost::routed("a b c, invalid"));
    let connection = graphersal_mcp::Connection::new(mcp, host);
    let lines = connection.handle(
        &json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": {"name": "tester", "version": "1"}}})
        .to_string(),
    );
    assert!(lines.iter().any(|l| l.contains("\"result\"")), "{lines:?}");
}
