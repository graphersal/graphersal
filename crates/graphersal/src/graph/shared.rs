//! [`Graph`]: a storage behind a read-write lock, the shape a graph is shared in.

use std::fmt;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use parking_lot::lock_api::{RawRwLock as _, RawRwLockUpgrade as _};
use web_time::Instant;

use crate::error::GraphError;
use crate::graph::workload::{SeriesKey, SeriesKind, WorkloadLevel, WorkloadStats};
use crate::graph::{
    ChangeSet, CommitMetadata, GraphStorage, RollbackReason, StorageIdentity, TraversalGraph,
    UnitChanges, UnitMark,
};

/// A shareable, lock-protected graph: a [`GraphStorage`] `S` behind a read-write lock.
///
/// It is what [`GraphSource`](crate::GraphSource)'s constructors return and what the `script`
/// feature's entry points (`graph_scope`, every `eval_*`) take, as an `Arc<Graph<S>>` of any
/// storage `S: GraphStorage + Default + Send + Sync + 'static` (through
/// [`IntoAnyGraph`](crate::graph::IntoAnyGraph)); a host storage is shared the same way
/// (the book page `for_developers/custom_storage.md`); `Graph::new_persistent` (feature
/// `persist`) wraps a `PersistentStorage`. The type parameter defaults to [`TraversalGraph`], so `Graph` and `Arc<Graph>` in type position mean
/// `Graph<TraversalGraph>` and `Arc<Graph<TraversalGraph>>`; an expression takes `S` from its
/// argument (`Graph::new(storage)`).
///
/// [`read`](Self::read) shares the lock, [`write`](Self::write) takes it exclusively; the
/// guards dereference to `S`. `Graph<S>` is `Send`/`Sync` exactly when `S` is.
///
/// # Units across locks
///
/// A unit opened on the storage through a [`write`](Self::write) guard
/// (`graph.write().begin_unit()`) must be closed under that same guard: once the guard is
/// dropped, every other thread's [`read`](Self::read) sees the unit's uncommitted changes (and
/// a Store checkpoint could write them to disk). To keep ONE unit over several locks (a whole
/// script, a playground run), open it on the graph instead with [`begin_unit`](Self::begin_unit)
/// and close it with [`commit_unit`](Self::commit_unit), [`rollback_unit`](Self::rollback_unit)
/// or [`discard_unit`](Self::discard_unit) on the same thread. While the unit is open:
///
/// - the opening thread OWNS it: its own [`read`](Self::read) and [`write`](Self::write) calls
///   return views of the held lock instead of locking again (so the script's traversals run
///   inside the unit);
/// - until the unit's first [`write`](Self::write), it holds the lock UPGRADABLE: other
///   threads keep reading (they see the committed state, because nothing changed yet), while
///   other writers and other units wait;
/// - its first [`write`](Self::write) makes the lock exclusive (it waits for the current
///   readers) and opens the storage's unit ([`GraphStorage::begin_unit_at`]); the lock stays
///   exclusive until the unit closes: from then on other threads' reads wait too, so nobody
///   ever sees uncommitted changes;
/// - nested [`begin_unit`](Self::begin_unit) calls on the owning thread open savepoints (on
///   the storage too once the unit wrote, else recorded and opened in order at its first
///   write);
/// - opening a unit, and closing one that never wrote, take no exclusive moment: they never
///   wait for the readers in flight (only a dry run that never wrote,
///   [`discard_unit`](Self::discard_unit), takes the lock exclusively to ask the storage for
///   its change set);
/// - the unit's time ([`GraphStorage::unit_started_at`], read by `defaultFunction`
///   `now`/`today` and the GQL clocks) is pinned when the unit opens, so the unit's queries
///   read one value before and after its first write: the owner's [`read`](Self::read) views
///   of a [`TraversalGraph`] report it before the storage's unit opens, and a script's
///   traversals inside the unit are pinned to it by the graph for any storage (a host storage's
///   own read view reports `None` until the unit's first write, as before a unit opens).
///
/// The owning thread must not hold a [`write`](Self::write) view together with another view of
/// the same graph (a `&mut S` would alias a `&S`): outside a unit this deadlocks (the lock is
/// not reentrant); inside one, [`read`](Self::read) and [`write`](Self::write) panic (see their
/// "Panics"), while [`read_checked`](Self::read_checked), [`write_checked`](Self::write_checked),
/// [`try_read`](Self::try_read), [`try_write`](Self::try_write),
/// [`try_write_checked`](Self::try_write_checked), [`begin_unit`](Self::begin_unit)
/// and the closes return [`GraphError::GraphViewHeld`] (or `None`) instead.
/// [`try_write_checked`](Self::try_write_checked) is the non-blocking write that tells "busy"
/// (`Ok(None)`, try again later) from "not allowed on this thread" (`Err`, drop the other view
/// first), which [`try_write`](Self::try_write) answers both as `None`. Opening a unit on
/// a thread that already holds a guard of this graph taken outside a unit deadlocks (at the
/// unit's first write at the latest), as `write()` does there.
///
/// ```rust
/// use graphersal::changes::RollbackReason;
/// use graphersal::prelude::*;
///
/// let graph = Graph::new(TraversalGraph::tinkerpop_modern());
/// let mark = graph.begin_unit().unwrap();
/// // Reads of the unit's thread run inside it; other threads read on meanwhile.
/// assert_eq!(graph.read().vertex_count(), 6);
/// // The first write makes the unit exclusive until it closes.
/// GraphTraversalSource::new(&mut *graph.write()).add_v("person").next().unwrap();
/// assert_eq!(graph.read().vertex_count(), 7);
/// graph.rollback_unit(mark, &RollbackReason::failed(&"not wanted")).unwrap();
/// assert_eq!(graph.read().vertex_count(), 6);
/// ```
///
/// ```rust
/// use graphersal::prelude::*;
///
/// let graph = Graph::new(TraversalGraph::tinkerpop_modern());
/// assert_eq!(graph.read().vertex_count(), 6);
/// GraphTraversalSource::new(&mut *graph.write()).add_v("person").next().unwrap();
/// assert_eq!(graph.into_inner().vertex_count(), 7);
/// ```
pub struct Graph<S: GraphStorage = TraversalGraph> {
    lock: parking_lot::RwLock<S>,
    /// The packed-snapshot writer of `S`, set by [`Graph::new_persistent`]: the dyn boundary
    /// ([`AnyGraph::write_snapshot`](crate::graph::AnyGraph::write_snapshot)) cannot test
    /// `S: PersistentStorage` itself (`book/src/for_developers/custom_storage.md`).
    #[cfg(feature = "persist")]
    snapshot_writer: Option<SnapshotWriter<S>>,
    /// The workload statistics the lock's wait and hold times go to
    /// ([`Graph::attach_workload_stats`]; a Store attaches its own).
    workload: OnceLock<Arc<WorkloadStats>>,
    /// The unit a thread holds across locks ([`Graph::begin_unit`]).
    unit: UnitOwner,
    /// Set for good when a rollback of one of the graph's units failed ([`Graph::is_poisoned`]).
    poison: Poison,
}

/// The poison of a [`Graph`]: set once a rollback of one of its units failed, never cleared.
///
/// `cause` is written before `flag`, so a reader that sees the flag also sees the cause.
#[derive(Default)]
struct Poison {
    flag: AtomicBool,
    cause: OnceLock<String>,
}

impl Poison {
    /// Poisons the graph with the text of the failed rollback (the first cause stays).
    #[cold]
    fn set(&self, cause: String) {
        tracing::error!("the graph is poisoned: a rollback failed: {cause}");
        let _ = self.cause.set(cause);
        self.flag.store(true, Ordering::Release);
    }

    /// `Err(`[`GraphError::Poisoned`]`)` once the graph is poisoned: one atomic load.
    #[inline]
    fn check(&self) -> Result<(), GraphError> {
        match self.flag.load(Ordering::Acquire) {
            false => Ok(()),
            true => Err(self.error()),
        }
    }

    #[cold]
    fn error(&self) -> GraphError {
        GraphError::poisoned(self.cause.get().cloned().unwrap_or_default())
    }
}

thread_local! {
    /// The rollback watches open on this thread, innermost last ([`RollbackWatch`]).
    static ROLLBACK_WATCHES: std::cell::RefCell<RollbackWatches> =
        const { std::cell::RefCell::new(RollbackWatches { next: 1, open: Vec::new() }) };
}

