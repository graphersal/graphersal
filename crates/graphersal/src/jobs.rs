//! Scheduled jobs (book page "Scheduled Jobs"): a [`Scheduler`] the host ticks, jobs with a
//! standard 5-field [`CronSchedule`], system jobs from the engine (a Store's
//! `system_jobs()`), run history in the workload statistics.
//!
//! For hosts: the engine owns no thread; call [`Scheduler::tick`] periodically (every few
//! seconds) with the current time.

pub use crate::graph::jobs::{
    CronError, CronSchedule, Job, JobAction, JobContext, JobError, JobInfo, JobKind, JobLimits,
    Scheduler,
};
