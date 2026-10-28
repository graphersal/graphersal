//! [`PersistentStorage`]: what a snapshot and a Store need from a storage (feature `persist`).

use compact_str::CompactString;

use crate::error::GraphError;
use crate::graph::change::{ChangeSet, Mutation};
use crate::graph::definition::Definition;
use crate::graph::hooks::JournalSlot;
use crate::graph::replay;
use crate::graph::schema::GraphSchema;
use crate::graph::storage::GraphStorage;
use crate::graph::{ExecutionPolicy, TraversalGraph};
use crate::value::PropertyMap;

/// A storage that can be persisted: written as a snapshot, loaded from one, journaled and
/// replayed. The snapshot functions and the Store (`graphersal::persist`) need it; a storage
/// without it can still be queried through the DSL and the session, but not stored.
///
/// **EXPERIMENTAL in 0.2.x**, like [`GraphStorage`]: methods may change in any 0.2.x release.
/// Guides: the book pages `persistence/store_own_storage.md` (a `Store<S>`) and
/// `for_developers/custom_storage.md` (every front end).
///
/// # A guide for storage implementers
///
/// - **Precondition.** A persistent storage is atomic and captures changes
///   ([`StorageCapabilities::atomic`](crate::graph::StorageCapabilities) and `change_capture`):
///   the journal writes its record in `before_commit` and a later failure must be able to roll
///   the unit back. It keeps its hooks in a [`CommitHooks`](crate::graph::CommitHooks), whose
///   journal slot runs after every user hook.
/// - **Position.** [`Self::graph_id`] is the lineage id snapshots and journals carry (a UUID v7
///   created on first use); [`GraphStorage::last_commit_seq`], [`Self::last_committed_at`] and
///   [`Self::id_sequences`] are the position a snapshot records, and [`Self::raise_position`] /
///   [`GraphStorage::raise_id_sequences`] restore it (raise only, never lower).
/// - **Loading.** A snapshot is loaded into an empty storage nobody sees yet through the load
///   sink: [`Self::load_schema`], [`Self::load_definition`], [`Self::load_vertex`] (every vertex
///   before the edges), [`Self::load_edge`], then [`Self::finish_load`] once. These run outside
///   units: nothing is recorded, no hook fires, the schema is not checked against the data
///   (the snapshot was valid when written).
/// - **Reloading in place.** [`Self::clear`] empties data, schema, catalog and position but
///   KEEPS the registered hooks and the journal slot, so a Store can load another state into
///   the same instance.
/// - **Replay.** [`Self::replay`] applies a journal's change sets outside units; the default
///   works through the trait's write methods.
pub trait PersistentStorage: GraphStorage + Send + Sync + 'static {
    /// The lineage id (a UUID v7 as a `u128`, RFC 9562 bit order), created on first use.
    /// Snapshots and journals carry it; a recovery never replays a journal of another lineage.
    fn graph_id(&self) -> u128;

    /// Sets the lineage id (a snapshot load, a fork or a repair that starts a new lineage).
    /// `Err`: the storage could not store it (a backend error); the load, fork or repair
    /// fails with it.
    fn set_graph_id(&mut self, graph_id: u128) -> Result<(), GraphError>;

    /// The commit time of the last committed unit (microseconds since the Unix epoch, UTC; `0`
    /// before the first commit); never goes back.
    fn last_committed_at(&self) -> i64;

    /// The vertex and edge auto-id sequences: the last auto id handed out of each kind.
    fn id_sequences(&self) -> (u64, u64);

    /// Raises the commit sequence number ([`GraphStorage::last_commit_seq`]) and the commit
    /// time ([`Self::last_committed_at`]) to at least these values; never lowers either.
    /// `Err`: the storage could not store the position (a backend error); the load or
    /// recovery fails with it.
    fn raise_position(&mut self, commit_seq: u64, committed_at: i64) -> Result<(), GraphError>;

    /// Load sink: stores a vertex as it is (no validation, nothing recorded, no hook, no
    /// sequence change). [`GraphError::VertexIdUniqueConstraint`] when `id` exists already.
    fn load_vertex(
        &mut self,
        id: CompactString,
        labels: Vec<CompactString>,
        properties: PropertyMap,
    ) -> Result<Self::VertexId, GraphError>;

    /// Load sink: stores an edge from `from_id` to `to_id` (both loaded already) as it is.
    /// [`GraphError::VertexNotFound`] for a missing endpoint,
    /// [`GraphError::EdgeIdUniqueConstraint`] when `id` exists already.
    fn load_edge(
        &mut self,
        id: CompactString,
        label: Option<CompactString>,
        from_id: &str,
        to_id: &str,
        properties: PropertyMap,
    ) -> Result<Self::EdgeId, GraphError>;

    /// Load sink: stores the schema without checking the data against it.
    fn load_schema(&mut self, schema: GraphSchema) -> Result<(), GraphError>;

    /// Load sink: stores a catalog definition without validating it (it was valid when
    /// committed; a definition of an unknown kind stays opaque).
    fn load_definition(&mut self, definition: Definition) -> Result<(), GraphError>;

    /// Load sink: the load is about to store about `vertices` vertices and `edges` edges (the
    /// counts of a snapshot's manifest, checked against its memory estimate but not trusted):
    /// a storage may allocate for them at once. Default: nothing. `Err`: the storage cannot
    /// take that many (a backend error; not for a mere allocation hint it ignores); the load
    /// fails with it.
    fn reserve_load(&mut self, vertices: u64, edges: u64) -> Result<(), GraphError> {
        let _ = (vertices, edges);
        Ok(())
    }

    /// Ends a load: derived state is built here ([`TraversalGraph`] runs its compression pass
    /// and shrinks its indexes). Default: nothing. `Err`: the derived state could not be built
    /// (a backend error); the load fails with it and the storage is dropped or cleared.
    fn finish_load(&mut self) -> Result<(), GraphError> {
        Ok(())
    }

    /// Empties data, schema, catalog, statistics and position (commit sequence, time, id
    /// sequences, lineage id), and KEEPS the registered commit hooks and the journal slot. An
    /// open unit is closed without a rollback. Used to load another state into the same
    /// instance. `Err`: the storage could not empty itself (a backend error); the Store
    /// operation that reloads fails with it and does not load into the storage.
    fn clear(&mut self) -> Result<(), GraphError>;

    /// Puts `journal` into the storage's journal slot
    /// ([`CommitHooks::set_journal`](crate::graph::CommitHooks::set_journal)).
    /// [`GraphError::Unsupported`] when a journal is attached already or a unit is open.
    fn attach_journal(&mut self, journal: JournalSlot) -> Result<(), GraphError>;

    /// Applies `changes` to a storage nobody sees yet (recovery: snapshot plus journal),
    /// outside units: no hook fires. With `verify`, every mutation's before image is compared
    /// with the current state first (through the read API) and a mismatch fails: the journal
    /// must continue exactly this state. Afterwards the commit position and the id sequences
    /// are raised to the change set's (never lowered). On error the storage is half-applied:
    /// drop or [`clear`](Self::clear) it. The error text names the element and the reason.
    ///
    /// Default: every mutation through the trait's write methods (the schema enforced, ids
    /// preserved), a schema change through [`Self::load_schema`], a stored definition through
    /// [`Self::load_definition`] and a removed one through
    /// [`GraphStorage::remove_definition`]; then [`GraphStorage::raise_id_sequences`] and
    /// [`Self::raise_position`]. Write methods outside a unit must record nothing (the
    /// [`GraphStorage`] contract); a storage whose `remove_definition` opens a unit of its own
    /// overrides this.
    fn replay(&mut self, changes: &ChangeSet, verify: bool) -> Result<(), String> {
        for mutation in changes.mutations() {
            if verify && let Some(reason) = replay::before_mismatch(&*self, mutation, true) {
                return Err(format!("{}: {reason}", replay::mutation_subject(mutation)));
            }
            replay_one(self, mutation)
                .map_err(|err| format!("{}: {err}", replay::mutation_subject(mutation)))?;
        }
        self.raise_id_sequences(changes.node_sequence(), changes.edge_sequence())
            .map_err(|err| err.to_string())?;
        self.raise_position(changes.commit_seq(), changes.committed_at())
            .map_err(|err| err.to_string())?;
        Ok(())
    }
}

