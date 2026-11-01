//! The memory checks against the process memory ceiling (`memory.limit`,
//! [`crate::traversal::memory_limit`]: one process-wide setting, ON by default), so an oversized
//! query fails with [`TraverserError::ResourceLimitExceeded`] (and its unit rolls back) instead
//! of the process being killed by the operating system (a Kubernetes/cgroup OOM kill).
//!
//! A Rust allocator cannot fail softly, so the ceiling is cooperative: it is compared at the
//! places that already check `traversal.max_materialized_bytes` (a bulk expansion before it
//! allocates, a bucket push of copies, every `repeat()` iteration) and at every step boundary.
//! What is compared:
//! - **tracked** (a [`TrackingAllocator`](crate::alloc_tracking::TrackingAllocator) is installed
//!   and the `alloc-tracking` feature is on): the process's exact live heap bytes
//!   (`alloc_tracking::global_live_bytes`, one relaxed atomic load) plus the estimated size of
//!   what a check is about to allocate;
//! - **estimated** otherwise: the graph footprint when the run starts, plus the elements the run
//!   added since (at the graph's average element size), plus what the run holds by the engine's
//!   count (its step buffers, values and `repeat()` frontier: `traversal/run_memory.rs`, the same
//!   counter as the per-execution limit), plus the estimated size of what a check is about to
//!   allocate.
//!
//! **No walk of the graph per run** (the ceiling is on by default, so a run must not pay for
//! it): the estimated start takes the storage's cheap
//! [`GraphStorage::memory_footprint_hint`] (for `TraversalGraph` the last walked figure,
//! adjusted by the elements added or removed since, at the average element size) and walks the
//! graph ([`GraphStorage::memory_footprint_bytes`], cached by the commit position) only when
//! there is no hint yet (the first run on a graph) or the hint is at least half of the ceiling,
//! where precision matters. `TraversalGraph` also has no hint once its element count doubled or
//! halved since the last walk, but only outside an open unit with changes, where the walk is
//! cached: inside such a unit (a script, a transaction) the hint stays an estimate, so the
//! runs of a unit do not walk the graph one after the other (`graph::memory::footprint_hint`).
//! The tracked measure never walks (`RunMemory::tracked_growth`). Every check is a few loads
//! and one comparison.
//!
//! Units that are not traversals ([`UnitBudget`]: a GraphML/GraphSON import into a live graph,
//! `apply_change_set`) check the same ceiling every [`UNIT_CHECK_INTERVAL`] elements and at
//! their end; `TraversalGraph` checks every outermost unit at its commit
//! ([`MemoryBudget::unit_check`]). Only a unit that grew the measure fails, with
//! [`GraphError::ResourceLimitExceeded`] naming the operation; the caller rolls it back.

use crate::error::{GraphError, TraverserError, TraverserResult};
use crate::graph::GraphStorage;
pub(crate) use crate::traversal::memory_limit::MEMORY_LIMIT;

/// The smallest default headroom: 64 MiB for the stack, the code, allocator overhead and what the
/// engine does not count.
pub(crate) const MIN_DEFAULT_HEADROOM: u64 = 64 * 1024 * 1024;

/// The cgroup v1 "no limit" value is `i64::MAX` rounded down to a page; anything this large is
/// no limit.
#[cfg(not(target_arch = "wasm32"))]
const CGROUP_UNLIMITED_FROM: u64 = 1 << 62;

/// The smallest size the estimate assumes for one element a run adds (an empty graph has no
/// average to take): a topology slot, a handle generation and an index entry.
pub(crate) const MIN_ELEMENT_BYTES: u64 = 128;

/// The default headroom `auto` keeps below a cgroup limit of `limit` bytes: a tenth of it, at
/// least [`MIN_DEFAULT_HEADROOM`], but never more than half of the limit.
pub(crate) fn default_headroom(limit: u64) -> u64 {
    (limit / 10).max(MIN_DEFAULT_HEADROOM).min(limit / 2)
}

/// The ceiling `auto` yields for a cgroup limit `cgroup` (`None`: no limit, so no budget) and an
/// optional explicit `headroom`: the limit minus the headroom, at least 1 byte.
pub(crate) fn auto_budget(cgroup: Option<u64>, headroom: Option<u64>) -> Option<u64> {
    let limit = cgroup?;
    let headroom = headroom.unwrap_or_else(|| default_headroom(limit));
    Some(limit.saturating_sub(headroom).max(1))
}

/// Reads one cgroup memory-limit file's content: a byte count, or `max` (no limit).
#[cfg(not(target_arch = "wasm32"))]
fn parse_cgroup_value(text: &str) -> Option<u64> {
    let text = text.trim();
    if text == "max" {
        return None;
    }
    text.parse::<u64>()
        .ok()
        .filter(|limit| *limit < CGROUP_UNLIMITED_FROM)
}

/// The memory limit of the cgroup mounted at `root` (normally `/sys/fs/cgroup`): cgroup v2
/// `<root>/memory.max`, else cgroup v1 `<root>/memory/memory.limit_in_bytes`. `None` when neither
/// file exists or is readable, or when the limit is `max` (unlimited). The root is a parameter so
/// tests can point it at a fake directory.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn cgroup_limit_at(root: &std::path::Path) -> Option<u64> {
    if let Ok(text) = std::fs::read_to_string(root.join("memory.max")) {
        return parse_cgroup_value(&text);
    }
    std::fs::read_to_string(root.join("memory").join("memory.limit_in_bytes"))
        .ok()
        .and_then(|text| parse_cgroup_value(&text))
}

