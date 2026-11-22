//! MCP session ids: a bounded table of the ids `initialize` assigned (the default), or
//! stateless ids signed with a server key ([`SessionKey`], [`crate::Mcp::with_session_key`]):
//! `hex(nonce || client digest || tag)`, the tag an HMAC-SHA256 of the nonce and the digest
//! under the key, so any process holding the same key accepts the id without a table (several
//! replicas behind a load balancer), and no number of clients evicts another.
//!
//! A host may pin a session to the graph it resolved at `initialize` with a route tag
//! ([`crate::Host::session_route`]): the id is then `<route>~<id>`, the route covered by the
//! HMAC of a signed id (and part of the remembered id in table mode), so a changed route is an
//! unknown session. An id without a route is byte-identical to one minted before routes
//! existed. [`session_route`] reads the route back from a request's `Mcp-Session-Id`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use ring::{digest, hmac};
use serde_json::Value;

use crate::host::McpError;

/// Most MCP sessions the table remembers by default ([`crate::Mcp::with_max_sessions`] sets
/// another); the oldest is forgotten first (its client gets `404`, initializes again and, in
/// directory mode, selects its graph again).
pub const MAX_SESSIONS: usize = 64;

/// Most client names (`clientInfo` of `initialize`) an endpoint remembers for its statistics;
/// the least recently initialized one is forgotten first (with its counters).
pub const MAX_CLIENT_NAMES: usize = 4096;

/// Bytes of the random nonce of a signed id.
const NONCE_BYTES: usize = 16;
/// Bytes of the client digest of a signed id.
const CLIENT_BYTES: usize = 8;
/// Bytes of the (truncated) HMAC-SHA256 tag of a signed id.
const TAG_BYTES: usize = 16;
/// Bytes of a signed id before its hex encoding.
const ID_BYTES: usize = NONCE_BYTES + CLIENT_BYTES + TAG_BYTES;

/// The domain of the signed data, so the key signs nothing else by accident.
const DOMAIN: &[u8] = b"graphersal-mcp-session/1";

/// The most bytes of a session route tag ([`crate::Host::session_route`]).
pub const MAX_SESSION_ROUTE_BYTES: usize = 128;

/// The character between a session route tag and the id proper (`<route>~<id>`); neither a
/// route nor an id contains it.
const ROUTE_SEPARATOR: char = '~';

/// Whether `route` can be a session route tag ([`crate::Host::session_route`]): 1 to
/// [`MAX_SESSION_ROUTE_BYTES`] bytes of ASCII letters, digits and `.` `_` `-` `@` `/` `:` `+`
/// `=` (a graph name and version such as `acme@2026.3`, or an opaque token in base64url or
/// hex). These characters are safe in an HTTP header value and never contain the separator.
pub fn is_valid_session_route(route: &str) -> bool {
    !route.is_empty()
        && route.len() <= MAX_SESSION_ROUTE_BYTES
        && route
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-@/:+=".contains(&b))
}

/// The route tag inside the MCP session id `session_id` (a request's `Mcp-Session-Id` header,
/// `<route>~<id>`), or `None` for an id without one (or text that is no routed id). A host's
/// graph resolver calls it to send every request of a session to the graph the session was
/// pinned to at `initialize` ([`crate::Host::session_route`]), also after the host moved its
/// alias, without a table and across replicas.
///
/// It only reads the id; it does not check it. The endpoint that then serves the request
/// refuses a forged or altered id (`404`, an unknown session): with signed ids the route is
/// covered by the signature, in table mode the whole routed id is what the table remembers.
/// So a resolver must not trust the route further than choosing where the request goes (e.g.
/// it checks that the route is a version of the graph the path names);
/// [`crate::Mcp::verified_session_route`] also checks the id.
pub fn session_route(session_id: &str) -> Option<&str> {
    split_route(session_id).0
}

/// `(route, id proper)` of a session id: no route when the id has no separator or what
/// precedes it is no valid route (then the whole text is the id, which no endpoint knows).
fn split_route(session_id: &str) -> (Option<&str>, &str) {
    match session_id.split_once(ROUTE_SEPARATOR) {
        Some((route, id)) if is_valid_session_route(route) => (Some(route), id),
        _ => (None, session_id),
    }
}

