//! The Store's view of its live graph (`book/src/for_developers/custom_storage.md`): the
//! crate-internal, object-safe [`StoreGraph`], so the store's code (open, recovery, journal,
//! checkpoints, rollback, backups, close) is compiled once and works on any
//! [`Graph<S>`](Graph) whose storage is a [`PersistentStorage`].
//!
//! - [`StoreGraph::read`] / [`StoreGraph::write`] take the graph's lock and hand out a boxed
//!   guard ([`LiveRead`] / [`LiveWrite`]) with what the Store does under that lock.
//! - The journal lives in a [`JournalCell`] the Store keeps; the graph's journal slot holds a
//!   hook forwarding to the cell. A reload in place ([`LiveWrite::reload`]: clear + load into
//!   the SAME instance, so the hooks the application registered stay) replaces the journal in
//!   the cell; the slot itself is attached once, at the open.

use std::any::Any;
use std::io::Read;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use super::backup::BackupMode;
use super::files::{DirPath, GraphFile};
use super::maintenance::BackupReport;
use super::snapshot_dir::{DirSnapshotReader, write_graph_snapshot};
use super::wal_files::{WalWriter, open_segment_reader};
use crate::error::GraphError;
use crate::graph::hooks::{CommitHook, HookError, JournalHook, JournalSlot, Mark};
use crate::graph::{
    ChangeSet, DefinitionKind, Graph, GraphStorage, PersistentStorage, RollbackReason,
    StorageCapabilities, TraversalGraph,
};
use crate::persist::error::{PersistError, StoreFailure};
use crate::persist::journal::Journal;
use crate::persist::recover::{Recovered, Recovery, RecoveryReport, RecoveryTarget, recover_into};
use crate::persist::snapshot::{ReadOptions, WriteOptions, load_snapshot, write_snapshot};

/// The live graph of a Store, behind its lock. Implemented for every [`Graph<S>`](Graph) with
/// `S: PersistentStorage + Default`.
pub(crate) trait StoreGraph: Send + Sync + 'static {
    /// The graph as `Any`, to get the typed `Arc<Graph<S>>` back.
    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync>;
    /// Takes the read lock.
    fn read(&self) -> Box<dyn LiveRead + '_>;
    /// Takes the write lock (commits wait).
    fn write(&self) -> Box<dyn LiveWrite + '_>;
}

/// What the Store reads from its graph under the read lock.
pub(crate) trait LiveRead {
    /// [`GraphStorage::last_commit_seq`].
    fn last_commit_seq(&self) -> Result<u64, GraphError>;
    /// [`PersistentStorage::last_committed_at`].
    fn last_committed_at(&self) -> i64;
    /// [`PersistentStorage::graph_id`].
    fn graph_id(&self) -> u128;
    /// The kind of every definition of the catalog.
    fn definition_kinds(&self) -> Result<Vec<DefinitionKind>, GraphError>;
    /// The model identities the schema's `embedder` keywords name (format spec 4.5).
    fn schema_models(&self) -> Result<Vec<String>, GraphError>;
    /// Why the state must not be written by this version: a critical definition of a kind it
    /// does not know (format spec 16 rule 3).
    fn critical_definition(&self) -> Result<Option<String>, GraphError>;
    /// Writes the graph as a snapshot directory into `tmp` (the streaming encoder).
    fn write_snapshot_live(&self, tmp: &DirPath, write: &WriteOptions) -> Result<(), PersistError>;
    /// A backup of the graph in memory (format spec 12.11, [`super::from_memory`]).
    fn backup_from_memory(
        &self,
        store: &DirPath,
        graph_file: &GraphFile,
        write: &WriteOptions,
        target: &DirPath,
        mode: BackupMode,
    ) -> Result<BackupReport, PersistError>;
}

