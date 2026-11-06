//! Usage statistics of the endpoint ([`McpStats`]): counters since the process started, kept in
//! process memory only (never written anywhere, lost on a restart), cheap under parallel load:
//! every counter is an atomic; the maps of tools, saved queries and clients take a read lock to
//! find their entry (a write lock only for a name seen the first time, or an `initialize`).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::host::McpError;
use crate::sessions::ClientNames;

/// Most tool names and most saved-query names counted each; calls of further names are counted
/// under `(other)`.
pub const MAX_COUNTED_NAMES: usize = 1024;

/// The entry of the names beyond [`MAX_COUNTED_NAMES`] (and of unknown tools: `(unknown)`).
const OTHER: &str = "(other)";
const UNKNOWN_TOOL: &str = "(unknown)";

/// The upper bounds (exclusive) of the latency histogram's buckets in milliseconds; the last
/// bucket holds everything slower.
pub const LATENCY_BUCKETS_MS: [u64; 5] = [1, 10, 100, 1_000, 10_000];

/// Distinct bits of one minute's sketch of the active sessions (linear counting).
const ACTIVE_BITS: usize = 1 << 16;
const ACTIVE_WORDS: usize = ACTIVE_BITS / 64;
/// The minutes the active-session window spans (the current one and the five before it).
const ACTIVE_MINUTES: u64 = 5;
const ACTIVE_SLOTS: usize = ACTIVE_MINUTES as usize + 1;

/// The JSON-RPC methods counted by name; any other is `(other)`, every notification
/// `(notification)`.
const METHODS: [&str; 10] = [
    "initialize",
    "ping",
    "tools/list",
    "tools/call",
    "resources/list",
    "resources/templates/list",
    "resources/read",
    "prompts/list",
    "(notification)",
    "(other)",
];

/// Why a request to the endpoint was refused, for [`McpStats::record_refusal`]. The endpoint
/// counts the ones it decides itself (`UnknownSession`, `Unauthorized`, `InvalidRequest`); the
/// host reports its own (its queue, its connection cap).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Refusal {
    /// The host's job queue was full.
    QueueFull,
    /// A job waited longer than the host's queue timeout.
    QueueTimeout,
    /// The client had too many jobs running or waiting.
    ClientLimit,
    /// The host handled too many connections at once.
    TooManyConnections,
    /// The host was stopping.
    Stopping,
    /// The request named an unknown or invalid MCP session id (`404`).
    UnknownSession,
    /// The request lacked the bearer token (`401`).
    Unauthorized,
    /// A request the endpoint could not read (a wrong `Accept`, `Content-Type`, protocol
    /// version, size, JSON).
    InvalidRequest,
}

impl Refusal {
    const ALL: [Refusal; 8] = [
        Refusal::QueueFull,
        Refusal::QueueTimeout,
        Refusal::ClientLimit,
        Refusal::TooManyConnections,
        Refusal::Stopping,
        Refusal::UnknownSession,
        Refusal::Unauthorized,
        Refusal::InvalidRequest,
    ];

    fn index(self) -> usize {
        match self {
            Refusal::QueueFull => 0,
            Refusal::QueueTimeout => 1,
            Refusal::ClientLimit => 2,
            Refusal::TooManyConnections => 3,
            Refusal::Stopping => 4,
            Refusal::UnknownSession => 5,
            Refusal::Unauthorized => 6,
            Refusal::InvalidRequest => 7,
        }
    }

    /// The key of the refusal in [`McpStats::to_json`].
    pub fn key(self) -> &'static str {
        match self {
            Refusal::QueueFull => "queueFull",
            Refusal::QueueTimeout => "queueTimeout",
            Refusal::ClientLimit => "clientLimit",
            Refusal::TooManyConnections => "tooManyConnections",
            Refusal::Stopping => "stopping",
            Refusal::UnknownSession => "unknownSession",
            Refusal::Unauthorized => "unauthorized",
            Refusal::InvalidRequest => "invalidRequest",
        }
    }
}

/// The counters of one tool or saved query.
#[derive(Default)]
struct Counters {
    calls: AtomicU64,
    errors: AtomicU64,
    total_us: AtomicU64,
    max_us: AtomicU64,
    answer_chars: AtomicU64,
    histogram: [AtomicU64; LATENCY_BUCKETS_MS.len() + 1],
}

