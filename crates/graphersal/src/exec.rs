//! Execution control for hosts: limits and their locks, the process memory ceiling,
//! cancellation, result rendering.
//!
//! For a host that runs other people's queries (a server, a REPL, a notebook): an
//! [`ExecutionPolicy`] sets and locks administrative options, a [`CancelToken`] stops a running
//! execution, [`RenderOptions`] bound a displayed result. Query writers need only
//! [`crate::prelude`]. Guides: the book pages `users_guide/resource_limits.md` and
//! `users_guide/displaying_results.md`.
//!
//! # Memory: two limits
//!
//! - The **process memory ceiling** (`memory.limit`): ONE process-wide setting, on by default,
//!   that every execution, import, applied change set and commit is checked against, so a query
//!   that would take the process past it fails (and is rolled back) instead of the operating
//!   system killing the process. Set it once with [`set_memory_limit`] (bytes, `auto`, or
//!   `none` to turn it off) and [`set_memory_headroom`]; [`memory_limit`] reports the value in
//!   effect and its [`MemoryLimitSource`]. Unset, it is the cgroup memory limit minus a headroom
//!   (a container), else 80 % of the physical memory, else 3.5 GiB on `wasm32`, else none.
//!   With a tracking allocator installed (`alloc_tracking`) the process's live heap is compared,
//!   otherwise an estimate (the graph's footprint plus what the query holds and adds).
//!   Loads, store opens, recoveries, checkpoints, backup verifies and GraphSON/GraphML loads
//!   check it BEFORE they allocate (a graph that does not fit refuses the open, naming the
//!   minimum). A host that allocates data-proportional memory itself (a model it loads) asks
//!   first with [`check_memory_for`]; [`memory_room`] reports the figures ([`MemoryRoom`]).
//! - The **per-query limit** `traversal.max_materialized_bytes`: an execution option (lockable
//!   through [`ExecutionPolicy`]) bounding what one query materializes.

pub use crate::graph::ExecutionPolicy;
pub use crate::traversal::memory_limit::{
    MemoryLimit, MemoryLimitSource, MemoryRoom, ProcessMemoryLimit, check_memory_for, memory_limit,
    memory_limit_scope, memory_room, set_memory_headroom, set_memory_limit,
};
pub use crate::traversal::{
    CancelToken, GlobPathOptions, RenderOptions, RenderedOutput, Spelling, Truncation,
    optimizer_rule_names, traversal_limit_defaults,
};
