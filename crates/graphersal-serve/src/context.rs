//! The hosted graph runtime: the [`Context`] every request handler shares, the session thread
//! and its job queue ([`spawn_session`], [`Runner`]), the change feed, the client of each
//! request, and the scheduled-jobs ticker ([`start_job_ticker`]).
//!
//! Threads: one session thread (64 MiB stack, [`SESSION_STACK_BYTES`]) owns the [`Session`] and
//! runs the jobs one after the other; every HTTP request gets a thread of its own (the host's)
//! that sends a job and waits for its answer ([`Context::call`]). A host may queue the jobs
//! elsewhere instead ([`Runner::Queue`], the CLI's reader pool of the read-only serving mode).
//!
//! Extension points of this module (each with the core default a host gets without it):
//! - **executor policy** ([`Runner`], [`JobQueue`]; default: one session thread, unbounded
//!   queue): how the jobs reach a session. The CLI's reader pool of `--server --read-only` is a
//!   [`JobQueue`] (bounded queue, per-client fairness, 503 when busy, its first worker takes
//!   over the session thread: [`spawn_session`]'s `takeover`);
//! - **request filter** ([`RequestFilter`]; default: none beyond the context's [`HostCheck`],
//!   which always runs first): sees each request the checks accepted before routing, refuses it
//!   or attaches an identity;
//! - **extra routes** ([`Hooks::route`]; default: `404 unknown endpoint`): the `/api/`
//!   endpoints the core does not know (the CLI's AI Chat), plus the host's fields of
//!   `GET /api/info` and the statistics ([`Hooks`]);
//! - **run observer + cancel handle**: [`crate::RunObserver`] on the run registry
//!   ([`crate::Runs::with_observer`], [`ContextBuilder::runs`]).
//!
//! The graph resolver and the lifecycle (probes, loading, stopping, activity) are one level up,
//! in [`crate::Server`].
//!
//! Change feed: every job that changes the commit position of the served graph (or swaps the
//! graph: a view, a rollback) is recorded with the client that sent it (`X-Graphersal-Client`
//! of a browser tab, `mcp` for the MCP endpoint, `ai` for the AI Chat); `GET /api/changes`
//! answers it without the session thread, so a tab polls it cheaply and reloads when somebody
//! else changed the graph.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::fmt;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use graphersal::exec::CancelToken;
use graphersal::persist::Store;
use graphersal_session::{Session, SessionPresets};
use serde_json::{Value, json};

use crate::host_check::HostCheck;
use crate::http::{HttpError, Request, Response};
use crate::runs::{self, Runs};
use crate::ui::Ui;
use crate::{job_workers, mcp};

/// A job for the session: runs on the session thread (or a worker of a [`JobQueue`]) with the
/// session it owns.
pub type Job = Box<dyn FnOnce(&mut Session) + Send>;

/// Stack of the session thread: the scripting engine recurses on deeply nested values.
pub const SESSION_STACK_BYTES: usize = 64 * 1024 * 1024;

/// A failure of the runtime: a thread that cannot start, a graph that cannot be loaded.
///
/// `#[non_exhaustive]`: further failures may be added.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ServeError {
    /// The session could not be set up (the host's diagnostic, shown as it is).
    Setup(String),
    /// A thread could not be started or ended early.
    Thread(String),
    /// The UI directory is not usable (the diagnostic, shown as it is).
    Ui(String),
    /// A lifecycle step out of order ([`crate::Server::ready`] twice).
    Lifecycle(String),
}

impl fmt::Display for ServeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Setup(message)
            | Self::Thread(message)
            | Self::Ui(message)
            | Self::Lifecycle(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for ServeError {}

/// Why a [`JobQueue`] did not take a job. Every one is a `503` with `Retry-After: 1`.
///
/// `#[non_exhaustive]`: a queue may refuse for further reasons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum BusyReason {
    /// The queue is full.
    QueueFull,
    /// The job waited longer than the queue timeout.
    QueueTimeout,
    /// The client had too many jobs running or waiting.
    ClientLimit,
    /// The server is stopping.
    Stopping,
}

