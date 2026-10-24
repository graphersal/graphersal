//! The memory budget (`memory.limit`): a host-level ceiling on the memory a traversal may leave
//! the process at, so an oversized query fails with
//! [`TraverserError::ResourceLimitExceeded`] (and its unit rolls back) instead of the process
//! being killed by the operating system (a Kubernetes/cgroup OOM kill).
//!
//! **Off by default.** A host opts in with an explicit byte value, or with `"auto"`: the cgroup
//! memory limit of the process (cgroup v2 `memory.max`, else v1 `memory.limit_in_bytes`; `max` or
//! no file means no limit) minus a headroom (`memory.headroom`, default
//! [`default_headroom`]). `"auto"` never limits on `wasm32` (there is no cgroup); an explicit value
//! works there too.
//!
//! A Rust allocator cannot fail softly, so the budget is cooperative: it is compared at the
//! places that already check `traversal.max_materialized_bytes` (a bulk expansion before it
//! allocates, a bucket push of copies, every `repeat()` iteration) and at every step boundary.
//! What is compared:
//! - **tracked** (a [`TrackingAllocator`](crate::alloc_tracking::TrackingAllocator) is installed
//!   and the `alloc-tracking` feature is on): the process's exact live heap bytes
//!   (`alloc_tracking::global_live_bytes`, one relaxed atomic load) plus the estimated size of
//!   what a check is about to allocate;
//! - **estimated** otherwise: the graph footprint taken ONCE when the run starts
//!   ([`GraphStorage::memory_footprint_bytes`], a walk of the graph that `TraversalGraph` caches
//!   by its commit position, so a run on an unchanged graph does not walk it again), plus the
//!   elements the run added since
//!   (at the graph's average element size), plus the traversers alive at a step boundary, plus
//!   the estimated size of what a check is about to allocate.
//!
//! Every check is a few loads and one comparison; nothing walks the graph per check.
//!
//! Units that are not traversals ([`UnitBudget`]: a GraphML/GraphSON import into a live graph,
//! `apply_change_set`) take their budget from the storage's execution policy (plus a script
//! session's presets for a script import) and check it every [`UNIT_CHECK_INTERVAL`] elements
//! and at their end; `TraversalGraph` checks every outermost unit at its commit
//! ([`MemoryBudget::unit_check`]). Only a unit that grew the measure fails, with
//! [`GraphError::ResourceLimitExceeded`] naming the operation; the caller rolls it back.

use std::collections::HashMap;

use compact_str::CompactString;

use crate::error::{GraphError, TraverserError, TraverserResult};
use crate::graph::{ExecutionPolicy, GraphStorage};
use crate::value::ElementProperty;

/// Option key of the memory budget, in bytes, or `"auto"` (see the module documentation). Not
/// set by default: no budget.
pub(crate) const MEMORY_LIMIT: &str = "memory.limit";

/// Option key of the headroom `"auto"` keeps below the cgroup limit, in bytes (a non-negative
/// integer). Ignored for an explicit `memory.limit`. Default: [`default_headroom`].
pub(crate) const MEMORY_HEADROOM: &str = "memory.headroom";

/// The `memory.limit` value that derives the budget from the cgroup limit.
pub(crate) const MEMORY_LIMIT_AUTO: &str = "auto";

/// The smallest default headroom: 64 MiB for the stack, the code, allocator overhead and what the
/// engine does not count.
pub(crate) const MIN_DEFAULT_HEADROOM: u64 = 64 * 1024 * 1024;

/// The cgroup v1 "no limit" value is `i64::MAX` rounded down to a page; anything this large is
/// no limit.
#[cfg(not(target_arch = "wasm32"))]
const CGROUP_UNLIMITED_FROM: u64 = 1 << 62;

/// The smallest size the estimate assumes for one element a run adds (an empty graph has no
/// average to take): a topology slot, a handle generation and an index entry.
const MIN_ELEMENT_BYTES: u64 = 128;

/// The default headroom `"auto"` keeps below a cgroup limit of `limit` bytes: a tenth of it, at
/// least [`MIN_DEFAULT_HEADROOM`], but never more than half of the limit.
pub(crate) fn default_headroom(limit: u64) -> u64 {
    (limit / 10).max(MIN_DEFAULT_HEADROOM).min(limit / 2)
}