/// The rollback watches of one thread: the next id and the open watches, innermost last.
struct RollbackWatches {
    next: u64,
    open: Vec<OpenWatch>,
}

/// One open [`RollbackWatch`]: which storage it watches and the cause of the first failed
/// rollback of that storage noted while it was open.
struct OpenWatch {
    id: u64,
    storage: WatchedStorage,
    cause: Option<String>,
}

/// The storage of one [`Graph`], as a [`RollbackWatch`] recognizes it: its
/// [`GraphStorage::storage_identity`] and the memory of the `S` inside the graph's lock.
#[derive(Clone, Copy)]
struct WatchedStorage {
    /// [`GraphStorage::storage_identity`] of the graph's storage.
    identity: StorageIdentity,
    /// The address of the `S` inside the graph's lock.
    start: usize,
    /// `size_of::<S>()`.
    len: usize,
}

impl WatchedStorage {
    /// The storage of `graph`, read through `storage` (the graph's own `S`, borrowed from a view
    /// that holds the lock).
    fn of<S: GraphStorage>(graph: &Graph<S>, storage: &S) -> Self {
        Self {
            identity: storage.storage_identity(),
            start: graph.lock.data_ptr() as usize,
            len: std::mem::size_of::<S>(),
        }
    }

    /// Whether the storage with identity `identity` is this graph's storage: the same identity,
    /// or an address inside the graph's `S` (a storage nested inline in the graph's storage,
    /// such as the graph a wrapper storage forwards to, rolling back on its own). The `S` of two
    /// live graphs never overlap, so at most one graph matches.
    fn contains(&self, identity: StorageIdentity) -> bool {
        identity == self.identity || identity.address().wrapping_sub(self.start) < self.len
    }
}

/// How a [`Graph`] learns that a rollback of ITS storage failed, wherever the engine ran it.
///
/// Every write view of a graph ([`GraphWrite`]) and every storage call that closes a held unit
/// opens a watch on its thread, naming the graph's storage ([`WatchedStorage`]); the engine's
/// rollback paths (`graph::storage::rollback_failed_unit`, `rollback_unwound`,
/// `Transactional::transaction_with` and `dry_run`, the closes of held units) report a failure
/// with [`note_failed_rollback`], naming the storage that failed by its
/// [`GraphStorage::storage_identity`], and the graph is poisoned when the watch of THAT storage
/// ends (before the view releases the lock, also while a panic unwinds).
///
/// Why the storage's identity and not the innermost watch, a token passed down, or the
/// graph observing results: a process holds many independent graphs, and one thread may have
/// write views of several graphs open (nested, dropped out of order) or roll back a bare
/// storage beside them; only the failing storage itself says reliably which graph it belongs
/// to. A token would have to travel through every storage-generic engine path
/// (`GraphTraversalSource` holds a plain `S`, `Transactional` runs on any `&mut S`) and the
/// graph cannot see the outcome of what a host runs on its write guard. The identity comes
/// from the storage after the wrapper impls (`&mut G`, `&G`, `Box<G>`, ...) forwarded, so the
/// same storage gives the same identity on the watch's side and on the rollback's side;
/// containment in the graph's `S` covers a storage nested inline that rolls back on its own.
/// A rollback of a storage no open watch names (a bare storage, a storage taken out with
/// `get_mut`) poisons nothing: poisoning is a property of [`Graph`].
///
/// The cost is one thread-local push and pop and one `storage_identity` call per write view;
/// the search over the open watches happens only when a rollback fails, never per element.
struct RollbackWatch(u64);

impl RollbackWatch {
    /// Opens a watch of `storage` as the thread's innermost one (id 0: the thread is exiting,
    /// nothing is watched).
    fn start(storage: WatchedStorage) -> Self {
        let id = ROLLBACK_WATCHES
            .try_with(|watches| match watches.try_borrow_mut() {
                Ok(mut watches) => {
                    let id = watches.next;
                    watches.next = watches.next.wrapping_add(1).max(1);
                    watches.open.push(OpenWatch {
                        id,
                        storage,
                        cause: None,
                    });
                    id
                }
                Err(_) => 0,
            })
            .unwrap_or(0);
        RollbackWatch(id)
    }

    /// Ends the watch: the cause of a failed rollback of its storage noted while it was open.
    fn finish(&mut self) -> Option<String> {
        let id = std::mem::replace(&mut self.0, 0);
        if id == 0 {
            return None;
        }
        ROLLBACK_WATCHES
            .try_with(|watches| {
                let mut watches = watches.try_borrow_mut().ok()?;
                let at = watches.open.iter().rposition(|open| open.id == id)?;
                watches.open.remove(at).cause
            })
            .ok()
            .flatten()
    }
}

impl Drop for RollbackWatch {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

/// Reports a failed rollback (or discard) of a unit of the storage with identity `storage`
/// ([`GraphStorage::storage_identity`]) to the innermost open [`RollbackWatch`] of that storage
/// on this thread, so exactly the [`Graph`] that holds the storage is poisoned when the watch
/// ends. Without such a watch (a bare storage, or a graph with no write view open on this
/// thread) nothing happens; the watches of other graphs are never touched.
#[cold]
pub(crate) fn note_failed_rollback(identity: StorageIdentity, cause: &dyn fmt::Display) {
    let _ = ROLLBACK_WATCHES.try_with(|watches| {
        if let Ok(mut watches) = watches.try_borrow_mut()
            && let Some(open) = watches
                .open
                .iter_mut()
                .rev()
                .find(|open| open.storage.contains(identity))
            && open.cause.is_none()
        {
            open.cause = Some(cause.to_string());
        }
    });
}

/// Whether a failed close of a unit (a rollback or a discard) leaves the storage in an unknown
/// state: every error except the `Unsupported` of a dry run on a storage without change capture,
/// which closes the unit by a successful rollback ([`GraphStorage::discard_unit`]'s default).
pub(crate) fn close_failed(err: &GraphError) -> bool {
    !matches!(err, GraphError::Unsupported { operation }
        if *operation == crate::graph::storage::DRY_RUN_OPERATION)
}

/// The owner of the unit a thread holds across locks ([`Graph::begin_unit`]) and its state.
///
/// While `thread` names a thread, that thread holds the raw lock UPGRADABLE (`exclusive` false)
/// or EXCLUSIVE (`exclusive` true). Only the owning thread reads `state` or changes either
/// field, so the mutex is never contended; it is never held across a storage call (a storage
/// call that reads the graph again on the same thread must not deadlock on it).
#[derive(Default)]
struct UnitOwner {
    /// The [`thread_token`] of the owning thread, [`NO_OWNER`] when no unit is held.
    thread: AtomicU64,
    state: parking_lot::Mutex<HeldUnit>,
}

/// The state of a held unit.
#[derive(Default)]
struct HeldUnit {
    /// Open units of the owner ([`Graph::begin_unit`] minus the closes): 1 = the outermost.
    depth: u32,
    /// The storage's marks of the levels opened on the storage, outermost first. The
    /// storage's units open lazily, all at once, when the lock becomes exclusive (the unit's
    /// first write); from then on a nested level opens right away. Fewer marks than `depth`
    /// while the unit has not written: the missing levels have nothing on the storage.
    marks: Vec<UnitMark>,
    /// The unit's time, pinned at the outermost begin
    /// ([`GraphStorage::next_unit_started_at`]) and handed to
    /// [`GraphStorage::begin_unit_at`] when the storage's unit opens.
    started_at: Option<i128>,
    /// Whether the owner holds the lock exclusively (since its first `write()`).
    exclusive: bool,
    /// Live read views ([`GraphRead`]) of the owner.
    reads: u32,
    /// Live write views ([`GraphWrite`]) of the owner (at most one).
    writes: u32,
    /// When the lock became exclusive, for `write.hold` (statistics attached only).
    exclusive_since: Option<Instant>,
}

/// [`UnitOwner::thread`] when no thread holds a unit; never a [`thread_token`].
const NO_OWNER: u64 = 0;

thread_local! {
    /// The times pinned for the units this thread holds ([`Graph::begin_unit`]), by the graph's
    /// storage ([`WatchedStorage`], matched like a [`RollbackWatch`]): until the storage's own
    /// unit opens (the unit's first write), a storage that knows this table
    /// ([`TraversalGraph`]'s `unit_started_at`) reports the pinned time through the owner's read
    /// views, also from inside a wrapper storage (`Box<TraversalGraph>`, a storage holding a
    /// `TraversalGraph` inline). Only the owning thread sees an entry; a thread may hold units of
    /// several graphs at once.
    static HELD_UNIT_CLOCKS: std::cell::RefCell<Vec<(WatchedStorage, i128)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// The time pinned for the unit the current thread holds on the storage with identity
/// `storage` (`None`: no such unit, or its storage has no unit clock).
pub(crate) fn held_unit_clock(storage: StorageIdentity) -> Option<i128> {
    HELD_UNIT_CLOCKS
        .try_with(|clocks| {
            clocks
                .borrow()
                .iter()
                .rev()
                .find(|(held, _)| held.contains(storage))
                .map(|(_, started_at)| *started_at)
        })
        .ok()
        .flatten()
}

/// Records the time pinned for the unit this thread holds on `storage`.
fn pin_held_unit_clock(storage: WatchedStorage, started_at: i128) {
    let _ = HELD_UNIT_CLOCKS.try_with(|clocks| {
        let mut clocks = clocks.borrow_mut();
        clocks.retain(|(held, _)| held.start != storage.start);
        clocks.push((storage, started_at));
    });
}

/// Removes the pinned time of the unit this thread held on the graph whose `S` starts at
/// `start` (the graph's lock data pointer).
fn unpin_held_unit_clock(start: usize) {
    let _ = HELD_UNIT_CLOCKS.try_with(|clocks| {
        clocks.borrow_mut().retain(|(held, _)| held.start != start);
    });
}

/// A number for the current thread, unique in the process, never [`NO_OWNER`]; `None` only
/// while the thread's locals are being destroyed.
fn thread_token() -> Option<u64> {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    thread_local! {
        static TOKEN: u64 = NEXT.fetch_add(1, Ordering::Relaxed);
    }
    TOKEN.try_with(|token| *token).ok()
}

/// The error of asking for a view of the held lock while another view is alive where it must
/// not be (a `&mut S` would alias a `&S`; outside a unit the same code deadlocks).
#[cold]
fn view_held(operation: &'static str) -> GraphError {
    GraphError::GraphViewHeld { operation }
}

/// The panic of the infallible [`Graph::read`] / [`Graph::write`] for such an error (their
/// "Panics" section); the engine asks through the checked forms instead.
#[cold]
#[track_caller]
fn view_panic(err: &GraphError) -> ! {
    panic!(
        "Graph: {err} (outside a unit this deadlocks; Graph::read_checked, \
         Graph::write_checked, Graph::try_read and Graph::try_write report it instead)"
    )
}

/// The panic of the infallible [`Graph::read`] / [`Graph::write`] on a poisoned graph (their
/// "Panics" section).
#[cold]
#[track_caller]
fn poison_panic(err: &GraphError) -> ! {
    panic!(
        "Graph: {err} (Graph::read_checked, Graph::write_checked and Graph::is_poisoned report \
         it instead)"
    )
}

/// The panic of [`Graph::read`] / [`Graph::write`] for an error of the checked forms.
#[cold]
#[track_caller]
fn checked_panic(err: &GraphError) -> ! {
    match err {
        GraphError::Poisoned { .. } => poison_panic(err),
        _ => view_panic(err),
    }
}

/// The decrement of a held view's counter when the view is dropped (also on unwind).
struct HeldView<'a> {
    unit: &'a UnitOwner,
    write: bool,
}

impl Drop for HeldView<'_> {
    fn drop(&mut self) {
        let mut held = self.unit.state.lock();
        match self.write {
            true => held.writes = held.writes.saturating_sub(1),
            false => held.reads = held.reads.saturating_sub(1),
        }
    }
}