/// A job a [`JobQueue`] refused: the message of the `503` answer and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Busy {
    message: String,
    reason: BusyReason,
}

impl Busy {
    /// A refusal for `reason` answered with `message`.
    pub fn new(reason: BusyReason, message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            reason,
        }
    }

    /// The message of the `503` answer.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Why the job was refused.
    pub fn reason(&self) -> BusyReason {
        self.reason
    }
}

/// **Extension point "executor policy"**: a queue of the host that runs the session jobs
/// instead of the one session thread (default: [`Runner::Session`], one session thread with an
/// unbounded queue). The CLI's reader pool of the read-only serving mode (`--server
/// --read-only`: N workers with a session each over the same graph, a bounded queue, a limit
/// per client) is one.
///
/// A context on a host queue offers no scheduled jobs (they need the one session that owns the
/// Store: `POST /api/jobs/*` answers `409` [`JobQueue::scheduled_jobs_refusal`]) and adds
/// `serving` ([`JobQueue::status`]) to `GET /api/info`. A refusal ([`Busy`]) is a `503` with
/// `Retry-After: 1`. To let the queue's first worker use the session thread the graph was
/// loaded on, pass its loop as [`spawn_session`]'s `takeover`.
pub trait JobQueue: Send + Sync {
    /// Queues `job` for the client `key` (`page:<tab>`, `mcp:<session>`, `ai:<conversation>`);
    /// `Err` when refused. The job's answer comes through the job itself.
    fn submit(&self, key: &str, job: Job) -> Result<(), Busy>;

    /// The `serving` value of `GET /api/info`: the queue's settings and load.
    fn status(&self) -> Value;

    /// The message of the `409` to `POST /api/jobs/define|drop|enable|schedule|run` on this
    /// queue. Default: [`SCHEDULED_JOBS_REFUSAL`].
    fn scheduled_jobs_refusal(&self) -> String {
        SCHEDULED_JOBS_REFUSAL.to_string()
    }
}

/// The default answer of a host queue to a scheduled-jobs write
/// ([`JobQueue::scheduled_jobs_refusal`]).
pub const SCHEDULED_JOBS_REFUSAL: &str = "not available on this server: scheduled jobs need the \
    one session thread that owns the Store, and this server runs its jobs on a queue of its host";

/// Where the session jobs run: the **executor policy** of a [`Context`] (default:
/// [`Runner::Session`]).
///
/// `#[non_exhaustive]`: further ways of running jobs may be added.
#[non_exhaustive]
pub enum Runner {
    /// The read-write server: one session thread runs them one after the other (the job queue
    /// of [`spawn_session`]).
    Session(Sender<Job>),
    /// The read-only serving mode: a queue of the host ([`JobQueue`]).
    Queue(Arc<dyn JobQueue>),
}

/// **Extension point "extra routes"** and the host's additions to the core's answers (the CLI's
/// AI Chat and the embedding models it lists). Default: nothing added, an unknown `/api/`
/// endpoint answers `404 unknown endpoint /api/<endpoint>`. Every method has a default that
/// adds nothing.
///
/// Only `/api/` endpoints the core does not answer reach [`Hooks::route`] (after the request
/// filter, with the request's client set); a path outside `/api/` is a static file of the UI.
pub trait Hooks: Send + Sync {
    /// Adds the host's fields to `GET /api/info` (the core's fields are set already).
    fn add_info(&self, context: &Context, info: &mut Value) {
        let _ = (context, info);
    }

    /// Adds the host's part of the process to the session's statistics (`GET
    /// /api/statistics`, the MCP `statistics` tool and resource).
    fn add_process_statistics(&self, context: &Context, statistics: &mut Value) {
        let _ = (context, statistics);
    }

    /// Answers an `/api/` endpoint the core does not know (`endpoint` without the `/api/`
    /// prefix, `client` the request's client name), or `None`: `404 unknown endpoint`.
    fn route(
        &self,
        context: &Arc<Context>,
        request: &mut Request<'_>,
        endpoint: &str,
        query: &str,
        client: &str,
    ) -> Option<Result<Response, HttpError>> {
        let _ = (context, request, endpoint, query, client);
        None
    }
}