/// One mutation of the default [`PersistentStorage::replay`].
fn replay_one<S: PersistentStorage + ?Sized>(
    storage: &mut S,
    mutation: &Mutation,
) -> Result<(), GraphError> {
    match mutation {
        Mutation::SetSchema { after, .. } => storage.load_schema((**after).clone()),
        Mutation::SetDefinition {
            after: Some(after), ..
        } => storage.load_definition((**after).clone()),
        Mutation::AddVertex { .. }
        | Mutation::DropVertex { .. }
        | Mutation::AddEdge { .. }
        | Mutation::DropEdge { .. }
        | Mutation::SetProperty { .. }
        | Mutation::RemoveProperty { .. }
        | Mutation::AddLabel { .. }
        | Mutation::RemoveLabel { .. }
        | Mutation::SetEdgeLabel { .. }
        | Mutation::SetDefinition { after: None, .. } => replay::apply_mutation(storage, mutation),
    }
}

impl PersistentStorage for TraversalGraph {
    fn graph_id(&self) -> u128 {
        *self.graph_id.get_or_init(crate::value::uuid::new_v7)
    }

    fn set_graph_id(&mut self, graph_id: u128) -> Result<(), GraphError> {
        self.graph_id = std::sync::OnceLock::from(graph_id);
        Ok(())
    }

