//! SIGINT and SIGTERM (Ctrl+C, `kill`, `docker stop`): the running query is cancelled and a
//! Store open in this process is closed cleanly before the process exits, so its next open
//! reports a clean close and replays nothing it does not have to. A second signal while the
//! store closes exits at once.
//!
//! As PID 1 of a container the kernel applies no default action to SIGTERM: a process that
//! installs no handler ignores it, and `docker stop` waits its grace period and kills it. The
//! dev server therefore always installs the handler ([`Stop::install`]), with or without a
//! Store; the one-shot runner and the REPL install one without a Store only as PID 1
//! ([`exit_on_signal_as_pid1`]), elsewhere the default action already ends them.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use graphersal::exec::CancelToken;
use graphersal::persist::Store;

/// The exit code after an interruption (128 + SIGINT, as shells report it).
pub const EXIT_INTERRUPTED: i32 = 130;

/// What the signal handler stops, filled in as the process learns it: the dev server installs
/// the handler before its graph is loaded and adds the store and the session's cancellation
/// token later. [`Stop::stopping`] turns true at the first signal (the server's readiness).
#[derive(Default)]
pub struct Stop {
    store: Mutex<Option<Arc<Store>>>,
    cancel: Mutex<Option<CancelToken>>,
    /// The graceful stop of the read-only serving mode ([`Stop::set_drain`]).
    drain: Mutex<Option<Drain>>,
    /// Runs beside the cancel of [`Stop::set_cancel`] ([`Stop::set_on_stop`]).
    on_stop: Mutex<Option<Drain>>,
    signals: AtomicUsize,
}

/// A graceful stop: runs on a thread of its own after the first signal (no new jobs, the
/// running ones finish or are cancelled), then the process exits.
pub type Drain = Arc<dyn Fn() + Send + Sync>;

impl Stop {
    /// The Store a signal closes before the process exits.
    pub fn set_store(&self, store: Arc<Store>) {
        if let Ok(mut slot) = self.store.lock() {
            *slot = Some(store);
        }
    }

    /// The token a signal cancels (the running query stops at its next check).
    pub fn set_cancel(&self, cancel: CancelToken) {
        if let Ok(mut slot) = self.cancel.lock() {
            *slot = Some(cancel);
        }
    }

    /// What a signal runs beside cancelling the token of [`Stop::set_cancel`], before the
    /// store closes (the read-write dev server cancels its clients' runs, each with a token of
    /// its own).
    #[cfg_attr(not(feature = "dev-server"), allow(dead_code))]
    pub fn set_on_stop(&self, on_stop: Drain) {
        if let Ok(mut slot) = self.on_stop.lock() {
            *slot = Some(on_stop);
        }
    }

    /// The graceful stop a signal runs instead of cancelling at once (the read-only serving
    /// mode, which has no Store): `drain` returns once the running jobs ended or were
    /// cancelled, then the process exits with the handler's exit code.
    #[cfg_attr(not(feature = "dev-server"), allow(dead_code))]
    pub fn set_drain(&self, drain: Drain) {
        if let Ok(mut slot) = self.drain.lock() {
            *slot = Some(drain);
        }
    }

    /// Whether a signal arrived: the process is stopping.
    #[cfg_attr(not(feature = "dev-server"), allow(dead_code))]
    pub fn stopping(&self) -> bool {
        self.signals.load(Ordering::SeqCst) > 0
    }

    /// Installs the process's one handler: the first signal cancels the running query, closes
    /// the store (when one was set) and exits with `exit_code`; without a store it prints
    /// `without_store` (when not empty) and exits with `exit_code` at once. A second signal
    /// exits with [`EXIT_INTERRUPTED`] immediately. A failure to install it is a warning.
    pub fn install(self: &Arc<Self>, exit_code: i32, without_store: &'static str) {
        let stop = self.clone();
        let installed = ctrlc::set_handler(move || stop.on_signal(exit_code, without_store));
        if let Err(err) = installed {
            errln!(
                "warning: cannot handle Ctrl+C ({err}); stop with /exit to close the store cleanly"
            );
        }
    }

    fn on_signal(&self, exit_code: i32, without_store: &'static str) {
        if self.signals.fetch_add(1, Ordering::SeqCst) > 0 {
            errln!("Interrupted again: exiting without waiting for the store to close.");
            std::process::exit(EXIT_INTERRUPTED);
        }
        if let Some(drain) = self.drain.lock().ok().and_then(|slot| slot.clone()) {
            // The handler thread stays free for a second signal meanwhile.
            let draining = std::thread::Builder::new()
                .name("graphersal-drain".into())
                .spawn(move || {
                    drain();
                    if !without_store.is_empty() {
                        errln!("{without_store}");
                    }
                    std::process::exit(exit_code);
                });
            if draining.is_err() {
                std::process::exit(exit_code);
            }
            return;
        }
        if let Some(cancel) = self.cancel.lock().ok().and_then(|slot| slot.clone()) {
            cancel.cancel();
        }
        if let Some(on_stop) = self.on_stop.lock().ok().and_then(|slot| slot.clone()) {
            on_stop();
        }
        let Some(store) = self.store.lock().ok().and_then(|slot| slot.clone()) else {
            if !without_store.is_empty() {
                errln!("{without_store}");
            }
            std::process::exit(exit_code);
        };
        // The close waits for a running commit; the handler thread stays free for a second
        // signal meanwhile.
        let closing = std::thread::Builder::new()
            .name("graphersal-close".into())
            .spawn(move || {
                errln!(
                    "Stopping: closing the store {} (Ctrl+C again to exit at once)...",
                    store.location()
                );
                match store.shutdown() {
                    Ok(()) if store.maintenance().is_some() => {
                        errln!(
                            "The store {} was left as it was (maintenance mode).",
                            store.location()
                        );
                    }
                    Ok(()) => errln!("The store {} was closed cleanly.", store.location()),
                    Err(err) => errln!("{}", graphersal::error::format_diagnostic(&err)),
                }
                std::process::exit(exit_code);
            });
        if closing.is_err() {
            std::process::exit(EXIT_INTERRUPTED);
        }
    }
}

/// Installs the handler for `store`: the first signal cancels the running query (`cancel`),
/// closes `store` and exits with `exit_code`; a second one exits with [`EXIT_INTERRUPTED`]
/// immediately. Only one handler per process.
pub fn close_store_on_signal(store: Arc<Store>, cancel: Option<CancelToken>, exit_code: i32) {
    let stop = Arc::new(Stop::default());
    stop.set_store(store);
    if let Some(cancel) = cancel {
        stop.set_cancel(cancel);
    }
    stop.install(exit_code, "");
}

/// Without a Store, as PID 1 only (a container's first process, where SIGINT and SIGTERM are
/// ignored without a handler): a signal exits with [`EXIT_INTERRUPTED`]. Elsewhere the
/// default action of the signal ends the process already, so nothing is installed.
pub fn exit_on_signal_as_pid1() {
    if std::process::id() == 1 {
        Arc::new(Stop::default()).install(EXIT_INTERRUPTED, "Interrupted.");
    }
}