/// The budget `"auto"` yields for a cgroup limit `cgroup` (`None`: no limit, so no budget) and an
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
fn cgroup_limit() -> Option<u64> {
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

/// The budget `memory.limit = "auto"` resolves to in this process: [`auto_budget`] of the
/// process's cgroup limit.
pub(crate) fn resolve_auto(headroom: Option<u64>) -> Option<u64> {
    auto_budget(cgroup_limit(), headroom)
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
    /// live counter already includes the graph), else the graph's footprint
    /// ([`GraphStorage::memory_footprint_bytes`]: at most ONE walk, none when the graph has not
    /// changed since the last run; a storage without introspection counts as 0 bytes) and its
    /// element count.
    pub(crate) fn start<S: GraphStorage + ?Sized>(self, storage: &S) -> Self {
        if let Some(live) = tracked_live_bytes() {
            return Self {
                tracked: true,
                live_at_start: live,
                ..self
            };
        }
        let base = storage.memory_footprint_bytes().unwrap_or(0);
        let elements = (storage.vertex_count() as u64).saturating_add(storage.edge_count() as u64);
        let element_bytes = base
            .checked_div(elements)
            .unwrap_or(0)
            .max(MIN_ELEMENT_BYTES);
        Self {
            tracked: false,
            base,
            elements_at_start: elements,
            element_bytes,
            ..self
        }
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

    /// The check at a step boundary, with `alive` traversers between the steps. Estimated: the
    /// traversers plus the elements the run added to `storage` since it started (at the
    /// average element size) count on top of the starting footprint. Tracked: the live heap
    /// already holds both.
    #[inline]
    pub(crate) fn check_boundary<S: GraphStorage>(
        &self,
        storage: &S,
        alive: usize,
    ) -> TraverserResult<()> {
        if self.tracked {
            return self.check(0);
        }
        let traversers = (alive as u64).saturating_mul(std::mem::size_of::<
            crate::traversal::traverser::Traverser<S>,
        >() as u64);
        self.check(traversers.saturating_add(self.grown_bytes(storage)))
    }

    /// Estimated: the elements added to `storage` since the start (net), at the average
    /// element size. Tracked: 0 (the live heap holds them).
    #[inline]
    fn grown_bytes<S: GraphStorage + ?Sized>(&self, storage: &S) -> u64 {
        if self.tracked {
            return 0;
        }
        let elements = (storage.vertex_count() as u64).saturating_add(storage.edge_count() as u64);
        elements
            .saturating_sub(self.elements_at_start)
            .saturating_mul(self.element_bytes)
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
        let (total, grew) = if self.tracked {
            let live = tracked_live_bytes().unwrap_or(0);
            (live, live > self.live_at_start)
        } else {
            let grown = self.grown_bytes(storage);
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

/// The memory budget of a unit that is not a traversal: a GraphML or GraphSON import into a
/// live graph, an applied change set. Built only when a budget is configured
/// ([`Self::start`]: the storage's [`ExecutionPolicy`] default of `memory.limit`, or a host's
/// preset options); without one nothing is paid, neither a walk nor a check.
#[derive(Debug, Clone, Copy)]
pub(crate) struct UnitBudget {
    budget: MemoryBudget,
    /// What the error names: `"GraphML import"`, `"change set apply"`, ...
    operation: &'static str,
    /// Elements left until the next check.
    countdown: u32,
}

impl UnitBudget {
    /// The budget of a unit `operation` on `storage`: `memory.limit` resolved from `presets`
    /// (a host's preset options, such as a session's; they win over the policy unless it locks
    /// the key) and the storage's execution policy, started on `storage` as it is now (at most
    /// one walk of the graph, none when its footprint is cached). `None` when no budget is
    /// configured, or the configuration is invalid (a traversal on the same graph reports that
    /// as `TraverserError::InvalidOption`; the unit is not stopped for it).
    pub(crate) fn start<S: GraphStorage + ?Sized>(
        storage: &S,
        presets: Option<&HashMap<CompactString, ElementProperty>>,
        operation: &'static str,
    ) -> Option<Self> {
        let limit = unit_limit(&storage.execution_policy(), presets)?;
        Some(Self {
            budget: MemoryBudget::new(limit).start(storage),
            operation,
            countdown: UNIT_CHECK_INTERVAL,
        })
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

    /// Checks the budget now ([`MemoryBudget::unit_check`]): the end of the unit, before its
    /// commit.
    pub(crate) fn check<S: GraphStorage + ?Sized>(&self, storage: &S) -> Result<(), GraphError> {
        self.budget.unit_check(storage, self.operation)
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

/// The budget in bytes a unit outside a traversal runs under: `memory.limit` of `presets` and
/// `policy` ([`crate::traversal::context::resolve_memory_limit`]); `None` without one. Costs a
/// lookup in the policy's (usually empty) defaults when nothing is configured. An invalid
/// configuration is logged and counts as none.
pub(crate) fn unit_limit(
    policy: &ExecutionPolicy,
    presets: Option<&HashMap<CompactString, ElementProperty>>,
) -> Option<u64> {
    let configured = |map: &HashMap<CompactString, ElementProperty>| map.contains_key(MEMORY_LIMIT);
    if !configured(&policy.defaults) && !presets.is_some_and(configured) {
        return None;
    }
    let empty = HashMap::new();
    match crate::traversal::context::resolve_memory_limit(presets.unwrap_or(&empty), policy) {
        Ok(limit) => limit,
        Err(err) => {
            tracing::warn!(%err, "invalid memory budget configuration; the unit runs without one");
            None
        }
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
    }
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

    /// The estimated budget walks the graph once per commit position: reads reuse the figure,
    /// a committed write walks again, a rolled-back unit does not, and a run inside a unit with
    /// uncommitted changes walks without caching.
    #[test]
    fn estimated_footprint_is_walked_once_per_commit_position() {
        use std::sync::atomic::Ordering;
        const LIMIT: i64 = 1 << 40;
        let mut graph = crate::TraversalGraph::tinkerpop_modern();
        let walks = |graph: &crate::TraversalGraph| graph.footprint.walks.load(Ordering::Relaxed);
        let read = |graph: &crate::TraversalGraph| {
            let names = graph
                .traversal()
                .with(MEMORY_LIMIT, LIMIT)
                .v(None)
                .values("name")
                .to_list()
                .unwrap();
            assert_eq!(names.len(), 6);
        };
        assert_eq!(walks(&graph), 0);
        read(&graph);
        read(&graph);
        assert_eq!(walks(&graph), 1, "two reads, one walk");

        graph
            .traversal_mut()
            .with(MEMORY_LIMIT, LIMIT)
            .add_v("extra")
            .to_list()
            .unwrap();
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
        let mark = graph.begin_unit();
        graph
            .insert_vertex(Some("open".into()), vec!["x".into()], Default::default())
            .unwrap();
        let seq = graph.last_commit_seq();
        graph
            .traversal_mut()
            .with(MEMORY_LIMIT, LIMIT)
            .v(None)
            .count()
            .to_list()
            .unwrap();
        graph
            .traversal_mut()
            .with(MEMORY_LIMIT, LIMIT)
            .v(None)
            .count()
            .to_list()
            .unwrap();
        assert_eq!(walks(&graph), 4, "uncommitted changes are never cached");
        graph.rollback_unit(mark, &crate::graph::RollbackReason::failed(&"test"));
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
    }

    #[test]
    fn estimated_budget_counts_base_traversers_and_growth() {
        let mut graph = crate::TraversalGraph::tinkerpop_modern();
        let base = graph.memory_usage().unwrap().total_bytes();
        let budget = MemoryBudget::new(base + 1000).start(&graph);
        assert!(!budget.tracked);
        assert!(budget.check(1000).is_ok());
        let err = budget.check(1001).unwrap_err();
        assert!(matches!(
            *err,
            TraverserError::ResourceLimitExceeded { ref limit, max: Some(max), observed: Some(observed) }
                if limit == MEMORY_LIMIT && max == base + 1000 && observed == base + 1001
        ));
        assert!(budget.check_boundary(&graph, 1).is_ok());
        assert!(budget.check_boundary(&graph, 1000).is_err());
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
        assert!(budget.check_boundary(&graph, 0).is_err());
    }
}
