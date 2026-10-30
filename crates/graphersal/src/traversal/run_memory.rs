//! The memory of one execution: what it holds NOW, checked against the per-execution limit
//! `traversal.max_materialized_bytes` and the process ceiling `memory.limit`, and the peak of it
//! that the profile reports ([`crate::profile::QueryMemory`]).
//!
//! The measure is LIVE bytes, never a sum of allocations: memory a step frees is given back, so
//! only what the execution holds at the same time counts ("+200, -200, +300" stays at 300).
//! - **tracked** (a [`TrackingAllocator`](crate::alloc_tracking::TrackingAllocator) is installed
//!   and the `alloc-tracking` feature is on): the live heap of the execution's thread minus its
//!   value when the run started (one thread-local read per check), minus what the run added to
//!   the graph (a plan that writes: estimated at every step boundary from the element count, at
//!   the graph's average element size; the graph's growth counts against the process ceiling,
//!   not the per-execution limit);
//! - **estimated** otherwise: the engine's own count of what the run holds (the per-level step
//!   buffers by capacity, the values a step builds where the engine already sizes them: bulk
//!   expansions, `fold()` lists, bucket copies, GQL lists, `product()`, step-built strings, the
//!   `repeat()` frontier, the path records), charged when built and released when a later step
//!   replaces the stream ([`GraphStep::keeps_values`](crate::traversal::step::GraphStep)) or a
//!   child traversal returns. The figures come from what the engine computes anyway (no walk
//!   per element), so the estimate is coarse and says so (`MemorySource::Estimated`).
//!
//! The peak is SAMPLED: the highest figure seen at a check (every step boundary, every bulk
//! expansion, every `repeat()` traverser, every sized value), including what that check was
//! about to allocate; an exact high-water mark would cost every allocation of every binary.

use std::cell::Cell;

use crate::error::{GraphError, TraverserResult};
use crate::graph::GraphStorage;
use crate::traversal::memory_budget::{MemoryBudget, tracked_live_bytes};

/// The memory of one execution (see the module documentation). Interior mutability: steps
/// holding a shared borrow of the context still charge and check.
#[derive(Debug, Clone)]
pub(crate) struct RunMemory {
    /// Whether the tracking allocator counts (decided once, when the run starts).
    tracked: bool,
    /// The thread's live heap bytes when the run started (tracked only).
    thread_start: i64,
    /// Estimated bytes charged by the stream being executed: buffers and values, released when
    /// the step that built them is replaced or the level returns ([`Self::mark`]).
    stream: Cell<u64>,
    /// Estimated bytes held for the rest of the run: values side-effect steps keep.
    retained: Cell<u64>,
    /// Estimated bytes of the run's path records (`PathArena::value_bytes`), set by the
    /// context before a check.
    arena: Cell<u64>,
    /// The highest live figure (plus pending) seen at a check.
    peak: Cell<u64>,
    /// `traversal.max_materialized_bytes` after job and policy caps.
    limit: u64,
    /// The process ceiling, started on the graph (`None`: no ceiling).
    process: Option<MemoryBudget>,
    /// Estimated: what the run added to the graph, refreshed at step boundaries (the process
    /// ceiling counts it). Tracked: the same estimate, refreshed at the boundaries of a plan that
    /// writes, and subtracted from the thread's live heap.
    grown: Cell<u64>,
    /// Vertices plus edges when the run started (tracked; for the graph growth).
    elements_at_start: u64,
    /// The estimated bytes of one element, taken once when the tracked check needs it.
    element_bytes: Cell<Option<u64>>,
    /// Whether the plan writes to the graph (tracked: only then is the graph's growth
    /// estimated, at every step boundary).
    writes: Cell<bool>,
}