/// What the Store changes in its graph under the write lock.
pub(crate) trait LiveWrite: LiveRead {
    /// [`GraphStorage::mark`].
    fn mark(&mut self, name: &str) -> Result<Mark, GraphError>;
    /// Empties the graph ([`PersistentStorage::clear`]: the hooks and the journal slot stay)
    /// and loads `plan` into the same instance. On error the graph is half-loaded: the caller
    /// empties it again with [`LiveWrite::clear`].
    fn reload(&mut self, plan: &LoadPlan) -> Result<RecoveryReport, PersistError>;
    /// Empties the graph ([`PersistentStorage::clear`]: data, schema, catalog and position go,
    /// the hooks and the journal slot stay). After a failed reload: an empty graph, never a
    /// partial one.
    fn clear(&mut self) -> Result<(), GraphError>;
    /// Puts `journal` into the graph's journal slot, after checking the precondition of a
    /// journaled storage (design 4.2): it claims `atomic` and `change_capture`.
    fn attach_journal(&mut self, journal: JournalSlot) -> Result<(), PersistError>;
    /// Registers one of the Store's vetoing hooks (read-only, damage, maintenance, backup).
    fn register_guard_hook(&mut self, hook: Box<dyn CommitHook>) -> Result<(), PersistError>;
}

impl<S: PersistentStorage + Default> StoreGraph for Graph<S> {
    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }

    fn read(&self) -> Box<dyn LiveRead + '_> {
        Box::new(Live(Graph::read(self)))
    }

    fn write(&self) -> Box<dyn LiveWrite + '_> {
        Box::new(Live(Graph::write(self)))
    }
}

/// A lock guard of a `Graph<S>` seen as [`LiveRead`] / [`LiveWrite`].
struct Live<G>(G);

impl<G> LiveRead for Live<G>
where
    G: Deref,
    G::Target: PersistentStorage,
{
    fn last_commit_seq(&self) -> Result<u64, GraphError> {
        self.0.last_commit_seq()
    }

    fn last_committed_at(&self) -> i64 {
        self.0.last_committed_at()
    }

    fn graph_id(&self) -> u128 {
        self.0.graph_id()
    }

    fn definition_kinds(&self) -> Result<Vec<DefinitionKind>, GraphError> {
        Ok(self.0.definitions()?.map(|d| d.kind()).collect())
    }

    fn schema_models(&self) -> Result<Vec<String>, GraphError> {
        Ok(self
            .0
            .get_schema()?
            .map(|schema| {
                crate::value::vector::embed::provenance::schema_models(&schema)
                    .into_iter()
                    .map(|(model, _)| model)
                    .collect()
            })
            .unwrap_or_default())
    }

    fn critical_definition(&self) -> Result<Option<String>, GraphError> {
        Ok(crate::persist::definitions::critical_unknown(
            self.0.definitions()?,
        ))
    }

    fn write_snapshot_live(&self, tmp: &DirPath, write: &WriteOptions) -> Result<(), PersistError> {
        write_graph_snapshot(tmp, &*self.0, write).map(|_| ())
    }

    fn backup_from_memory(
        &self,
        store: &DirPath,
        graph_file: &GraphFile,
        write: &WriteOptions,
        target: &DirPath,
        mode: BackupMode,
    ) -> Result<BackupReport, PersistError> {
        super::from_memory::backup_from_memory(store, &*self.0, graph_file, write, target, mode)
    }
}

impl<G> LiveWrite for Live<G>
where
    G: DerefMut,
    G::Target: PersistentStorage,
{
    fn mark(&mut self, name: &str) -> Result<Mark, GraphError> {
        self.0.mark(name)
    }

    fn reload(&mut self, plan: &LoadPlan) -> Result<RecoveryReport, PersistError> {
        let graph = &mut *self.0;
        graph.clear()?;
        plan.run_into(graph)
    }

    fn clear(&mut self) -> Result<(), GraphError> {
        self.0.clear()
    }

    fn attach_journal(&mut self, journal: JournalSlot) -> Result<(), PersistError> {
        require_journal_capabilities(self.0.capabilities()?)?;
        Ok(self.0.attach_journal(journal)?)
    }

    fn register_guard_hook(&mut self, hook: Box<dyn CommitHook>) -> Result<(), PersistError> {
        Ok(self.0.register_commit_hook(hook)?)
    }
}

