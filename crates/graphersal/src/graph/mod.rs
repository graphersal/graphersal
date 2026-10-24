use compact_str::CompactString;
use petgraph::prelude::StableGraph;
use std::num::NonZeroU32;
use std::sync::Arc;

use crate::error::{GraphError, SchemaError};

use crate::traversal::GraphTraversalSource;
use crate::value::{ElementProperty, PropertyMap};

/// The sealed dyn boundary the front ends hold ([`AnyGraph`]).
mod any_graph;
/// Authorization: the [`Authorizer`] hook and the ready-made [`AccessPolicy`].
pub mod authorization;
/// What a storage can do ([`StorageCapabilities`]).
pub mod capabilities;
pub(crate) mod capture;
/// Change capture: [`ChangeSet`] and [`Mutation`], what one committed unit changed.
pub mod change;
/// Compressed properties: compression rules ([`compression::CompressionDefinition`]) and their
/// in-memory state.
pub mod compression;
/// Estimating a compression rule before defining it: [`compression_estimate::CompressionEstimate`].
pub mod compression_estimate;
// The catalog of definitions (saved queries, ...) stored next to the schema; documented by its
// own module docs (an outer `///` here would resolve their links in this scope).
pub mod definition;
pub mod execution_policy;
/// Generational element handles ([`VertexHandle`], [`EdgeHandle`]).
pub mod handle;
/// Commit hooks ([`CommitHook`]): veto and observe what each unit of work changed.
pub mod hooks;
pub(crate) mod index;
/// A graph's own memory footprint (`memory_usage()`).
pub mod memory;
/// The memory report: where a graph's memory goes, per label and per property path.
pub mod memory_report;
#[cfg(feature = "persist")]
pub(crate) mod persistent;
/// Property indexes: declared, ordered indexes on `(element kind, label, key)`
/// ([`property_index::PropertyIndexDefinition`]) and their in-memory state.
pub mod property_index;
pub(crate) mod replay;
/// Schemas: the format, validation, inference, diff, patch and visualization.
pub mod schema;
/// Interned property key lists shared by the stored property maps.
pub(crate) mod shape;
mod shared;
mod statistics;
/// The [`GraphStorage`] trait and its implementations.
pub mod storage;
pub(crate) mod subgraph;
mod transaction;
pub(crate) mod undo;
pub mod write_checks;
pub use any_graph::{AnyGraph, IntoAnyGraph};
pub use authorization::{
    AccessPolicy, AccessRequest, AccessRequestBuf, Action, ActionKind, AllowAll, Authorizer,
    Denied, Resource, ResourceKind,
};
pub use capabilities::{IsolationLevel, StorageCapabilities};
pub use change::{
    ChangeSet, ChangeSetBuilder, CommitMetadata, EdgeState, ElementRef, Mutation, VertexState,
};
pub use compression::{CompressionDefinition, CompressionStats};
pub use compression_estimate::{CompressionEstimate, CompressionEstimateRequest};
pub use definition::{Definition, DefinitionKind, QueryDefinition};
pub use execution_policy::ExecutionPolicy;
pub use handle::{EdgeHandle, VertexHandle};
pub use hooks::{
    CommitHook, CommitHooks, HookError, HookRefusal, JournalSlot, Mark, RollbackReason,
};
pub(crate) use index::GraphIndex;
pub(crate) use index::{EdgeIndex, NodeIndex};
pub use memory::MemoryStats;
pub use memory_report::{MemoryReport, MemoryReportRequest, StructureMemory};
#[cfg(feature = "persist")]
pub use persistent::PersistentStorage;
pub use property_index::{
    PropertyIndexDefinition, PropertyIndexInfo, PropertyIndexRange, PropertyIndexStats,
    PropertyIndexTransform,
};
// Only the script layer names it through the crate glob.
#[cfg(feature = "script")]
pub use property_index::PropertyIndexType;
pub use schema::*;
pub use shared::{Graph, GraphRead, GraphWrite};
pub use statistics::GraphStatistics;
pub use storage::{GraphStorage, UnitChanges, UnitMark};
pub use transaction::Transactional;

/// Vertex weight stored in the underlying petgraph storage: `(id, labels, properties,
/// generation)`.
///
/// `labels` is an ordered, deduplicated set: empty means no labels, and the first entry is the
/// *primary* label — the one `vertex_label()`, `label()`, and `elementMap()`'s `"label"` key keep
/// returning. See [`storage::GraphStorage::vertex_labels`]. `generation` is the slot generation
/// the vertex was created with; a [`VertexHandle`] is valid iff it carries the same one.
pub(crate) type VertexData = (CompactString, Vec<CompactString>, PropertyMap, NonZeroU32);
/// Edge weight stored in the underlying petgraph storage: `(id, label, properties,
/// generations)`; see [`handle::EdgeGenerations`].
pub(crate) type EdgeData = (
    CompactString,
    Option<CompactString>,
    Option<PropertyMap>,
    handle::EdgeGenerations,
);

/// The underlying petgraph storage type: a stable-indexed directed graph of vertices/edges.
pub(crate) type GraphType = StableGraph<VertexData, EdgeData>;

