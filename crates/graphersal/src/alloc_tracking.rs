//! Exact, allocator-level per-step memory tracking (D4 of the memory-inspection spec). Entirely
//! behind the `alloc-tracking` feature: with the feature off, this module does not exist in the
//! compiled crate at all, not merely disabled code.
//!
//! [`TrackingAllocator`] must be installed by the **binary**, not `graphersal` itself — the
//! `#[global_allocator]` attribute can only be set once, by the final binary, never by a library
//! crate. The CLI (`graphersal`, in its `main.rs`), the browser playground's WebAssembly module
//! (`graphersal-wasm`, in its `lib.rs`) and the Python extension (`py-graphersal`, in its `lib.rs`)
//! install it by default (see CLAUDE.md). A consumer that enables this feature but never installs
//! [`TrackingAllocator`] will see [`crate::MemorySource::Tracked`] figures that read `0` for every
//! step — indistinguishable, from inside this library, from "genuinely allocated nothing." That is
//! a deliberate, documented limitation, not a bug: `graphersal` cannot verify from library code
//! whether the binary actually wired the allocator in.
//!
//! **Deliberately not built:** using this inside [`crate::graph::GraphStorage::memory_usage`]. A
//! process may hold more than one graph at once, and a thread-local (or process-wide) allocator
//! counter cannot attribute bytes to *one specific graph* among several — `memory_usage()` stays on
//! direct structure introspection (see `graph/memory.rs`).

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicI64, Ordering};

/// Net (live) heap bytes of the whole process, across all threads: what the process memory ceiling
/// (`memory.limit`) compares against when a [`TrackingAllocator`] is installed. A relaxed atomic,
/// updated on every allocation, deallocation and resizing `realloc` next to the thread-local
/// counters; it reads `0` when no [`TrackingAllocator`] is installed.
static GLOBAL_LIVE: AtomicI64 = AtomicI64::new(0);

thread_local! {
    /// Monotonically increasing count of bytes this thread has ever requested from the allocator.
    /// Never decremented — a per-step *delta* of this counter (exit minus entry) is therefore
    /// always `>= 0` and captures gross allocation churn, including memory a step allocated and
    /// freed again within its own execution (the `size_of`-based estimation path, D3, cannot see
    /// this; capturing it is the whole point of this module).
    static THREAD_ALLOCATED: Cell<u64> = const { Cell::new(0) };
    /// Net (live) bytes this thread currently holds: incremented on `alloc`/`alloc_zeroed` and on
    /// a growing `realloc`, decremented on `dealloc` and on a shrinking `realloc`. Unlike
    /// [`THREAD_ALLOCATED`], a per-step delta of this counter (exit minus entry) can be negative —
    /// a step that frees more than it allocates (e.g. one that drops a prior step's large
    /// intermediate) genuinely shows negative net retention, and that is reported honestly rather
    /// than clamped to zero (see the richer-memory-diagnostics spec, D1).
    static THREAD_LIVE: Cell<i64> = const { Cell::new(0) };
    /// Number of allocator calls this thread has made: one per `alloc()`/`alloc_zeroed()` call,
    /// plus one per `realloc()` call that grows the allocation (a shrinking `realloc` is not
    /// counted as "more work asked of the allocator" for this purpose). High counts relative to a
    /// step's output size signal per-item/per-small-batch allocation churn.
    static THREAD_ALLOC_COUNT: Cell<u64> = const { Cell::new(0) };
}