impl RunMemory {
    /// The memory of a run limited to `limit` bytes (`traversal.max_materialized_bytes`) under
    /// the process ceiling `process` (already started, or `None`). Takes the starting point of
    /// the tracked measure now; the graph's element count is 0 ([`Self::start`] takes it).
    pub(crate) fn new(limit: u64, process: Option<MemoryBudget>) -> Self {
        let tracked = tracked_live_bytes().is_some();
        Self {
            tracked,
            thread_start: thread_live(),
            stream: Cell::new(0),
            retained: Cell::new(0),
            arena: Cell::new(0),
            peak: Cell::new(0),
            limit,
            process,
            grown: Cell::new(0),
            elements_at_start: 0,
            element_bytes: Cell::new(None),
            writes: Cell::new(false),
        }
    }

    /// [`Self::new`] for a run on `storage`: in tracked mode also takes the graph's element
    /// count, so what the run adds to the graph is not charged to the execution.
    pub(crate) fn start<S: GraphStorage + ?Sized>(
        storage: &S,
        limit: u64,
        process: Option<MemoryBudget>,
    ) -> Result<Self, GraphError> {
        let mut memory = Self::new(limit, process);
        if memory.tracked {
            memory.elements_at_start = element_count(storage)?;
        }
        Ok(memory)
    }

    /// Notes whether the plan writes to the graph (set once the plan is analysed).
    #[inline]
    pub(crate) fn set_writes(&self, writes: bool) {
        self.writes.set(writes);
    }

    /// Whether the measure is the tracking allocator's (else the engine's estimate).
    #[inline]
    pub(crate) fn is_tracked(&self) -> bool {
        self.tracked
    }

    /// `traversal.max_materialized_bytes` of this run.
    #[inline]
    pub(crate) fn limit(&self) -> u64 {
        self.limit
    }

    /// The highest figure seen at a check so far.
    #[inline]
    pub(crate) fn peak(&self) -> u64 {
        self.peak.get()
    }

    /// What the execution holds now: tracked, the thread's live heap since the start minus the
    /// graph's growth (as last estimated); estimated, the charged bytes.
    #[inline]
    pub(crate) fn live(&self) -> u64 {
        if self.tracked {
            let delta = thread_live().saturating_sub(self.thread_start);
            u64::try_from(delta)
                .unwrap_or(0)
                .saturating_sub(self.grown.get())
        } else {
            self.held()
        }
    }

    /// The estimated bytes charged (estimated mode; 0 when tracked).
    #[inline]
    fn held(&self) -> u64 {
        self.stream
            .get()
            .saturating_add(self.retained.get())
            .saturating_add(self.arena.get())
    }

    /// Charges `bytes` the stream now holds (estimated mode only; nothing when tracked).
    #[inline]
    pub(crate) fn charge(&self, bytes: u64) {
        if !self.tracked {
            self.stream.set(self.stream.get().saturating_add(bytes));
        }
    }

    /// Releases `bytes` charged before (saturating).
    #[inline]
    pub(crate) fn release(&self, bytes: u64) {
        if !self.tracked {
            self.stream.set(self.stream.get().saturating_sub(bytes));
        }
    }

    /// Moves `bytes` the stream charged into what the run holds until it ends (the values a
    /// side-effect step keeps).
    #[inline]
    pub(crate) fn retain(&self, bytes: u64) {
        if !self.tracked && bytes > 0 {
            self.release(bytes);
            self.retained.set(self.retained.get().saturating_add(bytes));
        }
    }

    /// The stream's charge now, to [`Self::restore`] when a level or a loop returns.
    #[inline]
    pub(crate) fn mark(&self) -> u64 {
        self.stream.get()
    }

    /// Releases everything charged to the stream since `mark` (a level returned: its buffers
    /// and the values of its steps are its caller's business now).
    #[inline]
    pub(crate) fn restore(&self, mark: u64) {
        if !self.tracked {
            self.stream.set(mark);
        }
    }

    /// The stream's charge since `mark` (what one step charged).
    #[inline]
    pub(crate) fn since(&self, mark: u64) -> u64 {
        self.stream.get().saturating_sub(mark)
    }