/// Finishes the close of a held unit when dropped, also when the storage call unwinds (a
/// storage closes its unit before a panic of its own continues, `close_on_unwind`): one level
/// less (and its storage mark dropped); at the outermost level the owner is cleared and the
/// lock released, exclusive or upgradable as it is held.
struct FinishClose<'a, S: GraphStorage> {
    graph: &'a Graph<S>,
}

impl<S: GraphStorage> Drop for FinishClose<'_, S> {
    fn drop(&mut self) {
        let graph = self.graph;
        let mut held = graph.unit.state.lock();
        held.depth = held.depth.saturating_sub(1);
        let depth = held.depth as usize;
        held.marks.truncate(depth);
        if held.depth > 0 {
            return;
        }
        let exclusive = held.exclusive;
        let since = held.exclusive_since.take();
        let pinned = held.started_at.is_some();
        *held = HeldUnit::default();
        drop(held);
        if pinned {
            unpin_held_unit_clock(graph.lock.data_ptr() as usize);
        }
        graph.unit.thread.store(NO_OWNER, Ordering::Release);
        if let Some(since) = since
            && let Some((stats, key)) = lock_series(&graph.workload, "write.hold")
        {
            stats.record(
                key,
                now_micros(),
                crate::graph::workload::Sample::new(0, since.elapsed().as_micros() as u64),
            );
        }
        match exclusive {
            // SAFETY: this thread holds the lock exclusively (since the unit's first write, or
            // made so for the close), and the owner is cleared, so no view refers to it.
            true => unsafe { graph.raw().unlock_exclusive() },
            // SAFETY: this thread holds the lock upgradable (taken by the outermost begin, never
            // upgraded), and the owner is cleared.
            false => unsafe { graph.raw().unlock_upgradable() },
        }
    }
}

/// Releases the upgradable lock taken by an outermost [`Graph::begin_unit`] when a storage call
/// of the begin unwinds (no unit was recorded).
struct AbandonBegin<'a>(&'a parking_lot::RawRwLock);

impl Drop for AbandonBegin<'_> {
    fn drop(&mut self) {
        // SAFETY: armed only while the beginning thread holds the lock upgradable.
        unsafe { self.0.unlock_upgradable() };
    }
}

/// Writes a storage as a packed snapshot with the default options.
#[cfg(feature = "persist")]
pub(crate) type SnapshotWriter<S> =
    fn(
        &S,
        &mut dyn std::io::Write,
    ) -> Result<crate::persist::SnapshotInfo, crate::persist::PersistError>;

/// Shared read access to the storage of a [`Graph`] ([`Graph::read`]); dereferences to `S`.
/// The lock is released when the guard is dropped.
pub struct GraphRead<'a, S: GraphStorage = TraversalGraph> {
    guard: ReadAccess<'a, S>,
    /// Makes the graph's statistics the thread's while the guard lives (executions record into
    /// them).
    _attached: Option<crate::graph::workload::scope::Attached>,
}

/// Exclusive write access to the storage of a [`Graph`] ([`Graph::write`],
/// [`Graph::try_write`]); dereferences (mutably) to `S`. The lock is released when the guard is
/// dropped; with workload statistics attached, the time it was held is counted then.
pub struct GraphWrite<'a, S: GraphStorage = TraversalGraph> {
    guard: WriteAccess<'a, S>,
    /// Learns about a failed rollback while the view lives; the drop poisons the graph then,
    /// before `guard` releases the lock.
    watch: RollbackWatch,
    /// The poison of the graph the view belongs to.
    poison: &'a Poison,
    /// When the lock was acquired and where the hold time goes (statistics attached only).
    held: Option<(Instant, &'a Arc<WorkloadStats>)>,
    /// Makes the graph's statistics the thread's while the guard lives.
    _attached: Option<crate::graph::workload::scope::Attached>,
}

/// How a [`GraphRead`] reaches the storage: its own read lock, or the lock of the unit its
/// thread holds ([`Graph::begin_unit`]).
enum ReadAccess<'a, S> {
    Locked(parking_lot::RwLockReadGuard<'a, S>),
    /// The view counts in the unit's state while it lives (its `Drop`).
    Held(&'a S, #[allow(dead_code)] HeldView<'a>),
}

/// How a [`GraphWrite`] reaches the storage: its own write lock, or the (exclusive) lock of the
/// unit its thread holds ([`Graph::begin_unit`]).
enum WriteAccess<'a, S> {
    Locked(parking_lot::RwLockWriteGuard<'a, S>),
    /// The view counts in the unit's state while it lives (its `Drop`).
    Held(&'a mut S, #[allow(dead_code)] HeldView<'a>),
}

/// The statistics a lock event is counted in (attached), with the series key of `name` under
/// the current thread's channel; `None` when not attached, off, or the work is technical.
fn lock_series<'a>(
    workload: &'a OnceLock<Arc<WorkloadStats>>,
    name: &str,
) -> Option<(&'a Arc<WorkloadStats>, SeriesKey)> {
    let stats = workload.get()?;
    lock_key(stats, name).map(|key| (stats, key))
}

