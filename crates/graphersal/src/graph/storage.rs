use crate::graph::topology::Direction;
use compact_str::CompactString;
use std::sync::Arc;

use crate::error::GraphError;
use crate::graph::change::{ChangeSet, CommitMetadata};
use crate::graph::definition::{Definition, DefinitionKind};
use crate::graph::hooks::{Mark, RollbackReason};
use crate::graph::schema::defaults::CreateContext;
use crate::graph::schema::{GraphSchema, PropertySchema, SchemaInferer};
use crate::graph::undo::UndoEntry;
use crate::graph::{
    EdgeHandle, GraphNodeType, GraphStatistics, NodeIndex, TraversalGraph, VertexHandle,
};
use crate::graph::{IsolationLevel, StorageCapabilities};
use crate::value::{ElementProperty, PropertyMap};

use crate::graph::write_checks::{
    check_label, check_properties, check_property_value, normalize_vertex_labels,
    path_property_name, remove_path, single_segment_name, write_path, write_path_in,
};

/// The stored id of a vertex, as text.
///
/// Every vertex a [`TraversalGraph`] holds has an id (it assigns sequence ids on insert), so a
/// missing one means a handle to a removed vertex ([`GraphError::ElementRemoved`]) or a custom
/// storage without ids (an invalid element state), never a made-up id.
pub(crate) fn stored_vertex_id<S: GraphStorage>(
    storage: &S,
    vertex: S::VertexId,
) -> Result<&str, GraphError> {
    match storage.vertex_id(vertex) {
        Some(id) => Ok(id),
        None if !storage.contains_vertex(vertex) => Err(GraphError::vertex_removed()),
        None => Err(GraphError::InvalidElementState {
            reason: "vertex has no id".to_string(),
        }),
    }
}

/// The stored id of an edge, as text. See [`stored_vertex_id`].
pub(crate) fn stored_edge_id<S: GraphStorage>(
    storage: &S,
    edge: S::EdgeId,
) -> Result<&str, GraphError> {
    match storage.edge_id(edge) {
        Some(id) => Ok(id),
        None if !storage.contains_edge(edge) => Err(GraphError::edge_removed()),
        None => Err(GraphError::InvalidElementState {
            reason: "edge has no id".to_string(),
        }),
    }
}

/// [`stored_vertex_id`] for a filter (`hasId()`, `has(T.id, ..)`): `Ok(None)` for a removed
/// vertex, which the filter drops, as it drops any element whose data it cannot read.
pub(crate) fn filter_vertex_id<S: GraphStorage>(
    storage: &S,
    vertex: S::VertexId,
) -> Result<Option<&str>, GraphError> {
    match stored_vertex_id(storage, vertex) {
        Ok(id) => Ok(Some(id)),
        Err(GraphError::ElementRemoved { .. }) => Ok(None),
        Err(err) => Err(err),
    }
}

/// [`filter_vertex_id`] for edges.
pub(crate) fn filter_edge_id<S: GraphStorage>(
    storage: &S,
    edge: S::EdgeId,
) -> Result<Option<&str>, GraphError> {
    match stored_edge_id(storage, edge) {
        Ok(id) => Ok(Some(id)),
        Err(GraphError::ElementRemoved { .. }) => Ok(None),
        Err(err) => Err(err),
    }
}

/// The primary label of a vertex for `label()` and materialization: `Ok(None)` for an unlabeled
/// vertex, [`GraphError::ElementRemoved`] for a removed one. The liveness check runs only when
/// there is no label, so a labeled vertex costs nothing extra.
pub(crate) fn stored_vertex_label<S: GraphStorage>(
    storage: &S,
    vertex: S::VertexId,
) -> Result<Option<&str>, GraphError> {
    match storage.vertex_label(vertex) {
        Some(label) => Ok(Some(label)),
        None if !storage.contains_vertex(vertex) => Err(GraphError::vertex_removed()),
        None => Ok(None),
    }
}

/// The label set of a vertex for `labels()`; see [`stored_vertex_label`].
pub(crate) fn stored_vertex_labels<S: GraphStorage>(
    storage: &S,
    vertex: S::VertexId,
) -> Result<&[CompactString], GraphError> {
    let labels = storage.vertex_labels(vertex);
    if labels.is_empty() && !storage.contains_vertex(vertex) {
        return Err(GraphError::vertex_removed());
    }
    Ok(labels)
}

/// The label of an edge for `label()` and materialization; see [`stored_vertex_label`].
pub(crate) fn stored_edge_label<S: GraphStorage>(
    storage: &S,
    edge: S::EdgeId,
) -> Result<Option<&str>, GraphError> {
    match storage.edge_label(edge) {
        Some(label) => Ok(Some(label)),
        None if !storage.contains_edge(edge) => Err(GraphError::edge_removed()),
        None => Ok(None),
    }
}

/// Storage-agnostic backend the traversal pipeline runs against: implemented for
/// [`TraversalGraph`], its `&mut`/`Box`/`Arc` wrappers, `MockStorage` for tests, and as a
/// blanket for every storage `G` by `&G`, its read-only view (reads forwarded, writes rejected,
/// no capabilities, hooks refused), so a host storage gets the view the read terminals use
/// without writing it.
///
/// **EXPERIMENTAL in 0.2.x:** methods and associated types may change in any 0.2.x release
/// (`book/src/users_guide/transactions.md`: a layered storage and borrow-free read methods
/// are still to come). The crate `graphersal-storage-tests` (published, EXPERIMENTAL like this
/// trait) holds the conformance suite an implementation should pass:
/// `graphersal_storage_tests::conformance!(name, base = MyStorage::new() => base)`. Its
/// capability-conditional items run only for what [`Self::capabilities`] claims.
///
/// A host storage plugs into every front end that is generic over the storage: the engine
/// directly, the Rhai DSL and saved queries through [`Graph`](crate::graph::Graph) and
/// [`AnyGraph`](crate::graph::AnyGraph) (it must also be `Default + Send + Sync + 'static`), the
/// session of `graphersal-session`, and the Store when it is a `PersistentStorage` (feature
/// `persist`). What it implements, how it plugs in and what stays `TraversalGraph`-only:
/// the book page `for_developers/custom_storage.md`.
///
/// # A guide for storage implementers
///
/// Only the read methods, the element-inserting and element-removing methods and the property
/// writes are required. Everything else has a default that is correct for a simple,
/// non-transactional storage, so a new storage starts small and opts into more by overriding.
///
/// ## Inserting vs. adding
///
/// A storage implements the primitive [`Self::insert_vertex`] / [`Self::insert_edge`]: store
/// exactly this element (coerced and validated against the stored schema, nothing added).
/// Imports, change sets, replay and recovery, subgraph copies and loads call it, so they
/// reproduce stored data and never generate a value. The provided [`Self::add_vertex`] /
/// [`Self::add_edge`] (same arguments) create an element the way a query does, like SQL's
/// `INSERT`: they fill the schema's create-time defaults (`default`, `defaultFunction`) first
/// and then call the primitive; [`Self::add_vertex_with`] / [`Self::add_edge_with`] take an
/// explicit [`CreateContext`] (the time `now`/`today` store) and are the one override point per
/// kind (e.g. to borrow the schema instead of copying it through [`Self::get_schema`]).
///
/// ## Handles
///
/// [`Self::VertexId`] / [`Self::EdgeId`] are opaque, storage-local handles; the pipeline never
/// looks inside. A handle to a removed element must **never alias** another element, also not
/// one the storage later puts in the same place (slot reuse): reads through such a stale handle
/// behave as "removed" ([`Self::contains_vertex`] / [`Self::contains_edge`] are `false`, ids,
/// labels and properties are `None`, adjacency is empty), writes fail with
/// [`GraphError::ElementRemoved`], and a second drop is a no-op. [`TraversalGraph`] meets this
/// with generational handles ([`VertexHandle`] / [`EdgeHandle`]: slot plus generation). Override
/// `contains_*` when the default (`vertex_properties` / `edge_endpoints` is `Some`) is not cheap
/// or not exact for your storage. Checked by `stale_handle_never_aliases`.
///
/// ## Write checks and errors
///
/// Every write refuses a property value holding a vertex, an edge or a map with a non-string key
/// ([`GraphError::InvalidPropertyValue`]), and `insert_vertex`/`add_vertex_label` a label with
/// `"::"` ([`GraphError::InvalidLabel`], duplicates in `insert_vertex` collapse). The path writes
/// follow the JSONPath write rules and fail with [`GraphError::InvalidPropertyPath`]. These are
/// public helpers in [`crate::storage`] (`check_property_value`, `check_properties`,
/// `check_label`, `normalize_vertex_labels`, `path_property_name`, `write_path`,
/// `write_path_in`, `remove_path`, `remove_path_in`), the same functions [`TraversalGraph`] and
/// the defaults here call; they are the minimum the contract requires, and a storage may refuse
/// more with its own errors. Every error a storage returns has a constructor on [`GraphError`]
/// (`unsupported`, `element_removed`, `invalid_property_value`, `invalid_property_path`, ...).
///
/// ## Units of atomicity
///
/// The executor wraps every traversal in a *unit*: [`Self::begin_unit`] returns a [`UnitMark`],
/// and [`Self::commit_unit`] or [`Self::rollback_unit`] closes it, innermost first. Units nest:
/// an inner unit is a savepoint (an explicit transaction, a whole-script Rhai unit and a dry run
/// are outer units whose traversals are savepoints). The defaults do nothing, which makes a
/// **non-atomic** storage: a failing traversal keeps what it changed before the error. An
/// **atomic** storage
///
/// - undoes everything since a mark on `rollback_unit`: data, labels, properties, the schema,
///   and every index, count and id lookup derived from them (handles taken inside the unit stay
///   stale afterwards; automatic ids are not handed out again);
/// - answers [`Self::unit_changes`] exactly (part of the `atomic` contract: it makes
///   [`Execution::mutated`](crate::Execution::mutated) right for traversals nested in an outer
///   unit);
/// - counts its committed units in [`Self::last_commit_seq`] (only an outermost commit that
///   changed something advances it);
/// - reports `atomic: true` in [`Self::capabilities`].
///
/// **Outside a unit** a mutating method records nothing: no commit hook is called and
/// [`Self::last_commit_seq`] does not move (a builder, a loader and a generic replay rely on
/// it; checked by `mutation_outside_unit_records_nothing`). [`Self::set_schema`] and a catalog
/// change outside a unit are a unit of their own on [`TraversalGraph`].
///
/// Explicit transactions and dry runs are the blanket extension trait
/// [`Transactional`](crate::graph::Transactional) (`transaction`, `transaction_with`,
/// `dry_run`), available for every storage and built on three more unit methods:
///
/// - [`Self::set_unit_metadata`] hands the [`CommitMetadata`] of `transaction_with` to the open
///   unit right before its commit (the outer unit's wins); default: ignored.
/// - [`Self::discard_unit`] closes a unit by undoing it WITHOUT hooks and returns what it
///   changed as a never-committed [`ChangeSet`] (`commit_seq` 0); the data, schema and catalog
///   are as before [`Self::begin_unit`], the id sequences keep their progress. It requires
///   `change_capture`; the default rolls back and returns [`GraphError::Unsupported`], so
///   `dry_run` is unsupported on such a storage. Checked by
///   `discard_unit_restores_state_keeps_sequences`.
/// - [`Self::mark`] has the storage's journal record a named [`Mark`] (Rhai `g.mark`); default:
///   [`GraphError::MarkUnavailable`] (no journal).
///
/// A wrapper forwards all three, like [`Self::begin_unit`].
///
/// ## Capabilities
///
/// [`Self::capabilities`] states facts, not permissions ([`StorageCapabilities`]: `atomic`,
/// `change_capture`, `isolation`, `concurrent_writers`, `durability`, `writable`, `immutable`).
/// The default claims nothing. A claim must be consistent
/// ([`StorageCapabilities::is_consistent`]), and in particular **`change_capture` requires
/// `atomic`**: change capture exists for [commit hooks](crate::graph::CommitHook), whose
/// `before_commit` may veto a commit, and a veto is honoured by rolling the unit back. A storage
/// that cannot roll back would have to report a vetoed change as both "rejected" and "applied",
/// so there is no notification contract for it: it reports `change_capture: false` and keeps the
/// default [`Self::register_commit_hook`], which returns [`GraphError::Unsupported`]. A
/// wrapper forwards what it can actually guarantee (a shared `Arc` or the read-only `&G` view of
/// any storage claims nothing).
///
/// ## Change capture and hooks
///
/// With hooks registered, an atomic storage builds a [`ChangeSet`] at the outermost commit of
/// every unit that changed something (mutations in execution order with before and after
/// values, the commit sequence number, [`CommitMetadata`](crate::graph::CommitMetadata)) and
/// runs `before_commit` (veto: roll back, [`GraphError::CommitRejected`]), then `after_commit`;
/// a rolled-back outermost unit that had changed something reports `after_rollback`. The
/// [`changes`](crate::changes) module describes the events and a write-ahead-log pattern. A
/// storage builds its change sets with [`ChangeSet::builder`] and the
/// [`Mutation`](crate::graph::Mutation) constructors (there is no reusable undo log or capture
/// component: the storage derives what a unit changed itself) and embeds a
/// [`CommitHooks`](crate::graph::CommitHooks), which runs the hooks in the required order (user
/// hooks, then the journal slot) and whose documentation says when to call each event. A
/// storage that must also be persisted implements `PersistentStorage` (feature `persist`).
///
/// ## Applying change sets
///
/// [`Self::apply_change_set`] has a default over the trait's own mutating methods inside one
/// unit (ids preserved, the schema enforced), so it is atomic exactly when the storage is; it
/// ends by raising the auto-id sequences to the change set's through
/// [`Self::raise_id_sequences`], which a storage with numeric auto ids overrides (on
/// [`TraversalGraph`] a real raise makes the unit a commit even when every mutation was
/// compacted away: a sequence-only commit, so the raise reaches the hooks and the journal). To
/// rebuild a graph nobody sees yet use [`TraversalGraph::replay`], which runs outside units and
/// fires no hooks.
///
/// ## Statistics, schema and diagnostics
///
/// [`Self::statistics`] (default `None`: the executor scans instead) must, when reported, equal
/// what [`GraphStatistics::from_storage`] computes, after every mutation and every rollback.
/// [`Self::set_schema`] (default: unsupported), [`Self::get_schema`] and
/// [`Self::declared_property_schema`] default to "no schema"; [`Self::memory_usage`] and
/// [`Self::execution_policy`] are optional diagnostics and host policy.
///
/// ## The catalog of definitions
///
/// [`Self::definitions`] / [`Self::definition`] default to "none stored",
/// [`Self::set_definition`] / [`Self::remove_definition`] to [`GraphError::Unsupported`]. A
/// storage with a catalog validates every definition ([`Definition::validate`]) and, when
/// atomic, treats a catalog change like a schema change: recorded, rolled back, reported as
/// [`UnitChanges::definitions`] and captured as a
/// [`Mutation::SetDefinition`](crate::graph::Mutation::SetDefinition).
pub trait GraphStorage: Send + Sync {
    /// Opaque, storage-local vertex handle.
    ///
    /// **Contract:** a handle to a removed vertex must never resolve to another vertex, also not
    /// to one that a backend stores in the same place later (slot reuse). Reads through such a
    /// *stale* handle behave as "removed": [`Self::contains_vertex`] is `false`, `vertex_id`,
    /// `vertex_label` and `vertex_properties` are `None`, `vertex_labels` is empty, adjacency is
    /// empty; writes fail with [`GraphError::ElementRemoved`] and `drop_vertex` is a no-op.
    /// [`TraversalGraph`] meets this with generational handles
    /// ([`VertexHandle`]). Checked by `stale_handle_never_aliases` in
    /// `graphersal-storage-tests`.
    type VertexId: Eq + std::hash::Hash + Clone + Copy + Send + Sync + std::fmt::Debug;
    /// Opaque, storage-local edge handle. The contract of [`Self::VertexId`] applies: a handle
    /// to a removed edge never resolves to another edge.
    type EdgeId: Eq + std::hash::Hash + Clone + Copy + Send + Sync + std::fmt::Debug;

    /// The number of vertices currently stored.
    fn vertex_count(&self) -> usize;
    /// The number of edges currently stored.
    fn edge_count(&self) -> usize;

    /// Iterates every vertex handle.
    fn vertices(&self) -> impl Iterator<Item = Self::VertexId> + '_;
    /// Iterates every edge handle.
    fn edges(&self) -> impl Iterator<Item = Self::EdgeId> + '_;

    /// Looks up a vertex handle by its stored string id.
    fn get_vertex_by_id(&self, id: &str) -> Option<Self::VertexId>;
    /// Looks up an edge handle by its stored string id.
    fn get_edge_by_id(&self, id: &str) -> Option<Self::EdgeId>;

    /// Iterates the outgoing edges of `node`.
    fn out_edges(&self, node: Self::VertexId) -> impl Iterator<Item = Self::EdgeId> + '_;
    /// Iterates the incoming edges of `node`.
    fn in_edges(&self, node: Self::VertexId) -> impl Iterator<Item = Self::EdgeId> + '_;

    /// The stored string id of a vertex, or `None` for an invalid handle.
    fn vertex_id(&self, node: Self::VertexId) -> Option<&str>;
    /// The stored string id of an edge, or `None` for an invalid handle.
    fn edge_id(&self, edge: Self::EdgeId) -> Option<&str>;

    /// Returns the vertex's *primary* label — the first label ever assigned, or `None` for an
    /// unlabeled vertex. TinkerPop's single-valued `label()`/`elementMap()` contract is built on
    /// this; a vertex that carries more than one label (see [`Self::vertex_labels`]) still returns
    /// exactly one value here.
    fn vertex_label(&self, node: Self::VertexId) -> Option<&str>;
    /// The edge's label, or `None` for an unlabeled edge.
    fn edge_label(&self, edge: Self::EdgeId) -> Option<&str>;

    /// Returns the vertex's full ordered, deduplicated label set. Empty means no labels; a
    /// non-empty set's first entry is the vertex's primary label, the one [`Self::vertex_label`]
    /// returns. This is the storage-level primitive behind the `labels()` step and a future
    /// Cypher/GQL front end's `(n:A:B)` pattern.
    fn vertex_labels(&self, node: Self::VertexId) -> &[CompactString];

    /// Returns `true` iff `label` is one of the vertex's labels (see [`Self::vertex_labels`]).
    #[inline]
    fn vertex_has_label(&self, node: Self::VertexId, label: &str) -> bool {
        self.vertex_labels(node).iter().any(|l| l.as_str() == label)
    }

    /// The vertex's property map, or `None` for an invalid handle.
    fn vertex_properties(&self, node: Self::VertexId) -> Option<&PropertyMap>;
    /// The edge's property map, or `None` for an invalid handle.
    fn edge_properties(&self, edge: Self::EdgeId) -> Option<&PropertyMap>;

    /// The `(source, target)` vertex handles of an edge, or `None` for an invalid handle.
    fn edge_endpoints(&self, edge: Self::EdgeId) -> Option<(Self::VertexId, Self::VertexId)>;

    /// `true` iff `node` refers to a vertex that still exists (see [`Self::VertexId`]). Lets the
    /// pipeline tell a removed vertex from a live one without an id or label (`id()`/`label()` of
    /// a removed vertex fail with [`GraphError::ElementRemoved`]).
    ///
    /// Default: [`Self::vertex_properties`] is `Some`, which every live vertex has.
    fn contains_vertex(&self, node: Self::VertexId) -> bool {
        self.vertex_properties(node).is_some()
    }

    /// `true` iff `edge` refers to an edge that still exists; see [`Self::contains_vertex`].
    ///
    /// Default: [`Self::edge_endpoints`] is `Some`, which every live edge has.
    fn contains_edge(&self, edge: Self::EdgeId) -> bool {
        self.edge_endpoints(edge).is_some()
    }

