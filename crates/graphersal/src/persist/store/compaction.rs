//! Compaction of a Store (format spec 14.6): `Store::compact` (= `prune` + the backend's
//! compaction) and the cheap advice whether to call it now (`Store::compaction_advice`).
//!
//! The advice reads no element data: the backend's own bookkeeping (`StoreDir::space_usage`,
//! the free space of its file system) and the names and lengths of the store's files (what a
//! `prune` would remove: snapshot directories and WAL segments, sized by their lengths). Only
//! when a prune would remove something, it also reads the small manifests of the kept and the
//! attic snapshots (the files they still reference stay, so they are not counted) and, when a
//! snapshot would go, the `marks` file (the marks it would make unreachable). It is safe to
//! call often, from any thread, while the store is in use.

use super::super::dir::DirCompaction;
use super::super::error::PersistError;
use super::files::DirPath;
use super::maintenance::{PruneReport, prune_estimate};

/// The rule of [`Store::compaction_advice`](super::Store::compaction_advice): when a
/// compaction is worth it. The defaults are conservative: at least 50 % of the store's bytes
/// and at least 64 MiB to gain, in a store of at least 64 MiB.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct CompactionPolicy {
    /// The share of the store's bytes a compaction must give back (0.0..=1.0; default 0.5).
    pub min_garbage_ratio: f64,
    /// The bytes a compaction must give back (default 64 MiB).
    pub min_reclaimable_bytes: u64,
    /// The store's size below which nothing is recommended (default 64 MiB).
    pub min_total_bytes: u64,
}

impl Default for CompactionPolicy {
    fn default() -> Self {
        Self {
            min_garbage_ratio: 0.5,
            min_reclaimable_bytes: 64 << 20,
            min_total_bytes: 64 << 20,
        }
    }
}

impl CompactionPolicy {
    /// The defaults (50 %, 64 MiB to gain, a store of at least 64 MiB).
    pub fn new() -> Self {
        Self::default()
    }

    /// The share of the store's bytes a compaction must give back (clamped to 0.0..=1.0).
    pub fn with_min_garbage_ratio(mut self, ratio: f64) -> Self {
        self.min_garbage_ratio = if ratio.is_nan() {
            0.0
        } else {
            ratio.clamp(0.0, 1.0)
        };
        self
    }

    /// The bytes a compaction must give back.
    pub fn with_min_reclaimable_bytes(mut self, bytes: u64) -> Self {
        self.min_reclaimable_bytes = bytes;
        self
    }

    /// The store's size below which nothing is recommended.
    pub fn with_min_total_bytes(mut self, bytes: u64) -> Self {
        self.min_total_bytes = bytes;
        self
    }
}

/// Which part of the policy decided a [`CompactionAdvice`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AdviceRule {
    /// Every threshold is met: compact now.
    ThresholdsMet,
    /// The store cannot change now (maintenance mode, a backup, closed).
    NotWritable,
    /// The store is smaller than [`CompactionPolicy::min_total_bytes`].
    TotalBytes,
    /// Less than [`CompactionPolicy::min_reclaimable_bytes`] to gain.
    ReclaimableBytes,
    /// Less than [`CompactionPolicy::min_garbage_ratio`] of the store to gain.
    GarbageRatio,
    /// The file system lacks the free space the new file needs.
    DiskSpace,
}

impl AdviceRule {
    /// The rule as a snake_case word (`"thresholds_met"`, `"disk_space"`, ...).
    pub fn name(&self) -> &'static str {
        match self {
            AdviceRule::ThresholdsMet => "thresholds_met",
            AdviceRule::NotWritable => "not_writable",
            AdviceRule::TotalBytes => "total_bytes",
            AdviceRule::ReclaimableBytes => "reclaimable_bytes",
            AdviceRule::GarbageRatio => "garbage_ratio",
            AdviceRule::DiskSpace => "disk_space",
        }
    }
}

/// Whether to call [`Store::compact`](super::Store::compact) now, and the numbers behind it
/// ([`Store::compaction_advice`](super::Store::compaction_advice)).
///
/// For a single-file store the garbage is what removals and pruned files left in the file;
/// `compact()` prunes first and then rewrites the live data into a new file. For a directory
/// store (or one in memory) removals give the space back at once: the advice is about what
/// `compact()`'s prune would free, `garbage_bytes` is 0 and no free space is needed.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct CompactionAdvice {
    /// Whether `compact()` is worth it now.
    pub recommended: bool,
    /// The rule that decided.
    pub decided_by: AdviceRule,
    /// Why, in one sentence.
    pub reason: String,
    /// Whether the backend keeps garbage that a compaction gives back (a single file); `false`
    /// for directories, where `compact()` is a prune.
    pub compactable: bool,
    /// The store's bytes (the file's size, or the sum of the directory's files).
    pub total_bytes: u64,
    /// The bytes a compaction would keep (the live data with its framing).
    pub live_bytes: u64,
    /// The garbage in the file now (`total - live`; 0 for a directory).
    pub garbage_bytes: u64,
    /// `garbage_bytes / total_bytes`.
    pub garbage_ratio: f64,
    /// What a prune (of `compact()`) would remove first, in bytes (in a single file it becomes
    /// garbage that the compaction then gives back).
    pub prune_bytes: u64,
    /// Whether a prune would free more first (`prune_bytes > 0`); `prune` lists what.
    pub prune_first: bool,
    /// The snapshots and WAL segments the prune would remove.
    pub prune: PruneReport,
    /// What `compact()` would give back in all: the garbage plus the pruned bytes.
    pub reclaimable_bytes: u64,
    /// `reclaimable_bytes / total_bytes`: what [`CompactionPolicy::min_garbage_ratio`] is
    /// compared with.
    pub reclaimable_ratio: f64,
    /// The free disk space the new file needs (about the live bytes after the prune; 0 for a
    /// directory).
    pub space_needed: u64,
    /// The free space of the file system (`None`: unknown, or a directory store).
    pub free_space: Option<u64>,
    /// The policy applied.
    pub policy: CompactionPolicy,
}