    /// Sets the bytes of the run's path records (estimated mode counts them).
    #[inline]
    pub(crate) fn set_arena(&self, bytes: u64) {
        self.arena.set(bytes);
    }

    /// Fails when what the execution holds plus `pending` bytes (what the caller is about to
    /// allocate) exceeds `traversal.max_materialized_bytes`, then when the process is over its
    /// ceiling (`memory.limit`). Records the figure as the peak when it is the highest so far.
    #[inline]
    pub(crate) fn check(&self, pending: u64) -> TraverserResult<()> {
        let total = self.live().saturating_add(pending);
        if total > self.peak.get() {
            self.peak.set(total);
        }
        if total > self.limit {
            return Err(crate::traversal::bulk::materialized_bytes_error(
                self.limit, total,
            ));
        }
        self.check_process(pending)
    }

    /// The process ceiling with `pending` bytes about to be allocated: tracked, the process's
    /// live heap; estimated, the graph footprint at the start plus its growth plus what the run
    /// holds.
    #[inline]
    fn check_process(&self, pending: u64) -> TraverserResult<()> {
        match &self.process {
            None => Ok(()),
            Some(budget) if self.tracked => budget.check(pending),
            Some(budget) => budget.check(
                self.held()
                    .saturating_add(self.grown.get())
                    .saturating_add(pending),
            ),
        }
    }

    /// The check at a step boundary on `storage`: refreshes the graph's growth where a measure
    /// needs it (estimated with a process ceiling: every boundary; tracked: every boundary of a
    /// plan that writes, so neither the check nor the peak counts what a write added to the
    /// graph), then [`Self::check`].
    pub(crate) fn check_boundary<S: GraphStorage + ?Sized>(
        &self,
        storage: &S,
    ) -> TraverserResult<()> {
        if self.tracked {
            if self.writes.get() {
                self.grown.set(self.tracked_growth(storage)?);
            }
        } else if let Some(budget) = &self.process {
            self.grown.set(budget.grown_bytes(storage)?);
        }
        self.check(0)
    }

    /// Tracked: the bytes the run added to the graph and to its unit's undo record, at the
    /// graph's average element size (taken once per run from the storage's footprint hint, else
    /// its footprint) plus one undo entry per added element.
    fn tracked_growth<S: GraphStorage + ?Sized>(&self, storage: &S) -> Result<u64, GraphError> {
        let now = element_count(storage)?;
        let added = now.saturating_sub(self.elements_at_start);
        if added == 0 {
            return Ok(0);
        }
        let each = match self.element_bytes.get() {
            Some(each) => each,
            None => {
                let footprint = match storage.memory_footprint_hint()? {
                    Some(hint) => hint,
                    None => storage.memory_footprint_bytes()?.unwrap_or(0),
                };
                // The element itself, at the graph's average, plus the unit's record of the
                // write (an undo entry of `TraversalGraph`; for another storage an allowance for
                // its own record of the unit).
                let each = footprint
                    .checked_div(now)
                    .unwrap_or(0)
                    .saturating_add(std::mem::size_of::<crate::graph::undo::UndoEntry>() as u64);
                self.element_bytes.set(Some(each));
                each
            }
        };
        Ok(added.saturating_mul(each))
    }

    /// The run's figures for the profile.
    pub(crate) fn query_memory(&self) -> crate::traversal::metrics::QueryMemory {
        crate::traversal::metrics::QueryMemory {
            peak_bytes: self.peak(),
            limit_bytes: self.limit,
            source: if self.tracked {
                crate::traversal::metrics::MemorySource::Tracked
            } else {
                crate::traversal::metrics::MemorySource::Estimated
            },
        }
    }
}

/// Vertices plus edges of `storage`.
fn element_count<S: GraphStorage + ?Sized>(storage: &S) -> Result<u64, GraphError> {
    Ok((storage.vertex_count()? as u64).saturating_add(storage.edge_count()? as u64))
}