    /// Vertices whose label set contains `label` — under multi-label this is set membership, not
    /// primary-label equality, so a vertex tagged with `label` alongside others is still returned.
    fn vertices_by_label<'a>(
        &'a self,
        label: &'a str,
    ) -> Box<dyn Iterator<Item = Self::VertexId> + 'a> {
        Box::new(
            self.vertices()
                .filter(move |&n| self.vertex_has_label(n, label)),
        )
    }

    /// Edges whose label equals `label`.
    fn edges_by_label<'a>(&'a self, label: &'a str) -> Box<dyn Iterator<Item = Self::EdgeId> + 'a> {
        Box::new(
            self.edges()
                .filter(move |&e| self.edge_label(e) == Some(label)),
        )
    }

    /// The element counts this storage keeps up to date (see [`GraphStatistics`]), or `None` if
    /// it keeps none. Default: `None`. The executor reads label counts only through this: with
    /// `None`, a fused count per label (`group_count_pushdown`) scans the elements instead, with the
    /// same result. A storage that reports statistics must report exactly what
    /// [`GraphStatistics::from_storage`] computes from its data.
    fn statistics(&self) -> Option<&GraphStatistics> {
        None
    }

    /// Infers the schema the currently stored vertices and edges satisfy (see
    /// [`SchemaInferer`]). Never stores anything.
    fn infer_schema(&self) -> GraphSchema
    where
        Self: Sized,
    {
        SchemaInferer::infer_from_storage(self)
    }

    /// Replaces the stored schema. In mode `Open`/`Closed` an implementation validates ALL
    /// existing data first (one pass) and, if anything violates it, leaves the graph and its
    /// current schema unchanged and returns
    /// [`SchemaError::ExistingDataViolatesSchema`](crate::error::SchemaError) (inside
    /// [`GraphError::Schema`]); mode `None` applies without validation. Storing the empty schema
    /// ([`GraphSchema::is_empty`]) removes the schema.
    ///
    /// Default: [`GraphError::Unsupported`]; a backend with schemas overrides this together with
    /// [`Self::get_schema`].
    fn set_schema(&mut self, schema: GraphSchema) -> Result<(), GraphError> {
        let _ = schema;
        Err(GraphError::Unsupported {
            operation: "schemas",
        })
    }

    /// The schema this graph *stores* (set by [`Self::set_schema`]), never an inferred one:
    /// `None` when no schema is stored. A snapshot (`subgraph()`) copies only what is stored.
    /// Default: `None`.
    fn get_schema(&self) -> Option<GraphSchema> {
        None
    }

    /// The schema declared for the top-level property `key` on an element with these `labels`
    /// in the graph's *stored* schema, independent of its
    /// [`SchemaMode`](crate::graph::schema::SchemaMode). Uses the first label (in order) whose
    /// schema declares `key`. Default: `None`, no declarations.
    ///
    /// This never consults an inferred schema: importers use it to restore logical types only
    /// from what the consumer explicitly declared.
    fn declared_property_schema(
        &self,
        kind: GraphNodeType,
        labels: &[CompactString],
        key: &str,
    ) -> Option<PropertySchema> {
        let _ = (kind, labels, key);
        None
    }

    /// Every stored [definition](crate::catalog) (saved queries, ...), by kind code
    /// and then by name (byte order). Default: none.
    fn definitions(&self) -> impl Iterator<Item = &Definition> + '_ {
        std::iter::empty()
    }

    /// The stored definition of `kind` named `name`. Default: `None`.
    fn definition(&self, kind: DefinitionKind, name: &str) -> Option<&Definition> {
        let _ = (kind, name);
        None
    }

    /// Stores `definition`, replacing the one of the same kind and name. It is
    /// [validated](Definition::validate) first; a refused definition is
    /// [`GraphError::Definition`] and changes nothing. Storing an equal definition again is
    /// not a change.
    ///
    /// A catalog change is a database change like a schema change: an atomic storage records
    /// it in the open unit (rolled back with it, reported by [`Self::unit_changes`] as
    /// `definitions`) and captures it as a [`Mutation::SetDefinition`](crate::graph::Mutation)
    /// for the commit hooks; [`TraversalGraph`] makes a call outside any unit a unit of its own
    /// and holds at most [`MAX_DEFINITIONS`](crate::graph::definition::MAX_DEFINITIONS).
    ///
    /// Default: [`GraphError::Unsupported`]; a backend with a catalog overrides this together
    /// with [`Self::definitions`], [`Self::definition`] and [`Self::remove_definition`].
    fn set_definition(&mut self, definition: Definition) -> Result<(), GraphError> {
        let _ = definition;
        Err(GraphError::Unsupported {
            operation: "definitions",
        })
    }

    /// Removes the definition of `kind` named `name`; `false` when there is none (nothing
    /// changes). Recorded, captured and a unit of its own like [`Self::set_definition`].
    /// Default: [`GraphError::Unsupported`].
    fn remove_definition(&mut self, kind: DefinitionKind, name: &str) -> Result<bool, GraphError> {
        let _ = (kind, name);
        Err(GraphError::Unsupported {
            operation: "definitions",
        })
    }

    /// Re-runs the explicit pass of the compression rule `name` (book page "Compressed
    /// Properties"): rebuilds its dictionary from the current values and re-encodes the values
    /// at its path. Changes no logical data, records and captures nothing. `Ok(false)`: no such
    /// rule. Default: [`GraphError::Unsupported`] (a storage without compressed properties keeps
    /// compression rules in its catalog like any other definition, but never applies them).
    fn recompress(&mut self, name: &str) -> Result<bool, GraphError> {
        let _ = name;
        Err(GraphError::unsupported("compressed properties"))
    }

    /// What the compression rule `name` holds right now: the compressed values at its path,
    /// their plain and stored sizes, its dictionary size. `None`: no such rule, or a storage
    /// without compressed properties (the default).
    fn compression_stats(&self, name: &str) -> Option<crate::graph::CompressionStats> {
        let _ = name;
        None
    }

    /// Estimates compression rules for one `(element, label, path)` without changing anything
    /// (book page "Compressed Properties", section "Estimating the gain"): what each candidate
    /// of `request` would hold over the current values. Reads only: no unit, no commit, no
    /// catalog change. A storage should stop early, with `cancelled` set, when
    /// [`CompressionEstimateRequest::cancel_token`](crate::graph::CompressionEstimateRequest::cancel_token)
    /// is cancelled. Default: [`GraphError::Unsupported`] (a storage without compressed
    /// properties).
    fn estimate_compression(
        &self,
        request: &crate::graph::CompressionEstimateRequest,
    ) -> Result<crate::graph::CompressionEstimate, GraphError> {
        let _ = request;
        Err(GraphError::unsupported("compressed properties"))
    }

    /// The property indexes this storage maintains and serves (book page "Property Indexes"),
    /// with their current stats: one entry per index the lookups below answer. The optimizer
    /// reads it once per run to pick an index, so it must be cheap (no walk over the data).
    /// Default: none (a storage without property indexes keeps index definitions in its catalog
    /// like any other definition, but never serves them).
    fn property_indexes(&self) -> Vec<crate::graph::PropertyIndexInfo> {
        Vec::new()
    }

    /// The vertices labelled `label` whose top-level property `key` lies in `range`, read from
    /// the property index on `(vertex, label, key)`; `None` when there is no such index (the
    /// default). The result may hold MORE vertices than the range does (the caller re-checks
    /// every condition), never fewer, each vertex once; the order is unspecified.
    fn vertices_by_property_index<'a>(
        &'a self,
        label: &'a str,
        key: &'a str,
        range: &crate::graph::PropertyIndexRange,
    ) -> Option<Box<dyn Iterator<Item = Self::VertexId> + 'a>> {
        let _ = (label, key, range);
        None
    }

    /// The edge counterpart of [`Self::vertices_by_property_index`].
    fn edges_by_property_index<'a>(
        &'a self,
        label: &'a str,
        key: &'a str,
        range: &crate::graph::PropertyIndexRange,
    ) -> Option<Box<dyn Iterator<Item = Self::EdgeId> + 'a>> {
        let _ = (label, key, range);
        None
    }

    /// The vertices labelled `label` whose top-level property `key`, transformed by
    /// `transform` (`to_lower`: the lowered text of a string value), lies in `range`, read from
    /// the property index on `(vertex, label, key, transform)` (book page "Property Indexes",
    /// section "Case-insensitive search"); `None` when there is no such index (the default).
    /// The same contract as [`Self::vertices_by_property_index`]: a superset, each vertex once.
    /// A storage serving such an index must report its
    /// [`skipped`](crate::graph::PropertyIndexStats::skipped) count in
    /// [`Self::property_indexes`].
    fn vertices_by_transformed_property_index<'a>(
        &'a self,
        label: &'a str,
        key: &'a str,
        transform: &crate::graph::PropertyIndexTransform,
        range: &crate::graph::PropertyIndexRange,
    ) -> Option<Box<dyn Iterator<Item = Self::VertexId> + 'a>> {
        let _ = (label, key, transform, range);
        None
    }

    /// The edge counterpart of [`Self::vertices_by_transformed_property_index`].
    fn edges_by_transformed_property_index<'a>(
        &'a self,
        label: &'a str,
        key: &'a str,
        transform: &crate::graph::PropertyIndexTransform,
        range: &crate::graph::PropertyIndexRange,
    ) -> Option<Box<dyn Iterator<Item = Self::EdgeId> + 'a>> {
        let _ = (label, key, transform, range);
        None
    }

    // Mutation methods required for side-effect steps
    /// Stores a new vertex exactly as given: the storage primitive every storage implements
    /// (the heap level under SQL's `INSERT`). It applies no schema defaults; it coerces and
    /// validates against the stored schema as every write does. Imports, change sets
    /// ([`Self::apply_change_set`]), replay and recovery, subgraph copies and loads call it, so
    /// they reproduce stored data and never generate a value. Creating an element the way a
    /// query does (with the schema's `default`/`defaultFunction`) is [`Self::add_vertex`].
    ///
    /// `labels` is the vertex's initial label set, in insertion order; an empty
    /// vec means no labels. A duplicate label in `labels` is deduplicated, not an error — labels
    /// are a set, not a multiset. Rejects a label containing the literal substring `"::"` (D9):
    /// the GraphSON and GraphML vertex-side multi-label encoding joins labels with `"::"`, so such a label would
    /// be ambiguous with the delimiter on export.
    fn insert_vertex(
        &mut self,
        id: Option<CompactString>,
        labels: Vec<CompactString>,
        properties: PropertyMap,
    ) -> Result<Self::VertexId, GraphError>;

    /// Adds a single label to an existing vertex. Returns `Ok(true)` if the label set actually
    /// changed, `Ok(false)` if the vertex already carried this label (a no-op, not an error), and
    /// `Err` for an invalid vertex handle or when the vertex would violate the schema under the
    /// new label set. Behind the `add_label()` step (a Graphersal extension, TinkerPop has none)
    /// and GQL's `SET n:Admin`.
    fn add_vertex_label(
        &mut self,
        node: Self::VertexId,
        label: CompactString,
    ) -> Result<bool, GraphError>;

    /// Removes a single label from an existing vertex. Returns `Ok(true)` if the label was present
    /// and is now removed, `Ok(false)` if the vertex never carried it, and `Err` for an invalid
    /// vertex handle or when the vertex would violate the schema under the remaining labels.
    /// Behind the `drop_label()` step and GQL's `REMOVE n:Admin`, mirroring
    /// [`Self::add_vertex_label`].
    fn remove_vertex_label(
        &mut self,
        node: Self::VertexId,
        label: &str,
    ) -> Result<bool, GraphError>;

    /// Changes the label of an existing edge (edges stay single-labeled; an unlabeled edge
    /// gets `label`). Returns `Ok(true)` if the label changed, `Ok(false)` if the edge already
    /// carried `label` (a no-op, not an error), and `Err` for an invalid edge handle or when the
    /// edge would violate the schema under the new label. The use case is fixing unlabeled or
    /// mislabeled edges, e.g. before freezing a schema as `closed`; the `set_label()` step calls it.
    ///
    /// A schema-enforcing implementation checks the edge as it is stored against the new label
    /// (`Closed`: the label is declared and the endpoints match a declared connection; `Open`
    /// and `Closed`: the new label's `required` keys are present and the stored values have the
    /// declared types) and changes nothing on a violation. Stored values are not coerced (reads
    /// never coerce): later writes coerce by the new label's declarations.
    ///
    /// Default: [`GraphError::Unsupported`].
    fn set_edge_label(
        &mut self,
        edge: Self::EdgeId,
        label: CompactString,
    ) -> Result<bool, GraphError> {
        let _ = (edge, label);
        Err(GraphError::Unsupported {
            operation: "changing an edge label",
        })
    }

    /// Stores a new directed edge between two vertices exactly as given: the edge counterpart
    /// of [`Self::insert_vertex`] (the required primitive; no schema defaults). Creating an edge
    /// the way a query does is [`Self::add_edge`].
    fn insert_edge(
        &mut self,
        id: Option<CompactString>,
        label: Option<CompactString>,
        from: Self::VertexId,
        to: Self::VertexId,
        properties: PropertyMap,
    ) -> Result<Self::EdgeId, GraphError>;

    /// Creates a vertex the way a query does (SQL's `INSERT`): the stored schema's create-time
    /// defaults first, then [`Self::insert_vertex`]. The same as [`Self::add_vertex_with`] with
    /// [`CreateContext::for_storage`] (`now`/`today` = the start of the outermost open unit,
    /// else the system clock). Same arguments as [`Self::insert_vertex`]; on a graph whose
    /// schema declares no defaults both store the same.
    ///
    /// Do not override it: a storage that wants its own create path overrides
    /// [`Self::add_vertex_with`], which this calls.
    fn add_vertex(
        &mut self,
        id: Option<CompactString>,
        labels: Vec<CompactString>,
        properties: PropertyMap,
    ) -> Result<Self::VertexId, GraphError> {
        let context = CreateContext::for_storage(self);
        self.add_vertex_with(id, labels, properties, &context)
    }

    /// Creates an edge the way a query does: the edge counterpart of [`Self::add_vertex`]
    /// ([`Self::add_edge_with`] with [`CreateContext::for_storage`]).
    fn add_edge(
        &mut self,
        id: Option<CompactString>,
        label: Option<CompactString>,
        from: Self::VertexId,
        to: Self::VertexId,
        properties: PropertyMap,
    ) -> Result<Self::EdgeId, GraphError> {
        let context = CreateContext::for_storage(self);
        self.add_edge_with(id, label, from, to, properties, &context)
    }

    /// Creates a vertex the way a query does (`add_v`, `mergeV` on create) at the time
    /// `context` carries: first adds the stored schema's create-time defaults
    /// (`default`/`defaultFunction` of every top-level property the labels declare and
    /// `properties` lacks, in mode `open` and `closed`; see
    /// [`Annotations`](crate::graph::schema::Annotations)), then calls [`Self::insert_vertex`],
    /// which coerces and validates as always. [`Self::insert_vertex`] itself stores exactly what
    /// it is given: imports, change sets and replay use it, so they never apply defaults.
    ///
    /// `context` carries the time `defaultFunction` `now`/`today` store
    /// ([`CreateContext::now`]): a query passes one per execution (pinned by the script DSL's
    /// evaluation `now()` or [`GraphTraversalSource::with_now`](crate::GraphTraversalSource::with_now),
    /// else the unit's start, [`Self::unit_started_at`]); [`Self::add_vertex`] builds one with
    /// [`CreateContext::for_storage`], [`CreateContext::at`] pins a time. The stored value is a
    /// plain value, recorded as such in the change set and the journal, so replay is
    /// deterministic.
    ///
    /// This is the one override point of the create path for vertices: the default
    /// implementation reads the schema through [`Self::get_schema`]; a storage that can borrow
    /// its schema overrides it to save that copy (and still ends in [`Self::insert_vertex`]).
    fn add_vertex_with(
        &mut self,
        id: Option<CompactString>,
        labels: Vec<CompactString>,
        mut properties: PropertyMap,
        context: &CreateContext,
    ) -> Result<Self::VertexId, GraphError> {
        if let Some(schema) = self.get_schema() {
            // A repeated label changes nothing: the first label declaring a key decides.
            crate::graph::schema::defaults::fill_defaults(
                &schema,
                GraphNodeType::Vertex,
                &labels,
                &mut properties,
                context,
            );
        }
        self.insert_vertex(id, labels, properties)
    }

    /// Creates an edge the way a query does (`add_e`, `mergeE` on create) at the time `context`
    /// carries: the edge counterpart of [`Self::add_vertex_with`] (the label's create-time
    /// defaults, then [`Self::insert_edge`]) and the one override point of the edge create path.
    fn add_edge_with(
        &mut self,
        id: Option<CompactString>,
        label: Option<CompactString>,
        from: Self::VertexId,
        to: Self::VertexId,
        mut properties: PropertyMap,
        context: &CreateContext,
    ) -> Result<Self::EdgeId, GraphError> {
        if let Some(schema) = self.get_schema() {
            crate::graph::schema::defaults::fill_defaults(
                &schema,
                GraphNodeType::Edge,
                label.as_slice(),
                &mut properties,
                context,
            );
        }
        self.insert_edge(id, label, from, to, properties)
    }

    /// Sets (inserts or overwrites) one property on a vertex.
    fn set_vertex_property(
        &mut self,
        node: Self::VertexId,
        key: CompactString,
        value: ElementProperty,
    ) -> Result<(), GraphError>;

    /// Sets (inserts or overwrites) one property on an edge.
    fn set_edge_property(
        &mut self,
        edge: Self::EdgeId,
        key: CompactString,
        value: ElementProperty,
    ) -> Result<(), GraphError>;

    /// Sets the value at a JSONPath below a vertex property (`property(jpath(..), value)`): the
    /// first path segment names the stored property, the rest descends into its value. See
    /// [`crate::JsonPath`] for the write rules (missing keys are created, `[len]` appends, any
    /// mismatch is an error).
    ///
    /// The default reads the top-level property, applies the write to a copy and stores the
    /// whole result through [`Self::set_vertex_property`], so schema enforcement sees the
    /// complete resulting value; a backend may override it to mutate in place.
    fn set_vertex_property_path(
        &mut self,
        node: Self::VertexId,
        path: &crate::value::JsonPath,
        value: ElementProperty,
    ) -> Result<(), GraphError> {
        let name = path_property_name(path)?;
        let existing = self
            .vertex_properties(node)
            .and_then(|props| props.get(name.as_str()));
        let (key, resulting) = write_path(existing, path, value)?;
        self.set_vertex_property(node, key, resulting)
    }

    /// Edge counterpart of [`Self::set_vertex_property_path`].
    fn set_edge_property_path(
        &mut self,
        edge: Self::EdgeId,
        path: &crate::value::JsonPath,
        value: ElementProperty,
    ) -> Result<(), GraphError> {
        let name = path_property_name(path)?;
        let existing = self
            .edge_properties(edge)
            .and_then(|props| props.get(name.as_str()));
        let (key, resulting) = write_path(existing, path, value)?;
        self.set_edge_property(edge, key, resulting)
    }

    /// Removes the value at a JSONPath below a vertex property (`remove_property(jpath(..))`):
    /// the first segment names the stored property, the rest descends into its value; a
    /// one-segment path is a plain [`Self::drop_vertex_property`]. An object key is removed
    /// (an emptied parent stays), an array element is removed with shift (`[-k]` counts from the
    /// end). A missing path, an out-of-range index or a type mismatch on the way is NOT an
    /// error: nothing changes. See [`crate::JsonPath`].
    ///
    /// The default computes the whole resulting property on a copy and stores it through
    /// [`Self::set_vertex_property`], so schema enforcement sees the complete value; a backend
    /// may override it to mutate in place.
    fn remove_vertex_property_path(
        &mut self,
        node: Self::VertexId,
        path: &crate::value::JsonPath,
    ) -> Result<(), GraphError> {
        if let Some(name) = single_segment_name(path) {
            return self.drop_vertex_property(name, node);
        }
        let Some(props) = self.vertex_properties(node) else {
            return Err(GraphError::vertex_removed());
        };
        let existing = match path.segments().first() {
            Some(crate::value::json_path::JsonPathSegment::Key(name)) => props.get(name.as_str()),
            _ => None,
        };
        match remove_path(existing, path) {
            Some((key, resulting)) => self.set_vertex_property(node, key, resulting),
            None => Ok(()),
        }
    }

    /// Edge counterpart of [`Self::remove_vertex_property_path`].
    fn remove_edge_property_path(
        &mut self,
        edge: Self::EdgeId,
        path: &crate::value::JsonPath,
    ) -> Result<(), GraphError> {
        if let Some(name) = single_segment_name(path) {
            return self.drop_edge_property(name, edge);
        }
        let existing = match path.segments().first() {
            Some(crate::value::json_path::JsonPathSegment::Key(name)) => self
                .edge_properties(edge)
                .and_then(|props| props.get(name.as_str())),
            _ => None,
        };
        match remove_path(existing, path) {
            Some((key, resulting)) => self.set_edge_property(edge, key, resulting),
            None => Ok(()),
        }
    }

    /// Removes a vertex and its incident edges.
    fn drop_vertex(&mut self, node: Self::VertexId) -> Result<(), GraphError>;

    /// Removes one property from a vertex.
    fn drop_vertex_property(&mut self, prop: &str, node: Self::VertexId) -> Result<(), GraphError>;

    /// Removes an edge.
    fn drop_edge(&mut self, edge: Self::EdgeId) -> Result<(), GraphError>;

    /// Removes one property from an edge.
    fn drop_edge_property(&mut self, prop: &str, edge: Self::EdgeId) -> Result<(), GraphError>;

    /// Shrinks the capacity of internal indices as much as possible. Default: a no-op.
    fn shrink_to_fit(&mut self) {}

    /// An approximate memory-footprint snapshot, broken down by subsystem, or `None` if this
    /// storage backend does not support introspection. Default: `None` — a backend opts in by
    /// overriding this; callers must not assume `Some` just because the trait method exists.
    ///
    /// Placed on the trait, not as a `TraversalGraph`-only inherent method, so the pipeline's
    /// storage-backend-agnostic design isn't broken by this diagnostic: a future storage backend
    /// can be substituted without losing (or having to fake) this capability.
    fn memory_usage(&self) -> Option<crate::graph::MemoryStats> {
        None
    }

    /// What only the storage can tell about its memory for the memory report
    /// ([`MemoryReport::structure`](crate::graph::MemoryReport::structure)): the totals of
    /// [`Self::memory_usage`], the vertex and edge slots (live, tombstoned, spare), the id and
    /// label index bytes and the shape table, or `None` (the default) when the storage does not
    /// report them. The rest of the memory report (per label and per property, indexes, rules) is
    /// computed for any storage through the read methods; a storage needs nothing for it.
    fn memory_structure(&self) -> Option<crate::graph::StructureMemory> {
        None
    }

    /// The total of [`Self::memory_usage`] ([`MemoryStats::total_bytes`]), or `None` without
    /// introspection: the figure the estimated memory budget (`memory.limit` without a tracking
    /// allocator) starts every run from. A storage may cache it by its commit position
    /// ([`Self::last_commit_seq`]) so that a run on an unchanged graph does not walk it again;
    /// `TraversalGraph` does (also keyed by its element counts; not cached while an open unit
    /// holds uncommitted changes). Default: [`Self::memory_usage`]'s total, uncached.
    ///
    /// [`MemoryStats::total_bytes`]: crate::graph::MemoryStats::total_bytes
    fn memory_footprint_bytes(&self) -> Option<u64> {
        self.memory_usage().map(|stats| stats.total_bytes())
    }

    /// The host-set ceiling this storage instance's queries execute under: which `g.with()`
    /// options are locked, and their defaults when a query's own options don't set them. Default:
    /// [`crate::graph::ExecutionPolicy::permissive`] — nothing locked, no defaults overridden,
    /// identical to this crate's pre-policy behavior.
    ///
    /// Placed on the trait, not as a `TraversalGraph`-only inherent method, for the same reason
    /// [`Self::memory_usage`] is: a future storage backend can be substituted without losing this
    /// capability. Returned by value (matching `memory_usage()`'s `Option<MemoryStats>` shape)
    /// rather than by reference, since `ExecutionPolicy` is small and this sidesteps any
    /// lifetime/static-storage question for a default implementation.
    fn execution_policy(&self) -> crate::graph::ExecutionPolicy {
        crate::graph::ExecutionPolicy::permissive()
    }

    /// Opens a unit of atomicity and returns its mark.
    ///
    /// The executor opens one unit around every traversal it runs (implicit auto-commit) and
    /// closes it with [`Self::commit_unit`] when the traversal succeeded or
    /// [`Self::rollback_unit`] when it failed, so a failing traversal leaves nothing behind.
    /// Units nest: an inner unit is a savepoint, and rolling it back undoes only what happened
    /// since its mark. Every `begin_unit` is closed by exactly one `commit_unit` or
    /// `rollback_unit` with its own mark, innermost first.
    ///
    /// Default: a non-atomic storage. The mark carries nothing, commit and rollback do nothing,
    /// and a failing traversal keeps the changes it made before the error. [`TraversalGraph`]
    /// implements units with an internal undo log.
    fn begin_unit(&mut self) -> UnitMark {
        UnitMark::default()
    }

    /// Closes the innermost unit `mark` and keeps its changes. When it is the outermost unit,
    /// the changes become final.
    ///
    /// On a storage with [commit hooks](crate::graph::CommitHook) the outermost commit of a
    /// unit that changed something runs every `before_commit` first; a veto rolls the whole
    /// unit back (every hook gets `after_rollback`) and is returned as
    /// [`GraphError::CommitRejected`]. The unit is closed either way. Default: a no-op returning
    /// `Ok(())`; see [`Self::begin_unit`].
    fn commit_unit(&mut self, mark: UnitMark) -> Result<(), GraphError> {
        let _ = mark;
        Ok(())
    }

    /// Closes the innermost unit `mark` and undoes every change made since it was opened,
    /// including schema changes; the indexes, counts and id lookups are restored with the data.
    /// Handles taken inside the unit are stale afterwards. `reason` is what the
    /// [commit hooks](crate::graph::CommitHook) receive in `after_rollback` when this is the
    /// outermost unit and it had changed something. Default: a no-op (nothing is undone); see
    /// [`Self::begin_unit`].
    fn rollback_unit(&mut self, mark: UnitMark, reason: &RollbackReason) {
        let _ = (mark, reason);
    }

    /// What the open unit `mark` currently holds: whether data and whether the schema changed
    /// since it was opened and the change is still in effect (a nested unit that was rolled
    /// back does not count). Asked before the unit is closed; it tells whether a traversal or a
    /// script nested in an enclosing unit (an explicit transaction, a whole-script unit) changed
    /// something, which the commit sequence number cannot tell because only the outermost commit
    /// advances it ([`Execution::mutated`](crate::Execution::mutated),
    /// [`Execution::changes`](crate::Execution::changes)).
    ///
    /// **Part of the `atomic` contract:** a storage whose
    /// [`capabilities`](Self::capabilities) report [`atomic`](crate::graph::StorageCapabilities::atomic)
    /// must implement it; keeping the default (nothing) there makes `Execution::mutated` wrong
    /// for every traversal inside an explicit transaction or a whole-script unit, and the
    /// conformance suite (`graphersal-storage-tests`,
    /// `unit_changes_never_claims_an_undone_change`) fails.
    ///
    /// Default: [`UnitChanges::NONE`] (a non-atomic storage does not track units).
    fn unit_changes(&self, mark: UnitMark) -> UnitChanges {
        let _ = mark;
        UnitChanges::NONE
    }

    /// Attaches [`CommitMetadata`] (a principal, free attributes) to the open unit, so that the
    /// [`ChangeSet`] of the outermost commit carries it to the
    /// [commit hooks](crate::graph::CommitHook).
    ///
    /// [`Transactional::transaction_with`](crate::graph::Transactional::transaction_with) calls
    /// it right before it commits its unit, innermost unit first. The rule an implementation
    /// keeps: the metadata given for an outer unit wins over the one given for a unit nested
    /// in it (a principal only when none is set yet, attributes only for keys not set yet), and
    /// only the outermost unit's commit reports it; when the outermost unit closes, the
    /// metadata is reset. [`TraversalGraph`] does exactly this: a call while the outermost unit
    /// is the only one open takes precedence over what nested units left, a call inside a
    /// nested unit only adds what is missing. Without an open unit the call is ignored.
    ///
    /// Default: ignores the metadata (a storage without change capture has nobody to report it
    /// to).
    fn set_unit_metadata(&mut self, metadata: CommitMetadata) {
        let _ = metadata;
    }

    /// Closes the innermost unit `mark` by undoing everything since it was opened WITHOUT
    /// telling the [commit hooks](crate::graph::CommitHook), and returns what it had changed:
    /// "roll back without hooks, tell me what it changed", the end of a
    /// [dry run](crate::graph::Transactional::dry_run).
    ///
    /// The returned [`ChangeSet`] lists the unit's changes in execution order, exactly as a
    /// commit hook would have received them, with [`ChangeSet::commit_seq`] and
    /// [`ChangeSet::committed_at`] `0` (never committed) and empty metadata; its
    /// [`ChangeSet::node_sequence`] / [`ChangeSet::edge_sequence`] are the auto-id sequences
    /// after the unit, which keep their progress (an id once handed out is never reused). The
    /// data, the schema and the catalog are as before [`Self::begin_unit`];
    /// [`Self::last_commit_seq`] does not move.
    ///
    /// Requires [`StorageCapabilities::change_capture`]: only a storage that captures changes
    /// can describe them. Overriding it without claiming `change_capture` trips a debug
    /// assertion in `Transactional::dry_run`.
    ///
    /// Default: closes the unit with [`Self::rollback_unit`] (which tells the hooks, if any)
    /// and returns [`GraphError::Unsupported`].
    fn discard_unit(&mut self, mark: UnitMark) -> Result<ChangeSet, GraphError> {
        let error = GraphError::Unsupported {
            operation: DRY_RUN_OPERATION,
        };
        self.rollback_unit(mark, &RollbackReason::failed(&error));
        Err(error)
    }

    /// Sets a [`Mark`] named `name` at the current position (right after the last commit) and
    /// has the storage's journal record it, so a recovery can later stop exactly there
    /// (`persist::RecoveryTarget::Mark`, feature `persist`). Marks are journal records: a
    /// storage without a journal cannot set one. Rhai `g.mark(name)` calls it.
    ///
    /// [`TraversalGraph`] fails with [`GraphError::MarkUnavailable`] when no journal is
    /// registered, the name is empty or longer than 255 bytes, or a unit is open (a mark names
    /// a committed state), and with [`GraphError::MarkRejected`] when the journal refuses it (a
    /// name it has already recorded, a failed write).
    ///
    /// Default: [`GraphError::MarkUnavailable`] (no journal).
    fn mark(&mut self, name: &str) -> Result<Mark, GraphError> {
        let _ = name;
        Err(GraphError::mark_unavailable(
            "this storage keeps no journal",
        ))
    }

    /// What this storage guarantees: atomic units, change capture, isolation, durability,
    /// writability. Storage facts, not permissions; the conformance suite runs its
    /// capability-conditional items only for a storage that claims them.
    ///
    /// Default: [`StorageCapabilities::none`]. An implementation must keep its claims
    /// consistent ([`StorageCapabilities::is_consistent`]): `change_capture` requires `atomic`.
    fn capabilities(&self) -> StorageCapabilities {
        StorageCapabilities::none()
    }

    /// Registers a [commit hook](crate::graph::CommitHook); hooks run in registration order
    /// (see the [`changes`](crate::changes) module for the events and the veto rules).
    /// While at least one hook is registered the storage captures a
    /// [`ChangeSet`] per committed unit. Hooks cannot be removed.
    ///
    /// Only a storage with [`StorageCapabilities::change_capture`] (which requires `atomic`)
    /// supports hooks: a storage that cannot roll back cannot honour a veto. Default: rejects
    /// the hook with [`GraphError::Unsupported`].
    fn register_commit_hook(
        &mut self,
        hook: Box<dyn crate::graph::CommitHook>,
    ) -> Result<(), GraphError> {
        let _ = hook;
        Err(GraphError::Unsupported {
            operation: "commit hooks",
        })
    }

    /// The commit sequence number of the last committed unit that changed something (`0`: none
    /// yet). A storage that does not count commits (no `atomic` capability) stays at `0`.
    ///
    /// Together with a full export taken under the same read access (for example GraphML
    /// through a read lock of [`Graph`](crate::graph::Graph)) it is a snapshot with position: a
    /// server checkpoints the export and may truncate its write-ahead log up to this number.
    /// Default: `0`.
    fn last_commit_seq(&self) -> u64 {
        0
    }

    /// The start of the outermost open unit, in nanoseconds since the Unix epoch (UTC, the unit
    /// of [`DateTime::from_nanos`](crate::temporal::DateTime::from_nanos)): the time
    /// `defaultFunction` `now`/`today` store for the elements a query creates in it
    /// ([`CreateContext::for_storage`]) and the GQL clocks read, one value for the whole unit
    /// (PostgreSQL `now()`). `None` outside a unit. A value outside the years -9999 to 9999
    /// reads as the epoch.
    ///
    /// [`TraversalGraph`] takes it once when the outermost unit opens from the system clock
    /// (with the digits the platform gives: see the book page "Dates and Times", "Clock
    /// precision"), never earlier than the previous unit's start or the last commit time (so
    /// later units never get an earlier time). Default: `None` (the caller then reads the
    /// system clock).
    fn unit_started_at(&self) -> Option<i128> {
        None
    }

    /// Raises the vertex and edge auto-id sequences to at least `node_sequence` and
    /// `edge_sequence` (`max`, never lowered), so an auto id handed out later is above both:
    /// an id the source of a [`ChangeSet`] handed out (also to an element it dropped or rolled
    /// back since) is never handed out again here. Sequences are not part of the undo log: a
    /// raise inside a unit stays when the unit rolls back (ids are never reused, gaps are fine).
    ///
    /// [`Self::apply_change_set`] calls it inside its unit with the change set's own
    /// [`ChangeSet::node_sequence`] and [`ChangeSet::edge_sequence`], as
    /// [`TraversalGraph::replay`] does for an unpublished graph. Default: nothing (a storage
    /// without numeric auto-id sequences).
    fn raise_id_sequences(
        &mut self,
        node_sequence: u64,
        edge_sequence: u64,
    ) -> Result<(), GraphError> {
        let _ = (node_sequence, edge_sequence);
        Ok(())
    }

    /// Applies a [`ChangeSet`] (a journal, a replica's feed, a migration) as ONE live unit:
    /// every mutation in order, through the storage's own mutating methods, ids preserved (an
    /// explicit numeric id raises the auto-id sequence), the schema enforced. Only the
    /// after-values are used.
    ///
    /// On an atomic storage it is all-or-nothing, and its commit is an ordinary commit: it gets
    /// its own commit sequence number, is captured and runs the commit hooks (which may veto
    /// it). A missing element is [`GraphError::VertexNotFound`] / [`GraphError::EdgeNotFound`].
    /// On a non-atomic storage a failure keeps the mutations applied before it.
    ///
    /// Afterwards the auto-id sequences are at least the change set's own
    /// ([`ChangeSet::node_sequence`], [`ChangeSet::edge_sequence`]; [`Self::raise_id_sequences`]
    /// inside the unit), exactly as [`TraversalGraph::replay`] raises them: an id the source
    /// handed out to an element whose add and drop were compacted away, or in a unit it rolled
    /// back, is never handed out again by a later auto id here. A change set that changes
    /// nothing here but raises a sequence is still committed (a sequence-only commit: a
    /// [`ChangeSet`] without mutations reaches the hooks and a journal, so the raise survives a
    /// reopen).
    ///
    /// Under a memory budget (a `memory.limit` default of the storage's
    /// [`execution_policy`](Self::execution_policy)) the apply checks it every
    /// 4096 mutations and at its end; a change set that grows the memory in use over it is
    /// rolled back and fails with [`GraphError::ResourceLimitExceeded`] (operation
    /// `"change set apply"`). Without a budget nothing is checked.
    ///
    /// To rebuild a graph nobody sees yet (recovery from a snapshot plus the write-ahead log),
    /// use [`TraversalGraph::replay`] instead: it runs outside units and fires no hooks.
    fn apply_change_set(&mut self, changes: &ChangeSet) -> Result<(), GraphError> {
        apply_change_set_with(self, changes, &mut crate::graph::replay::clone_map)
    }
}

