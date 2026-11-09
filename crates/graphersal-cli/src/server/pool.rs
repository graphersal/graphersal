//! The reader pool of the read-only serving mode (`--server --read-only`): N worker threads,
//! each owning its own [`Session`] over the SAME immutable graph
//! ([`graphersal_session::Session::template`]), take the session jobs of every playground page
//! and MCP client from one bounded queue, so queries run in parallel instead of one after the
//! other on the single session thread of the read-write server.
//!
//! - **Bounded queue** (`--queue`): a job that finds it full is refused at once; a job that
//!   waited longer than `--queue-timeout` for a worker is refused when a worker reaches it (or
//!   when a later job finds it). Both answer 503 with `Retry-After: 1`.
//! - **Fairness** (`--max-inflight-per-client`): one client (an MCP session id, a page's
//!   `X-Graphersal-Client`, an AI Chat conversation) has at most this many jobs running or
//!   queued; a further job waits for a slot of ITS client, up to `--queue-timeout`, then 503.
//! - **Cancellation** is not the pool's: every run a client may cancel has a token of its own,
//!   registered before its job is queued ([`super::runs`]), so a cancel reaches it in the queue
//!   or on a worker alike.
//! - **Graceful stop** ([`Pool::drain`]): no new job is taken, the running and queued ones get
//!   the drain timeout, then the queued ones are refused and the runs cancelled.

use std::collections::{HashMap, VecDeque};
use std::panic::AssertUnwindSafe;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use graphersal_session::Session;
use serde_json::{Value, json};

use super::Job;

/// The settings of a pool.
#[derive(Clone, Debug)]
pub(super) struct PoolConfig {
    /// Worker threads.
    pub(super) workers: usize,
    /// Jobs that may wait for a worker.
    pub(super) queue: usize,
    /// How long a job may wait for a worker or for a slot of its client.
    pub(super) queue_timeout: Duration,
    /// Running and queued jobs of one client.
    pub(super) max_inflight_per_client: usize,
}

/// Why the pool did not run a job. Every one is a 503 with `Retry-After: 1` for the client.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Refused {
    /// The queue is full.
    QueueFull,
    /// The job waited longer than the queue timeout for a worker.
    Expired,
    /// The client's own jobs filled its slots for longer than the queue timeout.
    ClientBusy,
    /// The server is stopping (draining).
    Stopping,
}

impl Refused {
    /// The message of the 503 answer.
    pub(super) fn message(self, config: &PoolConfig) -> String {
        match self {
            Self::QueueFull => format!(
                "the server is busy: {} queries are waiting for its {} workers; retry in a \
                 moment",
                config.queue, config.workers
            ),
            Self::Expired => format!(
                "the server is busy: the query waited {} s for a worker; retry in a moment",
                config.queue_timeout.as_secs()
            ),
            Self::ClientBusy => format!(
                "this client already has {} queries running or waiting (the most one client \
                 may have); retry when one has finished",
                config.max_inflight_per_client
            ),
            Self::Stopping => "the server is stopping".to_string(),
        }
    }
}

/// What a graceful stop ([`Pool::drain`]) did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Drained {
    /// Runs cancelled past the drain timeout (0 when everything ended in time).
    pub(super) cancelled: usize,
    /// Jobs still running after the grace that follows the cancel (a job that does not check
    /// its token: a bug): the process exits under them.
    pub(super) still_running: usize,
}

/// A job waiting for a worker.
struct Queued {
    key: String,
    job: Job,
    since: Instant,
    done: Sender<Result<(), Refused>>,
}

/// What the pool's lock guards.
#[derive(Default)]
struct State {
    queue: VecDeque<Queued>,
    /// Running and queued jobs per client key.
    clients: HashMap<String, usize>,
    /// The running jobs: id -> client key.
    running: HashMap<u64, String>,
    next: u64,
    draining: bool,
    /// Jobs refused since the start, by reason (for `/api/info`).
    refused: [u64; 4],
}