/// A byte-counting wrapper around another global allocator. Install it as the process's
/// `#[global_allocator]` to let `.profile()` report real ([`crate::MemorySource::Tracked`])
/// per-step allocation figures instead of estimated ones. Generic over the wrapped allocator so a
/// consumer already using a non-default allocator (jemalloc, mimalloc, ...) can still wrap it.
///
/// Counting is **thread-local, not global**, specifically so that concurrent, unrelated allocation
/// on other threads (e.g. a server handling another request concurrently) does not
/// contaminate one query's measured numbers. This only holds as long as a single query's execution
/// runs start-to-finish on one thread without hopping between threads mid-query — true for this
/// engine's synchronous, `RwLock`-guarded execution model. A future async/work-stealing execution
/// path that could move an in-flight query across threads would silently corrupt this attribution;
/// on wasm32 (`graphersal-wasm`) there is only one thread, so the assumption holds trivially.
///
/// **The always-on cost this trades in:** once installed, *every* allocation anywhere in the
/// process — profiled query or not, `graphersal` or unrelated code in the same binary — pays one
/// `thread_local` read-modify-write per `alloc()`/`realloc()` call, plus one relaxed atomic add on
/// the process-wide live-bytes counter ([`global_live_bytes`], read by the process memory ceiling
/// `memory.limit`). The thread-local part has no cross-thread synchronization cost; the atomic
/// shares one cache line between threads that allocate concurrently. It is not literally zero;
/// see this crate's benchmarks for a measured percentage.
pub struct TrackingAllocator<A = System> {
    inner: A,
}

impl TrackingAllocator<System> {
    /// Wraps the process's default [`System`] allocator.
    pub const fn new() -> Self {
        Self { inner: System }
    }
}

impl Default for TrackingAllocator<System> {
    fn default() -> Self {
        Self::new()
    }
}

impl<A> TrackingAllocator<A> {
    /// Wraps an arbitrary [`GlobalAlloc`] implementation, for a consumer already using a
    /// non-default allocator.
    pub const fn wrapping(inner: A) -> Self {
        Self { inner }
    }
}

// SAFETY: every method is a pure pass-through to `self.inner`'s implementation of the same
// `unsafe` contract, with an extra, side-effect-free thread-local counter update on the sizes
// involved. This upholds `GlobalAlloc`'s safety contract exactly as `self.inner` does, since the
// wrapper never changes which allocation is made, only records its size.
unsafe impl<A: GlobalAlloc> GlobalAlloc for TrackingAllocator<A> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        unsafe {
            THREAD_ALLOCATED.with(|c| c.set(c.get() + layout.size() as u64));
            THREAD_LIVE.with(|c| c.set(c.get() + layout.size() as i64));
            THREAD_ALLOC_COUNT.with(|c| c.set(c.get() + 1));
            GLOBAL_LIVE.fetch_add(layout.size() as i64, Ordering::Relaxed);
            self.inner.alloc(layout)
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe {
            THREAD_LIVE.with(|c| c.set(c.get() - layout.size() as i64));
            GLOBAL_LIVE.fetch_sub(layout.size() as i64, Ordering::Relaxed);
            self.inner.dealloc(ptr, layout)
        }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        unsafe {
            THREAD_ALLOCATED.with(|c| c.set(c.get() + layout.size() as u64));
            THREAD_LIVE.with(|c| c.set(c.get() + layout.size() as i64));
            THREAD_ALLOC_COUNT.with(|c| c.set(c.get() + 1));
            GLOBAL_LIVE.fetch_add(layout.size() as i64, Ordering::Relaxed);
            self.inner.alloc_zeroed(layout)
        }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        unsafe {
            if new_size > layout.size() {
                let grown = (new_size - layout.size()) as u64;
                THREAD_ALLOCATED.with(|c| c.set(c.get() + grown));
                THREAD_LIVE.with(|c| c.set(c.get() + grown as i64));
                THREAD_ALLOC_COUNT.with(|c| c.set(c.get() + 1));
                GLOBAL_LIVE.fetch_add(grown as i64, Ordering::Relaxed);
            } else if new_size < layout.size() {
                let shrunk = (layout.size() - new_size) as i64;
                THREAD_LIVE.with(|c| c.set(c.get() - shrunk));
                GLOBAL_LIVE.fetch_sub(shrunk, Ordering::Relaxed);
            }
            self.inner.realloc(ptr, layout, new_size)
        }
    }
}

