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
//! - **Cancellation per client**: a worker registers its session's cancellation token for the
//!   client of the job it runs; `POST /api/cancel` of a page cancels the tokens of that page's
//!   running jobs only ([`Pool::cancel_client`]).
//! - **Graceful stop** ([`Pool::drain`]): no new job is taken, the running and queued ones get
//!   the drain timeout, then the running ones are cancelled.

use std::collections::{HashMap, VecDeque};
use std::panic::AssertUnwindSafe;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use graphersal::exec::CancelToken;
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
    /// The running jobs: id -> client key and the cancellation token of its worker's session.
    running: HashMap<u64, (String, CancelToken)>,
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
        let cancel = session.cancel_token();
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
                state
                    .running
                    .insert(id, (queued.key.clone(), cancel.clone()));
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

    /// Cancels the running jobs of the client `key` (a page's Stop); whether one was running.
    /// Its queued jobs stay queued.
    pub(super) fn cancel_client(&self, key: &str) -> bool {
        let state = self.lock();
        let mut cancelled = false;
        for (client, cancel) in state.running.values() {
            if client == key {
                cancel.cancel();
                cancelled = true;
            }
        }
        cancelled
    }

    /// The graceful stop: no new job is taken; waits up to `timeout` for the running and
    /// queued jobs, then cancels the running ones and waits a little for them to end. Returns
    /// how many were cancelled.
    pub(super) fn drain(&self, timeout: Duration) -> usize {
        let deadline = Instant::now() + timeout;
        let mut state = self.lock();
        state.draining = true;
        loop {
            if state.running.is_empty() && state.queue.is_empty() {
                return 0;
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
        let cancelled = state.running.len();
        for (_, cancel) in state.running.values() {
            cancel.cancel();
        }
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
        cancelled
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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn pool(workers: usize, queue: usize, per_client: usize, timeout_ms: u64) -> Arc<Pool> {
        let pool = Arc::new(Pool::new(PoolConfig {
            workers,
            queue,
            queue_timeout: Duration::from_millis(timeout_ms),
            max_inflight_per_client: per_client,
        }));
        for _ in 0..workers {
            let pool = pool.clone();
            std::thread::spawn(move || {
                let mut session = Session::new();
                pool.work(&mut session);
            });
        }
        pool
    }

    fn sleeper(ms: u64, ran: &Arc<AtomicUsize>) -> Job {
        let ran = ran.clone();
        Box::new(move |_| {
            std::thread::sleep(Duration::from_millis(ms));
            ran.fetch_add(1, Ordering::SeqCst);
        })
    }

    #[test]
    fn jobs_run_in_parallel_on_the_workers() {
        let pool = pool(4, 16, 4, 5000);
        let ran = Arc::new(AtomicUsize::new(0));
        let started = Instant::now();
        let threads: Vec<_> = (0..4)
            .map(|i| {
                let pool = pool.clone();
                let job = sleeper(300, &ran);
                std::thread::spawn(move || pool.run(&format!("c{i}"), job))
            })
            .collect();
        for thread in threads {
            assert_eq!(thread.join().unwrap(), Ok(()));
        }
        assert_eq!(ran.load(Ordering::SeqCst), 4);
        assert!(
            started.elapsed() < Duration::from_millis(1100),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_full_queue_and_a_busy_client_are_refused() {
        let pool = pool(1, 1, 2, 300);
        let ran = Arc::new(AtomicUsize::new(0));
        let busy = {
            let pool = pool.clone();
            let job = sleeper(800, &ran);
            std::thread::spawn(move || pool.run("a", job))
        };
        std::thread::sleep(Duration::from_millis(100));
        let queued = {
            let pool = pool.clone();
            let job = sleeper(10, &ran);
            std::thread::spawn(move || pool.run("b", job))
        };
        std::thread::sleep(Duration::from_millis(100));
        // The worker runs "a", "b" waits: the queue of one is full.
        assert_eq!(pool.run("c", sleeper(1, &ran)), Err(Refused::QueueFull));
        assert_eq!(busy.join().unwrap(), Ok(()));
        // "b" waited 700 ms > 300 ms for the worker: refused as expired.
        assert_eq!(queued.join().unwrap(), Err(Refused::Expired));
        assert_eq!(pool.status()["refused"]["queueFull"], 1);
    }

    #[test]
    fn a_client_over_its_limit_waits_then_is_refused() {
        let pool = pool(2, 8, 1, 200);
        let ran = Arc::new(AtomicUsize::new(0));
        let first = {
            let pool = pool.clone();
            let job = sleeper(600, &ran);
            std::thread::spawn(move || pool.run("a", job))
        };
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(pool.run("a", sleeper(1, &ran)), Err(Refused::ClientBusy));
        // Another client is not held up by "a".
        assert_eq!(pool.run("b", sleeper(1, &ran)), Ok(()));
        assert_eq!(first.join().unwrap(), Ok(()));
    }

    #[test]
    fn cancel_reaches_only_the_clients_own_job() {
        let pool = pool(2, 8, 4, 5000);
        let run = |key: &'static str| {
            let pool = pool.clone();
            std::thread::spawn(move || {
                let (reply, answer) = channel();
                let job: Job = Box::new(move |session| {
                    let text = session.execute(
                        "g.inject(1).repeat(__.identity()).times(100000000).count().next()",
                        &graphersal_session::RunOptions::default(),
                    );
                    let _ = reply.send(text);
                });
                pool.run(key, job).unwrap();
                answer.recv().unwrap()
            })
        };
        let a = run("a");
        let b = run("b");
        std::thread::sleep(Duration::from_millis(300));
        assert!(pool.cancel_client("a"));
        let a = a.join().unwrap();
        assert!(a.contains("ancel"), "{a}");
        std::thread::sleep(Duration::from_millis(200));
        assert!(pool.cancel_client("b"));
        let b = b.join().unwrap();
        assert!(b.contains("ancel"), "{b}");
    }

    #[test]
    fn drain_lets_running_jobs_finish_and_refuses_new_ones() {
        let pool = pool(1, 4, 4, 5000);
        let ran = Arc::new(AtomicUsize::new(0));
        let running = {
            let pool = pool.clone();
            let job = sleeper(300, &ran);
            std::thread::spawn(move || pool.run("a", job))
        };
        std::thread::sleep(Duration::from_millis(50));
        let drained = {
            let pool = pool.clone();
            std::thread::spawn(move || pool.drain(Duration::from_secs(5)))
        };
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(pool.run("b", sleeper(1, &ran)), Err(Refused::Stopping));
        assert_eq!(drained.join().unwrap(), 0);
        assert_eq!(running.join().unwrap(), Ok(()));
        assert_eq!(ran.load(Ordering::SeqCst), 1);
    }
}