impl State {
    fn release(&mut self, key: &str) {
        if let Some(count) = self.clients.get_mut(key) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.clients.remove(key);
            }
        }
    }

    fn count(&mut self, refused: Refused) {
        let slot = match refused {
            Refused::QueueFull => 0,
            Refused::Expired => 1,
            Refused::ClientBusy => 2,
            Refused::Stopping => 3,
        };
        self.refused[slot] += 1;
    }

    /// Refuses the queued jobs that waited longer than `timeout`.
    fn expire(&mut self, timeout: Duration) {
        let now = Instant::now();
        let mut kept = VecDeque::with_capacity(self.queue.len());
        while let Some(queued) = self.queue.pop_front() {
            if now.duration_since(queued.since) > timeout {
                self.release(&queued.key);
                self.count(Refused::Expired);
                let _ = queued.done.send(Err(Refused::Expired));
            } else {
                kept.push_back(queued);
            }
        }
        self.queue = kept;
    }
}

/// The reader pool.
pub(super) struct Pool {
    config: PoolConfig,
    state: Mutex<State>,
    /// Signalled when a job is queued or the pool drains (the workers wait on it).
    work: Condvar,
    /// Signalled when a job ends (clients waiting for a slot, the drain waiting for idle).
    ended: Condvar,
}

impl Pool {
    pub(super) fn new(config: PoolConfig) -> Self {
        Self {
            config,
            state: Mutex::new(State::default()),
            work: Condvar::new(),
            ended: Condvar::new(),
        }
    }

    pub(super) fn config(&self) -> &PoolConfig {
        &self.config
    }

    /// The pool's state, also after a panic of a holder (the state stays consistent: every
    /// change is a few plain assignments).
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Runs `job` for the client `key` on a worker and returns once it ran, or why it did not.
    pub(super) fn run(&self, key: &str, job: Job) -> Result<(), Refused> {
        let (done, finished) = channel();
        {
            let mut state = self.lock();
            let deadline = Instant::now() + self.config.queue_timeout;
            loop {
                if state.draining {
                    state.count(Refused::Stopping);
                    return Err(Refused::Stopping);
                }
                if state.clients.get(key).copied().unwrap_or(0)
                    < self.config.max_inflight_per_client
                {
                    break;
                }
                let now = Instant::now();
                if now >= deadline {
                    state.count(Refused::ClientBusy);
                    return Err(Refused::ClientBusy);
                }
                state = self
                    .ended
                    .wait_timeout(state, deadline - now)
                    .map(|(state, _)| state)
                    .unwrap_or_else(|poisoned| poisoned.into_inner().0);
            }
            state.expire(self.config.queue_timeout);
            if state.queue.len() >= self.config.queue {
                state.count(Refused::QueueFull);
                return Err(Refused::QueueFull);
            }
            *state.clients.entry(key.to_string()).or_insert(0) += 1;
            state.queue.push_back(Queued {
                key: key.to_string(),
                job,
                since: Instant::now(),
                done,
            });
        }
        self.work.notify_one();
        finished.recv().unwrap_or(Err(Refused::Stopping))
    }

    /// The loop of one worker thread: takes the queued jobs and runs them on `session` until
    /// the process ends.
    pub(super) fn work(&self, session: &mut Session) {
        loop {
            let (id, queued) = {
                let mut state = self.lock();
                let queued = loop {
                    if let Some(queued) = state.queue.pop_front() {
                        break queued;
                    }
                    state = self
                        .work
                        .wait(state)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                };
                if queued.since.elapsed() > self.config.queue_timeout {
                    state.release(&queued.key);
                    state.count(Refused::Expired);
                    drop(state);
                    self.ended.notify_all();
                    let _ = queued.done.send(Err(Refused::Expired));
                    continue;
                }
                state.next += 1;
                let id = state.next;
                state.running.insert(id, queued.key.clone());
                (id, queued)
            };
            let Queued { key, job, done, .. } = queued;
            // A panic is a bug; the session (its graph rolled back by the unit) serves on.
            if std::panic::catch_unwind(AssertUnwindSafe(|| job(session))).is_err() {
                errln!("warning: a request panicked in a worker session (a bug, please report)");
            }
            {
                let mut state = self.lock();
                state.running.remove(&id);
                state.release(&key);
            }
            self.ended.notify_all();
            let _ = done.send(Ok(()));
        }
    }

