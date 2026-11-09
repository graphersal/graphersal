//! One MCP session as a connection ([`Connection`]): the state a stream keeps between its
//! messages (initialized, the negotiated protocol version, the client's info and capabilities),
//! the host its jobs run on, and the outbound channel of the server's own messages
//! (`notifications/tools/list_changed`, `notifications/progress`).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::host::{ClientRef, Host};
use crate::protocol::{MCP_CLIENT, Mcp, Outcome, Peer, list_changed_notice, parse_message};
use crate::stats::Refusal;

/// How often a running tool call whose request asked for progress (`_meta.progressToken`)
/// tells the client it still runs, by default ([`Connection::with_progress_interval`]).
pub const DEFAULT_PROGRESS_INTERVAL: Duration = Duration::from_secs(2);

/// One MCP session over a stream (MCP's stdio framing, a socket after a host's handshake):
/// what it negotiated in `initialize`, the host its session jobs run on and the channel of the
/// messages the server sends on its own.
///
/// [`Connection::handle`] answers one JSON-RPC message with the lines to send back (its answer,
/// then a `notifications/tools/list_changed` the message caused). The server's own messages
/// while nothing is asked (progress of a running call, a tool list changed by another client)
/// go to the outbound channel ([`Connection::with_outbound`]); [`crate::serve_lines`] wires
/// both to a `Read`/`Write` pair. The answers are the HTTP endpoint's ([`Mcp::handle`]) byte
/// for byte; what differs:
///
/// - `initialize` assigns no session id (the connection is the session) and announces
///   `tools.listChanged: true` when saved queries are tools of their own: after a tool call of
///   any client of the same [`Mcp`] (or [`Mcp::check_tools_changed`]) that changed the listing
///   this connection last got from `tools/list`, it gets `notifications/tools/list_changed`.
/// - A `tools/call` whose request carries `_meta.progressToken` gets
///   `notifications/progress` every [`DEFAULT_PROGRESS_INTERVAL`] while it runs (`progress`
///   counts up, `message` says for how long; the engine has no finer progress).
/// - A busy host is a JSON-RPC error `-32603` (HTTP: `503` + `Retry-After`).
/// - `notifications/cancelled` cancels the named request of this connection through the host's
///   runs registry ([`Host::cancel_request`], keyed by [`Connection::key`]), as over HTTP;
///   [`Connection::close`] (end of the stream) cancels every request still running.
///
/// The connection's jobs run for the client `mcp` with [`Connection::key`] as its key, on the
/// host it was built with, so a process can serve connections over different hosts.
pub struct Connection {
    mcp: Arc<Mcp>,
    stream: Arc<Stream>,
}

impl Connection {
    /// A new connection to `mcp` (its configuration, statistics and tool catalog), its session
    /// jobs run by `host`. It is registered with `mcp` for the server's pushes until dropped.
    pub fn new(mcp: Arc<Mcp>, host: Arc<dyn Host>) -> Self {
        let stream = Arc::new(Stream {
            host,
            key: format!("{:032x}", rand::random::<u128>()),
            state: Mutex::new(State::default()),
            outbound: Mutex::new(None),
            progress_micros: AtomicU64::new(DEFAULT_PROGRESS_INTERVAL.as_micros() as u64),
            in_flight: Mutex::new(Vec::new()),
        });
        mcp.add_stream(Arc::downgrade(&stream));
        Self { mcp, stream }
    }

    /// Sends the server's own messages (one JSON-RPC message per string, without a line end) to
    /// `outbound`: progress of running calls, a changed tool list. Without it they are dropped.
    /// [`crate::serve_lines`] sets its own while it serves.
    pub fn with_outbound(self, outbound: Sender<String>) -> Self {
        self.stream.set_outbound(Some(outbound));
        self
    }

    /// Sends progress of a running call every `interval` instead of
    /// [`DEFAULT_PROGRESS_INTERVAL`] (at least a millisecond).
    pub fn with_progress_interval(self, interval: Duration) -> Self {
        let micros = interval.as_micros().clamp(1000, u128::from(u64::MAX)) as u64;
        self.stream.progress_micros.store(micros, Ordering::Relaxed);
        self
    }

