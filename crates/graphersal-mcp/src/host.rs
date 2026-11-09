//! The host boundary: how the server that embeds the endpoint runs session jobs ([`Executor`])
//! and what else the tools read from it ([`Host`]).

use std::cell::RefCell;
use std::sync::mpsc::channel;

use graphersal::exec::CancelToken;
use graphersal_session::Session;
use serde_json::Value;

/// A job on a [`Session`]: one call of a session method (or a few), run by the host's
/// [`Executor`].
pub type Job = Box<dyn FnOnce(&mut Session) + Send + 'static>;

/// Who sends a session job: a client name (`mcp` for the MCP endpoint, `ai` for an AI Chat
/// turn, a browser tab's id) and an optional detail (the AI Chat's conversation id). The host
/// uses it for its change feed and its workload channel.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClientRef {
    name: String,
    detail: Option<String>,
    /// Which client of that name: the MCP session id of an endpoint call (none without one).
    /// A host that shares its workers fairly counts the running and queued jobs per key.
    key: Option<String>,
    /// The JSON-RPC id of the request the jobs answer (its JSON text), which the client's
    /// `notifications/cancelled` names ([`Host::cancel_request`]).
    request: Option<String>,
    /// The identity the host attached to the request or connection
    /// ([`crate::McpRequest::with_identity`], [`crate::Connection::with_identity`]).
    identity: Option<String>,
}

impl ClientRef {
    /// A client `name` with an optional `detail`.
    pub fn new(name: &str, detail: Option<&str>) -> Self {
        Self {
            name: name.to_string(),
            detail: detail.map(str::to_string),
            key: None,
            request: None,
            identity: None,
        }
    }

    /// The same client with the identity the host established for it (a user name, a token's
    /// owner).
    pub fn with_identity(mut self, identity: &str) -> Self {
        self.identity = Some(identity.to_string());
        self
    }

    /// The identity of [`ClientRef::with_identity`], if any.
    pub fn identity(&self) -> Option<&str> {
        self.identity.as_deref()
    }

    /// The same client answering the JSON-RPC request `id` (its JSON text: `7`, `"a"`).
    pub fn with_request(mut self, id: &str) -> Self {
        self.request = Some(id.to_string());
        self
    }

    /// The JSON-RPC id (its JSON text) of the request the jobs answer, if any.
    pub fn request(&self) -> Option<&str> {
        self.request.as_deref()
    }

    /// The same client, told apart from others of its name by `key` (the endpoint sets the MCP
    /// session id of the request).
    pub fn with_key(mut self, key: &str) -> Self {
        self.key = Some(key.to_string());
        self
    }

    /// The key that tells this client apart from others of its name (the MCP session id), if
    /// any.
    pub fn key(&self) -> Option<&str> {
        self.key.as_deref()
    }

    /// The client's name (`mcp`, `ai`, a browser tab's id).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The detail (the AI Chat's conversation id), if any.
    pub fn detail(&self) -> Option<&str> {
        self.detail.as_deref()
    }
}

/// Runs session jobs for the endpoint: the one extension point between the MCP code and how a
/// host schedules work on its [`Session`] (the dev server: its single session thread).
pub trait Executor: Send + Sync {
    /// Runs `job` on a [`Session`] over the served graph for `client` and returns once it has
    /// run (or was dropped without running). `Err` when the host cannot run it; its message is
    /// shown to the agent as the failure.
    fn execute(&self, client: &ClientRef, job: Job) -> Result<(), McpError>;
}

/// What the tools need from the host besides running session jobs. Every method describes the
/// host's own state; none of them runs a query.
pub trait Host: Executor {
    /// The read-only view of a past state the host serves instead of the live graph (an object
    /// with `commitSeq` and `target`), or `Value::Null` on the live graph. While a view is open,
    /// tool errors and `graph_info` carry a note saying so.
    fn view(&self) -> Value;

    /// `statistics` (the session's `Session::statistics`) with the host's own part of the
    /// process added (the `statistics` tool and the `graphersal://statistics` resource).
    fn add_process_statistics(&self, statistics: Value) -> Value;

    /// Adds the host's fields to the `graph_info` answer (the session's `Session::info`), e.g.
    /// the source file, the store and damage found on disk. `Err` makes `graph_info` a tool
    /// error with its message.
    fn add_graph_info(&self, info: &mut Value) -> Result<(), McpError>;

    /// The playground's `js/examples.json` (the resource `graphersal://examples`), or `None`
    /// when the host has none.
    fn examples_json(&self) -> Option<Vec<u8>>;