impl Counters {
    fn record(&self, elapsed_us: u64, is_error: bool, answer_chars: u64) {
        self.calls.fetch_add(1, Ordering::Relaxed);
        if is_error {
            self.errors.fetch_add(1, Ordering::Relaxed);
        }
        self.total_us.fetch_add(elapsed_us, Ordering::Relaxed);
        self.max_us.fetch_max(elapsed_us, Ordering::Relaxed);
        self.answer_chars.fetch_add(answer_chars, Ordering::Relaxed);
        let bucket = LATENCY_BUCKETS_MS
            .iter()
            .position(|bound| elapsed_us < bound * 1000)
            .unwrap_or(LATENCY_BUCKETS_MS.len());
        self.histogram[bucket].fetch_add(1, Ordering::Relaxed);
    }

    fn to_json(&self, name: &str) -> Value {
        let calls = self.calls.load(Ordering::Relaxed);
        let total_us = self.total_us.load(Ordering::Relaxed);
        json!({
            "name": name,
            "calls": calls,
            "errors": self.errors.load(Ordering::Relaxed),
            "totalMs": ms(total_us),
            "meanMs": total_us.checked_div(calls).map_or(0.0, ms),
            "maxMs": ms(self.max_us.load(Ordering::Relaxed)),
            "histogram": self.histogram.iter().map(|b| b.load(Ordering::Relaxed)).collect::<Vec<_>>(),
            "answerChars": self.answer_chars.load(Ordering::Relaxed),
        })
    }
}

/// Microseconds as milliseconds with three decimals.
fn ms(us: u64) -> f64 {
    us as f64 / 1000.0
}

/// Counters by name, at most [`MAX_COUNTED_NAMES`] names (then [`OTHER`]).
#[derive(Default)]
struct CounterMap(RwLock<HashMap<String, Arc<Counters>>>);

impl CounterMap {
    fn entry(&self, name: &str) -> Arc<Counters> {
        {
            let map = self
                .0
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(counters) = map.get(name) {
                return counters.clone();
            }
        }
        let mut map = self
            .0
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let key = if map.len() >= MAX_COUNTED_NAMES && !map.contains_key(name) {
            OTHER
        } else {
            name
        };
        map.entry(key.to_string()).or_default().clone()
    }

    fn record(&self, name: &str, elapsed_us: u64, is_error: bool, answer_chars: u64) {
        self.entry(name).record(elapsed_us, is_error, answer_chars);
    }

    /// The entries, most calls first (then by name).
    fn to_json(&self) -> Vec<Value> {
        let map = self
            .0
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut entries: Vec<(&String, &Arc<Counters>)> = map.iter().collect();
        entries.sort_by(|a, b| {
            b.1.calls
                .load(Ordering::Relaxed)
                .cmp(&a.1.calls.load(Ordering::Relaxed))
                .then_with(|| a.0.cmp(b.0))
        });
        entries
            .into_iter()
            .map(|(name, counters)| counters.to_json(name))
            .collect()
    }

    fn bytes(&self) -> u64 {
        let map = self
            .0
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        map.keys()
            .map(|name| {
                (name.capacity()
                    + std::mem::size_of::<(String, Arc<Counters>)>()
                    + std::mem::size_of::<Counters>()) as u64
            })
            .sum()
    }
}

/// One minute's sketch of the session ids seen: a bitmap of [`ACTIVE_BITS`] bits.
struct MinuteSketch {
    /// The minute since the start it holds (`u64::MAX`: none yet).
    minute: AtomicU64,
    words: Box<[AtomicU64]>,
}

/// The sessions active in about the last five minutes: one sketch per minute in a ring, their
/// union estimated by linear counting (approximate; within a few percent up to tens of
/// thousands of sessions).
struct ActiveSessions {
    slots: Vec<MinuteSketch>,
}

impl ActiveSessions {
    fn new() -> Self {
        Self {
            slots: (0..ACTIVE_SLOTS)
                .map(|_| MinuteSketch {
                    minute: AtomicU64::new(u64::MAX),
                    words: (0..ACTIVE_WORDS).map(|_| AtomicU64::new(0)).collect(),
                })
                .collect(),
        }
    }