    /// Answers one JSON-RPC message (its text): the messages to send back, in order (none for a
    /// notification or a response; a JSON-RPC error for text that is no message). It blocks
    /// while the session jobs of the message run; call it from several threads to serve
    /// requests side by side (a `notifications/cancelled` must not wait behind the call it
    /// cancels: [`crate::serve_lines`] does so).
    pub fn handle(&self, message: &str) -> Vec<String> {
        match parse_message(message) {
            Ok(message) => self.handle_value(&message),
            Err(outcome) => self.outcome_lines(outcome),
        }
    }

    /// [`Connection::handle`] of a parsed message.
    pub(crate) fn handle_value(&self, message: &Value) -> Vec<String> {
        let request = message
            .get("method")
            .and(message.get("id"))
            .map(Value::to_string);
        if let Some(id) = &request
            && let Ok(mut running) = self.stream.in_flight.lock()
        {
            running.push(id.clone());
        }
        let peer = Peer {
            host: self.stream.host.as_ref(),
            key: Some(&self.stream.key),
            client_digest: self.stream.digest(),
            stream: Some(&self.stream),
        };
        let outcome = self.mcp.message(&peer, message);
        if let Some(id) = &request
            && let Ok(mut running) = self.stream.in_flight.lock()
            && let Some(at) = running.iter().position(|known| known == id)
        {
            running.swap_remove(at);
        }
        let mut lines = self.outcome_lines(outcome);
        if self.stream.take_list_changed() {
            lines.push(list_changed_notice().to_string());
        }
        lines
    }

    /// The endpoint's statistics.
    pub(crate) fn mcp_stats(&self) -> &crate::McpStats {
        self.mcp.stats()
    }

    /// The line of an outcome (an invalid message counts as a refusal).
    pub(crate) fn outcome_lines(&self, outcome: Outcome) -> Vec<String> {
        if matches!(outcome, Outcome::Invalid { .. }) {
            self.mcp.stats().record_refusal(Refusal::InvalidRequest);
        }
        outcome
            .into_message()
            .map(|message| vec![message.to_string()])
            .unwrap_or_default()
    }

    /// Ends the connection: cancels the runs of every request still running (the client is
    /// gone) and drops the outbound channel. Answers of those requests still come back from
    /// their [`Connection::handle`] calls. Also done when the connection is dropped.
    pub fn close(&self) {
        self.stream.set_outbound(None);
        let running = self
            .stream
            .in_flight
            .lock()
            .map(|running| running.clone())
            .unwrap_or_default();
        let client = ClientRef::new(MCP_CLIENT, None).with_key(&self.stream.key);
        for request in running {
            self.stream.host.cancel_request(&client, &request);
        }
    }

    /// The key of the connection's jobs ([`ClientRef::key`] of the client `mcp`): random, 128
    /// bits, what the host's runs registry and fairness tell this connection apart by.
    pub fn key(&self) -> &str {
        &self.stream.key
    }

    /// Whether the client sent `initialize`.
    pub fn is_initialized(&self) -> bool {
        self.stream.with_init(|_| ()).is_some()
    }

    /// The protocol revision `initialize` negotiated (one of [`crate::PROTOCOL_VERSIONS`]).
    pub fn protocol_version(&self) -> Option<&'static str> {
        self.stream.with_init(|init| init.version)
    }

    /// The client's `clientInfo` (`{name, version}`) of `initialize`.
    pub fn client_info(&self) -> Option<Value> {
        self.stream.with_init(|init| init.client.clone())
    }

    /// The client's `capabilities` of `initialize` (`{}` when it sent none).
    pub fn client_capabilities(&self) -> Option<Value> {
        self.stream.with_init(|init| init.capabilities.clone())
    }

    /// The outbound channel while [`crate::serve_lines`] serves the connection.
    pub(crate) fn set_outbound(&self, outbound: Option<Sender<String>>) {
        self.stream.set_outbound(outbound);
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.close();
    }
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection")
            .field("key", &self.stream.key)
            .field("protocol_version", &self.protocol_version())
            .finish_non_exhaustive()
    }
}

