//! Scheduled jobs (book page "Scheduled Jobs"): a [`Scheduler`] holds jobs with a cron schedule
//! and runs the due ones when its host calls [`Scheduler::tick`]. The engine owns no thread: the
//! host drives the clock (the dev server and the CLI with a thread, the playground with a timer
//! in its worker, an embedding host by calling `tick` itself).
//!
//! - A job never overlaps itself: `tick` runs due jobs one after another, synchronously.
//! - A missed time is skipped: after a pause (the process was stopped, the host ticked late) a due
//!   job runs once and its next time is computed from now.
//! - Every run is recorded in the [`WorkloadStats`] the scheduler was given (the job block).
//!
//! System jobs come from the engine (a Store's `system_jobs()`: flushing the workload
//! statistics, compacting); user jobs (saved queries, Store operations on a schedule) follow as
//! catalog definitions.
//!

mod cron;

use std::sync::Arc;

pub use cron::{CronError, CronSchedule};

use crate::graph::workload::{JobRunRecord, WorkloadStats};

/// Who a job belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum JobKind {
    /// Built into the engine: shown, its schedule and switch changeable, never removed.
    System,
    /// Defined by a user.
    User,
}

impl JobKind {
    /// `system` or `user`.
    pub fn name(self) -> &'static str {
        match self {
            JobKind::System => "system",
            JobKind::User => "user",
        }
    }
}

/// The action of a job: called with the time the run starts (microseconds since the Unix
/// epoch); an `Err` is the run's error text.
pub type JobAction = Box<dyn FnMut(i64) -> Result<(), String> + Send>;

/// A job: its name, kind, schedule, switch and action.
pub struct Job {
    name: String,
    kind: JobKind,
    schedule: CronSchedule,
    enabled: bool,
    action: JobAction,
}

impl std::fmt::Debug for Job {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Job")
            .field("name", &self.name)
            .field("kind", &self.kind)
            .field("schedule", &self.schedule)
            .field("enabled", &self.enabled)
            .finish_non_exhaustive()
    }
}

impl Job {
    /// A job `name` of `kind` running `action` on `schedule`, enabled.
    pub fn new(
        name: impl Into<String>,
        kind: JobKind,
        schedule: CronSchedule,
        action: impl FnMut(i64) -> Result<(), String> + Send + 'static,
    ) -> Self {
        Job {
            name: name.into(),
            kind,
            schedule,
            enabled: true,
            action: Box::new(action),
        }
    }

    /// The same job, switched off (it never runs until [`Scheduler::set_enabled`]).
    pub fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }

    /// The job's name.
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// What a scheduler tells about one of its jobs.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct JobInfo {
    /// The name.
    pub name: String,
    /// System or user.
    pub kind: JobKind,
    /// The schedule (cron text).
    pub schedule: String,
    /// Whether it runs.
    pub enabled: bool,
    /// When it runs next (microseconds since the Unix epoch), `None` when off or never.
    pub next_at: Option<i64>,
    /// Its last run in this process.
    pub last: Option<JobRunRecord>,
}

/// Why a scheduler refused a change.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum JobError {
    /// A job of that name exists already.
    Duplicate(String),
    /// No job of that name.
    Unknown(String),
    /// The time zone name is not an IANA zone.
    TimeZone(String),
}

impl std::fmt::Display for JobError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JobError::Duplicate(name) => write!(f, "a job named '{name}' exists already"),
            JobError::Unknown(name) => write!(f, "no job named '{name}'"),
            JobError::TimeZone(name) => write!(
                f,
                "'{name}' is not an IANA time zone (use a name such as 'Europe/Berlin' or 'UTC')"
            ),
        }
    }
}

impl std::error::Error for JobError {}

struct Entry {
    job: Job,
    next_at: Option<i64>,
    last: Option<JobRunRecord>,
}

/// Holds jobs and runs the due ones on [`Self::tick`] (module docs).
///
/// ```
/// use std::sync::{Arc, atomic::{AtomicU32, Ordering}};
/// use graphersal::jobs::{Job, JobKind, Scheduler};
///
/// let runs = Arc::new(AtomicU32::new(0));
/// let counter = runs.clone();
/// let mut scheduler = Scheduler::new();
/// scheduler
///     .add(Job::new("count", JobKind::User, "* * * * *".parse().unwrap(), move |_| {
///         counter.fetch_add(1, Ordering::Relaxed);
///         Ok(())
///     }))
///     .unwrap();
/// let t0 = 1_800_000_000_000_000; // microseconds since the Unix epoch
/// scheduler.tick(t0); // plans the first run: the next whole minute
/// scheduler.tick(t0 + 60_000_000);
/// assert_eq!(runs.load(Ordering::Relaxed), 1);
/// ```
pub struct Scheduler {
    zone: jiff::tz::TimeZone,
    zone_name: String,
    entries: Vec<Entry>,
    stats: Option<Arc<WorkloadStats>>,
}

