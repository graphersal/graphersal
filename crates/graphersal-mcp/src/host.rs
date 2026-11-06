//! The host boundary: how the server that embeds the endpoint runs session jobs ([`Executor`])
//! and what else the tools read from it ([`Host`]).

use std::sync::mpsc::channel;

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
}

impl ClientRef {
    /// A client `name` with an optional `detail`.
    pub fn new(name: &str, detail: Option<&str>) -> Self {
        Self {
            name: name.to_string(),
            detail: detail.map(str::to_string),
        }
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

    /// A query without a caller's own cancellation token starts: it runs under the host's
    /// session-wide token (the host may report it as running, e.g. for its Stop button).
    fn query_started(&self) {}

    /// The query of the last [`Host::query_started`] ended.
    fn query_finished(&self) {}
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
}

impl McpError {
    /// The message.
    pub fn message(&self) -> &str {
        match self {
            Self::Unavailable(message) | Self::Failed(message) => message,
        }
    }
}

impl std::fmt::Display for McpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for McpError {}

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
            .map_err(|err| err.to_string())?;
        answer
            .recv()
            .map_err(|_| "the request failed inside the session".to_string())
    }
}