/// The hooks of a host that adds nothing.
struct NoHooks;

impl Hooks for NoHooks {}

/// **Extension point "request filter"**: sees each request of the API before routing (after
/// the lifecycle answers of [`crate::Server`]: probes, loading, stopping), refuses it or
/// attaches an identity ([`Request::set_identity`]; the extra routes read it from the request,
/// a [`crate::RunObserver`] from [`crate::RunInfo::identity`]). Default: none, only the
/// context's [`HostCheck`] (the loopback/`Host`/`Origin` checks).
///
/// The checks ALWAYS run first and a filter cannot replace or skip them (user decision
/// 2026-11-09): it only sees the requests they accepted. A host that relays requests from
/// another process forwards the original `Host` (and `Origin`) header, or builds the context's
/// [`HostCheck`] with the rules of the address the browser uses.
pub trait RequestFilter: Send + Sync {
    /// `Ok` to serve `request`, or the answer that refuses it (`{"error": message}` with the
    /// error's status).
    fn filter(&self, request: &mut Request<'_>) -> Result<(), HttpError>;
}

/// How many recent changes the feed keeps (a poller that missed more reloads anyway).
const FEED_RECENT: usize = 32;

/// The change feed: a version that grows with every change of the served graph, the commit
/// position after it, and who made the recent ones.
#[derive(Default)]
pub(crate) struct Feed {
    version: u64,
    pub(crate) commit_seq: u64,
    /// The last [`FEED_RECENT`] changes, oldest first.
    recent: VecDeque<FeedEntry>,
}

/// One change of the feed.
struct FeedEntry {
    version: u64,
    client: Client,
    commit_seq: u64,
}

/// Who sent a session job, for the change feed: the client (`X-Graphersal-Client` of a browser
/// tab, `mcp`, `ai`) and an optional detail (the AI Chat's conversation id).
#[derive(Clone, Default)]
pub(crate) struct Client {
    pub(crate) name: String,
    pub(crate) detail: Option<String>,
    /// Which client of the name, for the pool's fairness and cancellation: an MCP session id.
    key: Option<String>,
    /// The identity the request filter attached to the request ([`Request::identity`]).
    pub(crate) identity: Option<String>,
}

impl Client {
    /// A client with a detail.
    pub(crate) fn with_detail(name: &str, detail: Option<&str>) -> Self {
        Self {
            name: name.to_string(),
            detail: detail.map(str::to_string),
            key: None,
            identity: None,
        }
    }

    /// The same client told apart from others of its name by `key` (an MCP session id).
    #[cfg_attr(not(feature = "mcp"), allow(dead_code))]
    pub(crate) fn with_key(mut self, key: Option<&str>) -> Self {
        self.key = key.map(str::to_string);
        self
    }

    /// The key the pool counts and the run registry cancels this client's jobs by: `mcp` (+ its MCP session
    /// id), `ai` + the conversation id, or `page` + a browser page's `X-Graphersal-Client`.
    pub(crate) fn pool_key(&self) -> String {
        match (self.name.as_str(), &self.key, &self.detail) {
            ("mcp", Some(key), _) => format!("mcp:{key}"),
            ("mcp", None, _) => "mcp".to_string(),
            ("ai", _, detail) => format!("ai:{}", detail.as_deref().unwrap_or_default()),
            (name, _, _) => format!("page:{name}"),
        }
    }
}

impl Feed {
    fn record(&mut self, client: Client, commit_seq: u64) {
        self.version += 1;
        self.commit_seq = commit_seq;
        if self.recent.len() == FEED_RECENT {
            self.recent.pop_front();
        }
        self.recent.push_back(FeedEntry {
            version: self.version,
            client,
            commit_seq,
        });
    }

