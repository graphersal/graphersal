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
//!
//! Every session belongs to the identity of its `initialize` request
//! ([`crate::McpRequest::with_identity`]; none = anonymous): a request naming the session with
//! another identity (anonymous included, both ways) is an unknown session (`404`), the same
//! answer as a forged or expired id. The table remembers the identity with the session; a
//! signed id covers it with its HMAC (an anonymous id signs exactly what it signed before
//! identities were bound).

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
/// covered by the signature, in table mode the table remembers the hash of the whole routed id.
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

/// Where a host keeps the MCP session table so the sessions survive a restart
/// ([`crate::Mcp::with_session_store`]): the endpoint loads the whole table once and saves the
/// whole table after every change (write-through: a session created, a graph selected, a
/// selection renamed or its note told, a session ended, evicted or dropped by
/// [`crate::Mcp::forget_sessions_of`]). Without a store the table lives in memory only.
///
/// The table never holds a plain session id, only its SHA-256
/// ([`StoredSession::id_sha256`]), so what a store writes cannot be presented as a session id.
/// `save` is called outside the table lock, one call at a time and never with an older table
/// after a newer one; a failed `save` is logged (`tracing` warning) and the request goes on
/// with the table in memory, so a store should write atomically (a crash mid-write must leave
/// the previous table readable).
pub trait SessionStore: Send + Sync {
    /// The stored sessions (in any order; the endpoint keeps the newest by
    /// [`StoredSession::created_unix_s`] up to its [`crate::Mcp::with_max_sessions`]). `Err`
    /// fails [`crate::Mcp::with_session_store`]; an empty store answers `Ok(vec![])`.
    fn load(&self) -> Result<Vec<StoredSession>, McpError>;

    /// Replaces the stored table with `sessions` (the whole table, oldest first).
    fn save(&self, sessions: &[StoredSession]) -> Result<(), McpError>;
}

/// One MCP session of the table as a [`SessionStore`] keeps it. Every field is a string, an
/// optional string, a number or JSON, so a host serialises it in any format.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct StoredSession {
    id_sha256: String,
    identity: Option<String>,
    selected: Option<String>,
    renamed_from: Option<String>,
    protocol_version: String,
    client: Value,
    created_unix_s: u64,
}

impl StoredSession {
    /// A stored session, as a host reads it back: the fields of the getters of the same names.
    pub fn new(
        id_sha256: impl Into<String>,
        identity: Option<String>,
        selected: Option<String>,
        renamed_from: Option<String>,
        protocol_version: impl Into<String>,
        client: Value,
        created_unix_s: u64,
    ) -> Self {
        Self {
            id_sha256: id_sha256.into(),
            identity,
            selected,
            renamed_from,
            protocol_version: protocol_version.into(),
            client,
            created_unix_s,
        }
    }

    /// The SHA-256 of the session id (the whole `Mcp-Session-Id`, route tag included), 64
    /// lowercase hex digits. A loaded session whose hash is not 64 hex digits is skipped.
    pub fn id_sha256(&self) -> &str {
        &self.id_sha256
    }

    /// The identity of the `initialize` that created the session (none = anonymous), compared
    /// exactly: only a request of the same identity finds the session.
    pub fn identity(&self) -> Option<&str> {
        self.identity.as_deref()
    }

    /// The name (graph or alias) the session selected with `use_graph` (directory mode).
    pub fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    /// The first old name of a rename ([`crate::Mcp::rename_selection`]) the session has not
    /// been told about yet: its next `tools/call` carries the note.
    pub fn renamed_from(&self) -> Option<&str> {
        self.renamed_from.as_deref()
    }

    /// The protocol revision `initialize` negotiated (one of [`crate::PROTOCOL_VERSIONS`] when
    /// this version of the endpoint created it).
    pub fn protocol_version(&self) -> &str {
        &self.protocol_version
    }

    /// The client of the session's `initialize`: `{name, version}`.
    pub fn client(&self) -> &Value {
        &self.client
    }

    /// When the session was created, in seconds since the Unix epoch.
    pub fn created_unix_s(&self) -> u64 {
        self.created_unix_s
    }
}

