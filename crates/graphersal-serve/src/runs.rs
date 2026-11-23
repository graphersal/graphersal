//! The runs of the server that a client may cancel: every query (`POST /api/execute`, an MCP
//! `query`/`gql`/saved query, an AI Chat tool call) and every force-layout slice.
//!
//! Each run gets a [`CancelToken`] of its own, registered here BEFORE its job is queued for a
//! session thread (or a pool worker) and removed when the job has ended. A cancel therefore
//! reaches exactly the runs it names, also while they still wait in the queue (the run then
//! stops at once when it starts), never another client's run, never the next job of the same
//! session, and never work that is not a run (a page's drawing, a memory report). Issues #29,
//! #30 and #31 were three ways a shared, reused token lost a cancel or hit the wrong work.
//!
//! A run is cancelled by:
//! - its client and the client's own id of the run ([`Runs::cancel_alias`]): a page's `runId`
//!   (a query tab's Stop), an MCP request id (`notifications/cancelled`);
//! - its client ([`Runs::cancel_client`]): `POST /api/cancel` without a `runId`;
//! - all at once ([`Runs::cancel_all`]): the graceful stop (SIGTERM).
//!
//! Stopping another client's runs is an administrative act and not part of this module's
//! client API. **Extension point "run observer + cancel handle"** ([`RunObserver`], default:
//! none): a host that wants to see every run (idle tracking, live progress, its own list) gives
//! the registry an observer ([`Runs::with_observer`]); it is told of every run's start
//! ([`RunInfo`]: client, kind, the head of the query text, identity, mount, start time) with a
//! [`RunHandle`] that cancels that run, and of its end with its duration.
//!
//! **Run administration** (issue #32, off by default): [`Runs::list`] answers every registered
//! run, queued or running ([`RunEntry`], [`RunState`]), whatever its client, and
//! [`Runs::cancel_run`] cancels one by the registry's id. A context serves both over HTTP only
//! when its host turns it on (`ContextBuilder::admin_runs`: `GET /api/admin/runs`, `POST
//! /api/admin/runs/cancel`), since the query texts are other clients' data.
//!
//! A run counts as running once the first session job queued by the thread that registered it
//! has started (`Context::call` marks the runs of its thread; [`pending_runs`]); before, it is
//! queued.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use graphersal::exec::CancelToken;

use crate::context::ServeError;

/// The longest client id of a run (`runId`) the server accepts.
pub(crate) const MAX_ALIAS_CHARS: usize = 128;

/// How many characters of a run's query text a [`RunObserver`] is shown ([`RunInfo::text`]).
pub const RUN_TEXT_HEAD_CHARS: usize = 200;

/// What a run does.
///
/// `#[non_exhaustive]`: further kinds of runs may be added.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RunKind {
    /// A query of the HTTP API (`POST /api/execute`).
    Query,
    /// A slice of a force layout (`POST /api/layout`, `/api/layout/step`).
    Layout,
    /// A query of a tool call of the MCP endpoint or of a host's agent (the AI Chat): the
    /// client name tells which.
    Tool,
    /// A run registered through [`Runs::start`] / [`Runs::register`] without a kind.
    Other,
}

impl RunKind {
    /// The kind's name in the HTTP API: `query`, `layout`, `tool`, `other`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Query => "query",
            Self::Layout => "layout",
            Self::Tool => "tool",
            Self::Other => "other",
        }
    }
}

/// Whether a listed run waits for its session job or runs ([`RunEntry::state`]).
///
/// `#[non_exhaustive]`: further states may be added.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RunState {
    /// Registered; its job waits in the queue (of the session thread or the host's).
    Queued,
    /// Its first session job has started.
    Running,
}

impl RunState {
    /// The state's name in the HTTP API: `queued`, `running`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
        }
    }
}

/// One run as a [`RunObserver`] sees it.
#[derive(Debug, Clone)]
pub struct RunInfo {
    id: u64,
    client: String,
    alias: Option<String>,
    kind: RunKind,
    text: Option<String>,
    identity: Option<String>,
    mount: Option<String>,
    started_at: SystemTime,
}