    pub(crate) fn to_value(&self) -> Value {
        json!({
            "version": self.version,
            "commitSeq": self.commit_seq,
            "recent": self.recent.iter().map(|entry| {
                let mut change = json!({
                    "version": entry.version, "by": entry.client.name,
                    "commitSeq": entry.commit_seq,
                });
                if let Some(detail) = &entry.client.detail {
                    change["detail"] = json!(detail);
                }
                change
            }).collect::<Vec<_>>(),
        })
    }
}

thread_local! {
    /// The client of the request this thread answers (every request has a thread of its own):
    /// the `X-Graphersal-Client` id of a browser tab, `mcp`, or empty; `ai` (with the
    /// conversation id as detail) on an AI Chat turn's thread.
    pub(crate) static CLIENT: RefCell<Client> = RefCell::new(Client::default());
}

/// Runs `run` with `client` as this thread's client (restored afterwards).
pub(crate) fn as_client<T>(client: Client, run: impl FnOnce() -> T) -> T {
    let previous = CLIENT.with(|c| c.replace(client));
    struct Restore(Option<Client>);
    impl Drop for Restore {
        fn drop(&mut self) {
            if let Some(previous) = self.0.take() {
                let _ = CLIENT.try_with(|c| *c.borrow_mut() = previous);
            }
        }
    }
    let _restore = Restore(Some(previous));
    run()
}

/// The commit position of the graph the session serves, and which graph it is (a view or a
/// rollback swaps it). Fails when the graph cannot be read.
pub(crate) fn position(session: &Session) -> Result<(usize, u64), graphersal::error::GraphError> {
    let graph = session.graph();
    Ok((
        Arc::as_ptr(graph) as *const () as usize,
        graph.last_commit_seq()?,
    ))
}

/// Starts the session thread: a [`Session`] with `presets` on a thread of
/// [`SESSION_STACK_BYTES`], set up by `setup` (load the graph; its answer is returned here once
/// it is done). Then the thread runs the jobs of the returned queue one after the other, or,
/// with `takeover` (the first worker of the host's [`JobQueue`]), hands the session to it.
///
/// Work outside a request (loading at start) is technical in the workload statistics;
/// [`Context::call`] sets each request's channel. A job that panics is a bug: the session (its
/// graph rolled back by the unit) serves on.
pub fn spawn_session<T: Send + 'static>(
    presets: SessionPresets,
    setup: impl FnOnce(&mut Session) -> Result<T, String> + Send + 'static,
    takeover: Option<Job>,
) -> Result<(Sender<Job>, T), ServeError> {
    let (jobs, queue) = channel::<Job>();
    let (ready, started) = channel::<Result<T, String>>();
    std::thread::Builder::new()
        .name("graphersal-session".to_string())
        .stack_size(SESSION_STACK_BYTES)
        .spawn(move || {
            graphersal::workload::set_thread_default(
                graphersal::workload::Channel::Playground,
                true,
            );
            let mut session = Session::with_presets(presets);
            let started = setup(&mut session);
            let failed = started.is_err();
            let _ = ready.send(started);
            if failed {
                return;
            }
            match takeover {
                Some(takeover) => takeover(&mut session),
                None => serve_jobs(&mut session, queue),
            }
        })
        .map_err(|err| {
            ServeError::Thread(format!("Error: cannot start the session thread: {err}"))
        })?;
    let started = started.recv().map_err(|_| {
        ServeError::Thread("Error: the session thread stopped while loading the graph".to_string())
    })?;
    started.map(|t| (jobs, t)).map_err(ServeError::Setup)
}

/// Runs the jobs of `queue` on `session` until every sender is gone.
fn serve_jobs(session: &mut Session, queue: Receiver<Job>) {
    for job in queue {
        // A panic is a bug; the session (its graph rolled back by the unit) serves on.
        if std::panic::catch_unwind(AssertUnwindSafe(|| job(session))).is_err() {
            tracing::error!(
                target: "graphersal::server",
                "a request panicked in the session (a bug, please report)"
            );
        }
    }
}

/// Where a [`Context`]'s jobs go (the [`Runner`], the session sender behind a lock).
enum Jobs {
    Session(Mutex<Sender<Job>>),
    Queue(Arc<dyn JobQueue>),
}

