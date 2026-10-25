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
    /// under the write lock). The jobs hold the store's core, not the store.
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
                    crate::graph::GraphStorage::shrink_to_fit(&mut *graph.write());
                    Ok(())
                })
                .disabled(),
            );
        }
        jobs
    }
}