    /// A query of `client` is about to be queued: it runs under `cancel`, a token of this run
    /// alone (the caller's own, an AI Chat turn's, or a new one). The host keeps it until
    /// [`Host::run_ended`] with the returned id, so it can cancel the run, also while the run
    /// still waits for a session: by the client's request id ([`Host::cancel_request`]), by
    /// the client, or all at a graceful stop.
    fn run_started(&self, client: &ClientRef, cancel: &CancelToken) -> u64 {
        let _ = (client, cancel);
        0
    }

    /// [`Host::run_started`] with the query `text` of the run (the script it evaluates), for a
    /// host that shows its runs (graphersal-serve's run observer). Default: `run_started`,
    /// the text unused. The endpoint calls this one.
    fn run_started_with_text(&self, client: &ClientRef, cancel: &CancelToken, text: &str) -> u64 {
        let _ = text;
        self.run_started(client, cancel)
    }

    /// The run of [`Host::run_started`] that returned `run` ended.
    fn run_ended(&self, run: u64) {
        let _ = run;
    }

    /// The client's `notifications/cancelled` for its request `request` (the JSON text of the
    /// id): cancels the runs of that request of this client (`client` carries the MCP session
    /// id); whether one was running. Never another client's run.
    fn cancel_request(&self, client: &ClientRef, request: &str) -> bool {
        let _ = (client, request);
        false
    }
}

/// Ends a run of [`Host::run_started`] when dropped (also when the job panics or the host
/// refuses it).
pub(crate) struct RunGuard<'a> {
    pub(crate) host: &'a dyn Host,
    pub(crate) run: u64,
}

impl Drop for RunGuard<'_> {
    fn drop(&mut self) {
        self.host.run_ended(self.run);
    }
}

/// A failure of the host or of an operation of the endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum McpError {
    /// The host could not run a session job (its session is not running, the job failed
    /// inside it, ...).
    Unavailable(String),
    /// An operation failed; the message says why.
    Failed(String),
    /// The host is too busy to run the job now (its queue is full, the job waited too long,
    /// the client has too many jobs running, the host is stopping): the endpoint answers the
    /// request with HTTP `503` and `Retry-After`, so the client retries it later.
    Busy(String),
    /// Reading or writing the stream of a connection failed ([`crate::serve_lines`]).
    #[non_exhaustive]
    Io {
        /// The kind of the I/O error.
        kind: std::io::ErrorKind,
        /// The I/O error's text.
        message: String,
    },
}

impl McpError {
    /// The message.
    pub fn message(&self) -> &str {
        match self {
            Self::Unavailable(message)
            | Self::Failed(message)
            | Self::Busy(message)
            | Self::Io { message, .. } => message,
        }
    }

    /// An [`McpError::Io`] of `err`.
    pub fn io(err: &std::io::Error) -> Self {
        Self::Io {
            kind: err.kind(),
            message: err.to_string(),
        }
    }
}

impl From<std::io::Error> for McpError {
    fn from(err: std::io::Error) -> Self {
        Self::io(&err)
    }
}

impl std::fmt::Display for McpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for McpError {}

thread_local! {
    /// The [`McpError::Busy`] refusal of the last job this thread tried to run for the request
    /// it answers ([`Ctx::call`]): the endpoint answers such a request `503` instead of a tool
    /// error ([`take_busy`]).
    static BUSY: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Forgets a busy refusal of this thread (a request starts).
pub(crate) fn clear_busy() {
    let _ = BUSY.try_with(|busy| busy.borrow_mut().take());
}

/// The busy refusal of a job this thread ran since [`clear_busy`], if any.
pub(crate) fn take_busy() -> Option<String> {
    BUSY.try_with(|busy| busy.borrow_mut().take())
        .ok()
        .flatten()
}

/// A host and the client its jobs run for: what every tool handler gets.
#[derive(Clone, Copy)]
pub(crate) struct Ctx<'a> {
    pub(crate) host: &'a dyn Host,
    pub(crate) client: &'a ClientRef,
}

impl Ctx<'_> {
    /// Runs `job` through the host's [`Executor`] and returns its answer; `Err` is the message
    /// of the failure.
    pub(crate) fn call<T: Send + 'static>(
        &self,
        job: impl FnOnce(&mut Session) -> T + Send + 'static,
    ) -> Result<T, String> {
        let (reply, answer) = channel();
        self.host
            .execute(
                self.client,
                Box::new(move |session| {
                    let _ = reply.send(job(session));
                }),
            )
            .map_err(|err| {
                if let McpError::Busy(message) = &err {
                    let _ = BUSY.try_with(|busy| *busy.borrow_mut() = Some(message.clone()));
                }
                err.to_string()
            })?;
        answer
            .recv()
            .map_err(|_| "the request failed inside the session".to_string())
    }
}