/// The cgroup memory limit of this process, read once per process from `/sys/fs/cgroup` (a
/// container's limit does not change while it runs). Never on `wasm32`.
pub(crate) fn cgroup_limit() -> Option<u64> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        static LIMIT: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
        *LIMIT.get_or_init(|| cgroup_limit_at(std::path::Path::new("/sys/fs/cgroup")))
    }
    #[cfg(target_arch = "wasm32")]
    {
        None
    }
}

/// The exact live heap bytes of the process when a `TrackingAllocator` is installed (feature
/// `alloc-tracking`); `None` otherwise (the counter reads `0` when no tracking allocator counts).
#[inline]
pub(crate) fn tracked_live_bytes() -> Option<u64> {
    #[cfg(feature = "alloc-tracking")]
    {
        let live = crate::alloc_tracking::global_live_bytes();
        if live > 0 {
            return Some(live);
        }
    }
    None
}

/// The memory budget of one run: the limit, and what it is compared with (see the module
/// documentation). Plain numbers, copied into the expansion limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MemoryBudget {
    /// The budget in bytes.
    pub(crate) limit: u64,
    /// Whether the process's live heap bytes are counted exactly (a tracking allocator is
    /// installed); otherwise the estimate below applies.
    tracked: bool,
    /// Estimated footprint of the graph when the run started.
    base: u64,
    /// Vertices plus edges when the run started.
    elements_at_start: u64,
    /// Estimated bytes of one element the run adds: the graph's average, at least
    /// [`MIN_ELEMENT_BYTES`].
    element_bytes: u64,
    /// The process's live heap bytes when the run started (tracked only; else 0): a unit
    /// grew the measure when the heap is larger now ([`Self::unit_check`]).
    live_at_start: u64,
}

impl MemoryBudget {
    /// A budget of `limit` bytes that has not taken its starting point yet ([`Self::start`]).
    pub(crate) fn new(limit: u64) -> Self {
        Self {
            limit,
            tracked: false,
            base: 0,
            elements_at_start: 0,
            element_bytes: MIN_ELEMENT_BYTES,
            live_at_start: 0,
        }
    }

    /// Takes the starting point of a run on `storage`: nothing with a tracking allocator (the
    /// live counter already includes the graph), else the graph's footprint and its element
    /// count. The footprint is the storage's cheap hint
    /// ([`GraphStorage::memory_footprint_hint`]) while that is below half of the ceiling, else
    /// the exact figure ([`GraphStorage::memory_footprint_bytes`]: at most ONE walk, none when
    /// the graph has not changed since the last one; a storage without introspection counts as
    /// 0 bytes), so a run far below the ceiling never walks the graph. A storage that cannot
    /// measure itself fails the run with its error.
    pub(crate) fn start<S: GraphStorage + ?Sized>(self, storage: &S) -> Result<Self, GraphError> {
        if let Some(live) = tracked_live_bytes() {
            return Ok(Self {
                tracked: true,
                live_at_start: live,
                ..self
            });
        }
        let base = match storage.memory_footprint_hint()? {
            Some(hint) if hint < self.limit / 2 => hint,
            _ => storage.memory_footprint_bytes()?.unwrap_or(0),
        };
        let elements =
            (storage.vertex_count()? as u64).saturating_add(storage.edge_count()? as u64);
        let element_bytes = base
            .checked_div(elements)
            .unwrap_or(0)
            .max(MIN_ELEMENT_BYTES);
        Ok(Self {
            tracked: false,
            // The embedding models loaded in the process are in memory too (counted exactly
            // by a tracking allocator; estimated here from their recorded sizes).
            base: base.saturating_add(crate::value::vector::embed::registry::loaded_model_bytes()),
            elements_at_start: elements,
            element_bytes,
            ..self
        })
    }

    /// What is in use now by the measure of this budget, before the run's own traversers and
    /// pending allocations: the live heap (tracked) or the graph footprint at the start
    /// (estimated).
    #[inline]
    fn in_use(&self) -> u64 {
        if self.tracked {
            tracked_live_bytes().unwrap_or(0)
        } else {
            self.base
        }
    }

    /// Fails when what is in use plus `pending` bytes (what the caller is about to allocate, by
    /// the engine's estimate) exceeds the budget.
    #[inline]
    pub(crate) fn check(&self, pending: u64) -> TraverserResult<()> {
        let total = self.in_use().saturating_add(pending);
        if total > self.limit {
            return Err(memory_limit_error(self.limit, total));
        }
        Ok(())
    }

    /// Estimated: the elements added to `storage` since the start (net), at the average
    /// element size. Tracked: 0 (the live heap holds them).
    #[inline]
    pub(crate) fn grown_bytes<S: GraphStorage + ?Sized>(
        &self,
        storage: &S,
    ) -> Result<u64, GraphError> {
        if self.tracked {
            return Ok(0);
        }
        let elements =
            (storage.vertex_count()? as u64).saturating_add(storage.edge_count()? as u64);
        Ok(elements
            .saturating_sub(self.elements_at_start)
            .saturating_mul(self.element_bytes))
    }

    /// The check of a unit that is not a traversal (an import, an applied change set, the
    /// commit of an outermost unit), on `storage` as the unit left it: what is in use by the
    /// measure of this budget (tracked: the live heap; estimated: the starting footprint plus
    /// the elements the unit added since the start). Fails, naming `operation`, only when that
    /// is over the budget AND the unit grew it (tracked: the heap is larger than at the start;
    /// estimated: the unit added elements), so a unit that only removes or rewrites data is
    /// never stopped, also on a graph that is already over its budget. On failure the caller
    /// rolls the unit back.
    pub(crate) fn unit_check<S: GraphStorage + ?Sized>(
        &self,
        storage: &S,
        operation: &'static str,
    ) -> Result<(), GraphError> {
        self.unit_check_adding(storage, operation, 0)
    }

