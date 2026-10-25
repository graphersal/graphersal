//! The limits of one job run ([`JobLimits`]: a timeout and a memory limit) and the scope that
//! carries them to every traversal the run executes.
//!
//! The engine cannot stop arbitrary code from outside (no thread of its own, none in wasm32), so
//! the limits reach the checks that already run inside every execution: while a job runs, its
//! remaining time caps each traversal's `evaluationTimeout` and its memory limit caps
//! `memory.limit` (a query's own stricter value stays). A breach is the engine's ordinary
//! `Timeout` / `ResourceLimitExceeded`; the work is rolled back and the run recorded as failed.

use std::cell::RefCell;
use std::time::Duration;

use web_time::Instant;

/// What one run of a job may use: at most `timeout` of wall-clock time and `memory_limit`
/// bytes (each optional).
///
/// ```
/// use std::time::Duration;
/// use graphersal::jobs::JobLimits;
///
/// let limits = JobLimits::new()
///     .with_timeout(Duration::from_secs(30))
///     .with_memory_limit(256 << 20);
/// assert_eq!(limits.timeout, Some(Duration::from_secs(30)));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct JobLimits {
    /// The longest a run may take.
    pub timeout: Option<Duration>,
    /// The most memory a run may use, in bytes (the traversals' `memory.limit`).
    pub memory_limit: Option<u64>,
}

impl JobLimits {
    /// No limits.
    pub fn new() -> Self {
        Self::default()
    }

    /// With a timeout per run.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// With a memory limit per run, in bytes.
    pub fn with_memory_limit(mut self, bytes: u64) -> Self {
        self.memory_limit = Some(bytes);
        self
    }
}

/// The limits of the running job on this thread: its deadline and memory limit.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RunLimits {
    pub(crate) deadline: Option<Instant>,
    pub(crate) memory_limit: Option<u64>,
}

impl RunLimits {
    /// The time left until the deadline (at least one microsecond: zero would mean "no budget"
    /// to `evaluationTimeout`), `None` without one.
    pub(crate) fn remaining(&self) -> Option<Duration> {
        self.deadline.map(|at| {
            at.checked_duration_since(Instant::now())
                .unwrap_or_default()
                .max(Duration::from_micros(1))
        })
    }
}

thread_local! {
    static CURRENT: RefCell<Option<RunLimits>> = const { RefCell::new(None) };
}

/// Runs `run` with `limits` as the current thread's job limits (restored afterwards, also on a
/// panic).
pub(crate) fn scope<R>(limits: RunLimits, run: impl FnOnce() -> R) -> R {
    struct Restore(Option<RunLimits>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let previous = self.0.take();
            let _ = CURRENT.try_with(|current| {
                if let Ok(mut current) = current.try_borrow_mut() {
                    *current = previous;
                }
            });
        }
    }
    let previous = CURRENT
        .try_with(|current| {
            current
                .try_borrow_mut()
                .ok()
                .and_then(|mut current| current.replace(limits))
        })
        .ok()
        .flatten();
    let _restore = Restore(previous);
    run()
}

/// The limits of the job running on this thread, if any.
pub(crate) fn current() -> Option<RunLimits> {
    CURRENT
        .try_with(|current| current.try_borrow().ok().and_then(|c| *c))
        .ok()
        .flatten()
}