/// In-memory graph structure holding vertices, edges, and property maps.
///
/// # Example
/// ```rust
/// use graphersal::prelude::*;
///
/// let graph = TraversalGraph::new();
/// assert_eq!(graph.vertex_count(), 0);
/// assert_eq!(graph.edge_count(), 0);
/// ```
#[allow(dead_code)]
pub struct TraversalGraph {
    pub(crate) graph: GraphType,
    pub(crate) index: GraphIndex,
    /// Slot generations of the vertices (see [`handle`]): bumped when a slot is freed.
    pub(crate) node_generations: handle::Generations,
    /// Slot generations of the edges (see [`handle`]): bumped when a slot is freed.
    pub(crate) edge_generations: handle::Generations,
    pub(crate) node_sequence: u64,
    pub(crate) edge_sequence: u64,
    pub(crate) schema: Arc<parking_lot::RwLock<GraphSchema>>,
    /// Host-set ceiling on which `g.with()` execution options a query may set, and their
    /// defaults when it doesn't. Set only via [`Self::with_execution_policy`], a consuming
    /// builder called at construction time, before this graph is ever wrapped in a
    /// `RwLock`/`Arc` for sharing — there is deliberately no setter after that point, not even
    /// one gated behind a write lock, so a query can never change the policy it executes under.
    /// See [`crate::graph::GraphStorage::execution_policy`].
    pub(crate) execution_policy: ExecutionPolicy,
    /// The undo log of the open units (see [`undo`]): empty outside a unit.
    pub(crate) undo: undo::UndoLog,
    /// Element counts kept up to date by the raw insert/remove/relabel helpers (see
    /// [`GraphStatistics`]); read through `GraphStorage::statistics`.
    pub(crate) statistics: GraphStatistics,
    /// The lineage id (a UUID v7) the persistence formats stamp on snapshots and journals:
    /// created on first use, restored by a snapshot load, set by [`Self::with_graph_id`].
    pub(crate) graph_id: std::sync::OnceLock<u128>,
    /// The catalog of definitions (saved queries, ...), kept beside the schema (see
    /// [`definition`]); changed through `GraphStorage::set_definition` / `remove_definition`.
    pub(crate) definitions: definition::Catalog,
    /// The in-memory state of the compression rules of the catalog (see [`compression`]):
    /// derived from `definitions`, like an index.
    pub(crate) compression: compression::CompressionRules,
    /// The entries of the property indexes of the catalog (see [`property_index`]): derived
    /// from `definitions` and the data, never persisted.
    pub(crate) property_indexes: property_index::PropertyIndexes,
    /// The cached total of the last footprint walk (see `memory::footprint_bytes`), read by the
    /// memory budget at the start of every run.
    pub(crate) footprint: memory::FootprintCache,
    /// The interned key lists of the stored property maps (see [`shape`]): every stored map is
    /// `Shaped` over one of them.
    pub(crate) shapes: shape::ShapeTable,
}

/// Whether an element is a vertex or an edge.
///
/// Closed set, so deliberately not `#[non_exhaustive]`: a property graph has exactly two element
/// kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphNodeType {
    /// A vertex.
    Vertex,
    /// An edge.
    Edge,
}

impl Default for TraversalGraph {
    fn default() -> Self {
        Self::new()
    }
}

impl TraversalGraph {
    /// A new, empty graph with a permissive execution policy.
    pub fn new() -> Self {
        Self {
            graph: GraphType::new(),
            index: GraphIndex::new(),
            node_generations: handle::Generations::default(),
            edge_generations: handle::Generations::default(),
            node_sequence: 0,
            edge_sequence: 0,
            schema: Arc::new(parking_lot::RwLock::new(GraphSchema::new())),
            execution_policy: ExecutionPolicy::permissive(),
            undo: undo::UndoLog::default(),
            statistics: GraphStatistics::new(),
            graph_id: std::sync::OnceLock::new(),
            definitions: definition::Catalog::default(),
            compression: compression::CompressionRules::default(),
            property_indexes: property_index::PropertyIndexes::default(),
            footprint: memory::FootprintCache::default(),
            shapes: shape::ShapeTable::new(),
        }
    }

    /// Sets this graph's [`ExecutionPolicy`], consuming and returning `Self` so it can only be
    /// set once, at construction time, before the graph is wrapped for sharing (`Graph` /
    /// `Arc<TraversalGraph>`).
    ///
    /// This is the *only* way a policy ever reaches a `TraversalGraph` — there is no setter that
    /// takes `&mut self` or `&self`, so a host that wants a query-proof ceiling on
    /// `evaluationTimeout`/`repeat.max_loops`/etc. must apply it here, before handing the graph to
    /// anything that might run an untrusted query against it:
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    /// use graphersal::exec::ExecutionPolicy;
    ///
    /// let policy = ExecutionPolicy::permissive()
    ///     .with_default("evaluationTimeout", 2000)
    ///     .with_locked("evaluationTimeout")
    ///     .with_default("repeat.max_loops", 500)
    ///     .with_locked("repeat.max_loops");
    /// let graph = TraversalGraph::tinkerpop_modern().with_execution_policy(policy);
    /// // Once wrapped for sharing, nothing can change the policy any more.
    /// let g = Graph::new(graph);
    /// ```
    pub fn with_execution_policy(mut self, policy: ExecutionPolicy) -> Self {
        self.execution_policy = policy;
        self
    }

    /// The commit sequence number of the last unit that changed something (`0`: none yet).
    ///
    /// Every traversal (and every explicit [`GraphStorage::begin_unit`] /
    /// [`GraphStorage::commit_unit`] pair) is a unit; its outermost commit advances the number
    /// by one when the unit changed something, also without commit hooks. A rolled-back,
    /// vetoed or read-only unit leaves it unchanged. A [`ChangeSet`] carries the number of its
    /// commit.
    ///
    /// Read under the same read access as a full export (GraphML through a read lock of
    /// [`Graph`]), it is the position of that snapshot: a server checkpoints the export and
    /// truncates its write-ahead log up to this number. A graph rebuilt from the snapshot
    /// continues from it with [`Self::with_last_commit_seq`].
    pub fn last_commit_seq(&self) -> u64 {
        self.commit_seq()
    }

