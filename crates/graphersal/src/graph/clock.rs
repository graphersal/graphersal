//! The instance clock ([`CommitClock`]): one monotonic time source in microseconds since the
//! Unix epoch (UTC) that the commit times, the mark and checkpoint times and (later) the system
//! journal of one instance share, so an event about commit N is never earlier than commit N and
//! a commit is never earlier than a record written before it.
//!
//! The clock reads the system clock through `web_time` (never `std::time`, which panics on
//! wasm32) and never goes back: every reading is `max(system clock, floor)` and raises the
//! floor to itself. The floor is not stored: after a restart a Store raises it again from the
//! latest time in its files (format spec 2, "Conventions", and 18).

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, OnceLock};

/// How far the system clock may lie behind the floor before a reading reports a step back: one
/// second (an NTP slew stays below it; a resumed VM or a manual change shows).
pub(crate) const STEP_BACK_THRESHOLD_US: i64 = 1_000_000;

/// Told when a reading of a [`CommitClock`] finds the system clock more than
/// [`STEP_BACK_THRESHOLD_US`] behind the floor. Without an observer the clock writes a
/// `tracing` warning instead; the system journal installs one that records a `clock.step_back`
/// event (and collapses repeats).
pub(crate) trait ClockObserver: Send + Sync {
    /// The system clock read `system` while the floor was `floor` (both microseconds since the
    /// Unix epoch); the reading returned `floor`.
    fn stepped_back(&self, system: i64, floor: i64);
}

/// One instance's monotonic clock, in microseconds since the Unix epoch (UTC). Cheap to clone:
/// the clones share one state (an `Arc`), so every part of an instance that holds a clone reads
/// the same, never-decreasing time.
///
/// Times are non-strictly monotonic: two readings may be equal, never decreasing. A storage that
/// captures changes embeds one inside its [`CommitHooks`](crate::graph::CommitHooks) and times
/// its commits with [`CommitHooks::commit_time`](crate::graph::CommitHooks::commit_time);
/// [`GraphStorage::commit_clock`](crate::graph::GraphStorage::commit_clock) hands it out.
///
/// ```rust
/// use graphersal::storage::CommitClock;
///
/// let clock = CommitClock::new();
/// let far_future = clock.now() + 3_600_000_000;
/// clock.raise(far_future);
/// // The system clock is an hour behind the floor: the clock stays at the floor.
/// assert_eq!(clock.now(), far_future);
/// clock.raise(0); // never lowers
/// assert_eq!(clock.floor(), far_future);
/// ```
#[derive(Clone)]
pub struct CommitClock(Arc<ClockState>);

struct ClockState {
    /// The latest time handed out or raised to (µs).
    floor: AtomicI64,
    /// Where readings come from: the system clock, a fake one in the crate's unit tests.
    source: fn() -> i64,
    /// Told about a step back of the system clock (`None`: a `tracing` warning).
    observer: OnceLock<Box<dyn ClockObserver>>,
}

impl std::fmt::Debug for CommitClock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommitClock")
            .field("floor", &self.floor())
            .finish_non_exhaustive()
    }
}

impl Default for CommitClock {
    fn default() -> Self {
        Self::new()
    }
}

impl CommitClock {
    /// A clock over the system clock with the floor at 0 (nothing handed out yet).
    pub fn new() -> Self {
        Self::with_source(crate::graph::undo::now_micros)
    }

    /// A clock reading `source` instead of the system clock.
    pub(crate) fn with_source(source: fn() -> i64) -> Self {
        CommitClock(Arc::new(ClockState {
            floor: AtomicI64::new(0),
            source,
            observer: OnceLock::new(),
        }))
    }

    /// The current time: `max(system clock, floor)`, and the floor is raised to it. Lock-free
    /// (one atomic `fetch_max`), so it may be called while a graph's write lock is held. A
    /// system clock more than one second behind the floor is reported (a `tracing` warning, or
    /// the installed observer), never an error.
    pub fn now(&self) -> i64 {
        let system = (self.0.source)();
        let floor = self.0.floor.fetch_max(system, Ordering::AcqRel);
        if system >= floor {
            return system;
        }
        if system < floor.saturating_sub(STEP_BACK_THRESHOLD_US) {
            self.stepped_back(system, floor);
        }
        floor
    }

    /// Raises the floor to `at` (microseconds since the Unix epoch); never lowers it.
    pub fn raise(&self, at: i64) {
        self.0.floor.fetch_max(at, Ordering::AcqRel);
    }