/// The series key of lock event `name` under the current thread's channel, `None` when the
/// statistics are off or the work is technical.
fn lock_key(stats: &WorkloadStats, name: &str) -> Option<SeriesKey> {
    if stats.level() == WorkloadLevel::Off {
        return None;
    }
    let origin = crate::graph::workload::scope::current();
    (!origin.technical).then(|| SeriesKey::new(SeriesKind::Lock(name.into()), origin.channel))
}

/// The current time in microseconds since the Unix epoch.
fn now_micros() -> i64 {
    web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map_or(0, |d| d.as_micros() as i64)
}

impl<S: GraphStorage> Graph<S> {
    /// Wraps `storage` for sharing.
    ///
    /// With feature `persist`: a graph built here can be written as a snapshot through the dyn
    /// boundary (`AnyGraph::write_snapshot`, the DSL's `g.export_snapshot(path)`) only when `S` is [`TraversalGraph`]; any other
    /// storage fails there with `GraphError::Unsupported`. A storage that implements
    /// `PersistentStorage` is wrapped with `Graph::new_persistent` to be writable there too.
    /// (The typed snapshot functions in `graphersal::persist` take any persistent storage
    /// directly.)
    pub fn new(storage: S) -> Self {
        Graph {
            lock: parking_lot::RwLock::new(storage),
            #[cfg(feature = "persist")]
            snapshot_writer: None,
            workload: OnceLock::new(),
            unit: UnitOwner::default(),
            poison: Poison::default(),
        }
    }

    /// Counts the lock's wait and hold times in `stats` from now on (book page "Workload
    /// Statistics"): `write.wait` (asking for the write lock until holding it), `write.hold`
    /// (holding it) and `read.wait` (a reader waiting), each under the channel of the work
    /// that took the lock, nothing for technical work. Set once (a second call is ignored and
    /// returns `false`); a Store attaches its own statistics.
    pub fn attach_workload_stats(&self, stats: Arc<WorkloadStats>) -> bool {
        self.workload.set(stats).is_ok()
    }

    /// The workload statistics attached by [`Self::attach_workload_stats`].
    pub fn workload_stats(&self) -> Option<&Arc<WorkloadStats>> {
        self.workload.get()
    }

    /// The storage advisor's recommendations (book page "Optimize",
    /// [`crate::advisor`]): the memory report and the measured compression candidates under one
    /// read lock, the workload statistics attached to the graph (none: only the data is looked
    /// at, so no index advice). Fails for a cancelled memory report or estimate (the running
    /// thread's [`CancelToken`](crate::CancelToken)), when the current thread may not read the
    /// graph ([`GraphError::GraphViewHeld`]) and when a compression estimate fails for any other
    /// reason than [`GraphError::Unsupported`] (a storage without compressed properties gets
    /// advice without compression recommendations, which is its full answer): the advice is
    /// never silently partial. Unauthorized: a front end asks its authorizer for `Read` on the
    /// data first.
    pub fn advise(
        &self,
        request: &crate::graph::advisor::AdviceRequest,
    ) -> crate::error::TraverserResult<crate::graph::advisor::Advice> {
        let now = web_time::SystemTime::now()
            .duration_since(web_time::UNIX_EPOCH)
            .map_or(0, |d| d.as_micros() as i64);
        let workload = self
            .workload_stats()
            .map(|stats| stats.report_with_history(now, crate::graph::workload::HistoryTier::Days));
        let guard = self.read_checked()?;
        let source = crate::GraphTraversalSource::new(&*guard);
        let memory =
            source.memory_report(&crate::graph::memory_report::MemoryReportRequest::new())?;
        let mut failure = None;
        let mut estimate =
            |request: &crate::graph::compression_estimate::CompressionEstimateRequest| {
                match source.estimate_compression(request) {
                    Ok(estimate) => Some(estimate),
                    // A storage without compressed properties: no compression advice (its
                    // answer, not a failure).
                    Err(err)
                        if matches!(
                            *err,
                            crate::TraverserError::Graph(GraphError::Unsupported { .. })
                        ) =>
                    {
                        None
                    }
                    // Anything else (cancelled, a failing storage) fails the whole advice: an
                    // answer without the compression part would read as "nothing to compress".
                    Err(err) => {
                        failure.get_or_insert(err);
                        None
                    }
                }
            };
        let marks = self
            .workload_stats()
            .map(|stats| stats.advice_marks())
            .unwrap_or_default();
        let advice = crate::graph::advisor::compute(
            &memory,
            workload.as_ref(),
            &marks,
            &mut estimate,
            request,
            now,
        );
        match failure {
            Some(err) => Err(err),
            None => Ok(advice),
        }
    }

    /// The snapshot writer set by [`Graph::new_persistent`].
    #[cfg(feature = "persist")]
    pub(crate) fn snapshot_writer(&self) -> Option<SnapshotWriter<S>> {
        self.snapshot_writer
    }

