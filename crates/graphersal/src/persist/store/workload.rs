//! The workload statistics of a Store (book page "Workload Statistics"): read from the sidecar
//! file `WORKLOAD` at open, written back by the system job `stats_flush` and at close.
//!
//! The sidecar is instance data, not data of the graph (format spec "Workload statistics
//! sidecar"): never in the WAL, never in a backup, a fork or a copy; damaged or unknown means
//! the statistics start anew, never store damage. Only a writable store writes it.

use std::sync::Arc;

use super::files::{self, DirPath};
use super::{Shared, Store, StoreCore};
use crate::graph::PersistentStorage;
use crate::graph::jobs::{Job, JobKind};
use crate::graph::workload::{WorkloadLevel, WorkloadStats};
use crate::persist::PersistError;

/// The sidecar's file name at the store root.
pub(crate) const WORKLOAD_FILE: &str = "WORKLOAD";
/// The largest sidecar read (a larger one is ignored: the statistics start anew).
const MAX_SIDECAR_BYTES: u64 = 64 << 20;

/// The statistics a store opened at `dir` keeps: read from its sidecar when `level` is
/// `persisted` and the store is `writable` (else empty, in memory).
pub(crate) fn open_stats(
    dir: &DirPath,
    level: WorkloadLevel,
    writable: bool,
) -> Arc<WorkloadStats> {
    if writable
        && level == WorkloadLevel::Persisted
        && let Ok(bytes) = files::read_bounded(&dir.join(WORKLOAD_FILE), MAX_SIDECAR_BYTES)
    {
        match WorkloadStats::from_sidecar(&bytes, level) {
            Some(stats) => return Arc::new(stats),
            None => tracing::warn!(
                store = %dir.display(),
                "the workload statistics sidecar is not readable; the statistics start anew"
            ),
        }
    }
    let level = if writable { level } else { level.min_memory() };
    Arc::new(WorkloadStats::new(level))
}

impl WorkloadLevel {
    /// `persisted` lowered to `memory` (a store that writes nothing keeps statistics in memory).
    fn min_memory(self) -> WorkloadLevel {
        match self {
            WorkloadLevel::Persisted => WorkloadLevel::Memory,
            other => other,
        }
    }
}

impl Shared {
    /// Writes the sidecar when the statistics are persisted, changed since the last write, and
    /// the store may write. Best effort: a failure is returned to the caller (the job records
    /// it; close only logs it), never store damage.
    pub(crate) fn flush_workload(&self, writable: bool) -> Result<bool, PersistError> {
        if !writable || self.workload.level() != WorkloadLevel::Persisted {
            return Ok(false);
        }
        if !self.workload.take_dirty() {
            return Ok(false);
        }
        let bytes = self.workload.to_sidecar();
        let path = self.dir.join(WORKLOAD_FILE);
        if let Err(err) = path.store_dir().write_atomic(path.name(), &bytes) {
            // Written next time.
            self.workload.mark_dirty();
            return Err(PersistError::io("writing the workload statistics", err));
        }
        Ok(true)
    }
}

impl StoreCore {
    /// Whether this store may write its sidecar (not a backup, not read-only, not in
    /// maintenance mode, not stopped, not closed).
    fn writes_workload(&self) -> bool {
        self.backup.is_none()
            && self.maintenance.is_none()
            && self
                .shared
                .read_only
                .lock()
                .is_ok_and(|reason| reason.is_none())
            && self
                .shared
                .stopped
                .lock()
                .is_ok_and(|reason| reason.is_none())
            && !self.closed.load(std::sync::atomic::Ordering::Acquire)
    }

    pub(crate) fn flush_workload(&self) -> Result<bool, PersistError> {
        self.shared.flush_workload(self.writes_workload())
    }
}

impl<S: PersistentStorage> Store<S> {
    /// The workload statistics of this store (book page "Workload Statistics"), at the level
    /// [`StoreOptions::with_workload_level`](super::StoreOptions::with_workload_level) set at
    /// open; shared with whoever records into them.
    pub fn workload_stats(&self) -> Arc<WorkloadStats> {
        Arc::clone(&self.core.shared.workload)
    }

