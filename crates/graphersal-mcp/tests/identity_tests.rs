//! MCP sessions bound to the identity of their `initialize` (`McpRequest::with_identity`; none
//! = anonymous), in table and signed mode, single-graph and directory mode; and a directory the
//! host builds per request for the caller (its graphs, hidden graphs, its profiles).

use std::collections::BTreeMap;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};

use graphersal_mcp::{
    ClientRef, Executor, GraphDirectory, GraphEntry, GraphState, Host, Job, Mcp, McpError,
    McpRequest, McpResponse, Profile, SessionKey,
};
use graphersal_session::Session;
use serde_json::{Value, json};

/// A host over one session on its own thread (the modern graph), with an optional route tag.
struct TestHost {
    jobs: Mutex<Sender<Job>>,
    route: Option<String>,
}

impl TestHost {
    fn modern(route: Option<&str>) -> Arc<Self> {
        let (jobs, queue) = channel::<Job>();
        std::thread::Builder::new()
            .stack_size(64 * 1024 * 1024)
            .spawn(move || {
                let mut session = Session::new();
                session
                    .load_sample("modern")
                    .expect("the modern graph loads");
                for job in queue {
                    job(&mut session);
                }
            })
            .expect("the session thread starts");
        Arc::new(Self {
            jobs: Mutex::new(jobs),
            route: route.map(str::to_string),
        })
    }
}

impl Executor for TestHost {
    fn execute(&self, _client: &ClientRef, job: Job) -> Result<(), McpError> {
        let (done, wait) = channel();
        self.jobs
            .lock()
            .expect("jobs")
            .send(Box::new(move |session| {
                job(session);
                let _ = done.send(());
            }))
            .map_err(|_| McpError::Unavailable("the session is not running".into()))?;
        let _ = wait.recv();
        Ok(())
    }
}

impl Host for TestHost {
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
        self.route.clone()
    }
}

/// A `POST` (or another method) with an optional session id and identity.
fn request(
    method: &str,
    body: &Value,
    session: Option<&str>,
    identity: Option<&str>,
) -> McpRequest<'static> {
    let text = std::io::Cursor::new(body.to_string().into_bytes());
    let mut request = McpRequest::new(method, text)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream");
    if let Some(id) = session {
        request = request
            .header("Mcp-Session-Id", id)
            .header("MCP-Protocol-Version", "2025-06-18");
    }
    match identity {
        Some(identity) => request.with_identity(identity),
        None => request,
    }
}

fn initialize_body() -> Value {
    json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {},
        "clientInfo": {"name": "tester", "version": "1"}}})
}

fn ping_body() -> Value {
    json!({"jsonrpc": "2.0", "id": 2, "method": "ping"})
}

fn session_of(response: &McpResponse) -> String {
    assert_eq!(response.status(), 200);
    response
        .header("mcp-session-id")
        .expect("a session id")
        .to_string()
}

fn json_of(response: &McpResponse) -> Value {
    serde_json::from_slice(response.body()).expect("a JSON body")
}

/// The identities a borrowed session is tried with: everyone but its owner.
const IDENTITIES: [Option<&str>; 3] = [None, Some("alice"), Some("bob")];