    fn mark(&self, id: &str, minute: u64) {
        let slot = &self.slots[(minute % ACTIVE_SLOTS as u64) as usize];
        let held = slot.minute.load(Ordering::Acquire);
        if held != minute
            && slot
                .minute
                .compare_exchange(held, minute, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        {
            // A new minute: the winner clears the slot (bits set meanwhile may be lost: the
            // figure is an estimate).
            for word in slot.words.iter() {
                word.store(0, Ordering::Relaxed);
            }
        }
        let bit = (fnv1a(id) % ACTIVE_BITS as u64) as usize;
        slot.words[bit / 64].fetch_or(1 << (bit % 64), Ordering::Relaxed);
    }

    fn estimate(&self, minute: u64) -> u64 {
        let live: Vec<&MinuteSketch> = self
            .slots
            .iter()
            .filter(|slot| {
                let held = slot.minute.load(Ordering::Acquire);
                held <= minute && minute - held <= ACTIVE_MINUTES
            })
            .collect();
        if live.is_empty() {
            return 0;
        }
        let zeros: u64 = (0..ACTIVE_WORDS)
            .map(|i| {
                let union = live.iter().fold(0u64, |acc, slot| {
                    acc | slot.words[i].load(Ordering::Relaxed)
                });
                u64::from(union.count_zeros())
            })
            .sum();
        if zeros == 0 {
            return ACTIVE_BITS as u64;
        }
        let m = ACTIVE_BITS as f64;
        (-m * (zeros as f64 / m).ln()).round() as u64
    }
}

/// FNV-1a 64 of `text` (the session ids are random already; this spreads any id).
fn fnv1a(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// The usage statistics of one endpoint ([`crate::Mcp::stats`]): requests by JSON-RPC method,
/// tool calls per tool and per saved query (calls, errors, total and largest latency, a
/// latency histogram, answer characters), requests per client name, sessions initialized and
/// active, refusals by kind. Since the process started; in memory only.
pub struct McpStats {
    started: Instant,
    started_at_ms: u64,
    requests: AtomicU64,
    without_session: AtomicU64,
    busy: AtomicU64,
    methods: [AtomicU64; METHODS.len()],
    tools: CounterMap,
    saved_queries: CounterMap,
    initialized: AtomicU64,
    active: ActiveSessions,
    refusals: [AtomicU64; Refusal::ALL.len()],
    /// The client names of the endpoint's sessions (shared with them), with their counters.
    clients: Arc<RwLock<ClientNames>>,
}

impl std::fmt::Debug for McpStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpStats")
            .field("requests", &self.requests.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl McpStats {
    pub(crate) fn new(clients: Arc<RwLock<ClientNames>>) -> Self {
        Self {
            started: Instant::now(),
            started_at_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_millis() as u64),
            requests: AtomicU64::new(0),
            without_session: AtomicU64::new(0),
            busy: AtomicU64::new(0),
            methods: Default::default(),
            tools: CounterMap::default(),
            saved_queries: CounterMap::default(),
            initialized: AtomicU64::new(0),
            active: ActiveSessions::new(),
            refusals: Default::default(),
            clients,
        }
    }

    fn minute(&self) -> u64 {
        self.started.elapsed().as_secs() / 60
    }

    /// Counts a refusal of a request to the endpoint (the host reports its own: a full queue,
    /// a queue timeout, a client over its limit, too many connections, stopping).
    pub fn record_refusal(&self, refusal: Refusal) {
        self.refusals[refusal.index()].fetch_add(1, Ordering::Relaxed);
    }

    /// One JSON-RPC message of a known (or no) session: its method (`None`: a notification).
    pub(crate) fn record_request(
        &self,
        method: Option<&str>,
        session: Option<&str>,
        client: Option<u64>,
        tool_call: bool,
    ) {
        self.requests.fetch_add(1, Ordering::Relaxed);
        let index = match method {
            None => METHODS.len() - 2,
            Some(method) => METHODS[..METHODS.len() - 2]
                .iter()
                .position(|m| *m == method)
                .unwrap_or(METHODS.len() - 1),
        };
        self.methods[index].fetch_add(1, Ordering::Relaxed);
        match session {
            Some(id) => self.active.mark(id, self.minute()),
            None => {
                self.without_session.fetch_add(1, Ordering::Relaxed);
            }
        }
        if let Some(digest) = client {
            let clients = self
                .clients
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            clients.count_request(digest, tool_call);
        }
    }

    /// A session was initialized (its id is marked active).
    pub(crate) fn record_initialize(&self, session: &str) {
        self.initialized.fetch_add(1, Ordering::Relaxed);
        self.active.mark(session, self.minute());
    }

    /// A request answered `503` because the host was busy.
    pub(crate) fn record_busy(&self) {
        self.busy.fetch_add(1, Ordering::Relaxed);
    }

    /// One tool call: the tool `name` (`None`: an unknown tool), the saved query it ran, if
    /// any, its time, whether it failed and its answer's characters.
    pub(crate) fn record_tool(
        &self,
        name: Option<&str>,
        saved_query: Option<&str>,
        elapsed_us: u64,
        is_error: bool,
        answer_chars: u64,
    ) {
        self.tools.record(
            name.unwrap_or(UNKNOWN_TOOL),
            elapsed_us,
            is_error,
            answer_chars,
        );
        if let Some(query) = saved_query {
            self.saved_queries
                .record(query, elapsed_us, is_error, answer_chars);
        }
    }

    /// Estimated bytes of the counters (the tool and saved-query maps and the session
    /// sketches; the client names are counted with the sessions).
    pub(crate) fn bytes(&self) -> u64 {
        (std::mem::size_of::<Self>() + ACTIVE_SLOTS * ACTIVE_WORDS * 8) as u64
            + self.tools.bytes()
            + self.saved_queries.bytes()
    }

    /// The statistics as JSON (`GET /api/mcp/stats`, the `usage_statistics` tool): `since`
    /// (`startedAtMs`, `uptimeS`), `requests`, `methods`, `sessions`, `refusals`,
    /// `latencyBucketsMs`, `tools`, `savedQueries` and `clients` (lists, most calls first),
    /// and `note`. `Err` is kept for a later source that can fail; the counters cannot.
    pub fn to_json(&self) -> Result<Value, McpError> {
        let methods: serde_json::Map<String, Value> = METHODS
            .iter()
            .zip(&self.methods)
            .map(|(name, count)| (name.to_string(), json!(count.load(Ordering::Relaxed))))
            .collect();
        let refusals: serde_json::Map<String, Value> = Refusal::ALL
            .iter()
            .map(|refusal| {
                (
                    refusal.key().to_string(),
                    json!(self.refusals[refusal.index()].load(Ordering::Relaxed)),
                )
            })
            .collect();
        let (clients, clients_kept) = {
            let clients = self
                .clients
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            (clients.to_json(), clients.len())
        };
        Ok(json!({
            "startedAtMs": self.started_at_ms,
            "uptimeS": self.started.elapsed().as_secs(),
            "note": "since the process started; kept in memory only, lost on a restart, never written",
            "requests": {
                "total": self.requests.load(Ordering::Relaxed),
                "withoutSession": self.without_session.load(Ordering::Relaxed),
                "busy503": self.busy.load(Ordering::Relaxed),
            },
            "methods": methods,
            "sessions": {
                "initialized": self.initialized.load(Ordering::Relaxed),
                "activeLast5Min": self.active.estimate(self.minute()),
                "activeIsEstimate": true,
            },
            "refusals": refusals,
            "latencyBucketsMs": LATENCY_BUCKETS_MS,
            "tools": self.tools.to_json(),
            "savedQueries": self.saved_queries.to_json(),
            "clients": clients,
            "clientsKept": clients_kept,
            "clientsMax": crate::sessions::MAX_CLIENT_NAMES,
        }))
    }

    /// A short summary for a status page: `{requests, toolCalls, toolErrors,
    /// sessionsInitialized, activeLast5Min}`.
    pub fn summary(&self) -> Value {
        let tools = self
            .tools
            .0
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (calls, errors) = tools.values().fold((0u64, 0u64), |(c, e), counters| {
            (
                c + counters.calls.load(Ordering::Relaxed),
                e + counters.errors.load(Ordering::Relaxed),
            )
        });
        json!({
            "requests": self.requests.load(Ordering::Relaxed),
            "toolCalls": calls,
            "toolErrors": errors,
            "sessionsInitialized": self.initialized.load(Ordering::Relaxed),
            "activeLast5Min": self.active.estimate(self.minute()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats() -> McpStats {
        McpStats::new(Arc::new(RwLock::new(ClientNames::default())))
    }

    #[test]
    fn tool_counters_add_up_with_a_histogram() {
        let stats = stats();
        stats.record_tool(Some("gql"), None, 500, false, 10);
        stats.record_tool(Some("gql"), None, 25_000, true, 5);
        stats.record_tool(
            Some("run_saved_query"),
            Some("kb.symbol"),
            2_000_000,
            false,
            7,
        );
        stats.record_tool(None, None, 1, true, 0);
        let json = stats.to_json().unwrap();
        let gql = &json["tools"][0];
        assert_eq!(gql["name"], "gql");
        assert_eq!(gql["calls"], 2);
        assert_eq!(gql["errors"], 1);
        assert_eq!(gql["answerChars"], 15);
        assert_eq!(gql["maxMs"], 25.0);
        assert_eq!(gql["histogram"], json!([1, 0, 1, 0, 0, 0]));
        assert_eq!(json["savedQueries"][0]["name"], "kb.symbol");
        assert_eq!(
            json["savedQueries"][0]["histogram"],
            json!([0, 0, 0, 0, 1, 0])
        );
        assert!(
            json["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["name"] == "(unknown)")
        );
    }

    #[test]
    fn names_beyond_the_bound_are_counted_as_other() {
        let stats = stats();
        for i in 0..MAX_COUNTED_NAMES + 5 {
            stats.record_tool(Some("run_saved_query"), Some(&format!("q{i}")), 1, false, 0);
        }
        let json = stats.to_json().unwrap();
        let queries = json["savedQueries"].as_array().unwrap();
        assert_eq!(queries.len(), MAX_COUNTED_NAMES + 1);
        let other = queries.iter().find(|q| q["name"] == OTHER).unwrap();
        assert_eq!(other["calls"], 5);
    }

    #[test]
    fn active_sessions_are_estimated_over_the_window() {
        let active = ActiveSessions::new();
        for i in 0..3000 {
            active.mark(&format!("{:032x}", i * 7919), 0);
        }
        // The same ids again in a later minute are not counted twice.
        for i in 0..3000 {
            active.mark(&format!("{:032x}", i * 7919), 3);
        }
        let estimate = active.estimate(3);
        assert!((2900..=3100).contains(&estimate), "{estimate}");
        // Six minutes later the window is empty.
        assert_eq!(active.estimate(9), 0);
        // Minute 6 reuses the slot of minute 0 and clears it first: minute 10 sees only "x".
        active.mark("x", 6);
        let estimate = active.estimate(6);
        assert!((2900..=3100).contains(&estimate), "{estimate}");
        assert_eq!(active.estimate(10), 1);
    }

    #[test]
    fn methods_and_refusals_are_counted() {
        let stats = stats();
        stats.record_request(Some("tools/call"), Some("a"), None, true);
        stats.record_request(Some("weird"), None, None, false);
        stats.record_request(None, Some("a"), None, false);
        stats.record_refusal(Refusal::QueueFull);
        stats.record_refusal(Refusal::UnknownSession);
        let json = stats.to_json().unwrap();
        assert_eq!(json["requests"]["total"], 3);
        assert_eq!(json["requests"]["withoutSession"], 1);
        assert_eq!(json["methods"]["tools/call"], 1);
        assert_eq!(json["methods"]["(other)"], 1);
        assert_eq!(json["methods"]["(notification)"], 1);
        assert_eq!(json["refusals"]["queueFull"], 1);
        assert_eq!(json["refusals"]["unknownSession"], 1);
        assert_eq!(json["refusals"]["tooManyConnections"], 0);
        assert_eq!(json["sessions"]["activeLast5Min"], 1);
    }
}