/// The shared part of a [`Connection`]: what the endpoint's pushes reach (it keeps a weak
/// reference of every open one).
pub(crate) struct Stream {
    host: Arc<dyn Host>,
    key: String,
    state: Mutex<State>,
    outbound: Mutex<Option<Sender<String>>>,
    progress_micros: AtomicU64,
    /// The JSON-RPC ids (JSON text) of the requests being answered.
    in_flight: Mutex<Vec<String>>,
}

#[derive(Default)]
struct State {
    init: Option<Init>,
    /// The fingerprint of the tool listing the client last got (`tools/list`, or the one the
    /// last `list_changed` announced); none before its first `tools/list`.
    listed: Option<u64>,
    /// A change of the listing found after this connection's own call: announced after its
    /// answer.
    list_changed: bool,
}

/// What `initialize` negotiated.
struct Init {
    version: &'static str,
    client: Value,
    capabilities: Value,
    digest: u64,
}

impl Stream {
    pub(crate) fn key(&self) -> &str {
        &self.key
    }

    pub(crate) fn host(&self) -> &Arc<dyn Host> {
        &self.host
    }

    fn with_init<T>(&self, read: impl FnOnce(&Init) -> T) -> Option<T> {
        self.state.lock().ok()?.init.as_ref().map(read)
    }

    fn digest(&self) -> Option<u64> {
        self.with_init(|init| init.digest)
    }

    fn set_outbound(&self, outbound: Option<Sender<String>>) {
        if let Ok(mut slot) = self.outbound.lock() {
            *slot = outbound;
        }
    }

    /// `initialize` negotiated `version` with `client` (`{name, version}`, its digest).
    pub(crate) fn initialized(
        &self,
        version: &'static str,
        client: Value,
        capabilities: Value,
        digest: u64,
    ) {
        if let Ok(mut state) = self.state.lock() {
            state.init = Some(Init {
                version,
                client,
                capabilities,
                digest,
            });
        }
    }

    /// The client got the tool listing of fingerprint `listed`.
    pub(crate) fn listed(&self, listed: u64) {
        if let Ok(mut state) = self.state.lock() {
            state.listed = Some(listed);
        }
    }

    /// Whether the client has listed the tools (only then is a change announced).
    pub(crate) fn has_listed(&self) -> bool {
        self.state.lock().is_ok_and(|state| state.listed.is_some())
    }

    /// The listing is now `now`: whether that differs from what the client saw (then `now` is
    /// what it is told about).
    pub(crate) fn list_changed(&self, now: u64) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        match state.listed {
            Some(seen) if seen != now => {
                state.listed = Some(now);
                true
            }
            _ => false,
        }
    }

    /// Announces the changed listing after the answer of this connection's own call.
    pub(crate) fn mark_list_changed(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.list_changed = true;
        }
    }

    fn take_list_changed(&self) -> bool {
        self.state
            .lock()
            .is_ok_and(|mut state| std::mem::take(&mut state.list_changed))
    }

    /// Sends a message of the server's own on the outbound channel (dropped without one).
    pub(crate) fn push(&self, message: Value) {
        if let Ok(slot) = self.outbound.lock()
            && let Some(outbound) = slot.as_ref()
        {
            let _ = outbound.send(message.to_string());
        }
    }

    /// Runs `work` (a tool call) and meanwhile sends `notifications/progress` with `token`
    /// every progress interval until it returns.
    pub(crate) fn with_progress<T>(&self, token: &Value, work: impl FnOnce() -> T) -> T {
        let interval = Duration::from_micros(self.progress_micros.load(Ordering::Relaxed));
        let (done, wait) = channel::<()>();
        std::thread::scope(|scope| {
            // Without a thread there is no progress; the call runs all the same.
            let _ticker = std::thread::Builder::new()
                .name("mcp-progress".into())
                .spawn_scoped(scope, move || {
                    let started = Instant::now();
                    let mut ticks = 0u64;
                    while let Err(RecvTimeoutError::Timeout) = wait.recv_timeout(interval) {
                        ticks += 1;
                        self.push(json!({
                            "jsonrpc": "2.0",
                            "method": "notifications/progress",
                            "params": {
                                "progressToken": token,
                                "progress": ticks,
                                "message": format!(
                                    "still running after {:.1} s",
                                    started.elapsed().as_secs_f64()
                                ),
                            },
                        }));
                    }
                });
            let result = work();
            drop(done);
            result
        })
    }
}
