//! The threads that do the work of the scheduled jobs (issue #26), so a running job never holds up
//! the session thread that answers every request: the session hands a [`JobRun`] out in a short
//! call, a worker runs it, a second short call gives the result back.
//!
//! One worker per job group, started at the group's first run and idle in between: the scheduler
//! never hands out two runs of a group at once, so the groups are the jobs that may run at the
//! same time, and they are few (the system jobs, `query`, `checkpoint`, `backup`). A worker lives
//! on so what a script run builds per thread (the DSL's registrations) is built once.

use std::collections::HashMap;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};

use graphersal::jobs::JobRun;

use crate::context::{CLIENT, Client, Context, SESSION_STACK_BYTES, as_client};

/// One run for a worker.
struct Work {
    /// The commit position when the run was handed out ([`Context::finish_job_run`]).
    before: Option<(usize, u64)>,
    run: JobRun,
    /// The client that asked for it (the change feed's name for its commits).
    client: Client,
    /// Told when the result is back in the session (a caller waiting for the run).
    done: Option<Sender<()>>,
}

/// The workers by job group.
#[derive(Default)]
pub(crate) struct JobWorkers {
    lanes: Mutex<HashMap<String, Sender<Work>>>,
}

/// Starts `run` on its group's worker. A run no worker can take is recorded as failed, so its job
/// is free again.
pub(crate) fn start(
    context: &Arc<Context>,
    before: Option<(usize, u64)>,
    run: JobRun,
    done: Option<Sender<()>>,
) {
    let group = run.group().to_string();
    let work = Work {
        before,
        run,
        client: CLIENT.with(|c| c.borrow().clone()),
        done,
    };
    let refused = match context.job_workers.lanes.lock() {
        Ok(mut lanes) => {
            let lane = match lanes.get(&group) {
                Some(lane) => Ok(lane.clone()),
                None => spawn_worker(context, &group).inspect(|lane| {
                    lanes.insert(group.clone(), lane.clone());
                }),
            };
            match lane {
                Ok(lane) => lane
                    .send(work)
                    .err()
                    .map(|back| (back.0, "the job's thread has ended".to_string())),
                Err(err) => Some((work, format!("cannot start the job's thread: {err}"))),
            }
        }
        Err(_) => Some((work, "the job threads are not available".to_string())),
    };
    if let Some((work, reason)) = refused {
        let finished = work.run.abandon(reason);
        as_client(work.client, || {
            context.finish_job_run(work.before, finished)
        });
        if let Some(done) = work.done {
            let _ = done.send(());
        }
    }
}

fn spawn_worker(context: &Arc<Context>, group: &str) -> std::io::Result<Sender<Work>> {
    let (lane, queue) = channel::<Work>();
    let context = context.clone();
    // The stack of the session thread: a query job evaluates a script, as it does there.
    std::thread::Builder::new()
        .name(format!("graphersal-job-{group}"))
        .stack_size(SESSION_STACK_BYTES)
        .spawn(move || {
            graphersal::workload::set_thread_default(
                graphersal::workload::Channel::Playground,
                true,
            );
            while let Ok(work) = queue.recv() {
                let finished = work.run.run();
                as_client(work.client, || {
                    context.finish_job_run(work.before, finished)
                });
                if let Some(done) = work.done {
                    let _ = done.send(());
                }
            }
        })?;
    Ok(lane)
}