    /// Sets the commit sequence number a graph that is being built continues from (a graph
    /// loaded from a snapshot taken at `last_commit_seq`), consuming and returning `Self` like
    /// [`Self::with_execution_policy`]: only a graph nobody sees yet can be renumbered. The next
    /// committed unit gets `last_commit_seq + 1`; [`Self::replay`] raises it further.
    pub fn with_last_commit_seq(mut self, last_commit_seq: u64) -> Self {
        self.undo.last_commit_seq = last_commit_seq;
        self
    }

    /// Sets the commit time ([`Self::last_committed_at`]) a graph that is being built continues
    /// from (a graph rebuilt from a snapshot or an export), consuming and returning `Self` like
    /// [`Self::with_last_commit_seq`]. The next commit's time is never earlier.
    pub fn with_last_committed_at(mut self, last_committed_at: i64) -> Self {
        self.undo.last_committed_at = last_committed_at;
        self
    }

    /// Sets the vertex and edge auto-id sequences ([`Self::node_sequence`],
    /// [`Self::edge_sequence`]) of a graph that is being built, consuming and returning `Self`:
    /// the next auto id is the next free number after them, so an id once handed out (also to
    /// an element that was dropped since) is not handed out again after a rebuild.
    pub fn with_sequences(mut self, node_sequence: u64, edge_sequence: u64) -> Self {
        self.node_sequence = node_sequence;
        self.edge_sequence = edge_sequence;
        self
    }

    /// The last vertex auto id handed out (`0`: none yet); see [`Self::with_sequences`].
    pub fn node_sequence(&self) -> u64 {
        self.node_sequence
    }

    /// The last edge auto id handed out (`0`: none yet); see [`Self::with_sequences`].
    pub fn edge_sequence(&self) -> u64 {
        self.edge_sequence
    }

    /// Sets the lineage id (`PersistentStorage::graph_id`) of a graph that is being built,
    /// consuming and returning `Self` (the builder form of `PersistentStorage::set_graph_id`).
    #[cfg(feature = "persist")]
    pub(crate) fn with_graph_id(mut self, graph_id: u128) -> Self {
        crate::graph::PersistentStorage::set_graph_id(&mut self, graph_id);
        self
    }

    /// The commit time of the last committed unit that changed something, in microseconds since
    /// the Unix epoch (UTC); `0` before the first commit. It is the
    /// [`ChangeSet::committed_at`] of that unit (also without commit hooks) and never goes
    /// back: every commit takes `max(now, last_committed_at)`. [`Self::replay`] raises it to
    /// the replayed change set's time. (With feature `persist` it is also
    /// `PersistentStorage::last_committed_at`.)
    pub fn last_committed_at(&self) -> i64 {
        self.undo.last_committed_at
    }

    /// Applies a [`ChangeSet`] to a graph that is not published yet: the recovery path of a
    /// server (snapshot plus write-ahead log) or a replica's initial load
    /// (`book/src/users_guide/transactions.md`).
    ///
    /// Runs OUTSIDE units: no undo log, no change capture, no commit hook fires, also when
    /// hooks are already registered (a server must not re-write the journal it is reading).
    /// Every mutation goes through the graph's own mutating methods (schema enforced, ids
    /// preserved, an explicit numeric id raises the auto-id sequence). Afterwards the commit
    /// sequence number, the commit time ([`Self::last_committed_at`]) and both auto-id
    /// sequences are raised to the change set's own ([`ChangeSet::node_sequence`],
    /// [`ChangeSet::edge_sequence`]; never lowered), so the next live commit continues the
    /// journal's numbering and its clock, and an auto id handed out to an element that was
    /// dropped or rolled back before is never handed out again. It consumes the graph: on error the half-built graph is dropped
    /// (cheaper than undoing), which is why it is not a method on a shared graph. A live graph
    /// applies a change set with [`GraphStorage::apply_change_set`] instead (one captured unit).
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    /// use graphersal::changes::{CommitHook, HookError};
    /// use graphersal::changes::ChangeSet;
    /// use std::sync::{Arc, Mutex};
    ///
    /// struct Journal(Arc<Mutex<Vec<ChangeSet>>>);
    /// impl CommitHook for Journal {
    ///     fn before_commit(&mut self, changes: &ChangeSet) -> Result<(), HookError> {
    ///         self.0.lock().map_err(|_| "poisoned")?.push(changes.clone());
    ///         Ok(())
    ///     }
    /// }
    ///
    /// // A live graph writes its journal...
    /// let journal = Arc::new(Mutex::new(Vec::new()));
    /// let mut live = TraversalGraph::new();
    /// live.register_commit_hook(Box::new(Journal(journal.clone()))).unwrap();
    /// live.traversal_mut().add_v("person").property("name", "ann").to_list().unwrap();
    ///
    /// // ...and recovery replays it into a fresh graph.
    /// let mut recovered = TraversalGraph::new();
    /// for changes in journal.lock().unwrap().iter() {
    ///     recovered = recovered.replay(changes).unwrap();
    /// }
    /// assert_eq!(recovered.vertex_count(), 1);
    /// assert_eq!(recovered.last_commit_seq(), live.last_commit_seq());
    /// ```
    pub fn replay(mut self, changes: &ChangeSet) -> Result<Self, GraphError> {
        self.replay_in_place(changes)?;
        Ok(self)
    }