/// The body of [`GraphStorage::apply_change_set`]: one unit applying every mutation, added
/// elements inserted with `copy`'s map (see `graph::replay::apply_mutations`).
pub(crate) fn apply_change_set_with<S: GraphStorage + ?Sized>(
    storage: &mut S,
    changes: &ChangeSet,
    copy: &mut impl FnMut(&mut S, &PropertyMap) -> PropertyMap,
) -> Result<(), GraphError> {
    let mut budget =
        crate::traversal::memory_budget::UnitBudget::start(&*storage, None, CHANGE_SET_OPERATION);
    let mark = storage.begin_unit();
    let applied = close_on_unwind(
        storage,
        |this| {
            crate::graph::replay::apply_mutations(this, changes, &mut budget, copy)?;
            crate::traversal::memory_budget::check(&budget, &*this)?;
            this.raise_id_sequences(changes.node_sequence(), changes.edge_sequence())
        },
        |this| this.rollback_unit(mark, &RollbackReason::failed(&UNWOUND)),
    );
    match applied {
        Ok(()) => storage.commit_unit(mark),
        Err(err) => {
            storage.rollback_unit(mark, &RollbackReason::failed(&err));
            Err(err)
        }
    }
}

/// The operation [`GraphError::Unsupported`] names for [`GraphStorage::discard_unit`] on a storage
/// without change capture.
pub(crate) const DRY_RUN_OPERATION: &str = "dry runs (discarding a unit with its changes)";

/// The operation [`GraphError::Unsupported`] names for [`GraphStorage::mark`] on the read-only
/// `&G` view.
const READ_ONLY_MARK: &str = "marks on a read-only view";

/// The operation [`GraphError::ResourceLimitExceeded`] names for
/// [`GraphStorage::apply_change_set`].
pub(crate) const CHANGE_SET_OPERATION: &str = "change set apply";

/// The [`RollbackReason`] message of a unit that a panic unwound through ([`close_on_unwind`]).
pub(crate) const UNWOUND: &str =
    "a panic unwound through the unit (an engine bug); its changes were rolled back";

/// Runs `body` on `target`; when it unwinds, `on_unwind` closes the open unit (rolls it back)
/// before the panic continues to the caller.
///
/// A panic in the engine is a bug (CLAUDE.md rule 1: user input maps to errors), so this is
/// defence in depth: a host that catches the panic must not keep serving a half-applied unit,
/// and every unit (a traversal run, a terminal, an explicit transaction or dry run, a
/// whole-script unit, a change-set apply, an import, a schema change) closes this way. The
/// success path costs only the landing pad of `catch_unwind`: no allocation, no clock, no
/// atomic operation. Inner units close first, because the unwinding passes them first.
#[inline]
pub(crate) fn close_on_unwind<X: ?Sized, T>(
    target: &mut X,
    body: impl FnOnce(&mut X) -> T,
    on_unwind: impl FnOnce(&mut X),
) -> T {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| body(&mut *target))) {
        Ok(value) => value,
        Err(payload) => {
            on_unwind(target);
            std::panic::resume_unwind(payload)
        }
    }
}

/// What a unit changed: the data (vertices, edges, labels, properties), the schema and/or the
/// catalog of definitions ([`GraphStorage::unit_changes`], [`Execution::changes`](crate::Execution::changes)). A
/// schema change is a database change like any other (it advances the commit sequence number
/// and makes a run `mutated`); this tells a front end which views to refresh.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct UnitChanges {
    /// Vertices, edges, labels or properties changed.
    pub data: bool,
    /// The stored schema changed.
    pub schema: bool,
    /// The catalog of definitions (saved queries, ...) changed.
    pub definitions: bool,
}

impl UnitChanges {
    /// Nothing changed.
    pub const NONE: UnitChanges = UnitChanges {
        data: false,
        schema: false,
        definitions: false,
    };

    /// The changes `data` and `schema` (the catalog unchanged; see
    /// [`Self::with_definitions`]).
    pub const fn new(data: bool, schema: bool) -> Self {
        UnitChanges {
            data,
            schema,
            definitions: false,
        }
    }

    /// The same changes with `definitions` set to `changed`.
    pub const fn with_definitions(mut self, changed: bool) -> Self {
        self.definitions = changed;
        self
    }

    /// Whether anything changed.
    pub const fn any(&self) -> bool {
        self.data || self.schema || self.definitions
    }

    /// Both changes together.
    pub const fn union(self, other: UnitChanges) -> UnitChanges {
        UnitChanges {
            data: self.data || other.data,
            schema: self.schema || other.schema,
            definitions: self.definitions || other.definitions,
        }
    }
}

/// The mark of an open unit of atomicity, returned by [`GraphStorage::begin_unit`] and handed
/// back to [`GraphStorage::commit_unit`] / [`GraphStorage::rollback_unit`].
///
/// Opaque to the caller: what the raw value means is up to the storage that returned it
/// ([`TraversalGraph`] stores the position in its undo log). A storage implementing units builds
/// its marks with [`UnitMark::new`] and reads them back with [`UnitMark::raw`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UnitMark(u64);

impl UnitMark {
    /// A mark with a storage-defined raw value.
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// The storage-defined raw value of this mark.
    pub const fn raw(self) -> u64 {
        self.0
    }
}

impl GraphStorage for TraversalGraph {
    type VertexId = VertexHandle;
    type EdgeId = EdgeHandle;

    #[inline]
    fn vertex_count(&self) -> usize {
        self.graph.node_count()
    }

    #[inline]
    fn edge_count(&self) -> usize {
        self.graph.edge_count()
    }

