//! A host's MCP session store (`Mcp::with_session_store`): a restarted endpoint accepts the old
//! session ids with their identity, selected graph and pending rename note; a revoked identity
//! loses its sessions; a failing store leaves the endpoint working; signed ids refuse a store.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};

use graphersal_mcp::{
    ClientRef, Executor, GraphDirectory, GraphEntry, GraphState, Host, Job, Mcp, McpError,
    McpRequest, McpResponse, Profile, SessionKey, SessionStore, StoredSession,
};
use graphersal_session::Session;
use serde_json::{Value, json};

/// A host over one session on its own thread (the modern graph).
struct TestHost {
    jobs: Mutex<Sender<Job>>,
}

impl TestHost {
    fn modern() -> Arc<Self> {
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
}

/// One loaded graph `a` and aliases onto it.
struct Registry {
    host: Arc<TestHost>,
    aliases: Mutex<BTreeMap<String, String>>,
}

impl Registry {
    fn new() -> Self {
        let registry = Self {
            host: TestHost::modern(),
            aliases: Mutex::default(),
        };
        registry
            .aliases
            .lock()
            .unwrap()
            .insert("prod".into(), "a".into());
        registry
    }

    fn rename_alias(&self, mcp: &Mcp, old: &str, new: &str) -> usize {
        let mut aliases = self.aliases.lock().unwrap();
        let target = aliases.remove(old).unwrap();
        aliases.insert(new.into(), target);
        drop(aliases);
        mcp.rename_selection(old, new)
    }
}

impl GraphDirectory for Registry {
    fn list(&self) -> Result<Vec<GraphEntry>, McpError> {
        let mut entries = vec![GraphEntry::new("a", GraphState::Loaded)];
        for (alias, target) in self.aliases.lock().unwrap().iter() {
            entries.push(GraphEntry::new(alias, GraphState::Loaded).with_alias_of(target));
        }
        Ok(entries)
    }

    fn resolve(&self, name: &str) -> Result<String, McpError> {
        if let Some(target) = self.aliases.lock().unwrap().get(name) {
            return Ok(target.clone());
        }
        if name == "a" {
            return Ok(name.into());
        }
        Err(McpError::Failed(format!("no database '{name}'")))
    }

    fn host(&self, route: &str) -> Result<(Arc<dyn Host>, Profile), McpError> {
        match route {
            "a" => Ok((self.host.clone(), Profile::new(false))),
            _ => Err(McpError::Failed(format!("no database '{route}'"))),
        }
    }
}

/// The host's store: the last table saved (a file in a real host), and whether saving fails.
#[derive(Default)]
struct FileLikeStore {
    table: Mutex<Vec<StoredSession>>,
    fail: AtomicBool,
}

impl SessionStore for FileLikeStore {
    fn load(&self) -> Result<Vec<StoredSession>, McpError> {
        // A real host parses its file; rebuild each row from its getters as it would.
        Ok(self
            .table
            .lock()
            .unwrap()
            .iter()
            .map(|s| {
                StoredSession::new(
                    s.id_sha256(),
                    s.identity().map(str::to_string),
                    s.selected().map(str::to_string),
                    s.renamed_from().map(str::to_string),
                    s.protocol_version(),
                    s.client().clone(),
                    s.created_unix_s(),
                )
            })
            .collect())
    }

    fn save(&self, sessions: &[StoredSession]) -> Result<(), McpError> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(McpError::Failed("the disk is full".into()));
        }
        *self.table.lock().unwrap() = sessions.to_vec();
        Ok(())
    }
}

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

fn call_body(name: &str, args: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call",
        "params": {"name": name, "arguments": args}})
}

fn session_of(response: &McpResponse) -> String {
    assert_eq!(response.status(), 200);
    response
        .header("mcp-session-id")
        .expect("a session id")
        .to_string()
}

/// The text of a `tools/call` answer that must be `200` and no tool error.
fn text_of(response: &McpResponse) -> String {
    assert_eq!(response.status(), 200);
    let answer: Value = serde_json::from_slice(response.body()).expect("a JSON body");
    assert_eq!(answer["result"]["isError"], false, "{answer}");
    answer["result"]["content"][0]["text"]
        .as_str()
        .expect("text")
        .to_string()
}

