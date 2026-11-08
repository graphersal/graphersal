//! The runs of the dev server that a client may cancel: every query (`POST /api/execute`, an MCP
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
//! client API.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

use graphersal::exec::CancelToken;

/// The longest client id of a run (`runId`) the server accepts.
pub(super) const MAX_ALIAS_CHARS: usize = 128;

/// One registered run.
struct Run {
    /// The client's key ([`super::Client::pool_key`]): `page:<tab>`, `mcp:<session>`, `ai:<id>`.
    client: String,
    /// The client's own id of the run, if it sent one.
    alias: Option<String>,
    token: CancelToken,
}

#[derive(Default)]
struct State {
    next: u64,
    runs: HashMap<u64, Run>,
}

/// The registry of cancellable runs (one per server).
#[derive(Default)]
pub(super) struct Runs {
    state: Mutex<State>,
}

/// Removes its run from the registry when dropped (also when the job panics or is refused).
pub(super) struct Registered<'a> {
    runs: &'a Runs,
    id: u64,
}

impl Drop for Registered<'_> {
    fn drop(&mut self) {
        self.runs.end(self.id);
    }
}

impl Runs {
    /// The state, also after a panic of a holder (every change is one map operation).
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Registers a run of `client` stopped by `token`, with the client's own id `alias`;
    /// returns its id for [`Self::end`].
    pub(super) fn start(&self, client: &str, alias: Option<&str>, token: CancelToken) -> u64 {
        let mut state = self.lock();
        state.next += 1;
        let id = state.next;
        state.runs.insert(
            id,
            Run {
                client: client.to_string(),
                alias: alias.map(str::to_string),
                token,
            },
        );
        id
    }

    /// [`Self::start`] ended by dropping the answer.
    pub(super) fn register(
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

    /// The run `id` ended.
    pub(super) fn end(&self, id: u64) {
        self.lock().runs.remove(&id);
    }

    /// Cancels the runs of `client`; whether there was one.
    pub(super) fn cancel_client(&self, client: &str) -> bool {
        self.cancel_where(|run| run.client == client) > 0
    }

    /// Cancels the runs of `client` with its own id `alias`; whether there was one.
    pub(super) fn cancel_alias(&self, client: &str, alias: &str) -> bool {
        self.cancel_where(|run| run.client == client && run.alias.as_deref() == Some(alias)) > 0
    }

    /// Cancels every run (the graceful stop); how many there were.
    pub(super) fn cancel_all(&self) -> usize {
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
    pub(super) fn len(&self) -> usize {
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