    /// The floor: the latest time handed out or raised to (0: none yet). Every later
    /// [`Self::now`] is at least this.
    pub fn floor(&self) -> i64 {
        self.0.floor.load(Ordering::Acquire)
    }

    /// Installs the observer told about a step back of the system clock (the system journal's);
    /// `false` when one is installed already (the first one stays).
    #[cfg_attr(not(test), allow(dead_code))] // installed by the system journal
    pub(crate) fn set_observer(&self, observer: Box<dyn ClockObserver>) -> bool {
        self.0.observer.set(observer).is_ok()
    }

    /// Whether `self` and `other` are clones of one clock.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn same_as(&self, other: &CommitClock) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    #[cold]
    fn stepped_back(&self, system: i64, floor: i64) {
        match self.0.observer.get() {
            Some(observer) => observer.stepped_back(system, floor),
            None => tracing::warn!(
                system_us = system,
                floor_us = floor,
                behind_ms = (floor - system) / 1000,
                "the system clock stepped back behind the instance clock: times stay at the \
                 floor until the system clock catches up"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::AtomicI64;

    /// The fake system clock of the tests in this module (each test uses its own range of
    /// readings, so tests running in parallel do not disturb one another's assertions about
    /// monotonicity; values are set under `FAKE_LOCK`).
    static FAKE_NOW: AtomicI64 = AtomicI64::new(0);
    static FAKE_LOCK: Mutex<()> = Mutex::new(());

    fn fake_now() -> i64 {
        FAKE_NOW.load(Ordering::SeqCst)
    }

    struct Recorder(Arc<Mutex<Vec<(i64, i64)>>>);

    impl ClockObserver for Recorder {
        fn stepped_back(&self, system: i64, floor: i64) {
            self.0.lock().unwrap().push((system, floor));
        }
    }

    #[test]
    fn readings_never_go_back_when_the_system_clock_does() {
        let _guard = FAKE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let clock = CommitClock::with_source(fake_now);
        let seen = Arc::new(Mutex::new(Vec::new()));
        assert!(clock.set_observer(Box::new(Recorder(seen.clone()))));
        assert!(!clock.set_observer(Box::new(Recorder(seen.clone()))));

        FAKE_NOW.store(10_000_000, Ordering::SeqCst);
        assert_eq!(clock.now(), 10_000_000);
        // A small step back (below the threshold): the floor, no report.
        FAKE_NOW.store(9_500_000, Ordering::SeqCst);
        assert_eq!(clock.now(), 10_000_000);
        assert!(seen.lock().unwrap().is_empty());
        // A step back by more than a second: the floor, reported.
        FAKE_NOW.store(8_000_000, Ordering::SeqCst);
        assert_eq!(clock.now(), 10_000_000);
        assert_eq!(*seen.lock().unwrap(), [(8_000_000, 10_000_000)]);
        // The system clock catches up and passes the floor.
        FAKE_NOW.store(10_000_001, Ordering::SeqCst);
        assert_eq!(clock.now(), 10_000_001);
        assert_eq!(clock.floor(), 10_000_001);
    }

    #[test]
    fn raise_never_lowers_and_now_follows_it() {
        let _guard = FAKE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        FAKE_NOW.store(5_000_000, Ordering::SeqCst);
        let clock = CommitClock::with_source(fake_now);
        assert_eq!(clock.floor(), 0);
        clock.raise(7_000_000);
        clock.raise(6_000_000);
        assert_eq!(clock.floor(), 7_000_000);
        assert_eq!(clock.now(), 7_000_000);
        // Clones share the floor.
        let other = clock.clone();
        assert!(other.same_as(&clock));
        other.raise(8_000_000);
        assert_eq!(clock.now(), 8_000_000);
        assert!(!CommitClock::new().same_as(&clock));
    }

    #[test]
    fn the_system_clock_drives_a_new_clock() {
        let clock = CommitClock::new();
        let before = crate::graph::undo::now_micros();
        let read = clock.now();
        assert!(read >= before);
        assert!(clock.floor() >= read);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn concurrent_readings_are_monotonic_per_thread_and_raise_the_floor() {
        let clock = CommitClock::new();
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let clock = clock.clone();
                std::thread::spawn(move || {
                    let mut last = 0;
                    for _ in 0..10_000 {
                        let now = clock.now();
                        assert!(now >= last, "{now} < {last}");
                        last = now;
                    }
                    last
                })
            })
            .collect();
        let max = threads
            .into_iter()
            .map(|t| t.join().unwrap())
            .max()
            .unwrap();
        assert!(clock.floor() >= max);
        assert!(clock.now() >= max);
    }
}