/// What every request handler shares: the job queue, the run registry, the served graph's
/// facts (its file, its Store, read-only), the UI, the change feed, the header checks and the
/// MCP endpoint. Built with [`Context::builder`].
pub struct Context {
    jobs: Jobs,
    /// The runs a client may cancel, each with a token of its own ([`runs`]).
    pub(crate) runs: Arc<Runs>,
    /// The playground UI: a directory over the embedded copy.
    pub(crate) ui: Ui,
    /// The `--graph` file, when the graph came from one ("Save to file" writes it back).
    pub(crate) source: Option<PathBuf>,
    /// The `--schema` file the graph was loaded with, when one was given.
    pub(crate) schema_source: Option<PathBuf>,
    /// The `Host`/`Origin` checks every request passes first.
    pub(crate) check: HostCheck,
    /// The host's request filter, run after the checks; `None`: none.
    pub(crate) filter: Option<Arc<dyn RequestFilter>>,
    /// Whether the host process counts its allocations (`GET /api/info` `memoryTracked`).
    pub(crate) memory_tracked: bool,
    /// The session presets, as `GET /api/info` reports them.
    pub(crate) presets: Value,
    /// `--read-only` (or a Store opened read-only): the graph refuses every write and
    /// "Save to file" is refused.
    pub(crate) read_only: bool,
    /// The Store of a `--graph <dir>`: checkpoints, marks, snapshot downloads, views, forks,
    /// rollbacks, the attic.
    pub(crate) store: Option<Arc<Store>>,
    /// The read-only view of a past state the session serves instead of the store's live
    /// graph (`POST /api/store/view`), as `GET /api/info` reports it; `None`: the live graph.
    pub(crate) view: Mutex<Option<Value>>,
    /// The change feed (`GET /api/changes`), updated by the session jobs.
    pub(crate) feed: Arc<Mutex<Feed>>,
    /// The threads that do the work of the scheduled jobs ([`job_workers`]).
    pub(crate) job_workers: job_workers::JobWorkers,
    /// The MCP endpoint (`--mcp`), or `None`; possibly shared with the contexts of other
    /// graphs of the process (one endpoint: its statistics, sessions and token).
    pub(crate) mcp: Option<Arc<mcp::Mcp>>,
    /// The MCP profile of this context's requests and connections; `None`: the endpoint's
    /// default.
    #[cfg(feature = "mcp")]
    pub(crate) mcp_profile: Option<graphersal_mcp::Profile>,
    /// What the host adds.
    pub(crate) hooks: Arc<dyn Hooks>,
}

/// Builds a [`Context`].
pub struct ContextBuilder {
    runner: Runner,
    check: HostCheck,
    runs: Option<Arc<Runs>>,
    ui: Ui,
    source: Option<PathBuf>,
    schema_source: Option<PathBuf>,
    presets: Value,
    read_only: bool,
    store: Option<Arc<Store>>,
    commit_seq: u64,
    mcp: Option<Arc<mcp::Mcp>>,
    #[cfg(feature = "mcp")]
    mcp_profile: Option<graphersal_mcp::Profile>,
    hooks: Option<Arc<dyn Hooks>>,
    filter: Option<Arc<dyn RequestFilter>>,
    memory_tracked: bool,
}

impl ContextBuilder {
    /// The run registry (shared with the host's graceful stop); default: a new one without an
    /// observer. A host that observes the runs passes [`Runs::with_observer`].
    pub fn runs(mut self, runs: Arc<Runs>) -> Self {
        self.runs = Some(runs);
        self
    }

    /// The UI ([`Ui::resolve`]); default: the embedded copy alone.
    pub fn ui(mut self, ui: Ui) -> Self {
        self.ui = ui;
        self
    }

    /// The file the graph came from ("Save to file" writes it back).
    pub fn source(mut self, source: Option<PathBuf>) -> Self {
        self.source = source;
        self
    }

    /// The schema file the graph was loaded with ("Save to file" writes the schema to it).
    pub fn schema_source(mut self, schema_source: Option<PathBuf>) -> Self {
        self.schema_source = schema_source;
        self
    }

    /// The session presets as `GET /api/info` reports them (`SessionPresets::to_value`).
    pub fn presets(mut self, presets: Value) -> Self {
        self.presets = presets;
        self
    }