    /// Shrinks the capacity of the graph indices as much as possible, and releases the shared
    /// property key lists no element uses any more.
    pub fn shrink_to_fit(&mut self) {
        self.index.shrink_to_fit();
        self.shapes.shrink_to_fit();
        self.node_generations.shrink_to_fit();
        self.edge_generations.shrink_to_fit();
    }

    /// The petgraph slot of a vertex handle, iff the vertex still exists: the slot is occupied
    /// and its occupant has the handle's generation. THE validation of every handle-taking
    /// method; a stale handle never resolves to the element that reuses its slot.
    #[inline]
    pub(crate) fn resolve_vertex(&self, handle: VertexHandle) -> Option<NodeIndex> {
        self.vertex_weight(handle).map(|_| handle.index())
    }

    /// The edge counterpart of [`Self::resolve_vertex`].
    #[inline]
    pub(crate) fn resolve_edge(&self, handle: EdgeHandle) -> Option<EdgeIndex> {
        self.edge_weight(handle).map(|_| handle.index())
    }

    /// The stored data of a live vertex ([`Self::resolve_vertex`] plus the read).
    #[inline]
    pub(crate) fn vertex_weight(&self, handle: VertexHandle) -> Option<&VertexData> {
        self.graph
            .node_weight(handle.index())
            .filter(|vertex| vertex.3 == handle.generation())
    }

    /// Mutable [`Self::vertex_weight`].
    #[inline]
    pub(crate) fn vertex_weight_mut(&mut self, handle: VertexHandle) -> Option<&mut VertexData> {
        self.graph
            .node_weight_mut(handle.index())
            .filter(|vertex| vertex.3 == handle.generation())
    }

    /// Runs `write` on the stored property map of a live vertex with the shape table beside it
    /// (a key-changing write goes through the table, or re-adopts the map after a generic
    /// helper); `None` when the vertex is gone.
    #[inline]
    pub(crate) fn with_vertex_map<R>(
        &mut self,
        handle: VertexHandle,
        write: impl FnOnce(&mut shape::ShapeTable, &mut PropertyMap) -> R,
    ) -> Option<R> {
        let vertex = self
            .graph
            .node_weight_mut(handle.index())
            .filter(|vertex| vertex.3 == handle.generation())?;
        Some(write(&mut self.shapes, &mut vertex.2))
    }

    /// Edge counterpart of [`Self::with_vertex_map`] (the map is optional on an edge).
    #[inline]
    pub(crate) fn with_edge_map<R>(
        &mut self,
        handle: EdgeHandle,
        write: impl FnOnce(&mut shape::ShapeTable, &mut Option<PropertyMap>) -> R,
    ) -> Option<R> {
        let edge = self
            .graph
            .edge_weight_mut(handle.index())
            .filter(|edge| edge.3.own == handle.generation())?;
        Some(write(&mut self.shapes, &mut edge.2))
    }

    /// The stored data of a live edge ([`Self::resolve_edge`] plus the read).
    #[inline]
    pub(crate) fn edge_weight(&self, handle: EdgeHandle) -> Option<&EdgeData> {
        self.graph
            .edge_weight(handle.index())
            .filter(|edge| edge.3.own == handle.generation())
    }

    /// Mutable [`Self::edge_weight`].
    #[inline]
    pub(crate) fn edge_weight_mut(&mut self, handle: EdgeHandle) -> Option<&mut EdgeData> {
        self.graph
            .edge_weight_mut(handle.index())
            .filter(|edge| edge.3.own == handle.generation())
    }

    /// Validates `schema` against the stored data and stores it: the body of
    /// [`GraphStorage::set_schema`], which records the old schema around it.
    ///
    /// In mode `Open` or `Closed` ALL existing vertices and edges are validated first, and on a
    /// violation the graph and its schema stay unchanged
    /// ([`SchemaError::ExistingDataViolatesSchema`]). Outside any unit the change is a unit of
    /// its own (recorded, captured as a [`Mutation::SetSchema`], seen by the commit hooks, which
    /// may veto it with [`GraphError::CommitRejected`]).
    pub(crate) fn store_schema(&self, schema: GraphSchema) -> Result<(), crate::error::GraphError> {
        let report = crate::graph::schema::apply::scan_storage(self, &schema);
        if !report.is_empty() {
            return Err(SchemaError::ExistingDataViolatesSchema {
                report: Box::new(report),
            }
            .into());
        }
        *self.schema.write() = schema;
        Ok(())
    }

    /// Closed mode: checks that every edge incident to `node` still matches a declared connection
    /// if `node` had the label set `new_labels` (a label change is rejected, atomically, when it
    /// would make an incident edge violate the connections). Other modes check nothing.
    pub(crate) fn check_incident_topology(
        &self,
        node: petgraph::graph::NodeIndex,
        new_labels: &[CompactString],
    ) -> Result<(), SchemaError> {
        use petgraph::visit::EdgeRef;
        let schema = self.schema.read();
        if !matches!(schema.mode, SchemaMode::Closed) {
            return Ok(());
        }
        let labels_of = |idx: petgraph::graph::NodeIndex| -> &[CompactString] {
            if idx == node {
                new_labels
            } else {
                self.graph
                    .node_weight(idx)
                    .map(|v| v.1.as_slice())
                    .unwrap_or(&[])
            }
        };
        for direction in [petgraph::Direction::Outgoing, petgraph::Direction::Incoming] {
            for edge in self.graph.edges_directed(node, direction) {
                SchemaValidator::check_topology(
                    &schema,
                    edge.weight().1.as_deref(),
                    labels_of(edge.source()),
                    labels_of(edge.target()),
                )?;
            }
        }
        Ok(())
    }