    #[inline]
    fn last_committed_at(&self) -> i64 {
        self.undo.last_committed_at
    }

    #[inline]
    fn id_sequences(&self) -> (u64, u64) {
        (self.node_sequence, self.edge_sequence)
    }

    fn raise_position(&mut self, commit_seq: u64, committed_at: i64) -> Result<(), GraphError> {
        self.undo.last_commit_seq = self.undo.last_commit_seq.max(commit_seq);
        self.undo.last_committed_at = self.undo.last_committed_at.max(committed_at);
        Ok(())
    }

    fn load_vertex(
        &mut self,
        id: CompactString,
        labels: Vec<CompactString>,
        properties: PropertyMap,
    ) -> Result<Self::VertexId, GraphError> {
        if self.index.get_node(&self.graph, &id).is_some() {
            return Err(GraphError::duplicate_vertex_id(id.as_str()));
        }
        Ok(self.insert_vertex_raw(id, labels, properties))
    }

    fn load_edge(
        &mut self,
        id: CompactString,
        label: Option<CompactString>,
        from_id: &str,
        to_id: &str,
        properties: PropertyMap,
    ) -> Result<Self::EdgeId, GraphError> {
        if self.index.get_edge(&self.graph, &id).is_some() {
            return Err(GraphError::duplicate_edge_id(id.as_str()));
        }
        let endpoint = |graph: &Self, id: &str| {
            graph
                .index
                .get_node(&graph.graph, id)
                .ok_or_else(|| GraphError::VertexNotFound { id: id.to_string() })
        };
        let from = endpoint(self, from_id)?;
        let to = endpoint(self, to_id)?;
        Ok(self.insert_edge_raw(id, label, Some(properties), from, to))
    }

    fn load_schema(&mut self, schema: GraphSchema) -> Result<(), GraphError> {
        *self.schema.write() = schema;
        Ok(())
    }

    fn load_definition(&mut self, definition: Definition) -> Result<(), GraphError> {
        self.definitions.insert(definition);
        Ok(())
    }

    /// The compression load pass (the rules' dictionaries and compressed values), the build of
    /// every property index from the data (entries are never persisted), then
    /// [`TraversalGraph::shrink_to_fit`].
    fn reserve_load(&mut self, vertices: u64, edges: u64) -> Result<(), GraphError> {
        let room = |n: u64| usize::try_from(n).unwrap_or(0);
        self.graph.reserve_exact(room(vertices), room(edges));
        Ok(())
    }

    fn finish_load(&mut self) -> Result<(), GraphError> {
        self.compression_load_pass();
        self.property_index_rebuild_all();
        TraversalGraph::shrink_to_fit(self);
        Ok(())
    }

    fn clear(&mut self) -> Result<(), GraphError> {
        let hooks = std::mem::take(&mut self.undo.hooks);
        let policy = std::mem::replace(&mut self.execution_policy, ExecutionPolicy::permissive());
        *self = TraversalGraph::new().with_execution_policy(policy);
        self.undo.hooks = hooks;
        Ok(())
    }

    fn attach_journal(&mut self, journal: JournalSlot) -> Result<(), GraphError> {
        if self.undo_depth() > 0 {
            return Err(GraphError::Unsupported {
                operation: "attaching a journal while a unit is open",
            });
        }
        self.undo.hooks.set_journal(journal)
    }