/// The secret that signs stateless MCP session ids ([`crate::Mcp::with_session_key`]). Never
/// shown: its `Debug` prints no byte of it.
#[derive(Clone)]
pub struct SessionKey(Vec<u8>);

impl SessionKey {
    /// The fewest bytes a key has (256 bits).
    pub const MIN_BYTES: usize = 32;
    /// The most bytes a key has.
    pub const MAX_BYTES: usize = 1024;

    /// A key of the given bytes (an operator's secret shared by replicas, e.g. the CLI's
    /// `GRAPHERSAL_MCP_SESSION_KEY`): [`Self::MIN_BYTES`] to [`Self::MAX_BYTES`] bytes, else
    /// [`McpError::Failed`] naming the rule (never the bytes).
    pub fn new(bytes: &[u8]) -> Result<Self, McpError> {
        if bytes.len() < Self::MIN_BYTES || bytes.len() > Self::MAX_BYTES {
            return Err(McpError::Failed(format!(
                "a session key has {} to {} bytes, this one has {}",
                Self::MIN_BYTES,
                Self::MAX_BYTES,
                bytes.len()
            )));
        }
        Ok(Self(bytes.to_vec()))
    }

    /// A random key of this process (ids it signs are valid until the process ends).
    pub fn random() -> Self {
        Self((0..Self::MIN_BYTES).map(|_| rand::random::<u8>()).collect())
    }
}

impl std::fmt::Debug for SessionKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SessionKey(..)")
    }
}

/// One session of the table.
pub(crate) struct TableEntry {
    id: String,
    client: Value,
    /// The route of the graph `use_graph` selected (directory mode), if any.
    selected: Option<String>,
}

/// The MCP sessions of an endpoint.
pub(crate) enum Sessions {
    /// The sessions (id, `clientInfo` of its `initialize`, the graph selected in directory
    /// mode), in the order they were created, at most `max`; the client names for the
    /// statistics.
    Table {
        ids: Mutex<Vec<TableEntry>>,
        clients: Arc<RwLock<ClientNames>>,
        max: usize,
    },
    /// Signed ids, checked without a table; the client names for the statistics.
    Signed {
        key: Box<hmac::Key>,
        clients: Arc<RwLock<ClientNames>>,
    },
}

impl Sessions {
    /// The table of today's dev server.
    pub(crate) fn table() -> Self {
        Self::Table {
            ids: Mutex::new(Vec::new()),
            clients: Arc::default(),
            max: MAX_SESSIONS,
        }
    }

    /// Remembers at most `max` sessions (at least 1); table only, signed ids keep no table.
    pub(crate) fn with_max(self, max: usize) -> Self {
        match self {
            Self::Table { ids, clients, .. } => Self::Table {
                ids,
                clients,
                max: max.max(1),
            },
            signed @ Self::Signed { .. } => signed,
        }
    }

    /// Stores `route` as the graph the table session `id` selected (directory mode): whether
    /// the session is known (signed ids keep nothing: `false`).
    pub(crate) fn select(&self, id: &str, route: &str) -> bool {
        match self {
            Self::Table { ids, .. } => ids.lock().is_ok_and(|mut sessions| {
                match sessions.iter_mut().find(|entry| entry.id == id) {
                    Some(entry) => {
                        entry.selected = Some(route.to_string());
                        true
                    }
                    None => false,
                }
            }),
            Self::Signed { .. } => false,
        }
    }

    /// The route of the graph the table session `id` selected, if any.
    pub(crate) fn selected(&self, id: &str) -> Option<String> {
        match self {
            Self::Table { ids, .. } => ids.lock().ok().and_then(|sessions| {
                sessions
                    .iter()
                    .find(|entry| entry.id == id)
                    .and_then(|entry| entry.selected.clone())
            }),
            Self::Signed { .. } => None,
        }
    }

    /// Stateless ids signed with `key`.
    pub(crate) fn signed(key: &SessionKey) -> Self {
        Self::Signed {
            key: Box::new(hmac::Key::new(hmac::HMAC_SHA256, &key.0)),
            clients: Arc::default(),
        }
    }

    /// The client names (shared with the endpoint's statistics).
    pub(crate) fn clients(&self) -> Arc<RwLock<ClientNames>> {
        match self {
            Self::Table { clients, .. } | Self::Signed { clients, .. } => clients.clone(),
        }
    }

