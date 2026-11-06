//! MCP session ids: a bounded table of the ids `initialize` assigned (the default), or
//! stateless ids signed with a server key ([`SessionKey`], [`crate::Mcp::with_session_key`]):
//! `hex(nonce || client digest || tag)`, the tag an HMAC-SHA256 of the nonce and the digest
//! under the key, so any process holding the same key accepts the id without a table (several
//! replicas behind a load balancer), and no number of clients evicts another.

use std::collections::HashMap;
use std::sync::Mutex;

use ring::{digest, hmac};
use serde_json::Value;

use crate::host::McpError;

/// Most MCP sessions the table remembers; the oldest is forgotten first (its client initializes
/// again).
const MAX_SESSIONS: usize = 64;

/// Most client names (`clientInfo` of `initialize`) a stateless endpoint remembers for its
/// statistics; the least recently initialized one is forgotten first.
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

/// The MCP sessions of an endpoint.
pub(crate) enum Sessions {
    /// Session id -> `clientInfo` of its `initialize`, in the order they were created.
    Table(Mutex<Vec<(String, Value)>>),
    /// Signed ids, checked without a table; the client names for the statistics.
    Signed {
        key: hmac::Key,
        clients: Mutex<ClientNames>,
    },
}

impl Sessions {
    /// The table of today's dev server.
    pub(crate) fn table() -> Self {
        Self::Table(Mutex::new(Vec::new()))
    }

    /// Stateless ids signed with `key`.
    pub(crate) fn signed(key: &SessionKey) -> Self {
        Self::Signed {
            key: hmac::Key::new(hmac::HMAC_SHA256, &key.0),
            clients: Mutex::new(ClientNames::default()),
        }
    }

    /// Whether the ids are signed (stateless).
    pub(crate) fn is_signed(&self) -> bool {
        matches!(self, Self::Signed { .. })
    }

    /// A new session id for the client `client` (`{name, version}`).
    pub(crate) fn create(&self, client: &Value) -> String {
        match self {
            Self::Table(sessions) => {
                let session = format!("{:032x}", rand::random::<u128>());
                if let Ok(mut sessions) = sessions.lock() {
                    if sessions.len() >= MAX_SESSIONS {
                        sessions.remove(0);
                    }
                    sessions.push((session.clone(), client.clone()));
                }
                session
            }
            Self::Signed { key, clients } => {
                let digest = client_digest(client);
                if let Ok(mut clients) = clients.lock() {
                    clients.remember(digest, client.clone());
                }
                let mut id = Vec::with_capacity(ID_BYTES);
                id.extend((0..NONCE_BYTES).map(|_| rand::random::<u8>()));
                id.extend_from_slice(&digest.to_be_bytes());
                let tag = sign(key, &id);
                id.extend_from_slice(&tag.as_ref()[..TAG_BYTES]);
                hex(&id)
            }
        }
    }

    /// Whether `id` names a session of this endpoint (in the table, or signed by its key).
    pub(crate) fn knows(&self, id: &str) -> bool {
        match self {
            Self::Table(sessions) => sessions
                .lock()
                .is_ok_and(|sessions| sessions.iter().any(|(known, _)| known == id)),
            Self::Signed { key, .. } => verify(key, id),
        }
    }

    /// Ends the session `id` (`DELETE /mcp`): whether it was known. A signed id stays valid
    /// (nothing is kept to forget); the client does not use it again.
    pub(crate) fn forget(&self, id: &str) -> bool {
        match self {
            Self::Table(sessions) => sessions.lock().is_ok_and(|mut sessions| {
                let before = sessions.len();
                sessions.retain(|(known, _)| known != id);
                sessions.len() < before
            }),
            Self::Signed { key, .. } => verify(key, id),
        }
    }

    /// The remembered sessions (table) or client names (signed) and their estimated bytes;
    /// `(0, 0)` while a request holds the lock.
    pub(crate) fn memory_bytes(&self) -> (usize, u64) {
        match self {
            Self::Table(sessions) => {
                let Ok(sessions) = sessions.try_lock() else {
                    return (0, 0);
                };
                let bytes = sessions
                    .iter()
                    .map(|(id, value)| {
                        (std::mem::size_of::<(String, Value)>()
                            + id.capacity()
                            + value.to_string().len()) as u64
                    })
                    .sum();
                (sessions.len(), bytes)
            }
            Self::Signed { clients, .. } => {
                let Ok(clients) = clients.try_lock() else {
                    return (0, 0);
                };
                (clients.names.len(), clients.bytes())
            }
        }
    }
}

/// The client names of a stateless endpoint, by client digest: at most
/// [`MAX_CLIENT_NAMES`], the least recently initialized forgotten first.
#[derive(Default)]
pub(crate) struct ClientNames {
    names: HashMap<u64, (Value, u64)>,
    tick: u64,
}

impl ClientNames {
    fn remember(&mut self, digest: u64, client: Value) {
        self.tick += 1;
        if !self.names.contains_key(&digest) && self.names.len() >= MAX_CLIENT_NAMES {
            let oldest = self
                .names
                .iter()
                .min_by_key(|(_, (_, tick))| *tick)
                .map(|(digest, _)| *digest);
            if let Some(oldest) = oldest {
                self.names.remove(&oldest);
            }
        }
        self.names.insert(digest, (client, self.tick));
    }

    fn bytes(&self) -> u64 {
        self.names
            .values()
            .map(|(client, _)| {
                (std::mem::size_of::<(u64, Value, u64)>() + client.to_string().len()) as u64
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

/// The HMAC-SHA256 of [`DOMAIN`] and `data` under `key`.
fn sign(key: &hmac::Key, data: &[u8]) -> hmac::Tag {
    let mut context = hmac::Context::with_key(key);
    context.update(DOMAIN);
    context.update(data);
    context.sign()
}

/// Whether `id` is a session id signed with `key` (compared without stopping at the first
/// difference).
fn verify(key: &hmac::Key, id: &str) -> bool {
    let Some(bytes) = unhex(id) else {
        return false;
    };
    if bytes.len() != ID_BYTES {
        return false;
    }
    let (signed, tag) = bytes.split_at(NONCE_BYTES + CLIENT_BYTES);
    let expected = sign(key, signed);
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
}