    /// Locks the graph for reading, blocking while a writer holds it, or a unit that has
    /// written ([`Self::begin_unit`]). Several readers may hold it at once, also together with
    /// a unit that has only read so far. On the thread that holds a unit of this graph it
    /// returns a view of the unit's lock instead (no new lock, nothing counted).
    ///
    /// # Panics
    ///
    /// Like [`RefCell::borrow`](std::cell::RefCell::borrow), when the current thread holds a
    /// unit of this graph and a [`write`](Self::write) view of it is alive on this thread (also
    /// a commit hook or an embedder reaching back into the graph while its unit writes or
    /// commits): a shared view would alias the `&mut S`. [`Self::read_checked`] returns
    /// [`GraphError::GraphViewHeld`] instead, [`Self::try_read`] `None`. (Outside a unit the
    /// same situation deadlocks, because the lock is not reentrant.)
    ///
    /// Also when the graph is poisoned ([`Self::is_poisoned`]: a rollback failed earlier, the
    /// storage is in an unknown state); [`Self::read_checked`] returns [`GraphError::Poisoned`]
    /// instead.
    #[track_caller]
    pub fn read(&self) -> GraphRead<'_, S> {
        match self.read_checked() {
            Ok(guard) => guard,
            Err(err) => checked_panic(&err),
        }
    }

    /// [`Self::read`], but a view the current thread may not have
    /// ([`read`](Self::read)'s "Panics") is [`GraphError::GraphViewHeld`] instead of a panic,
    /// and a poisoned graph [`GraphError::Poisoned`]. Blocks like `read()`.
    pub fn read_checked(&self) -> Result<GraphRead<'_, S>, GraphError> {
        if self.holds_unit() {
            self.poison.check()?;
            return Ok(GraphRead {
                guard: self.held_read()?,
                _attached: self.attached(),
            });
        }
        let guard = match lock_series(&self.workload, "read.wait") {
            None => self.lock.read(),
            Some((stats, key)) => match self.lock.try_read() {
                // Uncontended: no clock read, nothing counted.
                Some(guard) => guard,
                None => {
                    let asked = Instant::now();
                    let guard = self.lock.read();
                    let wait = asked.elapsed().as_micros() as u64;
                    stats.record(
                        key,
                        now_micros(),
                        crate::graph::workload::Sample::new(0, wait),
                    );
                    guard
                }
            },
        };
        // Checked under the lock: a poison set by the writer this reader waited for is seen.
        self.poison.check()?;
        Ok(GraphRead {
            guard: ReadAccess::Locked(guard),
            _attached: self.attached(),
        })
    }

    /// Locks the graph for reading if that is possible right now, else returns `None` without
    /// blocking or panicking: `None` while a writer or a unit that has written holds the lock
    /// (also on this thread: a thread that holds the write lock gets `None` instead of a
    /// deadlock), and on the thread that holds a unit of this graph while a
    /// [`write`](Self::write) view of it is alive there. On the thread that holds a unit it
    /// returns a view of the unit's lock otherwise, as [`Self::read`] does. A successful
    /// `try_read` counts no wait.
    ///
    /// **A poisoned graph ([`Self::is_poisoned`]) answers `None` as well**, exactly like a busy
    /// one, and it stays `None` for good: a caller that retries on `None` must tell the two
    /// apart, or it retries forever. [`Self::is_poisoned`] does so without blocking (one atomic
    /// load); [`Self::read_checked`] returns [`GraphError::Poisoned`] (or
    /// [`GraphError::GraphViewHeld`] for a view this thread may not have) but waits while the
    /// graph is busy.
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let graph = Graph::new(TraversalGraph::tinkerpop_modern());
    /// let answer = match graph.try_read() {
    ///     Some(view) => format!("{} vertices", view.vertex_count()),
    ///     // `None`: busy (retry later) or poisoned (never again).
    ///     None if graph.is_poisoned() => "poisoned: reopen the storage".to_string(),
    ///     None => "busy: try again later".to_string(),
    /// };
    /// assert_eq!(answer, "6 vertices");
    /// ```
    pub fn try_read(&self) -> Option<GraphRead<'_, S>> {
        if self.holds_unit() {
            self.poison.check().ok()?;
            return self.held_read().ok().map(|guard| GraphRead {
                guard,
                _attached: self.attached(),
            });
        }
        let guard = self.lock.try_read()?;
        self.poison.check().ok()?;
        Some(GraphRead {
            guard: ReadAccess::Locked(guard),
            _attached: self.attached(),
        })
    }

    /// The guard that makes this graph's statistics the thread's (none attached: `None`).
    fn attached(&self) -> Option<crate::graph::workload::scope::Attached> {
        self.workload
            .get()
            .map(crate::graph::workload::scope::Attached::new)
    }

    /// Locks the graph for writing, blocking until no reader, writer or unit
    /// ([`Self::begin_unit`]) holds it. On the thread that holds a unit of this graph it
    /// returns a view of the unit's lock instead, making that lock exclusive first (waiting for
    /// the other threads' current readers) and opening the storage's unit when this is the
    /// unit's first write; the lock then stays exclusive until the unit closes.
    ///
    /// # Panics
    ///
    /// Like [`RefCell::borrow_mut`](std::cell::RefCell::borrow_mut), when the current thread
    /// holds a unit of this graph and another view of it ([`read`](Self::read) or `write`) is
    /// alive on this thread (also a commit hook or an embedder reaching back into the graph
    /// while its unit writes or commits): the `&mut S` would alias it.
    /// [`Self::write_checked`] returns [`GraphError::GraphViewHeld`] instead,
    /// [`Self::try_write`] `None`. (Outside a unit the same situation deadlocks, because the
    /// lock is not reentrant.)
    ///
    /// Also when the graph is poisoned ([`Self::is_poisoned`]: a rollback failed earlier, the
    /// storage is in an unknown state); [`Self::write_checked`] returns
    /// [`GraphError::Poisoned`] instead.
    #[track_caller]
    pub fn write(&self) -> GraphWrite<'_, S> {
        match self.write_checked() {
            Ok(guard) => guard,
            Err(err) => checked_panic(&err),
        }
    }

    /// [`Self::write`], but a view the current thread may not have
    /// ([`write`](Self::write)'s "Panics") is [`GraphError::GraphViewHeld`] instead of a panic,
    /// and a poisoned graph [`GraphError::Poisoned`]. Blocks like `write()`.
    pub fn write_checked(&self) -> Result<GraphWrite<'_, S>, GraphError> {
        if self.holds_unit() {
            // Before the held view: a poisoned graph opens no storage unit.
            self.poison.check()?;
            return Ok(self.write_view(self.held_write()?, None));
        }
        let Some((stats, key)) = lock_series(&self.workload, "write.wait") else {
            let guard = self.lock.write();
            self.poison.check()?;
            return Ok(self.write_view(WriteAccess::Locked(guard), None));
        };
        let asked = Instant::now();
        let guard = self.lock.write();
        let acquired = Instant::now();
        let wait = acquired.duration_since(asked).as_micros() as u64;
        stats.record(
            key,
            now_micros(),
            crate::graph::workload::Sample::new(0, wait),
        );
        self.poison.check()?;
        Ok(self.write_view(WriteAccess::Locked(guard), Some((acquired, stats))))
    }

    /// The write view over `guard`, watching for a failed rollback while it lives.
    fn write_view<'a>(
        &'a self,
        guard: WriteAccess<'a, S>,
        held: Option<(Instant, &'a Arc<WorkloadStats>)>,
    ) -> GraphWrite<'a, S> {
        let storage = match &guard {
            WriteAccess::Locked(locked) => WatchedStorage::of(self, locked),
            WriteAccess::Held(held, _) => WatchedStorage::of(self, held),
        };
        GraphWrite {
            guard,
            watch: RollbackWatch::start(storage),
            poison: &self.poison,
            held,
            _attached: self.attached(),
        }
    }

    /// Locks the graph for writing if nobody holds it right now, else returns `None` without
    /// blocking or panicking (a thread that already holds the lock gets `None` instead of a
    /// deadlock). On the thread that holds a unit of this graph: a view of the unit's lock when
    /// it is exclusive already or can be made exclusive without waiting (then the storage's unit
    /// opens, as at the unit's first [`write`](Self::write)), `None` while other threads read or
    /// the thread holds another view of the graph.
    ///
    /// **`None` means "busy", "not allowed on this thread" and "poisoned" alike**, and on a
    /// poisoned graph ([`Self::is_poisoned`]) it stays `None` for good: a caller that retries on
    /// `None` must tell them apart. [`Self::try_write_checked`] does, without blocking:
    /// `Ok(None)` busy, [`GraphError::GraphViewHeld`] not allowed on this thread,
    /// [`GraphError::Poisoned`] poisoned.
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let graph = Graph::new(TraversalGraph::tinkerpop_modern());
    /// let answer = match graph.try_write_checked() {
    ///     Ok(Some(_guard)) => "written",
    ///     Ok(None) => "busy: try again later",
    ///     Err(GraphError::GraphViewHeld { .. }) => "drop this thread's other view first",
    ///     Err(GraphError::Poisoned { .. }) => "poisoned: reopen the storage",
    ///     Err(_) => "refused",
    /// };
    /// assert_eq!(answer, "written");
    /// ```
    pub fn try_write(&self) -> Option<GraphWrite<'_, S>> {
        self.try_write_checked().ok().flatten()
    }

    /// [`Self::try_write`] with the two refusals told apart: `Ok(Some(guard))` when the write
    /// lock (or, on the thread that holds a unit of this graph, the unit's write view) was taken
    /// without waiting, `Ok(None)` when the graph is busy right now (another thread reads,
    /// writes or holds a unit; a thread that holds the lock itself outside a unit gets `Ok(None)`
    /// too, because the lock does not know its holders), and
    /// [`GraphError::GraphViewHeld`] when the current thread holds a unit of this graph and
    /// another view of it is alive on this thread (a [`read`](Self::read) or `write` view, also
    /// a commit hook or an embedder reaching back into the graph): waiting would not help, the
    /// view has to go first. A poisoned graph ([`Self::is_poisoned`]) is
    /// [`GraphError::Poisoned`]. Never blocks, never panics. The error type is
    /// `#[non_exhaustive]`, so new refusals can be reported later.
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let graph = Graph::new(TraversalGraph::tinkerpop_modern());
    /// // Free: the guard.
    /// assert!(graph.try_write_checked().unwrap().is_some());
    /// // Busy: another reader holds the lock.
    /// let reader = graph.read();
    /// assert!(graph.try_write_checked().unwrap().is_none());
    /// drop(reader);
    /// // Forbidden: inside the thread's own unit, beside a live read view.
    /// let mark = graph.begin_unit().unwrap();
    /// let view = graph.read();
    /// assert!(matches!(
    ///     graph.try_write_checked(),
    ///     Err(GraphError::GraphViewHeld { .. })
    /// ));
    /// drop(view);
    /// assert!(graph.try_write_checked().unwrap().is_some());
    /// graph.commit_unit(mark).unwrap();
    /// ```
    pub fn try_write_checked(&self) -> Result<Option<GraphWrite<'_, S>>, GraphError> {
        if self.holds_unit() {
            self.poison.check()?;
            return Ok(self
                .try_held_write()?
                .map(|guard| self.write_view(guard, None)));
        }
        let Some(guard) = self.lock.try_write() else {
            return Ok(None);
        };
        self.poison.check()?;
        let held =
            lock_series(&self.workload, "write.hold").map(|(stats, _)| (Instant::now(), stats));
        Ok(Some(self.write_view(WriteAccess::Locked(guard), held)))
    }

    /// Whether the graph is poisoned: a rollback of one of its units failed (a host storage's
    /// [`GraphStorage::rollback_unit`] or [`GraphStorage::discard_unit`] returned `Err` while
    /// the engine closed a unit of this graph), so its storage is in an unknown state. A
    /// poisoned graph refuses every data operation with [`GraphError::Poisoned`]
    /// ([`Self::read_checked`], [`Self::write_checked`], [`Self::try_write_checked`],
    /// [`Self::begin_unit`], the closes of a held unit, every traversal, import and front-end
    /// operation on it), and [`Self::read`] / [`Self::write`] panic. The poison is never lifted
    /// in place: drop the graph and reopen or recover the storage ([`Self::into_inner`] /
    /// [`Self::get_mut`] still hand the storage out, to close or salvage it).
    ///
    /// The engine learns of a failed rollback in the rollbacks it runs itself (a traversal, a
    /// transaction or dry run on a write view, an import, a change-set apply, the closes of a
    /// held unit, a panic unwinding through a unit). A host that calls
    /// [`GraphStorage::rollback_unit`] on the storage through a [`Self::write`] guard itself
    /// gets the error directly and decides on its own.
    ///
    /// Reads only the poison flag (one atomic load), so it cannot fail and never blocks, also
    /// while another thread holds the lock: a diagnostic, not a data operation.
    pub fn is_poisoned(&self) -> bool {
        self.poison.flag.load(Ordering::Acquire)
    }

    /// The text of the failed rollback that poisoned the graph ([`Self::is_poisoned`]), `None`
    /// while it is not poisoned. Reads only the poison, never the storage or the lock.
    pub fn poison_cause(&self) -> Option<&str> {
        match self.is_poisoned() {
            true => self.poison.cause.get().map(String::as_str),
            false => None,
        }
    }

    /// Whether the current thread holds a unit of this graph: one opened by
    /// [`Self::begin_unit`] on this thread and not closed yet.
    pub fn holds_unit(&self) -> bool {
        let owner = self.unit.thread.load(Ordering::Acquire);
        owner != NO_OWNER && thread_token() == Some(owner)
    }

    /// Whether any thread holds a unit of this graph ([`Self::begin_unit`]) right now.
    pub(crate) fn unit_held(&self) -> bool {
        self.unit.thread.load(Ordering::Acquire) != NO_OWNER
    }

    /// Opens a unit that the current thread holds across locks, until it closes it with
    /// [`Self::commit_unit`], [`Self::commit_unit_with`], [`Self::rollback_unit`] or
    /// [`Self::discard_unit`] on the same thread (the type docs, "Units across locks"). The mark
    /// stands for the unit's level; pass it to the close.
    ///
    /// The outermost call waits until no other unit and no writer holds the graph, then holds
    /// the lock upgradable: other threads keep reading until the unit's first [`Self::write`].
    /// It takes no exclusive moment, so it never waits for the readers in flight: the storage's
    /// own unit ([`GraphStorage::begin_unit_at`]) opens at the unit's first write, with the
    /// unit's time pinned here ([`GraphStorage::next_unit_started_at`]). A call on the thread
    /// that holds a unit opens a nested unit (a savepoint). Never call it while the thread holds
    /// a guard of this graph taken outside a unit (a deadlock, as with `write()`).
    ///
    /// Fails with [`GraphError::GraphViewHeld`] when the thread holds a unit of this graph and a
    /// view of it is alive (a [`read`](Self::read) or [`write`](Self::write) guard, a commit
    /// hook of its unit), with [`GraphError::Poisoned`] on a poisoned graph
    /// ([`Self::is_poisoned`]), and with [`GraphError::Unsupported`] while the thread is exiting
    /// (its thread-locals destroyed); nothing is opened then.
    pub fn begin_unit(&self) -> Result<UnitMark, GraphError> {
        if self.holds_unit() {
            self.poison.check()?;
            return self.begin_nested_unit();
        }
        let Some(token) = thread_token() else {
            return Err(GraphError::unsupported(
                "opening a unit while the thread exits",
            ));
        };
        let raw = self.raw();
        raw.lock_upgradable();
        let abandon = AbandonBegin(raw);
        // Checked under the lock (`abandon` releases it), like the views.
        self.poison.check()?;
        // SAFETY: this thread holds the lock upgradable, which keeps every writer out, so no
        // thread has a `&mut S`; it holds no unit, so it has no view of its own.
        let base = unsafe { &*self.lock.data_ptr() };
        // A storage that cannot read its clock fails the begin: `abandon` releases the lock.
        let started_at = base.next_unit_started_at()?;
        std::mem::forget(abandon);
        *self.unit.state.lock() = HeldUnit {
            depth: 1,
            started_at,
            ..HeldUnit::default()
        };
        self.unit.thread.store(token, Ordering::Release);
        if let Some(started_at) = started_at {
            pin_held_unit_clock(WatchedStorage::of(self, base), started_at);
        }
        Ok(UnitMark::new(0))
    }

    /// [`Self::begin_unit`] on the thread that holds a unit: a savepoint, opened on the storage
    /// right away when the unit has written, else recorded (opened at the first write).
    fn begin_nested_unit(&self) -> Result<UnitMark, GraphError> {
        const OPERATION: &str = "begin a nested unit";
        {
            let mut held = self.unit.state.lock();
            if held.reads > 0 || held.writes > 0 {
                return Err(view_held(OPERATION));
            }
            if !(held.exclusive && held.marks.len() == held.depth as usize) {
                held.depth = held.depth.saturating_add(1);
                return Ok(UnitMark::new(u64::from(held.depth - 1)));
            }
            // The storage call counts as a write view (see `close_held_unit`).
            held.writes += 1;
        }
        let call = HeldView {
            unit: &self.unit,
            write: true,
        };
        // SAFETY: this thread holds the lock exclusively and no view of it (checked, and the
        // counted call refuses new ones).
        let mark = unsafe { &mut *self.lock.data_ptr() }.begin_unit();
        drop(call);
        // A storage that cannot open the savepoint fails the begin; nothing is recorded.
        let mark = mark?;
        let mut held = self.unit.state.lock();
        held.marks.push(mark);
        held.depth = held.depth.saturating_add(1);
        Ok(UnitMark::new(u64::from(held.depth - 1)))
    }

    /// The time pinned for the unit the current thread holds (`None`: no unit held, or a
    /// storage without a unit clock): what [`GraphStorage::unit_started_at`] reports once the
    /// storage's unit is open. The script layer pins its traversals inside a held unit to it, so
    /// they read it also before the unit's first write.
    #[cfg(feature = "script")]
    pub(crate) fn held_unit_started_at(&self) -> Option<i128> {
        match self.holds_unit() {
            true => self.unit.state.lock().started_at,
            false => None,
        }
    }

    /// Commits the innermost unit the current thread holds ([`GraphStorage::commit_unit`]: at
    /// the outermost unit the changes become final and the commit hooks run, under the
    /// exclusive lock); the outermost close releases the lock. The unit is closed also when the
    /// commit fails (a veto rolls it back). A unit that never wrote has nothing on the storage:
    /// it closes without a storage call (no hooks) and without an exclusive moment.
    ///
    /// Fails with [`GraphError::UnitNotHeld`] when the current thread holds no unit of this
    /// graph, with [`GraphError::GraphViewHeld`] while a view of the graph is alive on this
    /// thread (a guard, or a commit hook closing a unit of the graph it commits; nothing is
    /// closed), else with the storage's commit error.
    pub fn commit_unit(&self, mark: UnitMark) -> Result<(), GraphError> {
        let _ = mark;
        self.close_held_unit(
            "commit a unit",
            false,
            |storage, mark| storage.commit_unit(mark),
            || Ok(()),
        )?
    }

    /// [`Self::commit_unit`] with [`CommitMetadata`] for the commit's
    /// [`ChangeSet`] ([`GraphStorage::set_unit_metadata`] right before the commit, as
    /// [`Transactional::transaction_with`](crate::graph::Transactional::transaction_with)
    /// does). A unit that never wrote reports nothing, so its metadata goes nowhere.
    pub fn commit_unit_with(
        &self,
        mark: UnitMark,
        metadata: CommitMetadata,
    ) -> Result<(), GraphError> {
        let _ = mark;
        self.close_held_unit(
            "commit a unit",
            false,
            |storage, mark| match storage.set_unit_metadata(metadata) {
                Ok(()) => storage.commit_unit(mark),
                Err(err) => Err(crate::graph::storage::rollback_failed_unit(
                    storage, mark, err,
                )),
            },
            || Ok(()),
        )?
    }

    /// Rolls back the innermost unit the current thread holds
    /// ([`GraphStorage::rollback_unit`]); the outermost close releases the lock. A unit that
    /// never wrote closes without a storage call and without an exclusive moment. Fails with
    /// [`GraphError::UnitNotHeld`] when the current thread holds no unit of this graph and with
    /// [`GraphError::GraphViewHeld`] while a view of the graph is alive on this thread (nothing
    /// is rolled back).
    pub fn rollback_unit(&self, mark: UnitMark, reason: &RollbackReason) -> Result<(), GraphError> {
        let _ = mark;
        self.close_held_unit(
            "roll back a unit",
            false,
            |storage, mark| {
                let rolled_back = storage.rollback_unit(mark, reason);
                if let Err(err) = &rolled_back {
                    note_failed_rollback(storage.storage_identity(), err);
                }
                rolled_back
            },
            || Ok(()),
        )?
    }

    /// Closes the innermost unit the current thread holds by undoing it without the commit
    /// hooks and returns what it changed ([`GraphStorage::discard_unit`], the end of a dry
    /// run); the outermost close releases the lock. A unit that never wrote opens the storage's
    /// unit for this call (an exclusive moment that waits for the readers in flight) so the
    /// storage describes its empty change set. Fails with [`GraphError::UnitNotHeld`] when the
    /// current thread holds no unit of this graph, with [`GraphError::GraphViewHeld`] while a
    /// view of the graph is alive on this thread, else with the storage's error (a storage
    /// without change capture: `Unsupported`, the unit closed by a rollback).
    pub fn discard_unit(&self, mark: UnitMark) -> Result<ChangeSet, GraphError> {
        let _ = mark;
        self.close_held_unit(
            "discard a unit",
            true,
            |storage, mark| {
                let discarded = storage.discard_unit(mark);
                if let Err(err) = &discarded
                    && close_failed(err)
                {
                    note_failed_rollback(storage.storage_identity(), err);
                }
                discarded
            },
            || {
                Err(GraphError::unsupported(
                    "discarding a unit the storage did not open",
                ))
            },
        )?
    }

    /// What the open unit `mark` (a level the current thread holds) changed so far
    /// ([`GraphStorage::unit_changes`]; a level that has not written: nothing). Fails with
    /// [`GraphError::UnitNotHeld`] when the current thread holds no unit of this graph and with
    /// [`GraphError::GraphViewHeld`] while a write view of it is alive on this thread.
    pub fn unit_changes(&self, mark: UnitMark) -> Result<UnitChanges, GraphError> {
        if !self.holds_unit() {
            return Err(GraphError::UnitNotHeld {
                operation: "ask what a unit changed",
            });
        }
        let guard = self.read_checked()?;
        let level = usize::try_from(mark.raw()).unwrap_or(usize::MAX);
        let stored = self.unit.state.lock().marks.get(level).copied();
        Ok(match stored {
            Some(stored) => guard.unit_changes(stored)?,
            None => UnitChanges::NONE,
        })
    }

    /// The raw lock, for the held-unit protocol.
    fn raw(&self) -> &parking_lot::RawRwLock {
        // SAFETY: only the held-unit protocol locks and unlocks the raw lock directly
        // (`begin_unit`, the closes, the held views' first write), and it never releases an
        // acquisition a guard owns: every guard stands for an acquisition of its own.
        unsafe { self.lock.raw() }
    }

    /// A read view of the unit the current thread holds; [`GraphError::GraphViewHeld`] while a
    /// write view of it is alive on this thread.
    fn held_read(&self) -> Result<ReadAccess<'_, S>, GraphError> {
        const OPERATION: &str = "read the graph";
        {
            let mut held = self.unit.state.lock();
            if held.writes > 0 {
                return Err(view_held(OPERATION));
            }
            held.reads = held.reads.saturating_add(1);
        }
        let view = HeldView {
            unit: &self.unit,
            write: false,
        };
        // SAFETY: this thread holds the unit's lock (upgradable or exclusive), so no other
        // thread has a `&mut S`; on this thread no write view is alive (checked) and none is
        // handed out while this one lives (`held_write` checks `reads`).
        Ok(ReadAccess::Held(unsafe { &*self.lock.data_ptr() }, view))
    }

    /// The write view of the unit the current thread holds; its first one makes the lock
    /// exclusive and opens the storage's units until the unit closes.
    /// [`GraphError::GraphViewHeld`] while another view of it is alive on this thread.
    fn held_write(&self) -> Result<WriteAccess<'_, S>, GraphError> {
        let exclusive = {
            let mut held = self.unit.state.lock();
            if held.reads > 0 || held.writes > 0 {
                return Err(view_held("write the graph"));
            }
            held.writes += 1;
            held.exclusive
        };
        let view = HeldView {
            unit: &self.unit,
            write: true,
        };
        // Opens the storage's units not open yet (a storage that failed to open one at an
        // earlier write gets asked again): a failure is this write's error.
        match exclusive {
            false => self.make_unit_exclusive()?,
            true => self.open_storage_units()?,
        }
        // SAFETY: this thread holds the lock exclusively, and no other view of it exists on
        // this thread (checked above, and the counter refuses new ones while this one lives).
        Ok(WriteAccess::Held(
            unsafe { &mut *self.lock.data_ptr() },
            view,
        ))
    }

    /// [`Self::held_write`] without waiting: [`GraphError::GraphViewHeld`] while another view
    /// of the unit is alive on this thread, `None` while other threads read.
    fn try_held_write(&self) -> Result<Option<WriteAccess<'_, S>>, GraphError> {
        {
            let mut held = self.unit.state.lock();
            if held.reads > 0 || held.writes > 0 {
                return Err(view_held("write the graph"));
            }
            if !held.exclusive {
                // SAFETY: this thread holds the unit's lock upgradable.
                if !unsafe { self.raw().try_upgrade() } {
                    return Ok(None);
                }
                held.exclusive = true;
                held.exclusive_since =
                    lock_series(&self.workload, "write.hold").map(|_| Instant::now());
            }
            held.writes += 1;
        }
        let view = HeldView {
            unit: &self.unit,
            write: true,
        };
        self.open_storage_units()?;
        // SAFETY: as in `held_write`.
        Ok(Some(WriteAccess::Held(
            unsafe { &mut *self.lock.data_ptr() },
            view,
        )))
    }

    /// Upgrades the lock of the unit the current thread holds for good (its first write),
    /// waiting for the other threads' readers (the wait counts as `write.wait`), then opens the
    /// storage's units (their error when one cannot open; the lock stays exclusive). The caller
    /// is the thread's only view of the graph.
    fn make_unit_exclusive(&self) -> Result<(), GraphError> {
        let series = lock_series(&self.workload, "write.wait");
        let asked = series.as_ref().map(|_| Instant::now());
        // SAFETY: this thread holds the unit's lock upgradable (not exclusive yet).
        unsafe { self.raw().upgrade() };
        {
            let mut held = self.unit.state.lock();
            held.exclusive = true;
            if let (Some((stats, key)), Some(asked)) = (series, asked) {
                let acquired = Instant::now();
                let wait = acquired.duration_since(asked).as_micros() as u64;
                stats.record(
                    key,
                    now_micros(),
                    crate::graph::workload::Sample::new(0, wait),
                );
                held.exclusive_since = Some(acquired);
            }
        }
        self.open_storage_units()
    }

    /// Opens on the storage, in order, every level of the held unit not opened yet: the
    /// outermost with the pinned time ([`GraphStorage::begin_unit_at`]), nested ones as
    /// savepoints. The lock must be exclusive and the caller the thread's only view of it. A
    /// level the storage cannot open stops here with its error: the levels opened so far stay
    /// open (they belong to levels still held), the rest is opened at the next write.
    fn open_storage_units(&self) -> Result<(), GraphError> {
        loop {
            let (level, started_at) = {
                let held = self.unit.state.lock();
                if held.marks.len() >= held.depth as usize {
                    return Ok(());
                }
                (held.marks.len(), held.started_at)
            };
            // SAFETY: this thread holds the lock exclusively, and the caller is its only view
            // (none of its references is used during this call).
            let storage = unsafe { &mut *self.lock.data_ptr() };
            let mark = match (level, started_at) {
                (0, Some(started_at)) => storage.begin_unit_at(started_at),
                _ => storage.begin_unit(),
            }?;
            self.unit.state.lock().marks.push(mark);
        }
    }

    /// Closes the innermost unit the current thread holds, then releases the lock at the
    /// outermost level ([`FinishClose`], also on unwind). A level with a storage unit is closed
    /// by `close` (with the storage's mark) under the exclusive lock. A level without one (the
    /// unit never wrote) is closed by `skip`, without a storage call and without an exclusive
    /// moment, unless `always_storage` (a dry run asks the storage for its change set): then the
    /// lock is made exclusive and the storage's units are opened first.
    fn close_held_unit<R>(
        &self,
        operation: &'static str,
        always_storage: bool,
        close: impl FnOnce(&mut S, UnitMark) -> R,
        skip: impl FnOnce() -> R,
    ) -> Result<R, GraphError> {
        if !self.holds_unit() {
            return Err(GraphError::UnitNotHeld { operation });
        }
        let (level, opened) = {
            let held = self.unit.state.lock();
            if held.reads > 0 || held.writes > 0 {
                return Err(view_held(operation));
            }
            let level = held.depth.saturating_sub(1) as usize;
            (level, held.marks.get(level).is_some())
        };
        if let Err(poisoned) = self.poison.check() {
            // The storage is in an unknown state: it is not asked to commit or undo anything,
            // but the level is closed (the outermost close releases the lock), so the owner
            // can always let go of a poisoned graph.
            drop(FinishClose { graph: self });
            return Err(poisoned);
        }
        if !opened && !always_storage {
            drop(FinishClose { graph: self });
            return Ok(skip());
        }
        let exclusive = {
            let mut held = self.unit.state.lock();
            // The storage call counts as a write view: a commit hook that reached for the graph
            // on this thread meanwhile would alias it.
            held.writes += 1;
            held.exclusive
        };
        let finish = FinishClose { graph: self };
        // Dropped before `finish` (declared after it).
        let call = HeldView {
            unit: &self.unit,
            write: true,
        };
        let opened = match exclusive {
            false => self.make_unit_exclusive(),
            true => self.open_storage_units(),
        };
        if let Err(err) = opened {
            // Only a dry run of a level that never wrote gets here (an opened level has all
            // levels below it open): the level is closed (it changed nothing) and the storage's
            // refusal reported.
            drop(call);
            drop(finish);
            return Err(err);
        }
        let Some(mark) = self.unit.state.lock().marks.get(level).copied() else {
            // Not reached: the levels were just opened.
            drop(call);
            drop(finish);
            return Ok(skip());
        };
        // A failed rollback inside `close` (an explicit rollback or discard, the rollback of a
        // commit whose metadata the storage refused) poisons the graph.
        // SAFETY: this thread holds the lock exclusively and no view of it (checked above).
        let storage = unsafe { &mut *self.lock.data_ptr() };
        let mut watch = RollbackWatch::start(WatchedStorage::of(self, storage));
        let closed = close(storage, mark);
        if let Some(cause) = watch.finish() {
            self.poison.set(cause);
        }
        drop(call);
        drop(finish);
        Ok(closed)
    }

    /// The storage itself, without locking: the exclusive borrow proves nobody else holds it.
    /// Also on a poisoned graph ([`Self::is_poisoned`]), so a host can close or salvage the
    /// storage; what it holds after a failed rollback is up to the storage.
    pub fn get_mut(&mut self) -> &mut S {
        self.lock.get_mut()
    }

    /// Unwraps the storage. Also on a poisoned graph ([`Self::is_poisoned`]): the way to close
    /// or salvage the storage before reopening it.
    pub fn into_inner(self) -> S {
        self.lock.into_inner()
    }
}