/// This thread's current value of the monotonic allocated-bytes counter. `0` both when nothing has
/// been allocated yet on this thread *and* when no [`TrackingAllocator`] is actually installed —
/// the library cannot distinguish the two from inside itself (see [`crate::MemorySource::Tracked`]'s
/// doc comment).
pub fn thread_allocated_bytes() -> u64 {
    THREAD_ALLOCATED.with(Cell::get)
}

/// This thread's current net (live) byte count: bytes allocated minus bytes freed so far, signed
/// because a thread can (transiently, mid-step) hold a negative delta relative to an earlier
/// snapshot if it frees more than it allocates in between the two reads. The absolute value at any
/// single point in time is never negative in practice (a thread cannot free more than it has ever
/// held), but a *delta* between two snapshots — which is how `.profile()` uses this — can be.
/// Like [`thread_allocated_bytes`], reads `0` both before any allocation and when no
/// [`TrackingAllocator`] is installed.
pub fn thread_live_bytes() -> i64 {
    THREAD_LIVE.with(Cell::get)
}

/// The whole process's current net (live) heap bytes, across all threads: allocated minus freed
/// since start, as counted by an installed [`TrackingAllocator`] (one relaxed atomic, so a read
/// costs a plain load). Reads `0` when no [`TrackingAllocator`] is installed. The memory ceiling
/// (`memory.limit`) compares this figure with its limit when it is above `0`, and falls back to
/// the engine's estimate otherwise.
pub fn global_live_bytes() -> u64 {
    GLOBAL_LIVE.load(Ordering::Relaxed).max(0) as u64
}

/// This thread's current count of allocator calls: one per `alloc()`/`alloc_zeroed()`, plus one
/// per growing `realloc()`. A per-step delta of this counter is `.profile()`'s `alloc_count`
/// figure — high relative to a step's output size signals per-item allocation churn. Reads `0`
/// both before any allocation and when no [`TrackingAllocator`] is installed, same caveat as
/// [`thread_allocated_bytes`].
pub fn thread_alloc_count() -> u64 {
    THREAD_ALLOC_COUNT.with(Cell::get)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counter_is_monotonic_and_thread_local() {
        let before = thread_allocated_bytes();
        // This test does not install `TrackingAllocator` as the global allocator (that can only
        // happen once, process-wide), so it only exercises the counter's own read/increment
        // plumbing directly.
        THREAD_ALLOCATED.with(|c| c.set(c.get() + 128));
        let after = thread_allocated_bytes();
        assert_eq!(after, before + 128);

        let handle = std::thread::spawn(thread_allocated_bytes);
        let other_thread = handle.join().unwrap();
        assert_eq!(
            other_thread, 0,
            "a fresh thread starts its own counter at 0"
        );
    }

    #[test]
    fn live_and_alloc_count_counters_are_independent_and_thread_local() {
        let live_before = thread_live_bytes();
        let count_before = thread_alloc_count();

        THREAD_LIVE.with(|c| c.set(c.get() + 256));
        THREAD_ALLOC_COUNT.with(|c| c.set(c.get() + 1));
        assert_eq!(thread_live_bytes(), live_before + 256);
        assert_eq!(thread_alloc_count(), count_before + 1);

        THREAD_LIVE.with(|c| c.set(c.get() - 300));
        assert!(
            thread_live_bytes() < live_before,
            "net live bytes can go negative relative to an earlier snapshot"
        );

        let handle = std::thread::spawn(|| (thread_live_bytes(), thread_alloc_count()));
        let (other_live, other_count) = handle.join().unwrap();
        assert_eq!(
            other_live, 0,
            "a fresh thread starts its own live counter at 0"
        );
        assert_eq!(
            other_count, 0,
            "a fresh thread starts its own alloc-count counter at 0"
        );
    }
}
