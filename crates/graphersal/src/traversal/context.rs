use bitmask_enum::bitmask;
use compact_str::CompactString;
use fnv::FnvBuildHasher;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use web_time::Instant;

use crate::TraverserValue;
use crate::error::{TraverserError, TraverserResult};
use crate::graph::{ExecutionPolicy, GraphStorage};
use crate::traversal::memory_budget::{
    MEMORY_HEADROOM, MEMORY_LIMIT, MEMORY_LIMIT_AUTO, MemoryBudget,
};
use crate::traversal::optimizer::path_analysis::PathMode;
use crate::traversal::side_effect::SideEffectValue;
use crate::traversal::step::ProfileContext;
use crate::traversal::step::flatbarrier_steps::step_dedup::DedupKey;
use crate::value::{ElementProperty, PathArena, PathRef};

/// Execution switches of a traversal, combined as bit flags.
///
/// `#[bitmask]` expands this into a flags struct, not a Rust enum, so `#[non_exhaustive]` does not
/// apply: adding a flag is already an additive change.
#[bitmask(u32)]
pub(crate) enum TraversalStrategy {
    /// Collect step timings (and, with `alloc-tracking`, memory) for `.profile()`.
    ProfileEnabled,
    /// Capture the query as written, so a failing step can be reported against it rather than
    /// against the optimized plan alone. Enabled by default; see
    /// [`GraphTraversalSource::without_diagnostics`](crate::GraphTraversalSource::without_diagnostics).
    Diagnostics,
}

/// Opt-in categories of data `.profile_with(flags)` collects and renders, on top of the timing
/// figures a bare `.profile()` always reports (`.profile()` is equivalent to
/// `.profile_with(ProfileType::none())`).
///
/// A `bitmask_enum` (like the internal traversal strategies), so combining categories (once more
/// than one exists) works the same way: `ProfileType::Memory | ProfileType::Network`. Only `Memory` is
/// implemented today; a future `Network`/`Disk` category is a mechanical, additive change to this
/// enum and to the executor's gating (`source.rs`'s `profile_memory` check), not a redesign.
/// `#[bitmask]` expands this into a flags struct, not a Rust enum, so `#[non_exhaustive]` does not
/// apply.
#[bitmask(u32)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ProfileType {
    /// Collect and render per-step memory figures (`Mem`/`Retained`/`Allocs` columns). Off by
    /// default — `.profile()` with no argument, or `.profile_with(ProfileType::none())`, never
    /// collects memory data, matching this crate's "pay only when asked" convention. When set,
    /// gates both the collection (`AllocSnapshot::capture`/`step_memory_metrics` in `source.rs`)
    /// and the rendering (the `Mem`/`Retained`/`Allocs` columns in `.profile()`'s `Display` impl,
    /// `traversal/step/profile.rs`).
    Memory,
}

impl Default for ProfileType {
    /// No categories requested — the same state `.profile()` (no argument) leaves execution in.
    fn default() -> Self {
        Self::none()
    }
}

pub(crate) struct TraverserContext<'a, S: GraphStorage> {
    pub(crate) storage: &'a mut S,
    pub(crate) strategy: TraversalStrategy,
    pub(crate) side_effect: HashMap<CompactString, SideEffectValue<S>>,
    pub(crate) profile: ProfileContext,
    /// Set once when the terminal method builds this context: `ProfileType::none()` unless
    /// `.profile_with(flags)` was called. Gates whether memory figures are collected at all (D2).
    pub(crate) profile_type: ProfileType,
    /// Path nodes of this execution, shared by every nested traversal that runs on the context.
    pub(crate) paths: PathArena<S>,
    /// Sack values of this execution, shared by nested traversals (empty without `withSack`).
    pub(crate) sacks: crate::value::SackArena,
    /// The sack every generated traverser starts with (`SackRef::NONE` without `withSack`).
    pub(crate) initial_sack: crate::value::SackRef,
    /// What the step being executed must record into the paths of its outputs.
    ///
    /// Set by the executor from the path requirement analysis before a step runs.
    pub(crate) path_mode: PathMode,
    /// Reusable buffer for `simple_path()`/`cyclic_path()`, so the check does not allocate per
    /// traverser.
    pub(crate) scratch_path: Vec<(u64, PathRef)>,
    /// Reusable DFS stack and visited sets of `glob_path()`, so its walk does not allocate per
    /// visited vertex or per incoming traverser.
    pub(crate) glob_path:
        crate::traversal::step::flatmap_steps::step_glob_path::GlobPathScratch<S::VertexId>,
    /// Execution options of this run, parsed once from the source options. `evaluationTimeout`
    /// is consumed into `deadline` at creation; `repeat()` reads its order and loop limit here.
    pub(crate) options: ExecutionOptions,
    /// Wall-clock budget of this execution, shared by every nested traversal that runs on the
    /// context. `None` when no `evaluationTimeout` is set.
    pub(crate) deadline: Option<Deadline>,
    /// The traversers (with bulk) that reached each top-level step, by its position in the
    /// optimized plan; `Some` only when the run records workload statistics (the property
    /// statistics' item counts).
    pub(crate) step_inputs: Option<Vec<u64>>,
    /// One frame per `repeat()` being executed, innermost last. `loops()` reads the counters
    /// here, so traversers carry no loop state.
    pub(crate) loop_stack: Vec<LoopFrame<S>>,
    /// Whether a stateful step (`dedup()`) runs in the persistent scope of the innermost
    /// `repeat()` frame: `true` while a loop body runs, `false` inside a per-traverser child
    /// (`where`, `by(traversal)`, `local`, `until`/`emit` conditions), which TinkerPop resets for
    /// every start traverser.
    pub(crate) loop_persist: bool,
    /// Whether merge points may merge equal traversers in this execution: the `bulk.merge`
    /// option, switched off when the plan mutates the graph.
    pub(crate) bulk_merge: bool,
    /// Whether a merge actually merged something. Invariant: no traverser has a bulk above 1
    /// unless this is `true`, so count and expansion fast paths may use `len()` while it is
    /// `false`.
    pub(crate) bulk_active: bool,
    /// The merge operator of `withSack(initial, Operator)`: merging combines the sacks with it.
    pub(crate) sack_merge: Option<crate::value::Operator>,
    /// `bulk.one` (`withBulk(false)`): a merge keeps bulk 1.
    pub(crate) bulk_one: bool,
    /// Whether the automatic merge points (`repeat` frontier, `lazy_barrier`) merge. Off with a
    /// sack merge operator or `bulk.one`, where merging is visible and only an explicit
    /// `barrier()` merges.
    pub(crate) auto_merge: bool,
    /// The random number generator of this execution, shared by every step that draws
    /// (`coin()`, `sample()`, `order().by(Order.shuffle)`) and by every child traversal. Created
    /// on the first draw: seeded from `random.seed` when the option is set, from the operating
    /// system's entropy otherwise. See [`Self::rng`].
    rng: Option<rand::rngs::StdRng>,
    /// The time of `defaultFunction` `now`/`today` in this execution: pinned by
    /// `GraphTraversalSource::with_now`, else taken on the first create
    /// ([`Self::create_context`]).
    pub(crate) create_context: Option<crate::graph::schema::defaults::CreateContext>,
    /// What the GQL `DELETE` steps of this execution removed (feature `gql`): the labels of
    /// the deleted edges for `type(r)` (E3 of stage 3d) and whether anything was deleted (the
    /// returned-element check of a `RETURN`, W28).
    #[cfg(feature = "gql")]
    pub(crate) gql_deleted: crate::gql::runtime::GqlDeleted<S>,
    /// The instant the GQL clocks of this execution read (`date()`, `datetime()`,
    /// `X.statement()`, stage 3g U9; feature `gql`): taken on the first read from
    /// [`Self::create_context`] when set, else from the storage's open unit or the system
    /// clock, then the same for every row and every subquery of the execution.
    #[cfg(feature = "gql")]
    pub(crate) gql_now: std::cell::OnceCell<crate::value::temporal::DateTime>,
}

/// The loop counter of one active `repeat()`.
///
/// Every traverser of a breadth-first frontier shares the same counter, and depth-first order
/// sets it before each evaluation, so one frame per loop is exact.
pub(crate) struct LoopFrame<S: GraphStorage> {
    /// The loop name given with `repeat(name, body)`.
    pub(crate) name: Option<Arc<str>>,
    /// Completed iterations of the traverser being evaluated (TinkerPop `loops()`).
    pub(crate) loops: u32,
    /// The seen-sets of the `dedup()` steps of the body, keyed by the address of the step
    /// instance (the body is borrowed for the whole run, so the address is stable, and the map
    /// dies with the frame). TinkerPop's `dedup()` in a loop body is one step instance whose
    /// set lives as long as the loop.
    pub(crate) dedup_seen: HashMap<usize, HashSet<DedupKey<S>, FnvBuildHasher>>,
}

impl<S: GraphStorage> LoopFrame<S> {
    /// A frame for a loop that has not started iterating.
    pub(crate) fn new(name: Option<Arc<str>>) -> Self {
        Self {
            name,
            loops: 0,
            dedup_seen: HashMap::new(),
        }
    }
}

impl<S: GraphStorage> std::fmt::Debug for LoopFrame<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoopFrame")
            .field("name", &self.name)
            .field("loops", &self.loops)
            .field("dedup_sets", &self.dedup_seen.len())
            .finish()
    }
}

impl<'a, S: GraphStorage> TraverserContext<'a, S> {
    /// Runs `run` with `loop_persist` set to `persist`, restoring the previous value afterwards
    /// (also when `run` fails). A per-traverser child traversal passes `false`: its stateful
    /// steps start from scratch for every start traverser, like TinkerPop's reset children.
    #[inline]
    pub(crate) fn with_persist<R>(&mut self, persist: bool, run: impl FnOnce(&mut Self) -> R) -> R {
        let outer = std::mem::replace(&mut self.loop_persist, persist);
        let result = run(self);
        self.loop_persist = outer;
        result
    }