/// The precondition of a journaled storage (design 4.2): a journal writes its record in
/// `before_commit`, so the storage must roll a vetoed unit back (`atomic`) and build the
/// change set (`change_capture`). The error names the missing capability.
pub(crate) fn require_journal_capabilities(
    capabilities: StorageCapabilities,
) -> Result<(), PersistError> {
    let missing = match (capabilities.atomic, capabilities.change_capture) {
        (true, true) => return Ok(()),
        (false, false) => "atomic and change_capture",
        (false, true) => "atomic",
        (true, false) => "change_capture",
    };
    Err(PersistError::store(
        "attach the store's journal",
        StoreFailure::MissingCapability,
        format!(
            "the graph's storage does not claim the capability {missing} \
             (StorageCapabilities): a journal cannot honour a veto without it"
        ),
    ))
}

/// Builds the Store's live graph: an empty one to load the store into, or one holding the
/// state a salvage produced (maintenance mode).
pub(crate) trait GraphMaker {
    /// Checks the precondition of a journaled storage ([`require_journal_capabilities`])
    /// before the store's files are touched. Default: met (a [`TraversalGraph`]).
    fn check(&self) -> Result<(), PersistError> {
        Ok(())
    }
    /// A new, empty graph.
    fn empty(&self) -> Arc<dyn StoreGraph>;
    /// A graph holding `graph` (the salvaged state of a damaged store).
    fn salvaged(&self, graph: TraversalGraph) -> Result<Arc<dyn StoreGraph>, PersistError>;
}

/// The [`GraphMaker`] of a `Store` over [`TraversalGraph`].
pub(crate) struct TraversalGraphs;

impl GraphMaker for TraversalGraphs {
    fn empty(&self) -> Arc<dyn StoreGraph> {
        Arc::new(Graph::new_persistent(TraversalGraph::new()))
    }

    fn salvaged(&self, graph: TraversalGraph) -> Result<Arc<dyn StoreGraph>, PersistError> {
        Ok(Arc::new(Graph::new_persistent(graph)))
    }
}

/// The [`GraphMaker`] of a `Store<S>` over a host's storage: `F` builds the empty instance.
pub(crate) struct Storages<F>(pub(crate) F);

impl<S, F> GraphMaker for Storages<F>
where
    S: PersistentStorage + Default,
    F: Fn() -> S,
{
    /// Probes an empty instance: the capabilities are a property of the storage type.
    fn check(&self) -> Result<(), PersistError> {
        require_journal_capabilities((self.0)().capabilities()?)
    }

    fn empty(&self) -> Arc<dyn StoreGraph> {
        Arc::new(Graph::new_persistent((self.0)()))
    }

    /// Copies the salvaged state into a new `S`: written as a packed snapshot (no
    /// compression) and loaded through the storage's load sink, so the data, schema,
    /// catalog, lineage id, position and id sequences all arrive (maintenance mode is rare and
    /// read-only; the copy costs one more graph in memory while it runs).
    fn salvaged(&self, graph: TraversalGraph) -> Result<Arc<dyn StoreGraph>, PersistError> {
        let mut bytes = Vec::new();
        write_snapshot(
            &graph,
            &mut bytes,
            &WriteOptions::new().with_codec(crate::persist::Codec::None),
        )?;
        drop(graph);
        let mut storage = (self.0)();
        load_snapshot(
            bytes.as_slice(),
            &ReadOptions::new().with_max_value_depth(4096),
            &mut storage,
        )?;
        Ok(Arc::new(Graph::new_persistent(storage)))
    }
}

/// What to load from a store's files: a snapshot directory, the WAL segments after it (each
/// with an optional byte limit: a recorded cut), the target and the lineage chain.
pub(crate) struct LoadPlan {
    pub(crate) snapshot: DirPath,
    pub(crate) journals: Vec<(DirPath, Option<u64>)>,
    pub(crate) target: RecoveryTarget,
    pub(crate) read: ReadOptions,
    pub(crate) chain: Vec<(u128, u64)>,
}

