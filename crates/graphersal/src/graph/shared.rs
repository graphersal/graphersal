//! [`Graph`]: a storage behind a read-write lock, the shape a graph is shared in.

use std::fmt;
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, OnceLock};

use web_time::Instant;

use crate::graph::workload::{SeriesKey, SeriesKind, WorkloadLevel, WorkloadStats};
use crate::graph::{GraphStorage, TraversalGraph};

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
    guard: parking_lot::RwLockReadGuard<'a, S>,
    /// Makes the graph's statistics the thread's while the guard lives (executions record into
    /// them).
    _attached: Option<crate::graph::workload::scope::Attached>,
}

/// Exclusive write access to the storage of a [`Graph`] ([`Graph::write`],
/// [`Graph::try_write`]); dereferences (mutably) to `S`. The lock is released when the guard is
/// dropped; with workload statistics attached, the time it was held is counted then.
pub struct GraphWrite<'a, S: GraphStorage = TraversalGraph> {
    guard: parking_lot::RwLockWriteGuard<'a, S>,
    /// When the lock was acquired and where the hold time goes (statistics attached only).
    held: Option<(Instant, &'a Arc<WorkloadStats>)>,
    /// Makes the graph's statistics the thread's while the guard lives.
    _attached: Option<crate::graph::workload::scope::Attached>,
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
    /// thread's [`CancelToken`](crate::CancelToken)). Unauthorized: a front end asks its
    /// authorizer for `Read` on the data first.
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
        let guard = self.read();
        let source = crate::GraphTraversalSource::new(&*guard);
        let memory =
            source.memory_report(&crate::graph::memory_report::MemoryReportRequest::new())?;
        let mut failure = None;
        let mut estimate =
            |request: &crate::graph::compression_estimate::CompressionEstimateRequest| {
                match source.estimate_compression(request) {
                    Ok(estimate) => Some(estimate),
                    Err(err) if matches!(*err, crate::TraverserError::Cancelled { .. }) => {
                        failure = Some(err);
                        None
                    }
                    // A storage without compressed properties: no compression advice.
                    Err(_) => None,
                }
            };
        let advice =
            crate::graph::advisor::compute(&memory, workload.as_ref(), &mut estimate, request, now);
        match failure {
            Some(err) => Err(err),
            None => Ok(advice),
        }
    }

    /// A new graph around `storage` with the same capabilities at the dyn boundary as `self`
    /// (the snapshot writer of [`Graph::new_persistent`]): a whole-script unit moves the
    /// storage into such a private graph.
    #[cfg_attr(not(feature = "script"), allow(dead_code))]
    pub(crate) fn new_like(&self, storage: S) -> Self {
        Graph {
            lock: parking_lot::RwLock::new(storage),
            #[cfg(feature = "persist")]
            snapshot_writer: self.snapshot_writer,
            workload: OnceLock::new(),
        }
    }

    /// The snapshot writer set by [`Graph::new_persistent`].
    #[cfg(feature = "persist")]
    pub(crate) fn snapshot_writer(&self) -> Option<SnapshotWriter<S>> {
        self.snapshot_writer
    }

    /// Locks the graph for reading, blocking while a writer holds it. Several readers may hold
    /// it at once.
    pub fn read(&self) -> GraphRead<'_, S> {
        let attached = self.attached();
        let Some((stats, key)) = lock_series(&self.workload, "read.wait") else {
            return GraphRead {
                guard: self.lock.read(),
                _attached: attached,
            };
        };
        // Uncontended: no clock read, nothing counted.
        if let Some(guard) = self.lock.try_read() {
            return GraphRead {
                guard,
                _attached: attached,
            };
        }
        let asked = Instant::now();
        let guard = self.lock.read();
        let wait = asked.elapsed().as_micros() as u64;
        stats.record(
            key,
            now_micros(),
            crate::graph::workload::Sample::new(0, wait),
        );
        GraphRead {
            guard,
            _attached: attached,
        }
    }

    /// The guard that makes this graph's statistics the thread's (none attached: `None`).
    fn attached(&self) -> Option<crate::graph::workload::scope::Attached> {
        self.workload
            .get()
            .map(crate::graph::workload::scope::Attached::new)
    }

    /// Locks the graph for writing, blocking until no reader or writer holds it.
    pub fn write(&self) -> GraphWrite<'_, S> {
        let Some((stats, key)) = lock_series(&self.workload, "write.wait") else {
            return GraphWrite {
                guard: self.lock.write(),
                held: None,
                _attached: self.attached(),
            };
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
        GraphWrite {
            guard,
            held: Some((acquired, stats)),
            _attached: self.attached(),
        }
    }

    /// Locks the graph for writing if nobody holds it right now, else returns `None` without
    /// blocking (a thread that already holds the lock gets `None` instead of a deadlock).
    pub fn try_write(&self) -> Option<GraphWrite<'_, S>> {
        let guard = self.lock.try_write()?;
        let held =
            lock_series(&self.workload, "write.hold").map(|(stats, _)| (Instant::now(), stats));
        Some(GraphWrite {
            guard,
            held,
            _attached: self.attached(),
        })
    }

    /// The storage itself, without locking: the exclusive borrow proves nobody else holds it.
    pub fn get_mut(&mut self) -> &mut S {
        self.lock.get_mut()
    }

    /// Unwraps the storage.
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
        &self.guard
    }
}

impl<S: GraphStorage> Drop for GraphWrite<'_, S> {
    /// Counts the hold time (`write.hold`) when statistics are attached.
    fn drop(&mut self) {
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
        &self.guard
    }
}

impl<S: GraphStorage> DerefMut for GraphWrite<'_, S> {
    #[inline]
    fn deref_mut(&mut self) -> &mut S {
        &mut self.guard
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