impl std::fmt::Debug for Scheduler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Scheduler")
            .field("zone", &self.zone_name)
            .field("jobs", &self.entries.len())
            .finish_non_exhaustive()
    }
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::new()
    }
}

impl Scheduler {
    /// An empty scheduler in UTC without statistics.
    pub fn new() -> Self {
        Scheduler {
            zone: jiff::tz::TimeZone::UTC,
            zone_name: "UTC".to_string(),
            entries: Vec::new(),
            stats: None,
        }
    }

    /// The same scheduler evaluating schedules in the IANA time zone `name` (the host passes the
    /// instance's local zone: `TZ` or `/etc/localtime` natively, the browser's zone in the
    /// playground).
    pub fn with_time_zone(mut self, name: &str) -> Result<Self, JobError> {
        self.zone =
            jiff::tz::TimeZone::get(name).map_err(|_| JobError::TimeZone(name.to_string()))?;
        self.zone_name = name.to_string();
        Ok(self)
    }

    /// The same scheduler recording every run into `stats`.
    pub fn with_stats(mut self, stats: Arc<WorkloadStats>) -> Self {
        self.stats = Some(stats);
        self
    }

    /// The time zone name schedules are evaluated in.
    pub fn time_zone(&self) -> &str {
        &self.zone_name
    }

    /// Adds `job`; its first time is planned by the next [`Self::tick`].
    pub fn add(&mut self, job: Job) -> Result<(), JobError> {
        if self.entries.iter().any(|e| e.job.name == job.name) {
            return Err(JobError::Duplicate(job.name));
        }
        self.entries.push(Entry {
            job,
            next_at: None,
            last: None,
        });
        Ok(())
    }

    /// Removes the user job `name` (a system job cannot be removed: switch it off).
    pub fn remove(&mut self, name: &str) -> Result<(), JobError> {
        let index = self
            .entries
            .iter()
            .position(|e| e.job.name == name && e.job.kind == JobKind::User)
            .ok_or_else(|| JobError::Unknown(name.to_string()))?;
        self.entries.remove(index);
        Ok(())
    }

    /// Switches job `name` on or off.
    pub fn set_enabled(&mut self, name: &str, enabled: bool) -> Result<(), JobError> {
        let entry = self.entry(name)?;
        entry.job.enabled = enabled;
        entry.next_at = None;
        Ok(())
    }

    /// Gives job `name` a new schedule (planned from the next tick).
    pub fn set_schedule(&mut self, name: &str, schedule: CronSchedule) -> Result<(), JobError> {
        let entry = self.entry(name)?;
        entry.job.schedule = schedule;
        entry.next_at = None;
        Ok(())
    }

    fn entry(&mut self, name: &str) -> Result<&mut Entry, JobError> {
        self.entries
            .iter_mut()
            .find(|e| e.job.name == name)
            .ok_or_else(|| JobError::Unknown(name.to_string()))
    }

    /// Runs every enabled job whose time has come at `now` (microseconds since the Unix epoch),
    /// one after another, and plans each one's next time; a job not planned yet (just added,
    /// re-scheduled or switched on) is planned from `now` without running. Returns the names of
    /// the jobs that ran.
    pub fn tick(&mut self, now: i64) -> Vec<String> {
        let mut ran = Vec::new();
        for index in 0..self.entries.len() {
            let Some(entry) = self.entries.get_mut(index) else {
                continue;
            };
            if !entry.job.enabled {
                continue;
            }
            match entry.next_at {
                None => {
                    entry.next_at = entry.job.schedule.next_after(now, &self.zone);
                }
                Some(at) if at <= now => {
                    let name = entry.job.name.clone();
                    self.run_entry(index, now);
                    ran.push(name);
                }
                Some(_) => {}
            }
        }
        ran
    }

    /// Runs job `name` now, whatever its schedule (its next time stays planned).
    pub fn run_now(&mut self, name: &str, now: i64) -> Result<(), JobError> {
        let index = self
            .entries
            .iter()
            .position(|e| e.job.name == name)
            .ok_or_else(|| JobError::Unknown(name.to_string()))?;
        let next = self.entries.get(index).and_then(|e| e.next_at);
        self.run_entry(index, now);
        if let Some(entry) = self.entries.get_mut(index)
            && next.is_some_and(|at| at > now)
        {
            entry.next_at = next;
        }
        Ok(())
    }