    /// [`Self::unit_check`] of a unit that also wrote `written` bytes into existing elements
    /// (the vectors of a vector rule fill): an estimate counts them on top of the elements it
    /// added (a tracking allocator sees them in the live heap anyway).
    pub(crate) fn unit_check_adding<S: GraphStorage + ?Sized>(
        &self,
        storage: &S,
        operation: &'static str,
        written: u64,
    ) -> Result<(), GraphError> {
        let (total, grew) = if self.tracked {
            let live = tracked_live_bytes().unwrap_or(0);
            (live, live > self.live_at_start)
        } else {
            let grown = self.grown_bytes(storage)?.saturating_add(written);
            (self.base.saturating_add(grown), grown > 0)
        };
        if grew && total > self.limit {
            return Err(unit_limit_error(operation, self.limit, total));
        }
        Ok(())
    }
}

/// How many elements a unit that is not a traversal handles between two checks of its memory
/// budget ([`UnitBudget::tick`]): imported vertices and edges, applied mutations. Its end is
/// checked too, so this bounds only how far past the budget a unit gets before it stops.
pub(crate) const UNIT_CHECK_INTERVAL: u32 = 4096;

/// The memory ceiling of a unit that is not a traversal: a GraphML or GraphSON import into a
/// live graph, an applied change set. Built only when there is a ceiling ([`Self::start`]);
/// without one nothing is paid, neither a walk nor a check.
#[derive(Debug, Clone)]
pub(crate) struct UnitBudget {
    budget: MemoryBudget,
    /// What the error names: `"GraphML import"`, `"change set apply"`, ...
    operation: &'static str,
    /// Elements left until the next check.
    countdown: u32,
    /// The input bytes a load has read so far ([`Self::with_input`]), the estimate of what it
    /// added in estimated mode.
    input: Option<InputCounter>,
}

impl UnitBudget {
    /// The ceiling of a unit `operation` on `storage` (the process ceiling), started on
    /// `storage` as it is now (no walk of the graph far below the ceiling, see
    /// [`MemoryBudget::start`]). `None` when there is no ceiling. `Err`: the storage cannot
    /// measure itself (the unit fails before it starts).
    pub(crate) fn start<S: GraphStorage + ?Sized>(
        storage: &S,
        operation: &'static str,
    ) -> Result<Option<Self>, GraphError> {
        let Some(limit) = crate::traversal::memory_limit::effective_limit() else {
            return Ok(None);
        };
        Ok(Some(Self {
            budget: MemoryBudget::new(limit).start(storage)?,
            operation,
            countdown: UNIT_CHECK_INTERVAL,
            input: None,
        }))
    }

    /// [`Self::start`] with the estimated size of one element the unit adds set to
    /// `element_bytes` (at least the 128-byte floor) instead of the graph's average: a load into
    /// an empty graph has no average, but knows one (a snapshot's manifest estimate divided by
    /// its element count). Tracked mode ignores it.
    #[cfg(any(feature = "persist", test))]
    pub(crate) fn start_with_element_bytes<S: GraphStorage + ?Sized>(
        storage: &S,
        operation: &'static str,
        element_bytes: u64,
    ) -> Result<Option<Self>, GraphError> {
        Ok(Self::start(storage, operation)?.map(|mut unit| {
            unit.budget.element_bytes = element_bytes.max(MIN_ELEMENT_BYTES);
            unit
        }))
    }

    /// Estimates what the unit added by `input` instead of the element count (estimated mode
    /// only): the bytes of text a load into a new graph has read so far (`CountingReader`;
    /// GraphSON or GraphML text is the same order of size as the graph it builds). The larger
    /// of both estimates counts.
    #[cfg(any(feature = "io", test))]
    pub(crate) fn with_input(mut self, input: InputCounter) -> Self {
        self.input = Some(input);
        self
    }

    /// Counts one handled element; every [`UNIT_CHECK_INTERVAL`]-th one checks the budget
    /// ([`MemoryBudget::unit_check`]).
    #[inline]
    pub(crate) fn tick<S: GraphStorage + ?Sized>(&mut self, storage: &S) -> Result<(), GraphError> {
        self.countdown = self.countdown.saturating_sub(1);
        if self.countdown > 0 {
            return Ok(());
        }
        self.countdown = UNIT_CHECK_INTERVAL;
        self.check(storage)
    }

    /// Counts `n` handled elements at once (the mutations of one replayed commit, the records
    /// of one decoded chunk); checks the budget when the countdown runs out.
    #[inline]
    #[cfg(any(feature = "persist", test))]
    pub(crate) fn tick_n<S: GraphStorage + ?Sized>(
        &mut self,
        storage: &S,
        n: usize,
    ) -> Result<(), GraphError> {
        let n = u32::try_from(n).unwrap_or(u32::MAX);
        if n < self.countdown {
            self.countdown -= n;
            return Ok(());
        }
        self.countdown = UNIT_CHECK_INTERVAL;
        self.check(storage)
    }