/// This thread's live heap bytes (0 without the `alloc-tracking` feature).
#[inline]
fn thread_live() -> i64 {
    #[cfg(feature = "alloc-tracking")]
    {
        crate::alloc_tracking::thread_live_bytes()
    }
    #[cfg(not(feature = "alloc-tracking"))]
    {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::TraverserError;

    /// An estimated run memory (the test binary installs no tracking allocator).
    fn estimated(limit: u64) -> RunMemory {
        let memory = RunMemory::new(limit, None);
        assert!(!memory.is_tracked());
        memory
    }

    fn observed(err: &TraverserError) -> Option<u64> {
        match err {
            TraverserError::ResourceLimitExceeded {
                limit, observed, ..
            } if limit == "traversal.max_materialized_bytes" => *observed,
            _ => None,
        }
    }

    /// The user's example: "+200, -200, +300" under a limit of 400 never exceeds it.
    #[test]
    fn freed_bytes_are_given_back() {
        let memory = estimated(400);
        memory.charge(200);
        assert!(memory.check(0).is_ok());
        memory.release(200);
        assert!(memory.check(0).is_ok());
        memory.charge(300);
        assert!(memory.check(0).is_ok());
        assert_eq!(memory.live(), 300);
        assert_eq!(memory.peak(), 300);
        // What is held at once is limited: 300 held plus 101 pending.
        let err = memory.check(101).unwrap_err();
        assert_eq!(observed(&err), Some(401));
        assert_eq!(memory.peak(), 401, "the peak includes the failing check");
    }

    #[test]
    fn release_saturates_and_restore_returns_to_the_mark() {
        let memory = estimated(1000);
        memory.release(50);
        assert_eq!(memory.live(), 0);
        memory.charge(100);
        let mark = memory.mark();
        memory.charge(300);
        assert_eq!(memory.since(mark), 300);
        memory.restore(mark);
        assert_eq!(memory.live(), 100);
    }

    #[test]
    fn retained_bytes_survive_a_restore_and_the_arena_counts() {
        let memory = estimated(1000);
        let mark = memory.mark();
        memory.charge(400);
        memory.retain(150);
        memory.restore(mark);
        assert_eq!(memory.live(), 150);
        memory.set_arena(50);
        assert_eq!(memory.live(), 200);
    }

    #[test]
    fn the_peak_is_the_highest_figure_seen_at_a_check() {
        let memory = estimated(10_000);
        memory.charge(5000);
        // Not checked: the peak is sampled, not exact.
        memory.release(4000);
        assert!(memory.check(0).is_ok());
        assert_eq!(memory.peak(), 1000);
        memory.charge(2000);
        assert!(memory.check(500).is_ok());
        assert_eq!(memory.peak(), 3500);
        memory.release(3000);
        assert!(memory.check(0).is_ok());
        assert_eq!(memory.peak(), 3500);
    }

    #[test]
    fn the_process_ceiling_counts_what_the_run_holds() {
        // A ceiling without a starting point: the graph counts 0, the run's charges count.
        let memory = RunMemory::new(u64::MAX, Some(MemoryBudget::new(1000)));
        memory.charge(900);
        assert!(memory.check(100).is_ok());
        let err = memory.check(101).unwrap_err();
        assert!(matches!(
            *err,
            TraverserError::ResourceLimitExceeded { ref limit, observed: Some(1001), .. }
                if limit == "memory.limit"
        ));
    }

    #[cfg(feature = "alloc-tracking")]
    #[test]
    fn a_tracked_delta_below_the_start_counts_as_zero() {
        // Built by hand: the test binary installs no tracking allocator.
        let mut memory = RunMemory::new(100, None);
        memory.tracked = true;
        memory.thread_start = thread_live().saturating_add(1_000_000);
        assert_eq!(memory.live(), 0);
        assert!(memory.check(100).is_ok());
    }
}