    /// The client digest of the known session `id` (for the statistics' per-client counts).
    pub(crate) fn client_of(&self, id: &str) -> Option<u64> {
        match self {
            Self::Table { ids, .. } => ids.lock().ok().and_then(|ids| {
                ids.iter()
                    .find(|entry| entry.id == id)
                    .map(|entry| client_digest(&entry.client))
            }),
            Self::Signed { .. } => {
                let (_, id) = split_route(id);
                let hex = id.get(NONCE_BYTES * 2..(NONCE_BYTES + CLIENT_BYTES) * 2)?;
                u64::from_str_radix(hex, 16).ok()
            }
        }
    }

    /// Whether the ids are signed (stateless).
    pub(crate) fn is_signed(&self) -> bool {
        matches!(self, Self::Signed { .. })
    }

    /// A new session id for the client `client` (`{name, version}`), without a route.
    #[cfg(test)]
    pub(crate) fn create(&self, client: &Value) -> String {
        self.create_routed(client, None)
    }

    /// A new session id for the client `client` (`{name, version}`), pinned to `route` when
    /// the host gave one (`<route>~<id>`; the caller checked it with
    /// [`is_valid_session_route`]).
    pub(crate) fn create_routed(&self, client: &Value, route: Option<&str>) -> String {
        let prefix = route.map_or(String::new(), |route| format!("{route}{ROUTE_SEPARATOR}"));
        match self {
            Self::Table { ids, clients, max } => {
                let session = format!("{prefix}{:032x}", rand::random::<u128>());
                if let Ok(mut sessions) = ids.lock() {
                    if sessions.len() >= *max {
                        let excess = sessions.len() + 1 - *max;
                        sessions.drain(..excess);
                    }
                    sessions.push(TableEntry {
                        id: session.clone(),
                        client: client.clone(),
                        selected: None,
                    });
                }
                if let Ok(mut clients) = clients.write() {
                    clients.remember(client_digest(client), client.clone());
                }
                session
            }
            Self::Signed { key, clients } => {
                let digest = client_digest(client);
                if let Ok(mut clients) = clients.write() {
                    clients.remember(digest, client.clone());
                }
                let mut id = Vec::with_capacity(ID_BYTES);
                id.extend((0..NONCE_BYTES).map(|_| rand::random::<u8>()));
                id.extend_from_slice(&digest.to_be_bytes());
                let tag = sign(key, &id, route.unwrap_or_default());
                id.extend_from_slice(&tag.as_ref()[..TAG_BYTES]);
                format!("{prefix}{}", hex(&id))
            }
        }
    }

    /// Remembers the client `client` (`{name, version}`) of a stream connection for the
    /// statistics, without a session id (a stream is its own session); its digest.
    pub(crate) fn remember_client(&self, client: &Value) -> u64 {
        let digest = client_digest(client);
        if let Ok(mut clients) = self.clients().write() {
            clients.remember(digest, client.clone());
        }
        digest
    }

    /// Whether `id` names a session of this endpoint (in the table, or signed by its key).
    pub(crate) fn knows(&self, id: &str) -> bool {
        match self {
            Self::Table { ids, .. } => ids
                .lock()
                .is_ok_and(|sessions| sessions.iter().any(|entry| entry.id == id)),
            Self::Signed { key, .. } => verify(key, id),
        }
    }

    /// Ends the session `id` (`DELETE /mcp`): whether it was known. A signed id stays valid
    /// (nothing is kept to forget); the client does not use it again.
    pub(crate) fn forget(&self, id: &str) -> bool {
        match self {
            Self::Table { ids, .. } => ids.lock().is_ok_and(|mut sessions| {
                let before = sessions.len();
                sessions.retain(|entry| entry.id != id);
                sessions.len() < before
            }),
            Self::Signed { key, .. } => verify(key, id),
        }
    }