    /// The standard TinkerPop "modern" toy graph.
    // Based on the following schema: https://tinkerpop.apache.org/docs/current/images/tinkerpop-modern.png
    pub fn tinkerpop_modern() -> Self {
        let mut tg = TraversalGraph::new();

        let n_1 = tg
            .insert_vertex(Some("1".into()), vec!["person".into()], {
                let mut m = PropertyMap::default();
                m.insert("name", "marko".into());
                m.insert("age", ElementProperty::Number(29.into()));
                m
            })
            .unwrap();
        let n_2 = tg
            .insert_vertex(Some("2".into()), vec!["person".into()], {
                let mut m = PropertyMap::default();
                m.insert("name", "vadas".into());
                m.insert("age", ElementProperty::Number(27.into()));
                m
            })
            .unwrap();
        let n_3 = tg
            .insert_vertex(Some("3".into()), vec!["software".into()], {
                let mut m = PropertyMap::default();
                m.insert("name", "lop".into());
                m.insert("lang", "java".into());
                m
            })
            .unwrap();
        let n_4 = tg
            .insert_vertex(Some("4".into()), vec!["person".into()], {
                let mut m = PropertyMap::default();
                m.insert("name", "josh".into());
                m.insert("age", ElementProperty::Number(32.into()));
                m
            })
            .unwrap();
        let n_5 = tg
            .insert_vertex(Some("5".into()), vec!["software".into()], {
                let mut m = PropertyMap::default();
                m.insert("name", "ripple".into());
                m.insert("lang", "java".into());
                m
            })
            .unwrap();
        let n_6 = tg
            .insert_vertex(Some("6".into()), vec!["person".into()], {
                let mut m = PropertyMap::default();
                m.insert("name", "peter".into());
                m.insert("age", ElementProperty::Number(35.into()));
                m
            })
            .unwrap();

        let add_edge_helper = |tg: &mut TraversalGraph,
                               id: &str,
                               label: &str,
                               from: VertexHandle,
                               to: VertexHandle,
                               weight: f64| {
            let mut m = PropertyMap::default();
            m.insert("weight", ElementProperty::Number(weight.into()));
            tg.insert_edge(Some(id.into()), Some(label.into()), from, to, m)
                .unwrap();
        };

        add_edge_helper(&mut tg, "0", "knows", n_1, n_2, 0.5);
        add_edge_helper(&mut tg, "1", "created", n_1, n_3, 0.4);
        add_edge_helper(&mut tg, "2", "knows", n_1, n_4, 1.0);
        add_edge_helper(&mut tg, "3", "created", n_4, n_3, 0.4);
        add_edge_helper(&mut tg, "4", "created", n_4, n_5, 1.0);
        add_edge_helper(&mut tg, "5", "created", n_6, n_3, 0.2);

        tg
    }