    /// Creates the context of one execution, with an empty path arena.
    pub(crate) fn new(
        storage: &'a mut S,
        strategy: TraversalStrategy,
        side_effect: HashMap<CompactString, SideEffectValue<S>>,
    ) -> Self {
        Self {
            storage,
            strategy,
            side_effect,
            profile: ProfileContext::new(),
            profile_type: ProfileType::none(),
            paths: PathArena::new(),
            sacks: crate::value::SackArena::new(),
            initial_sack: crate::value::SackRef::NONE,
            path_mode: PathMode::None,
            scratch_path: Vec::new(),
            glob_path: Default::default(),
            options: ExecutionOptions::default(),
            deadline: None,
            step_inputs: None,
            loop_stack: Vec::new(),
            loop_persist: false,
            bulk_merge: false,
            bulk_active: false,
            sack_merge: None,
            bulk_one: false,
            auto_merge: false,
            rng: None,
            create_context: None,
            #[cfg(feature = "gql")]
            gql_deleted: crate::gql::runtime::GqlDeleted::default(),
            #[cfg(feature = "gql")]
            gql_now: std::cell::OnceCell::new(),
        }
    }

    /// The [`CreateContext`](crate::graph::schema::defaults::CreateContext) of the elements this
    /// execution creates: pinned, else the storage's unit start (or the system clock), taken
    /// once, so every element of one execution gets the same time.
    pub(crate) fn create_context(&mut self) -> crate::graph::schema::defaults::CreateContext {
        let storage = &*self.storage;
        *self.create_context.get_or_insert_with(|| {
            crate::graph::schema::defaults::CreateContext::for_storage(storage)
        })
    }

    /// The [`CreateContext`](crate::graph::schema::defaults::CreateContext) of a creating step:
    /// the step's pinned time (`pinned`, the script DSL's evaluation `now()`), else
    /// [`Self::create_context`].
    pub(crate) fn create_context_or(
        &mut self,
        pinned: Option<crate::value::temporal::DateTime>,
    ) -> crate::graph::schema::defaults::CreateContext {
        match pinned {
            Some(now) => crate::graph::schema::defaults::CreateContext::at(now),
            None => self.create_context(),
        }
    }

    /// Creates the context of one execution governed by `options`, starting the
    /// `evaluationTimeout` clock now.
    pub(crate) fn with_options(
        storage: &'a mut S,
        strategy: TraversalStrategy,
        side_effect: HashMap<CompactString, SideEffectValue<S>>,
        options: ExecutionOptions,
    ) -> Self {
        let mut context = Self::new(storage, strategy, side_effect);
        context.deadline = Deadline::start(
            options.evaluation_timeout,
            crate::traversal::cancel::CancelToken::current(),
        );
        context.options = options;
        context
    }

    /// The random number generator of this execution (see the `rng` field). With
    /// `g.with("random.seed", n)` the draws, and so the results of `coin()`, `sample()` and
    /// `Order.shuffle`, are the same on every run of the same plan (for one Graphersal version):
    /// the steps draw in plan order, child traversals included.
    pub(crate) fn rng(&mut self) -> &mut rand::rngs::StdRng {
        use rand::SeedableRng;
        let seed = self.options.random_seed;
        self.rng.get_or_insert_with(|| match seed {
            Some(seed) => rand::rngs::StdRng::seed_from_u64(seed),
            None => rand::rngs::StdRng::from_entropy(),
        })
    }

    /// Fails with [`TraverserError::Timeout`] once the `evaluationTimeout` budget is spent.
    ///
    /// Reads the clock on every call; per-traverser sites use [`tick`](Self::tick) instead.
    #[inline]
    pub(crate) fn check_deadline(&self) -> TraverserResult<()> {
        match &self.deadline {
            None => Ok(()),
            Some(deadline) => deadline.check(),
        }
    }

    /// Fails with [`TraverserError::ResourceLimitExceeded`] when a step left more than
    /// `traversal.max_traversers` traversers alive. One comparison per step boundary.
    #[inline]
    pub(crate) fn check_traverser_limit(&self, alive: usize) -> TraverserResult<()> {
        let alive = alive as u64;
        if alive > self.options.max_traversers {
            return Err(Box::new(TraverserError::ResourceLimitExceeded {
                limit: MAX_TRAVERSERS.to_string(),
                max: Some(self.options.max_traversers),
                observed: Some(alive),
            }));
        }
        Ok(())
    }

    /// Fails with [`TraverserError::ResourceLimitExceeded`] (`memory.limit`) when the run is over
    /// its memory budget at a step boundary with `alive` traversers between the steps. Without a
    /// budget (the default) one branch.
    #[inline]
    pub(crate) fn check_memory_budget(&self, alive: usize) -> TraverserResult<()> {
        match &self.options.memory {
            None => Ok(()),
            Some(budget) => budget.check_boundary(&*self.storage, alive),
        }
    }

    /// Fails with [`TraverserError::ResourceLimitExceeded`] when `value` nests deeper than
    /// `traversal.max_value_depth`. An element handle or a scalar costs one `match`.
    #[inline]
    pub(crate) fn check_value_depth(&self, value: &TraverserValue<S>) -> TraverserResult<()> {
        if crate::value::depth::traverser_value_exceeds_depth(value, self.options.max_value_depth) {
            return Err(self.value_depth_error());
        }
        Ok(())
    }

    /// [`check_value_depth`](Self::check_value_depth) for a traverser's value and its sack: what a
    /// loop body can wrap again on every iteration (`repeat(__.fold())`, `repeat(__.path())`, a
    /// sack folded into itself), checked once per traverser per `repeat()` iteration.
    #[inline]
    pub(crate) fn check_traverser_depth(
        &self,
        traverser: &crate::Traverser<S>,
    ) -> TraverserResult<()> {
        self.check_value_depth(&traverser.value)?;
        if !traverser.sack.is_none() {
            self.check_property_depth(self.sacks.get(traverser.sack))?;
        }
        Ok(())
    }

    /// What `repeat()` checks on every traverser after every iteration: the depth
    /// ([`check_traverser_depth`](Self::check_traverser_depth)) and the estimated size of the
    /// value, the sack and the values the execution's path records hold
    /// ([`PathArena::value_bytes`]) against `traversal.max_materialized_bytes`, so a body that
    /// doubles its value (`repeat(__.path())`, `repeat(__.group())`) stops at the byte budget,
    /// at most one doubling past it. The same estimate counts against the memory budget
    /// (`memory.limit`) when one is set. A scalar or element handle costs a `match`; a container
    /// is walked once, as the depth check already does.
    #[inline]
    pub(crate) fn check_loop_traverser(
        &self,
        traverser: &crate::Traverser<S>,
    ) -> TraverserResult<()> {
        self.check_traverser_depth(traverser)?;
        let max = self.options.max_materialized_bytes;
        let mut bytes =
            crate::traversal::step::profile::estimate_traverser_value_bytes(&traverser.value)
                .saturating_add(self.paths.value_bytes());
        if !traverser.sack.is_none() {
            bytes = bytes.saturating_add(self.sacks.get(traverser.sack).estimated_heap_bytes());
        }
        if bytes > max {
            return Err(crate::traversal::bulk::materialized_bytes_error(max, bytes));
        }
        if let Some(budget) = &self.options.memory {
            budget.check(bytes)?;
        }
        Ok(())
    }

    /// Fails with [`TraverserError::ResourceLimitExceeded`] (`traversal.max_string_bytes`) when
    /// a string of `len` bytes would be longer than the limit. One comparison; string-building
    /// steps call it with the length of their result BEFORE they build it where the result can
    /// outgrow the input (`replace()`, `concat()`, `conjoin()`, `format()`), after it otherwise.
    #[inline]
    pub(crate) fn check_string_bytes(&self, len: usize) -> TraverserResult<()> {
        check_string_bytes(len, self.options.max_string_bytes)
    }

    /// [`check_value_depth`](Self::check_value_depth) for a stored or written property value.
    #[inline]
    pub(crate) fn check_property_depth(&self, value: &ElementProperty) -> TraverserResult<()> {
        if crate::value::depth::property_exceeds_depth(value, self.options.max_value_depth) {
            return Err(self.value_depth_error());
        }
        Ok(())
    }

    /// Checks the depth a property write leaves behind: writing `value` at `key` nests it one
    /// level per path segment below the stored property (`jpath("a.b.c")`: two levels), so the
    /// resulting property must stay within `traversal.max_value_depth`. The rest of the stored
    /// value was checked when it was written.
    #[inline]
    pub(crate) fn check_written_depth(
        &self,
        key: &crate::value::PropKey,
        value: &ElementProperty,
    ) -> TraverserResult<()> {
        let below = match key {
            crate::value::PropKey::Name(_) => 0,
            crate::value::PropKey::Path(path) => path.segments().len().saturating_sub(1),
        };
        let max = self.options.max_value_depth;
        if below > max || crate::value::depth::property_exceeds_depth(value, max - below) {
            return Err(self.value_depth_error());
        }
        Ok(())
    }

    /// The error of a value deeper than `traversal.max_value_depth`.
    #[cold]
    pub(crate) fn value_depth_error(&self) -> Box<TraverserError> {
        Box::new(TraverserError::ResourceLimitExceeded {
            limit: MAX_VALUE_DEPTH.to_string(),
            max: Some(self.options.max_value_depth as u64),
            observed: None,
        })
    }

    /// Counts one unit of work and checks the deadline every 1024 units.
    #[inline]
    pub(crate) fn tick(&mut self) -> TraverserResult<()> {
        tick_deadline(&mut self.deadline)
    }