    /// The graph refuses every write; "Save to file" is refused.
    pub fn read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    /// The Store the session serves (`/api/store*`).
    pub fn store(mut self, store: Option<Arc<Store>>) -> Self {
        self.store = store;
        self
    }

    /// The commit position of the loaded graph (the change feed starts there).
    pub fn commit_seq(mut self, commit_seq: u64) -> Self {
        self.commit_seq = commit_seq;
        self
    }

    /// The MCP endpoint (`POST /mcp`).
    #[cfg(feature = "mcp")]
    pub fn mcp(mut self, mcp: Option<graphersal_mcp::Mcp>) -> Self {
        self.mcp = mcp.map(Arc::new);
        self
    }

    /// The MCP endpoint (`POST /mcp`), shared with the contexts of the host's other graphs: one
    /// endpoint for the process (its statistics, MCP sessions, token and default profile),
    /// whose requests the graph resolver sends to this context, so this context's session runs
    /// their jobs ([`Context`] is the endpoint's `Host`).
    #[cfg(feature = "mcp")]
    pub fn shared_mcp(mut self, mcp: Arc<graphersal_mcp::Mcp>) -> Self {
        self.mcp = Some(mcp);
        self
    }

    /// The MCP profile of this context's requests and connections (what an agent may do here:
    /// configuration, read-only flag, answer limit, access policy), chosen by the host;
    /// default: the endpoint's own ([`graphersal_mcp::Mcp::profile`]). Two contexts over the
    /// same session ([`Runner`]) with different profiles serve one graph to two kinds of
    /// clients, the resolver choosing which a request gets.
    #[cfg(feature = "mcp")]
    pub fn mcp_profile(mut self, profile: graphersal_mcp::Profile) -> Self {
        self.mcp_profile = Some(profile);
        self
    }

    /// What the host adds ([`Hooks`]: extra routes, `/api/info` and statistics fields);
    /// default: nothing.
    pub fn hooks(mut self, hooks: Arc<dyn Hooks>) -> Self {
        self.hooks = Some(hooks);
        self
    }

    /// The host's request filter ([`RequestFilter`]), run after the [`HostCheck`] of
    /// [`Context::builder`] (which always runs); default: none.
    pub fn filter(mut self, filter: Arc<dyn RequestFilter>) -> Self {
        self.filter = Some(filter);
        self
    }

    /// Whether the host process counts its allocations (it installs
    /// `graphersal::alloc_tracking`'s allocator), as `GET /api/info` `memoryTracked` tells the
    /// UI; default: `false`.
    pub fn memory_tracked(mut self, tracked: bool) -> Self {
        self.memory_tracked = tracked;
        self
    }

    /// The context.
    pub fn build(self) -> Arc<Context> {
        Arc::new(Context {
            jobs: match self.runner {
                Runner::Session(jobs) => Jobs::Session(Mutex::new(jobs)),
                Runner::Queue(queue) => Jobs::Queue(queue),
            },
            runs: self.runs.unwrap_or_default(),
            ui: self.ui,
            source: self.source,
            schema_source: self.schema_source,
            check: self.check,
            filter: self.filter,
            memory_tracked: self.memory_tracked,
            presets: self.presets,
            read_only: self.read_only,
            store: self.store,
            view: Mutex::new(None),
            feed: Arc::new(Mutex::new(Feed {
                commit_seq: self.commit_seq,
                ..Feed::default()
            })),
            job_workers: job_workers::JobWorkers::default(),
            mcp: self.mcp,
            #[cfg(feature = "mcp")]
            mcp_profile: self.mcp_profile,
            hooks: self.hooks.unwrap_or_else(|| Arc::new(NoHooks)),
        })
    }
}

impl Context {
    /// A builder of the context whose jobs go to `runner` (the executor policy) and whose
    /// requests pass `check` (always, before any request filter).
    pub fn builder(runner: Runner, check: HostCheck) -> ContextBuilder {
        ContextBuilder {
            runner,
            check,
            runs: None,
            ui: Ui::embedded(),
            source: None,
            schema_source: None,
            presets: Value::Null,
            read_only: false,
            store: None,
            commit_seq: 0,
            mcp: None,
            #[cfg(feature = "mcp")]
            mcp_profile: None,
            hooks: None,
            filter: None,
            memory_tracked: false,
        }
    }