#[cfg(feature = "persist")]
impl<S: crate::graph::PersistentStorage> Graph<S> {
    /// Wraps a persistent `storage` for sharing, so the dyn boundary can write it as a packed
    /// snapshot too: [`AnyGraph::write_snapshot`](crate::graph::AnyGraph::write_snapshot) and
    /// the DSL's `g.export_snapshot(path)` call
    /// [`write_snapshot`](crate::persist::write_snapshot) for `S`. Otherwise the same as
    /// [`Graph::new`] (a [`TraversalGraph`] is writable through either).
    ///
    /// ```rust
    /// use std::sync::Arc;
    /// use graphersal::prelude::*;
    /// use graphersal::storage::AnyGraph;
    ///
    /// let graph = Arc::new(Graph::new_persistent(TraversalGraph::tinkerpop_modern()));
    /// let mut bytes = Vec::new();
    /// let info = graph.write_snapshot(&mut bytes).unwrap();
    /// assert_eq!(info.vertex_count, 6);
    /// ```
    pub fn new_persistent(storage: S) -> Self {
        fn write<S: crate::graph::PersistentStorage>(
            storage: &S,
            writer: &mut dyn std::io::Write,
        ) -> Result<crate::persist::SnapshotInfo, crate::persist::PersistError> {
            crate::persist::write_snapshot(storage, writer, &crate::persist::WriteOptions::new())
        }
        Graph {
            lock: parking_lot::RwLock::new(storage),
            snapshot_writer: Some(write::<S>),
            workload: OnceLock::new(),
            unit: UnitOwner::default(),
            poison: Poison::default(),
        }
    }
}