    /// Counts `n` units of work at once, such as the children one `FlatMap` input expanded
    /// into, and checks the deadline when they cross a 1024-unit boundary.
    #[inline]
    pub(crate) fn tick_n(&mut self, n: usize) -> TraverserResult<()> {
        match &mut self.deadline {
            None => Ok(()),
            Some(deadline) => deadline.tick_n(n),
        }
    }
}

/// [`TraverserContext::check_string_bytes`] with an explicit limit, for code that holds a borrow
/// of the context while it builds a string.
#[inline]
pub(crate) fn check_string_bytes(len: usize, max: u64) -> TraverserResult<()> {
    if len as u64 > max {
        return Err(string_bytes_error(max, len));
    }
    Ok(())
}

/// The error of a step-built string longer than `traversal.max_string_bytes`.
#[cold]
fn string_bytes_error(max: u64, len: usize) -> Box<TraverserError> {
    Box::new(TraverserError::ResourceLimitExceeded {
        limit: MAX_STRING_BYTES.to_string(),
        max: Some(max),
        observed: Some(len as u64),
    })
}

/// [`TraverserContext::tick`] on the deadline field alone, for loops that hold a borrow of the
/// storage while they produce traversers.
#[inline]
pub(crate) fn tick_deadline(deadline: &mut Option<Deadline>) -> TraverserResult<()> {
    match deadline {
        None => Ok(()),
        Some(deadline) => deadline.tick(),
    }
}

/// Option key of the per-execution wall-clock budget, as in TinkerPop (`Tokens.ARGS_EVAL_TIMEOUT`).
pub(crate) const EVALUATION_TIMEOUT: &str = "evaluationTimeout";

/// Option key of the order `repeat()` visits its loop in: `"bfs"` (default) or `"dfs"`.
pub(crate) const REPEAT_ORDER: &str = "repeat.order";

/// Option key of the iteration limit of a `repeat()` without `times()`.
pub(crate) const REPEAT_MAX_LOOPS: &str = "repeat.max_loops";

/// Iteration limit of a `repeat()` without `times()` when `repeat.max_loops` is not set.
pub(crate) const DEFAULT_REPEAT_MAX_LOOPS: u32 = 10_000;

/// Option key that switches merging of equal traversers (traverser bulk) on or off. A boolean;
/// `true` by default. Merged and unmerged runs return the same multiset of results; mutating
/// plans never merge. Merging happens at `barrier()` and on the `repeat()` BFS frontier.
pub(crate) const BULK_MERGE: &str = "bulk.merge";

/// Option key behind `withBulk(false)`: a boolean, `false` by default. While `true`, a merge
/// collapses equal traversers but the merged traverser keeps bulk 1 (TinkerPop `ONE_BULK`), a
/// traverser with a sack and no merge operator never merges, and only an explicit `barrier()`
/// merges (the automatic merge points are off).
pub(crate) const BULK_ONE: &str = "bulk.one";

/// Option key of the largest number of values one bulk expansion may materialize (a terminal
/// list, `fold()`, `aggregate()`, ...). An integer from 1; see [`DEFAULT_BULK_MAX_EXPANSION`].
pub(crate) const BULK_MAX_EXPANSION: &str = "bulk.max_expansion";

/// Expansion limit when `bulk.max_expansion` is not set: ten million values. A second line
/// behind the byte budget [`MAX_MATERIALIZED_BYTES`]: at 80 bytes per expanded traverser this is
/// about 800 MB before the values themselves.
pub(crate) const DEFAULT_BULK_MAX_EXPANSION: u64 = 10_000_000;

/// Option key of the largest estimated size, in bytes, of one value or buffer a step
/// materializes: a bulk expansion (a terminal list, `fold()`, `aggregate()`, `group()` value
/// lists, ...) or a value a `repeat()` loop builds up again on every iteration
/// (`repeat(__.path())`, `repeat(__.group())`). An integer from 1; see
/// [`DEFAULT_MAX_MATERIALIZED_BYTES`]. The size is the engine's estimate (the estimator behind
/// the `Mem` column of `.profile_with(ProfileType::Memory)` without `alloc-tracking`), not a
/// measurement.
pub(crate) const MAX_MATERIALIZED_BYTES: &str = "traversal.max_materialized_bytes";

/// Byte budget when `traversal.max_materialized_bytes` is not set: 1 GiB natively, 256 MiB on
/// `wasm32`, whose linear memory is at most 4 GiB and usually far less in a browser tab.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) const DEFAULT_MAX_MATERIALIZED_BYTES: u64 = 1024 * 1024 * 1024;

/// Byte budget when `traversal.max_materialized_bytes` is not set: 1 GiB natively, 256 MiB on
/// `wasm32`, whose linear memory is at most 4 GiB and usually far less in a browser tab.
#[cfg(target_arch = "wasm32")]
pub(crate) const DEFAULT_MAX_MATERIALIZED_BYTES: u64 = 256 * 1024 * 1024;

/// Option key of the longest string, in bytes (UTF-8), a step may build: `replace()`,
/// `concat()`, `format()`, `conjoin()`, `as_string()`, `to_upper()`/`to_lower()`, `substring()`,
/// `split()`, the trims and `reverse()`. Rhai's own `ScriptLimits::max_string_size` bounds the
/// strings a script builds; this option bounds the strings the engine builds. An integer from 1;
/// see [`DEFAULT_MAX_STRING_BYTES`].
pub(crate) const MAX_STRING_BYTES: &str = "traversal.max_string_bytes";

/// String limit when `traversal.max_string_bytes` is not set: 16 MiB, the SAFE
/// `ScriptLimits::max_string_size` of a script.
pub(crate) const DEFAULT_MAX_STRING_BYTES: u64 = 16 * 1024 * 1024;

/// Option key of the largest number of traversers one step may leave alive (the stream between
/// two steps, which is also what a terminal list materializes). An integer from 1; pass
/// `9223372036854775807` for "unlimited". See [`DEFAULT_MAX_TRAVERSERS`].
pub(crate) const MAX_TRAVERSERS: &str = "traversal.max_traversers";

/// Traverser limit when `traversal.max_traversers` is not set: 10 million records, which at 80
/// bytes each is about 800 MB before the values they point at.
pub(crate) const DEFAULT_MAX_TRAVERSERS: u64 = 10_000_000;

/// Option key of the deepest nesting of arrays, maps and paths one value may reach inside the
/// engine (a `jpath(..)` write, a `repeat()` that wraps its value again on every iteration). An
/// integer from 1; see [`crate::value::depth`] for how levels are counted and why the depth is
/// bounded (recursive readers would overflow the native stack).
pub(crate) const MAX_VALUE_DEPTH: &str = "traversal.max_value_depth";

/// The resource-limit options of a traversal that have a default, with the default in effect on
/// this target when neither the host nor the query sets them, as `(option key, value)`:
/// `traversal.max_traversers`, `traversal.max_materialized_bytes` (smaller on `wasm32`),
/// `traversal.max_string_bytes`, `traversal.max_value_depth`, `bulk.max_expansion` and
/// `repeat.max_loops`. `evaluationTimeout` and `memory.limit` have no default (unbounded) and
/// are not listed. For displays such as the playground's statistics panel; the book page
/// "Resource Limits" has the table.
///
/// ```
/// let defaults = graphersal::exec::traversal_limit_defaults();
/// assert!(defaults.iter().any(|(key, _)| *key == "traversal.max_traversers"));
/// ```
pub fn traversal_limit_defaults() -> Vec<(&'static str, ElementProperty)> {
    let int = |n: u64| ElementProperty::from(i64::try_from(n).unwrap_or(i64::MAX));
    vec![
        (MAX_TRAVERSERS, int(DEFAULT_MAX_TRAVERSERS)),
        (MAX_MATERIALIZED_BYTES, int(DEFAULT_MAX_MATERIALIZED_BYTES)),
        (MAX_STRING_BYTES, int(DEFAULT_MAX_STRING_BYTES)),
        (
            MAX_VALUE_DEPTH,
            int(crate::value::depth::DEFAULT_MAX_VALUE_DEPTH as u64),
        ),
        (BULK_MAX_EXPANSION, int(DEFAULT_BULK_MAX_EXPANSION)),
        (REPEAT_MAX_LOOPS, int(u64::from(DEFAULT_REPEAT_MAX_LOOPS))),
    ]
}

/// Option key that turns off named optimizer rules for this execution, e.g.
/// `g.with("optimizer.disabled", ["filter_reorder"])`, or every rule at once with the reserved
/// [`OPTIMIZER_RULE_ALL`] token: `g.with("optimizer.disabled", ["all"])`. Rule names are the
/// stable identifiers [`crate::traversal::optimizer::KNOWN_RULES`] lists, and are exactly what
/// `.profile()` renders in its "Optimizer rules applied" line.
pub(crate) const OPTIMIZER_DISABLED: &str = "optimizer.disabled";

/// Option key that force-enables named optimizer rules that default to off for this execution,
/// e.g. `g.with("optimizer.enabled", ["some_experimental_rule"])`. As of this crate's current
/// rule set, every rule already defaults to on, so this has nothing to force on yet; it exists so
/// a future opt-in rule can be turned on without a new mechanism.
pub(crate) const OPTIMIZER_ENABLED: &str = "optimizer.enabled";

/// Reserved name accepted anywhere a rule name is, in either `optimizer.disabled` or
/// `optimizer.enabled` (e.g. `g.with("optimizer.disabled", ["all"])`): expands at parse time to
/// every name in [`crate::traversal::optimizer::KNOWN_RULES`], rather than being a rule name
/// itself. Mainly useful for `optimizer.disabled`, to measure a query's genuinely-unoptimized plan
/// without hand-listing every current rule, a list that would silently go stale as rules are
/// added.
pub(crate) const OPTIMIZER_RULE_ALL: &str = "all";