    /// Writes the workload statistics to the store's sidecar now (also done by the system job
    /// `stats_flush` and at close): `true` when something was written, `false` when there was
    /// nothing new, the level is not `persisted`, or the store writes nothing (a backup,
    /// read-only, maintenance mode).
    pub fn flush_workload_stats(&self) -> Result<bool, PersistError> {
        self.core.flush_workload()
    }

    /// The system jobs of this store, for the host's [`Scheduler`](crate::jobs::Scheduler):
    /// `stats_flush` (every minute: [`Self::flush_workload_stats`]) and `compact` (off by
    /// default, daily at 03:00: releases spare capacity, [`crate::storage::GraphStorage::shrink_to_fit`]
    /// under the write lock), each with the instance's settings applied
    /// ([`WorkloadStats::set_job_override`]: a schedule and a switch its user chose, kept in the
    /// sidecar). The jobs hold the store's core, not the store.
    pub fn system_jobs(&self) -> Vec<Job> {
        let flush = Arc::clone(&self.core);
        let graph = Arc::clone(&self.graph);
        let every_minute = "* * * * *".parse().ok();
        let nightly = "0 3 * * *".parse().ok();
        let mut jobs = Vec::new();
        if let Some(schedule) = every_minute {
            jobs.push(Job::new(
                "stats_flush",
                JobKind::System,
                schedule,
                move |_| {
                    flush
                        .flush_workload()
                        .map(|_| ())
                        .map_err(|err| err.to_string())
                },
            ));
        }
        if let Some(schedule) = nightly {
            jobs.push(
                Job::new("compact", JobKind::System, schedule, move |_| {
                    crate::graph::GraphStorage::shrink_to_fit(&mut *graph.write())
                        .map_err(|err| err.to_string())
                })
                .disabled(),
            );
        }
        // The instance's settings (book page "Scheduled Jobs": a system job's schedule and
        // switch changed by its user).
        let stats = &self.core.shared.workload;
        jobs.into_iter()
            .map(|job| match stats.job_override(job.name()) {
                Some(setting) => job.with_override(&setting),
                None => job,
            })
            .collect()
    }
}

/// What [`Store::user_jobs`] found in the catalog: the jobs to schedule and the job
/// definitions this instance does not schedule, each with the reason (front ends list them;
/// none is dropped silently).
#[derive(Debug)]
#[non_exhaustive]
pub struct UserJobs {
    /// The jobs for the host's [`Scheduler`](crate::jobs::Scheduler), by name.
    pub jobs: Vec<Job>,
    /// The job definitions that are not scheduled, by name.
    pub not_scheduled: Vec<NotScheduledJob>,
}

/// A job definition [`Store::user_jobs`] does not schedule, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct NotScheduledJob {
    /// The job's name.
    pub name: String,
    /// Why it is not scheduled: an action or `run_on` form this version does not know, `run_on`
    /// `replica` on a single instance, a schedule that does not parse (a definition written by a
    /// newer version, or stored without the DSL's checks), or a definition kept opaque.
    pub reason: String,
}

impl<S: PersistentStorage + Default> Store<S> {
    /// The scheduled jobs of users in this store's catalog (kind `job`, book page "Scheduled
    /// Jobs"), for the host's [`Scheduler`](crate::jobs::Scheduler)
    /// ([`sync_user_jobs`](crate::jobs::Scheduler::sync_user_jobs) whenever the catalog's jobs
    /// changed): a `query` job calls its saved query as one unit carrying `{job: <name>}` in its
    /// commit metadata, under an authorizer limited to the job's stored effects (feature
    /// `script`); `checkpoint` writes a checkpoint, `backup` backs the store up into its directory
    /// (not on wasm32). A disabled job is listed switched off. A job this instance does not run
    /// (an unknown action, `run_on` `replica` on a single instance, a schedule that does not
    /// parse, a definition kept opaque) is in [`UserJobs::not_scheduled`] with its reason.
    ///
    /// Fails when the catalog cannot be read: [`PersistError::Graph`] holding
    /// [`GraphError::GraphViewHeld`](crate::error::GraphError::GraphViewHeld) on a thread that
    /// holds a unit of the graph beside another view of it.
    pub fn user_jobs(&self) -> Result<UserJobs, PersistError> {
        let definitions: Vec<crate::graph::definition::Definition> = self
            .graph
            .read_checked()
            .map_err(PersistError::Graph)?
            .definitions()
            .map_err(PersistError::Graph)?
            .filter(|d| d.kind() == crate::graph::definition::DefinitionKind::JOB)
            .cloned()
            .collect();
        let mut found = UserJobs {
            jobs: Vec::new(),
            not_scheduled: Vec::new(),
        };
        for definition in &definitions {
            let built = match definition.as_job() {
                Some(job) => self.user_job(definition.name(), job),
                None => Err("this version could not decode its definition (kept opaque)".into()),
            };
            match built {
                Ok(job) => found.jobs.push(job),
                Err(reason) => found.not_scheduled.push(NotScheduledJob {
                    name: definition.name().to_string(),
                    reason,
                }),
            }
        }
        Ok(found)
    }