    /// Checks the budget now ([`MemoryBudget::unit_check`]): the end of the unit, before its
    /// commit. With an input counter (estimated mode) the bytes read count as what the unit
    /// added when they are more than the element estimate.
    pub(crate) fn check<S: GraphStorage + ?Sized>(&self, storage: &S) -> Result<(), GraphError> {
        if let Some(input) = &self.input
            && !self.budget.tracked
        {
            let read = input.get();
            let grown = self.budget.grown_bytes(storage)?.max(read);
            let total = self.budget.base.saturating_add(grown);
            if grown > 0 && total > self.budget.limit {
                return Err(unit_limit_error(self.operation, self.budget.limit, total));
            }
            return Ok(());
        }
        self.budget.unit_check(storage, self.operation)
    }
}

/// The bytes a load has read from its input so far, shared between the `CountingReader` that
/// counts them and the [`UnitBudget`] that compares them (estimated mode).
#[derive(Debug, Clone, Default)]
pub(crate) struct InputCounter(std::sync::Arc<std::sync::atomic::AtomicU64>);

impl InputCounter {
    /// The bytes counted so far.
    pub(crate) fn get(&self) -> u64 {
        self.0.load(std::sync::atomic::Ordering::Relaxed)
    }

    #[cfg(any(feature = "io", test))]
    fn add(&self, bytes: usize) {
        self.0
            .fetch_add(bytes as u64, std::sync::atomic::Ordering::Relaxed);
    }
}

/// A reader that counts the bytes consumed from `inner` into an [`InputCounter`] (one relaxed
/// add per read or `consume`, never per element).
#[cfg(any(feature = "io", test))]
pub(crate) struct CountingReader<R> {
    inner: R,
    counter: InputCounter,
}

#[cfg(any(feature = "io", test))]
impl<R> CountingReader<R> {
    /// Counts what is read from `inner` into `counter`.
    pub(crate) fn new(inner: R, counter: InputCounter) -> Self {
        Self { inner, counter }
    }
}

#[cfg(any(feature = "io", test))]
impl<R: std::io::Read> std::io::Read for CountingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buf)?;
        self.counter.add(read);
        Ok(read)
    }
}

#[cfg(any(feature = "io", test))]
impl<R: std::io::BufRead> std::io::BufRead for CountingReader<R> {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        self.inner.fill_buf()
    }

    fn consume(&mut self, amount: usize) {
        self.counter.add(amount);
        self.inner.consume(amount);
    }
}

/// [`UnitBudget::tick`] when there is a budget; nothing otherwise.
#[inline]
pub(crate) fn tick<S: GraphStorage + ?Sized>(
    budget: &mut Option<UnitBudget>,
    storage: &S,
) -> Result<(), GraphError> {
    match budget {
        Some(budget) => budget.tick(storage),
        None => Ok(()),
    }
}

/// `UnitBudget::tick_n` when there is a budget; nothing otherwise.
#[inline]
#[cfg(feature = "persist")]
pub(crate) fn tick_n<S: GraphStorage + ?Sized>(
    budget: &mut Option<UnitBudget>,
    storage: &S,
    n: usize,
) -> Result<(), GraphError> {
    match budget {
        Some(budget) => budget.tick_n(storage, n),
        None => Ok(()),
    }
}

/// [`UnitBudget::check`] when there is a budget; nothing otherwise.
#[inline]
pub(crate) fn check<S: GraphStorage + ?Sized>(
    budget: &Option<UnitBudget>,
    storage: &S,
) -> Result<(), GraphError> {
    match budget {
        Some(budget) => budget.check(storage),
        None => Ok(()),
    }
}

/// The error of a unit that is not a traversal over its memory budget: `observed` bytes in use
/// against `limit`.
#[cold]
pub(crate) fn unit_limit_error(operation: &'static str, limit: u64, observed: u64) -> GraphError {
    GraphError::ResourceLimitExceeded {
        operation,
        limit: MEMORY_LIMIT.to_string(),
        max: Some(limit),
        observed: Some(observed),
        needed: None,
    }
}

/// Fails when `needed` more bytes would take the process over its memory ceiling (no ceiling:
/// `Ok`), BEFORE the caller allocates them. Tracked: the live heap plus `needed`; estimated:
/// `resident` (what the caller knows is held for the operation, such as the graph a load has
/// built so far) plus `needed`. `operation` names the caller (`"snapshot load"`, ...). One
/// relaxed load and one comparison; the error path is cold.
#[inline]
pub(crate) fn ensure_room(
    operation: &'static str,
    needed: u64,
    resident: u64,
) -> Result<(), GraphError> {
    let Some(limit) = crate::traversal::memory_limit::effective_limit() else {
        return Ok(());
    };
    let in_use = tracked_live_bytes().unwrap_or(resident);
    let total = in_use.saturating_add(needed);
    if total > limit {
        return Err(room_error(operation, limit, total, needed));
    }
    Ok(())
}

/// The error of [`ensure_room`]: `observed` = in use + `needed` against `limit`.
#[cold]
pub(crate) fn room_error(
    operation: &'static str,
    limit: u64,
    observed: u64,
    needed: u64,
) -> GraphError {
    GraphError::ResourceLimitExceeded {
        operation,
        limit: MEMORY_LIMIT.to_string(),
        max: Some(limit),
        observed: Some(observed),
        needed: Some(needed),
    }
}

/// [`ensure_room`] as an `io::Error` of kind `OutOfMemory` that carries the
/// [`GraphError::ResourceLimitExceeded`] as its inner error (for the byte reads of a store
/// directory, whose callers take `io::Result`).
#[cfg(all(feature = "persist", not(target_arch = "wasm32")))]
pub(crate) fn ensure_room_io(operation: &'static str, needed: u64) -> std::io::Result<()> {
    ensure_room(operation, needed, 0)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::OutOfMemory, err))
}