    /// The MCP endpoint, if the server has one.
    #[cfg(feature = "mcp")]
    pub fn mcp(&self) -> Option<&graphersal_mcp::Mcp> {
        self.mcp.as_deref()
    }

    /// The MCP profile of this context's requests and connections: its own
    /// ([`ContextBuilder::mcp_profile`]), else the endpoint's default; `None` without an
    /// endpoint.
    #[cfg(feature = "mcp")]
    pub fn mcp_profile(&self) -> Option<&graphersal_mcp::Profile> {
        let mcp = self.mcp.as_deref()?;
        Some(self.mcp_profile.as_ref().unwrap_or_else(|| mcp.profile()))
    }

    /// A new MCP connection (one MCP session over a stream: `graphersal_mcp::serve_lines`)
    /// whose jobs run on this context's session, under this context's profile; `None`
    /// without an endpoint. The host picks the context (its graph and profile) from its own
    /// handshake, as the graph resolver does for a request.
    #[cfg(feature = "mcp")]
    pub fn mcp_connection(self: &Arc<Self>) -> Option<graphersal_mcp::Connection> {
        let mcp = self.mcp.clone()?;
        let profile = self.mcp_profile.clone();
        let host: Arc<dyn graphersal_mcp::Host> = self.clone();
        let connection = graphersal_mcp::Connection::new(mcp, host);
        Some(match profile {
            Some(profile) => connection.with_profile(profile),
            None => connection,
        })
    }

    /// The Store the session serves, if any.
    pub fn store(&self) -> Option<&Arc<Store>> {
        self.store.as_ref()
    }

    /// The run registry.
    pub fn runs(&self) -> &Arc<Runs> {
        &self.runs
    }

    /// Gives a job's finished run back to the session. A commit made on the job's thread is in
    /// no session job's before and after, so it is recorded in the change feed here, when the
    /// position moved since `before` and the feed does not know the new one yet.
    pub(crate) fn finish_job_run(
        &self,
        before: Option<(usize, u64)>,
        finished: graphersal::jobs::JobFinished,
    ) {
        let feed = self.feed.clone();
        let client = CLIENT.with(|c| c.borrow().clone());
        let _ = self.call(move |session| {
            session.finish_job_run(finished);
            if let (Some(before), Ok(after)) = (before, position(session))
                && before.0 == after.0
                && before.1 != after.1
                && let Ok(mut feed) = feed.lock()
                && feed.commit_seq != after.1
            {
                feed.record(client, after.1);
            }
        });
    }