/// Every identity but `owner` gets the 404 of an unknown session (same status, same body) on
/// `ping` and on `DELETE`, which leaves the session alive; the owner's `ping` still works.
fn assert_not_borrowable(
    owner: Option<&str>,
    session: &str,
    send: &dyn Fn(McpRequest<'static>) -> McpResponse,
) {
    let unknown = send(request("POST", &ping_body(), Some("0123abcd"), owner));
    assert_eq!(unknown.status(), 404);
    for other in IDENTITIES.into_iter().filter(|other| *other != owner) {
        let borrowed = send(request("POST", &ping_body(), Some(session), other));
        assert_eq!(borrowed.status(), 404, "{owner:?} borrowed by {other:?}");
        assert_eq!(borrowed.body(), unknown.body(), "{owner:?} by {other:?}");
        let deleted = send(request("DELETE", &json!({}), Some(session), other));
        assert_eq!(deleted.status(), 404, "{owner:?} deleted by {other:?}");
    }
    let own = send(request("POST", &ping_body(), Some(session), owner));
    assert_eq!(own.status(), 200, "{owner:?}");
}

#[test]
fn a_single_graph_session_cannot_be_borrowed_in_table_or_signed_mode() {
    let host = TestHost::modern(None);
    let key = SessionKey::new(&[3u8; 32]).unwrap();
    for (mode, mcp) in [
        ("table", Mcp::new(false, None, false)),
        ("signed", Mcp::new(false, None, false).with_session_key(key)),
    ] {
        let send = |request: McpRequest<'static>| mcp.handle(host.as_ref(), request);
        for owner in IDENTITIES {
            let session = session_of(&send(request("POST", &initialize_body(), None, owner)));
            assert_not_borrowable(owner, &session, &send);
            // The owner can end it (a signed id stays valid: nothing is stored).
            let deleted = send(request("DELETE", &json!({}), Some(&session), owner));
            assert_eq!(deleted.status(), 200, "{mode} {owner:?}");
        }
    }
}

#[test]
fn a_verified_route_is_the_owners_only() {
    let host = TestHost::modern(Some("acme@2026.3"));
    let key = SessionKey::new(&[4u8; 32]).unwrap();
    for (mode, mcp) in [
        ("table", Mcp::new(false, None, false)),
        ("signed", Mcp::new(false, None, false).with_session_key(key)),
    ] {
        let anonymous = session_of(&mcp.handle(
            host.as_ref(),
            request("POST", &initialize_body(), None, None),
        ));
        let alices = session_of(&mcp.handle(
            host.as_ref(),
            request("POST", &initialize_body(), None, Some("alice")),
        ));
        assert_eq!(
            mcp.verified_session_route(&anonymous),
            Some("acme@2026.3"),
            "{mode}"
        );
        assert_eq!(mcp.verified_session_route(&alices), None, "{mode}");
        assert_eq!(
            mcp.verified_session_route_for(&alices, Some("alice")),
            Some("acme@2026.3"),
            "{mode}"
        );
        assert_eq!(
            mcp.verified_session_route_for(&alices, Some("bob")),
            None,
            "{mode}"
        );
        assert_eq!(
            mcp.verified_session_route_for(&anonymous, Some("alice")),
            None,
            "{mode}"
        );
    }
}

/// One graph of the registry and who may see it; `readers` get a read-only profile.
struct Graph {
    host: Arc<TestHost>,
    visible_to: Vec<Option<&'static str>>,
    readers: Vec<Option<&'static str>>,
}

/// The host's registry, shared by every request.
struct Registry {
    graphs: BTreeMap<&'static str, Graph>,
}

impl Registry {
    /// `shared` (everyone; bob reads only), `alices` (alice), `secret` (bob).
    fn new() -> Arc<Self> {
        let everyone = vec![None, Some("alice"), Some("bob")];
        let mut graphs = BTreeMap::new();
        graphs.insert(
            "shared",
            Graph {
                host: TestHost::modern(None),
                visible_to: everyone,
                readers: vec![Some("bob")],
            },
        );
        graphs.insert(
            "alices",
            Graph {
                host: TestHost::modern(None),
                visible_to: vec![Some("alice")],
                readers: vec![],
            },
        );
        graphs.insert(
            "secret",
            Graph {
                host: TestHost::modern(None),
                visible_to: vec![Some("bob")],
                readers: vec![],
            },
        );
        Arc::new(Self { graphs })
    }

    /// The directory of one request, for its caller (what a host builds per request).
    fn for_caller(self: &Arc<Self>, identity: Option<&str>) -> CallerDirectory {
        CallerDirectory {
            registry: self.clone(),
            identity: identity.map(str::to_string),
        }
    }
}

/// The registry as one caller sees it.
struct CallerDirectory {
    registry: Arc<Registry>,
    identity: Option<String>,
}

impl CallerDirectory {
    /// The graph `name` when the caller may see it; a hidden graph is the same error as an
    /// unknown one.
    fn visible(&self, name: &str) -> Result<&Graph, McpError> {
        self.registry
            .graphs
            .get(name)
            .filter(|graph| graph.visible_to.contains(&self.identity.as_deref()))
            .ok_or_else(|| McpError::Failed(format!("no database '{name}'")))
    }
}

impl GraphDirectory for CallerDirectory {
    fn list(&self) -> Result<Vec<GraphEntry>, McpError> {
        Ok(self
            .registry
            .graphs
            .keys()
            .filter(|name| self.visible(name).is_ok())
            .map(|name| GraphEntry::new(name, GraphState::Loaded))
            .collect())
    }

    fn resolve(&self, name: &str) -> Result<String, McpError> {
        self.visible(name).map(|_| name.to_string())
    }

    fn host(&self, route: &str) -> Result<(Arc<dyn Host>, Profile), McpError> {
        let graph = self.visible(route)?;
        let read_only = graph.readers.contains(&self.identity.as_deref());
        Ok((graph.host.clone(), Profile::new(read_only)))
    }
}

/// The endpoint as the host serves it: each request gets the directory of its identity.
struct Endpoint {
    mcp: Mcp,
    registry: Arc<Registry>,
}

impl Endpoint {
    fn new() -> Self {
        Self {
            mcp: Mcp::new(false, None, false),
            registry: Registry::new(),
        }
    }

    fn send(&self, request: McpRequest<'static>) -> McpResponse {
        let directory = self.registry.for_caller(request.identity());
        self.mcp.handle_directory(&directory, request)
    }

    fn initialize(&self, identity: Option<&str>) -> String {
        session_of(&self.send(request("POST", &initialize_body(), None, identity)))
    }

    /// `tools/call`: `(text, isError)`.
    fn call(
        &self,
        session: &str,
        identity: Option<&str>,
        name: &str,
        args: Value,
    ) -> (String, bool) {
        let response = self.send(request(
            "POST",
            &json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call",
                "params": {"name": name, "arguments": args}}),
            Some(session),
            identity,
        ));
        assert_eq!(response.status(), 200);
        let answer = json_of(&response);
        let result = &answer["result"];
        (
            result["content"][0]["text"]
                .as_str()
                .expect("text")
                .to_string(),
            result["isError"].as_bool().expect("isError"),
        )
    }

    fn listed(&self, session: &str, identity: Option<&str>) -> Vec<String> {
        let (text, is_error) = self.call(session, identity, "list_graphs", json!({}));
        assert!(!is_error, "{text}");
        let listed: Value = serde_json::from_str(&text).unwrap();
        listed["graphs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|graph| graph["name"].as_str().unwrap().to_string())
            .collect()
    }
}