/// One session of the table.
pub(crate) struct TableEntry {
    /// The SHA-256 of the session id (the plain id is never kept).
    id_sha256: [u8; 32],
    client: Value,
    /// The NAME (graph or alias) `use_graph` selected (directory mode), if any; resolved on
    /// every call.
    selected: Option<String>,
    /// The name the selection had before the renames ([`Sessions::rename`]) the session has
    /// not been told about yet: the first old name of the pending note.
    renamed_from: Option<String>,
    /// The identity of the `initialize` request that created the session (none = anonymous);
    /// a request with another identity does not find the session.
    identity: Option<String>,
    /// The protocol revision `initialize` negotiated.
    protocol_version: String,
    /// When the session was created, in seconds since the Unix epoch.
    created_unix_s: u64,
}

impl TableEntry {
    /// Whether this entry is the session of the id hash `hash` of the identity `identity`
    /// (strict equality: anonymous only matches anonymous).
    fn is(&self, hash: &[u8; 32], identity: Option<&str>) -> bool {
        self.id_sha256 == *hash && self.identity.as_deref() == identity
    }

    fn stored(&self) -> StoredSession {
        StoredSession {
            id_sha256: hex(&self.id_sha256),
            identity: self.identity.clone(),
            selected: self.selected.clone(),
            renamed_from: self.renamed_from.clone(),
            protocol_version: self.protocol_version.clone(),
            client: self.client.clone(),
            created_unix_s: self.created_unix_s,
        }
    }

    /// The entry of a loaded session; `None` when its hash is not 64 hex digits.
    fn restored(stored: StoredSession) -> Option<Self> {
        let bytes = unhex(&stored.id_sha256)?;
        let id_sha256: [u8; 32] = bytes.try_into().ok()?;
        Some(Self {
            id_sha256,
            client: stored.client,
            selected: stored.selected,
            renamed_from: stored.renamed_from,
            identity: stored.identity,
            protocol_version: stored.protocol_version,
            created_unix_s: stored.created_unix_s,
        })
    }
}

/// The SHA-256 of a presented session id: what the table compares.
fn id_hash(id: &str) -> [u8; 32] {
    let mut hash = [0u8; 32];
    hash.copy_from_slice(digest::digest(&digest::SHA256, id.as_bytes()).as_ref());
    hash
}

/// The seconds since the Unix epoch (0 on a clock before it).
fn now_unix_s() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// Locks `mutex`, also after a panic of another holder (the data stays consistent: every
/// change under these locks is a single push, retain or field assignment).
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// The sessions of the table in the order they were created, and the number of changes made
/// to them (the generation of a snapshot for the store).
#[derive(Default)]
pub(crate) struct Table {
    entries: Vec<TableEntry>,
    generation: u64,
}

/// The store of a table and the generation of the newest snapshot handed to it.
pub(crate) struct Persist {
    store: Arc<dyn SessionStore>,
    /// Held while a snapshot is written, so saves run one at a time; an older snapshot than
    /// the last one written is dropped.
    written: Mutex<u64>,
}

impl Persist {
    fn new(store: Arc<dyn SessionStore>) -> Self {
        Self {
            store,
            written: Mutex::new(0),
        }
    }

    /// Saves `sessions`, the table at `generation`, unless a newer snapshot was written already;
    /// a failure is a warning (the table stays in memory, the next change saves it again).
    fn write(&self, generation: u64, sessions: &[StoredSession]) {
        let mut written = lock(&self.written);
        if *written >= generation {
            return;
        }
        *written = generation;
        if let Err(error) = self.store.save(sessions) {
            tracing::warn!(
                %error,
                sessions = sessions.len(),
                "the MCP session store could not save the session table; the sessions stay in \
                 memory and the next change saves them again"
            );
        }
    }
}

