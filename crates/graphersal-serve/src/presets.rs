//! The session presets of a [`crate::Context`] and their change at run time (EXPERIMENTAL): a
//! host changes the query limits of a served graph
//! ([`crate::Context::set_presets`]) without reloading it: connections, MCP sessions and the
//! graph stay.
//!
//! The context keeps the current presets; every job ([`crate::Context::call`]: the HTTP API, the
//! MCP tools, the scheduled jobs' hand-out) first brings the session that runs it up to date
//! ([`Presets::ensure_applied`]: the session's own presets compared with the current ones, the
//! engine rebuilt when they differ), then runs. So a run started after the change runs under the
//! new presets, on the session thread and on every worker of a host's queue alike, and a run
//! already running keeps the presets it started with. Until a host changes them, no job looks at
//! them (one atomic load): the launch presets the session was built with stay in force.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use graphersal_session::{Session, SessionPresets};
use serde_json::Value;

/// A context's session presets: their JSON (`GET /api/info` `presets`) and, once a host changed
/// them, the presets every session runs under.
pub(crate) struct Presets {
    /// Set by the first change: until then the sessions keep their launch presets unchecked.
    changed: AtomicBool,
    current: Mutex<Current>,
}

struct Current {
    /// The presets a host set at run time; `None`: the launch presets (never changed).
    presets: Option<SessionPresets>,
    /// What `GET /api/info` reports: the builder's value, then the set presets' `to_value`.
    value: Value,
}

impl Presets {
    /// The presets of a new context: `value` is the builder's JSON of the launch presets.
    pub(crate) fn new(value: Value) -> Self {
        Self {
            changed: AtomicBool::new(false),
            current: Mutex::new(Current {
                presets: None,
                value,
            }),
        }
    }

    /// Both fields are replaced together by one assignment, which a panic cannot leave
    /// half-written: a poisoned lock is read through.
    fn lock(&self) -> MutexGuard<'_, Current> {
        self.current.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The presets as `GET /api/info` reports them.
    pub(crate) fn value(&self) -> Value {
        self.lock().value.clone()
    }

    /// Makes `presets` the presets of every run that starts from now on.
    pub(crate) fn set(&self, presets: SessionPresets) {
        let value = presets.to_value();
        *self.lock() = Current {
            presets: Some(presets),
            value,
        };
        self.changed.store(true, Ordering::SeqCst);
    }

    /// Brings `session` to the current presets before a job runs on it (nothing to do before
    /// the first change, or when it runs under them already). `Err`: the session's refusal, as
    /// text.
    pub(crate) fn ensure_applied(&self, session: &mut Session) -> Result<(), String> {
        if !self.changed.load(Ordering::SeqCst) {
            return Ok(());
        }
        let presets = {
            let current = self.lock();
            match &current.presets {
                Some(presets) if session.presets() != presets => presets.clone(),
                _ => return Ok(()),
            }
        };
        session
            .set_presets(presets)
            .map_err(|err| format!("cannot apply the new session presets: {err}"))
    }
}