    /// A small, deterministic, filesystem-like sample tree (37 vertices, 38 edges).
    ///
    /// Every vertex id is its path from the root (`"root"`, `"root/src/main.rs"`, ...) and every
    /// vertex carries exactly one label: `directory` (18), `file` (17) or `symlink` (2). Only the
    /// `name` property is set, normally to the last path segment, with a few deliberate
    /// exceptions for edge cases:
    ///
    /// - `root/#unnamed` has no `name` property at all;
    /// - `root/#42` has the `int64` name `42` (not a string);
    /// - `root/#slash` has the name `"a/b"` (contains a slash);
    /// - `root/docs/data[1].csv` and `root/docs/résumé.md` contain glob metacharacters and
    ///   non-ASCII characters.
    ///
    /// Edges:
    ///
    /// - `contains` (parent → child) for every non-root vertex, with id `contains:<child id>`
    ///   (36 edges);
    /// - `points_to` (symlink → target) with id `points_to:<source id>`: `root/y/loop → root`
    ///   (a cycle) and `root/latest → root/author` (a diamond: `root/author` has in-degree 2).
    ///
    /// The tree contains three `author` directories (`root/author`, `root/author/q/author`,
    /// `root/y/author`), each with a `*/some/*.rs` file below it, a `src/` directory
    /// (`main.rs`, `lib.rs`, `test_a.rs`, `test_b.rs`, `util/mod.rs`, `util/io.rs`) and a `docs/`
    /// directory (`guide.md`, `notes.txt`, `data[1].csv`, `résumé.md`). It is the fixture for
    /// the `glob_path` tests and documentation examples.
    pub fn file_tree() -> Self {
        /// `(id, label, name)`; `None` means no `name` property.
        const VERTICES: &[(&str, &str, Option<&str>)] = &[
            ("root", "directory", Some("root")),
            ("root/author", "directory", Some("author")),
            ("root/author/x", "directory", Some("x")),
            ("root/author/x/some", "directory", Some("some")),
            ("root/author/x/some/f.rs", "file", Some("f.rs")),
            ("root/author/x/some/readme.md", "file", Some("readme.md")),
            ("root/author/q", "directory", Some("q")),
            ("root/author/q/author", "directory", Some("author")),
            ("root/author/q/author/w", "directory", Some("w")),
            ("root/author/q/author/w/some", "directory", Some("some")),
            ("root/author/q/author/w/some/h.rs", "file", Some("h.rs")),
            ("root/y", "directory", Some("y")),
            ("root/y/author", "directory", Some("author")),
            ("root/y/author/z", "directory", Some("z")),
            ("root/y/author/z/some", "directory", Some("some")),
            ("root/y/author/z/some/g.rs", "file", Some("g.rs")),
            ("root/y/loop", "symlink", Some("loop")),
            ("root/src", "directory", Some("src")),
            ("root/src/main.rs", "file", Some("main.rs")),
            ("root/src/lib.rs", "file", Some("lib.rs")),
            ("root/src/test_a.rs", "file", Some("test_a.rs")),
            ("root/src/test_b.rs", "file", Some("test_b.rs")),
            ("root/src/util", "directory", Some("util")),
            ("root/src/util/mod.rs", "file", Some("mod.rs")),
            ("root/src/util/io.rs", "file", Some("io.rs")),
            ("root/docs", "directory", Some("docs")),
            ("root/docs/guide.md", "file", Some("guide.md")),
            ("root/docs/notes.txt", "file", Some("notes.txt")),
            ("root/docs/data[1].csv", "file", Some("data[1].csv")),
            ("root/docs/résumé.md", "file", Some("résumé.md")),
            ("root/#unnamed", "directory", None),
            ("root/#unnamed/orphan.rs", "file", Some("orphan.rs")),
            // The name of `root/#42` is the int64 `42`; set separately below.
            ("root/#42", "directory", None),
            ("root/#42/answer.rs", "file", Some("answer.rs")),
            ("root/#slash", "directory", Some("a/b")),
            ("root/#slash/c.rs", "file", Some("c.rs")),
            ("root/latest", "symlink", Some("latest")),
        ];
        /// `(source id, target id)` of the `points_to` edges.
        const POINTS_TO: &[(&str, &str)] =
            &[("root/y/loop", "root"), ("root/latest", "root/author")];

        let mut tg = TraversalGraph::new();
        let mut indices: std::collections::HashMap<&str, VertexHandle> =
            std::collections::HashMap::with_capacity(VERTICES.len());

        for &(id, label, name) in VERTICES {
            let mut props = PropertyMap::default();
            if let Some(name) = name {
                props.insert("name", name.into());
            } else if id == "root/#42" {
                props.insert("name", ElementProperty::Number(42i64.into()));
            }
            match tg.insert_vertex(Some(id.into()), vec![label.into()], props) {
                Ok(idx) => {
                    indices.insert(id, idx);
                }
                Err(err) => tracing::error!(id, %err, "file_tree: failed to add vertex"),
            }
        }

        let insert_edge =
            |tg: &mut TraversalGraph, edge_id: String, label: &str, from: &str, to: &str| match (
                indices.get(from),
                indices.get(to),
            ) {
                (Some(&f), Some(&t)) => {
                    if let Err(err) = tg.insert_edge(
                        Some(edge_id.as_str().into()),
                        Some(label.into()),
                        f,
                        t,
                        PropertyMap::default(),
                    ) {
                        tracing::error!(edge_id, %err, "file_tree: failed to add edge");
                    }
                }
                _ => tracing::error!(edge_id, "file_tree: missing edge endpoint"),
            };

        for &(id, _, _) in VERTICES {
            if let Some((parent, _)) = id.rsplit_once('/') {
                insert_edge(&mut tg, format!("contains:{id}"), "contains", parent, id);
            }
        }
        for &(from, to) in POINTS_TO {
            insert_edge(&mut tg, format!("points_to:{from}"), "points_to", from, to);
        }

        tg
    }