/// The MCP sessions of an endpoint.
pub(crate) enum Sessions {
    /// The sessions (id hash, `clientInfo` of its `initialize`, the graph selected in
    /// directory mode, ...), in the order they were created, at most `max`; the client names
    /// for the statistics; the host's store, if any.
    Table {
        table: Mutex<Table>,
        clients: Arc<RwLock<ClientNames>>,
        max: usize,
        store: Option<Persist>,
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
            table: Mutex::default(),
            clients: Arc::default(),
            max: MAX_SESSIONS,
            store: None,
        }
    }

    /// Remembers at most `max` sessions (at least 1); table only, signed ids keep no table.
    pub(crate) fn with_max(self, max: usize) -> Self {
        match self {
            Self::Table {
                table,
                clients,
                store,
                ..
            } => Self::Table {
                table,
                clients,
                max: max.max(1),
                store,
            },
            signed @ Self::Signed { .. } => signed,
        }
    }

    /// Keeps the table in `store`: loads it (the newest `max` sessions, replacing the table)
    /// and saves it after every change. `Err` with signed ids (they keep no table) or when the
    /// store cannot load.
    pub(crate) fn with_store(self, store: Arc<dyn SessionStore>) -> Result<Self, McpError> {
        let Self::Table { clients, max, .. } = self else {
            return Err(McpError::Failed(
                "an MCP session store keeps a session table, but this endpoint signs its \
                 session ids (with_session_key) and keeps none"
                    .into(),
            ));
        };
        let loaded = store.load()?;
        let count = loaded.len();
        let mut entries: Vec<TableEntry> = loaded
            .into_iter()
            .filter_map(TableEntry::restored)
            .collect();
        if entries.len() < count {
            tracing::warn!(
                skipped = count - entries.len(),
                "the MCP session store holds sessions whose id hash is not 64 hex digits; \
                 they are skipped"
            );
        }
        entries.sort_by_key(|entry| entry.created_unix_s);
        if entries.len() > max {
            entries.drain(..entries.len() - max);
        }
        if let Ok(mut names) = clients.write() {
            for entry in &entries {
                names.restore(client_digest(&entry.client), entry.client.clone());
            }
        }
        Ok(Self::Table {
            table: Mutex::new(Table {
                entries,
                generation: 0,
            }),
            clients,
            max,
            store: Some(Persist::new(store)),
        })
    }

    /// Runs `change` on the table's sessions (`None` with signed ids); when it reports a
    /// change and a store is set, the table is saved: the snapshot is taken under the table
    /// lock, written after it is released.
    fn change<R>(
        &self,
        change: impl FnOnce(&mut Vec<TableEntry>, usize) -> (R, bool),
    ) -> Option<R> {
        let Self::Table {
            table, max, store, ..
        } = self
        else {
            return None;
        };
        let mut table = lock(table);
        let (result, changed) = change(&mut table.entries, *max);
        let snapshot = match store {
            Some(_) if changed => {
                table.generation += 1;
                let sessions: Vec<StoredSession> =
                    table.entries.iter().map(TableEntry::stored).collect();
                Some((table.generation, sessions))
            }
            _ => None,
        };
        drop(table);
        if let (Some(store), Some((generation, sessions))) = (store, snapshot) {
            store.write(generation, &sessions);
        }
        Some(result)
    }

    /// Reads the table's sessions (`None` with signed ids).
    fn read<R>(&self, read: impl FnOnce(&[TableEntry]) -> R) -> Option<R> {
        match self {
            Self::Table { table, .. } => Some(read(&lock(table).entries)),
            Self::Signed { .. } => None,
        }
    }

    /// Stores the name `name` as the graph the table session `id` selected (directory mode),
    /// dropping a pending rename note (the agent named its graph itself): whether the session
    /// is known (signed ids keep nothing: `false`).
    pub(crate) fn select(&self, id: &str, name: &str) -> bool {
        let hash = id_hash(id);
        self.change(
            |sessions, _| match sessions.iter_mut().find(|entry| entry.id_sha256 == hash) {
                Some(entry) => {
                    entry.selected = Some(name.to_string());
                    entry.renamed_from = None;
                    (true, true)
                }
                None => (false, false),
            },
        )
        .unwrap_or(false)
    }

    /// Moves every table session whose selection is `old` (ASCII case ignored) to `new`, keeping
    /// the first old name for the pending note: the number of sessions moved. A session whose
    /// renames lead back to its first name has no note left.
    pub(crate) fn rename(&self, old: &str, new: &str) -> usize {
        self.change(|sessions, _| {
            let mut moved = 0;
            for entry in sessions.iter_mut() {
                let Some(selected) = entry
                    .selected
                    .take_if(|name| name.eq_ignore_ascii_case(old))
                else {
                    continue;
                };
                let first = entry.renamed_from.take().unwrap_or(selected);
                entry.renamed_from = (first != new).then_some(first);
                entry.selected = Some(new.to_string());
                moved += 1;
            }
            (moved, moved > 0)
        })
        .unwrap_or(0)
    }

    /// Takes the pending rename note of the table session `id`: `(first old name, current
    /// name)`, then none until the next rename.
    pub(crate) fn take_rename_note(&self, id: &str) -> Option<(String, String)> {
        let hash = id_hash(id);
        self.change(|sessions, _| {
            let Some(entry) = sessions.iter_mut().find(|entry| entry.id_sha256 == hash) else {
                return (None, false);
            };
            let Some(first) = entry.renamed_from.take() else {
                return (None, false);
            };
            (entry.selected.clone().map(|current| (first, current)), true)
        })
        .flatten()
    }

    /// The name of the graph the table session `id` selected, if any.
    pub(crate) fn selected(&self, id: &str) -> Option<String> {
        let hash = id_hash(id);
        self.read(|sessions| {
            sessions
                .iter()
                .find(|entry| entry.id_sha256 == hash)
                .and_then(|entry| entry.selected.clone())
        })
        .flatten()
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
            Self::Table { .. } => {
                let hash = id_hash(id);
                self.read(|sessions| {
                    sessions
                        .iter()
                        .find(|entry| entry.id_sha256 == hash)
                        .map(|entry| client_digest(&entry.client))
                })
                .flatten()
            }
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

    /// A new anonymous session id for the client `client` (`{name, version}`), without a route.
    #[cfg(test)]
    pub(crate) fn create(&self, client: &Value) -> String {
        self.create_routed(client, None, None)
    }

    /// [`Sessions::create_versioned`] with the newest protocol revision.
    #[cfg(test)]
    pub(crate) fn create_routed(
        &self,
        client: &Value,
        route: Option<&str>,
        identity: Option<&str>,
    ) -> String {
        self.create_versioned(client, route, identity, crate::PROTOCOL_VERSIONS[0])
    }

    /// A new session id for the client `client` (`{name, version}`), pinned to `route` when
    /// the host gave one (`<route>~<id>`; the caller checked it with
    /// [`is_valid_session_route`]), bound to `identity` (the `initialize` request's; none =
    /// anonymous), with the protocol revision `initialize` negotiated.
    pub(crate) fn create_versioned(
        &self,
        client: &Value,
        route: Option<&str>,
        identity: Option<&str>,
        protocol_version: &str,
    ) -> String {
        let prefix = route.map_or(String::new(), |route| format!("{route}{ROUTE_SEPARATOR}"));
        match self {
            Self::Table { clients, .. } => {
                let session = format!("{prefix}{:032x}", rand::random::<u128>());
                let entry = TableEntry {
                    id_sha256: id_hash(&session),
                    client: client.clone(),
                    selected: None,
                    renamed_from: None,
                    identity: identity.map(str::to_string),
                    protocol_version: protocol_version.to_string(),
                    created_unix_s: now_unix_s(),
                };
                self.change(|sessions, max| {
                    if sessions.len() >= max {
                        let excess = sessions.len() + 1 - max;
                        sessions.drain(..excess);
                    }
                    sessions.push(entry);
                    ((), true)
                });
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
                let tag = sign(key, &id, route.unwrap_or_default(), identity);
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

    /// Whether `id` names an anonymous session of this endpoint ([`Sessions::knows_as`]).
    #[cfg(test)]
    pub(crate) fn knows(&self, id: &str) -> bool {
        self.knows_as(id, None)
    }

    /// Whether `id` names a session of this endpoint (in the table, or signed by its key) that
    /// belongs to `identity` (strict: anonymous only matches anonymous). A session of another
    /// identity is answered exactly like an unknown id.
    pub(crate) fn knows_as(&self, id: &str, identity: Option<&str>) -> bool {
        match self {
            Self::Table { .. } => {
                let hash = id_hash(id);
                self.read(|sessions| sessions.iter().any(|entry| entry.is(&hash, identity)))
                    .unwrap_or(false)
            }
            Self::Signed { key, .. } => verify(key, id, identity),
        }
    }

    /// Ends the anonymous session `id` ([`Sessions::forget_as`]).
    #[cfg(test)]
    pub(crate) fn forget(&self, id: &str) -> bool {
        self.forget_as(id, None)
    }

    /// Ends the session `id` of `identity` (`DELETE /mcp`): whether it was known (a session
    /// of another identity is not, and stays). A signed id stays valid (nothing is kept to
    /// forget); the client does not use it again.
    pub(crate) fn forget_as(&self, id: &str, identity: Option<&str>) -> bool {
        match self {
            Self::Table { .. } => {
                let hash = id_hash(id);
                self.change(|sessions, _| {
                    let before = sessions.len();
                    sessions.retain(|entry| !entry.is(&hash, identity));
                    let removed = sessions.len() < before;
                    (removed, removed)
                })
                .unwrap_or(false)
            }
            Self::Signed { key, .. } => verify(key, id, identity),
        }
    }

    /// Ends every table session of `identity` (strict: `None` ends the anonymous ones): the
    /// number ended. Signed ids keep nothing to end: `0`.
    pub(crate) fn forget_identity(&self, identity: Option<&str>) -> usize {
        self.change(|sessions, _| {
            let before = sessions.len();
            sessions.retain(|entry| entry.identity.as_deref() != identity);
            let removed = before - sessions.len();
            (removed, removed > 0)
        })
        .unwrap_or(0)
    }

    /// The remembered sessions (table) or client names (signed) and their estimated bytes;
    /// `(0, 0)` while a request holds the lock.
    pub(crate) fn memory_bytes(&self) -> (usize, u64) {
        match self {
            Self::Table { table, clients, .. } => {
                let Ok(table) = table.try_lock() else {
                    return (0, 0);
                };
                let clients = clients.try_read().map_or(0, |clients| clients.bytes());
                let bytes: u64 = table
                    .entries
                    .iter()
                    .map(|entry| {
                        (std::mem::size_of::<TableEntry>()
                            + entry.client.to_string().len()
                            + entry.selected.as_ref().map_or(0, String::capacity)
                            + entry.renamed_from.as_ref().map_or(0, String::capacity)
                            + entry.identity.as_ref().map_or(0, String::capacity)
                            + entry.protocol_version.capacity()) as u64
                    })
                    .sum();
                (table.entries.len(), bytes + clients)
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
        self.insert(digest, client, 1);
    }

    /// Remembers the client of a session loaded from a store, without counting an
    /// `initialize` (none was answered by this process).
    fn restore(&mut self, digest: u64, client: Value) {
        self.insert(digest, client, 0);
    }

    fn insert(&mut self, digest: u64, client: Value, initialized: u64) {
        self.tick += 1;
        if let Some(entry) = self.names.get_mut(&digest) {
            entry.tick = self.tick;
            entry.initialized.fetch_add(initialized, Ordering::Relaxed);
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
                initialized: AtomicU64::new(initialized),
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

/// The HMAC-SHA256 of [`DOMAIN`], `data` (the nonce and the client digest, fixed length), the
/// session's `route` (empty without one, so an id without a route signs exactly what it
/// signed before routes existed) and its `identity` under `key`. An identity is appended as a
/// zero byte (never part of a route), its length (8 bytes, big-endian) and its bytes, so no
/// route/identity pair signs what another pair signs; an anonymous session appends nothing
/// (byte-identical to the ids minted before identities were bound).
fn sign(key: &hmac::Key, data: &[u8], route: &str, identity: Option<&str>) -> hmac::Tag {
    let mut context = hmac::Context::with_key(key);
    context.update(DOMAIN);
    context.update(data);
    context.update(route.as_bytes());
    if let Some(identity) = identity {
        context.update(&[0]);
        context.update(&(identity.len() as u64).to_be_bytes());
        context.update(identity.as_bytes());
    }
    context.sign()
}

/// Whether `id` is a session id signed with `key` for `identity` (compared without stopping
/// at the first difference).
fn verify(key: &hmac::Key, id: &str, identity: Option<&str>) -> bool {
    let (route, id) = split_route(id);
    let Some(bytes) = unhex(id) else {
        return false;
    };
    if bytes.len() != ID_BYTES {
        return false;
    }
    let (signed, tag) = bytes.split_at(NONCE_BYTES + CLIENT_BYTES);
    let expected = sign(key, signed, route.unwrap_or_default(), identity);
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

    #[test]
    fn a_rename_moves_the_selections_and_keeps_one_note_from_the_first_name() {
        let sessions = Sessions::table();
        let one = sessions.create(&json!({"name": "a"}));
        let two = sessions.create(&json!({"name": "b"}));
        let other = sessions.create(&json!({"name": "c"}));
        assert!(sessions.select(&one, "prod") && sessions.select(&two, "Prod"));
        assert!(sessions.select(&other, "acme"));
        assert_eq!(sessions.take_rename_note(&one), None);
        assert_eq!(sessions.rename("PROD", "live"), 2);
        assert_eq!(sessions.rename("live", "main"), 2);
        assert_eq!(sessions.selected(&one).as_deref(), Some("main"));
        assert_eq!(sessions.selected(&other).as_deref(), Some("acme"));
        assert_eq!(
            sessions.take_rename_note(&one),
            Some(("prod".into(), "main".into()))
        );
        assert_eq!(sessions.take_rename_note(&one), None);
        assert_eq!(
            sessions.take_rename_note(&two),
            Some(("Prod".into(), "main".into()))
        );
        // Back to the first name: nothing to tell. A new selection drops the note.
        assert_eq!(sessions.rename("main", "x"), 2);
        assert_eq!(sessions.rename("x", "main"), 2);
        assert_eq!(sessions.take_rename_note(&one), None);
        assert_eq!(sessions.rename("main", "y"), 2);
        assert!(sessions.select(&two, "acme"));
        assert_eq!(sessions.take_rename_note(&two), None);
        assert_eq!(sessions.rename("nothing", "z"), 0);
        let signed = Sessions::signed(&SessionKey::random());
        assert_eq!(signed.rename("prod", "live"), 0);
    }
    #[test]
    fn a_session_belongs_to_the_identity_of_its_initialize() {
        let client = json!({"name": "a", "version": "1"});
        let key = SessionKey::new(&[9u8; 32]).unwrap();
        for sessions in [Sessions::table(), Sessions::signed(&key)] {
            let anonymous = sessions.create_routed(&client, None, None);
            let alice = sessions.create_routed(&client, Some("g@1"), Some("alice"));
            assert!(sessions.knows_as(&anonymous, None));
            assert!(!sessions.knows_as(&anonymous, Some("alice")));
            assert!(!sessions.knows_as(&anonymous, Some("")));
            assert!(sessions.knows_as(&alice, Some("alice")));
            assert!(!sessions.knows_as(&alice, None));
            assert!(!sessions.knows_as(&alice, Some("Alice")));
            assert!(!sessions.forget_as(&alice, Some("bob")));
            assert!(sessions.knows_as(&alice, Some("alice")));
        }
        // A signed id cannot move bytes between its route and its identity.
        let signed = Sessions::signed(&key);
        let id = signed.create_routed(&client, Some("ab"), None);
        let moved = id.replacen("ab~", "a~", 1);
        assert!(signed.knows_as(&id, None));
        assert!(!signed.knows_as(&moved, Some("b")));
        // An anonymous signed id is the one minted before identities were bound.
        let plain = signed.create(&client);
        let (_, raw) = split_route(&plain);
        let bytes = unhex(raw).unwrap();
        let (data, tag) = bytes.split_at(NONCE_BYTES + CLIENT_BYTES);
        let mut context = hmac::Context::with_key(&hmac::Key::new(hmac::HMAC_SHA256, &[9u8; 32]));
        context.update(DOMAIN);
        context.update(data);
        assert_eq!(&context.sign().as_ref()[..TAG_BYTES], tag);
    }

    /// A session store in memory: the last table saved, the number of saves, and whether
    /// saving (or loading) fails.
    #[derive(Default)]
    struct MemoryStore {
        table: Mutex<Vec<StoredSession>>,
        saves: AtomicU64,
        fail_save: std::sync::atomic::AtomicBool,
        fail_load: bool,
    }

    impl SessionStore for MemoryStore {
        fn load(&self) -> Result<Vec<StoredSession>, McpError> {
            if self.fail_load {
                return Err(McpError::Failed("the store is unreadable".into()));
            }
            Ok(self.table.lock().unwrap().clone())
        }

        fn save(&self, sessions: &[StoredSession]) -> Result<(), McpError> {
            self.saves.fetch_add(1, Ordering::SeqCst);
            if self.fail_save.load(Ordering::SeqCst) {
                return Err(McpError::Failed("the disk is full".into()));
            }
            *self.table.lock().unwrap() = sessions.to_vec();
            Ok(())
        }
    }

    impl MemoryStore {
        fn stored(&self) -> Vec<StoredSession> {
            self.table.lock().unwrap().clone()
        }

        fn saves(&self) -> u64 {
            self.saves.load(Ordering::SeqCst)
        }
    }

    fn stored_table(store: &Arc<MemoryStore>) -> Sessions {
        Sessions::table()
            .with_store(store.clone())
            .expect("the store loads")
    }

    #[test]
    fn the_table_keeps_only_the_hash_of_an_id() {
        let store = Arc::new(MemoryStore::default());
        let sessions = stored_table(&store);
        let id = sessions.create_routed(&json!({"name": "a", "version": "1"}), Some("g@1"), None);
        let stored = store.stored();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].id_sha256(), hex(&id_hash(&id)));
        assert_eq!(stored[0].id_sha256().len(), 64);
        assert!(!format!("{stored:?}").contains(&id[4..]));
        // The plain table hashes the presented id as well.
        let plain = Sessions::table();
        let id = plain.create(&json!({"name": "a"}));
        // The hash itself is no session id.
        assert!(plain.knows(&id) && !plain.knows(&hex(&id_hash(&id))));
    }

    #[test]
    fn a_restored_table_keeps_identity_selection_note_and_protocol() {
        let store = Arc::new(MemoryStore::default());
        let client = json!({"name": "agent", "version": "2"});
        let before = stored_table(&store);
        let alice = before.create_versioned(&client, None, Some("alice"), "2025-06-18");
        let anonymous = before.create(&client);
        assert!(before.select(&alice, "prod"));
        assert_eq!(before.rename("prod", "live"), 1);
        drop(before);

        let stored = store.stored();
        assert_eq!(stored.len(), 2);
        assert_eq!(stored[0].identity(), Some("alice"));
        assert_eq!(stored[0].selected(), Some("live"));
        assert_eq!(stored[0].renamed_from(), Some("prod"));
        assert_eq!(stored[0].protocol_version(), "2025-06-18");
        assert_eq!(stored[0].client(), &client);
        assert!(stored[0].created_unix_s() > 0);
        assert_eq!(stored[1].identity(), None);

        let after = stored_table(&store);
        assert!(after.knows_as(&alice, Some("alice")));
        assert!(!after.knows_as(&alice, None) && !after.knows_as(&alice, Some("bob")));
        assert!(after.knows_as(&anonymous, None));
        assert_eq!(after.selected(&alice).as_deref(), Some("live"));
        assert_eq!(after.client_of(&alice), Some(client_digest(&client)));
        // The client names are back for the statistics, without a counted initialize.
        let clients = after.clients();
        let listed = clients.read().unwrap().to_json();
        assert_eq!(listed[0]["initialized"], 0);
        // Telling the note is saved, so a second restart does not tell it again.
        let saves = store.saves();
        assert_eq!(
            after.take_rename_note(&alice),
            Some(("prod".into(), "live".into()))
        );
        assert_eq!(store.saves(), saves + 1);
        assert_eq!(store.stored()[0].renamed_from(), None);
        assert_eq!(after.take_rename_note(&alice), None);
        assert_eq!(store.saves(), saves + 1);
    }

    #[test]
    fn every_change_saves_and_reads_do_not() {
        let store = Arc::new(MemoryStore::default());
        let sessions = stored_table(&store).with_max(2);
        let client = json!({"name": "a"});
        let one = sessions.create(&client);
        let two = sessions.create_routed(&client, None, Some("bob"));
        assert_eq!(store.saves(), 2);
        assert!(sessions.knows(&one) && sessions.selected(&one).is_none());
        assert!(!sessions.select("unknown", "x") && !sessions.forget("unknown"));
        assert_eq!(sessions.rename("nothing", "x"), 0);
        assert_eq!(sessions.take_rename_note(&one), None);
        assert_eq!(store.saves(), 2);
        assert!(sessions.select(&one, "a"));
        assert_eq!(sessions.rename("a", "b"), 1);
        assert_eq!(store.saves(), 4);
        // Evicting the oldest is part of the create's save.
        let three = sessions.create(&client);
        assert_eq!(store.saves(), 5);
        let hashes: Vec<String> = store
            .stored()
            .iter()
            .map(|s| s.id_sha256().into())
            .collect();
        assert_eq!(hashes, [hex(&id_hash(&two)), hex(&id_hash(&three))]);
        assert!(sessions.forget(&three));
        assert_eq!(store.saves(), 6);
        assert_eq!(sessions.forget_identity(Some("alice")), 0);
        assert_eq!(store.saves(), 6);
        assert_eq!(sessions.forget_identity(Some("bob")), 1);
        assert_eq!(store.saves(), 7);
        assert!(store.stored().is_empty() && !sessions.knows_as(&two, Some("bob")));
    }

    #[test]
    fn a_load_keeps_the_newest_and_skips_bad_hashes() {
        let store = Arc::new(MemoryStore::default());
        let row = |hash: String, created: u64| {
            StoredSession::new(
                hash,
                None,
                Some(format!("g{created}")),
                None,
                "2025-03-26",
                json!({"name": "a", "version": ""}),
                created,
            )
        };
        let ids: Vec<String> = (0..4).map(|i| format!("{i:032x}")).collect();
        *store.table.lock().unwrap() = vec![
            row(hex(&id_hash(&ids[3])), 30),
            row(hex(&id_hash(&ids[0])), 0),
            row("not hex".into(), 5),
            row(hex(&id_hash(&ids[2])), 20),
            row(hex(&id_hash(&ids[1])), 10),
            row(hex(&[1u8; 31]), 40),
        ];
        let sessions = Sessions::table()
            .with_max(2)
            .with_store(store.clone())
            .unwrap();
        assert!(!sessions.knows(&ids[0]) && !sessions.knows(&ids[1]));
        assert_eq!(sessions.selected(&ids[2]).as_deref(), Some("g20"));
        assert_eq!(sessions.selected(&ids[3]).as_deref(), Some("g30"));
        // Upper-case hex is read as well.
        *store.table.lock().unwrap() = vec![row(hex(&id_hash(&ids[0])).to_uppercase(), 1)];
        assert!(stored_table(&store).knows(&ids[0]));
        // Loading does not save.
        assert_eq!(store.saves(), 0);
        // A store that cannot load, and signed ids, are refused.
        let unreadable = Arc::new(MemoryStore {
            fail_load: true,
            ..MemoryStore::default()
        });
        let error = Sessions::table().with_store(unreadable).err().unwrap();
        assert!(error.to_string().contains("unreadable"), "{error}");
        let signed = Sessions::signed(&SessionKey::random()).with_store(store);
        assert!(
            signed
                .err()
                .unwrap()
                .to_string()
                .contains("with_session_key")
        );
    }

    #[test]
    fn an_older_snapshot_never_overwrites_a_newer_one() {
        let store = Arc::new(MemoryStore::default());
        let persist = Persist::new(store.clone());
        let row = |name: &str| {
            StoredSession::new(
                hex(&[0u8; 32]),
                None,
                Some(name.into()),
                None,
                "v",
                json!({}),
                1,
            )
        };
        persist.write(2, &[row("newer")]);
        persist.write(1, &[row("older")]);
        persist.write(2, &[row("same")]);
        assert_eq!(store.saves(), 1);
        assert_eq!(store.stored()[0].selected(), Some("newer"));
        persist.write(3, &[row("newest")]);
        assert_eq!(store.stored()[0].selected(), Some("newest"));

        // Concurrent changes: the store ends with the table as it is in memory.
        let sessions = Arc::new(stored_table(&store).with_max(16));
        let threads: Vec<_> = (0..8)
            .map(|t| {
                let sessions = sessions.clone();
                std::thread::spawn(move || {
                    for i in 0..50 {
                        let id = sessions.create(&json!({"name": format!("c{t}")}));
                        sessions.select(&id, &format!("g{t}-{i}"));
                        if i % 3 == 0 {
                            sessions.forget(&id);
                        }
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        let memory: Vec<StoredSession> = sessions
            .read(|entries| entries.iter().map(TableEntry::stored).collect())
            .unwrap();
        assert!(!memory.is_empty());
        assert_eq!(store.stored(), memory);
    }

    /// Counts the `WARN` events of the thread it is the default subscriber of.
    #[derive(Default)]
    struct WarnCounter(AtomicU64);

    impl tracing::Subscriber for WarnCounter {
        fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
        fn event(&self, event: &tracing::Event<'_>) {
            if *event.metadata().level() == tracing::Level::WARN {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
    }

    #[test]
    fn a_failing_store_warns_and_the_sessions_go_on() {
        let store = Arc::new(MemoryStore::default());
        store.fail_save.store(true, Ordering::SeqCst);
        let sessions = stored_table(&store);
        let warnings = Arc::new(WarnCounter::default());
        let id = tracing::subscriber::with_default(warnings.clone(), || {
            let id = sessions.create_routed(&json!({"name": "a"}), None, Some("alice"));
            assert!(sessions.select(&id, "prod"));
            id
        });
        assert_eq!(warnings.0.load(Ordering::SeqCst), 2);
        assert_eq!(store.saves(), 2);
        assert!(store.stored().is_empty());
        assert!(sessions.knows_as(&id, Some("alice")));
        assert_eq!(sessions.selected(&id).as_deref(), Some("prod"));
        // Once the store works again, the next change saves the whole table.
        store.fail_save.store(false, Ordering::SeqCst);
        assert_eq!(sessions.rename("prod", "live"), 1);
        assert_eq!(store.stored().len(), 1);
        assert_eq!(store.stored()[0].selected(), Some("live"));
    }
}