#[test]
fn a_restarted_directory_endpoint_keeps_the_sessions_of_its_store() {
    let registry = Registry::new();
    let store = Arc::new(FileLikeStore::default());
    let first = Mcp::new(false, None, false)
        .with_session_store(store.clone())
        .expect("an empty store loads");
    let alice = session_of(&first.handle_directory(
        &registry,
        request("POST", &initialize_body(), None, Some("alice")),
    ));
    let bob = session_of(&first.handle_directory(
        &registry,
        request("POST", &initialize_body(), None, Some("bob")),
    ));
    text_of(&first.handle_directory(
        &registry,
        request(
            "POST",
            &call_body("use_graph", json!({"name": "prod"})),
            Some(&alice),
            Some("alice"),
        ),
    ));
    assert_eq!(registry.rename_alias(&first, "prod", "live"), 1);
    drop(first);

    // The host's table: hashes only, the identity, the selection, the pending note.
    let stored = store.table.lock().unwrap().clone();
    assert_eq!(stored.len(), 2);
    assert!(stored.iter().all(|s| s.id_sha256().len() == 64));
    assert!(!format!("{stored:?}").contains(&alice) && !format!("{stored:?}").contains(&bob));
    assert_eq!(stored[0].identity(), Some("alice"));
    assert_eq!(stored[0].selected(), Some("live"));
    assert_eq!(stored[0].renamed_from(), Some("prod"));
    assert_eq!(stored[0].protocol_version(), "2025-06-18");

    // A new endpoint over the same store (a restarted host).
    let second = Mcp::new(false, None, false)
        .with_session_store(store.clone())
        .expect("the store loads");
    let list = |identity: Option<&str>| {
        second.handle_directory(
            &registry,
            request(
                "POST",
                &call_body("list_graphs", json!({})),
                Some(&alice),
                identity,
            ),
        )
    };
    // Another identity (anonymous included) does not find the session.
    assert_eq!(list(Some("bob")).status(), 404);
    assert_eq!(list(None).status(), 404);
    let text = text_of(&list(Some("alice")));
    let (note, listing) = text.split_once('\n').expect("a note line");
    assert_eq!(note, "note: alias \"prod\" was renamed to \"live\"");
    let listing: Value = serde_json::from_str(listing).unwrap();
    assert_eq!(listing["selected"], "live");
    assert_eq!(listing["selectedGraph"], "a");
    // The note was told once, also across another restart.
    let third = Mcp::new(false, None, false)
        .with_session_store(store.clone())
        .unwrap();
    let text = text_of(&third.handle_directory(
        &registry,
        request(
            "POST",
            &call_body("list_graphs", json!({})),
            Some(&alice),
            Some("alice"),
        ),
    ));
    assert!(!text.contains("renamed"), "{text}");

    // Revoking alice ends her sessions and saves the table; bob's stays.
    assert_eq!(third.forget_sessions_of(Some("alice")), 1);
    assert_eq!(third.forget_sessions_of(Some("alice")), 0);
    let stored = store.table.lock().unwrap().clone();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].identity(), Some("bob"));
    let refused = third.handle_directory(
        &registry,
        request(
            "POST",
            &call_body("list_graphs", json!({})),
            Some(&alice),
            Some("alice"),
        ),
    );
    assert_eq!(refused.status(), 404);
    let fourth = Mcp::new(false, None, false)
        .with_session_store(store.clone())
        .unwrap();
    let bobs = fourth.handle_directory(
        &registry,
        request(
            "POST",
            &call_body("list_graphs", json!({})),
            Some(&bob),
            Some("bob"),
        ),
    );
    text_of(&bobs);
}

#[test]
fn a_single_graph_session_survives_and_delete_is_saved() {
    let host = TestHost::modern();
    let store = Arc::new(FileLikeStore::default());
    let first = Mcp::new(false, None, false)
        .with_session_store(store.clone())
        .unwrap();
    let id = session_of(&first.handle(
        host.as_ref(),
        request("POST", &initialize_body(), None, None),
    ));
    let second = Mcp::new(false, None, false)
        .with_session_store(store.clone())
        .unwrap();
    let ping = json!({"jsonrpc": "2.0", "id": 2, "method": "ping"});
    let pong = second.handle(host.as_ref(), request("POST", &ping, Some(&id), None));
    assert_eq!(pong.status(), 200);
    let deleted = second.handle(
        host.as_ref(),
        request("DELETE", &json!({}), Some(&id), None),
    );
    assert_eq!(deleted.status(), 200);
    assert!(store.table.lock().unwrap().is_empty());
}

#[test]
fn a_failing_store_keeps_the_endpoint_working() {
    let host = TestHost::modern();
    let store = Arc::new(FileLikeStore::default());
    store.fail.store(true, Ordering::SeqCst);
    let mcp = Mcp::new(false, None, false)
        .with_session_store(store.clone())
        .unwrap();
    let id = session_of(&mcp.handle(
        host.as_ref(),
        request("POST", &initialize_body(), None, None),
    ));
    let ping = json!({"jsonrpc": "2.0", "id": 2, "method": "ping"});
    let pong = mcp.handle(host.as_ref(), request("POST", &ping, Some(&id), None));
    assert_eq!(pong.status(), 200);
    assert!(store.table.lock().unwrap().is_empty());
}

#[test]
fn signed_ids_refuse_a_store_and_the_default_keeps_none() {
    let store = Arc::new(FileLikeStore::default());
    let signed = Mcp::new(false, None, false)
        .with_session_key(SessionKey::random())
        .with_session_store(store.clone());
    let error = signed.err().expect("signed ids refuse a store");
    assert!(error.to_string().contains("with_session_key"), "{error}");
    assert_eq!(
        Mcp::new(false, None, false)
            .with_session_key(SessionKey::random())
            .forget_sessions_of(None),
        0
    );
    // The default table forgets on revocation too, without a store.
    let host = TestHost::modern();
    let mcp = Mcp::new(false, None, false);
    let id = session_of(&mcp.handle(
        host.as_ref(),
        request("POST", &initialize_body(), None, Some("carol")),
    ));
    assert_eq!(mcp.forget_sessions_of(Some("carol")), 1);
    let ping = json!({"jsonrpc": "2.0", "id": 2, "method": "ping"});
    let gone = mcp.handle(
        host.as_ref(),
        request("POST", &ping, Some(&id), Some("carol")),
    );
    assert_eq!(gone.status(), 404);
}
