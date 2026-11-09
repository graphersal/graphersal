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
//! none): a host that wants to see every run (an administrative list, idle tracking, live
//! progress) gives the registry an observer ([`Runs::with_observer`]); it is told of every run's
//! start ([`RunInfo`]: client, kind, the head of the query text, identity, start time) with a
//! [`RunHandle`] that cancels that run, and of its end with its duration. This crate only calls
//! the observer: it offers no endpoint that lists or stops runs.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use graphersal::exec::CancelToken;

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

/// One run as a [`RunObserver`] sees it.
#[derive(Debug, Clone)]
pub struct RunInfo {
    id: u64,
    client: String,
    alias: Option<String>,
    kind: RunKind,
    text: Option<String>,
    identity: Option<String>,
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

    /// When the run was registered (before its job was queued).
    pub fn started_at(&self) -> SystemTime {
        self.started_at
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
}

/// One registered run.
struct Run {
    /// The client's key (`Client::pool_key`): `page:<tab>`, `mcp:<session>`, `ai:<id>`.
    client: String,
    /// The client's own id of the run, if it sent one.
    alias: Option<String>,
    token: CancelToken,
    /// What the observer was told, and when the run started (only with an observer).
    observed: Option<(RunInfo, Instant)>,
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
        self.runs.end(self.id);
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
    pub fn start(&self, client: &str, alias: Option<&str>, token: CancelToken) -> u64 {
        self.start_with(client, alias, token, RunDetails::default())
    }

    /// [`Self::start`] with what the run does (for the observer).
    pub(crate) fn start_with(
        &self,
        client: &str,
        alias: Option<&str>,
        token: CancelToken,
        details: RunDetails,
    ) -> u64 {
        let mut state = self.lock();
        state.next += 1;
        let id = state.next;
        let observed = self.observer.as_ref().map(|_| {
            (
                RunInfo {
                    id,
                    client: client.to_string(),
                    alias: alias.map(str::to_string),
                    kind: details.kind.unwrap_or(RunKind::Other),
                    text: details
                        .text
                        .map(|text| text.chars().take(RUN_TEXT_HEAD_CHARS).collect()),
                    identity: details.identity,
                    started_at: SystemTime::now(),
                },
                Instant::now(),
            )
        });
        let told = observed.as_ref().map(|(info, _)| info.clone());
        state.runs.insert(
            id,
            Run {
                client: client.to_string(),
                alias: alias.map(str::to_string),
                token: token.clone(),
                observed,
            },
        );
        drop(state);
        if let (Some(observer), Some(info)) = (&self.observer, told) {
            observer.run_started(&info, RunHandle { token });
        }
        id
    }

    /// [`Self::start`] ended by dropping the answer.
    pub fn register(
        &self,
        client: &str,
        alias: Option<&str>,
        token: CancelToken,
    ) -> Registered<'_> {
        Registered {
            runs: self,
            id: self.start(client, alias, token),
        }
    }

    /// [`Self::register`] with what the run does (for the observer).
    pub(crate) fn register_with(
        &self,
        client: &str,
        alias: Option<&str>,
        token: CancelToken,
        details: RunDetails,
    ) -> Registered<'_> {
        Registered {
            runs: self,
            id: self.start_with(client, alias, token, details),
        }
    }

    /// The run `id` ended.
    pub fn end(&self, id: u64) {
        let ended = self.lock().runs.remove(&id);
        if let (
            Some(observer),
            Some(Run {
                observed: Some((info, started)),
                ..
            }),
        ) = (&self.observer, ended)
        {
            observer.run_ended(&info, started.elapsed());
        }
    }

    /// Cancels the runs of `client`; whether there was one.
    pub fn cancel_client(&self, client: &str) -> bool {
        self.cancel_where(|run| run.client == client) > 0
    }

    /// Cancels the runs of `client` with its own id `alias`; whether there was one.
    pub fn cancel_alias(&self, client: &str, alias: &str) -> bool {
        self.cancel_where(|run| run.client == client && run.alias.as_deref() == Some(alias)) > 0
    }

    /// Cancels every run (the graceful stop); how many there were.
    pub fn cancel_all(&self) -> usize {
        self.cancel_where(|_| true)
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
        let _a1 = runs.register("page:a", Some("1"), tokens[0].clone());
        let _a2 = runs.register("page:a", Some("2"), tokens[1].clone());
        let _b1 = runs.register("page:b", Some("1"), tokens[2].clone());
        let _m = runs.register("mcp:s", None, tokens[3].clone());

        assert!(runs.cancel_alias("page:a", "2"));
        let cancelled = |tokens: &[CancelToken]| -> Vec<bool> {
            tokens.iter().map(CancelToken::is_cancelled).collect()
        };
        assert_eq!(cancelled(&tokens), [false, true, false, false]);
        // Another client's id of the same text is not this client's run.
        assert!(!runs.cancel_alias("page:c", "1"));
        assert_eq!(cancelled(&tokens), [false, true, false, false]);

        assert!(runs.cancel_client("page:a"));
        assert_eq!(cancelled(&tokens), [true, true, false, false]);
        assert_eq!(runs.cancel_all(), 4);
        assert_eq!(cancelled(&tokens), [true, true, true, true]);
    }

    #[test]
    fn an_ended_run_is_no_longer_cancelled() {
        let runs = Runs::default();
        let token = CancelToken::new();
        drop(runs.register("page:a", Some("1"), token.clone()));
        assert_eq!(runs.len(), 0);
        assert!(!runs.cancel_client("page:a"));
        assert!(!runs.cancel_alias("page:a", "1"));
        assert_eq!(runs.cancel_all(), 0);
        assert!(!token.is_cancelled());
    }
}