impl<S: GraphStorage + Default> Default for Graph<S> {
    /// An empty graph (`S::default()`).
    fn default() -> Self {
        Graph::new(S::default())
    }
}

impl<S: GraphStorage> From<S> for Graph<S> {
    fn from(storage: S) -> Self {
        Graph::new(storage)
    }
}

impl<S: GraphStorage> fmt::Debug for Graph<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Graph").finish_non_exhaustive()
    }
}

impl<S: GraphStorage> Deref for GraphRead<'_, S> {
    type Target = S;

    #[inline]
    fn deref(&self) -> &S {
        match &self.guard {
            ReadAccess::Locked(guard) => guard,
            ReadAccess::Held(storage, _) => storage,
        }
    }
}

impl<S: GraphStorage> Drop for GraphWrite<'_, S> {
    /// Poisons the graph when a rollback failed while the view lived (before the lock is
    /// released), and counts the hold time (`write.hold`) when statistics are attached.
    fn drop(&mut self) {
        if let Some(cause) = self.watch.finish() {
            self.poison.set(cause);
        }
        if let Some((acquired, stats)) = self.held.take()
            && let Some(key) = lock_key(stats, "write.hold")
        {
            let hold = acquired.elapsed().as_micros() as u64;
            stats.record(
                key,
                now_micros(),
                crate::graph::workload::Sample::new(0, hold),
            );
        }
    }
}