/// Option key of the seed of the execution's random number generator (`coin()`, `sample()`,
/// `order().by(Order.shuffle)`): an integer (int64). Graphersal's counterpart of TinkerPop's
/// `SeedStrategy`. Without it the generator is seeded from the operating system's entropy.
///
/// Deviation from TinkerPop: the draws are those of Graphersal's generator (Rust `rand`), stable
/// for one Graphersal version, never those of TinkerPop's Java `Random` for the same seed.
pub(crate) const RANDOM_SEED: &str = "random.seed";

/// Option key that switches path recording between "only the positions a step actually reads"
/// (the default) and "every position, unconditionally".
pub(crate) const PATH_ANALYSIS: &str = "path.analysis";

/// Option key of the spelling of rendered step names in diagnostics (`.profile()` step names,
/// error step locations, `help()` rewrites of the failing query): `"snake"` (the default) or
/// `"camel"` (Gremlin camelCase). See [`Spelling`](crate::traversal::Spelling). Data output is
/// never affected.
pub(crate) const RENDER_SPELLING: &str = "render.spelling";

/// Every `g.with()` key this crate recognises, in the order [`ExecutionOptions::parse`] resolves
/// them. A host's [`ExecutionPolicy`] can lock or default any of these; used by
/// [`ExecutionPolicy`]-aware code (and tests) that needs to iterate every known key rather than
/// name them one by one. Keep in sync with the book's canonical options reference page.
// Only exercised by this module's own tests today (iterating every key to assert the generic
// lock check covers each one uniformly); kept `pub(crate)` rather than test-only since it is the
// natural single source of truth for "every g.with() key" a future caller (a policy-validation
// helper, a docs generator) would also want.
#[allow(dead_code)]
pub(crate) const KNOWN_OPTION_KEYS: &[&str] = &[
    EVALUATION_TIMEOUT,
    REPEAT_ORDER,
    REPEAT_MAX_LOOPS,
    OPTIMIZER_DISABLED,
    OPTIMIZER_ENABLED,
    PATH_ANALYSIS,
    BULK_MERGE,
    BULK_ONE,
    BULK_MAX_EXPANSION,
    MAX_TRAVERSERS,
    RANDOM_SEED,
    RENDER_SPELLING,
    MAX_VALUE_DEPTH,
    MAX_MATERIALIZED_BYTES,
    MAX_STRING_BYTES,
    MEMORY_LIMIT,
    MEMORY_HEADROOM,
];

/// The administrative `g.with()` keys: the resource ceilings and the engine switches. A
/// query needs `Update` on [`Resource::Option`](crate::graph::Resource::Option) to set
/// one (a host's own preset value is not the query's setting). Every other known key is
/// semantic and harmless ([`HARMLESS_OPTION_KEYS`]); an unknown key is ignored by the engine and
/// needs nothing either.
pub(crate) const ADMIN_OPTION_KEYS: &[&str] = &[
    EVALUATION_TIMEOUT,
    REPEAT_MAX_LOOPS,
    BULK_MAX_EXPANSION,
    MAX_TRAVERSERS,
    MAX_VALUE_DEPTH,
    MAX_MATERIALIZED_BYTES,
    MAX_STRING_BYTES,
    MEMORY_LIMIT,
    MEMORY_HEADROOM,
    OPTIMIZER_DISABLED,
    OPTIMIZER_ENABLED,
    PATH_ANALYSIS,
    BULK_MERGE,
];

/// The known `g.with()` keys that change only what a query means, not the host's limits:
/// `repeat.order` (TinkerPop's traversal order), `bulk.one` (`withBulk(false)`) and
/// `random.seed` (which random draws `coin()`, `sample()` and `Order.shuffle` make), plus
/// `render.spelling`, which changes only how diagnostics spell step names.
#[allow(dead_code)]
pub(crate) const HARMLESS_OPTION_KEYS: &[&str] =
    &[REPEAT_ORDER, BULK_ONE, RANDOM_SEED, RENDER_SPELLING];

/// Whether setting the `g.with()` option `key` is an administrative request (`Update` on
/// [`Resource::Option`](crate::graph::Resource::Option)); a harmless or unknown key needs
/// nothing.
pub(crate) fn is_admin_option(key: &str) -> bool {
    ADMIN_OPTION_KEYS.contains(&key)
}

/// The order in which `repeat()` walks its loop.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum RepeatOrder {
    /// Breadth first: all of iteration `k` before iteration `k + 1` (the default).
    #[default]
    Bfs,
    /// Depth first, pre-order: each traverser is followed to the end of its loop before its
    /// siblings.
    Dfs,
}

/// Typed execution options of one traversal run, parsed once from
/// [`GraphTraversalSource::with`](crate::GraphTraversalSource::with) before it executes.
///
/// Unknown keys are ignored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExecutionOptions {
    /// `evaluationTimeout`: wall-clock budget of the execution. `None` when absent or `0`.
    pub(crate) evaluation_timeout: Option<Duration>,
    /// `repeat.order`: the order `repeat()` walks its loop in.
    pub(crate) repeat_order: RepeatOrder,
    /// `repeat.max_loops`: iterations a `repeat()` without `times()` may run before it fails
    /// with [`TraverserError::RepeatLimitExceeded`].
    pub(crate) repeat_max_loops: u32,
    /// `optimizer.disabled`: names of on-by-default optimizer rules to leave out of this
    /// execution's pipeline. Validated against
    /// [`crate::traversal::optimizer::KNOWN_RULES`] at parse time; empty when the option is
    /// absent.
    pub(crate) optimizer_disabled: HashSet<String>,
    /// `optimizer.enabled`: names of off-by-default optimizer rules to force into this
    /// execution's pipeline. Validated the same way as `optimizer_disabled`; empty when the
    /// option is absent, and currently always a no-op in practice since no rule defaults to off
    /// yet.
    pub(crate) optimizer_enabled: HashSet<String>,
    /// `path.analysis`: `true` (the default) records only the path positions a step reads,
    /// `false` records every position unconditionally.
    pub(crate) path_analysis: bool,
    /// `bulk.merge`: whether merge points merge equal traversers (default `false`).
    pub(crate) bulk_merge: bool,
    /// `bulk.one`: a merge keeps bulk 1 (default `false`).
    pub(crate) bulk_one: bool,
    /// `bulk.max_expansion`: the most values one bulk expansion may materialize.
    pub(crate) bulk_max_expansion: u64,
    /// `traversal.max_traversers`: the most traversers one step may leave alive.
    pub(crate) max_traversers: u64,
    /// `random.seed`: the seed of the execution's random number generator; `None` seeds it from
    /// entropy.
    pub(crate) random_seed: Option<u64>,
    /// `render.spelling`: the spelling of step names in this execution's diagnostics.
    pub(crate) render_spelling: crate::traversal::Spelling,
    /// `traversal.max_value_depth`: the deepest nesting one value may reach inside the engine.
    pub(crate) max_value_depth: usize,
    /// `traversal.max_materialized_bytes`: the largest estimated size of one value or buffer a
    /// step materializes.
    pub(crate) max_materialized_bytes: u64,
    /// `traversal.max_string_bytes`: the longest string a step may build.
    pub(crate) max_string_bytes: u64,
    /// `memory.limit` (with `memory.headroom` for `"auto"`): the memory budget of the run, `None`
    /// without one (the default). Parsed with no starting point; the run takes it
    /// ([`MemoryBudget::start`]) before the first step.
    pub(crate) memory: Option<MemoryBudget>,
}

impl Default for ExecutionOptions {
    fn default() -> Self {
        Self {
            evaluation_timeout: None,
            repeat_order: RepeatOrder::default(),
            repeat_max_loops: DEFAULT_REPEAT_MAX_LOOPS,
            optimizer_disabled: HashSet::new(),
            optimizer_enabled: HashSet::new(),
            path_analysis: true,
            bulk_merge: true,
            bulk_one: false,
            bulk_max_expansion: DEFAULT_BULK_MAX_EXPANSION,
            max_traversers: DEFAULT_MAX_TRAVERSERS,
            random_seed: None,
            render_spelling: crate::traversal::Spelling::Snake,
            max_value_depth: crate::value::depth::DEFAULT_MAX_VALUE_DEPTH,
            max_materialized_bytes: DEFAULT_MAX_MATERIALIZED_BYTES,
            max_string_bytes: DEFAULT_MAX_STRING_BYTES,
            memory: None,
        }
    }
}