    #[inline]
    fn vertices(&self) -> impl Iterator<Item = VertexHandle> + '_ {
        self.graph
            .node_references()
            .map(|(index, vertex)| VertexHandle::new(index, vertex.3))
    }

    #[inline]
    fn edges(&self) -> impl Iterator<Item = EdgeHandle> + '_ {
        self.graph
            .edge_references()
            .map(|edge| EdgeHandle::new(edge.id(), edge.weight().3.own))
    }

    #[inline]
    fn get_vertex_by_id(&self, id: &str) -> Option<VertexHandle> {
        self.index.get_node(&self.graph, id)
    }

    #[inline]
    fn get_edge_by_id(&self, id: &str) -> Option<EdgeHandle> {
        self.index.get_edge(&self.graph, id)
    }

    #[inline]
    fn out_edges(&self, node: VertexHandle) -> impl Iterator<Item = EdgeHandle> + '_ {
        // A stale handle iterates the edges of no slot (`NodeIndex::end()` is never occupied).
        let index = self.resolve_vertex(node).unwrap_or_else(NodeIndex::end);
        self.graph
            .edges_directed(index, Direction::Outgoing)
            .map(|e| EdgeHandle::new(e.id(), e.weight().3.own))
    }

    #[inline]
    fn in_edges(&self, node: VertexHandle) -> impl Iterator<Item = EdgeHandle> + '_ {
        let index = self.resolve_vertex(node).unwrap_or_else(NodeIndex::end);
        self.graph
            .edges_directed(index, Direction::Incoming)
            .map(|e| EdgeHandle::new(e.id(), e.weight().3.own))
    }

    #[inline]
    fn vertex_id(&self, node: VertexHandle) -> Option<&str> {
        self.vertex_weight(node).map(|v| v.0.as_str())
    }

    #[inline]
    fn edge_id(&self, edge: EdgeHandle) -> Option<&str> {
        self.edge_weight(edge).map(|e| e.0.as_str())
    }

    #[inline]
    fn vertex_label(&self, node: VertexHandle) -> Option<&str> {
        self.vertex_weight(node)
            .and_then(|v| self.labels.set(v.1).first())
            .map(|l| l.as_str())
    }

    #[inline]
    fn edge_label(&self, edge: EdgeHandle) -> Option<&str> {
        self.edge_weight(edge)
            .and_then(|e| self.labels.name_str(e.1))
    }

    #[inline]
    fn vertex_labels(&self, node: VertexHandle) -> &[CompactString] {
        self.vertex_weight(node)
            .map(|v| self.labels.set(v.1))
            .unwrap_or(&[])
    }

    #[inline]
    fn vertex_properties(&self, node: VertexHandle) -> Option<&PropertyMap> {
        self.vertex_weight(node).map(|v| &v.2)
    }

    #[inline]
    fn edge_properties(&self, edge: EdgeHandle) -> Option<&PropertyMap> {
        self.edge_weight(edge).and_then(|e| e.2.as_ref())
    }

    #[inline]
    fn edge_endpoints(&self, edge: EdgeHandle) -> Option<(VertexHandle, VertexHandle)> {
        // The endpoint generations were recorded when the edge was created: no vertex load.
        let generations = self.edge_weight(edge)?.3;
        let (source, target) = self.graph.edge_endpoints(edge.index())?;
        Some((
            VertexHandle::new(source, generations.source),
            VertexHandle::new(target, generations.target),
        ))
    }

    #[inline]
    fn contains_vertex(&self, node: VertexHandle) -> bool {
        self.resolve_vertex(node).is_some()
    }

    #[inline]
    fn contains_edge(&self, edge: EdgeHandle) -> bool {
        self.resolve_edge(edge).is_some()
    }

    #[inline]
    fn vertices_by_label<'a>(
        &'a self,
        label: &'a str,
    ) -> Box<dyn Iterator<Item = VertexHandle> + 'a> {
        match self.index.get_nodes_by_label(label) {
            Some(nodes) => Box::new(nodes),
            None => Box::new(std::iter::empty()),
        }
    }

    #[inline]
    fn edges_by_label<'a>(&'a self, label: &'a str) -> Box<dyn Iterator<Item = EdgeHandle> + 'a> {
        match self.index.get_edges_by_label(label) {
            Some(edges) => Box::new(edges),
            None => Box::new(std::iter::empty()),
        }
    }

    #[inline]
    fn statistics(&self) -> Option<&GraphStatistics> {
        Some(&self.statistics)
    }

    /// The default's unit, with every added element's map copied `Shaped` into this graph's
    /// shape table (no `IndexMap` clone, adopted with one comparison).
    fn apply_change_set(&mut self, changes: &ChangeSet) -> Result<(), GraphError> {
        apply_change_set_with(self, changes, &mut TraversalGraph::stored_copy)
    }

    #[inline]
    fn insert_vertex(
        &mut self,
        id: Option<CompactString>,
        labels: Vec<CompactString>,
        mut properties: PropertyMap,
    ) -> Result<VertexHandle, GraphError> {
        let labels = normalize_vertex_labels(labels)?;
        check_properties(&properties)?;

        // Coerce to, and validate against, the active schema.
        {
            let schema = self.schema.read();
            crate::graph::schema::SchemaValidator::validate_vertex(
                &schema,
                &labels,
                &mut properties,
            )?;
        }
        // Compression rules (D7): strings at rule paths are stored compressed.
        self.compression_prepare_vertex(&labels, &mut properties);

        let sequence = self.node_sequence;
        let id = match id {
            Some(id_str) => {
                if let Ok(parsed_id) = id_str.parse::<u64>() {
                    self.node_sequence = self.node_sequence.max(parsed_id);
                }
                id_str
            }
            None => self.next_value_node_sequence()?,
        };

        if self.index.contains_node(&self.graph, &id) {
            self.node_sequence = sequence;
            Err(GraphError::VertexIdUniqueConstraint { id: id.to_string() })
        } else {
            if self.undo.is_recording() {
                self.undo.record(UndoEntry::AddedVertex { id: id.clone() });
            }
            Ok(self.insert_vertex_raw(id, labels, properties))
        }
    }

    fn add_vertex_with(
        &mut self,
        id: Option<CompactString>,
        labels: Vec<CompactString>,
        mut properties: PropertyMap,
        context: &CreateContext,
    ) -> Result<VertexHandle, GraphError> {
        // Borrows the schema instead of copying it (the trait default reads `get_schema`).
        crate::graph::schema::defaults::fill_defaults(
            &self.schema.read(),
            GraphNodeType::Vertex,
            &labels,
            &mut properties,
            context,
        );
        GraphStorage::insert_vertex(self, id, labels, properties)
    }

    fn add_edge_with(
        &mut self,
        id: Option<CompactString>,
        label: Option<CompactString>,
        from: VertexHandle,
        to: VertexHandle,
        mut properties: PropertyMap,
        context: &CreateContext,
    ) -> Result<EdgeHandle, GraphError> {
        crate::graph::schema::defaults::fill_defaults(
            &self.schema.read(),
            GraphNodeType::Edge,
            label.as_slice(),
            &mut properties,
            context,
        );
        GraphStorage::insert_edge(self, id, label, from, to, properties)
    }

    #[inline]
    fn add_vertex_label(
        &mut self,
        node: VertexHandle,
        label: CompactString,
    ) -> Result<bool, GraphError> {
        check_label(&label)?;
        match self.vertex_weight(node) {
            Some(vertex) => {
                if self.labels.set(vertex.1).contains(&label) {
                    return Ok(false);
                }
                // Schema: the vertex must be valid under the new label set (a Closed schema
                // rejects an undeclared label; a newly declaring label's required fields must be
                // present and the existing properties must conform). Nothing is mutated on error.
                // Closed: every incident edge must still match a declared connection.
                let mut new_labels = self.labels.set(vertex.1).to_vec();
                new_labels.push(label.clone());
                crate::graph::schema::SchemaValidator::check_vertex(
                    &self.schema.read(),
                    &new_labels,
                    &vertex.2,
                )?;
                self.check_incident_topology(node.index(), &new_labels)?;
                let recording = self.undo.is_recording();
                self.pindex_vertex(node, false, None);
                // Borrows only the `graph` field, so the statistics can be updated alongside.
                if let Some(vertex) = self
                    .graph
                    .node_weight_mut(node.index())
                    .filter(|vertex| vertex.3 == node.generation())
                {
                    let new_set = self.labels.intern_set(&new_labels);
                    let old = self
                        .labels
                        .set(std::mem::replace(&mut vertex.1, new_set))
                        .to_vec();
                    self.statistics.vertex_relabeled(&old, &new_labels);
                    if recording {
                        let id = vertex.0.clone();
                        self.undo.record(UndoEntry::VertexLabels { id, old });
                    }
                }
                self.index.insert_node_label(label, node);
                // The rules of the new label set apply now (D7).
                self.compression_normalize_vertex(node);
                self.pindex_vertex(node, true, None);
                Ok(true)
            }
            None => Err(GraphError::vertex_removed()),
        }
    }

    #[inline]
    fn remove_vertex_label(&mut self, node: VertexHandle, label: &str) -> Result<bool, GraphError> {
        match self.vertex_weight(node) {
            Some(vertex) => {
                if !self
                    .labels
                    .set(vertex.1)
                    .iter()
                    .any(|l| l.as_str() == label)
                {
                    return Ok(false);
                }
                // Schema: Closed rejects removing the last label or leaving properties that no
                // remaining label declares; Open and Closed re-check the remaining labels.
                // Closed: every incident edge must still match a declared connection.
                let new_labels: Vec<CompactString> = self
                    .labels
                    .set(vertex.1)
                    .iter()
                    .filter(|l| l.as_str() != label)
                    .cloned()
                    .collect();
                crate::graph::schema::SchemaValidator::check_vertex(
                    &self.schema.read(),
                    &new_labels,
                    &vertex.2,
                )?;
                self.check_incident_topology(node.index(), &new_labels)?;
                let recording = self.undo.is_recording();
                self.pindex_vertex(node, false, None);
                // Borrows only the `graph` field, so the statistics can be updated alongside.
                if let Some(vertex) = self
                    .graph
                    .node_weight_mut(node.index())
                    .filter(|vertex| vertex.3 == node.generation())
                {
                    let new_set = self.labels.intern_set(&new_labels);
                    let old = self
                        .labels
                        .set(std::mem::replace(&mut vertex.1, new_set))
                        .to_vec();
                    self.statistics.vertex_relabeled(&old, &new_labels);
                    if recording {
                        let id = vertex.0.clone();
                        self.undo.record(UndoEntry::VertexLabels { id, old });
                    }
                }
                self.index.remove_node_label(label, node);
                self.compression_normalize_vertex(node);
                self.pindex_vertex(node, true, None);
                Ok(true)
            }
            None => Err(GraphError::vertex_removed()),
        }
    }

    fn set_edge_label(
        &mut self,
        edge: EdgeHandle,
        label: CompactString,
    ) -> Result<bool, GraphError> {
        let Some(index) = self.resolve_edge(edge) else {
            return Err(GraphError::edge_removed());
        };
        let (Some(weight), Some((from, to))) = (
            self.graph.edge_weight(index),
            self.graph.edge_endpoints(index),
        ) else {
            return Err(GraphError::edge_removed());
        };
        if self.labels.name_str(weight.1) == Some(label.as_str()) {
            return Ok(false);
        }
        // Schema: the edge as stored must be valid under the new label (Closed: declared label
        // and topology; Open/Closed: required keys and declared types). Nothing is mutated on
        // error, nothing is coerced.
        {
            let schema = self.schema.read();
            if schema.mode != crate::graph::schema::SchemaMode::None {
                let empty = PropertyMap::default();
                let (Some(source), Some(target)) =
                    (self.graph.node_weight(from), self.graph.node_weight(to))
                else {
                    return Err(GraphError::vertex_removed());
                };
                crate::graph::schema::SchemaValidator::check_edge(
                    &schema,
                    Some(label.as_str()),
                    self.labels.set(source.1),
                    self.labels.set(target.1),
                    weight.2.as_ref().unwrap_or(&empty),
                )?;
            }
        }
        let recording = self.undo.is_recording();
        self.pindex_edge(edge, false, None);
        let Some(weight) = self.graph.edge_weight_mut(index) else {
            return Err(GraphError::edge_removed());
        };
        let new_id = self.labels.intern_name(Some(label.as_str()));
        let old = self
            .labels
            .name(std::mem::replace(&mut weight.1, new_id))
            .cloned();
        self.statistics
            .edge_relabeled(old.as_deref(), Some(label.as_str()));
        if recording {
            let id = weight.0.clone();
            self.undo.record(UndoEntry::EdgeLabel {
                id,
                old: old.clone(),
            });
        }
        if let Some(old) = old {
            self.index.remove_edge_label(&old, edge);
        }
        self.index.insert_edge_label(label, edge);
        // The compression rules of the new label apply now (D7).
        self.compression_normalize_edge(edge);
        self.pindex_edge(edge, true, None);
        Ok(true)
    }

    #[inline]
    fn insert_edge(
        &mut self,
        id: Option<CompactString>,
        label: Option<CompactString>,
        from: VertexHandle,
        to: VertexHandle,
        mut properties: PropertyMap,
    ) -> Result<EdgeHandle, GraphError> {
        // Both endpoints must be live vertices: the topology refuses a removed index, and a stale
        // handle must be an error, never a panic, and never reach the vertex that reuses its slot.
        let (Some(source), Some(target)) = (self.vertex_weight(from), self.vertex_weight(to))
        else {
            return Err(GraphError::vertex_removed());
        };
        check_properties(&properties)?;
        // Topology: ANY label of an endpoint can satisfy a declared connection.
        let source_labels: &[CompactString] = self.labels.set(source.1);
        let target_labels: &[CompactString] = self.labels.set(target.1);

        // Coerce to, and validate against, the active schema.
        {
            let schema = self.schema.read();
            crate::graph::schema::SchemaValidator::validate_edge(
                &schema,
                label.as_deref(),
                source_labels,
                target_labels,
                &mut properties,
            )?;
        }
        self.compression_prepare_edge(label.as_deref(), &mut properties);

        let edge_id = match id {
            Some(edge_id_str) => {
                if self.index.get_edge(&self.graph, &edge_id_str).is_some() {
                    return Err(GraphError::EdgeIdUniqueConstraint {
                        id: edge_id_str.to_string(),
                    });
                }
                if let Ok(parsed_id) = edge_id_str.parse::<u64>() {
                    self.edge_sequence = self.edge_sequence.max(parsed_id);
                }
                edge_id_str
            }
            None => self.next_value_edge_sequence()?,
        };
        if self.undo.is_recording() {
            self.undo.record(UndoEntry::AddedEdge {
                id: edge_id.clone(),
            });
        }
        Ok(self.insert_edge_raw(edge_id, label, Some(properties), from, to))
    }

    #[inline]
    fn set_vertex_property(
        &mut self,
        node: VertexHandle,
        key: CompactString,
        value: ElementProperty,
    ) -> Result<(), GraphError> {
        check_property_value(&key, &value)?;
        let Some(index) = self.resolve_vertex(node) else {
            return Err(GraphError::vertex_removed());
        };
        match self.graph.node_weight_mut(index) {
            Some(node_ref) => {
                let value = {
                    let schema = self.schema.read();
                    // All labels, in label order: coercion follows the first label that declares
                    // the key, validation checks every label that declares it.
                    crate::graph::schema::SchemaValidator::validate_element_property(
                        &schema,
                        "vertex",
                        self.labels.set(node_ref.1),
                        &key,
                        value,
                    )?
                };
                let mut value = value;
                self.compression.prepare_vertex_value(
                    self.labels.set(node_ref.1),
                    &key,
                    &mut value,
                );
                let undo_key = self.undo.is_recording().then(|| key.clone());
                // The property indexes on `key`: the old entry out, the new one in.
                let index_key = (!self.property_indexes.is_empty()).then(|| key.clone());
                if index_key.is_some() {
                    self.property_indexes.vertex_entries(
                        false,
                        node,
                        self.labels.set(node_ref.1),
                        &node_ref.2,
                        Some(&key),
                    );
                }
                let (position, old) = self.shapes.insert_full(&mut node_ref.2, key, value);
                if let Some(index_key) = &index_key {
                    self.property_indexes.vertex_entries(
                        true,
                        node,
                        self.labels.set(node_ref.1),
                        &node_ref.2,
                        Some(index_key),
                    );
                }
                if let Some(key) = undo_key {
                    self.undo.record(UndoEntry::VertexProperty {
                        id: node_ref.0.clone(),
                        key,
                        old: old.map(|value| (position, value)),
                    });
                }
                Ok(())
            }
            None => Err(GraphError::vertex_removed()),
        }
    }

    #[inline]
    fn set_edge_property(
        &mut self,
        edge: EdgeHandle,
        key: CompactString,
        value: ElementProperty,
    ) -> Result<(), GraphError> {
        check_property_value(&key, &value)?;
        let Some(index) = self.resolve_edge(edge) else {
            return Err(GraphError::edge_removed());
        };
        match self.graph.edge_weight_mut(index) {
            Some(edge_ref) => {
                let value = {
                    let schema = self.schema.read();
                    crate::graph::schema::SchemaValidator::validate_element_property(
                        &schema,
                        "edge",
                        self.labels.name_slice(edge_ref.1),
                        &key,
                        value,
                    )?
                };
                let mut value = value;
                self.compression.prepare_edge_value(
                    self.labels.name_str(edge_ref.1),
                    &key,
                    &mut value,
                );
                let undo_key = self.undo.is_recording().then(|| key.clone());
                let map_was_none = edge_ref.2.is_none();
                // The property indexes on `key`: the old entry out, the new one in.
                let index_key = (!self.property_indexes.is_empty()).then(|| key.clone());
                if index_key.is_some() {
                    self.property_indexes.edge_entries(
                        false,
                        edge,
                        self.labels.name_str(edge_ref.1),
                        edge_ref.2.as_ref(),
                        Some(&key),
                    );
                }
                let (position, old) = self.shapes.insert_full(
                    edge_ref.2.get_or_insert_with(PropertyMap::default),
                    key,
                    value,
                );
                if let Some(index_key) = &index_key {
                    self.property_indexes.edge_entries(
                        true,
                        edge,
                        self.labels.name_str(edge_ref.1),
                        edge_ref.2.as_ref(),
                        Some(index_key),
                    );
                }
                if let Some(key) = undo_key {
                    self.undo.record(UndoEntry::EdgeProperty {
                        id: edge_ref.0.clone(),
                        key,
                        old: old.map(|value| (position, value)),
                        map_was_none,
                    });
                }
                Ok(())
            }
            None => Err(GraphError::edge_removed()),
        }
    }

    /// In-place path write when the schema is off (nothing to coerce or validate); otherwise the
    /// default copy-and-store route, so the schema layer sees the whole resulting property.
    fn set_vertex_property_path(
        &mut self,
        node: VertexHandle,
        path: &crate::value::JsonPath,
        value: ElementProperty,
    ) -> Result<(), GraphError> {
        let name = path_property_name(path)?;
        check_property_value(name, &value)?;
        if matches!(
            self.schema.read().mode,
            crate::graph::schema::SchemaMode::None
        ) {
            self.record_vertex_property(node, name);
            self.pindex_vertex(node, false, Some(name));
            let written = self
                .with_vertex_map(node, |shapes, map| {
                    let written = write_path_in(map, path, value);
                    shapes.adopt(map);
                    written
                })
                .unwrap_or_else(|| Err(GraphError::vertex_removed()));
            self.compression_normalize_vertex_key(node, name);
            self.pindex_vertex(node, true, Some(name));
            return written;
        }
        let (key, resulting) = {
            let Some(vertex) = self.vertex_weight(node) else {
                return Err(GraphError::vertex_removed());
            };
            let schema = self.schema.read();
            // Coerce the leaf at the type the schema declares at that nested location, write it,
            // then validate the complete resulting top-level property (no re-coercion of siblings).
            let value = crate::graph::schema::SchemaValidator::coerce_path_leaf(
                &schema,
                "vertex",
                self.labels.set(vertex.1),
                path,
                value,
            )?;
            let (key, resulting) = write_path(vertex.2.get(name.as_str()), path, value)?;
            crate::graph::schema::SchemaValidator::check_element_property(
                &schema,
                "vertex",
                self.labels.set(vertex.1),
                &key,
                &resulting,
            )?;
            (key, resulting)
        };
        self.record_vertex_property(node, &key);
        self.pindex_vertex(node, false, Some(&key));
        self.with_vertex_map(node, |shapes, map| {
            shapes.insert_full(map, key.clone(), resulting)
        });
        self.compression_normalize_vertex_key(node, &key);
        self.pindex_vertex(node, true, Some(&key));
        Ok(())
    }

    /// Edge counterpart of [`Self::set_vertex_property_path`].
    fn set_edge_property_path(
        &mut self,
        edge: EdgeHandle,
        path: &crate::value::JsonPath,
        value: ElementProperty,
    ) -> Result<(), GraphError> {
        let name = path_property_name(path)?;
        check_property_value(name, &value)?;
        if matches!(
            self.schema.read().mode,
            crate::graph::schema::SchemaMode::None
        ) {
            self.record_edge_property(edge, name);
            self.pindex_edge(edge, false, Some(name));
            let written = self
                .with_edge_map(edge, |shapes, map| {
                    let properties = map.get_or_insert_with(PropertyMap::default);
                    let written = write_path_in(properties, path, value);
                    shapes.adopt(properties);
                    written
                })
                .unwrap_or_else(|| Err(GraphError::edge_removed()));
            self.compression_normalize_edge_key(edge, name);
            self.pindex_edge(edge, true, Some(name));
            return written;
        }
        let (key, resulting) = {
            let Some(edge_ref) = self.edge_weight(edge) else {
                return Err(GraphError::edge_removed());
            };
            let schema = self.schema.read();
            let value = crate::graph::schema::SchemaValidator::coerce_path_leaf(
                &schema,
                "edge",
                self.labels.name_slice(edge_ref.1),
                path,
                value,
            )?;
            let existing = edge_ref
                .2
                .as_ref()
                .and_then(|props| props.get(name.as_str()));
            let (key, resulting) = write_path(existing, path, value)?;
            crate::graph::schema::SchemaValidator::check_element_property(
                &schema,
                "edge",
                self.labels.name_slice(edge_ref.1),
                &key,
                &resulting,
            )?;
            (key, resulting)
        };
        self.record_edge_property(edge, &key);
        self.pindex_edge(edge, false, Some(&key));
        self.with_edge_map(edge, |shapes, map| {
            shapes.insert_full(
                map.get_or_insert_with(PropertyMap::default),
                key.clone(),
                resulting,
            )
        });
        self.compression_normalize_edge_key(edge, &key);
        self.pindex_edge(edge, true, Some(&key));
        Ok(())
    }

    /// In-place path removal when the schema is off (nothing to validate); otherwise the default
    /// copy-and-store route, so the schema layer sees the whole resulting property.
    fn remove_vertex_property_path(
        &mut self,
        node: VertexHandle,
        path: &crate::value::JsonPath,
    ) -> Result<(), GraphError> {
        if let Some(name) = single_segment_name(path) {
            return self.drop_vertex_property(name, node);
        }
        let schema_off = matches!(
            self.schema.read().mode,
            crate::graph::schema::SchemaMode::None
        );
        if schema_off {
            // Recorded only when the path exists: removing a missing path changes nothing.
            if let Some(crate::value::json_path::JsonPathSegment::Key(name)) =
                path.segments().first()
                && self.vertex_weight(node).is_some_and(|vertex| {
                    crate::value::json_path::descend_properties(&vertex.2, path.segments())
                        .is_some()
                })
            {
                self.record_vertex_property(node, name.as_str());
            }
            let top = match path.segments().first() {
                Some(crate::value::json_path::JsonPathSegment::Key(name)) => Some(name.clone()),
                _ => None,
            };
            if let Some(top) = &top {
                self.pindex_vertex(node, false, Some(top));
            }
            let removed = self
                .with_vertex_map(node, |shapes, map| {
                    crate::graph::write_checks::remove_path_in(map, path);
                    shapes.adopt(map);
                })
                .ok_or_else(GraphError::vertex_removed);
            if let Some(top) = &top {
                self.pindex_vertex(node, true, Some(top));
            }
            return removed;
        }
        let Some(vertex) = self.vertex_weight(node) else {
            return Err(GraphError::vertex_removed());
        };
        let existing = match path.segments().first() {
            Some(crate::value::json_path::JsonPathSegment::Key(name)) => {
                vertex.2.get(name.as_str())
            }
            _ => None,
        };
        match remove_path(existing, path) {
            Some((key, resulting)) => {
                // The schema must still accept the property without that value (required nested
                // keys, `min_items`, ...); nothing is coerced.
                crate::graph::schema::SchemaValidator::check_element_property(
                    &self.schema.read(),
                    "vertex",
                    self.labels.set(vertex.1),
                    &key,
                    &resulting,
                )?;
                self.record_vertex_property(node, &key);
                self.pindex_vertex(node, false, Some(&key));
                self.with_vertex_map(node, |shapes, map| {
                    shapes.insert_full(map, key.clone(), resulting)
                });
                self.pindex_vertex(node, true, Some(&key));
                Ok(())
            }
            None => Ok(()),
        }
    }

    /// Edge counterpart of [`Self::remove_vertex_property_path`].
    fn remove_edge_property_path(
        &mut self,
        edge: EdgeHandle,
        path: &crate::value::JsonPath,
    ) -> Result<(), GraphError> {
        if let Some(name) = single_segment_name(path) {
            return self.drop_edge_property(name, edge);
        }
        let schema_off = matches!(
            self.schema.read().mode,
            crate::graph::schema::SchemaMode::None
        );
        if schema_off {
            // Recorded only when the path exists: removing a missing path changes nothing.
            if let Some(crate::value::json_path::JsonPathSegment::Key(name)) =
                path.segments().first()
                && self.edge_weight(edge).is_some_and(|edge_ref| {
                    edge_ref.2.as_ref().is_some_and(|properties| {
                        crate::value::json_path::descend_properties(properties, path.segments())
                            .is_some()
                    })
                })
            {
                self.record_edge_property(edge, name.as_str());
            }
            let top = match path.segments().first() {
                Some(crate::value::json_path::JsonPathSegment::Key(name)) => Some(name.clone()),
                _ => None,
            };
            if let Some(top) = &top {
                self.pindex_edge(edge, false, Some(top));
            }
            let removed = self
                .with_edge_map(edge, |shapes, map| {
                    if let Some(properties) = map.as_mut() {
                        crate::graph::write_checks::remove_path_in(properties, path);
                        shapes.adopt(properties);
                    }
                })
                .ok_or_else(GraphError::edge_removed);
            if let Some(top) = &top {
                self.pindex_edge(edge, true, Some(top));
            }
            return removed;
        }
        let Some(edge_ref) = self.edge_weight(edge) else {
            return Err(GraphError::edge_removed());
        };
        let existing = match (path.segments().first(), edge_ref.2.as_ref()) {
            (Some(crate::value::json_path::JsonPathSegment::Key(name)), Some(props)) => {
                props.get(name.as_str())
            }
            _ => None,
        };
        match remove_path(existing, path) {
            Some((key, resulting)) => {
                crate::graph::schema::SchemaValidator::check_element_property(
                    &self.schema.read(),
                    "edge",
                    self.labels.name_slice(edge_ref.1),
                    &key,
                    &resulting,
                )?;
                self.record_edge_property(edge, &key);
                self.pindex_edge(edge, false, Some(&key));
                self.with_edge_map(edge, |shapes, map| {
                    shapes.insert_full(
                        map.get_or_insert_with(PropertyMap::default),
                        key.clone(),
                        resulting,
                    )
                });
                self.pindex_edge(edge, true, Some(&key));
                Ok(())
            }
            None => Ok(()),
        }
    }

    #[inline]
    fn drop_vertex(&mut self, node: VertexHandle) -> Result<(), GraphError> {
        // A stale handle is a no-op (idempotent drop); it never reaches the slot's new occupant.
        let capture = self.undo.is_recording();
        if let Some(entry) = self.remove_vertex_raw(node, capture) {
            self.undo.record(entry);
        }
        Ok(())
    }

    #[inline]
    fn drop_vertex_property(&mut self, prop: &str, node: VertexHandle) -> Result<(), GraphError> {
        // Schema: a required (non-nullable) declared property cannot be removed (decision B);
        // an absent one is a no-op. Checked before anything is touched.
        if let Some(vertex) = self.vertex_weight(node)
            && vertex.2.contains_key(prop)
        {
            crate::graph::schema::SchemaValidator::check_removal(
                &self.schema.read(),
                "vertex",
                self.labels.set(vertex.1),
                prop,
            )?;
        }
        let recording = self.undo.is_recording();
        self.pindex_vertex(node, false, Some(prop));
        let entry = match self
            .graph
            .node_weight_mut(node.index())
            .filter(|vertex| vertex.3 == node.generation())
        {
            Some(vertex) => self
                .shapes
                .shift_remove_full(&mut vertex.2, prop)
                .filter(|_| recording)
                .map(|(position, key, value)| UndoEntry::VertexProperty {
                    id: vertex.0.clone(),
                    key,
                    old: Some((position, value)),
                }),
            None => return Err(GraphError::vertex_removed()),
        };
        if let Some(entry) = entry {
            self.undo.record(entry);
        }
        Ok(())
    }

    #[inline]
    fn drop_edge(&mut self, edge: EdgeHandle) -> Result<(), GraphError> {
        // A stale handle is a no-op (idempotent drop); it never reaches the slot's new occupant.
        let capture = self.undo.is_recording();
        if let Some(image) = self.remove_edge_raw(edge, capture) {
            self.undo.record(UndoEntry::DroppedEdge(Box::new(image)));
        }
        Ok(())
    }

    #[inline]
    fn drop_edge_property(&mut self, prop: &str, edge: EdgeHandle) -> Result<(), GraphError> {
        // Schema: see `drop_vertex_property`.
        if let Some(edge_weight) = self.edge_weight(edge)
            && edge_weight.2.as_ref().is_some_and(|p| p.contains_key(prop))
        {
            crate::graph::schema::SchemaValidator::check_removal(
                &self.schema.read(),
                "edge",
                self.labels.name_slice(edge_weight.1),
                prop,
            )?;
        }
        let recording = self.undo.is_recording();
        self.pindex_edge(edge, false, Some(prop));
        let entry = match self
            .graph
            .edge_weight_mut(edge.index())
            .filter(|weight| weight.3.own == edge.generation())
        {
            Some(edge_weight) => edge_weight
                .2
                .as_mut()
                .and_then(|props| self.shapes.shift_remove_full(props, prop))
                .filter(|_| recording)
                .map(|(position, key, value)| UndoEntry::EdgeProperty {
                    id: edge_weight.0.clone(),
                    key,
                    old: Some((position, value)),
                    map_was_none: false,
                }),
            None => return Err(GraphError::edge_removed()),
        };
        if let Some(entry) = entry {
            self.undo.record(entry);
        }
        Ok(())
    }

    #[inline]
    fn set_schema(&mut self, schema: GraphSchema) -> Result<(), GraphError> {
        self.in_own_unit(|this| this.set_schema_recorded(schema))
    }

    #[inline]
    fn definitions(&self) -> impl Iterator<Item = &Definition> + '_ {
        self.definitions.iter()
    }

    #[inline]
    fn definition(&self, kind: DefinitionKind, name: &str) -> Option<&Definition> {
        self.definitions.get(kind, name)
    }

    fn set_definition(&mut self, definition: Definition) -> Result<(), GraphError> {
        self.in_own_unit(|this| this.set_definition_recorded(definition))
    }

    fn remove_definition(&mut self, kind: DefinitionKind, name: &str) -> Result<bool, GraphError> {
        self.in_own_unit(|this| Ok(this.remove_definition_recorded(kind, name)))
    }

    fn recompress(&mut self, name: &str) -> Result<bool, GraphError> {
        Ok(self.recompress_rule(name))
    }

    fn compression_stats(&self, name: &str) -> Option<crate::graph::CompressionStats> {
        TraversalGraph::compression_stats(self, name)
    }

    fn estimate_compression(
        &self,
        request: &crate::graph::CompressionEstimateRequest,
    ) -> Result<crate::graph::CompressionEstimate, GraphError> {
        Ok(TraversalGraph::estimate_compression(self, request))
    }

    fn property_indexes(&self) -> Vec<crate::graph::PropertyIndexInfo> {
        self.property_index_infos()
    }

    fn vertices_by_property_index<'a>(
        &'a self,
        label: &'a str,
        key: &'a str,
        range: &crate::graph::PropertyIndexRange,
    ) -> Option<Box<dyn Iterator<Item = VertexHandle> + 'a>> {
        self.property_index_vertices(label, key, None, range)
    }

    fn vertices_by_transformed_property_index<'a>(
        &'a self,
        label: &'a str,
        key: &'a str,
        transform: &crate::graph::PropertyIndexTransform,
        range: &crate::graph::PropertyIndexRange,
    ) -> Option<Box<dyn Iterator<Item = VertexHandle> + 'a>> {
        self.property_index_vertices(label, key, Some(transform), range)
    }

    fn edges_by_transformed_property_index<'a>(
        &'a self,
        label: &'a str,
        key: &'a str,
        transform: &crate::graph::PropertyIndexTransform,
        range: &crate::graph::PropertyIndexRange,
    ) -> Option<Box<dyn Iterator<Item = EdgeHandle> + 'a>> {
        self.property_index_edges(label, key, Some(transform), range)
    }

    fn edges_by_property_index<'a>(
        &'a self,
        label: &'a str,
        key: &'a str,
        range: &crate::graph::PropertyIndexRange,
    ) -> Option<Box<dyn Iterator<Item = EdgeHandle> + 'a>> {
        self.property_index_edges(label, key, None, range)
    }

    #[inline]
    fn get_schema(&self) -> Option<GraphSchema> {
        let active = self.schema.read();
        (!active.is_empty()).then(|| active.clone())
    }

    #[inline]
    fn declared_property_schema(
        &self,
        kind: GraphNodeType,
        labels: &[CompactString],
        key: &str,
    ) -> Option<PropertySchema> {
        self.schema
            .read()
            .declared_property(kind, labels, key)
            .cloned()
    }

    #[inline]
    fn shrink_to_fit(&mut self) {
        self.shrink_to_fit();
    }

    #[inline]
    fn memory_usage(&self) -> Option<crate::graph::MemoryStats> {
        Some(crate::graph::memory::compute(self))
    }

    fn memory_structure(&self) -> Option<crate::graph::StructureMemory> {
        Some(crate::graph::memory::structure(self))
    }

    #[inline]
    fn memory_footprint_bytes(&self) -> Option<u64> {
        Some(crate::graph::memory::footprint_bytes(self))
    }

    #[inline]
    fn execution_policy(&self) -> crate::graph::ExecutionPolicy {
        self.execution_policy.clone()
    }

    /// Opens a unit of the undo log (see the `graph::undo` module): from now on every mutation
    /// records its inverse. The mark is the log position.
    #[inline]
    fn begin_unit(&mut self) -> UnitMark {
        self.begin_undo_unit()
    }

    #[inline]
    fn commit_unit(&mut self, mark: UnitMark) -> Result<(), GraphError> {
        self.commit_undo_unit(mark)
    }

    #[inline]
    fn rollback_unit(&mut self, mark: UnitMark, reason: &RollbackReason) {
        self.rollback_undo_unit(mark, reason);
    }

    #[inline]
    fn unit_changes(&self, mark: UnitMark) -> UnitChanges {
        self.undo_unit_changes(mark)
    }

    /// The undo log's metadata: while only the outermost unit is open the given metadata takes
    /// precedence over what nested units left; inside a nested unit it adds what is missing.
    fn set_unit_metadata(&mut self, metadata: CommitMetadata) {
        self.set_undo_metadata(metadata);
    }

    /// Derives the unit's mutations from the undo log, then undoes them and closes the unit
    /// without hooks.
    fn discard_unit(&mut self, mark: UnitMark) -> Result<ChangeSet, GraphError> {
        Ok(self.discard_undo_unit_with_changes(mark))
    }

    /// The journal slot's mark (see [`GraphStorage::mark`]).
    fn mark(&mut self, name: &str) -> Result<Mark, GraphError> {
        self.journal_mark(name)
    }

    /// Atomic (undo log), change capture (commit hooks), writable; no isolation of its own
    /// (readers and the writer are serialized by `&mut` or the lock of [`Graph`](crate::graph::Graph)), one writer,
    /// in memory only.
    #[inline]
    fn capabilities(&self) -> StorageCapabilities {
        TRAVERSAL_GRAPH_CAPABILITIES
    }

    fn register_commit_hook(
        &mut self,
        hook: Box<dyn crate::graph::CommitHook>,
    ) -> Result<(), GraphError> {
        self.undo.hooks.register(hook);
        Ok(())
    }

    #[inline]
    fn last_commit_seq(&self) -> u64 {
        self.commit_seq()
    }

    fn unit_started_at(&self) -> Option<i128> {
        self.undo_unit_started_at()
    }

    /// `max` of each sequence and the given value; nothing is recorded (sequences are never
    /// rolled back). A real raise inside a unit makes its outermost commit a commit even when
    /// nothing else changed (a sequence-only commit), so hooks and the journal keep the raise.
    #[inline]
    fn raise_id_sequences(
        &mut self,
        node_sequence: u64,
        edge_sequence: u64,
    ) -> Result<(), GraphError> {
        if node_sequence > self.node_sequence || edge_sequence > self.edge_sequence {
            self.node_sequence = self.node_sequence.max(node_sequence);
            self.edge_sequence = self.edge_sequence.max(edge_sequence);
            if self.undo.is_recording() {
                self.undo.mark_sequences_raised();
            }
        }
        Ok(())
    }
}