impl LoadPlan {
    fn journal_readers(&self) -> Result<Vec<Box<dyn Read + Send>>, PersistError> {
        self.journals
            .iter()
            .map(|(path, limit)| open_segment_reader(path, *limit))
            .collect()
    }

    /// Loads the plan into a new [`TraversalGraph`] (a past state, a fork, a closed store's
    /// checkpoint).
    pub(crate) fn run(&self) -> Result<Recovered, PersistError> {
        self.run_in(TraversalGraph::new())
    }

    /// Loads the plan into `empty`, an empty storage of any type (a past state of a
    /// `Store<S>`).
    pub(crate) fn run_in<S: PersistentStorage>(
        &self,
        empty: S,
    ) -> Result<Recovered<S>, PersistError> {
        let mut recovery = Recovery::new_in(empty)
            .snapshot(DirSnapshotReader::open(&self.snapshot)?)
            .target(self.target.clone())
            .options(self.read.clone())
            .chain(self.chain.clone());
        for reader in self.journal_readers()? {
            recovery = recovery.journal(reader);
        }
        recovery.run()
    }

    /// Loads the plan into `graph`, which must be empty.
    pub(crate) fn run_into<S: PersistentStorage + ?Sized>(
        &self,
        graph: &mut S,
    ) -> Result<RecoveryReport, PersistError> {
        let journals = self
            .journal_readers()?
            .into_iter()
            .map(|reader| reader as Box<dyn Read>)
            .collect();
        recover_into(
            graph,
            DirSnapshotReader::open(&self.snapshot)?,
            journals,
            self.target.clone(),
            &self.read,
            Some(&self.chain),
        )
    }
}

/// The Store's journal, shared between the Store and the hook in the graph's journal slot:
/// the Store syncs it and records checkpoints through it, and a reload in place replaces it
/// (the old one is dropped, so no record reaches its segment any more). Locked only while the
/// graph's write lock is held (a commit, or the Store's own calls), so the lock order is
/// always graph, then cell.
#[derive(Clone, Default)]
pub(crate) struct JournalCell(Arc<parking_lot::Mutex<Option<Journal<WalWriter>>>>);

impl JournalCell {
    /// The hook for the graph's journal slot.
    pub(crate) fn slot(&self) -> JournalSlot {
        JournalSlot::new(Box::new(CellHook(self.clone())))
    }

    /// Puts `journal` in (`None`: none; commits and marks are refused).
    pub(crate) fn set(&self, journal: Option<Journal<WalWriter>>) {
        let old = std::mem::replace(&mut *self.0.lock(), journal);
        drop(old);
    }

    /// Syncs the journal; `None` without one.
    pub(crate) fn sync_now(&self) -> Option<Result<(), HookError>> {
        self.0.lock().as_mut().map(|journal| journal.sync_now())
    }

    /// Records a Checkpoint record; `None` without a journal.
    pub(crate) fn record_checkpoint(
        &self,
        commit_seq: u64,
        time: i64,
    ) -> Option<Result<(), HookError>> {
        self.0
            .lock()
            .as_mut()
            .map(|journal| journal.record_checkpoint(commit_seq, time))
    }
}

/// The hook in the graph's journal slot: forwards to the journal in the cell.
struct CellHook(JournalCell);

impl CellHook {
    fn detached() -> HookError {
        Box::new(PersistError::store(
            "journal a commit",
            StoreFailure::Closed,
            "the store's journal is detached (a rollback or restore failed, or the store went \
             read-only): reopen the store",
        ))
    }
}

impl CommitHook for CellHook {
    fn name(&self) -> &str {
        "journal"
    }

    fn before_commit(&mut self, changes: &ChangeSet) -> Result<(), HookError> {
        match self.0.0.lock().as_mut() {
            Some(journal) => journal.before_commit(changes),
            None => Err(Self::detached()),
        }
    }

    fn after_commit(&mut self, changes: &ChangeSet) {
        if let Some(journal) = self.0.0.lock().as_mut() {
            journal.after_commit(changes);
        }
    }