impl ExecutionOptions {
    /// Parses the recognised keys of `options`, applying `policy`'s locks and defaults uniformly
    /// to every one of them (D4 of the execution-policy spec).
    ///
    /// For each key in [`KNOWN_OPTION_KEYS`], [`resolve_option`] is consulted first: a query
    /// attempt to set a key `policy` has locked fails immediately with
    /// [`TraverserError::LockedOption`], before that key's own value-shape parsing ever runs (so
    /// a locked key's malformed query value is never even inspected, let alone applied). This
    /// makes policy-checking automatic for every key parsed here — a future key that calls
    /// [`resolve_option`] instead of reading `options` directly participates in locking/defaults
    /// without any extra code.
    ///
    /// Fails with [`TraverserError::InvalidOption`] on a value of the wrong shape (from either
    /// the query or the policy's own default — a host is expected to set well-formed defaults,
    /// but this is not trusted blindly).
    pub(crate) fn parse(
        options: &HashMap<CompactString, ElementProperty>,
        policy: &ExecutionPolicy,
    ) -> TraverserResult<Self> {
        let mut parsed = Self::default();
        if let Some(value) = resolve_option(EVALUATION_TIMEOUT, options, policy)? {
            parsed.evaluation_timeout = parse_timeout(value)?;
        }
        if let Some(value) = resolve_option(REPEAT_ORDER, options, policy)? {
            parsed.repeat_order = parse_repeat_order(value)?;
        }
        if let Some(value) = resolve_option(REPEAT_MAX_LOOPS, options, policy)? {
            parsed.repeat_max_loops = parse_max_loops(value)?;
        }
        if let Some(value) = resolve_option(OPTIMIZER_DISABLED, options, policy)? {
            parsed.optimizer_disabled = parse_rule_names(OPTIMIZER_DISABLED, value)?;
        }
        if let Some(value) = resolve_option(OPTIMIZER_ENABLED, options, policy)? {
            parsed.optimizer_enabled = parse_rule_names(OPTIMIZER_ENABLED, value)?;
        }
        if let Some(value) = resolve_option(PATH_ANALYSIS, options, policy)? {
            parsed.path_analysis = value.as_boolean().unwrap_or(true);
        }
        if let Some(value) = resolve_option(BULK_MERGE, options, policy)? {
            parsed.bulk_merge = value.as_boolean().ok_or_else(|| {
                Box::new(TraverserError::InvalidOption {
                    key: BULK_MERGE.to_string(),
                    reason: format!(
                        "expected true or false, got {} {}",
                        property_kind(value),
                        crate::value::literal::rhai_literal(value)
                    ),
                })
            })?;
        }
        if let Some(value) = resolve_option(BULK_ONE, options, policy)? {
            parsed.bulk_one = value.as_boolean().ok_or_else(|| {
                Box::new(TraverserError::InvalidOption {
                    key: BULK_ONE.to_string(),
                    reason: format!(
                        "expected true or false, got {} {}",
                        property_kind(value),
                        crate::value::literal::rhai_literal(value)
                    ),
                })
            })?;
        }
        if let Some(value) = resolve_option(BULK_MAX_EXPANSION, options, policy)? {
            parsed.bulk_max_expansion = parse_max_expansion(value)?;
        }
        if let Some(value) = resolve_option(MAX_TRAVERSERS, options, policy)? {
            parsed.max_traversers = parse_positive_count(MAX_TRAVERSERS, value, "traversers")?;
        }
        if let Some(value) = resolve_option(RANDOM_SEED, options, policy)? {
            parsed.random_seed = Some(parse_random_seed(value)?);
        }
        if let Some(value) = resolve_option(RENDER_SPELLING, options, policy)? {
            parsed.render_spelling = parse_spelling(value)?;
        }
        if let Some(value) = resolve_option(MAX_VALUE_DEPTH, options, policy)? {
            parsed.max_value_depth =
                usize::try_from(parse_positive_count(MAX_VALUE_DEPTH, value, "levels")?)
                    .unwrap_or(usize::MAX);
        }
        if let Some(value) = resolve_option(MAX_MATERIALIZED_BYTES, options, policy)? {
            parsed.max_materialized_bytes =
                parse_positive_count(MAX_MATERIALIZED_BYTES, value, "bytes")?;
        }
        if let Some(value) = resolve_option(MAX_STRING_BYTES, options, policy)? {
            parsed.max_string_bytes = parse_positive_count(MAX_STRING_BYTES, value, "bytes")?;
        }
        parsed.memory = resolve_memory_limit(options, policy)?.map(MemoryBudget::new);
        // A scheduled job's limits cap the run's own (the stricter value stays).
        if let Some(job) = crate::graph::jobs::limits::current() {
            if let Some(remaining) = job.remaining() {
                parsed.evaluation_timeout = Some(
                    parsed
                        .evaluation_timeout
                        .map_or(remaining, |own| own.min(remaining)),
                );
            }
            if let Some(limit) = job.memory_limit {
                let limit = parsed.memory.map_or(limit, |own| own.limit.min(limit));
                parsed.memory = Some(MemoryBudget::new(limit));
            }
        }
        if let Some(name) = parsed
            .optimizer_disabled
            .intersection(&parsed.optimizer_enabled)
            .next()
        {
            return Err(Box::new(TraverserError::InvalidOption {
                key: OPTIMIZER_ENABLED.to_string(),
                reason: format!(
                    "\"{name}\" is in both '{OPTIMIZER_ENABLED}' and '{OPTIMIZER_DISABLED}'; a \
                     rule cannot be both enabled and disabled for the same execution"
                ),
            }));
        }
        Ok(parsed)
    }
}

/// The memory budget in bytes that `memory.limit` (with `memory.headroom` for `"auto"`)
/// resolves to under `options` and `policy`, by the precedence of [`resolve_option`]; `None`:
/// no budget. Shared by [`ExecutionOptions::parse`] and the units that are not traversals
/// ([`crate::traversal::memory_budget::UnitBudget`]).
pub(crate) fn resolve_memory_limit(
    options: &HashMap<CompactString, ElementProperty>,
    policy: &ExecutionPolicy,
) -> TraverserResult<Option<u64>> {
    let headroom = match resolve_option(MEMORY_HEADROOM, options, policy)? {
        Some(value) => Some(parse_headroom(value)?),
        None => None,
    };
    match resolve_option(MEMORY_LIMIT, options, policy)? {
        Some(value) => parse_memory_limit(value, headroom),
        None => Ok(None),
    }
}

/// The single generic "is this key locked / what wins" check every key in
/// [`ExecutionOptions::parse`] runs through, so policy enforcement cannot be forgotten for a
/// future key (D4).
///
/// Resolution order: if `policy` locks `key`, the query's own `options` must not contain it —
/// checked and rejected *before* any value-shape parsing of either the query's or the policy's
/// value runs, so a locked key's query value never takes effect in any way, not even to produce a
/// more specific parse error. Otherwise: the query's own value wins if present, else `policy`'s
/// default for `key`, else `None` (the caller's engine-hardcoded default applies).
fn resolve_option<'q>(
    key: &'static str,
    options: &'q HashMap<CompactString, ElementProperty>,
    policy: &'q ExecutionPolicy,
) -> TraverserResult<Option<&'q ElementProperty>> {
    if policy.is_locked(key) {
        if options.contains_key(key) {
            return Err(Box::new(TraverserError::LockedOption {
                key: key.to_string(),
            }));
        }
        return Ok(policy.default_for(key));
    }
    match options.get(key) {
        Some(value) => Ok(Some(value)),
        None => Ok(policy.default_for(key)),
    }
}

/// Reads `optimizer.disabled`/`optimizer.enabled`: an array of optimizer rule names, each one
/// either a string matching an entry of [`crate::traversal::optimizer::KNOWN_RULES`] or the
/// reserved [`OPTIMIZER_RULE_ALL`] token, which expands to every known rule name. An unknown name
/// fails immediately rather than silently doing nothing, matching this crate's convention of
/// erroring on a bad option value (`repeat.order`, `evaluationTimeout`) rather than ignoring it.
fn parse_rule_names(
    key: &'static str,
    value: &ElementProperty,
) -> TraverserResult<HashSet<String>> {
    let invalid = |reason: String| {
        Box::new(TraverserError::InvalidOption {
            key: key.to_string(),
            reason,
        })
    };
    let items = value.as_array().ok_or_else(|| {
        invalid(format!(
            "expected an array of optimizer rule names, got {} {}",
            property_kind(value),
            crate::value::literal::rhai_literal(value)
        ))
    })?;
    let mut names = HashSet::with_capacity(items.len());
    for item in items {
        let name = item.as_str().ok_or_else(|| {
            invalid(format!(
                "expected an array of optimizer rule names (strings), got {} {} in the list",
                property_kind(item),
                crate::value::literal::rhai_literal(item)
            ))
        })?;
        if name == OPTIMIZER_RULE_ALL {
            names.extend(
                crate::traversal::optimizer::KNOWN_RULES
                    .iter()
                    .map(|(known, _)| known.to_string()),
            );
            continue;
        }
        if !crate::traversal::optimizer::KNOWN_RULES
            .iter()
            .any(|(known, _)| *known == name)
        {
            let valid: Vec<&str> = crate::traversal::optimizer::KNOWN_RULES
                .iter()
                .map(|(known, _)| *known)
                .chain(std::iter::once(OPTIMIZER_RULE_ALL))
                .collect();
            return Err(invalid(format!(
                "unknown optimizer rule \"{name}\"; valid names are: {}",
                valid.join(", ")
            )));
        }
        names.insert(name.to_string());
    }
    Ok(names)
}

/// Reads `repeat.order`: `"bfs"` or `"dfs"`, in any letter case.
fn parse_repeat_order(value: &ElementProperty) -> TraverserResult<RepeatOrder> {
    match value.as_str() {
        Some(order) if order.eq_ignore_ascii_case("bfs") => Ok(RepeatOrder::Bfs),
        Some(order) if order.eq_ignore_ascii_case("dfs") => Ok(RepeatOrder::Dfs),
        Some(order) => Err(Box::new(TraverserError::InvalidOption {
            key: REPEAT_ORDER.to_string(),
            reason: format!("expected \"bfs\" or \"dfs\", got \"{order}\""),
        })),
        None => Err(Box::new(TraverserError::InvalidOption {
            key: REPEAT_ORDER.to_string(),
            reason: format!(
                "expected \"bfs\" or \"dfs\", got {} {}",
                property_kind(value),
                crate::value::literal::rhai_literal(value)
            ),
        })),
    }
}