    /// The raw path: with `verify` the before images are checked first
    /// (`replay_verified`), without it the inherent [`TraversalGraph::replay`] in place.
    fn replay(&mut self, changes: &ChangeSet, verify: bool) -> Result<(), String> {
        if verify {
            self.replay_verified(changes)
        } else {
            self.replay_in_place(changes).map_err(|err| err.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::graph::definition::{DefinitionKind, QueryDefinition};
    use crate::graph::hooks::{CommitHook, HookError};
    use crate::graph::schema::{PropertySchema, SchemaMode};
    use crate::graph::{EdgeHandle, Transactional, VertexHandle};

    /// A storage that keeps every default of [`PersistentStorage`] (above all `replay`), over a
    /// boxed `TraversalGraph` (whose `GraphStorage` forwards to the graph).
    impl PersistentStorage for Box<TraversalGraph> {
        fn graph_id(&self) -> u128 {
            (**self).graph_id()
        }
        fn set_graph_id(&mut self, graph_id: u128) -> Result<(), GraphError> {
            (**self).set_graph_id(graph_id)
        }
        fn last_committed_at(&self) -> i64 {
            PersistentStorage::last_committed_at(&**self)
        }
        fn id_sequences(&self) -> (u64, u64) {
            (**self).id_sequences()
        }
        fn raise_position(&mut self, commit_seq: u64, committed_at: i64) -> Result<(), GraphError> {
            (**self).raise_position(commit_seq, committed_at)
        }
        fn load_vertex(
            &mut self,
            id: CompactString,
            labels: Vec<CompactString>,
            properties: PropertyMap,
        ) -> Result<VertexHandle, GraphError> {
            (**self).load_vertex(id, labels, properties)
        }
        fn load_edge(
            &mut self,
            id: CompactString,
            label: Option<CompactString>,
            from_id: &str,
            to_id: &str,
            properties: PropertyMap,
        ) -> Result<EdgeHandle, GraphError> {
            (**self).load_edge(id, label, from_id, to_id, properties)
        }
        fn load_schema(&mut self, schema: GraphSchema) -> Result<(), GraphError> {
            (**self).load_schema(schema)
        }
        fn load_definition(&mut self, definition: Definition) -> Result<(), GraphError> {
            (**self).load_definition(definition)
        }
        fn clear(&mut self) -> Result<(), GraphError> {
            (**self).clear()
        }
        fn attach_journal(&mut self, journal: JournalSlot) -> Result<(), GraphError> {
            (**self).attach_journal(journal)
        }
    }

    struct Collect(Arc<Mutex<Vec<ChangeSet>>>);
    impl CommitHook for Collect {
        fn before_commit(&mut self, changes: &ChangeSet) -> Result<(), HookError> {
            self.0.lock().map_err(|_| "poisoned")?.push(changes.clone());
            Ok(())
        }
    }

    fn query(name: &str) -> Definition {
        Definition::query(
            name,
            QueryDefinition::new(format!("fn {name}() {{ g.v() }}")),
        )
    }

    /// A history that touches every mutation kind: a schema commit, a data and catalog
    /// commit, and a commit of drops, removals and relabels.
    fn journal() -> Vec<ChangeSet> {
        let collected = Arc::new(Mutex::new(Vec::new()));
        let mut live = TraversalGraph::new();
        live.register_commit_hook(Box::new(Collect(collected.clone())))
            .unwrap();
        live.set_schema(
            GraphSchema::new()
                .with_mode(SchemaMode::Open)
                .with_vertex("person", PropertySchema::object()),
        )
        .unwrap();
        live.transaction(|g| {
            let a = g.insert_vertex(Some("a".into()), vec!["person".into()], PropertyMap::new())?;
            let b = g.insert_vertex(None, vec!["person".into()], PropertyMap::new())?;
            let c = g.insert_vertex(Some("c".into()), vec![], PropertyMap::new())?;
            g.set_vertex_property(a, "name".into(), "ann".into())?;
            g.add_vertex_label(c, "robot".into())?;
            let e = g.insert_edge(None, Some("knows".into()), a, b, PropertyMap::new())?;
            g.set_edge_property(e, "w".into(), 1.5.into())?;
            g.insert_edge(
                Some("e2".into()),
                Some("likes".into()),
                b,
                c,
                PropertyMap::new(),
            )?;
            g.set_definition(query("q1"))?;
            g.set_definition(query("q2"))?;
            Ok::<_, GraphError>(())
        })
        .unwrap();
        live.transaction(|g| {
            let e = g
                .get_edge_by_id("e2")?
                .ok_or(GraphError::unsupported("e2"))?;
            g.set_edge_label(e, "loves".into())?;
            let a = g
                .get_vertex_by_id("a")?
                .ok_or(GraphError::unsupported("a"))?;
            g.drop_vertex_property("name", a)?;
            let c = g
                .get_vertex_by_id("c")?
                .ok_or(GraphError::unsupported("c"))?;
            g.remove_vertex_label(c, "robot")?;
            g.drop_vertex(a)?;
            g.remove_definition(DefinitionKind::QUERY, "q1")?;
            Ok::<_, GraphError>(())
        })
        .unwrap();
        collected.lock().unwrap().clone()
    }

    type State = (
        Vec<String>,
        Vec<String>,
        String,
        Vec<String>,
        u64,
        i64,
        (u64, u64),
    );

    fn state<S: PersistentStorage>(storage: &S) -> State {
        let mut vertices: Vec<String> = storage
            .vertices()
            .unwrap()
            .map(|v| {
                format!(
                    "{:?} {:?} {:?}",
                    storage.vertex_id(v).unwrap(),
                    storage.vertex_labels(v).unwrap(),
                    storage.vertex_properties(v).unwrap()
                )
            })
            .collect();
        vertices.sort();
        let mut edges: Vec<String> = storage
            .edges()
            .unwrap()
            .map(|e| {
                let ends = storage.edge_endpoints(e).unwrap().map(|(from, to)| {
                    (
                        storage.vertex_id(from).unwrap(),
                        storage.vertex_id(to).unwrap(),
                    )
                });
                format!(
                    "{:?} {:?} {:?} {:?}",
                    storage.edge_id(e).unwrap(),
                    storage.edge_label(e).unwrap(),
                    ends,
                    storage.edge_properties(e).unwrap()
                )
            })
            .collect();
        edges.sort();
        let schema = storage.get_schema().unwrap().unwrap_or_default().to_json();
        let definitions = storage
            .definitions()
            .unwrap()
            .map(|d| d.name().to_string())
            .collect();
        (
            vertices,
            edges,
            schema,
            definitions,
            storage.last_commit_seq().unwrap(),
            storage.last_committed_at(),
            storage.id_sequences(),
        )
    }

    #[test]
    fn default_replay_equals_the_override() {
        let journal = journal();
        assert_eq!(journal.len(), 3);
        for verify in [false, true] {
            let mut own = TraversalGraph::new();
            let mut boxed = Box::new(TraversalGraph::new());
            for changes in &journal {
                PersistentStorage::replay(&mut own, changes, verify).unwrap();
                PersistentStorage::replay(&mut boxed, changes, verify).unwrap();
            }
            let (mut own_state, mut boxed_state) = (state(&own), state(&boxed));
            // The wrapper forwards `remove_definition` to `TraversalGraph`, which makes a
            // catalog change outside a unit a unit of its own: it stamps its own (later)
            // commit time, which `raise_position` keeps (the exception the default documents).
            assert!(boxed_state.5 >= own_state.5);
            (own_state.5, boxed_state.5) = (0, 0);
            assert_eq!(own_state, boxed_state, "verify: {verify}");
            assert_eq!(own.vertex_count(), 2);
            assert_eq!(own.last_commit_seq(), 3);
            assert_eq!(own.definitions().unwrap().count(), 1);
        }
    }

    #[test]
    fn verified_replay_refuses_a_journal_that_does_not_continue_the_state() {
        let journal = journal();
        let mut own = TraversalGraph::new();
        PersistentStorage::replay(&mut own, &journal[1], true).unwrap();
        let err = PersistentStorage::replay(&mut own, &journal[1], true).unwrap_err();
        assert!(err.contains("exists already"), "{err}");
        let mut boxed = Box::new(TraversalGraph::new());
        PersistentStorage::replay(&mut boxed, &journal[1], true).unwrap();
        let err = PersistentStorage::replay(&mut boxed, &journal[1], true).unwrap_err();
        assert!(err.contains("exists already"), "{err}");
        // A commit whose elements are missing does not continue an empty state.
        let mut boxed = Box::new(TraversalGraph::new());
        let err = PersistentStorage::replay(&mut boxed, &journal[2], true).unwrap_err();
        assert!(err.contains("missing"), "{err}");
    }
}