    /// The graceful stop: no new job is taken; waits up to `timeout` for the running and
    /// queued jobs, then refuses the queued ones, cancels the runs (`cancel`: the server's
    /// [`super::runs::Runs::cancel_all`], which answers how many) and waits a little for the
    /// running jobs to end.
    pub(super) fn drain(&self, timeout: Duration, cancel: impl FnOnce() -> usize) -> Drained {
        let deadline = Instant::now() + timeout;
        let mut state = self.lock();
        state.draining = true;
        loop {
            if state.running.is_empty() && state.queue.is_empty() {
                return Drained::default();
            }
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            state = self
                .ended
                .wait_timeout(state, deadline - now)
                .map(|(state, _)| state)
                .unwrap_or_else(|poisoned| poisoned.into_inner().0);
        }
        // Past the drain timeout: the waiting jobs are refused, the running ones cancelled.
        while let Some(queued) = state.queue.pop_front() {
            state.release(&queued.key);
            let _ = queued.done.send(Err(Refused::Stopping));
        }
        let cancelled = cancel();
        // The cancelled queries stop at their next check; give their answers a moment.
        let grace = Instant::now() + Duration::from_secs(2);
        while !state.running.is_empty() {
            let now = Instant::now();
            if now >= grace {
                break;
            }
            state = self
                .ended
                .wait_timeout(state, grace - now)
                .map(|(state, _)| state)
                .unwrap_or_else(|poisoned| poisoned.into_inner().0);
        }
        Drained {
            cancelled,
            still_running: state.running.len(),
        }
    }