    /// The job `name` of `job`, or why this instance does not schedule it.
    fn user_job(&self, name: &str, job: &crate::graph::jobs::JobDefinition) -> Result<Job, String> {
        use crate::graph::jobs::{JobTask, RunOn};
        if let Some(reason) = job.unserved_reason() {
            return Err(reason);
        }
        match job.run_on() {
            RunOn::Replica => {
                return Err(
                    "it runs on a replica (run_on \"replica\"), and this store is a \
                            single instance"
                        .into(),
                );
            }
            RunOn::Unknown(form) => {
                return Err(format!("its run_on {form:?} is not known to this version"));
            }
            _ => {}
        }
        let schedule = job
            .schedule()
            .parse()
            .map_err(|err| format!("its schedule {:?} does not parse: {err}", job.schedule()))?;
        let job_name = name.to_string();
        let action: crate::graph::jobs::JobAction = match job.action() {
            JobTask::Query { name: query, args } => {
                self.query_action(&job_name, query, args, job.effects())
            }
            JobTask::Checkpoint => {
                let core = Arc::clone(&self.core);
                Box::new(move |_| {
                    core.checkpoint(None)
                        .map(|_| ())
                        .map_err(|err| err.to_string())
                })
            }
            JobTask::Backup { path } => {
                // The directory the definer was allowed (resolved under the host's file
                // root when the job was defined), else the path as written.
                let target = job
                    .effects()
                    .iter()
                    .find_map(|effect| match effect.as_request().resource {
                        crate::graph::Resource::File { path } => Some(path.to_path_buf()),
                        _ => None,
                    })
                    .unwrap_or_else(|| std::path::PathBuf::from(path));
                let core = Arc::clone(&self.core);
                Box::new(move |_| backup_to(&core, &target))
            }
            JobTask::Unknown(_) => {
                return Err("its action is not known to this version".into());
            }
        };
        let mut built = Job::new(name, JobKind::User, schedule, action).with_limits(job.limits());
        if !job.enabled() {
            built = built.disabled();
        }
        Ok(built)
    }

    #[cfg(feature = "script")]
    fn query_action(
        &self,
        job: &str,
        query: &str,
        args: &crate::value::PropertyMap,
        effects: &[crate::graph::AccessRequestBuf],
    ) -> crate::graph::jobs::JobAction {
        let graph = crate::graph::IntoAnyGraph::into_any_graph(Arc::clone(&self.graph));
        let (job, query, args, effects) = (
            job.to_string(),
            query.to_string(),
            args.clone(),
            effects.to_vec(),
        );
        Box::new(move |_| {
            crate::script::run_query_job(graph.clone(), &job, &query, &args, effects.clone())
        })
    }

    #[cfg(not(feature = "script"))]
    fn query_action(
        &self,
        _job: &str,
        _query: &str,
        _args: &crate::value::PropertyMap,
        _effects: &[crate::graph::AccessRequestBuf],
    ) -> crate::graph::jobs::JobAction {
        Box::new(|_| Err("this build cannot call saved queries (feature `script`)".to_string()))
    }
}

/// A backup job's run: the store into the directory `target`.
#[cfg(not(target_arch = "wasm32"))]
fn backup_to(core: &StoreCore, target: &std::path::Path) -> Result<(), String> {
    core.backup(target)
        .map(|_| ())
        .map_err(|err| err.to_string())
}

#[cfg(target_arch = "wasm32")]
fn backup_to(_core: &StoreCore, _target: &std::path::Path) -> Result<(), String> {
    Err("a backup job writes a directory, and this build has no file system".to_string())
}