impl TraversalGraph {
    /// Runs `change` (a schema or catalog change) in the open unit, or, outside any unit
    /// (`graph.write().set_schema(..)` on a live graph), as a unit of its own: recorded, part
    /// of the commit sequence, captured and seen by the commit hooks. Loading a graph nobody
    /// sees yet (`replay`, the GraphML constructors) opens no unit.
    fn in_own_unit<T>(
        &mut self,
        change: impl FnOnce(&mut Self) -> Result<T, GraphError>,
    ) -> Result<T, GraphError> {
        if self.undo.is_recording() || self.undo.loading {
            return change(self);
        }
        let mark = self.begin_undo_unit();
        let changed = close_on_unwind(self, change, |this| {
            this.rollback_undo_unit(mark, &RollbackReason::failed(&UNWOUND))
        });
        match changed {
            Ok(value) => {
                self.commit_undo_unit(mark)?;
                Ok(value)
            }
            Err(err) => {
                self.rollback_undo_unit(mark, &RollbackReason::failed(&err));
                Err(err)
            }
        }
    }
}

/// What [`TraversalGraph`] (and a wrapper that can mutate it) reports.
const TRAVERSAL_GRAPH_CAPABILITIES: StorageCapabilities = StorageCapabilities::none()
    .with_atomic(true)
    .with_change_capture(true)
    .with_isolation(IsolationLevel::None)
    .with_writable(true)
    .with_property_indexes(true);

/// The error every write through a read-only `&G` view returns.
#[inline]
fn read_only_view_error() -> GraphError {
    GraphError::InvalidElementState {
        reason: "Cannot mutate immutable graph reference".to_string(),
    }
}

/// A shared reference to any storage is its read-only view: every read is forwarded to `G`,
/// every write (data, labels, properties, paths, schema, catalog, `recompress`) fails, it claims
/// no capability ([`StorageCapabilities::none`]) and it refuses commit hooks (the defaults).
///
/// The read terminals run the engine over `&S`, so every storage gets this view without writing
/// it itself.
impl<G: GraphStorage> GraphStorage for &G {
    type VertexId = G::VertexId;
    type EdgeId = G::EdgeId;

    #[inline]
    fn vertex_count(&self) -> usize {
        (**self).vertex_count()
    }
    #[inline]
    fn edge_count(&self) -> usize {
        (**self).edge_count()
    }
    #[inline]
    fn vertices(&self) -> impl Iterator<Item = Self::VertexId> + '_ {
        (**self).vertices()
    }
    #[inline]
    fn edges(&self) -> impl Iterator<Item = Self::EdgeId> + '_ {
        (**self).edges()
    }
    #[inline]
    fn get_vertex_by_id(&self, id: &str) -> Option<Self::VertexId> {
        (**self).get_vertex_by_id(id)
    }
    #[inline]
    fn get_edge_by_id(&self, id: &str) -> Option<Self::EdgeId> {
        (**self).get_edge_by_id(id)
    }
    #[inline]
    fn out_edges(&self, node: Self::VertexId) -> impl Iterator<Item = Self::EdgeId> + '_ {
        (**self).out_edges(node)
    }
    #[inline]
    fn in_edges(&self, node: Self::VertexId) -> impl Iterator<Item = Self::EdgeId> + '_ {
        (**self).in_edges(node)
    }
    #[inline]
    fn vertices_by_label<'b>(
        &'b self,
        label: &'b str,
    ) -> Box<dyn Iterator<Item = Self::VertexId> + 'b> {
        (**self).vertices_by_label(label)
    }
    #[inline]
    fn edges_by_label<'b>(&'b self, label: &'b str) -> Box<dyn Iterator<Item = Self::EdgeId> + 'b> {
        (**self).edges_by_label(label)
    }
    #[inline]
    fn vertex_id(&self, node: Self::VertexId) -> Option<&str> {
        (**self).vertex_id(node)
    }
    #[inline]
    fn edge_id(&self, edge: Self::EdgeId) -> Option<&str> {
        (**self).edge_id(edge)
    }
    #[inline]
    fn vertex_label(&self, node: Self::VertexId) -> Option<&str> {
        (**self).vertex_label(node)
    }
    #[inline]
    fn edge_label(&self, edge: Self::EdgeId) -> Option<&str> {
        (**self).edge_label(edge)
    }
    #[inline]
    fn vertex_labels(&self, node: Self::VertexId) -> &[CompactString] {
        (**self).vertex_labels(node)
    }
    #[inline]
    fn vertex_has_label(&self, node: Self::VertexId, label: &str) -> bool {
        (**self).vertex_has_label(node, label)
    }
    #[inline]
    fn vertex_properties(&self, node: Self::VertexId) -> Option<&PropertyMap> {
        (**self).vertex_properties(node)
    }
    #[inline]
    fn edge_properties(&self, edge: Self::EdgeId) -> Option<&PropertyMap> {
        (**self).edge_properties(edge)
    }
    #[inline]
    fn edge_endpoints(&self, edge: Self::EdgeId) -> Option<(Self::VertexId, Self::VertexId)> {
        (**self).edge_endpoints(edge)
    }
    #[inline]
    fn contains_vertex(&self, node: Self::VertexId) -> bool {
        (**self).contains_vertex(node)
    }
    #[inline]
    fn contains_edge(&self, edge: Self::EdgeId) -> bool {
        (**self).contains_edge(edge)
    }
    #[inline]
    fn statistics(&self) -> Option<&GraphStatistics> {
        (**self).statistics()
    }

    #[inline]
    fn insert_vertex(
        &mut self,
        _id: Option<CompactString>,
        _labels: Vec<CompactString>,
        _properties: PropertyMap,
    ) -> Result<Self::VertexId, GraphError> {
        Err(read_only_view_error())
    }

    #[inline]
    fn add_vertex_label(
        &mut self,
        _node: Self::VertexId,
        _label: CompactString,
    ) -> Result<bool, GraphError> {
        Err(read_only_view_error())
    }

    #[inline]
    fn remove_vertex_label(
        &mut self,
        _node: Self::VertexId,
        _label: &str,
    ) -> Result<bool, GraphError> {
        Err(read_only_view_error())
    }

    #[inline]
    fn set_edge_label(
        &mut self,
        _edge: Self::EdgeId,
        _label: CompactString,
    ) -> Result<bool, GraphError> {
        Err(read_only_view_error())
    }

    #[inline]
    fn insert_edge(
        &mut self,
        _id: Option<CompactString>,
        _label: Option<CompactString>,
        _from: Self::VertexId,
        _to: Self::VertexId,
        _properties: PropertyMap,
    ) -> Result<Self::EdgeId, GraphError> {
        Err(read_only_view_error())
    }

    #[inline]
    fn set_vertex_property(
        &mut self,
        _node: Self::VertexId,
        _key: CompactString,
        _value: ElementProperty,
    ) -> Result<(), GraphError> {
        Err(read_only_view_error())
    }

    #[inline]
    fn set_edge_property(
        &mut self,
        _edge: Self::EdgeId,
        _key: CompactString,
        _value: ElementProperty,
    ) -> Result<(), GraphError> {
        Err(read_only_view_error())
    }

    #[inline]
    fn drop_vertex(&mut self, _node: Self::VertexId) -> Result<(), GraphError> {
        Err(read_only_view_error())
    }

    #[inline]
    fn drop_vertex_property(
        &mut self,
        _prop: &str,
        _node: Self::VertexId,
    ) -> Result<(), GraphError> {
        Err(read_only_view_error())
    }

    #[inline]
    fn drop_edge(&mut self, _edge: Self::EdgeId) -> Result<(), GraphError> {
        Err(read_only_view_error())
    }

    #[inline]
    fn drop_edge_property(&mut self, _prop: &str, _edge: Self::EdgeId) -> Result<(), GraphError> {
        Err(read_only_view_error())
    }

    #[inline]
    fn set_schema(&mut self, _schema: GraphSchema) -> Result<(), GraphError> {
        Err(read_only_view_error())
    }

    #[inline]
    fn get_schema(&self) -> Option<GraphSchema> {
        (**self).get_schema()
    }

    #[inline]
    fn definitions(&self) -> impl Iterator<Item = &Definition> + '_ {
        (**self).definitions()
    }

    #[inline]
    fn definition(&self, kind: DefinitionKind, name: &str) -> Option<&Definition> {
        (**self).definition(kind, name)
    }

    #[inline]
    fn set_definition(&mut self, _definition: Definition) -> Result<(), GraphError> {
        Err(read_only_view_error())
    }

    #[inline]
    fn remove_definition(
        &mut self,
        _kind: DefinitionKind,
        _name: &str,
    ) -> Result<bool, GraphError> {
        Err(read_only_view_error())
    }

    #[inline]
    fn recompress(&mut self, _name: &str) -> Result<bool, GraphError> {
        Err(read_only_view_error())
    }

    #[inline]
    fn compression_stats(&self, name: &str) -> Option<crate::graph::CompressionStats> {
        (**self).compression_stats(name)
    }

    #[inline]
    fn estimate_compression(
        &self,
        request: &crate::graph::CompressionEstimateRequest,
    ) -> Result<crate::graph::CompressionEstimate, GraphError> {
        (**self).estimate_compression(request)
    }

    #[inline]
    fn property_indexes(&self) -> Vec<crate::graph::PropertyIndexInfo> {
        (**self).property_indexes()
    }

    #[inline]
    fn vertices_by_property_index<'b>(
        &'b self,
        label: &'b str,
        key: &'b str,
        range: &crate::graph::PropertyIndexRange,
    ) -> Option<Box<dyn Iterator<Item = Self::VertexId> + 'b>> {
        (**self).vertices_by_property_index(label, key, range)
    }

    #[inline]
    fn edges_by_property_index<'b>(
        &'b self,
        label: &'b str,
        key: &'b str,
        range: &crate::graph::PropertyIndexRange,
    ) -> Option<Box<dyn Iterator<Item = Self::EdgeId> + 'b>> {
        (**self).edges_by_property_index(label, key, range)
    }

    #[inline]
    fn vertices_by_transformed_property_index<'b>(
        &'b self,
        label: &'b str,
        key: &'b str,
        transform: &crate::graph::PropertyIndexTransform,
        range: &crate::graph::PropertyIndexRange,
    ) -> Option<Box<dyn Iterator<Item = Self::VertexId> + 'b>> {
        (**self).vertices_by_transformed_property_index(label, key, transform, range)
    }

    #[inline]
    fn edges_by_transformed_property_index<'b>(
        &'b self,
        label: &'b str,
        key: &'b str,
        transform: &crate::graph::PropertyIndexTransform,
        range: &crate::graph::PropertyIndexRange,
    ) -> Option<Box<dyn Iterator<Item = Self::EdgeId> + 'b>> {
        (**self).edges_by_transformed_property_index(label, key, transform, range)
    }

    #[inline]
    fn declared_property_schema(
        &self,
        kind: GraphNodeType,
        labels: &[CompactString],
        key: &str,
    ) -> Option<PropertySchema> {
        (**self).declared_property_schema(kind, labels, key)
    }

    #[inline]
    fn memory_usage(&self) -> Option<crate::graph::MemoryStats> {
        (**self).memory_usage()
    }

    #[inline]
    fn memory_structure(&self) -> Option<crate::graph::StructureMemory> {
        (**self).memory_structure()
    }

    #[inline]
    fn memory_footprint_bytes(&self) -> Option<u64> {
        (**self).memory_footprint_bytes()
    }

    #[inline]
    fn execution_policy(&self) -> crate::graph::ExecutionPolicy {
        (**self).execution_policy()
    }

    /// A read-only view: claims nothing (no writes, so nothing to make atomic or capture).
    #[inline]
    fn capabilities(&self) -> StorageCapabilities {
        StorageCapabilities::none()
    }

    /// A read-only view writes no journal record.
    fn mark(&mut self, name: &str) -> Result<Mark, GraphError> {
        let _ = name;
        Err(GraphError::Unsupported {
            operation: READ_ONLY_MARK,
        })
    }

    #[inline]
    fn last_commit_seq(&self) -> u64 {
        (**self).last_commit_seq()
    }

    fn unit_started_at(&self) -> Option<i128> {
        (**self).unit_started_at()
    }
}

