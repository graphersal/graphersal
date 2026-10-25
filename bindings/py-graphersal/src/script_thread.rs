//! Script evaluation on a thread with a large native stack.
//!
//! The scripting engine (Rhai) copies nested values recursively: a few thousand nested maps
//! overflow an 8 MiB stack before any Graphersal code runs, and a stack overflow aborts the whole
//! Python process. Every script therefore runs on a worker thread with a 64 MiB stack
//! ([`SCRIPT_STACK_BYTES`], virtual memory only: pages are committed as the stack is used).
//!
//! Each calling (Python) thread gets its own long-lived worker, so scripts of different Python
//! threads still run in parallel, and the engine's per-thread registrations are built once per
//! worker instead of once per call. The worker ends when its calling thread ends.

use std::cell::RefCell;
use std::sync::mpsc;

/// Stack size of a script worker thread.
pub(crate) const SCRIPT_STACK_BYTES: usize = 64 * 1024 * 1024;

type Job = Box<dyn FnOnce() + Send + 'static>;

/// The worker of one calling thread: the sending end of its job queue.
struct Worker {
    jobs: mpsc::Sender<Job>,
}

impl Worker {
    /// Starts a worker thread with a [`SCRIPT_STACK_BYTES`] stack; `None` when the operating
    /// system refuses the thread.
    fn start() -> Option<Self> {
        let (jobs, queue) = mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("graphersal-script".to_string())
            .stack_size(SCRIPT_STACK_BYTES)
            .spawn(move || {
                // Ends when the calling thread drops its `Worker` (the queue disconnects).
                while let Ok(job) = queue.recv() {
                    job();
                }
            })
            .ok()?;
        Some(Self { jobs })
    }
}

thread_local! {
    /// The worker of this calling thread, started on its first script.
    static WORKER: RefCell<Option<Worker>> = const { RefCell::new(None) };
}

/// Runs `job` on the calling thread's script worker and returns its result; a panic in `job`
/// continues on the calling thread. Runs `job` directly when no worker thread can be started.
pub(crate) fn on_script_thread<T: Send + 'static>(job: impl FnOnce() -> T + Send + 'static) -> T {
    let (done, result) = mpsc::channel();
    // Everything Python runs counts under the `python` channel of the workload statistics.
    let job =
        move || graphersal::workload::scope(graphersal::workload::Channel::Python, false, job);
    let mut job = Some(job);
    let sent = WORKER.with(|worker| {
        let mut worker = worker.borrow_mut();
        if worker.is_none() {
            *worker = Worker::start();
        }
        let Some(worker) = worker.as_ref() else {
            return false;
        };
        let Some(job) = job.take() else {
            return false;
        };
        let done = done.clone();
        worker
            .jobs
            .send(Box::new(move || {
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
                let _ = done.send(outcome);
            }))
            .is_ok()
    });
    if sent && let Ok(outcome) = result.recv() {
        return match outcome {
            Ok(value) => value,
            Err(panic) => std::panic::resume_unwind(panic),
        };
    }
    // No worker (or it died): forget it and run here, on the caller's stack.
    WORKER.with(|worker| *worker.borrow_mut() = None);
    match job {
        Some(job) => job(),
        // The job was handed to a worker that died before answering: the job is lost.
        None => panic!("the script worker thread ended before it answered"),
    }
}