    /// `{workers, queue, queueTimeoutS, maxInflightPerClient, running, queued, refused: {..}}`
    /// for `GET /api/info`.
    pub(super) fn status(&self) -> Value {
        let state = self.lock();
        json!({
            "workers": self.config.workers,
            "queue": self.config.queue,
            "queueTimeoutS": self.config.queue_timeout.as_secs(),
            "maxInflightPerClient": self.config.max_inflight_per_client,
            "running": state.running.len(),
            "queued": state.queue.len(),
            "refused": {
                "queueFull": state.refused[0],
                "expired": state.refused[1],
                "clientBusy": state.refused[2],
                "stopping": state.refused[3],
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::runs::Runs;
    use graphersal::exec::CancelToken;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A pool whose workers are all ready: `Session::new` takes 40-650 ms in a debug build under
    /// load, and a job submitted before that only waits in the queue.
    fn pool(workers: usize, queue: usize, per_client: usize, timeout_ms: u64) -> Arc<Pool> {
        let pool = Arc::new(Pool::new(PoolConfig {
            workers,
            queue,
            queue_timeout: Duration::from_millis(timeout_ms),
            max_inflight_per_client: per_client,
        }));
        let (ready, up) = channel();
        for _ in 0..workers {
            let pool = pool.clone();
            let ready = ready.clone();
            std::thread::spawn(move || {
                let mut session = Session::new();
                let _ = ready.send(());
                pool.work(&mut session);
            });
        }
        for _ in 0..workers {
            up.recv().unwrap();
        }
        pool
    }

    /// Waits until the pool's state satisfies `reached`: the tests follow the pool, not the clock.
    fn wait_until(pool: &Pool, reached: impl Fn(&State) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(60);
        while !reached(&pool.lock()) {
            assert!(Instant::now() < deadline, "the pool never got there");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// A job that runs until the returned sender is dropped.
    fn held(ran: &Arc<AtomicUsize>) -> (Job, Sender<()>) {
        let ran = ran.clone();
        let (release, released) = channel::<()>();
        let job: Job = Box::new(move |_| {
            let _ = released.recv();
            ran.fetch_add(1, Ordering::SeqCst);
        });
        (job, release)
    }

    fn quick(ran: &Arc<AtomicUsize>) -> Job {
        let ran = ran.clone();
        Box::new(move |_| {
            ran.fetch_add(1, Ordering::SeqCst);
        })
    }

    fn submit(
        pool: &Arc<Pool>,
        key: &'static str,
        job: Job,
    ) -> std::thread::JoinHandle<Result<(), Refused>> {
        let pool = pool.clone();
        std::thread::spawn(move || pool.run(key, job))
    }

    #[test]
    fn jobs_run_in_parallel_on_the_workers() {
        let pool = pool(4, 16, 4, 60_000);
        let ran = Arc::new(AtomicUsize::new(0));
        let mut releases = Vec::new();
        let mut threads = Vec::new();
        for key in ["c0", "c1", "c2", "c3"] {
            let (job, release) = held(&ran);
            releases.push(release);
            threads.push(submit(&pool, key, job));
        }
        // All four run at once, none having ended.
        wait_until(&pool, |state| state.running.len() == 4);
        assert_eq!(ran.load(Ordering::SeqCst), 0);
        drop(releases);
        for thread in threads {
            assert_eq!(thread.join().unwrap(), Ok(()));
        }
        assert_eq!(ran.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn a_full_queue_is_refused() {
        let pool = pool(1, 1, 2, 60_000);
        let ran = Arc::new(AtomicUsize::new(0));
        let (job, release) = held(&ran);
        let busy = submit(&pool, "a", job);
        wait_until(&pool, |state| state.running.len() == 1);
        let queued = submit(&pool, "b", quick(&ran));
        wait_until(&pool, |state| state.queue.len() == 1);
        // The worker runs "a", "b" waits: the queue of one is full.
        assert_eq!(pool.run("c", quick(&ran)), Err(Refused::QueueFull));
        assert_eq!(pool.status()["refused"]["queueFull"], 1);
        drop(release);
        assert_eq!(busy.join().unwrap(), Ok(()));
        assert_eq!(queued.join().unwrap(), Ok(()));
        assert_eq!(ran.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_job_that_waited_too_long_is_refused_when_a_worker_reaches_it() {
        let pool = pool(1, 1, 2, 50);
        let ran = Arc::new(AtomicUsize::new(0));
        let (job, release) = held(&ran);
        let busy = submit(&pool, "a", job);
        wait_until(&pool, |state| state.running.len() == 1);
        let queued = submit(&pool, "b", quick(&ran));
        wait_until(&pool, |state| state.queue.len() == 1);
        std::thread::sleep(Duration::from_millis(100));
        drop(release);
        assert_eq!(busy.join().unwrap(), Ok(()));
        assert_eq!(queued.join().unwrap(), Err(Refused::Expired));
        assert_eq!(pool.status()["refused"]["expired"], 1);
        assert_eq!(ran.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_job_that_waited_too_long_gives_its_place_to_a_later_job() {
        let pool = pool(1, 1, 2, 50);
        let ran = Arc::new(AtomicUsize::new(0));
        let (job, release) = held(&ran);
        let busy = submit(&pool, "a", job);
        wait_until(&pool, |state| state.running.len() == 1);
        let expired = submit(&pool, "b", quick(&ran));
        wait_until(&pool, |state| state.queue.len() == 1);
        std::thread::sleep(Duration::from_millis(100));
        // "b" is purged, so "c" finds room instead of a full queue.
        let later = submit(&pool, "c", quick(&ran));
        assert_eq!(expired.join().unwrap(), Err(Refused::Expired));
        drop(release);
        assert_eq!(busy.join().unwrap(), Ok(()));
        // "c" itself may expire or run; it was not refused as full.
        assert_ne!(later.join().unwrap(), Err(Refused::QueueFull));
    }

    #[test]
    fn a_client_over_its_limit_waits_then_is_refused() {
        let pool = pool(2, 8, 1, 1000);
        let ran = Arc::new(AtomicUsize::new(0));
        let (job, release) = held(&ran);
        let first = submit(&pool, "a", job);
        wait_until(&pool, |state| state.running.len() == 1);
        assert_eq!(pool.run("a", quick(&ran)), Err(Refused::ClientBusy));
        // Another client is not held up by "a".
        assert_eq!(pool.run("b", quick(&ran)), Ok(()));
        drop(release);
        assert_eq!(first.join().unwrap(), Ok(()));
    }

    /// A query job with a run token of its own, registered in `runs` for `key` before the job
    /// is queued (as the server does); its answer comes through the receiver. The job waits for
    /// the returned sender to be dropped before its query starts.
    fn registered_query(
        runs: &Arc<Runs>,
        key: &'static str,
        script: &'static str,
    ) -> (Job, Sender<()>, std::sync::mpsc::Receiver<String>, u64) {
        let (reply, answer) = channel();
        let (release, released) = channel::<()>();
        let cancel = CancelToken::new();
        let run = runs.start(key, None, cancel.clone()).unwrap();
        let job: Job = Box::new(move |session| {
            let _ = released.recv();
            let text = session.execute_with_cancel(
                script,
                &graphersal_session::RunOptions::default(),
                graphersal_session::playground_authorizer(),
                &cancel,
            );
            let _ = reply.send(text);
        });
        (job, release, answer, run)
    }

    const ENDLESS: &str = "g.inject(1).repeat(__.identity()).times(100000000).count().next()";
    const SHORT: &str = "let s = 0; for i in 0..200000 { s += i; } s";

    #[test]
    fn cancel_reaches_only_the_clients_own_job() {
        let pool = pool(2, 8, 4, 60_000);
        let runs = Arc::new(Runs::default());
        let run = |key: &'static str| {
            let (job, release, answer, run) = registered_query(&runs, key, ENDLESS);
            drop(release);
            let thread = submit(&pool, key, job);
            (thread, answer, run)
        };
        let (a, a_answer, a_run) = run("a");
        let (b, b_answer, b_run) = run("b");
        wait_until(&pool, |state| state.running.len() == 2);
        assert!(runs.cancel_client("a").unwrap());
        let a_text = a_answer.recv().unwrap();
        a.join().unwrap().unwrap();
        runs.end(a_run).unwrap();
        assert!(a_text.contains("ancel"), "{a_text}");
        // "b" still runs: the cancel of "a" did not reach it.
        assert!(!b.is_finished());
        assert_eq!(pool.status()["running"], 1);
        assert!(runs.cancel_client("b").unwrap());
        let b_text = b_answer.recv().unwrap();
        b.join().unwrap().unwrap();
        runs.end(b_run).unwrap();
        assert!(b_text.contains("ancel"), "{b_text}");
    }

    #[test]
    fn a_cancel_before_the_query_starts_stops_it() {
        let pool = pool(1, 4, 4, 60_000);
        let runs = Arc::new(Runs::default());
        let (job, release, answer, _) = registered_query(&runs, "a", SHORT);
        let thread = submit(&pool, "a", job);
        wait_until(&pool, |state| state.running.len() == 1);
        assert!(runs.cancel_client("a").unwrap());
        drop(release);
        let text = answer.recv().unwrap();
        thread.join().unwrap().unwrap();
        assert!(text.contains("ancel"), "{text}");
    }

    /// The window of issue #30: the job is cancelled while it still waits in the queue behind
    /// another client's job; it stops when it starts, and the other job is not touched.
    #[test]
    fn a_cancel_of_a_queued_job_stops_it_and_spares_the_running_one() {
        let pool = pool(1, 4, 4, 60_000);
        let runs = Arc::new(Runs::default());
        let (busy_job, busy_release, busy_answer, _) = registered_query(&runs, "b", SHORT);
        let busy = submit(&pool, "b", busy_job);
        wait_until(&pool, |state| state.running.len() == 1);
        let (job, release, answer, _) = registered_query(&runs, "a", SHORT);
        drop(release);
        let queued = submit(&pool, "a", job);
        wait_until(&pool, |state| state.queue.len() == 1);
        assert!(runs.cancel_client("a").unwrap());
        drop(busy_release);
        let busy_text = busy_answer.recv().unwrap();
        busy.join().unwrap().unwrap();
        assert!(!busy_text.contains("ancel"), "{busy_text}");
        let text = answer.recv().unwrap();
        queued.join().unwrap().unwrap();
        assert!(text.contains("ancel"), "{text}");
    }

    #[test]
    fn a_cancel_of_one_job_does_not_reach_the_workers_next_job() {
        let pool = pool(1, 4, 4, 60_000);
        let runs = Arc::new(Runs::default());
        let (job, release, answer, run) = registered_query(&runs, "a", SHORT);
        let thread = submit(&pool, "a", job);
        wait_until(&pool, |state| state.running.len() == 1);
        assert!(runs.cancel_client("a").unwrap());
        drop(release);
        let text = answer.recv().unwrap();
        thread.join().unwrap().unwrap();
        runs.end(run).unwrap();
        assert!(text.contains("ancel"), "{text}");

        let (job, release, answer, _) = registered_query(&runs, "a", SHORT);
        drop(release);
        let thread = submit(&pool, "a", job);
        let text = answer.recv().unwrap();
        thread.join().unwrap().unwrap();
        assert!(!text.contains("ancel"), "{text}");
    }

    #[test]
    fn a_drain_past_its_timeout_stops_a_query_that_has_not_started() {
        let pool = pool(1, 4, 4, 60_000);
        let runs = Arc::new(Runs::default());
        let (job, release, answer, _) = registered_query(&runs, "a", SHORT);
        let thread = submit(&pool, "a", job);
        wait_until(&pool, |state| state.running.len() == 1);
        let drained = {
            let pool = pool.clone();
            let runs = runs.clone();
            std::thread::spawn(move || {
                pool.drain(Duration::ZERO, || runs.cancel_all().unwrap_or(0))
            })
        };
        // With no timeout to wait for, the drain cancels in the same lock hold that sets the flag.
        wait_until(&pool, |state| state.draining);
        drop(release);
        let text = answer.recv().unwrap();
        thread.join().unwrap().unwrap();
        assert_eq!(
            drained.join().unwrap(),
            Drained {
                cancelled: 1,
                still_running: 0
            }
        );
        assert!(text.contains("ancel"), "{text}");
    }

    #[test]
    fn drain_lets_running_jobs_finish_and_refuses_new_ones() {
        let pool = pool(1, 4, 4, 60_000);
        let ran = Arc::new(AtomicUsize::new(0));
        let (job, release) = held(&ran);
        let running = submit(&pool, "a", job);
        wait_until(&pool, |state| state.running.len() == 1);
        let drained = {
            let pool = pool.clone();
            std::thread::spawn(move || pool.drain(Duration::from_secs(60), || 0))
        };
        wait_until(&pool, |state| state.draining);
        assert_eq!(pool.run("b", quick(&ran)), Err(Refused::Stopping));
        drop(release);
        assert_eq!(drained.join().unwrap(), Drained::default());
        assert_eq!(running.join().unwrap(), Ok(()));
        assert_eq!(ran.load(Ordering::SeqCst), 1);
    }
}