/// Reads a count option (`bulk.max_expansion`, `traversal.max_traversers`, the byte limits):
/// an integer from 1 to `i64::MAX`. `unit` names what is counted in the error text.
fn parse_positive_count(
    key: &'static str,
    value: &ElementProperty,
    unit: &str,
) -> TraverserResult<u64> {
    let invalid = |reason: String| {
        Box::new(TraverserError::InvalidOption {
            key: key.to_string(),
            reason,
        })
    };
    let limit = match value {
        ElementProperty::Number(number) => match (number.as_i64(), number.as_f64()) {
            (Some(n), _) => n,
            (None, Some(n)) if n.is_finite() && n.fract() == 0.0 && n.abs() <= i64::MAX as f64 => {
                n as i64
            }
            _ => {
                return Err(invalid(format!(
                    "expected a positive integer number of {unit}, got {number}"
                )));
            }
        },
        other => {
            return Err(invalid(format!(
                "expected a positive integer number of {unit}, got {} {}",
                property_kind(other),
                crate::value::literal::rhai_literal(other)
            )));
        }
    };
    match u64::try_from(limit) {
        Ok(limit) if limit >= 1 => Ok(limit),
        _ => Err(invalid(format!(
            "expected a positive integer number of {unit}, got {limit}"
        ))),
    }
}

/// The `render.spelling` a query asks for, read leniently (a missing or malformed value is the
/// default): the spelling of an error raised before the options are parsed, or by their parsing.
pub(crate) fn requested_spelling(
    options: &HashMap<CompactString, ElementProperty>,
) -> crate::traversal::Spelling {
    options
        .get(RENDER_SPELLING)
        .and_then(ElementProperty::as_str)
        .and_then(crate::traversal::Spelling::parse)
        .unwrap_or_default()
}

/// Reads `render.spelling`: `"snake"` or `"camel"`.
fn parse_spelling(value: &ElementProperty) -> TraverserResult<crate::traversal::Spelling> {
    value
        .as_str()
        .and_then(crate::traversal::Spelling::parse)
        .ok_or_else(|| {
            Box::new(TraverserError::InvalidOption {
                key: RENDER_SPELLING.to_string(),
                reason: format!(
                    "expected \"snake\" or \"camel\", got {} {}",
                    property_kind(value),
                    crate::value::literal::rhai_literal(value)
                ),
            })
        })
}

/// Reads `random.seed`: any int64 (a negative seed is used by its bit pattern). A float, a
/// string or any other value is rejected.
fn parse_random_seed(value: &ElementProperty) -> TraverserResult<u64> {
    match value {
        ElementProperty::Number(number) => match number.as_i64() {
            Some(seed) => Ok(seed as u64),
            None => Err(Box::new(TraverserError::InvalidOption {
                key: RANDOM_SEED.to_string(),
                reason: format!("expected an integer seed, got {number}"),
            })),
        },
        other => Err(Box::new(TraverserError::InvalidOption {
            key: RANDOM_SEED.to_string(),
            reason: format!(
                "expected an integer seed, got {} {}",
                property_kind(other),
                crate::value::literal::rhai_literal(other)
            ),
        })),
    }
}

/// Reads `memory.limit`: a positive integer number of bytes, or `"auto"` (any letter case), which
/// is the process's cgroup memory limit minus `headroom` (`None`: the default headroom), or no
/// budget when the process has no cgroup limit (and always on `wasm32`).
fn parse_memory_limit(
    value: &ElementProperty,
    headroom: Option<u64>,
) -> TraverserResult<Option<u64>> {
    if let Some(text) = value.as_str() {
        if text.eq_ignore_ascii_case(MEMORY_LIMIT_AUTO) {
            return Ok(crate::traversal::memory_budget::resolve_auto(headroom));
        }
        return Err(Box::new(TraverserError::InvalidOption {
            key: MEMORY_LIMIT.to_string(),
            reason: format!(
                "expected a positive integer number of bytes or \"auto\", got \"{text}\""
            ),
        }));
    }
    parse_positive_count(MEMORY_LIMIT, value, "bytes").map(Some)
}

/// Reads `memory.headroom`: a non-negative integer number of bytes.
fn parse_headroom(value: &ElementProperty) -> TraverserResult<u64> {
    if matches!(value, ElementProperty::Number(number) if number.as_i64() == Some(0)) {
        return Ok(0);
    }
    parse_positive_count(MEMORY_HEADROOM, value, "bytes")
}

/// Reads `bulk.max_expansion`: an integer from 1.
fn parse_max_expansion(value: &ElementProperty) -> TraverserResult<u64> {
    parse_positive_count(BULK_MAX_EXPANSION, value, "values")
}

/// Reads `repeat.max_loops`: an integer from 1 to `u32::MAX`.
fn parse_max_loops(value: &ElementProperty) -> TraverserResult<u32> {
    let invalid = |reason: String| {
        Box::new(TraverserError::InvalidOption {
            key: REPEAT_MAX_LOOPS.to_string(),
            reason,
        })
    };
    let loops = match value {
        ElementProperty::Number(number) => match (number.as_i64(), number.as_f64()) {
            (Some(n), _) => n,
            (None, Some(n)) if n.is_finite() && n.fract() == 0.0 && n.abs() <= i64::MAX as f64 => {
                n as i64
            }
            _ => {
                return Err(invalid(format!(
                    "expected a positive integer number of iterations, got {number}"
                )));
            }
        },
        other => {
            return Err(invalid(format!(
                "expected a positive integer number of iterations, got {} {}",
                property_kind(other),
                crate::value::literal::rhai_literal(other)
            )));
        }
    };
    match u32::try_from(loops) {
        Ok(loops) if loops >= 1 => Ok(loops),
        _ => Err(invalid(format!(
            "expected a positive integer number of iterations up to {}, got {loops}",
            u32::MAX
        ))),
    }
}

/// Reads `evaluationTimeout`: a non-negative integer number of milliseconds, `0` meaning none.
fn parse_timeout(value: &ElementProperty) -> TraverserResult<Option<Duration>> {
    let invalid = |reason: String| {
        Box::new(TraverserError::InvalidOption {
            key: EVALUATION_TIMEOUT.to_string(),
            reason,
        })
    };
    let millis = match value {
        ElementProperty::Number(number) => match (number.as_i64(), number.as_f64()) {
            (Some(ms), _) if ms >= 0 => ms as u64,
            (Some(ms), _) => {
                return Err(invalid(format!(
                    "expected a non-negative number of milliseconds, got {ms}"
                )));
            }
            // An integral float such as `5000.0` names a whole number of milliseconds too.
            (None, Some(ms))
                if ms.is_finite() && ms.fract() == 0.0 && ms >= 0.0 && ms <= u64::MAX as f64 =>
            {
                ms as u64
            }
            (None, Some(ms)) if ms < 0.0 => {
                return Err(invalid(format!(
                    "expected a non-negative number of milliseconds, got {ms}"
                )));
            }
            _ => {
                return Err(invalid(format!(
                    "expected an integer number of milliseconds, got {number}"
                )));
            }
        },
        other => {
            return Err(invalid(format!(
                "expected an integer number of milliseconds, got {} {}",
                property_kind(other),
                crate::value::literal::rhai_literal(other)
            )));
        }
    };
    Ok((millis > 0).then(|| Duration::from_millis(millis)))
}

/// Names the kind of a non-numeric option value for the `InvalidOption` reason.
fn property_kind(value: &ElementProperty) -> &'static str {
    match value {
        ElementProperty::Null => "null",
        ElementProperty::String(_) => "string",
        ElementProperty::Boolean(_) => "boolean",
        ElementProperty::Array(_) => "array",
        ElementProperty::Object(_) | ElementProperty::Map(_) => "object",
        _ => "value",
    }
}

/// Wall-clock budget of one execution (`evaluationTimeout`), and the cancellation token of the
/// thread that runs it ([`CancelToken::scope`](crate::CancelToken::scope)).
///
/// The clock and the token are read only by [`check`](Self::check); per-traverser sites
/// [`tick`](Self::tick), which reads them every 1024 units of work, so the budget and a
/// cancellation are enforced cooperatively: between steps, or every 1024 traversers.
#[derive(Debug, Clone)]
pub(crate) struct Deadline {
    started: Instant,
    /// When the budget runs out; `None` without an `evaluationTimeout` (only a token).
    at: Option<Instant>,
    limit: Duration,
    /// The cancellation token the execution started under.
    cancel: Option<crate::CancelToken>,
    /// Counts cheap ticks between real clock reads.
    ticks: u32,
}

impl Deadline {
    /// Starts a budget of `limit` now. `None` when the deadline is too far away to represent,
    /// which is as good as no deadline at all.
    #[cfg(test)]
    pub(crate) fn new(limit: Duration) -> Option<Self> {
        Self::start(Some(limit), None)
    }

    /// The checks of one execution: a budget of `limit` from now (`None`: no budget, or one too
    /// far away to represent) and the `cancel` token. `None` when there is nothing to check.
    pub(crate) fn start(
        limit: Option<Duration>,
        cancel: Option<crate::CancelToken>,
    ) -> Option<Self> {
        let started = Instant::now();
        let at = limit.and_then(|limit| started.checked_add(limit));
        if at.is_none() && cancel.is_none() {
            return None;
        }
        Some(Self {
            started,
            at,
            limit: limit.unwrap_or_default(),
            cancel,
            ticks: 0,
        })
    }

    /// Reads the token and the clock; fails once the run is cancelled or the budget is spent.
    #[inline]
    pub(crate) fn check(&self) -> TraverserResult<()> {
        if let Some(cancel) = &self.cancel
            && cancel.is_cancelled()
        {
            return Err(Box::new(TraverserError::Cancelled {
                elapsed_ms: saturating_millis(self.started.elapsed()),
            }));
        }
        let Some(at) = self.at else {
            return Ok(());
        };
        let now = Instant::now();
        if now >= at {
            return Err(Box::new(TraverserError::Timeout {
                limit_ms: saturating_millis(self.limit),
                elapsed_ms: saturating_millis(now.saturating_duration_since(self.started)),
            }));
        }
        Ok(())
    }