impl RunInfo {
    /// The registry's id of the run (unique within the registry).
    pub fn id(&self) -> u64 {
        self.id
    }

    /// The client's key: `page:<X-Graphersal-Client>`, `mcp:<MCP session id>` (or `mcp`),
    /// `ai:<conversation>`.
    pub fn client(&self) -> &str {
        &self.client
    }

    /// The client's own id of the run (a page's `runId`, an MCP request id), if it sent one.
    pub fn alias(&self) -> Option<&str> {
        self.alias.as_deref()
    }

    /// What the run does.
    pub fn kind(&self) -> RunKind {
        self.kind
    }

    /// The first [`RUN_TEXT_HEAD_CHARS`] characters of the query text, when the run has one.
    pub fn text(&self) -> Option<&str> {
        self.text.as_deref()
    }

    /// The identity a request filter attached to the request that started the run
    /// ([`crate::Request::set_identity`]), if any.
    pub fn identity(&self) -> Option<&str> {
        self.identity.as_deref()
    }

    /// The mount path of the context that registered the run (`ContextBuilder::mount_path`,
    /// `/db/acme`), or `None`: a context at the server's root, or a run registered through
    /// [`Runs::start`] / [`Runs::register`].
    pub fn mount(&self) -> Option<&str> {
        self.mount.as_deref()
    }

    /// When the run was registered (before its job was queued).
    pub fn started_at(&self) -> SystemTime {
        self.started_at
    }
}

/// One run as [`Runs::list`] answers it: what it is, whether it runs, how long since it was
/// registered.
#[derive(Debug, Clone)]
pub struct RunEntry {
    info: RunInfo,
    state: RunState,
    duration: Duration,
    cancelled: bool,
}

impl RunEntry {
    /// What the run is (id, client, kind, query text head, identity, mount, start time).
    pub fn info(&self) -> &RunInfo {
        &self.info
    }

    /// Whether the run waits for its job or runs.
    pub fn state(&self) -> RunState {
        self.state
    }

    /// The time since the run was registered (waiting included).
    pub fn duration(&self) -> Duration {
        self.duration
    }

    /// Whether the run was cancelled (it ends at its next check, or when it starts).
    pub fn is_cancelled(&self) -> bool {
        self.cancelled
    }
}

/// Cancels one run: the handle a [`RunObserver`] gets with each run's start. A cancel of a
/// waiting run stops it when it starts; of a running one at its next check (its unit rolls
/// back). Cancelling an ended run does nothing.
#[derive(Clone)]
pub struct RunHandle {
    token: CancelToken,
}

impl RunHandle {
    /// Cancels the run.
    pub fn cancel(&self) {
        self.token.cancel();
    }

    /// Whether the run was cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }
}

impl fmt::Debug for RunHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RunHandle")
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

/// **Extension point "run observer + cancel handle"**: told of every run's start and end.
/// Default: none (a registry without an observer calls nothing).
///
/// Both methods are called on the thread that registers or ends the run (a request thread, an
/// agent's turn thread), outside the registry's lock; they must return quickly. Every method
/// has a default that does nothing.
pub trait RunObserver: Send + Sync {
    /// A run was registered (before its job is queued); `handle` cancels it.
    fn run_started(&self, run: &RunInfo, handle: RunHandle) {
        let _ = (run, handle);
    }

    /// The run ended (answered, refused, failed or cancelled) after `duration`.
    fn run_ended(&self, run: &RunInfo, duration: Duration) {
        let _ = (run, duration);
    }
}

/// What a run registration says about the run, beyond its client and token.
#[derive(Default)]
pub(crate) struct RunDetails {
    pub(crate) kind: Option<RunKind>,
    pub(crate) text: Option<String>,
    pub(crate) identity: Option<String>,
    pub(crate) mount: Option<String>,
}

/// One registered run.
struct Run {
    /// What the run is (also what the observer was told); `info.client` is the client's key
    /// (`Client::pool_key`): `page:<tab>`, `mcp:<session>`, `ai:<id>`.
    info: RunInfo,
    token: CancelToken,
    /// When the run was registered.
    started: Instant,
    /// Set when its first session job starts ([`pending_runs`]).
    running: Arc<AtomicBool>,
}

