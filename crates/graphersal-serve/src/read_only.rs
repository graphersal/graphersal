//! The runtime read-only toggle of a [`crate::Context`] (EXPERIMENTAL; plan
//! `ai/graphersal-server/PLAN.md` H2, decision Q2): a host that asked for it
//! ([`crate::ContextBuilder::runtime_read_only`]) turns read-only on and off while the graph is
//! served ([`crate::Context::set_read_only`]), without reopening it: connections and sessions
//! stay.
//!
//! Two parts refuse the writes while it is on:
//! - a commit hook on the served graph ([`ReadOnlyGate`]) vetoes every unit that changes
//!   something (data, schema, catalog: the queries of the HTTP API and of MCP, the element
//!   editor, the schema and catalog endpoints, the scheduled queries), so they are all refused
//!   with ONE diagnostic: [`READ_ONLY_HEADLINE`] and the host's help text. A veto runs before
//!   the Store's journal, so nothing reaches the disk. The hook is registered the first time
//!   read-only is turned on (no change capture before that) and stays (hooks are never
//!   removed): while read-only is off it lets every commit through;
//! - the HTTP API refuses the writes that are no commit (store checkpoint, mark, rollback,
//!   attic restore and remove, compaction; the scheduled jobs' define, drop, enable, schedule
//!   and run; save to file) with `409` and the same text, and the job ticker starts no
//!   scheduled job.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use graphersal::changes::{ChangeSet, CommitHook, HookError, HookRefusal};
use graphersal_session::Session;

/// The headline of every refusal while a context is read-only at run time: the same words as
/// the `graphersal --server --read-only` refusal, so a client sees one message whichever way
/// the graph became read-only.
pub const READ_ONLY_HEADLINE: &str = "the graph is served read-only: writes are refused";

/// The help text of a runtime read-only refusal when the host gives none
/// ([`crate::ContextBuilder::runtime_read_only`]).
pub const READ_ONLY_HELP: &str =
    "An operator turned read-only on; ask them to turn it off to change the data.";

/// The help text of the `409` that refuses a write on a request the host marked read-only
/// ([`crate::Request::mark_read_only`]): `"{READ_ONLY_HEADLINE}. {READ_ONLY_REQUEST_HELP}"`.
pub const READ_ONLY_REQUEST_HELP: &str =
    "This request has read access only: ask for write access to change the data.";

/// The commit hook of the runtime read-only toggle: vetoes every unit while `on` holds.
pub(crate) struct ReadOnlyGate {
    on: Arc<AtomicBool>,
    help: Arc<str>,
}

impl CommitHook for ReadOnlyGate {
    fn name(&self) -> &str {
        "runtime-read-only"
    }

    fn before_commit(&mut self, _: &ChangeSet) -> Result<(), HookError> {
        if self.on.load(Ordering::SeqCst) {
            return Err(Box::new(HookRefusal::new(
                READ_ONLY_HEADLINE,
                self.help.to_string(),
            )));
        }
        Ok(())
    }
}

/// A context's toggle: the flag (shared with the context's `readOnly`), the help text and
/// whether the gate is on the graph yet.
pub(crate) struct Toggle {
    pub(crate) on: Arc<AtomicBool>,
    help: Arc<str>,
    registered: Mutex<bool>,
}

impl Toggle {
    pub(crate) fn new(on: Arc<AtomicBool>, help: Option<String>) -> Self {
        Self {
            on,
            help: Arc::from(help.as_deref().unwrap_or(READ_ONLY_HELP)),
            registered: Mutex::new(false),
        }
    }

    /// Whether read-only is on now.
    pub(crate) fn is_on(&self) -> bool {
        self.on.load(Ordering::SeqCst)
    }

    /// The text of the HTTP refusals (`409`): the headline and the help.
    pub(crate) fn refusal(&self) -> String {
        format!("{READ_ONLY_HEADLINE}. {}", self.help)
    }

    /// Registers the gate on the graph `session` serves (on a Store: its live graph, also while
    /// a view of a past state is open), once. `Err`: the graph's refusal (a storage without
    /// commit hooks), as text.
    pub(crate) fn ensure_registered(&self, session: &Session) -> Result<(), String> {
        let mut registered = self
            .registered
            .lock()
            .map_err(|_| "the read-only toggle is unusable (a job panicked)".to_string())?;
        if *registered {
            return Ok(());
        }
        let graph = match session.active_store() {
            Some(store) => store.session_graph(),
            None => session.graph().clone(),
        };
        graph
            .register_hook(Box::new(ReadOnlyGate {
                on: self.on.clone(),
                help: self.help.clone(),
            }))
            .map_err(|err| format!("cannot turn read-only on: {err}"))?;
        *registered = true;
        Ok(())
    }
}