#[test]
fn two_identities_see_their_own_graphs() {
    let endpoint = Endpoint::new();
    let alice = endpoint.initialize(Some("alice"));
    let bob = endpoint.initialize(Some("bob"));
    let anonymous = endpoint.initialize(None);
    assert_eq!(endpoint.listed(&alice, Some("alice")), ["alices", "shared"]);
    assert_eq!(endpoint.listed(&bob, Some("bob")), ["secret", "shared"]);
    assert_eq!(endpoint.listed(&anonymous, None), ["shared"]);
}

#[test]
fn a_hidden_graph_is_the_same_error_as_an_unknown_one() {
    let endpoint = Endpoint::new();
    let alice = endpoint.initialize(Some("alice"));
    let use_graph =
        |name: &str| endpoint.call(&alice, Some("alice"), "use_graph", json!({ "name": name }));
    let (hidden, hidden_error) = use_graph("secret");
    let (unknown, unknown_error) = use_graph("nothing");
    assert!(hidden_error && unknown_error);
    assert_eq!(hidden.replace("secret", "nothing"), unknown, "{hidden}");
    // The `graph` argument of one call too.
    let count = |graph: &str| {
        endpoint.call(
            &alice,
            Some("alice"),
            "gql",
            json!({"query": "MATCH (n) RETURN count(n) AS c", "graph": graph}),
        )
    };
    let (hidden, hidden_error) = count("secret");
    let (unknown, unknown_error) = count("nothing");
    assert!(hidden_error && unknown_error);
    assert_eq!(hidden.replace("secret", "nothing"), unknown, "{hidden}");
    // The owner of the hidden graph reaches it.
    let bob = endpoint.initialize(Some("bob"));
    let (text, is_error) = endpoint.call(&bob, Some("bob"), "use_graph", json!({"name": "secret"}));
    assert!(!is_error, "{text}");
}

#[test]
fn the_profile_is_chosen_per_identity() {
    let endpoint = Endpoint::new();
    let insert = json!({"query": "INSERT (:person {name: 'zed'})", "graph": "shared"});
    let alice = endpoint.initialize(Some("alice"));
    let (text, is_error) = endpoint.call(&alice, Some("alice"), "gql", insert.clone());
    assert!(!is_error, "alice writes: {text}");
    let bob = endpoint.initialize(Some("bob"));
    let (text, is_error) = endpoint.call(&bob, Some("bob"), "gql", insert);
    assert!(is_error, "bob only reads: {text}");
    assert!(text.contains("Permission denied"), "{text}");
}

#[test]
fn a_directory_session_cannot_be_borrowed_and_its_selection_stays_the_owners() {
    let endpoint = Endpoint::new();
    let send = |request: McpRequest<'static>| endpoint.send(request);
    for owner in IDENTITIES {
        let session = endpoint.initialize(owner);
        let (text, is_error) =
            endpoint.call(&session, owner, "use_graph", json!({"name": "shared"}));
        assert!(!is_error, "{text}");
        assert_not_borrowable(owner, &session, &send);
        // The selection survived the attempts.
        let (text, is_error) = endpoint.call(
            &session,
            owner,
            "gql",
            json!({"query": "MATCH (n) RETURN count(n) AS c"}),
        );
        assert!(!is_error, "{owner:?}: {text}");
        let deleted = send(request("DELETE", &json!({}), Some(&session), owner));
        assert_eq!(deleted.status(), 200);
        let gone = send(request("POST", &ping_body(), Some(&session), owner));
        assert_eq!(gone.status(), 404);
    }
}