impl<S: GraphStorage> Deref for GraphWrite<'_, S> {
    type Target = S;

    #[inline]
    fn deref(&self) -> &S {
        match &self.guard {
            WriteAccess::Locked(guard) => guard,
            WriteAccess::Held(storage, _) => storage,
        }
    }
}

impl<S: GraphStorage> DerefMut for GraphWrite<'_, S> {
    #[inline]
    fn deref_mut(&mut self) -> &mut S {
        match &mut self.guard {
            WriteAccess::Locked(guard) => guard,
            WriteAccess::Held(storage, _) => storage,
        }
    }
}

impl<S: GraphStorage> fmt::Debug for GraphRead<'_, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GraphRead").finish_non_exhaustive()
    }
}

impl<S: GraphStorage> fmt::Debug for GraphWrite<'_, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GraphWrite").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::PropertyMap;

    #[test]
    fn new_read_write_try_write_get_mut_into_inner() {
        let mut graph = Graph::new(TraversalGraph::new());
        assert_eq!(graph.read().vertex_count(), 0);
        graph
            .write()
            .insert_vertex(Some("a".into()), vec!["n".into()], PropertyMap::default())
            .expect("a writable storage");
        {
            let first = graph.read();
            let second = graph.read();
            assert_eq!((first.vertex_count(), second.vertex_count()), (1, 1));
            assert!(graph.try_write().is_none(), "readers hold the lock");
        }
        {
            let held = graph.try_write().expect("nobody holds the lock");
            assert!(graph.try_write().is_none(), "a writer holds the lock");
            drop(held);
        }
        graph
            .get_mut()
            .insert_vertex(Some("b".into()), vec![], PropertyMap::default())
            .expect("a writable storage");
        let storage = graph.into_inner();
        assert_eq!(storage.vertex_count(), 2);
        assert!(storage.get_vertex_by_id("b").is_some());
    }

    #[test]
    fn default_is_empty_and_the_type_is_shareable() {
        fn shareable<T: Send + Sync>() {}
        shareable::<Graph>();
        let graph: Graph = Graph::default();
        assert_eq!(graph.read().vertex_count(), 0);
        assert_eq!(format!("{graph:?}"), "Graph { .. }");
    }
}