/// The error of a run over its memory budget: `observed` bytes in use (or about to be) against
/// `limit`.
#[cold]
pub(crate) fn memory_limit_error(limit: u64, observed: u64) -> Box<TraverserError> {
    Box::new(TraverserError::ResourceLimitExceeded {
        limit: MEMORY_LIMIT.to_string(),
        max: Some(limit),
        observed: Some(observed),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::Transactional;

    const MIB: u64 = 1024 * 1024;

    #[test]
    fn default_headroom_is_a_tenth_with_bounds() {
        assert_eq!(default_headroom(8 * 1024 * MIB), 8 * 1024 * MIB / 10);
        assert_eq!(default_headroom(512 * MIB), 64 * MIB);
        // Never more than half of a tiny limit.
        assert_eq!(default_headroom(100 * MIB), 50 * MIB);
    }

    #[test]
    fn auto_budget_subtracts_the_headroom() {
        assert_eq!(auto_budget(None, None), None);
        assert_eq!(auto_budget(Some(512 * MIB), None), Some(448 * MIB));
        assert_eq!(
            auto_budget(Some(512 * MIB), Some(12 * MIB)),
            Some(500 * MIB)
        );
        assert_eq!(auto_budget(Some(512 * MIB), Some(0)), Some(512 * MIB));
        // A headroom at or above the limit leaves a budget of one byte, not "unlimited".
        assert_eq!(auto_budget(Some(MIB), Some(2 * MIB)), Some(1));
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn cgroup_values_parse() {
        assert_eq!(parse_cgroup_value("536870912\n"), Some(536_870_912));
        assert_eq!(parse_cgroup_value("max\n"), None);
        assert_eq!(parse_cgroup_value("9223372036854771712\n"), None);
        assert_eq!(parse_cgroup_value("garbage"), None);
    }

    #[cfg(not(target_arch = "wasm32"))]
    /// A fresh, empty directory under the system temp dir for one fake cgroup tree.
    fn fake_root(name: &str) -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("graphersal-cgroup-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn cgroup_v2_file_wins() {
        let root = fake_root("v2");
        std::fs::write(root.join("memory.max"), "1073741824\n").unwrap();
        std::fs::create_dir_all(root.join("memory")).unwrap();
        std::fs::write(root.join("memory").join("memory.limit_in_bytes"), "1\n").unwrap();
        assert_eq!(cgroup_limit_at(&root), Some(1_073_741_824));
        std::fs::write(root.join("memory.max"), "max\n").unwrap();
        assert_eq!(cgroup_limit_at(&root), None, "v2 'max' is no limit");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn cgroup_v1_is_the_fallback() {
        let root = fake_root("v1");
        assert_eq!(cgroup_limit_at(&root), None, "no file: no limit");
        std::fs::create_dir_all(root.join("memory")).unwrap();
        std::fs::write(
            root.join("memory").join("memory.limit_in_bytes"),
            "268435456\n",
        )
        .unwrap();
        assert_eq!(cgroup_limit_at(&root), Some(268_435_456));
        std::fs::write(
            root.join("memory").join("memory.limit_in_bytes"),
            "9223372036854771712\n",
        )
        .unwrap();
        assert_eq!(cgroup_limit_at(&root), None, "v1 unlimited sentinel");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Near the ceiling (the hint at least half of it) the estimated check walks the graph
    /// once per commit position: reads reuse the figure, a committed write walks again, a
    /// rolled-back unit does not, and a run inside a unit with uncommitted changes walks without
    /// caching.
    #[test]
    fn estimated_footprint_is_walked_once_per_commit_position_near_the_ceiling() {
        use std::sync::atomic::Ordering;
        let mut graph = crate::TraversalGraph::tinkerpop_modern();
        // A ceiling of 1.5 times the footprint: the hint is above half of it, so every start
        // takes the exact (cached) figure.
        let limit = crate::graph::memory::compute(&graph).total_bytes() * 3 / 2;
        crate::traversal::memory_limit::memory_limit_scope(limit, || {
            let walks =
                |graph: &crate::TraversalGraph| graph.footprint.walks.load(Ordering::Relaxed);
            let read = |graph: &crate::TraversalGraph| {
                let names = graph.traversal().v(None).values("name").to_list().unwrap();
                assert_eq!(names.len(), 6);
            };
            assert_eq!(walks(&graph), 0);
            read(&graph);
            read(&graph);
            assert_eq!(walks(&graph), 1, "two reads, one walk");

            graph.traversal_mut().add_v("extra").to_list().unwrap();
            assert_eq!(walks(&graph), 1, "the write's own start reuses the figure");
            read(&graph);
            assert_eq!(walks(&graph), 2, "a committed write: walked again");
            read(&graph);
            assert_eq!(walks(&graph), 2);

            // A failing unit rolls back: data, counts and commit position are as before.
            let failed: Result<(), GraphError> = graph.transaction(|g| {
                g.insert_vertex(Some("gone".into()), vec!["x".into()], Default::default())?;
                Err(GraphError::Unsupported {
                    operation: "test rollback",
                })
            });
            assert!(failed.is_err());
            read(&graph);
            assert_eq!(walks(&graph), 2, "a rolled-back unit keeps the figure");

            // Inside a unit with uncommitted changes every run walks and nothing is cached.
            let mark = graph.begin_unit().unwrap();
            graph
                .insert_vertex(Some("open".into()), vec!["x".into()], Default::default())
                .unwrap();
            let seq = graph.last_commit_seq();
            graph.traversal_mut().v(None).count().to_list().unwrap();
            graph.traversal_mut().v(None).count().to_list().unwrap();
            assert_eq!(walks(&graph), 4, "uncommitted changes are never cached");
            graph
                .rollback_unit(mark, &crate::graph::RollbackReason::failed(&"test"))
                .unwrap();
            assert_eq!(graph.last_commit_seq(), seq);
            read(&graph);
            assert_eq!(
                walks(&graph),
                4,
                "after the rollback the cached figure applies again"
            );

            // A direct call outside any unit is not counted, but it changes the element counts.
            graph
                .insert_vertex(Some("direct".into()), vec!["x".into()], Default::default())
                .unwrap();
            read(&graph);
            assert_eq!(walks(&graph), 5, "changed element counts: walked again");
        });
    }

    /// Far below the ceiling the estimated check takes the hint: one walk on the first run,
    /// none after a committed write, again once the element count has more than doubled.
    #[test]
    fn far_below_the_ceiling_a_write_does_not_walk_the_graph() {
        use std::sync::atomic::Ordering;
        let mut graph = crate::TraversalGraph::tinkerpop_modern();
        let walks = |graph: &crate::TraversalGraph| graph.footprint.walks.load(Ordering::Relaxed);
        crate::traversal::memory_limit::memory_limit_scope(1 << 40, || {
            let count = |graph: &crate::TraversalGraph| {
                graph.traversal().v(None).count().to_list().unwrap();
            };
            count(&graph);
            assert_eq!(walks(&graph), 1, "the first run walks once");
            for i in 0..10 {
                graph
                    .traversal_mut()
                    .add_v(format!("extra{i}").as_str())
                    .to_list()
                    .unwrap();
                count(&graph);
            }
            assert_eq!(walks(&graph), 1, "committed writes: the hint, no walk");
            let hint = crate::graph::memory::footprint_hint(&graph).unwrap();
            let exact = crate::graph::memory::compute(&graph).total_bytes();
            assert!(hint >= exact / 2 && hint <= exact * 2, "{hint} vs {exact}");
            // More than double the elements (plus 1024): the hint expires, one walk.
            for i in 0..1100 {
                graph
                    .insert_vertex(
                        Some(format!("bulk{i}").into()),
                        vec!["x".into()],
                        Default::default(),
                    )
                    .unwrap();
            }
            count(&graph);
            assert_eq!(walks(&graph), 2, "a doubled graph is walked again");
            count(&graph);
            assert_eq!(walks(&graph), 2);
        });
    }

    #[test]
    fn memory_usage_refreshes_the_footprint_hint() {
        let mut graph = crate::TraversalGraph::tinkerpop_modern();
        assert!(
            crate::graph::memory::footprint_hint(&graph).is_none(),
            "no walk yet"
        );
        let exact = graph.memory_usage().unwrap().unwrap().total_bytes();
        assert_eq!(crate::graph::memory::footprint_hint(&graph), Some(exact));
        // A property rewrite keeps the element counts: the hint does not see it until the
        // next walk, which `memory_usage()` is.
        graph
            .traversal_mut()
            .v(None)
            .has_label("person")
            .property("bio", "x".repeat(100_000))
            .iterate()
            .unwrap();
        assert_eq!(crate::graph::memory::footprint_hint(&graph), Some(exact));
        let grown = graph.memory_usage().unwrap().unwrap().total_bytes();
        assert!(grown > exact + 300_000, "{grown} vs {exact}");
        assert_eq!(crate::graph::memory::footprint_hint(&graph), Some(grown));
    }

    #[test]
    fn ensure_room_compares_resident_plus_needed_against_the_scoped_ceiling() {
        crate::traversal::memory_limit::memory_limit_scope(1000, || {
            assert!(ensure_room("snapshot load", 600, 400).is_ok());
            let err = ensure_room("snapshot load", 601, 400).unwrap_err();
            assert!(matches!(
                err,
                GraphError::ResourceLimitExceeded {
                    operation: "snapshot load",
                    max: Some(1000),
                    observed: Some(1001),
                    needed: Some(601),
                    ..
                }
            ));
            assert!(err.to_string().contains("it needs 601 more bytes"), "{err}");
        });
        // Far below the process ceiling (no scope) a small allocation always fits.
        assert!(ensure_room("snapshot load", 1, 0).is_ok());
    }

    #[test]
    fn a_unit_with_known_element_bytes_counts_them_and_ticks_in_batches() {
        let mut graph = crate::TraversalGraph::new();
        crate::traversal::memory_limit::memory_limit_scope(1 << 30, || {
            let unit = UnitBudget::start_with_element_bytes(&graph, "snapshot load", 10_000)
                .unwrap()
                .unwrap();
            assert_eq!(unit.budget.element_bytes, 10_000);
            let floor = UnitBudget::start_with_element_bytes(&graph, "snapshot load", 1)
                .unwrap()
                .unwrap();
            assert_eq!(floor.budget.element_bytes, MIN_ELEMENT_BYTES);
        });
        let base = graph.memory_usage().unwrap().unwrap().total_bytes();
        crate::traversal::memory_limit::memory_limit_scope(base + 50_000, || {
            let mut unit = UnitBudget::start_with_element_bytes(&graph, "snapshot load", 10_000)
                .unwrap()
                .unwrap();
            for i in 0..6 {
                graph
                    .insert_vertex(
                        Some(format!("v{i}").into()),
                        vec!["x".into()],
                        Default::default(),
                    )
                    .unwrap();
            }
            // Below the countdown: no check yet.
            assert!(unit.tick_n(&graph, 6).is_ok());
            // The end check sees 6 x 10 000 bytes over the room of 50 000.
            let err = unit.check(&graph).unwrap_err();
            assert!(matches!(
                err,
                GraphError::ResourceLimitExceeded {
                    operation: "snapshot load",
                    needed: None,
                    ..
                }
            ));
            // A batch that reaches the countdown checks at once.
            assert!(unit.tick_n(&graph, UNIT_CHECK_INTERVAL as usize).is_err());
        });
    }

    #[test]
    fn a_load_with_an_input_counter_counts_the_bytes_read() {
        use std::io::Read;
        let graph = crate::TraversalGraph::new();
        let base = crate::graph::memory::compute(&graph).total_bytes();
        crate::traversal::memory_limit::memory_limit_scope(base + 1000, || {
            let counter = InputCounter::default();
            let unit = UnitBudget::start(&graph, "GraphSON load")
                .unwrap()
                .unwrap()
                .with_input(counter.clone());
            let text = vec![b'x'; 2000];
            let mut reader = CountingReader::new(text.as_slice(), counter.clone());
            let mut buf = [0u8; 500];
            reader.read_exact(&mut buf).unwrap();
            assert_eq!(counter.get(), 500);
            assert!(unit.check(&graph).is_ok(), "500 bytes read fit");
            let mut rest = Vec::new();
            reader.read_to_end(&mut rest).unwrap();
            assert_eq!(counter.get(), 2000);
            assert!(unit.check(&graph).is_err(), "2000 bytes read do not");
        });
    }

    #[test]
    fn estimated_budget_counts_base_traversers_and_growth() {
        let mut graph = crate::TraversalGraph::tinkerpop_modern();
        let base = graph.memory_usage().unwrap().unwrap().total_bytes();
        let budget = MemoryBudget::new(base + 1000).start(&graph).unwrap();
        assert!(!budget.tracked);
        assert!(budget.check(1000).is_ok());
        let err = budget.check(1001).unwrap_err();
        assert!(matches!(
            *err,
            TraverserError::ResourceLimitExceeded { ref limit, max: Some(max), observed: Some(observed) }
                if limit == MEMORY_LIMIT && max == base + 1000 && observed == base + 1001
        ));
        // The run's held bytes (here: one traverser slot, then a thousand) count on top
        // (`RunMemory` feeds both limits; the per-execution limit is out of the way).
        let slot = std::mem::size_of::<crate::traversal::Traverser<crate::TraversalGraph>>() as u64;
        let run = crate::traversal::run_memory::RunMemory::new(u64::MAX, Some(budget));
        run.charge(slot);
        assert!(run.check_boundary(&graph).is_ok());
        run.charge(999 * slot);
        assert!(run.check_boundary(&graph).is_err());
        run.release(1000 * slot);
        // Added elements count at the average element size.
        let per = budget.element_bytes;
        assert!(per >= MIN_ELEMENT_BYTES);
        let room = 1000 / per + 1;
        for i in 0..room {
            graph
                .insert_vertex(
                    Some(format!("extra{i}").into()),
                    vec!["x".into()],
                    Default::default(),
                )
                .unwrap();
        }
        assert!(run.check_boundary(&graph).is_err());
    }

    /// Regression tests of 0.5.1: no footprint walk per traversal inside an open unit (a
    /// script, a transaction) once the walked figure is out of date (after a mass drop, after
    /// growth past twice the walked element count). Before the fix every run of such a unit
    /// walked the whole graph, uncached because the unit held changes: O(graph) per traversal.
    mod no_walk_per_traversal {
        use std::sync::atomic::Ordering;

        use super::*;

        fn walks(graph: &crate::TraversalGraph) -> u64 {
            graph.footprint.walks.load(Ordering::Relaxed)
        }

        /// `kept` vertices labelled `kept` and `gone` vertices labelled `gone`, each `gone`
        /// vertex with an edge to the first `kept` one, inserted directly (no unit).
        fn thinning_graph(kept: usize, gone: usize) -> crate::TraversalGraph {
            let mut graph = crate::TraversalGraph::new();
            let mut first = None;
            for i in 0..kept {
                let handle = graph
                    .insert_vertex(
                        Some(format!("k{i}").into()),
                        vec!["kept".into()],
                        Default::default(),
                    )
                    .unwrap();
                first.get_or_insert(handle);
            }
            let first = first.unwrap();
            for i in 0..gone {
                let handle = graph
                    .insert_vertex(
                        Some(format!("g{i}").into()),
                        vec!["gone".into()],
                        Default::default(),
                    )
                    .unwrap();
                graph
                    .insert_edge(None, Some("to".into()), handle, first, Default::default())
                    .unwrap();
            }
            graph
        }

        /// Estimated mode, one explicit unit: a mass drop (the element count falls below half
        /// of the walked one), then 2000 writing traversals: at most one walk in all.
        #[test]
        fn writes_after_a_mass_drop_in_one_unit_do_not_walk() {
            let mut graph = thinning_graph(200, 3000);
            crate::traversal::memory_limit::memory_limit_scope(1 << 40, || {
                graph.memory_usage().unwrap();
                let before = walks(&graph);
                graph
                    .transaction(|g| -> Result<(), Box<TraverserError>> {
                        g.traversal_mut()
                            .v(None)
                            .has_label("gone")
                            .drop()
                            .iterate()?;
                        for _ in 0..2000 {
                            g.traversal_mut().v("k0").add_e("self").to("k1").iterate()?;
                        }
                        Ok(())
                    })
                    .unwrap();
                assert_eq!(graph.edge_count(), 2000);
                let walked = walks(&graph) - before;
                assert!(walked <= 1, "{walked} walks for 2000 traversals");
            });
        }

        /// Estimated mode, one unit: the graph grows past twice the walked element count
        /// (plus 1024) through single writing traversals: at most one walk in all.
        #[test]
        fn writes_growing_past_twice_the_walked_count_in_one_unit_do_not_walk() {
            let mut graph = thinning_graph(100, 0);
            crate::traversal::memory_limit::memory_limit_scope(1 << 40, || {
                graph.memory_usage().unwrap();
                let before = walks(&graph);
                graph
                    .transaction(|g| -> Result<(), Box<TraverserError>> {
                        for _ in 0..3000 {
                            g.traversal_mut().add_v("new").iterate()?;
                        }
                        Ok(())
                    })
                    .unwrap();
                assert_eq!(graph.vertex_count(), 3100);
                let walked = walks(&graph) - before;
                assert!(walked <= 1, "{walked} walks for 3000 traversals");
            });
        }

        /// Separate top-level traversals (each its own unit, committed): a walk only where its
        /// figure is cached (between units), never one per traversal.
        #[test]
        fn separate_writes_after_a_mass_drop_walk_at_most_once() {
            let mut graph = thinning_graph(200, 3000);
            crate::traversal::memory_limit::memory_limit_scope(1 << 40, || {
                graph.memory_usage().unwrap();
                let before = walks(&graph);
                graph
                    .traversal_mut()
                    .v(None)
                    .has_label("gone")
                    .drop()
                    .iterate()
                    .unwrap();
                for _ in 0..2000 {
                    graph
                        .traversal_mut()
                        .v("k0")
                        .add_e("self")
                        .to("k1")
                        .iterate()
                        .unwrap();
                }
                // One walk after the drop, one more once the 2000 new edges have more than
                // doubled the walked count (plus 1024): both outside any open unit, so cached
                // (amortized O(log n) walks), never one per traversal.
                let walked = walks(&graph) - before;
                assert!(walked <= 2, "{walked} walks for 2000 traversals");
            });
        }

        /// The tracked measure (switched on by hand: the test binary installs no tracking
        /// allocator) of 2000 writing runs inside one unit after a mass drop: the graph's
        /// growth is estimated without a walk, whether the hint is stale or missing.
        #[test]
        fn tracked_growth_never_walks() {
            for walked_before in [true, false] {
                let mut graph = thinning_graph(200, 3000);
                if walked_before {
                    graph.memory_usage().unwrap();
                }
                let mark = graph.begin_unit().unwrap();
                // Counted from here: `begin_unit` takes the ESTIMATED starting point of the
                // process ceiling (this binary has no tracking allocator), which a tracked
                // process does not.
                let before = walks(&graph);
                if !walked_before {
                    // No hint at all: forget the walk `begin_unit` just made.
                    graph.footprint.forget();
                }
                // Dropped by direct calls: a traversal would take an (estimated) starting
                // point of its own.
                for i in 0..3000 {
                    let gone = GraphStorage::get_vertex_by_id(&graph, &format!("g{i}"))
                        .unwrap()
                        .unwrap();
                    GraphStorage::drop_vertex(&mut graph, gone).unwrap();
                }
                for i in 0..2000 {
                    let mut run = crate::traversal::run_memory::RunMemory::new(u64::MAX, None);
                    run.force_tracked_for_test(&graph);
                    run.set_writes(true);
                    graph
                        .insert_vertex(
                            Some(format!("n{i}").into()),
                            vec!["new".into()],
                            Default::default(),
                        )
                        .unwrap();
                    run.check_boundary(&graph).unwrap();
                    assert!(run.grown_for_test() > 0, "the growth is still estimated");
                }
                graph.commit_unit(mark).unwrap();
                assert_eq!(walks(&graph), before, "walked before: {walked_before}");
            }
        }

        /// Scripts: a mass drop then 2000 writing traversals, as ONE unit
        /// (`eval_value_atomic`) and as one unit per statement (`eval_value_with_limits`).
        #[cfg(feature = "script")]
        #[test]
        fn scripts_writing_after_a_mass_drop_do_not_walk_per_traversal() {
            use crate::script::{ScriptLimits, eval_value_atomic, eval_value_with_limits};
            use std::sync::Arc;
            const SCRIPT: &str = r#"
                g.V().hasLabel("gone").drop().iterate();
                for i in 0..2000 { g.V("k0").addE("self").to(__.V("k1")).iterate(); }
            "#;
            for atomic in [true, false] {
                let graph = Arc::new(crate::graph::Graph::new(thinning_graph(200, 3000)));
                crate::traversal::memory_limit::memory_limit_scope(1 << 40, || {
                    graph.read().memory_usage().unwrap();
                    let before = walks(&graph.read());
                    let limits = ScriptLimits::default();
                    let result = if atomic {
                        eval_value_atomic(
                            graph.clone(),
                            SCRIPT,
                            Default::default(),
                            &limits,
                            Arc::new(crate::graph::AllowAll),
                        )
                    } else {
                        eval_value_with_limits(
                            graph.clone(),
                            SCRIPT,
                            Default::default(),
                            &limits,
                            Arc::new(crate::graph::AllowAll),
                        )
                    };
                    result.unwrap();
                    let graph = graph.read();
                    assert_eq!(graph.edge_count(), 2000);
                    // One unit: no walk at all. One unit per statement: walks only between
                    // units, where they are cached (after the drop, once the new edges have
                    // doubled the count): amortized, never one per traversal.
                    let walked = walks(&graph) - before;
                    assert!(
                        walked <= if atomic { 1 } else { 2 },
                        "atomic {atomic}: {walked} walks for 2000 traversals"
                    );
                });
            }
        }
    }
}