thread_local! {
    /// The running flags of the runs this thread registered and has not ended yet: the next
    /// session job this thread queues marks them running when it starts ([`pending_runs`]).
    static PENDING: RefCell<Vec<Arc<AtomicBool>>> = const { RefCell::new(Vec::new()) };
}

/// The running flags of the runs the current thread registered that are not yet running: a
/// job queued now (`Context::call`) sets them when it starts. A flag whose run ended on
/// another thread is dropped here (only this list still holds it).
pub(crate) fn pending_runs() -> Vec<Arc<AtomicBool>> {
    PENDING.with(|pending| {
        let mut pending = pending.borrow_mut();
        pending.retain(|flag| Arc::strong_count(flag) > 1);
        pending
            .iter()
            .filter(|flag| !flag.load(Ordering::Relaxed))
            .cloned()
            .collect()
    })
}

/// Marks runs of [`pending_runs`] running (on the thread that runs their job).
pub(crate) fn mark_running(flags: &[Arc<AtomicBool>]) {
    for flag in flags {
        flag.store(true, Ordering::Relaxed);
    }
}

#[derive(Default)]
struct State {
    next: u64,
    runs: HashMap<u64, Run>,
}

/// The registry of cancellable runs (one per server), optionally with a [`RunObserver`].
#[derive(Default)]
pub struct Runs {
    state: Mutex<State>,
    observer: Option<Arc<dyn RunObserver>>,
}

/// Removes its run from the registry when dropped (also when the job panics or is refused).
pub struct Registered<'a> {
    runs: &'a Runs,
    id: u64,
}

impl Drop for Registered<'_> {
    fn drop(&mut self) {
        if let Err(err) = self.runs.end(self.id) {
            tracing::warn!(target: "graphersal::server", %err, run = self.id, "cannot end a run");
        }
    }
}

impl Runs {
    /// A registry that tells `observer` of every run's start and end.
    pub fn with_observer(observer: Arc<dyn RunObserver>) -> Self {
        Self {
            state: Mutex::default(),
            observer: Some(observer),
        }
    }

    /// The state, also after a panic of a holder (every change is one map operation).
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Registers a run of `client` stopped by `token`, with the client's own id `alias`;
    /// returns its id for [`Self::end`]. An observer sees it as [`RunKind::Other`].
    ///
    /// The registry's methods answer `Result` (it is shared state); none fails today.
    pub fn start(
        &self,
        client: &str,
        alias: Option<&str>,
        token: CancelToken,
    ) -> Result<u64, ServeError> {
        self.start_with(client, alias, token, RunDetails::default())
    }

    /// [`Self::start`] with what the run does (for the observer).
    pub(crate) fn start_with(
        &self,
        client: &str,
        alias: Option<&str>,
        token: CancelToken,
        details: RunDetails,
    ) -> Result<u64, ServeError> {
        let mut state = self.lock();
        state.next += 1;
        let id = state.next;
        let info = RunInfo {
            id,
            client: client.to_string(),
            alias: alias.map(str::to_string),
            kind: details.kind.unwrap_or(RunKind::Other),
            text: details
                .text
                .map(|text| text.chars().take(RUN_TEXT_HEAD_CHARS).collect()),
            identity: details.identity,
            mount: details.mount,
            started_at: SystemTime::now(),
        };
        let told = self.observer.as_ref().map(|_| info.clone());
        let running = Arc::new(AtomicBool::new(false));
        state.runs.insert(
            id,
            Run {
                info,
                token: token.clone(),
                started: Instant::now(),
                running: running.clone(),
            },
        );
        drop(state);
        PENDING.with(|pending| {
            let mut pending = pending.borrow_mut();
            pending.retain(|flag| Arc::strong_count(flag) > 1);
            pending.push(running);
        });
        if let (Some(observer), Some(info)) = (&self.observer, told) {
            observer.run_started(&info, RunHandle { token });
        }
        Ok(id)
    }