    /// Starts a read-only traversal source over this graph.
    pub fn traversal<'a>(&'a self) -> GraphTraversalSource<'a, &'a TraversalGraph> {
        GraphTraversalSource::new(self)
    }

    /// Starts a mutating traversal source over this graph.
    pub fn traversal_mut<'a>(&'a mut self) -> GraphTraversalSource<'a, &'a mut TraversalGraph> {
        GraphTraversalSource::new(self)
    }

    /// The next free vertex auto id. The sequence never wraps: at `u64::MAX` it fails with
    /// [`GraphError::SequenceOverflow`] (and stays at the maximum).
    #[inline]
    pub(crate) fn next_value_node_sequence(&mut self) -> Result<CompactString, GraphError> {
        loop {
            self.node_sequence =
                self.node_sequence
                    .checked_add(1)
                    .ok_or(GraphError::SequenceOverflow {
                        sequence: crate::error::VERTEX_ID_SEQUENCE,
                    })?;
            let candidate = CompactString::from(self.node_sequence.to_string());
            if !self.index.contains_node(&self.graph, &candidate) {
                return Ok(candidate);
            }
        }
    }

    /// The next free edge auto id; see [`Self::next_value_node_sequence`].
    #[inline]
    pub(crate) fn next_value_edge_sequence(&mut self) -> Result<CompactString, GraphError> {
        loop {
            self.edge_sequence =
                self.edge_sequence
                    .checked_add(1)
                    .ok_or(GraphError::SequenceOverflow {
                        sequence: crate::error::EDGE_ID_SEQUENCE,
                    })?;
            let candidate = CompactString::from(self.edge_sequence.to_string());
            if self.index.get_edge(&self.graph, &candidate).is_none() {
                return Ok(candidate);
            }
        }
    }

    /// The number of vertices currently stored.
    #[inline]
    pub fn vertex_count(&self) -> usize {
        self.graph.node_count()
    }

    /// The number of edges currently stored.
    #[inline]
    pub fn edge_count(&self) -> usize {
        self.graph.edge_count()
    }

    // The following helpers are only exercised by the crate's own unit tests below; production
    // code reaches the graph exclusively through the `GraphStorage` trait (`graph/storage.rs`).
    #[cfg(test)]
    #[inline]
    pub(crate) fn get_vertex_by_id<'a>(
        &'a self,
        id: &str,
    ) -> Option<(VertexHandle, &'a VertexData)> {
        if let Some(handle) = self.index.get_node(&self.graph, id) {
            let vertex = self.vertex_weight(handle);
            vertex.map(|v| (handle, v))
        } else {
            None
        }
    }

    #[cfg(test)]
    #[inline]
    pub(crate) fn get_edge_by_id<'a>(&'a self, id: &str) -> Option<(EdgeHandle, &'a EdgeData)> {
        if let Some(handle) = self.index.get_edge(&self.graph, id) {
            let edge = self.edge_weight(handle);
            edge.map(|e| (handle, e))
        } else {
            None
        }
    }

    #[cfg(test)]
    #[inline]
    pub(crate) fn drop_vertex(&mut self, vertex_index: VertexHandle) -> Result<(), GraphError> {
        <Self as GraphStorage>::drop_vertex(self, vertex_index)
    }

    #[cfg(test)]
    #[inline]
    pub(crate) fn drop_vertex_property(
        &mut self,
        prop: &str,
        vertex_index: VertexHandle,
    ) -> Result<(), GraphError> {
        <Self as GraphStorage>::drop_vertex_property(self, prop, vertex_index)
    }

    #[cfg(test)]
    #[inline]
    pub(crate) fn drop_edge(&mut self, edge_index: EdgeHandle) -> Result<(), GraphError> {
        <Self as GraphStorage>::drop_edge(self, edge_index)
    }

    #[cfg(test)]
    #[inline]
    pub(crate) fn drop_edge_property(
        &mut self,
        prop: &str,
        edge_index: EdgeHandle,
    ) -> Result<(), GraphError> {
        <Self as GraphStorage>::drop_edge_property(self, prop, edge_index)
    }

    /// Creates a vertex with the given ID (or an auto-generated one), labels, and properties,
    /// applying the schema's create-time defaults ([`GraphStorage::add_vertex`]).
    #[inline]
    pub fn add_vertex(
        &mut self,
        id: Option<CompactString>,
        labels: Vec<CompactString>,
        data: PropertyMap,
    ) -> Result<VertexHandle, GraphError> {
        <Self as GraphStorage>::add_vertex(self, id, labels, data)
    }

    /// Creates a directed edge between two vertices identified by their handles, applying the
    /// schema's create-time defaults ([`GraphStorage::add_edge`]).
    #[inline]
    pub fn add_edge(
        &mut self,
        id: Option<CompactString>,
        label: Option<CompactString>,
        v_from: VertexHandle,
        v_to: VertexHandle,
        data: PropertyMap,
    ) -> Result<EdgeHandle, GraphError> {
        <Self as GraphStorage>::add_edge(self, id, label, v_from, v_to, data)
    }

    /// Stores a vertex exactly as given, without schema defaults
    /// ([`GraphStorage::insert_vertex`]).
    #[inline]
    pub fn insert_vertex(
        &mut self,
        id: Option<CompactString>,
        labels: Vec<CompactString>,
        data: PropertyMap,
    ) -> Result<VertexHandle, GraphError> {
        <Self as GraphStorage>::insert_vertex(self, id, labels, data)
    }

    /// Stores a directed edge exactly as given, without schema defaults
    /// ([`GraphStorage::insert_edge`]).
    #[inline]
    pub fn insert_edge(
        &mut self,
        id: Option<CompactString>,
        label: Option<CompactString>,
        v_from: VertexHandle,
        v_to: VertexHandle,
        data: PropertyMap,
    ) -> Result<EdgeHandle, GraphError> {
        <Self as GraphStorage>::insert_edge(self, id, label, v_from, v_to, data)
    }

    /// Creates a directed edge between two vertices identified by their string IDs, applying
    /// the schema's create-time defaults ([`GraphStorage::add_edge`]).
    pub fn add_edge_by_ids(
        &mut self,
        id: Option<CompactString>,
        label: Option<CompactString>,
        from_id: &str,
        to_id: &str,
        properties: PropertyMap,
    ) -> Result<EdgeHandle, GraphError> {
        let from_node = self.index.get_node(&self.graph, from_id).ok_or_else(|| {
            GraphError::VertexNotFound {
                id: from_id.to_string(),
            }
        })?;
        let to_node =
            self.index
                .get_node(&self.graph, to_id)
                .ok_or_else(|| GraphError::VertexNotFound {
                    id: to_id.to_string(),
                })?;
        self.add_edge(id, label, from_node, to_node, properties)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_o1_vertex_lookup_and_uniqueness() {
        let mut tg = TraversalGraph::new();
        for i in 0..1000 {
            let id = format!("v_{i}");
            let idx = tg
                .insert_vertex(
                    Some(id.clone().into()),
                    vec!["node".into()],
                    PropertyMap::default(),
                )
                .expect("should add vertex");
            assert_eq!(tg.index.get_node(&tg.graph, &id), Some(idx));
        }

        assert_eq!(tg.vertex_count(), 1000);

        // Fast O(1) lookup check
        let (_idx, v) = tg.get_vertex_by_id("v_500").expect("should find v_500");
        assert_eq!(v.0, "v_500");
        assert_eq!(v.1.first().map(|l| l.as_str()), Some("node"));

        // Uniqueness constraint
        let duplicate_res = tg.insert_vertex(Some("v_500".into()), vec![], PropertyMap::default());
        assert!(duplicate_res.is_err());
    }

    #[test]
    fn test_vertex_deletion_and_readdition() {
        let mut tg = TraversalGraph::new();
        let idx = tg
            .insert_vertex(
                Some("v_test".into()),
                vec!["person".into()],
                PropertyMap::default(),
            )
            .expect("should add vertex");

        assert!(tg.get_vertex_by_id("v_test").is_some());
        assert!(tg.index.contains_node(&tg.graph, "v_test"));

        tg.drop_vertex(idx).expect("should drop vertex");

        assert!(tg.get_vertex_by_id("v_test").is_none());
        assert!(!tg.index.contains_node(&tg.graph, "v_test"));

        // Re-adding same ID must succeed after deletion
        let readd_idx = tg
            .insert_vertex(
                Some("v_test".into()),
                vec!["person".into()],
                PropertyMap::default(),
            )
            .expect("should re-add vertex");
        assert_eq!(tg.get_vertex_by_id("v_test").unwrap().0, readd_idx);
    }

    #[test]
    fn test_edge_indexing_add_lookup_drop() {
        let mut tg = TraversalGraph::new();
        let v1 = tg
            .insert_vertex(Some("v1".into()), vec![], PropertyMap::default())
            .unwrap();
        let v2 = tg
            .insert_vertex(Some("v2".into()), vec![], PropertyMap::default())
            .unwrap();

        let e_idx = tg
            .insert_edge(None, Some("knows".into()), v1, v2, PropertyMap::default())
            .expect("should add edge");
        let e_id = tg
            .index
            .get_edge_id_by_index(&tg.graph, e_idx)
            .expect("edge id exists")
            .to_string();

        assert_eq!(tg.index.get_edge(&tg.graph, &e_id), Some(e_idx));

        let (found_e_idx, e_weight) = tg.get_edge_by_id(&e_id).expect("should find edge");
        assert_eq!(found_e_idx, e_idx);
        assert_eq!(e_weight.0, e_id);
        assert_eq!(e_weight.1.as_deref(), Some("knows"));

        tg.drop_edge(e_idx).expect("should drop edge");
        assert!(tg.get_edge_by_id(&e_id).is_none());
        assert_eq!(tg.index.get_edge(&tg.graph, &e_id), None);
    }

    #[test]
    fn test_add_vertex_numeric_id_sequence_advancement() {
        let mut tg = TraversalGraph::new();
        let idx1 = tg
            .insert_vertex(Some("100".into()), vec![], PropertyMap::default())
            .expect("should add vertex 100");
        assert_eq!(tg.get_vertex_by_id("100").unwrap().0, idx1);

        let idx2 = tg
            .insert_vertex(None, vec![], PropertyMap::default())
            .expect("should add vertex auto id");
        let (found_idx, v) = tg.get_vertex_by_id("101").expect("auto id should be 101");
        assert_eq!(found_idx, idx2);
        assert_eq!(v.0, "101");
    }

    #[test]
    fn test_add_edge_numeric_id_sequence_advancement_and_uniqueness() {
        use crate::graph::storage::GraphStorage;
        let mut tg = TraversalGraph::new();
        let v1 = tg
            .insert_vertex(Some("1".into()), vec![], PropertyMap::default())
            .unwrap();
        let v2 = tg
            .insert_vertex(Some("2".into()), vec![], PropertyMap::default())
            .unwrap();

        // Add edge with explicit numeric ID via GraphStorage
        let e1 = GraphStorage::insert_edge(
            &mut tg,
            Some("50".into()),
            Some("knows".into()),
            v1,
            v2,
            PropertyMap::default(),
        )
        .expect("should add edge 50");
        assert_eq!(tg.get_edge_by_id("50").unwrap().0, e1);

        // Next auto-generated edge should be 51
        let e2 = GraphStorage::insert_edge(
            &mut tg,
            None,
            Some("knows".into()),
            v1,
            v2,
            PropertyMap::default(),
        )
        .expect("should add edge auto");
        assert_eq!(tg.get_edge_by_id("51").unwrap().0, e2);

        // Explicit collision should fail
        let duplicate_res = GraphStorage::insert_edge(
            &mut tg,
            Some("51".into()),
            None,
            v1,
            v2,
            PropertyMap::default(),
        );
        assert!(duplicate_res.is_err());
    }

    #[test]
    fn test_traversal_graph_shrink_to_fit() {
        use crate::graph::storage::GraphStorage;
        let mut tg = TraversalGraph::new();
        for i in 0..50 {
            let v = GraphStorage::insert_vertex(
                &mut tg,
                Some(format!("v_{}", i).into()),
                vec!["person".into()],
                PropertyMap::default(),
            )
            .unwrap();
            if i > 0 {
                let prev = tg.get_vertex_by_id(&format!("v_{}", i - 1)).unwrap().0;
                GraphStorage::insert_edge(
                    &mut tg,
                    Some(format!("e_{}", i).into()),
                    Some("knows".into()),
                    prev,
                    v,
                    PropertyMap::default(),
                )
                .unwrap();
            }
        }
        assert_eq!(tg.vertex_count(), 50);
        assert_eq!(tg.edge_count(), 49);

        tg.shrink_to_fit();

        assert_eq!(tg.vertex_count(), 50);
        assert_eq!(tg.edge_count(), 49);
        assert!(tg.get_vertex_by_id("v_0").is_some());
        assert!(tg.get_edge_by_id("e_1").is_some());
    }

    #[test]
    fn test_traversal_graph_schema_initialization() {
        let tg = TraversalGraph::new();
        let schema_read = tg.schema.read();
        assert!(schema_read.is_empty());
        assert_eq!(schema_read.mode, SchemaMode::None);
        assert!(schema_read.vertices.is_empty());
        assert!(schema_read.edges.is_empty());
    }
}