    /// Runs entry `index`, records the run, plans the next time from the end of the run (times
    /// missed meanwhile are skipped).
    fn run_entry(&mut self, index: usize, now: i64) {
        let Some(entry) = self.entries.get_mut(index) else {
            return;
        };
        let started = web_time::Instant::now();
        // A panicking action is a failed run, never a failed host.
        let action = &mut entry.job.action;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| action(now)))
            .unwrap_or_else(|_| Err("the job panicked".to_string()));
        let micros = started.elapsed().as_micros() as u64;
        let run = JobRunRecord {
            started_at: now,
            micros,
            error: result.err(),
        };
        if let Some(error) = &run.error {
            tracing::warn!(job = %entry.job.name, %error, "scheduled job failed");
        }
        let end = now.saturating_add(micros as i64);
        entry.next_at = entry.job.schedule.next_after(end, &self.zone);
        if let Some(stats) = &self.stats {
            stats.record_job_run(&entry.job.name, run.clone());
        }
        entry.last = Some(run);
    }

    /// Every job, in the order they were added.
    pub fn jobs(&self) -> Vec<JobInfo> {
        self.entries
            .iter()
            .map(|e| JobInfo {
                name: e.job.name.clone(),
                kind: e.job.kind,
                schedule: e.job.schedule.to_string(),
                enabled: e.job.enabled,
                next_at: e.next_at.filter(|_| e.job.enabled),
                last: e.last.clone(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::graph::workload::WorkloadLevel;

    const MIN: i64 = 60_000_000;
    /// 2027-01-15T08:00:00Z.
    const T0: i64 = 1_800_000_000_000_000 - (1_800_000_000_000_000 % (60 * MIN));

    fn recording(log: &Arc<Mutex<Vec<(String, i64)>>>, name: &str) -> Job {
        let log = log.clone();
        let label = name.to_string();
        Job::new(
            name,
            JobKind::User,
            "*/5 * * * *".parse().unwrap(),
            move |at| {
                log.lock().unwrap().push((label.clone(), at));
                Ok(())
            },
        )
    }

    #[test]
    fn due_jobs_run_once_and_missed_times_are_skipped() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let stats = Arc::new(WorkloadStats::new(WorkloadLevel::Memory));
        let mut s = Scheduler::new().with_stats(stats.clone());
        s.add(recording(&log, "a")).unwrap();
        assert_eq!(s.tick(T0), Vec::<String>::new(), "the first tick plans");
        assert_eq!(s.jobs()[0].next_at, Some(T0 + 5 * MIN));
        assert!(s.tick(T0 + 4 * MIN).is_empty());
        assert_eq!(s.tick(T0 + 5 * MIN), ["a"]);
        // A long pause: three times missed, one run, the next time after now.
        assert_eq!(s.tick(T0 + 23 * MIN), ["a"]);
        assert_eq!(s.jobs()[0].next_at, Some(T0 + 25 * MIN));
        assert_eq!(log.lock().unwrap().len(), 2);
        let report = stats.report(T0 + 23 * MIN);
        assert_eq!(report.jobs[0].0, "a");
        assert_eq!(report.series[0].last_hour.count, 2);
    }

    #[test]
    fn failures_and_panics_are_recorded_not_raised() {
        let mut s = Scheduler::new();
        s.add(Job::new(
            "fails",
            JobKind::User,
            "* * * * *".parse().unwrap(),
            |_| Err("no disk".to_string()),
        ))
        .unwrap();
        s.add(Job::new(
            "panics",
            JobKind::User,
            "* * * * *".parse().unwrap(),
            |_| panic!("boom"),
        ))
        .unwrap();
        s.tick(T0);
        assert_eq!(s.tick(T0 + MIN), ["fails", "panics"]);
        let jobs = s.jobs();
        assert_eq!(
            jobs[0].last.as_ref().unwrap().error.as_deref(),
            Some("no disk")
        );
        assert_eq!(
            jobs[1].last.as_ref().unwrap().error.as_deref(),
            Some("the job panicked")
        );
    }

    #[test]
    fn switches_schedules_names_and_zones() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut s = Scheduler::new();
        s.add(recording(&log, "a")).unwrap();
        assert_eq!(
            s.add(recording(&log, "a")),
            Err(JobError::Duplicate("a".into()))
        );
        s.add(
            Job::new("sys", JobKind::System, "0 * * * *".parse().unwrap(), |_| {
                Ok(())
            })
            .disabled(),
        )
        .unwrap();
        s.tick(T0);
        assert_eq!(
            s.jobs()[1].next_at,
            None,
            "a switched-off job is not planned"
        );
        assert_eq!(s.remove("sys"), Err(JobError::Unknown("sys".into())));
        s.set_enabled("a", false).unwrap();
        assert!(s.tick(T0 + 5 * MIN).is_empty());
        s.set_enabled("a", true).unwrap();
        s.set_schedule("a", "0 12 * * *".parse().unwrap()).unwrap();
        s.tick(T0 + 5 * MIN);
        // 12:00 UTC is four hours after T0.
        assert_eq!(s.jobs()[0].next_at, Some(T0 + 4 * 60 * MIN));
        s.run_now("a", T0 + 6 * MIN).unwrap();
        assert_eq!(log.lock().unwrap().len(), 1);
        assert_eq!(
            s.jobs()[0].next_at,
            Some(T0 + 4 * 60 * MIN),
            "run_now keeps the plan"
        );
        s.remove("a").unwrap();
        assert!(Scheduler::new().with_time_zone("Mars/Base").is_err());
        let berlin = Scheduler::new().with_time_zone("Europe/Berlin").unwrap();
        assert_eq!(berlin.time_zone(), "Europe/Berlin");
    }
}