/// What [`Store::compact`](super::Store::compact) did.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct CompactReport {
    /// The prune that ran first.
    pub prune: PruneReport,
    /// The backend's compaction (`None` for a directory: the prune freed the space).
    pub compaction: Option<DirCompaction>,
}

impl CompactReport {
    /// The bytes given back by the backend's compaction (0 for a directory).
    pub fn bytes_freed(&self) -> u64 {
        self.compaction
            .map_or(0, |c| c.bytes_before.saturating_sub(c.bytes_after))
    }
}

/// A byte count for a sentence (`512 B`, `3.2 MiB`).
fn size_text(n: u64) -> String {
    const UNITS: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut value = n as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// Computes the advice for the store in `dir` (`blocked`: why it cannot change, if so).
pub(super) fn advise(
    dir: &DirPath,
    policy: CompactionPolicy,
    blocked: Option<String>,
) -> Result<CompactionAdvice, PersistError> {
    let (prune, prune_bytes) = prune_estimate(dir, u64::MAX)?;
    let backend = dir.store_dir();
    let usage = backend
        .space_usage()
        .map_err(|err| PersistError::io("reading the space usage of the store", err))?;
    let compactable = usage.is_some();
    let (total, live, garbage, needed, free) = match usage {
        Some(usage) => (
            usage.total_bytes,
            usage.live_bytes,
            usage.garbage_bytes(),
            usage.live_bytes.saturating_sub(prune_bytes),
            backend.free_space().map_err(|err| {
                PersistError::io("reading the free space of the file system", err)
            })?,
        ),
        None => {
            let total = super::rollback::tree_bytes(dir);
            (total, total, 0, 0, None)
        }
    };
    let reclaimable = garbage.saturating_add(prune_bytes).min(total);
    let ratio = |n: u64| {
        if total == 0 {
            0.0
        } else {
            n as f64 / total as f64
        }
    };
    let reclaimable_ratio = ratio(reclaimable);
    let mib = size_text;
    let action = if compactable {
        "a compaction"
    } else {
        "compact() (a prune for a directory store)"
    };
    let (decided_by, reason) = if let Some(why) = blocked {
        (
            AdviceRule::NotWritable,
            format!("the store cannot change now: {why}"),
        )
    } else if total < policy.min_total_bytes {
        (
            AdviceRule::TotalBytes,
            // The size itself is `total_bytes`, shown beside the reason (CLI `store`, the
            // Store menu's store size): the reason names only the rule, so no front end
            // shows the store's size twice.
            format!(
                "the store is smaller than the minimum of {}",
                mib(policy.min_total_bytes)
            ),
        )
    } else if reclaimable < policy.min_reclaimable_bytes.max(1) {
        (
            AdviceRule::ReclaimableBytes,
            format!(
                "{action} would give back {} (below the minimum of {})",
                mib(reclaimable),
                mib(policy.min_reclaimable_bytes)
            ),
        )
    } else if reclaimable_ratio < policy.min_garbage_ratio {
        (
            AdviceRule::GarbageRatio,
            format!(
                "{action} would give back {:.0} % of the store (below the minimum of {:.0} %)",
                reclaimable_ratio * 100.0,
                policy.min_garbage_ratio * 100.0
            ),
        )
    } else if compactable && free.is_some_and(|free| free < needed) {
        (
            AdviceRule::DiskSpace,
            format!(
                "the new file needs about {} of free disk space, {} are free",
                mib(needed),
                mib(free.unwrap_or(0))
            ),
        )
    } else {
        (
            AdviceRule::ThresholdsMet,
            format!(
                "{action} would give back {} ({:.0} % of the store){}",
                mib(reclaimable),
                reclaimable_ratio * 100.0,
                if prune_bytes > 0 {
                    format!(
                        ", {} of it by pruning old snapshots and WAL",
                        mib(prune_bytes)
                    )
                } else {
                    String::new()
                }
            ),
        )
    };
    Ok(CompactionAdvice {
        recommended: decided_by == AdviceRule::ThresholdsMet,
        decided_by,
        reason,
        compactable,
        total_bytes: total,
        live_bytes: live,
        garbage_bytes: garbage,
        garbage_ratio: ratio(garbage),
        prune_bytes,
        prune_first: prune_bytes > 0,
        prune,
        reclaimable_bytes: reclaimable,
        reclaimable_ratio,
        space_needed: needed,
        free_space: free,
        policy,
    })
}