impl<G: GraphStorage> GraphStorage for Box<G> {
    type VertexId = G::VertexId;
    type EdgeId = G::EdgeId;

    #[inline]
    fn vertex_count(&self) -> usize {
        (**self).vertex_count()
    }
    #[inline]
    fn edge_count(&self) -> usize {
        (**self).edge_count()
    }
    #[inline]
    fn vertices(&self) -> impl Iterator<Item = Self::VertexId> + '_ {
        (**self).vertices()
    }
    #[inline]
    fn edges(&self) -> impl Iterator<Item = Self::EdgeId> + '_ {
        (**self).edges()
    }
    #[inline]
    fn get_vertex_by_id(&self, id: &str) -> Option<Self::VertexId> {
        (**self).get_vertex_by_id(id)
    }
    #[inline]
    fn get_edge_by_id(&self, id: &str) -> Option<Self::EdgeId> {
        (**self).get_edge_by_id(id)
    }
    #[inline]
    fn out_edges(&self, node: Self::VertexId) -> impl Iterator<Item = Self::EdgeId> + '_ {
        (**self).out_edges(node)
    }
    #[inline]
    fn in_edges(&self, node: Self::VertexId) -> impl Iterator<Item = Self::EdgeId> + '_ {
        (**self).in_edges(node)
    }
    #[inline]
    fn vertices_by_label<'b>(
        &'b self,
        label: &'b str,
    ) -> Box<dyn Iterator<Item = Self::VertexId> + 'b> {
        (**self).vertices_by_label(label)
    }
    #[inline]
    fn edges_by_label<'b>(&'b self, label: &'b str) -> Box<dyn Iterator<Item = Self::EdgeId> + 'b> {
        (**self).edges_by_label(label)
    }
    #[inline]
    fn vertex_id(&self, node: Self::VertexId) -> Option<&str> {
        (**self).vertex_id(node)
    }
    #[inline]
    fn edge_id(&self, edge: Self::EdgeId) -> Option<&str> {
        (**self).edge_id(edge)
    }
    #[inline]
    fn vertex_label(&self, node: Self::VertexId) -> Option<&str> {
        (**self).vertex_label(node)
    }
    #[inline]
    fn edge_label(&self, edge: Self::EdgeId) -> Option<&str> {
        (**self).edge_label(edge)
    }
    #[inline]
    fn vertex_labels(&self, node: Self::VertexId) -> &[CompactString] {
        (**self).vertex_labels(node)
    }
    #[inline]
    fn vertex_properties(&self, node: Self::VertexId) -> Option<&PropertyMap> {
        (**self).vertex_properties(node)
    }
    #[inline]
    fn edge_properties(&self, edge: Self::EdgeId) -> Option<&PropertyMap> {
        (**self).edge_properties(edge)
    }
    #[inline]
    fn edge_endpoints(&self, edge: Self::EdgeId) -> Option<(Self::VertexId, Self::VertexId)> {
        (**self).edge_endpoints(edge)
    }

    #[inline]
    fn contains_vertex(&self, node: Self::VertexId) -> bool {
        (**self).contains_vertex(node)
    }

    #[inline]
    fn contains_edge(&self, edge: Self::EdgeId) -> bool {
        (**self).contains_edge(edge)
    }

    #[inline]
    fn statistics(&self) -> Option<&GraphStatistics> {
        (**self).statistics()
    }

    #[inline]
    fn insert_vertex(
        &mut self,
        id: Option<CompactString>,
        labels: Vec<CompactString>,
        properties: PropertyMap,
    ) -> Result<Self::VertexId, GraphError> {
        (**self).insert_vertex(id, labels, properties)
    }

    #[inline]
    fn add_vertex_with(
        &mut self,
        id: Option<CompactString>,
        labels: Vec<CompactString>,
        properties: PropertyMap,
        context: &CreateContext,
    ) -> Result<Self::VertexId, GraphError> {
        (**self).add_vertex_with(id, labels, properties, context)
    }

    #[inline]
    fn add_edge_with(
        &mut self,
        id: Option<CompactString>,
        label: Option<CompactString>,
        from: Self::VertexId,
        to: Self::VertexId,
        properties: PropertyMap,
        context: &CreateContext,
    ) -> Result<Self::EdgeId, GraphError> {
        (**self).add_edge_with(id, label, from, to, properties, context)
    }

    #[inline]
    fn add_vertex_label(
        &mut self,
        node: Self::VertexId,
        label: CompactString,
    ) -> Result<bool, GraphError> {
        (**self).add_vertex_label(node, label)
    }

    #[inline]
    fn remove_vertex_label(
        &mut self,
        node: Self::VertexId,
        label: &str,
    ) -> Result<bool, GraphError> {
        (**self).remove_vertex_label(node, label)
    }

    #[inline]
    fn set_edge_label(
        &mut self,
        edge: Self::EdgeId,
        label: CompactString,
    ) -> Result<bool, GraphError> {
        (**self).set_edge_label(edge, label)
    }

    #[inline]
    fn insert_edge(
        &mut self,
        id: Option<CompactString>,
        label: Option<CompactString>,
        from: Self::VertexId,
        to: Self::VertexId,
        properties: PropertyMap,
    ) -> Result<Self::EdgeId, GraphError> {
        (**self).insert_edge(id, label, from, to, properties)
    }

    #[inline]
    fn set_vertex_property(
        &mut self,
        node: Self::VertexId,
        key: CompactString,
        value: ElementProperty,
    ) -> Result<(), GraphError> {
        (**self).set_vertex_property(node, key, value)
    }

    #[inline]
    fn set_edge_property(
        &mut self,
        edge: Self::EdgeId,
        key: CompactString,
        value: ElementProperty,
    ) -> Result<(), GraphError> {
        (**self).set_edge_property(edge, key, value)
    }

    #[inline]
    fn set_vertex_property_path(
        &mut self,
        node: Self::VertexId,
        path: &crate::value::JsonPath,
        value: ElementProperty,
    ) -> Result<(), GraphError> {
        (**self).set_vertex_property_path(node, path, value)
    }

    #[inline]
    fn set_edge_property_path(
        &mut self,
        edge: Self::EdgeId,
        path: &crate::value::JsonPath,
        value: ElementProperty,
    ) -> Result<(), GraphError> {
        (**self).set_edge_property_path(edge, path, value)
    }

    #[inline]
    fn remove_vertex_property_path(
        &mut self,
        node: Self::VertexId,
        path: &crate::value::JsonPath,
    ) -> Result<(), GraphError> {
        (**self).remove_vertex_property_path(node, path)
    }

    #[inline]
    fn remove_edge_property_path(
        &mut self,
        edge: Self::EdgeId,
        path: &crate::value::JsonPath,
    ) -> Result<(), GraphError> {
        (**self).remove_edge_property_path(edge, path)
    }

    #[inline]
    fn drop_vertex(&mut self, node: Self::VertexId) -> Result<(), GraphError> {
        (**self).drop_vertex(node)
    }

    #[inline]
    fn drop_vertex_property(&mut self, prop: &str, node: Self::VertexId) -> Result<(), GraphError> {
        (**self).drop_vertex_property(prop, node)
    }

    #[inline]
    fn drop_edge(&mut self, edge: Self::EdgeId) -> Result<(), GraphError> {
        (**self).drop_edge(edge)
    }

    #[inline]
    fn drop_edge_property(&mut self, prop: &str, edge: Self::EdgeId) -> Result<(), GraphError> {
        (**self).drop_edge_property(prop, edge)
    }

    #[inline]
    fn set_schema(&mut self, schema: GraphSchema) -> Result<(), GraphError> {
        (**self).set_schema(schema)
    }

    #[inline]
    fn definitions(&self) -> impl Iterator<Item = &Definition> + '_ {
        (**self).definitions()
    }

    #[inline]
    fn definition(&self, kind: DefinitionKind, name: &str) -> Option<&Definition> {
        (**self).definition(kind, name)
    }

    #[inline]
    fn set_definition(&mut self, definition: Definition) -> Result<(), GraphError> {
        (**self).set_definition(definition)
    }

    #[inline]
    fn remove_definition(&mut self, kind: DefinitionKind, name: &str) -> Result<bool, GraphError> {
        (**self).remove_definition(kind, name)
    }

    #[inline]
    fn recompress(&mut self, name: &str) -> Result<bool, GraphError> {
        (**self).recompress(name)
    }

    #[inline]
    fn compression_stats(&self, name: &str) -> Option<crate::graph::CompressionStats> {
        (**self).compression_stats(name)
    }

    #[inline]
    fn estimate_compression(
        &self,
        request: &crate::graph::CompressionEstimateRequest,
    ) -> Result<crate::graph::CompressionEstimate, GraphError> {
        (**self).estimate_compression(request)
    }

    #[inline]
    fn property_indexes(&self) -> Vec<crate::graph::PropertyIndexInfo> {
        (**self).property_indexes()
    }

    #[inline]
    fn vertices_by_property_index<'b>(
        &'b self,
        label: &'b str,
        key: &'b str,
        range: &crate::graph::PropertyIndexRange,
    ) -> Option<Box<dyn Iterator<Item = Self::VertexId> + 'b>> {
        (**self).vertices_by_property_index(label, key, range)
    }

    #[inline]
    fn edges_by_property_index<'b>(
        &'b self,
        label: &'b str,
        key: &'b str,
        range: &crate::graph::PropertyIndexRange,
    ) -> Option<Box<dyn Iterator<Item = Self::EdgeId> + 'b>> {
        (**self).edges_by_property_index(label, key, range)
    }

    #[inline]
    fn vertices_by_transformed_property_index<'b>(
        &'b self,
        label: &'b str,
        key: &'b str,
        transform: &crate::graph::PropertyIndexTransform,
        range: &crate::graph::PropertyIndexRange,
    ) -> Option<Box<dyn Iterator<Item = Self::VertexId> + 'b>> {
        (**self).vertices_by_transformed_property_index(label, key, transform, range)
    }

    #[inline]
    fn edges_by_transformed_property_index<'b>(
        &'b self,
        label: &'b str,
        key: &'b str,
        transform: &crate::graph::PropertyIndexTransform,
        range: &crate::graph::PropertyIndexRange,
    ) -> Option<Box<dyn Iterator<Item = Self::EdgeId> + 'b>> {
        (**self).edges_by_transformed_property_index(label, key, transform, range)
    }

    #[inline]
    fn get_schema(&self) -> Option<GraphSchema> {
        (**self).get_schema()
    }

    #[inline]
    fn declared_property_schema(
        &self,
        kind: GraphNodeType,
        labels: &[CompactString],
        key: &str,
    ) -> Option<PropertySchema> {
        (**self).declared_property_schema(kind, labels, key)
    }

    #[inline]
    fn memory_usage(&self) -> Option<crate::graph::MemoryStats> {
        (**self).memory_usage()
    }

    #[inline]
    fn memory_structure(&self) -> Option<crate::graph::StructureMemory> {
        (**self).memory_structure()
    }

    #[inline]
    fn memory_footprint_bytes(&self) -> Option<u64> {
        (**self).memory_footprint_bytes()
    }

    #[inline]
    fn execution_policy(&self) -> crate::graph::ExecutionPolicy {
        (**self).execution_policy()
    }

    #[inline]
    fn begin_unit(&mut self) -> UnitMark {
        (**self).begin_unit()
    }

    #[inline]
    fn commit_unit(&mut self, mark: UnitMark) -> Result<(), GraphError> {
        (**self).commit_unit(mark)
    }

    #[inline]
    fn rollback_unit(&mut self, mark: UnitMark, reason: &RollbackReason) {
        (**self).rollback_unit(mark, reason);
    }

    #[inline]
    fn unit_changes(&self, mark: UnitMark) -> UnitChanges {
        (**self).unit_changes(mark)
    }

    #[inline]
    fn set_unit_metadata(&mut self, metadata: CommitMetadata) {
        (**self).set_unit_metadata(metadata);
    }

    #[inline]
    fn discard_unit(&mut self, mark: UnitMark) -> Result<ChangeSet, GraphError> {
        (**self).discard_unit(mark)
    }

    #[inline]
    fn mark(&mut self, name: &str) -> Result<Mark, GraphError> {
        (**self).mark(name)
    }

    #[inline]
    fn capabilities(&self) -> StorageCapabilities {
        (**self).capabilities()
    }

    #[inline]
    fn register_commit_hook(
        &mut self,
        hook: Box<dyn crate::graph::CommitHook>,
    ) -> Result<(), GraphError> {
        (**self).register_commit_hook(hook)
    }

    #[inline]
    fn last_commit_seq(&self) -> u64 {
        (**self).last_commit_seq()
    }

    fn unit_started_at(&self) -> Option<i128> {
        (**self).unit_started_at()
    }

    #[inline]
    fn raise_id_sequences(
        &mut self,
        node_sequence: u64,
        edge_sequence: u64,
    ) -> Result<(), GraphError> {
        (**self).raise_id_sequences(node_sequence, edge_sequence)
    }

    #[inline]
    fn apply_change_set(&mut self, changes: &ChangeSet) -> Result<(), GraphError> {
        (**self).apply_change_set(changes)
    }
}

impl<G: GraphStorage> GraphStorage for &mut G {
    type VertexId = G::VertexId;
    type EdgeId = G::EdgeId;