    /// The remembered sessions (table) or client names (signed) and their estimated bytes;
    /// `(0, 0)` while a request holds the lock.
    pub(crate) fn memory_bytes(&self) -> (usize, u64) {
        match self {
            Self::Table { ids, clients, .. } => {
                let Ok(sessions) = ids.try_lock() else {
                    return (0, 0);
                };
                let clients = clients.try_read().map_or(0, |clients| clients.bytes());
                let bytes: u64 = sessions
                    .iter()
                    .map(|entry| {
                        (std::mem::size_of::<TableEntry>()
                            + entry.id.capacity()
                            + entry.client.to_string().len()
                            + entry.selected.as_ref().map_or(0, String::capacity))
                            as u64
                    })
                    .sum();
                (sessions.len(), bytes + clients)
            }
            Self::Signed { clients, .. } => {
                let Ok(clients) = clients.try_read() else {
                    return (0, 0);
                };
                (clients.names.len(), clients.bytes())
            }
        }
    }
}

/// The client names of an endpoint, by client digest, with their usage counters: at most
/// [`MAX_CLIENT_NAMES`], the least recently initialized forgotten first.
#[derive(Default)]
pub(crate) struct ClientNames {
    names: HashMap<u64, ClientEntry>,
    tick: u64,
}

/// One client name (`{name, version}`) and its counters.
struct ClientEntry {
    client: Value,
    tick: u64,
    initialized: AtomicU64,
    requests: AtomicU64,
    tool_calls: AtomicU64,
}

impl ClientNames {
    fn remember(&mut self, digest: u64, client: Value) {
        self.tick += 1;
        if let Some(entry) = self.names.get_mut(&digest) {
            entry.tick = self.tick;
            entry.initialized.fetch_add(1, Ordering::Relaxed);
            return;
        }
        if self.names.len() >= MAX_CLIENT_NAMES {
            let oldest = self
                .names
                .iter()
                .min_by_key(|(_, entry)| entry.tick)
                .map(|(digest, _)| *digest);
            if let Some(oldest) = oldest {
                self.names.remove(&oldest);
            }
        }
        self.names.insert(
            digest,
            ClientEntry {
                client,
                tick: self.tick,
                initialized: AtomicU64::new(1),
                requests: AtomicU64::new(0),
                tool_calls: AtomicU64::new(0),
            },
        );
    }