    fn after_rollback(&mut self, reason: &RollbackReason) {
        if let Some(journal) = self.0.0.lock().as_mut() {
            journal.after_rollback(reason);
        }
    }
}

impl JournalHook for CellHook {
    fn record_mark(&mut self, mark: &Mark) -> Result<(), HookError> {
        match self.0.0.lock().as_mut() {
            Some(journal) => journal.record_mark(mark),
            None => Err(Self::detached()),
        }
    }

    fn record_checkpoint(&mut self, commit_seq: u64, time: i64) -> Result<(), HookError> {
        self.0.record_checkpoint(commit_seq, time).unwrap_or(Ok(()))
    }

    fn sync_now(&mut self) -> Result<(), HookError> {
        self.0.sync_now().unwrap_or(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use compact_str::CompactString;

    use super::*;
    use crate::graph::definition::Definition;
    use crate::graph::schema::GraphSchema;
    use crate::graph::{EdgeHandle, VertexHandle};
    use crate::value::{ElementProperty, PropertyMap};

    /// A `TraversalGraph` that claims only the capabilities in `claims` (everything it does not
    /// forward keeps the trait's default).
    #[derive(Default)]
    struct ClaimsLess {
        inner: TraversalGraph,
        claims: StorageCapabilities,
    }

    impl GraphStorage for ClaimsLess {
        type VertexId = VertexHandle;
        type EdgeId = EdgeHandle;

        fn capabilities(&self) -> Result<StorageCapabilities, GraphError> {
            Ok(self.claims)
        }
        fn vertex_count(&self) -> usize {
            self.inner.vertex_count()
        }
        fn edge_count(&self) -> usize {
            self.inner.edge_count()
        }
        fn vertices(&self) -> impl Iterator<Item = VertexHandle> + '_ {
            self.inner.vertices()
        }
        fn edges(&self) -> impl Iterator<Item = EdgeHandle> + '_ {
            self.inner.edges()
        }
        fn get_vertex_by_id(&self, id: &str) -> Option<VertexHandle> {
            GraphStorage::get_vertex_by_id(&self.inner, id)
        }
        fn get_edge_by_id(&self, id: &str) -> Option<EdgeHandle> {
            GraphStorage::get_edge_by_id(&self.inner, id)
        }
        fn out_edges(&self, node: VertexHandle) -> impl Iterator<Item = EdgeHandle> + '_ {
            self.inner.out_edges(node)
        }
        fn in_edges(&self, node: VertexHandle) -> impl Iterator<Item = EdgeHandle> + '_ {
            self.inner.in_edges(node)
        }
        fn vertex_id(&self, node: VertexHandle) -> Option<&str> {
            self.inner.vertex_id(node)
        }
        fn edge_id(&self, edge: EdgeHandle) -> Option<&str> {
            self.inner.edge_id(edge)
        }
        fn vertex_label(&self, node: VertexHandle) -> Option<&str> {
            self.inner.vertex_label(node)
        }
        fn edge_label(&self, edge: EdgeHandle) -> Option<&str> {
            self.inner.edge_label(edge)
        }
        fn vertex_labels(&self, node: VertexHandle) -> &[CompactString] {
            self.inner.vertex_labels(node)
        }
        fn vertex_properties(&self, node: VertexHandle) -> Option<&PropertyMap> {
            self.inner.vertex_properties(node)
        }
        fn edge_properties(&self, edge: EdgeHandle) -> Option<&PropertyMap> {
            self.inner.edge_properties(edge)
        }
        fn edge_endpoints(&self, edge: EdgeHandle) -> Option<(VertexHandle, VertexHandle)> {
            self.inner.edge_endpoints(edge)
        }
        fn insert_vertex(
            &mut self,
            id: Option<CompactString>,
            labels: Vec<CompactString>,
            properties: PropertyMap,
        ) -> Result<VertexHandle, GraphError> {
            self.inner.insert_vertex(id, labels, properties)
        }
        fn add_vertex_label(
            &mut self,
            node: VertexHandle,
            label: CompactString,
        ) -> Result<bool, GraphError> {
            self.inner.add_vertex_label(node, label)
        }
        fn remove_vertex_label(
            &mut self,
            node: VertexHandle,
            label: &str,
        ) -> Result<bool, GraphError> {
            self.inner.remove_vertex_label(node, label)
        }
        fn insert_edge(
            &mut self,
            id: Option<CompactString>,
            label: Option<CompactString>,
            from: VertexHandle,
            to: VertexHandle,
            properties: PropertyMap,
        ) -> Result<EdgeHandle, GraphError> {
            self.inner.insert_edge(id, label, from, to, properties)
        }
        fn set_vertex_property(
            &mut self,
            node: VertexHandle,
            key: CompactString,
            value: ElementProperty,
        ) -> Result<(), GraphError> {
            self.inner.set_vertex_property(node, key, value)
        }
        fn set_edge_property(
            &mut self,
            edge: EdgeHandle,
            key: CompactString,
            value: ElementProperty,
        ) -> Result<(), GraphError> {
            self.inner.set_edge_property(edge, key, value)
        }
        fn drop_vertex(&mut self, node: VertexHandle) -> Result<(), GraphError> {
            self.inner.drop_vertex(node)
        }
        fn drop_vertex_property(
            &mut self,
            prop: &str,
            node: VertexHandle,
        ) -> Result<(), GraphError> {
            self.inner.drop_vertex_property(prop, node)
        }
        fn drop_edge(&mut self, edge: EdgeHandle) -> Result<(), GraphError> {
            self.inner.drop_edge(edge)
        }
        fn drop_edge_property(&mut self, prop: &str, edge: EdgeHandle) -> Result<(), GraphError> {
            self.inner.drop_edge_property(prop, edge)
        }
    }

    impl PersistentStorage for ClaimsLess {
        fn graph_id(&self) -> u128 {
            self.inner.graph_id()
        }
        fn set_graph_id(&mut self, graph_id: u128) -> Result<(), GraphError> {
            self.inner.set_graph_id(graph_id)
        }
        fn last_committed_at(&self) -> i64 {
            PersistentStorage::last_committed_at(&self.inner)
        }
        fn id_sequences(&self) -> (u64, u64) {
            self.inner.id_sequences()
        }
        fn raise_position(&mut self, commit_seq: u64, committed_at: i64) -> Result<(), GraphError> {
            self.inner.raise_position(commit_seq, committed_at)
        }
        fn load_vertex(
            &mut self,
            id: CompactString,
            labels: Vec<CompactString>,
            properties: PropertyMap,
        ) -> Result<VertexHandle, GraphError> {
            self.inner.load_vertex(id, labels, properties)
        }
        fn load_edge(
            &mut self,
            id: CompactString,
            label: Option<CompactString>,
            from_id: &str,
            to_id: &str,
            properties: PropertyMap,
        ) -> Result<EdgeHandle, GraphError> {
            self.inner.load_edge(id, label, from_id, to_id, properties)
        }
        fn load_schema(&mut self, schema: GraphSchema) -> Result<(), GraphError> {
            self.inner.load_schema(schema)
        }
        fn load_definition(&mut self, definition: Definition) -> Result<(), GraphError> {
            self.inner.load_definition(definition)
        }
        fn clear(&mut self) -> Result<(), GraphError> {
            self.inner.clear()
        }
        fn attach_journal(&mut self, journal: JournalSlot) -> Result<(), GraphError> {
            self.inner.attach_journal(journal)
        }
    }

    fn claims(atomic: bool, change_capture: bool) -> StorageCapabilities {
        let mut claims = TraversalGraph::new().capabilities().unwrap();
        claims.atomic = atomic;
        claims.change_capture = change_capture;
        claims
    }

    /// Attaching the Store's journal to a storage that does not claim `atomic` and
    /// `change_capture` fails with `MissingCapability` naming what is missing (design 4.2), and
    /// leaves the journal slot empty; one that claims both (as `TraversalGraph` does) attaches.
    #[test]
    fn a_journal_needs_an_atomic_storage_with_change_capture() {
        for (atomic, change_capture, missing) in [
            (false, false, Some("atomic and change_capture")),
            (false, true, Some("atomic")),
            (true, false, Some("change_capture")),
            (true, true, None),
        ] {
            let graph: Arc<dyn StoreGraph> = Arc::new(Graph::new_persistent(ClaimsLess {
                inner: TraversalGraph::new(),
                claims: claims(atomic, change_capture),
            }));
            let result = graph.write().attach_journal(JournalCell::default().slot());
            let has_journal = {
                let typed = graph
                    .clone()
                    .into_any()
                    .downcast::<Graph<ClaimsLess>>()
                    .unwrap();
                typed.read().inner.undo.hooks.has_journal()
            };
            match missing {
                None => {
                    result.unwrap();
                    assert!(has_journal);
                }
                Some(missing) => {
                    let err = result.unwrap_err();
                    assert!(
                        matches!(
                            err,
                            PersistError::Store {
                                cause: StoreFailure::MissingCapability,
                                ..
                            }
                        ),
                        "{err:?}"
                    );
                    let text = err.to_string();
                    assert!(
                        text.contains(&format!("the capability {missing} ")),
                        "{text}"
                    );
                    assert!(!has_journal);
                }
            }
        }
        let typed: Arc<dyn StoreGraph> = Arc::new(Graph::new_persistent(TraversalGraph::new()));
        typed
            .write()
            .attach_journal(JournalCell::default().slot())
            .unwrap();
    }

    /// The public entry points check the precondition before any file is touched:
    /// `Store::create_in` writes nothing, `Store::open_in` and `Store::open_backup_in` leave
    /// the store as it was (it opens normally afterwards). (A storage claiming both opens:
    /// `tests/all/foreign_store_tests.rs`.)
    #[test]
    fn store_create_in_and_open_in_refuse_a_storage_without_the_capabilities() {
        use crate::persist::{MemDir, Store, StoreDir, StoreOptions};
        fn missing_capability(err: PersistError) {
            assert!(
                matches!(
                    err,
                    PersistError::Store {
                        cause: StoreFailure::MissingCapability,
                        ..
                    }
                ),
                "{err:?}"
            );
        }
        let dir = MemDir::named("memory:claims");
        let err = Store::create_in(
            &dir,
            ClaimsLess {
                inner: TraversalGraph::tinkerpop_modern(),
                claims: claims(true, false),
            },
            StoreOptions::new(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("change_capture"), "{err}");
        missing_capability(err);
        assert!(dir.list("").unwrap_or_default().is_empty());

        let store = Store::create_with(
            &dir,
            TraversalGraph::tinkerpop_modern(),
            StoreOptions::new(),
        )
        .unwrap();
        store.close().unwrap();
        let factory = || ClaimsLess {
            inner: TraversalGraph::new(),
            claims: claims(false, true),
        };
        missing_capability(Store::open_in(&dir, StoreOptions::new(), factory).unwrap_err());
        let backup = MemDir::named("memory:claims-backup");
        Store::open(&dir, StoreOptions::new())
            .unwrap()
            .backup(&backup)
            .unwrap();
        missing_capability(
            Store::open_backup_in(&backup, StoreOptions::new(), factory).unwrap_err(),
        );
    }

    /// Without a journal in the cell (a reload that failed or went read-only) the slot's hook
    /// refuses commits and marks instead of letting them through unjournaled.
    #[test]
    fn an_empty_journal_cell_refuses_commits_and_marks() {
        let graph = Graph::new_persistent(TraversalGraph::new());
        let cell = JournalCell::default();
        graph.write().attach_journal(cell.slot()).unwrap();
        let err = graph
            .write()
            .traversal_mut()
            .add_v("person")
            .to_list()
            .unwrap_err();
        assert!(err.to_string().contains("journal is detached"), "{err}");
        assert_eq!(graph.read().vertex_count(), 0);
        assert!(graph.write().mark("m").is_err());
        assert!(cell.sync_now().is_none());
        assert!(cell.record_checkpoint(1, 1).is_none());
    }
}