    #[inline]
    fn vertex_count(&self) -> usize {
        (**self).vertex_count()
    }
    #[inline]
    fn edge_count(&self) -> usize {
        (**self).edge_count()
    }
    #[inline]
    fn vertices(&self) -> impl Iterator<Item = Self::VertexId> + '_ {
        (**self).vertices()
    }
    #[inline]
    fn edges(&self) -> impl Iterator<Item = Self::EdgeId> + '_ {
        (**self).edges()
    }
    #[inline]
    fn get_vertex_by_id(&self, id: &str) -> Option<Self::VertexId> {
        (**self).get_vertex_by_id(id)
    }
    #[inline]
    fn get_edge_by_id(&self, id: &str) -> Option<Self::EdgeId> {
        (**self).get_edge_by_id(id)
    }
    #[inline]
    fn out_edges(&self, node: Self::VertexId) -> impl Iterator<Item = Self::EdgeId> + '_ {
        (**self).out_edges(node)
    }
    #[inline]
    fn in_edges(&self, node: Self::VertexId) -> impl Iterator<Item = Self::EdgeId> + '_ {
        (**self).in_edges(node)
    }
    #[inline]
    fn vertices_by_label<'b>(
        &'b self,
        label: &'b str,
    ) -> Box<dyn Iterator<Item = Self::VertexId> + 'b> {
        (**self).vertices_by_label(label)
    }
    #[inline]
    fn edges_by_label<'b>(&'b self, label: &'b str) -> Box<dyn Iterator<Item = Self::EdgeId> + 'b> {
        (**self).edges_by_label(label)
    }
    #[inline]
    fn vertex_id(&self, node: Self::VertexId) -> Option<&str> {
        (**self).vertex_id(node)
    }
    #[inline]
    fn edge_id(&self, edge: Self::EdgeId) -> Option<&str> {
        (**self).edge_id(edge)
    }
    #[inline]
    fn vertex_label(&self, node: Self::VertexId) -> Option<&str> {
        (**self).vertex_label(node)
    }
    #[inline]
    fn edge_label(&self, edge: Self::EdgeId) -> Option<&str> {
        (**self).edge_label(edge)
    }
    #[inline]
    fn vertex_labels(&self, node: Self::VertexId) -> &[CompactString] {
        (**self).vertex_labels(node)
    }
    #[inline]
    fn vertex_properties(&self, node: Self::VertexId) -> Option<&PropertyMap> {
        (**self).vertex_properties(node)
    }
    #[inline]
    fn edge_properties(&self, edge: Self::EdgeId) -> Option<&PropertyMap> {
        (**self).edge_properties(edge)
    }
    #[inline]
    fn edge_endpoints(&self, edge: Self::EdgeId) -> Option<(Self::VertexId, Self::VertexId)> {
        (**self).edge_endpoints(edge)
    }

    #[inline]
    fn contains_vertex(&self, node: Self::VertexId) -> bool {
        (**self).contains_vertex(node)
    }

    #[inline]
    fn contains_edge(&self, edge: Self::EdgeId) -> bool {
        (**self).contains_edge(edge)
    }

    #[inline]
    fn statistics(&self) -> Option<&GraphStatistics> {
        (**self).statistics()
    }

    #[inline]
    fn insert_vertex(
        &mut self,
        id: Option<CompactString>,
        labels: Vec<CompactString>,
        properties: PropertyMap,
    ) -> Result<Self::VertexId, GraphError> {
        (**self).insert_vertex(id, labels, properties)
    }

    #[inline]
    fn add_vertex_with(
        &mut self,
        id: Option<CompactString>,
        labels: Vec<CompactString>,
        properties: PropertyMap,
        context: &CreateContext,
    ) -> Result<Self::VertexId, GraphError> {
        (**self).add_vertex_with(id, labels, properties, context)
    }

    #[inline]
    fn add_edge_with(
        &mut self,
        id: Option<CompactString>,
        label: Option<CompactString>,
        from: Self::VertexId,
        to: Self::VertexId,
        properties: PropertyMap,
        context: &CreateContext,
    ) -> Result<Self::EdgeId, GraphError> {
        (**self).add_edge_with(id, label, from, to, properties, context)
    }

    #[inline]
    fn add_vertex_label(
        &mut self,
        node: Self::VertexId,
        label: CompactString,
    ) -> Result<bool, GraphError> {
        (**self).add_vertex_label(node, label)
    }

    #[inline]
    fn remove_vertex_label(
        &mut self,
        node: Self::VertexId,
        label: &str,
    ) -> Result<bool, GraphError> {
        (**self).remove_vertex_label(node, label)
    }

    #[inline]
    fn set_edge_label(
        &mut self,
        edge: Self::EdgeId,
        label: CompactString,
    ) -> Result<bool, GraphError> {
        (**self).set_edge_label(edge, label)
    }

    #[inline]
    fn insert_edge(
        &mut self,
        id: Option<CompactString>,
        label: Option<CompactString>,
        from: Self::VertexId,
        to: Self::VertexId,
        properties: PropertyMap,
    ) -> Result<Self::EdgeId, GraphError> {
        (**self).insert_edge(id, label, from, to, properties)
    }

    #[inline]
    fn set_vertex_property(
        &mut self,
        node: Self::VertexId,
        key: CompactString,
        value: ElementProperty,
    ) -> Result<(), GraphError> {
        (**self).set_vertex_property(node, key, value)
    }

    #[inline]
    fn set_edge_property(
        &mut self,
        edge: Self::EdgeId,
        key: CompactString,
        value: ElementProperty,
    ) -> Result<(), GraphError> {
        (**self).set_edge_property(edge, key, value)
    }

    #[inline]
    fn set_vertex_property_path(
        &mut self,
        node: Self::VertexId,
        path: &crate::value::JsonPath,
        value: ElementProperty,
    ) -> Result<(), GraphError> {
        (**self).set_vertex_property_path(node, path, value)
    }

    #[inline]
    fn set_edge_property_path(
        &mut self,
        edge: Self::EdgeId,
        path: &crate::value::JsonPath,
        value: ElementProperty,
    ) -> Result<(), GraphError> {
        (**self).set_edge_property_path(edge, path, value)
    }

    #[inline]
    fn remove_vertex_property_path(
        &mut self,
        node: Self::VertexId,
        path: &crate::value::JsonPath,
    ) -> Result<(), GraphError> {
        (**self).remove_vertex_property_path(node, path)
    }

    #[inline]
    fn remove_edge_property_path(
        &mut self,
        edge: Self::EdgeId,
        path: &crate::value::JsonPath,
    ) -> Result<(), GraphError> {
        (**self).remove_edge_property_path(edge, path)
    }

    #[inline]
    fn drop_vertex(&mut self, node: Self::VertexId) -> Result<(), GraphError> {
        (**self).drop_vertex(node)
    }

    #[inline]
    fn drop_vertex_property(&mut self, prop: &str, node: Self::VertexId) -> Result<(), GraphError> {
        (**self).drop_vertex_property(prop, node)
    }

    #[inline]
    fn drop_edge(&mut self, edge: Self::EdgeId) -> Result<(), GraphError> {
        (**self).drop_edge(edge)
    }

    #[inline]
    fn drop_edge_property(&mut self, prop: &str, edge: Self::EdgeId) -> Result<(), GraphError> {
        (**self).drop_edge_property(prop, edge)
    }

    #[inline]
    fn set_schema(&mut self, schema: GraphSchema) -> Result<(), GraphError> {
        (**self).set_schema(schema)
    }

    #[inline]
    fn definitions(&self) -> impl Iterator<Item = &Definition> + '_ {
        (**self).definitions()
    }

    #[inline]
    fn definition(&self, kind: DefinitionKind, name: &str) -> Option<&Definition> {
        (**self).definition(kind, name)
    }

    #[inline]
    fn set_definition(&mut self, definition: Definition) -> Result<(), GraphError> {
        (**self).set_definition(definition)
    }

    #[inline]
    fn remove_definition(&mut self, kind: DefinitionKind, name: &str) -> Result<bool, GraphError> {
        (**self).remove_definition(kind, name)
    }

    #[inline]
    fn recompress(&mut self, name: &str) -> Result<bool, GraphError> {
        (**self).recompress(name)
    }

    #[inline]
    fn compression_stats(&self, name: &str) -> Option<crate::graph::CompressionStats> {
        (**self).compression_stats(name)
    }

    #[inline]
    fn estimate_compression(
        &self,
        request: &crate::graph::CompressionEstimateRequest,
    ) -> Result<crate::graph::CompressionEstimate, GraphError> {
        (**self).estimate_compression(request)
    }

    #[inline]
    fn property_indexes(&self) -> Vec<crate::graph::PropertyIndexInfo> {
        (**self).property_indexes()
    }

    #[inline]
    fn vertices_by_property_index<'b>(
        &'b self,
        label: &'b str,
        key: &'b str,
        range: &crate::graph::PropertyIndexRange,
    ) -> Option<Box<dyn Iterator<Item = Self::VertexId> + 'b>> {
        (**self).vertices_by_property_index(label, key, range)
    }

    #[inline]
    fn edges_by_property_index<'b>(
        &'b self,
        label: &'b str,
        key: &'b str,
        range: &crate::graph::PropertyIndexRange,
    ) -> Option<Box<dyn Iterator<Item = Self::EdgeId> + 'b>> {
        (**self).edges_by_property_index(label, key, range)
    }

    #[inline]
    fn vertices_by_transformed_property_index<'b>(
        &'b self,
        label: &'b str,
        key: &'b str,
        transform: &crate::graph::PropertyIndexTransform,
        range: &crate::graph::PropertyIndexRange,
    ) -> Option<Box<dyn Iterator<Item = Self::VertexId> + 'b>> {
        (**self).vertices_by_transformed_property_index(label, key, transform, range)
    }

    #[inline]
    fn edges_by_transformed_property_index<'b>(
        &'b self,
        label: &'b str,
        key: &'b str,
        transform: &crate::graph::PropertyIndexTransform,
        range: &crate::graph::PropertyIndexRange,
    ) -> Option<Box<dyn Iterator<Item = Self::EdgeId> + 'b>> {
        (**self).edges_by_transformed_property_index(label, key, transform, range)
    }

    #[inline]
    fn get_schema(&self) -> Option<GraphSchema> {
        (**self).get_schema()
    }

    #[inline]
    fn declared_property_schema(
        &self,
        kind: GraphNodeType,
        labels: &[CompactString],
        key: &str,
    ) -> Option<PropertySchema> {
        (**self).declared_property_schema(kind, labels, key)
    }

    #[inline]
    fn memory_usage(&self) -> Option<crate::graph::MemoryStats> {
        (**self).memory_usage()
    }

    #[inline]
    fn memory_structure(&self) -> Option<crate::graph::StructureMemory> {
        (**self).memory_structure()
    }

    #[inline]
    fn memory_footprint_bytes(&self) -> Option<u64> {
        (**self).memory_footprint_bytes()
    }

    #[inline]
    fn execution_policy(&self) -> crate::graph::ExecutionPolicy {
        (**self).execution_policy()
    }

    #[inline]
    fn begin_unit(&mut self) -> UnitMark {
        (**self).begin_unit()
    }

    #[inline]
    fn commit_unit(&mut self, mark: UnitMark) -> Result<(), GraphError> {
        (**self).commit_unit(mark)
    }

    #[inline]
    fn rollback_unit(&mut self, mark: UnitMark, reason: &RollbackReason) {
        (**self).rollback_unit(mark, reason);
    }

    #[inline]
    fn unit_changes(&self, mark: UnitMark) -> UnitChanges {
        (**self).unit_changes(mark)
    }

    #[inline]
    fn set_unit_metadata(&mut self, metadata: CommitMetadata) {
        (**self).set_unit_metadata(metadata);
    }

    #[inline]
    fn discard_unit(&mut self, mark: UnitMark) -> Result<ChangeSet, GraphError> {
        (**self).discard_unit(mark)
    }

    #[inline]
    fn mark(&mut self, name: &str) -> Result<Mark, GraphError> {
        (**self).mark(name)
    }

    #[inline]
    fn capabilities(&self) -> StorageCapabilities {
        (**self).capabilities()
    }

    #[inline]
    fn register_commit_hook(
        &mut self,
        hook: Box<dyn crate::graph::CommitHook>,
    ) -> Result<(), GraphError> {
        (**self).register_commit_hook(hook)
    }

    #[inline]
    fn last_commit_seq(&self) -> u64 {
        (**self).last_commit_seq()
    }

    fn unit_started_at(&self) -> Option<i128> {
        (**self).unit_started_at()
    }

    #[inline]
    fn raise_id_sequences(
        &mut self,
        node_sequence: u64,
        edge_sequence: u64,
    ) -> Result<(), GraphError> {
        (**self).raise_id_sequences(node_sequence, edge_sequence)
    }

    #[inline]
    fn apply_change_set(&mut self, changes: &ChangeSet) -> Result<(), GraphError> {
        (**self).apply_change_set(changes)
    }
}

impl GraphStorage for Arc<TraversalGraph> {
    type VertexId = VertexHandle;
    type EdgeId = EdgeHandle;

    #[inline]
    fn vertex_count(&self) -> usize {
        self.as_ref().vertex_count()
    }

    #[inline]
    fn edge_count(&self) -> usize {
        self.as_ref().edge_count()
    }

