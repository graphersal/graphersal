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
//!   added since (at the graph's average element size), plus the traversers alive at a step
//!   boundary, plus the estimated size of what a check is about to allocate.
//!
//! **No walk of the graph per run** (the ceiling is on by default, so a run must not pay for
//! it): the estimated start takes the storage's cheap
//! [`GraphStorage::memory_footprint_hint`] (for `TraversalGraph` the last walked figure,
//! adjusted by the elements added or removed since, at the average element size) and walks the
//! graph ([`GraphStorage::memory_footprint_bytes`], cached by the commit position) only when
//! there is no hint yet (the first run on a graph) or the hint is at least half of the ceiling,
//! where precision matters. Every check is a few loads and one comparison.
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
const MIN_ELEMENT_BYTES: u64 = 128;

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
            base,
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
        self.check(traversers.saturating_add(self.grown_bytes(storage)?))
    }

    /// Estimated: the elements added to `storage` since the start (net), at the average
    /// element size. Tracked: 0 (the live heap holds them).
    #[inline]
    fn grown_bytes<S: GraphStorage + ?Sized>(&self, storage: &S) -> Result<u64, GraphError> {
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
        let (total, grew) = if self.tracked {
            let live = tracked_live_bytes().unwrap_or(0);
            (live, live > self.live_at_start)
        } else {
            let grown = self.grown_bytes(storage)?;
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
#[derive(Debug, Clone, Copy)]
pub(crate) struct UnitBudget {
    budget: MemoryBudget,
    /// What the error names: `"GraphML import"`, `"change set apply"`, ...
    operation: &'static str,
    /// Elements left until the next check.
    countdown: u32,
}

impl UnitBudget {
    /// The ceiling of a unit `operation` on `storage` (the process ceiling, lowered by a running
    /// job's memory limit), started on `storage` as it is now (no walk of the graph far below the
    /// ceiling, see [`MemoryBudget::start`]). `None` when there is no ceiling. `Err`: the storage
    /// cannot measure itself (the unit fails before it starts).
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
        }))
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