    /// Counts one unit of work, reading the clock every 1024 units.
    #[inline]
    pub(crate) fn tick(&mut self) -> TraverserResult<()> {
        self.ticks = self.ticks.wrapping_add(1);
        if self.ticks & 1023 == 0 {
            self.check()
        } else {
            Ok(())
        }
    }

    /// Counts `n` units of work, reading the clock when they cross a 1024-unit boundary.
    #[inline]
    pub(crate) fn tick_n(&mut self, n: usize) -> TraverserResult<()> {
        let before = self.ticks;
        self.ticks = before.wrapping_add(n as u32);
        if n >= 1024 || (before >> 10) != (self.ticks >> 10) {
            self.check()
        } else {
            Ok(())
        }
    }
}

fn saturating_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Number;

    #[test]
    fn traversal_limit_defaults_are_the_execution_defaults() {
        let defaults: HashMap<&str, ElementProperty> =
            traversal_limit_defaults().into_iter().collect();
        let options = ExecutionOptions::default();
        let int = |key: &str| match defaults.get(key) {
            Some(ElementProperty::Number(n)) => n.as_i64().unwrap_or(-1),
            other => panic!("{key}: {other:?}"),
        };
        assert_eq!(int(MAX_TRAVERSERS) as u64, options.max_traversers);
        assert_eq!(
            int(MAX_MATERIALIZED_BYTES) as u64,
            options.max_materialized_bytes
        );
        assert_eq!(int(MAX_STRING_BYTES) as u64, options.max_string_bytes);
        assert_eq!(int(MAX_VALUE_DEPTH) as usize, options.max_value_depth);
        assert_eq!(int(BULK_MAX_EXPANSION) as u64, options.bulk_max_expansion);
        assert_eq!(int(REPEAT_MAX_LOOPS) as u32, options.repeat_max_loops);
        assert_eq!(defaults.len(), 6);
    }

    fn options(value: ElementProperty) -> HashMap<CompactString, ElementProperty> {
        HashMap::from([(CompactString::new(EVALUATION_TIMEOUT), value)])
    }

    #[test]
    fn evaluation_timeout_parses_integers_and_zero() {
        let permissive = ExecutionPolicy::permissive();
        let parsed = ExecutionOptions::parse(
            &options(ElementProperty::Number(Number::from_i64(250))),
            &permissive,
        )
        .unwrap();
        assert_eq!(parsed.evaluation_timeout, Some(Duration::from_millis(250)));
        let parsed = ExecutionOptions::parse(
            &options(ElementProperty::Number(Number::from_f64(250.0))),
            &permissive,
        )
        .unwrap();
        assert_eq!(parsed.evaluation_timeout, Some(Duration::from_millis(250)));
        let parsed = ExecutionOptions::parse(
            &options(ElementProperty::Number(Number::from_i64(0))),
            &permissive,
        )
        .unwrap();
        assert_eq!(parsed.evaluation_timeout, None);
        assert_eq!(
            ExecutionOptions::parse(&HashMap::new(), &permissive).unwrap(),
            ExecutionOptions::default()
        );
    }

    #[test]
    fn max_traversers_defaults_parses_and_rejects_invalid_values() {
        let defaults =
            ExecutionOptions::parse(&HashMap::new(), &ExecutionPolicy::permissive()).unwrap();
        assert_eq!(defaults.max_traversers, DEFAULT_MAX_TRAVERSERS);
        let options = HashMap::from([(
            CompactString::new(MAX_TRAVERSERS),
            ElementProperty::Number(Number::from_i64(7)),
        )]);
        let parsed = ExecutionOptions::parse(&options, &ExecutionPolicy::permissive()).unwrap();
        assert_eq!(parsed.max_traversers, 7);
        for bad in [
            ElementProperty::Number(Number::from_i64(0)),
            ElementProperty::String("7".into()),
        ] {
            let options = HashMap::from([(CompactString::new(MAX_TRAVERSERS), bad)]);
            let err =
                ExecutionOptions::parse(&options, &ExecutionPolicy::permissive()).unwrap_err();
            assert!(
                matches!(*err, TraverserError::InvalidOption { ref key, .. } if key == MAX_TRAVERSERS)
            );
        }
    }

    #[test]
    fn evaluation_timeout_rejects_invalid_values() {
        for value in [
            ElementProperty::Number(Number::from_i64(-1)),
            ElementProperty::Number(Number::from_f64(1.5)),
            ElementProperty::Number(Number::from_f64(-2.0)),
            ElementProperty::String("abc".into()),
            ElementProperty::Boolean(true),
            ElementProperty::Null,
        ] {
            let err = ExecutionOptions::parse(&options(value), &ExecutionPolicy::permissive())
                .unwrap_err();
            assert!(
                matches!(*err, TraverserError::InvalidOption { ref key, .. } if key == EVALUATION_TIMEOUT)
            );
        }
    }

    #[test]
    fn repeat_options_parse_and_default() {
        let defaults =
            ExecutionOptions::parse(&HashMap::new(), &ExecutionPolicy::permissive()).unwrap();
        assert_eq!(defaults.repeat_order, RepeatOrder::Bfs);
        assert_eq!(defaults.repeat_max_loops, DEFAULT_REPEAT_MAX_LOOPS);

        let parsed = ExecutionOptions::parse(
            &HashMap::from([
                (
                    CompactString::new(REPEAT_ORDER),
                    ElementProperty::String("DFS".into()),
                ),
                (
                    CompactString::new(REPEAT_MAX_LOOPS),
                    ElementProperty::Number(Number::from_i64(50)),
                ),
            ]),
            &ExecutionPolicy::permissive(),
        )
        .unwrap();
        assert_eq!(parsed.repeat_order, RepeatOrder::Dfs);
        assert_eq!(parsed.repeat_max_loops, 50);
    }

    #[test]
    fn repeat_options_reject_invalid_values() {
        for (key, value) in [
            (REPEAT_ORDER, ElementProperty::String("sideways".into())),
            (REPEAT_ORDER, ElementProperty::Number(Number::from_i64(1))),
            (
                REPEAT_MAX_LOOPS,
                ElementProperty::Number(Number::from_i64(0)),
            ),
            (
                REPEAT_MAX_LOOPS,
                ElementProperty::Number(Number::from_i64(-3)),
            ),
            (
                REPEAT_MAX_LOOPS,
                ElementProperty::Number(Number::from_i64(i64::from(u32::MAX) + 1)),
            ),
            (
                REPEAT_MAX_LOOPS,
                ElementProperty::Number(Number::from_f64(1.5)),
            ),
            (REPEAT_MAX_LOOPS, ElementProperty::String("10".into())),
        ] {
            let options = HashMap::from([(CompactString::new(key), value)]);
            let err =
                ExecutionOptions::parse(&options, &ExecutionPolicy::permissive()).unwrap_err();
            assert!(
                matches!(*err, TraverserError::InvalidOption { key: ref k, .. } if k == key),
                "{err}"
            );
        }
    }

    #[test]
    fn bulk_options_parse_and_default() {
        let defaults =
            ExecutionOptions::parse(&HashMap::new(), &ExecutionPolicy::permissive()).unwrap();
        assert!(defaults.bulk_merge);
        assert_eq!(defaults.bulk_max_expansion, DEFAULT_BULK_MAX_EXPANSION);

        let parsed = ExecutionOptions::parse(
            &HashMap::from([
                (
                    CompactString::new(BULK_MERGE),
                    ElementProperty::Boolean(false),
                ),
                (
                    CompactString::new(BULK_MAX_EXPANSION),
                    ElementProperty::Number(Number::from_i64(5)),
                ),
            ]),
            &ExecutionPolicy::permissive(),
        )
        .unwrap();
        assert!(!parsed.bulk_merge);
        assert_eq!(parsed.bulk_max_expansion, 5);
    }

    #[test]
    fn bulk_options_reject_invalid_values() {
        for (key, value) in [
            (BULK_MERGE, ElementProperty::String("yes".into())),
            (BULK_MERGE, ElementProperty::Number(Number::from_i64(1))),
            (
                BULK_MAX_EXPANSION,
                ElementProperty::Number(Number::from_i64(0)),
            ),
            (
                BULK_MAX_EXPANSION,
                ElementProperty::Number(Number::from_i64(-3)),
            ),
            (
                BULK_MAX_EXPANSION,
                ElementProperty::Number(Number::from_f64(1.5)),
            ),
            (BULK_MAX_EXPANSION, ElementProperty::String("10".into())),
            (BULK_MAX_EXPANSION, ElementProperty::Boolean(true)),
        ] {
            let options = HashMap::from([(CompactString::new(key), value)]);
            let err =
                ExecutionOptions::parse(&options, &ExecutionPolicy::permissive()).unwrap_err();
            assert!(
                matches!(*err, TraverserError::InvalidOption { key: ref k, .. } if k == key),
                "{err}"
            );
        }
    }

    #[test]
    fn byte_limits_default_parse_and_reject_invalid_values() {
        let defaults =
            ExecutionOptions::parse(&HashMap::new(), &ExecutionPolicy::permissive()).unwrap();
        assert_eq!(
            defaults.max_materialized_bytes,
            DEFAULT_MAX_MATERIALIZED_BYTES
        );
        assert_eq!(defaults.max_string_bytes, 16 * 1024 * 1024);
        assert_eq!(defaults.bulk_max_expansion, 10_000_000);
        #[cfg(not(target_arch = "wasm32"))]
        assert_eq!(DEFAULT_MAX_MATERIALIZED_BYTES, 1 << 30);
        for key in [MAX_MATERIALIZED_BYTES, MAX_STRING_BYTES] {
            let options =
                HashMap::from([(CompactString::new(key), ElementProperty::Number(7.into()))]);
            let parsed = ExecutionOptions::parse(&options, &ExecutionPolicy::permissive()).unwrap();
            let value = if key == MAX_STRING_BYTES {
                parsed.max_string_bytes
            } else {
                parsed.max_materialized_bytes
            };
            assert_eq!(value, 7, "{key}");
            for bad in [
                ElementProperty::Number(Number::from_i64(0)),
                ElementProperty::Number(Number::from_f64(1.5)),
                ElementProperty::String("1 GiB".into()),
            ] {
                let options = HashMap::from([(CompactString::new(key), bad)]);
                let err =
                    ExecutionOptions::parse(&options, &ExecutionPolicy::permissive()).unwrap_err();
                assert!(
                    matches!(*err, TraverserError::InvalidOption { key: ref k, ref reason }
                        if k == key && reason.contains("number of bytes")),
                    "{err}"
                );
            }
            assert!(is_admin_option(key));
        }
    }

    #[test]
    fn string_limit_check_compares_bytes() {
        assert!(check_string_bytes(16, 16).is_ok());
        let err = check_string_bytes(17, 16).unwrap_err();
        assert!(matches!(
            *err,
            TraverserError::ResourceLimitExceeded { ref limit, max: Some(16), observed: Some(17) }
                if limit == MAX_STRING_BYTES
        ));
    }

    #[test]
    fn deadline_far_in_the_future_does_not_overflow() {
        // Either representable or treated as no deadline; it must never panic.
        let _ = Deadline::new(Duration::from_millis(u64::MAX));
        let mut deadline = Deadline::new(Duration::from_secs(3600)).unwrap();
        for _ in 0..4096 {
            deadline.tick().unwrap();
        }
        deadline.tick_n(5000).unwrap();
    }

    #[test]
    fn expired_deadline_fails_on_the_next_clock_read() {
        let mut deadline = Deadline::new(Duration::from_nanos(1)).unwrap();
        std::thread::sleep(Duration::from_millis(2));
        assert!(
            matches!(deadline.check(), Err(ref e) if matches!(**e, TraverserError::Timeout { .. }))
        );
        // Cheap ticks only read the clock at the 1024 boundary.
        let mut failed = false;
        for _ in 0..1024 {
            failed |= deadline.tick().is_err();
        }
        assert!(failed);
        assert!(deadline.tick_n(1024).is_err());
    }

    fn rule_names(names: &[&str]) -> ElementProperty {
        ElementProperty::Array(
            names
                .iter()
                .map(|n| ElementProperty::String((*n).into()))
                .collect(),
        )
    }

    #[test]
    fn optimizer_disabled_and_enabled_parse_known_rule_names() {
        let options = HashMap::from([(
            CompactString::new(OPTIMIZER_DISABLED),
            rule_names(&["filter_reorder", "count_pushdown"]),
        )]);
        let parsed = ExecutionOptions::parse(&options, &ExecutionPolicy::permissive()).unwrap();
        assert_eq!(
            parsed.optimizer_disabled,
            HashSet::from(["filter_reorder".to_string(), "count_pushdown".to_string()])
        );
        assert!(parsed.optimizer_enabled.is_empty());
    }

    #[test]
    fn optimizer_disabled_rejects_unknown_rule_names() {
        let options = HashMap::from([(
            CompactString::new(OPTIMIZER_DISABLED),
            rule_names(&["not_a_real_rule"]),
        )]);
        let err = ExecutionOptions::parse(&options, &ExecutionPolicy::permissive()).unwrap_err();
        assert!(
            matches!(*err, TraverserError::InvalidOption { ref key, .. } if key == OPTIMIZER_DISABLED)
        );
    }

    #[test]
    fn optimizer_disabled_all_expands_to_every_known_rule() {
        let options =
            HashMap::from([(CompactString::new(OPTIMIZER_DISABLED), rule_names(&["all"]))]);
        let parsed = ExecutionOptions::parse(&options, &ExecutionPolicy::permissive()).unwrap();
        let expected: HashSet<String> = crate::traversal::optimizer::KNOWN_RULES
            .iter()
            .map(|(name, _)| name.to_string())
            .collect();
        assert_eq!(parsed.optimizer_disabled, expected);
    }

    #[test]
    fn optimizer_disabled_and_enabled_reject_a_shared_name() {
        let options = HashMap::from([
            (
                CompactString::new(OPTIMIZER_DISABLED),
                rule_names(&["filter_reorder"]),
            ),
            (
                CompactString::new(OPTIMIZER_ENABLED),
                rule_names(&["filter_reorder"]),
            ),
        ]);
        let err = ExecutionOptions::parse(&options, &ExecutionPolicy::permissive()).unwrap_err();
        assert!(
            matches!(*err, TraverserError::InvalidOption { ref key, .. } if key == OPTIMIZER_ENABLED)
        );
    }

    #[test]
    fn optimizer_disabled_rejects_a_non_array_value() {
        let options = HashMap::from([(
            CompactString::new(OPTIMIZER_DISABLED),
            ElementProperty::String("filter_reorder".into()),
        )]);
        let err = ExecutionOptions::parse(&options, &ExecutionPolicy::permissive()).unwrap_err();
        assert!(
            matches!(*err, TraverserError::InvalidOption { ref key, .. } if key == OPTIMIZER_DISABLED)
        );
    }

    #[test]
    fn a_locked_key_the_query_tries_to_set_fails_with_locked_option() {
        let policy = ExecutionPolicy::permissive()
            .with_locked(EVALUATION_TIMEOUT)
            .with_default(EVALUATION_TIMEOUT, 2000);
        let options = options(ElementProperty::Number(Number::from_i64(999_999)));
        let err = ExecutionOptions::parse(&options, &policy).unwrap_err();
        assert!(
            matches!(*err, TraverserError::LockedOption { ref key } if key == EVALUATION_TIMEOUT)
        );
    }

    #[test]
    fn a_locked_keys_policy_default_still_applies_when_the_query_omits_it() {
        let policy = ExecutionPolicy::permissive()
            .with_locked(EVALUATION_TIMEOUT)
            .with_default(EVALUATION_TIMEOUT, 2000);
        let parsed = ExecutionOptions::parse(&HashMap::new(), &policy).unwrap();
        assert_eq!(parsed.evaluation_timeout, Some(Duration::from_millis(2000)));
    }

    #[test]
    fn a_default_with_nothing_locked_can_still_be_overridden_by_the_query() {
        let policy = ExecutionPolicy::permissive().with_default(EVALUATION_TIMEOUT, 2000);
        // The policy default applies when the query says nothing.
        let parsed = ExecutionOptions::parse(&HashMap::new(), &policy).unwrap();
        assert_eq!(parsed.evaluation_timeout, Some(Duration::from_millis(2000)));
        // But the query can still override it, since it isn't locked.
        let parsed = ExecutionOptions::parse(
            &options(ElementProperty::Number(Number::from_i64(9000))),
            &policy,
        )
        .unwrap();
        assert_eq!(parsed.evaluation_timeout, Some(Duration::from_millis(9000)));
    }

    #[test]
    fn locking_repeat_max_loops_rejects_a_query_override_and_keeps_its_own_default() {
        let policy = ExecutionPolicy::permissive()
            .with_locked(REPEAT_MAX_LOOPS)
            .with_default(REPEAT_MAX_LOOPS, 5);
        let options = HashMap::from([(
            CompactString::new(REPEAT_MAX_LOOPS),
            ElementProperty::Number(Number::from_i64(999_999_999)),
        )]);
        let err = ExecutionOptions::parse(&options, &policy).unwrap_err();
        assert!(
            matches!(*err, TraverserError::LockedOption { ref key } if key == REPEAT_MAX_LOOPS)
        );

        let parsed = ExecutionOptions::parse(&HashMap::new(), &policy).unwrap();
        assert_eq!(parsed.repeat_max_loops, 5);
    }

    #[test]
    fn every_known_option_key_is_locked_through_the_same_generic_check() {
        // A locked-with-no-policy-default key still rejects the query's own value and falls back
        // to the engine's hardcoded default, for every key this crate currently recognises — not
        // just evaluationTimeout/repeat.max_loops.
        for key in KNOWN_OPTION_KEYS {
            let policy = ExecutionPolicy::permissive().with_locked(*key);
            let value = if *key == OPTIMIZER_DISABLED || *key == OPTIMIZER_ENABLED {
                rule_names(&["filter_reorder"])
            } else if *key == PATH_ANALYSIS || *key == BULK_MERGE || *key == BULK_ONE {
                ElementProperty::Boolean(false)
            } else if *key == REPEAT_ORDER {
                ElementProperty::String("dfs".into())
            } else {
                ElementProperty::Number(Number::from_i64(42))
            };
            let options = HashMap::from([(CompactString::new(*key), value)]);
            let err = ExecutionOptions::parse(&options, &policy).unwrap_err();
            assert!(
                matches!(*err, TraverserError::LockedOption { key: ref k } if k == key),
                "key {key} did not reject a query override via the generic lock check: {err}"
            );
        }
    }

    #[test]
    fn permissive_policy_matches_pre_policy_behavior() {
        // The implicit policy for any GraphStorage that doesn't override execution_policy(): no
        // key locked, no default overridden, so parse() behaves exactly as it did before this
        // mechanism existed.
        let permissive = ExecutionPolicy::permissive();
        assert_eq!(
            ExecutionOptions::parse(&HashMap::new(), &permissive).unwrap(),
            ExecutionOptions::default()
        );
        let options = HashMap::from([(
            CompactString::new(EVALUATION_TIMEOUT),
            ElementProperty::Number(Number::from_i64(1234)),
        )]);
        let parsed = ExecutionOptions::parse(&options, &permissive).unwrap();
        assert_eq!(parsed.evaluation_timeout, Some(Duration::from_millis(1234)));
    }
}