    /// Registers a run of this request's client stopped by `cancel` (the client's own id of
    /// it: `alias`; what it does: `kind` and its query `text`, for the run observer) until the
    /// answer is dropped; register it BEFORE its job is queued.
    pub(crate) fn register_run(
        &self,
        alias: Option<&str>,
        cancel: CancelToken,
        kind: runs::RunKind,
        text: Option<&str>,
    ) -> runs::Registered<'_> {
        let (key, identity) = CLIENT.with(|c| {
            let client = c.borrow();
            (client.pool_key(), client.identity.clone())
        });
        self.runs.register_with(
            &key,
            alias,
            cancel,
            runs::RunDetails {
                kind: Some(kind),
                text: text.map(str::to_string),
                identity,
            },
        )
    }

    /// Runs `job` on the session thread and waits for its answer. A job that changes the
    /// served graph's commit position (or swaps the graph) is recorded in the change feed with
    /// the client of this request.
    pub fn call<T: Send + 'static>(
        &self,
        job: impl FnOnce(&mut Session) -> T + Send + 'static,
    ) -> Result<T, HttpError> {
        let (reply, answer) = channel();
        let feed = self.feed.clone();
        let client = CLIENT.with(|c| c.borrow().clone());
        // The workload statistics count only the queries a client runs (`Session::execute*`)
        // under its channel; the page's drawing and panels are technical.
        let channel = match client.name.as_str() {
            "mcp" | "ai" => graphersal::workload::Channel::Mcp,
            _ => graphersal::workload::Channel::Playground,
        };
        let key = client.pool_key();
        let job: Job = Box::new(move |session| {
            let before = position(session);
            let out = graphersal::workload::scope(channel, true, || job(session));
            let after = position(session);
            // The change feed is a notification for the other clients: a position that cannot
            // be read before or after counts as a change (they reload and see for themselves).
            let changed = match (&before, &after) {
                (Ok(before), Ok(after)) => before != after,
                _ => true,
            };
            if changed && let Ok(mut feed) = feed.lock() {
                let seq = match (&after, &before) {
                    (Ok(after), _) | (Err(_), Ok(after)) => after.1,
                    (Err(_), Err(_)) => 0,
                };
                feed.record(client, seq);
            }
            let _ = reply.send(out);
        });
        match &self.jobs {
            Jobs::Session(jobs) => {
                let sent = match jobs.lock() {
                    Ok(jobs) => jobs.send(job).is_ok(),
                    Err(_) => false,
                };
                if !sent {
                    return Err(HttpError(500, "the session is not running".to_string()));
                }
            }
            Jobs::Queue(queue) => {
                queue.submit(&key, job).map_err(|refused| {
                    // The MCP endpoint's usage statistics count its refusals by kind.
                    if key.starts_with("mcp")
                        && let Some(mcp) = &self.mcp
                    {
                        mcp::record_refusal(mcp, refused.reason());
                    }
                    HttpError(503, refused.message)
                })?;
            }
        }
        answer
            .recv()
            .map_err(|_| HttpError(500, "the request failed inside the session".to_string()))
    }

    /// The host's job queue, if this is the read-only serving mode.
    pub(crate) fn queue(&self) -> Option<&Arc<dyn JobQueue>> {
        match &self.jobs {
            Jobs::Queue(queue) => Some(queue),
            Jobs::Session(_) => None,
        }
    }

    /// A session method that fails with a diagnostic: the failure is a 422 answer.
    pub fn call_fallible<T: Send + 'static, E: std::fmt::Display + Send + 'static>(
        &self,
        job: impl FnOnce(&mut Session) -> Result<T, E> + Send + 'static,
    ) -> Result<T, HttpError> {
        self.call(job)?
            .map_err(|message| HttpError(422, message.to_string()))
    }

    /// The session's statistics with the host's part of the process added
    /// ([`Hooks::add_process_statistics`]). Served by `GET /api/statistics` and the MCP
    /// `statistics` tool.
    pub(crate) fn with_process_memory(&self, mut statistics: Value) -> Value {
        self.hooks.add_process_statistics(self, &mut statistics);
        statistics
    }
}

/// How often the server ticks the scheduled jobs of the active store.
const JOB_TICK: std::time::Duration = std::time::Duration::from_secs(5);

/// Ticks the session's scheduled jobs (book page "Scheduled Jobs") from a thread of its own,
/// in the time zone `zone` (the machine's; `None`: the session's default). Each tick is a short
/// session job like a browser call that only decides what is due; the work happens on
/// job worker threads (one per job group). `Err` when the thread cannot start.
pub fn start_job_ticker(context: &Arc<Context>, zone: Option<String>) -> Result<(), ServeError> {
    if let Some(zone) = zone {
        let _ = context.call(move |session| session.set_time_zone(&zone));
    }
    let context = context.clone();
    std::thread::Builder::new()
        .name("graphersal-jobs".to_string())
        .spawn(move || {
            loop {
                std::thread::sleep(JOB_TICK);
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_micros() as i64);
                let due = context.call(move |session| {
                    let before = position(session).ok();
                    session
                        .start_due_jobs(now)
                        .map(|runs| (before, runs))
                        .map_err(|err| tracing::warn!(%err, "cannot start the scheduled jobs"))
                });
                match due {
                    // The session thread is gone: the server is stopping.
                    Err(_) => break,
                    Ok(Err(())) => {}
                    Ok(Ok((before, runs))) => {
                        for run in runs {
                            job_workers::start(&context, before, run, None);
                        }
                    }
                }
            }
        })
        .map(|_| ())
        .map_err(|err| ServeError::Thread(err.to_string()))
}