    #[inline]
    fn vertices(&self) -> impl Iterator<Item = VertexHandle> + '_ {
        GraphStorage::vertices(self.as_ref())
    }

    #[inline]
    fn edges(&self) -> impl Iterator<Item = EdgeHandle> + '_ {
        GraphStorage::edges(self.as_ref())
    }

    #[inline]
    fn get_vertex_by_id(&self, id: &str) -> Option<VertexHandle> {
        GraphStorage::get_vertex_by_id(self.as_ref(), id)
    }

    #[inline]
    fn get_edge_by_id(&self, id: &str) -> Option<EdgeHandle> {
        GraphStorage::get_edge_by_id(self.as_ref(), id)
    }

    #[inline]
    fn out_edges(&self, node: VertexHandle) -> impl Iterator<Item = EdgeHandle> + '_ {
        GraphStorage::out_edges(self.as_ref(), node)
    }

    #[inline]
    fn in_edges(&self, node: VertexHandle) -> impl Iterator<Item = EdgeHandle> + '_ {
        GraphStorage::in_edges(self.as_ref(), node)
    }

    #[inline]
    fn vertex_id(&self, node: VertexHandle) -> Option<&str> {
        self.as_ref().vertex_id(node)
    }

    #[inline]
    fn edge_id(&self, edge: EdgeHandle) -> Option<&str> {
        self.as_ref().edge_id(edge)
    }

    #[inline]
    fn vertex_label(&self, node: VertexHandle) -> Option<&str> {
        self.as_ref().vertex_label(node)
    }

    #[inline]
    fn edge_label(&self, edge: EdgeHandle) -> Option<&str> {
        self.as_ref().edge_label(edge)
    }

    #[inline]
    fn vertex_labels(&self, node: VertexHandle) -> &[CompactString] {
        self.as_ref().vertex_labels(node)
    }

    #[inline]
    fn vertex_properties(&self, node: VertexHandle) -> Option<&PropertyMap> {
        self.as_ref().vertex_properties(node)
    }

    #[inline]
    fn edge_properties(&self, edge: EdgeHandle) -> Option<&PropertyMap> {
        self.as_ref().edge_properties(edge)
    }

    #[inline]
    fn edge_endpoints(&self, edge: EdgeHandle) -> Option<(VertexHandle, VertexHandle)> {
        self.as_ref().edge_endpoints(edge)
    }

    #[inline]
    fn contains_vertex(&self, node: VertexHandle) -> bool {
        self.as_ref().contains_vertex(node)
    }

    #[inline]
    fn contains_edge(&self, edge: EdgeHandle) -> bool {
        self.as_ref().contains_edge(edge)
    }

    #[inline]
    fn vertices_by_label<'b>(
        &'b self,
        label: &'b str,
    ) -> Box<dyn Iterator<Item = VertexHandle> + 'b> {
        self.as_ref().vertices_by_label(label)
    }

    #[inline]
    fn edges_by_label<'b>(&'b self, label: &'b str) -> Box<dyn Iterator<Item = EdgeHandle> + 'b> {
        self.as_ref().edges_by_label(label)
    }

    #[inline]
    fn statistics(&self) -> Option<&GraphStatistics> {
        self.as_ref().statistics()
    }

    #[inline]
    fn insert_vertex(
        &mut self,
        id: Option<CompactString>,
        labels: Vec<CompactString>,
        properties: PropertyMap,
    ) -> Result<VertexHandle, GraphError> {
        if let Some(graph) = Arc::get_mut(self) {
            GraphStorage::insert_vertex(graph, id, labels, properties)
        } else {
            Err(GraphError::InvalidElementState {
                reason: "Cannot mutate shared Arc<TraversalGraph>".to_string(),
            })
        }
    }

    #[inline]
    fn add_vertex_with(
        &mut self,
        id: Option<CompactString>,
        labels: Vec<CompactString>,
        properties: PropertyMap,
        context: &CreateContext,
    ) -> Result<VertexHandle, GraphError> {
        if let Some(graph) = Arc::get_mut(self) {
            GraphStorage::add_vertex_with(graph, id, labels, properties, context)
        } else {
            Err(GraphError::InvalidElementState {
                reason: "Cannot mutate shared Arc<TraversalGraph>".to_string(),
            })
        }
    }

    #[inline]
    fn add_vertex_label(
        &mut self,
        node: VertexHandle,
        label: CompactString,
    ) -> Result<bool, GraphError> {
        if let Some(graph) = Arc::get_mut(self) {
            GraphStorage::add_vertex_label(graph, node, label)
        } else {
            Err(GraphError::InvalidElementState {
                reason: "Cannot mutate shared Arc<TraversalGraph>".to_string(),
            })
        }
    }

    #[inline]
    fn remove_vertex_label(&mut self, node: VertexHandle, label: &str) -> Result<bool, GraphError> {
        if let Some(graph) = Arc::get_mut(self) {
            GraphStorage::remove_vertex_label(graph, node, label)
        } else {
            Err(GraphError::InvalidElementState {
                reason: "Cannot mutate shared Arc<TraversalGraph>".to_string(),
            })
        }
    }

    #[inline]
    fn set_edge_label(
        &mut self,
        edge: EdgeHandle,
        label: CompactString,
    ) -> Result<bool, GraphError> {
        if let Some(graph) = Arc::get_mut(self) {
            GraphStorage::set_edge_label(graph, edge, label)
        } else {
            Err(GraphError::InvalidElementState {
                reason: "Cannot mutate shared Arc<TraversalGraph>".to_string(),
            })
        }
    }

    #[inline]
    fn insert_edge(
        &mut self,
        id: Option<CompactString>,
        label: Option<CompactString>,
        from: VertexHandle,
        to: VertexHandle,
        properties: PropertyMap,
    ) -> Result<EdgeHandle, GraphError> {
        if let Some(graph) = Arc::get_mut(self) {
            GraphStorage::insert_edge(graph, id, label, from, to, properties)
        } else {
            Err(GraphError::InvalidElementState {
                reason: "Cannot mutate shared Arc<TraversalGraph>".to_string(),
            })
        }
    }

    #[inline]
    fn add_edge_with(
        &mut self,
        id: Option<CompactString>,
        label: Option<CompactString>,
        from: VertexHandle,
        to: VertexHandle,
        properties: PropertyMap,
        context: &CreateContext,
    ) -> Result<EdgeHandle, GraphError> {
        if let Some(graph) = Arc::get_mut(self) {
            GraphStorage::add_edge_with(graph, id, label, from, to, properties, context)
        } else {
            Err(GraphError::InvalidElementState {
                reason: "Cannot mutate shared Arc<TraversalGraph>".to_string(),
            })
        }
    }

    #[inline]
    fn set_vertex_property(
        &mut self,
        node: VertexHandle,
        key: CompactString,
        value: ElementProperty,
    ) -> Result<(), GraphError> {
        if let Some(graph) = Arc::get_mut(self) {
            GraphStorage::set_vertex_property(graph, node, key, value)
        } else {
            Err(GraphError::InvalidElementState {
                reason: "Cannot mutate shared Arc<TraversalGraph>".to_string(),
            })
        }
    }

    #[inline]
    fn set_edge_property(
        &mut self,
        edge: EdgeHandle,
        key: CompactString,
        value: ElementProperty,
    ) -> Result<(), GraphError> {
        if let Some(graph) = Arc::get_mut(self) {
            GraphStorage::set_edge_property(graph, edge, key, value)
        } else {
            Err(GraphError::InvalidElementState {
                reason: "Cannot mutate shared Arc<TraversalGraph>".to_string(),
            })
        }
    }

    #[inline]
    fn set_vertex_property_path(
        &mut self,
        node: VertexHandle,
        path: &crate::value::JsonPath,
        value: ElementProperty,
    ) -> Result<(), GraphError> {
        if let Some(graph) = Arc::get_mut(self) {
            GraphStorage::set_vertex_property_path(graph, node, path, value)
        } else {
            Err(GraphError::InvalidElementState {
                reason: "Cannot mutate shared Arc<TraversalGraph>".to_string(),
            })
        }
    }

    #[inline]
    fn set_edge_property_path(
        &mut self,
        edge: EdgeHandle,
        path: &crate::value::JsonPath,
        value: ElementProperty,
    ) -> Result<(), GraphError> {
        if let Some(graph) = Arc::get_mut(self) {
            GraphStorage::set_edge_property_path(graph, edge, path, value)
        } else {
            Err(GraphError::InvalidElementState {
                reason: "Cannot mutate shared Arc<TraversalGraph>".to_string(),
            })
        }
    }

    #[inline]
    fn remove_vertex_property_path(
        &mut self,
        node: VertexHandle,
        path: &crate::value::JsonPath,
    ) -> Result<(), GraphError> {
        if let Some(graph) = Arc::get_mut(self) {
            GraphStorage::remove_vertex_property_path(graph, node, path)
        } else {
            Err(GraphError::InvalidElementState {
                reason: "Cannot mutate shared Arc<TraversalGraph>".to_string(),
            })
        }
    }

    #[inline]
    fn remove_edge_property_path(
        &mut self,
        edge: EdgeHandle,
        path: &crate::value::JsonPath,
    ) -> Result<(), GraphError> {
        if let Some(graph) = Arc::get_mut(self) {
            GraphStorage::remove_edge_property_path(graph, edge, path)
        } else {
            Err(GraphError::InvalidElementState {
                reason: "Cannot mutate shared Arc<TraversalGraph>".to_string(),
            })
        }
    }

    #[inline]
    fn drop_vertex(&mut self, node: VertexHandle) -> Result<(), GraphError> {
        if let Some(graph) = Arc::get_mut(self) {
            GraphStorage::drop_vertex(graph, node)
        } else {
            Err(GraphError::InvalidElementState {
                reason: "Cannot mutate shared Arc<TraversalGraph>".to_string(),
            })
        }
    }

    #[inline]
    fn drop_vertex_property(&mut self, prop: &str, node: VertexHandle) -> Result<(), GraphError> {
        if let Some(graph) = Arc::get_mut(self) {
            GraphStorage::drop_vertex_property(graph, prop, node)
        } else {
            Err(GraphError::InvalidElementState {
                reason: "Cannot mutate shared Arc<TraversalGraph>".to_string(),
            })
        }
    }

    #[inline]
    fn drop_edge(&mut self, edge: EdgeHandle) -> Result<(), GraphError> {
        if let Some(graph) = Arc::get_mut(self) {
            GraphStorage::drop_edge(graph, edge)
        } else {
            Err(GraphError::InvalidElementState {
                reason: "Cannot mutate shared Arc<TraversalGraph>".to_string(),
            })
        }
    }

    #[inline]
    fn drop_edge_property(&mut self, prop: &str, edge: EdgeHandle) -> Result<(), GraphError> {
        if let Some(graph) = Arc::get_mut(self) {
            GraphStorage::drop_edge_property(graph, prop, edge)
        } else {
            Err(GraphError::InvalidElementState {
                reason: "Cannot mutate shared Arc<TraversalGraph>".to_string(),
            })
        }
    }

    #[inline]
    fn set_schema(&mut self, schema: GraphSchema) -> Result<(), GraphError> {
        // Like every other write: only the sole owner can change the graph, and the change is
        // recorded in its open unit.
        match Arc::get_mut(self) {
            Some(graph) => GraphStorage::set_schema(graph, schema),
            None => Err(GraphError::InvalidElementState {
                reason: "Cannot mutate shared Arc<TraversalGraph>".to_string(),
            }),
        }
    }

    #[inline]
    fn definitions(&self) -> impl Iterator<Item = &Definition> + '_ {
        self.as_ref().definitions.iter()
    }

    #[inline]
    fn definition(&self, kind: DefinitionKind, name: &str) -> Option<&Definition> {
        self.as_ref().definitions.get(kind, name)
    }

    #[inline]
    fn set_definition(&mut self, definition: Definition) -> Result<(), GraphError> {
        match Arc::get_mut(self) {
            Some(graph) => GraphStorage::set_definition(graph, definition),
            None => Err(GraphError::InvalidElementState {
                reason: "Cannot mutate shared Arc<TraversalGraph>".to_string(),
            }),
        }
    }

    #[inline]
    fn remove_definition(&mut self, kind: DefinitionKind, name: &str) -> Result<bool, GraphError> {
        match Arc::get_mut(self) {
            Some(graph) => GraphStorage::remove_definition(graph, kind, name),
            None => Err(GraphError::InvalidElementState {
                reason: "Cannot mutate shared Arc<TraversalGraph>".to_string(),
            }),
        }
    }

    #[inline]
    fn recompress(&mut self, name: &str) -> Result<bool, GraphError> {
        match Arc::get_mut(self) {
            Some(graph) => GraphStorage::recompress(graph, name),
            None => Err(GraphError::InvalidElementState {
                reason: "Cannot mutate shared Arc<TraversalGraph>".to_string(),
            }),
        }
    }

    #[inline]
    fn compression_stats(&self, name: &str) -> Option<crate::graph::CompressionStats> {
        TraversalGraph::compression_stats(self, name)
    }

    fn estimate_compression(
        &self,
        request: &crate::graph::CompressionEstimateRequest,
    ) -> Result<crate::graph::CompressionEstimate, GraphError> {
        Ok(TraversalGraph::estimate_compression(self, request))
    }

    #[inline]
    fn property_indexes(&self) -> Vec<crate::graph::PropertyIndexInfo> {
        self.as_ref().property_index_infos()
    }

    #[inline]
    fn vertices_by_property_index<'a>(
        &'a self,
        label: &'a str,
        key: &'a str,
        range: &crate::graph::PropertyIndexRange,
    ) -> Option<Box<dyn Iterator<Item = VertexHandle> + 'a>> {
        self.as_ref()
            .property_index_vertices(label, key, None, range)
    }

    #[inline]
    fn edges_by_property_index<'a>(
        &'a self,
        label: &'a str,
        key: &'a str,
        range: &crate::graph::PropertyIndexRange,
    ) -> Option<Box<dyn Iterator<Item = EdgeHandle> + 'a>> {
        self.as_ref().property_index_edges(label, key, None, range)
    }

    #[inline]
    fn vertices_by_transformed_property_index<'a>(
        &'a self,
        label: &'a str,
        key: &'a str,
        transform: &crate::graph::PropertyIndexTransform,
        range: &crate::graph::PropertyIndexRange,
    ) -> Option<Box<dyn Iterator<Item = VertexHandle> + 'a>> {
        self.as_ref()
            .property_index_vertices(label, key, Some(transform), range)
    }

    #[inline]
    fn edges_by_transformed_property_index<'a>(
        &'a self,
        label: &'a str,
        key: &'a str,
        transform: &crate::graph::PropertyIndexTransform,
        range: &crate::graph::PropertyIndexRange,
    ) -> Option<Box<dyn Iterator<Item = EdgeHandle> + 'a>> {
        self.as_ref()
            .property_index_edges(label, key, Some(transform), range)
    }

    #[inline]
    fn get_schema(&self) -> Option<GraphSchema> {
        self.as_ref().get_schema()
    }

    #[inline]
    fn declared_property_schema(
        &self,
        kind: GraphNodeType,
        labels: &[CompactString],
        key: &str,
    ) -> Option<PropertySchema> {
        self.as_ref().declared_property_schema(kind, labels, key)
    }

    #[inline]
    fn memory_usage(&self) -> Option<crate::graph::MemoryStats> {
        self.as_ref().memory_usage()
    }

    #[inline]
    fn memory_structure(&self) -> Option<crate::graph::StructureMemory> {
        self.as_ref().memory_structure()
    }

    #[inline]
    fn memory_footprint_bytes(&self) -> Option<u64> {
        self.as_ref().memory_footprint_bytes()
    }

    #[inline]
    fn execution_policy(&self) -> crate::graph::ExecutionPolicy {
        self.as_ref().execution_policy()
    }

    /// Forwarded while this `Arc` is the only owner (the only case in which it can mutate at
    /// all); a shared `Arc` is non-atomic like the trait default.
    #[inline]
    fn begin_unit(&mut self) -> UnitMark {
        Arc::get_mut(self).map_or_else(UnitMark::default, GraphStorage::begin_unit)
    }

    #[inline]
    fn commit_unit(&mut self, mark: UnitMark) -> Result<(), GraphError> {
        match Arc::get_mut(self) {
            Some(graph) => GraphStorage::commit_unit(graph, mark),
            None => Ok(()),
        }
    }

    #[inline]
    fn rollback_unit(&mut self, mark: UnitMark, reason: &RollbackReason) {
        if let Some(graph) = Arc::get_mut(self) {
            GraphStorage::rollback_unit(graph, mark, reason);
        }
    }

    #[inline]
    fn unit_changes(&self, mark: UnitMark) -> UnitChanges {
        GraphStorage::unit_changes(self.as_ref(), mark)
    }

    #[inline]
    fn set_unit_metadata(&mut self, metadata: CommitMetadata) {
        if let Some(graph) = Arc::get_mut(self) {
            GraphStorage::set_unit_metadata(graph, metadata);
        }
    }

    /// Forwarded while this `Arc` is the only owner; a shared `Arc` behaves like the trait
    /// default (nothing to undo, [`GraphError::Unsupported`]).
    #[inline]
    fn discard_unit(&mut self, mark: UnitMark) -> Result<ChangeSet, GraphError> {
        match Arc::get_mut(self) {
            Some(graph) => GraphStorage::discard_unit(graph, mark),
            None => Err(GraphError::Unsupported {
                operation: DRY_RUN_OPERATION,
            }),
        }
    }

    #[inline]
    fn mark(&mut self, name: &str) -> Result<Mark, GraphError> {
        match Arc::get_mut(self) {
            Some(graph) => GraphStorage::mark(graph, name),
            None => Err(GraphError::InvalidElementState {
                reason: "Cannot mutate shared Arc<TraversalGraph>".to_string(),
            }),
        }
    }

    /// The capabilities of [`TraversalGraph`] while this `Arc` is the only owner; a shared
    /// `Arc` cannot mutate at all and claims nothing.
    #[inline]
    fn capabilities(&self) -> StorageCapabilities {
        if Arc::strong_count(self) == 1 && Arc::weak_count(self) == 0 {
            self.as_ref().capabilities()
        } else {
            StorageCapabilities::none()
        }
    }

    #[inline]
    fn register_commit_hook(
        &mut self,
        hook: Box<dyn crate::graph::CommitHook>,
    ) -> Result<(), GraphError> {
        match Arc::get_mut(self) {
            Some(graph) => GraphStorage::register_commit_hook(graph, hook),
            None => Err(GraphError::InvalidElementState {
                reason: "Cannot mutate shared Arc<TraversalGraph>".to_string(),
            }),
        }
    }

    #[inline]
    fn last_commit_seq(&self) -> u64 {
        GraphStorage::last_commit_seq(self.as_ref())
    }

    fn unit_started_at(&self) -> Option<i128> {
        GraphStorage::unit_started_at(self.as_ref())
    }

    #[inline]
    fn raise_id_sequences(
        &mut self,
        node_sequence: u64,
        edge_sequence: u64,
    ) -> Result<(), GraphError> {
        // Like every other write: only the sole owner can change the graph.
        match Arc::get_mut(self) {
            Some(graph) => GraphStorage::raise_id_sequences(graph, node_sequence, edge_sequence),
            None => Err(GraphError::InvalidElementState {
                reason: "Cannot mutate shared Arc<TraversalGraph>".to_string(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_generic_storage<S: GraphStorage>(storage: &S) {
        assert_eq!(storage.vertex_count(), 2);
        assert_eq!(storage.edge_count(), 1);

        let v1 = storage.get_vertex_by_id("v1").expect("v1 should exist");
        let v2 = storage.get_vertex_by_id("v2").expect("v2 should exist");

        assert_eq!(storage.vertex_label(v1), Some("person"));
        assert_eq!(storage.vertex_label(v2), Some("person"));

        let e1 = storage.get_edge_by_id("e1").expect("e1 should exist");
        assert_eq!(storage.edge_label(e1), Some("knows"));
        assert_eq!(storage.edge_endpoints(e1), Some((v1, v2)));

        let out_e: Vec<_> = storage.out_edges(v1).collect();
        assert_eq!(out_e, vec![e1]);

        let in_e: Vec<_> = storage.in_edges(v2).collect();
        assert_eq!(in_e, vec![e1]);
    }

    fn build_test_graph() -> TraversalGraph {
        let mut tg = TraversalGraph::new();
        let v1 = tg
            .insert_vertex(
                Some("v1".into()),
                vec!["person".into()],
                PropertyMap::default(),
            )
            .unwrap();
        let v2 = tg
            .insert_vertex(
                Some("v2".into()),
                vec!["person".into()],
                PropertyMap::default(),
            )
            .unwrap();
        GraphStorage::insert_edge(
            &mut tg,
            Some("e1".into()),
            Some("knows".into()),
            v1,
            v2,
            PropertyMap::default(),
        )
        .unwrap();
        tg
    }

    #[test]
    fn test_traversal_graph_storage() {
        let mut graph = build_test_graph();
        test_generic_storage(&graph);

        let v3 = graph
            .insert_vertex(
                Some("v3".into()),
                vec!["software".into()],
                PropertyMap::default(),
            )
            .unwrap();
        assert_eq!(graph.vertex_count(), 3);
        assert_eq!(graph.vertex_label(v3), Some("software"));
    }

    #[test]
    fn test_ref_traversal_graph_storage() {
        let graph = build_test_graph();
        let ref_graph: &TraversalGraph = &graph;
        test_generic_storage(&ref_graph);
    }

    #[test]
    fn test_arc_traversal_graph_storage() {
        let graph = build_test_graph();
        let arc_graph = Arc::new(graph);
        test_generic_storage(&arc_graph);
    }

    #[test]
    fn test_label_indexing_and_drop_lifecycle() {
        let mut tg = TraversalGraph::new();

        let v1 = tg
            .insert_vertex(
                Some("v1".into()),
                vec!["person".into()],
                PropertyMap::default(),
            )
            .unwrap();
        let v2 = tg
            .insert_vertex(
                Some("v2".into()),
                vec!["person".into()],
                PropertyMap::default(),
            )
            .unwrap();
        let v3 = tg
            .insert_vertex(
                Some("v3".into()),
                vec!["software".into()],
                PropertyMap::default(),
            )
            .unwrap();

        let e1 = tg
            .insert_edge(
                Some("e1".into()),
                Some("knows".into()),
                v1,
                v2,
                PropertyMap::default(),
            )
            .unwrap();
        let e2 = tg
            .insert_edge(
                Some("e2".into()),
                Some("created".into()),
                v1,
                v3,
                PropertyMap::default(),
            )
            .unwrap();

        // Check TraversalGraph direct implementation
        let persons: Vec<_> = GraphStorage::vertices_by_label(&tg, "person").collect();
        assert_eq!(persons, vec![v1, v2]);
        let softwares: Vec<_> = GraphStorage::vertices_by_label(&tg, "software").collect();
        assert_eq!(softwares, vec![v3]);
        let unknown_v: Vec<_> = GraphStorage::vertices_by_label(&tg, "unknown").collect();
        assert!(unknown_v.is_empty());

        let knows_edges: Vec<_> = GraphStorage::edges_by_label(&tg, "knows").collect();
        assert_eq!(knows_edges, vec![e1]);
        let created_edges: Vec<_> = GraphStorage::edges_by_label(&tg, "created").collect();
        assert_eq!(created_edges, vec![e2]);
        let unknown_e: Vec<_> = GraphStorage::edges_by_label(&tg, "unknown").collect();
        assert!(unknown_e.is_empty());

        let stats = tg.statistics().expect("TraversalGraph keeps statistics");
        assert_eq!(stats.vertex_label_count("person"), 2);
        assert_eq!(stats.vertex_label_count("software"), 1);
        assert_eq!(stats.edge_label_count("knows"), 1);
        assert_eq!(stats.edge_label_count("created"), 1);
        assert_eq!(stats, &GraphStatistics::from_storage(&tg));

        // Check forwarding through &TraversalGraph and Box
        {
            let ref_tg: &TraversalGraph = &tg;
            assert_eq!(
                ref_tg.vertices_by_label("person").collect::<Vec<_>>(),
                vec![v1, v2]
            );
            assert_eq!(ref_tg.edges_by_label("knows").collect::<Vec<_>>(), vec![e1]);

            let boxed: Box<&TraversalGraph> = Box::new(ref_tg);
            assert_eq!(
                boxed.vertices_by_label("person").collect::<Vec<_>>(),
                vec![v1, v2]
            );
            assert_eq!(boxed.edges_by_label("knows").collect::<Vec<_>>(), vec![e1]);
        }

        // Test drop edge and label index removal
        tg.drop_edge(e1).unwrap();
        let knows_after_drop: Vec<_> = GraphStorage::edges_by_label(&tg, "knows").collect();
        assert!(knows_after_drop.is_empty());
        assert_eq!(
            GraphStorage::edges_by_label(&tg, "created").collect::<Vec<_>>(),
            vec![e2]
        );

        // Test drop vertex and label index removal
        tg.drop_vertex(v2).unwrap();
        let persons_after_drop: Vec<_> = GraphStorage::vertices_by_label(&tg, "person").collect();
        assert_eq!(persons_after_drop, vec![v1]);

        tg.drop_vertex(v1).unwrap();
        let persons_empty: Vec<_> = GraphStorage::vertices_by_label(&tg, "person").collect();
        assert!(persons_empty.is_empty());
    }

    // A mock storage to explicitly test default trait methods
    struct MockStorage {
        tg: TraversalGraph,
    }

    impl GraphStorage for MockStorage {
        type VertexId = VertexHandle;
        type EdgeId = EdgeHandle;

        fn vertex_count(&self) -> usize {
            self.tg.vertex_count()
        }
        fn edge_count(&self) -> usize {
            self.tg.edge_count()
        }
        fn vertices(&self) -> impl Iterator<Item = VertexHandle> + '_ {
            GraphStorage::vertices(&self.tg)
        }
        fn edges(&self) -> impl Iterator<Item = EdgeHandle> + '_ {
            GraphStorage::edges(&self.tg)
        }
        fn get_vertex_by_id(&self, id: &str) -> Option<VertexHandle> {
            GraphStorage::get_vertex_by_id(&self.tg, id)
        }
        fn get_edge_by_id(&self, id: &str) -> Option<EdgeHandle> {
            GraphStorage::get_edge_by_id(&self.tg, id)
        }
        fn out_edges(&self, node: VertexHandle) -> impl Iterator<Item = EdgeHandle> + '_ {
            GraphStorage::out_edges(&self.tg, node)
        }
        fn in_edges(&self, node: VertexHandle) -> impl Iterator<Item = EdgeHandle> + '_ {
            GraphStorage::in_edges(&self.tg, node)
        }
        fn vertex_id(&self, node: VertexHandle) -> Option<&str> {
            self.tg.vertex_id(node)
        }
        fn edge_id(&self, edge: EdgeHandle) -> Option<&str> {
            self.tg.edge_id(edge)
        }
        fn vertex_label(&self, node: VertexHandle) -> Option<&str> {
            self.tg.vertex_label(node)
        }
        fn edge_label(&self, edge: EdgeHandle) -> Option<&str> {
            self.tg.edge_label(edge)
        }
        fn vertex_labels(&self, node: VertexHandle) -> &[CompactString] {
            self.tg.vertex_labels(node)
        }
        fn vertex_properties(&self, node: VertexHandle) -> Option<&PropertyMap> {
            self.tg.vertex_properties(node)
        }
        fn edge_properties(&self, edge: EdgeHandle) -> Option<&PropertyMap> {
            self.tg.edge_properties(edge)
        }
        fn edge_endpoints(&self, edge: EdgeHandle) -> Option<(VertexHandle, VertexHandle)> {
            self.tg.edge_endpoints(edge)
        }
        fn insert_vertex(
            &mut self,
            id: Option<CompactString>,
            labels: Vec<CompactString>,
            properties: PropertyMap,
        ) -> Result<VertexHandle, GraphError> {
            self.tg.insert_vertex(id, labels, properties)
        }
        fn add_vertex_label(
            &mut self,
            node: VertexHandle,
            label: CompactString,
        ) -> Result<bool, GraphError> {
            self.tg.add_vertex_label(node, label)
        }
        fn remove_vertex_label(
            &mut self,
            node: VertexHandle,
            label: &str,
        ) -> Result<bool, GraphError> {
            self.tg.remove_vertex_label(node, label)
        }
        fn insert_edge(
            &mut self,
            id: Option<CompactString>,
            label: Option<CompactString>,
            from: VertexHandle,
            to: VertexHandle,
            properties: PropertyMap,
        ) -> Result<EdgeHandle, GraphError> {
            self.tg.insert_edge(id, label, from, to, properties)
        }
        fn set_vertex_property(
            &mut self,
            node: VertexHandle,
            key: CompactString,
            value: ElementProperty,
        ) -> Result<(), GraphError> {
            self.tg.set_vertex_property(node, key, value)
        }
        fn set_edge_property(
            &mut self,
            edge: EdgeHandle,
            key: CompactString,
            value: ElementProperty,
        ) -> Result<(), GraphError> {
            self.tg.set_edge_property(edge, key, value)
        }
        fn drop_vertex(&mut self, node: VertexHandle) -> Result<(), GraphError> {
            self.tg.drop_vertex(node)
        }
        fn drop_vertex_property(
            &mut self,
            prop: &str,
            node: VertexHandle,
        ) -> Result<(), GraphError> {
            self.tg.drop_vertex_property(prop, node)
        }
        fn drop_edge(&mut self, edge: EdgeHandle) -> Result<(), GraphError> {
            self.tg.drop_edge(edge)
        }
        fn drop_edge_property(&mut self, prop: &str, edge: EdgeHandle) -> Result<(), GraphError> {
            self.tg.drop_edge_property(prop, edge)
        }

        /// What it really does: it writes, but keeps the default (no-op) units, so it is
        /// neither atomic nor capturing.
        fn capabilities(&self) -> StorageCapabilities {
            StorageCapabilities::none().with_writable(true)
        }
    }

    #[test]
    fn mock_storage_defaults_reject_hooks_and_count_no_commits() {
        let mut mock = MockStorage {
            tg: build_test_graph(),
        };
        assert!(mock.capabilities().is_consistent());
        assert!(!mock.capabilities().atomic);
        struct Nop;
        impl crate::graph::CommitHook for Nop {}
        assert!(matches!(
            mock.register_commit_hook(Box::new(Nop)),
            Err(GraphError::Unsupported { .. })
        ));
        let mark = mock.begin_unit();
        mock.insert_vertex(Some("m".into()), vec![], PropertyMap::default())
            .expect("add");
        mock.commit_unit(mark).expect("commit");
        assert_eq!(GraphStorage::last_commit_seq(&mock), 0);
        // The catalog defaults: nothing stored, writes unsupported.
        let query = Definition::query("q", crate::graph::QueryDefinition::new("fn q() { 1 }"));
        assert!(matches!(
            mock.set_definition(query),
            Err(GraphError::Unsupported { .. })
        ));
        assert!(matches!(
            mock.remove_definition(DefinitionKind::QUERY, "q"),
            Err(GraphError::Unsupported { .. })
        ));
        assert_eq!(mock.definitions().count(), 0);
        assert!(mock.definition(DefinitionKind::QUERY, "q").is_none());
    }

    #[test]
    fn traversal_graph_capabilities_are_consistent() {
        let graph = TraversalGraph::new();
        let caps = GraphStorage::capabilities(&graph);
        assert!(caps.atomic && caps.change_capture && caps.writable && !caps.immutable);
        assert!(caps.is_consistent());
        assert_eq!(
            GraphStorage::capabilities(&&graph),
            StorageCapabilities::none()
        );
        let shared = Arc::new(TraversalGraph::new());
        assert_eq!(GraphStorage::capabilities(&shared), caps);
        let second = shared.clone();
        assert_eq!(
            GraphStorage::capabilities(&shared),
            StorageCapabilities::none()
        );
        drop(second);
    }

    #[test]
    fn test_default_trait_label_filtering() {
        let tg = build_test_graph();
        let mock = MockStorage { tg };

        // Test default vertices_by_label implementation
        let persons: Vec<_> = mock.vertices_by_label("person").collect();
        assert_eq!(persons.len(), 2);

        let unknown: Vec<_> = mock.vertices_by_label("non_existent").collect();
        assert!(unknown.is_empty());

        // Test default edges_by_label implementation
        let knows: Vec<_> = mock.edges_by_label("knows").collect();
        assert_eq!(knows.len(), 1);

        let unknown_edges: Vec<_> = mock.edges_by_label("non_existent").collect();
        assert!(unknown_edges.is_empty());

        // A storage without its own bookkeeping reports no statistics; they can be computed.
        assert!(mock.statistics().is_none());
        let stats = GraphStatistics::from_storage(&mock);
        assert_eq!(stats.vertex_label_count("person"), 2);
        assert_eq!(stats.edge_label_count("knows"), 1);
    }

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn test_send_sync_bounds() {
        assert_send_sync::<TraversalGraph>();
        assert_send_sync::<Arc<TraversalGraph>>();
    }
}