    /// Counts a request (and a tool call) of the client `digest`, if it is remembered.
    pub(crate) fn count_request(&self, digest: u64, tool_call: bool) {
        if let Some(entry) = self.names.get(&digest) {
            entry.requests.fetch_add(1, Ordering::Relaxed);
            if tool_call {
                entry.tool_calls.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// How many client names are remembered.
    pub(crate) fn len(&self) -> usize {
        self.names.len()
    }

    /// The clients as `[{name, version, initialized, requests, toolCalls}]`, most requests
    /// first.
    pub(crate) fn to_json(&self) -> Vec<Value> {
        let mut entries: Vec<&ClientEntry> = self.names.values().collect();
        entries.sort_by(|a, b| {
            b.requests
                .load(Ordering::Relaxed)
                .cmp(&a.requests.load(Ordering::Relaxed))
                .then_with(|| a.client.to_string().cmp(&b.client.to_string()))
        });
        entries
            .into_iter()
            .map(|entry| {
                serde_json::json!({
                    "name": entry.client["name"],
                    "version": entry.client["version"],
                    "initialized": entry.initialized.load(Ordering::Relaxed),
                    "requests": entry.requests.load(Ordering::Relaxed),
                    "toolCalls": entry.tool_calls.load(Ordering::Relaxed),
                })
            })
            .collect()
    }

    fn bytes(&self) -> u64 {
        self.names
            .values()
            .map(|entry| {
                (std::mem::size_of::<(u64, ClientEntry)>() + entry.client.to_string().len()) as u64
            })
            .sum()
    }
}

/// The first 8 bytes of the SHA-256 of the client's name and version.
fn client_digest(client: &Value) -> u64 {
    let mut context = digest::Context::new(&digest::SHA256);
    context.update(client["name"].as_str().unwrap_or_default().as_bytes());
    context.update(&[0]);
    context.update(client["version"].as_str().unwrap_or_default().as_bytes());
    let digest = context.finish();
    let mut first = [0u8; 8];
    first.copy_from_slice(&digest.as_ref()[..8]);
    u64::from_be_bytes(first)
}

/// The HMAC-SHA256 of [`DOMAIN`], `data` (the nonce and the client digest, fixed length) and
/// the session's `route` (empty without one, so an id without a route signs exactly what it
/// signed before routes existed) under `key`.
fn sign(key: &hmac::Key, data: &[u8], route: &str) -> hmac::Tag {
    let mut context = hmac::Context::with_key(key);
    context.update(DOMAIN);
    context.update(data);
    context.update(route.as_bytes());
    context.sign()
}

/// Whether `id` is a session id signed with `key` (compared without stopping at the first
/// difference).
fn verify(key: &hmac::Key, id: &str) -> bool {
    let (route, id) = split_route(id);
    let Some(bytes) = unhex(id) else {
        return false;
    };
    if bytes.len() != ID_BYTES {
        return false;
    }
    let (signed, tag) = bytes.split_at(NONCE_BYTES + CLIENT_BYTES);
    let expected = sign(key, signed, route.unwrap_or_default());
    expected.as_ref()[..TAG_BYTES]
        .iter()
        .zip(tag)
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || !text.is_ascii() {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn signed_ids_verify_under_the_same_key_only() {
        let key = SessionKey::new(&[7u8; 32]).unwrap();
        let one = Sessions::signed(&key);
        let two = Sessions::signed(&key);
        let other = Sessions::signed(&SessionKey::new(&[8u8; 32]).unwrap());
        let id = one.create(&json!({"name": "claude-code", "version": "1"}));
        assert_eq!(id.len(), ID_BYTES * 2);
        assert!(one.knows(&id) && two.knows(&id));
        assert!(!other.knows(&id));
        // A changed character, a cut id and text that is not hex are refused.
        let mut changed = id.clone().into_bytes();
        changed[3] = if changed[3] == b'0' { b'1' } else { b'0' };
        assert!(!one.knows(&String::from_utf8(changed).unwrap()));
        assert!(!one.knows(&id[..id.len() - 2]));
        assert!(!one.knows("zz"));
        assert!(!one.knows("é"));
        // DELETE keeps a signed id valid (nothing is stored).
        assert!(one.forget(&id) && one.knows(&id));
    }

    #[test]
    fn keys_are_checked_and_never_shown() {
        assert!(SessionKey::new(&[1u8; 31]).is_err());
        assert!(SessionKey::new(&[1u8; 1025]).is_err());
        let key = SessionKey::new(b"0123456789abcdef0123456789abcdef").unwrap();
        assert_eq!(format!("{key:?}"), "SessionKey(..)");
        let message = SessionKey::new(b"short secret").unwrap_err().to_string();
        assert!(!message.contains("short secret"), "{message}");
    }

    #[test]
    fn client_names_are_bounded() {
        let sessions = Sessions::signed(&SessionKey::random());
        for i in 0..MAX_CLIENT_NAMES + 10 {
            sessions.create(&json!({"name": format!("client {i}"), "version": "1"}));
        }
        let (names, bytes) = sessions.memory_bytes();
        assert_eq!(names, MAX_CLIENT_NAMES);
        assert!(bytes > 0);
    }

    #[test]
    fn the_table_forgets_the_oldest_beyond_its_size() {
        let sessions = Sessions::table();
        let first = sessions.create(&json!({"name": "a"}));
        for _ in 0..MAX_SESSIONS {
            sessions.create(&json!({"name": "b"}));
        }
        assert!(!sessions.knows(&first));
        assert!(!sessions.is_signed());
    }

    #[test]
    fn a_table_session_keeps_its_selection_and_its_size_is_configurable() {
        let sessions = Sessions::table().with_max(2);
        let first = sessions.create(&json!({"name": "a"}));
        assert_eq!(sessions.selected(&first), None);
        assert!(sessions.select(&first, "acme@2"));
        assert_eq!(sessions.selected(&first).as_deref(), Some("acme@2"));
        assert!(!sessions.select("unknown", "acme@2"));
        let second = sessions.create(&json!({"name": "b"}));
        assert!(sessions.knows(&first) && sessions.knows(&second));
        sessions.create(&json!({"name": "c"}));
        // The oldest is forgotten with its selection.
        assert!(!sessions.knows(&first) && sessions.selected(&first).is_none());
        // Signed ids keep no selection.
        let signed = Sessions::signed(&SessionKey::random()).with_max(2);
        let id = signed.create(&json!({"name": "a"}));
        assert!(!signed.select(&id, "acme") && signed.selected(&id).is_none());
    }
}