    /// [`Self::start`] ended by dropping the answer.
    pub fn register(
        &self,
        client: &str,
        alias: Option<&str>,
        token: CancelToken,
    ) -> Result<Registered<'_>, ServeError> {
        Ok(Registered {
            runs: self,
            id: self.start(client, alias, token)?,
        })
    }

    /// [`Self::register`] with what the run does (for the observer).
    pub(crate) fn register_with(
        &self,
        client: &str,
        alias: Option<&str>,
        token: CancelToken,
        details: RunDetails,
    ) -> Result<Registered<'_>, ServeError> {
        Ok(Registered {
            runs: self,
            id: self.start_with(client, alias, token, details)?,
        })
    }

    /// The run `id` ended.
    pub fn end(&self, id: u64) -> Result<(), ServeError> {
        let ended = self.lock().runs.remove(&id);
        if let Some(run) = ended {
            // Usually ended on the thread that registered it: forget its flag there.
            PENDING.with(|pending| {
                pending
                    .borrow_mut()
                    .retain(|flag| !Arc::ptr_eq(flag, &run.running));
            });
            if let Some(observer) = &self.observer {
                observer.run_ended(&run.info, run.started.elapsed());
            }
        }
        Ok(())
    }

    /// Every registered run, queued or running, whatever its client, in the order they were
    /// registered: the **run administration**'s list (issue #32). The query texts in it are
    /// other clients' data: a host answers it only to its administrator (a context serves it
    /// only with `ContextBuilder::admin_runs`).
    pub fn list(&self) -> Result<Vec<RunEntry>, ServeError> {
        let state = self.lock();
        let mut runs: Vec<RunEntry> = state
            .runs
            .values()
            .map(|run| RunEntry {
                info: run.info.clone(),
                state: if run.running.load(Ordering::Relaxed) {
                    RunState::Running
                } else {
                    RunState::Queued
                },
                duration: run.started.elapsed(),
                cancelled: run.token.is_cancelled(),
            })
            .collect();
        drop(state);
        runs.sort_by_key(|run| run.info.id);
        Ok(runs)
    }

    /// Cancels the run `id` ([`RunInfo::id`]) whatever its client: the **run
    /// administration**'s kill (issue #32), separate from the client's own cancel. Whether
    /// the run was registered (an ended run is not, and nothing happens).
    pub fn cancel_run(&self, id: u64) -> Result<bool, ServeError> {
        Ok(self.cancel_where(|run| run.info.id == id) > 0)
    }

    /// [`Self::cancel_run`] of a run registered under `mount` only (a context's own runs).
    pub(crate) fn cancel_run_in(&self, id: u64, mount: Option<&str>) -> bool {
        self.cancel_where(|run| run.info.id == id && run.info.mount.as_deref() == mount) > 0
    }

    /// Cancels the runs of `client`; whether there was one.
    pub fn cancel_client(&self, client: &str) -> Result<bool, ServeError> {
        Ok(self.cancel_where(|run| run.info.client == client) > 0)
    }

    /// Cancels the runs of `client` with its own id `alias`; whether there was one.
    pub fn cancel_alias(&self, client: &str, alias: &str) -> Result<bool, ServeError> {
        Ok(self.cancel_where(|run| {
            run.info.client == client && run.info.alias.as_deref() == Some(alias)
        }) > 0)
    }

    /// Cancels every run (the graceful stop); how many there were.
    pub fn cancel_all(&self) -> Result<usize, ServeError> {
        Ok(self.cancel_where(|_| true))
    }

    fn cancel_where(&self, matches: impl Fn(&Run) -> bool) -> usize {
        let state = self.lock();
        let mut cancelled = 0;
        for run in state.runs.values().filter(|run| matches(run)) {
            run.token.cancel();
            cancelled += 1;
        }
        cancelled
    }

    /// How many runs are registered.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.lock().runs.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cancel_reaches_only_the_runs_it_names() {
        let runs = Runs::default();
        let tokens: Vec<CancelToken> = (0..4).map(|_| CancelToken::new()).collect();
        let _a1 = runs
            .register("page:a", Some("1"), tokens[0].clone())
            .unwrap();
        let _a2 = runs
            .register("page:a", Some("2"), tokens[1].clone())
            .unwrap();
        let _b1 = runs
            .register("page:b", Some("1"), tokens[2].clone())
            .unwrap();
        let _m = runs.register("mcp:s", None, tokens[3].clone()).unwrap();

        assert!(runs.cancel_alias("page:a", "2").unwrap());
        let cancelled = |tokens: &[CancelToken]| -> Vec<bool> {
            tokens.iter().map(CancelToken::is_cancelled).collect()
        };
        assert_eq!(cancelled(&tokens), [false, true, false, false]);
        // Another client's id of the same text is not this client's run.
        assert!(!runs.cancel_alias("page:c", "1").unwrap());
        assert_eq!(cancelled(&tokens), [false, true, false, false]);

        assert!(runs.cancel_client("page:a").unwrap());
        assert_eq!(cancelled(&tokens), [true, true, false, false]);
        assert_eq!(runs.cancel_all().unwrap(), 4);
        assert_eq!(cancelled(&tokens), [true, true, true, true]);
    }

    #[test]
    fn an_ended_run_is_no_longer_cancelled() {
        let runs = Runs::default();
        let token = CancelToken::new();
        drop(runs.register("page:a", Some("1"), token.clone()).unwrap());
        assert_eq!(runs.len(), 0);
        assert!(!runs.cancel_client("page:a").unwrap());
        assert!(!runs.cancel_alias("page:a", "1").unwrap());
        assert_eq!(runs.cancel_all().unwrap(), 0);
        assert!(!token.is_cancelled());
    }

    #[test]
    fn the_list_answers_every_client_and_the_state_and_cancel_by_id() {
        let runs = Runs::default();
        let a = CancelToken::new();
        let b = CancelToken::new();
        let _a = runs
            .register_with(
                "page:a",
                Some("1"),
                a.clone(),
                RunDetails {
                    kind: Some(RunKind::Query),
                    text: Some("é".repeat(RUN_TEXT_HEAD_CHARS + 5)),
                    ..RunDetails::default()
                },
            )
            .unwrap();
        // The job this thread queues next marks the run running when it starts.
        let pending = pending_runs();
        assert_eq!(pending.len(), 1);
        let _b = runs.register("mcp:s", None, b.clone()).unwrap();
        let listed = runs.list().unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].info().client(), "page:a");
        assert_eq!(listed[0].info().kind(), RunKind::Query);
        assert_eq!(
            listed[0].info().text().unwrap().chars().count(),
            RUN_TEXT_HEAD_CHARS
        );
        assert_eq!(listed[0].state(), RunState::Queued);
        assert_eq!(listed[1].info().kind(), RunKind::Other);
        assert_eq!(listed[1].info().mount(), None);
        mark_running(&pending);
        assert_eq!(runs.list().unwrap()[0].state(), RunState::Running);
        assert_eq!(runs.list().unwrap()[1].state(), RunState::Queued);
        // Already running: no longer pending; the other one still is.
        assert_eq!(pending_runs().len(), 1);

        let id_b = listed[1].info().id();
        assert!(runs.cancel_run(id_b).unwrap());
        assert!(b.is_cancelled() && !a.is_cancelled());
        assert!(runs.list().unwrap()[1].is_cancelled());
        assert!(!runs.cancel_run(999).unwrap());
    }

    #[test]
    fn an_ended_run_leaves_the_list_and_the_pending_flags() {
        let runs = Runs::default();
        let id = runs.start("page:a", None, CancelToken::new()).unwrap();
        assert_eq!(pending_runs().len(), 1);
        runs.end(id).unwrap();
        assert!(runs.list().unwrap().is_empty());
        assert!(pending_runs().is_empty());
        assert!(!runs.cancel_run(id).unwrap());
        // Ended on another thread: the flag is dropped from this thread's list on its next read.
        let id = runs.start("page:a", None, CancelToken::new()).unwrap();
        std::thread::scope(|scope| {
            scope.spawn(|| runs.end(id).unwrap());
        });
        assert!(pending_runs().is_empty());
    }
}
