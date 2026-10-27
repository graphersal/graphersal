//! The session over ANY storage (`book/src/for_developers/custom_storage.md`): the
//! object-safe boundary between [`Session`](crate::Session) and the storage it serves.
//!
//! The session never names a storage type. It holds
//!
//! - its graph as `Arc<dyn` [`SessionGraph`]`>`: a [`AnyGraph`] (the script layer's boundary,
//!   reached by trait upcasting) plus one method per view, selection, edit, creation, saved
//!   query, compression and index operation. Each method locks the graph once and runs the
//!   generic helper of its module (`views`, `selection`, `edit`, `create`, `queries`,
//!   `compression`, `indexes`) monomorphised for the storage, so the cost of the boundary is one indirect call per
//!   operation, never one per element. It is blanket-implemented for every
//!   [`Graph<S>`](Graph) with `S: GraphStorage + Default + Send + Sync + 'static`;
//! - the host's choice of storage as `Arc<dyn` [`GraphFactory`]`>`: how a loaded graph (a
//!   sample, a GraphML or GraphSON file, a `.gsnap` snapshot) becomes a graph of that storage,
//!   and how a Store of it is created and opened. [`StorageKind<S>`](StorageKind) is the one
//!   implementation: [`StorageKind::new`] for any storage (graphs only),
//!   [`StorageKind::persistent`] for a [`PersistentStorage`] (graphs, `.gsnap` and Stores);
//! - its stores as `Arc<dyn` [`SessionStore`]`>`, blanket-implemented for every
//!   [`Store<S>`](Store): what the Store menu calls.
//!
//! A host serves its own storage with
//! [`Session::with_storage`](crate::Session::with_storage)`(Arc::new(StorageKind::<MyStorage>::persistent()))`
//! (or `StorageKind::new()` for a storage without persistence) and puts a graph it already holds
//! in front of the session with [`Session::load_shared`](crate::Session::load_shared).
//! [`Session::new`](crate::Session::new) serves [`TraversalGraph`], as the playground and the
//! dev server do.

use std::any::Any;
use std::io::{Read, Write};
use std::marker::PhantomData;
use std::sync::Arc;

use graphersal::catalog::{Definition, DefinitionKind, QueryDefinition};
use graphersal::changes::{CommitHook, Mark, RollbackReason};
use graphersal::error::format_diagnostic;
use graphersal::exec::CancelToken;
use graphersal::persist::{
    BackupMarker, BackupMode, BackupReport, ChangesSince, CompactReport, CompactionAdvice,
    DamagePolicy, DamageReport, ForkReport, FoundDamage, PersistError, PruneReport, ReadOptions,
    RecoveryTarget, RollbackMode, RollbackReport, SnapshotInfo, Store, StoreDir, StoreInfo,
    StoreOptions, VerifyReport,
};
use graphersal::prelude::{ElementProperty, GraphSchema, PropertyMap};
use graphersal::storage::{
    AnyGraph, ElementKind, Graph, GraphError, GraphStorage, PersistentStorage, TraversalGraph,
    UnitChanges, UnitMark,
};
use serde_json::Value;

use crate::display::{CompactGraph, InspectedElement};
use crate::error::SessionError;
use crate::queries::FieldError;
use crate::selection::{MetaGraph, NeighbourhoodRequest, SampleRequest, SelectionView};

/// The text of an operation the session's storage does not support (`what`: "packed snapshots
/// (.gsnap)", "stores", ...).
pub fn unsupported_text(what: &str, storage: &str) -> String {
    format!(
        "This graph storage does not support {what}: the session serves {storage}, which is \
         not a PersistentStorage (the host builds the session with \
         StorageKind::persistent() for a storage that is one)"
    )
}

// ------------------------------------------------------------------------------- SessionGraph

/// The graph a [`Session`](crate::Session) serves, of any storage, as one object-safe type
/// (`Arc<dyn SessionGraph>`). It is an [`AnyGraph`] (trait upcasting gives the script layer its
/// `Arc<dyn AnyGraph>`) with one method per session operation; each locks the graph for that
/// operation only and returns storage-independent data (JSON, the drawing's columns, field
/// errors).
///
/// Blanket-implemented for every [`Graph<S>`](Graph) with `S: GraphStorage + Default + Send +
/// Sync + 'static` (the `Default` of [`AnyGraph`]: the session builds new graphs of the served
/// storage). Internal like the rest of this crate: methods are added as the session needs
/// them.
///
/// The methods never panic on the graph's lock and never answer as for no data: on the thread
/// that holds the graph's unit, a view it may not have (a write view alive there, as in a commit
/// hook of that unit; `Graph::read_checked`) is the method's error
/// ([`GraphError::GraphViewHeld`], inside [`SessionError::Graph`] or as a `general` field
/// error). Every method that reads or writes the graph returns a `Result`, also where it cannot
/// fail today.
pub trait SessionGraph: AnyGraph {
    /// The number of vertices.
    fn vertex_count(&self) -> Result<usize, GraphError>;

    /// The number of edges.
    fn edge_count(&self) -> Result<usize, GraphError>;

    /// The schema the stored data satisfies ([`GraphStorage::infer_schema`], mode `none`).
    fn inferred_schema(&self) -> Result<GraphSchema, GraphError>;

    /// [`crate::views::schema_view`] of the graph.
    fn schema_view(&self) -> Result<Value, GraphError>;

    /// [`crate::views::schema_view_of`]: the views of `schema` with the graph's label counts.
    fn schema_view_of(&self, schema: GraphSchema) -> Result<Value, GraphError>;

    /// [`crate::views::graph_view`] of the graph.
    fn graph_view(&self, max_nodes: usize, max_edges: usize) -> Result<CompactGraph, GraphError>;

    /// [`crate::views::graph_view_request`]: the drawing with a filter and degree columns.
    fn graph_view_request(
        &self,
        request: &crate::views::GraphViewRequest,
    ) -> Result<CompactGraph, SessionError>;

    /// [`crate::views::graph_filter_query`]: the Rhai query of a filter.
    fn graph_filter_query(&self, filter: &Value) -> Result<String, SessionError>;

    /// [`CompactGraph::add_degrees`] (a result's subgraph asked with degrees).
    fn add_degrees(&self, view: &mut CompactGraph) -> Result<(), GraphError>;

    /// [`crate::views::statistics_view`] of the graph.
    fn statistics_view(&self) -> Result<Value, GraphError>;

    /// [`crate::views::memory_usage`] of the graph.
    fn memory_view(&self) -> Result<Value, GraphError>;

    /// [`crate::display::element_of`]: one element for the inspector.
    fn element(&self, kind: ElementKind, id: &str) -> Result<Option<InspectedElement>, GraphError>;

    /// [`crate::display::collect_subgraph`]: the elements `results` refer to.
    fn subgraph(
        &self,
        results: &[ElementProperty],
        max_elements: usize,
    ) -> Result<Option<CompactGraph>, GraphError>;

    /// [`crate::selection::neighbourhood`].
    fn neighbourhood(
        &self,
        request: &NeighbourhoodRequest,
        cancel: &CancelToken,
    ) -> Result<SelectionView, SessionError>;

    /// [`crate::selection::label_sample`].
    fn label_sample(
        &self,
        request: &SampleRequest,
        cancel: &CancelToken,
    ) -> Result<SelectionView, SessionError>;

    /// [`crate::selection::meta_graph`].
    fn meta_graph(&self, cancel: &CancelToken) -> Result<MetaGraph, SessionError>;

    /// [`crate::edit::element_view`]: the property editor's view of one element.
    fn element_view(&self, kind: ElementKind, id: &str) -> Result<Value, GraphError>;

    /// The writes of an update request (`set`, `remove`; [`crate::edit`]) to the element `id`
    /// of `kind` as ONE unit (`validate_only`: a dry run): `Ok(changed)` or the field errors.
    fn update_element(
        &self,
        kind: ElementKind,
        id: &str,
        request: &Value,
        validate_only: bool,
    ) -> Result<bool, Vec<FieldError>>;

    /// [`crate::create::label_choices_in`].
    fn label_choices(&self, request: &Value) -> Result<Value, SessionError>;

    /// A creation request of `kind` ([`crate::create`]) as ONE unit (`validate_only`: a dry
    /// run): `Ok(Some(id))` of the stored element, `Ok(None)` for a dry run, or the field
    /// errors.
    fn create_element(
        &self,
        kind: ElementKind,
        request: &Value,
        validate_only: bool,
    ) -> Result<Option<String>, Vec<FieldError>>;

    /// [`crate::create::drawn_in`].
    fn drawn(&self, kind: ElementKind, id: &str) -> Result<Value, GraphError>;

    /// [`crate::create::incident_edges`].
    fn incident_edges(&self, kind: ElementKind, id: &str) -> Result<Option<usize>, GraphError>;

    /// [`crate::create::delete_in`].
    fn delete_element(
        &self,
        kind: ElementKind,
        id: &str,
        validate_only: bool,
    ) -> Result<bool, GraphError>;

    /// [`crate::queries::queries_in`].
    fn queries(&self, folder: Option<&str>) -> Result<Vec<(String, QueryDefinition)>, GraphError>;

    /// [`crate::queries::query_in`].
    fn query(&self, name: &str) -> Result<Option<QueryDefinition>, GraphError>;

    /// [`crate::queries::count_kind`].
    fn count_kind(&self, kind: DefinitionKind) -> Result<usize, GraphError>;

    /// Stores a checked saved query with the editor's name rules, in ONE commit.
    fn store_query(
        &self,
        definition: Definition,
        is_edit: bool,
        replaces: Option<&str>,
    ) -> Result<(), Vec<FieldError>>;

    /// [`crate::queries::remove_definition_in`].
    fn drop_definition(&self, kind: DefinitionKind, name: &str) -> Result<bool, SessionError>;

    /// [`crate::queries::move_query_in`].
    fn move_query_into(&self, name: &str, folder: Option<&str>) -> Result<(), SessionError>;

    /// [`crate::queries::catalog_text`].
    fn catalog_text(&self) -> Result<String, GraphError>;

    /// [`crate::queries::load_catalog_in`].
    fn load_catalog(&self, definitions: Vec<Definition>, replace: bool)
    -> Result<(), SessionError>;

    /// Copies `catalog` into the graph in one commit (a graph loaded from a file that carries
    /// no catalog).
    fn carry_catalog(&self, catalog: Vec<Definition>) -> Result<(), SessionError>;

    /// [`crate::compression::compressions_in`].
    fn compressions(&self) -> Result<Value, GraphError>;

    /// [`crate::compression::compression_in`].
    fn compression(&self, name: &str) -> Result<Value, GraphError>;

    /// Stores a checked compression rule with the form's name rules, in ONE commit.
    fn store_compression(
        &self,
        definition: Definition,
        is_edit: bool,
        replaces: Option<&str>,
    ) -> Result<(), Vec<FieldError>>;

    /// [`crate::compression::recompress_in`]; a storage without compressed properties fails
    /// with its `Unsupported` diagnostic.
    fn recompress_rule(&self, name: &str) -> Result<(), SessionError>;

    /// [`crate::compression::estimate_compression_in`] under the read lock; a storage without
    /// compressed properties fails with its `Unsupported` diagnostic.
    fn estimate_compression_json(
        &self,
        request: &graphersal::catalog::CompressionEstimateRequest,
    ) -> Result<Value, SessionError>;

    /// [`crate::indexes::indexes_in`].
    fn indexes(&self) -> Result<Value, GraphError>;

    /// [`crate::indexes::index_in`].
    fn index(&self, name: &str) -> Result<Value, GraphError>;

    /// Stores a checked property index (a free name), in ONE commit; a storage without
    /// property indexes refuses it with its `Unsupported` diagnostic.
    fn store_index(&self, definition: Definition) -> Result<(), Vec<FieldError>>;

    /// Opens the unit of a whole run (`Graph::begin_unit`: held by this thread across the
    /// run's locks); a refusal of the graph (a view of it alive on this thread) as its error.
    fn begin_run(&self) -> Result<UnitMark, GraphError>;

    /// What the run's unit changed so far ([`GraphStorage::unit_changes`]).
    fn run_changes(&self, mark: UnitMark) -> Result<UnitChanges, GraphError>;

    /// Commits the run's unit ([`GraphStorage::commit_unit`]).
    fn commit_run(&self, mark: UnitMark) -> Result<(), GraphError>;

    /// Rolls the run's unit back ([`GraphStorage::rollback_unit`]).
    fn rollback_run(&self, mark: UnitMark, reason: &RollbackReason) -> Result<(), GraphError>;

    /// Registers a commit hook ([`GraphStorage::register_commit_hook`]): a read-only view's
    /// refusal.
    fn register_hook(&self, hook: Box<dyn CommitHook>) -> Result<(), GraphError>;
}

/// A read of `graph` for a [`SessionGraph`] method: `answer` on the read view, or the refusal
/// when the current thread may not have one ([`Graph::read_checked`]: it holds the graph's unit
/// and a write view of it is alive, as in a commit hook of that unit); never a panic.
fn reading<S: GraphStorage, T>(
    graph: &Graph<S>,
    answer: impl FnOnce(&S) -> T,
) -> Result<T, GraphError> {
    Ok(answer(&*graph.read_checked()?))
}

/// [`reading`] for an answer that can fail itself.
fn reading_with<S: GraphStorage, T, E: From<GraphError>>(
    graph: &Graph<S>,
    answer: impl FnOnce(&S) -> Result<T, E>,
) -> Result<T, E> {
    answer(&*graph.read_checked()?)
}

/// A write of `graph` for a [`SessionGraph`] method, like [`reading_with`]
/// ([`Graph::write_checked`]).
fn writing_with<S: GraphStorage, T, E: From<GraphError>>(
    graph: &Graph<S>,
    change: impl FnOnce(&mut S) -> Result<T, E>,
) -> Result<T, E> {
    change(&mut *graph.write_checked()?)
}

/// A write of `graph` whose refusal is a `general` field error.
fn writing_fields<S: GraphStorage, T>(
    graph: &Graph<S>,
    change: impl FnOnce(&mut S) -> Result<T, Vec<FieldError>>,
) -> Result<T, Vec<FieldError>> {
    match graph.write_checked() {
        Ok(mut guard) => change(&mut guard),
        Err(err) => Err(field_errors(err)),
    }
}

/// The field errors of a refused view (field `general`).
fn field_errors(err: GraphError) -> Vec<FieldError> {
    vec![FieldError::new("general", err.to_string())]
}

impl<S> SessionGraph for Graph<S>
where
    S: GraphStorage + Default + Send + Sync + 'static,
{
    fn vertex_count(&self) -> Result<usize, GraphError> {
        reading(self, |g| g.vertex_count())
    }

    fn edge_count(&self) -> Result<usize, GraphError> {
        reading(self, |g| g.edge_count())
    }

    fn inferred_schema(&self) -> Result<GraphSchema, GraphError> {
        reading_with(self, |g| g.infer_schema())
    }

    fn schema_view(&self) -> Result<Value, GraphError> {
        reading_with(self, crate::views::schema_view)
    }

    fn schema_view_of(&self, schema: GraphSchema) -> Result<Value, GraphError> {
        reading_with(self, |g| crate::views::schema_view_of(g, schema))
    }

    fn graph_view(&self, max_nodes: usize, max_edges: usize) -> Result<CompactGraph, GraphError> {
        reading(self, |g| crate::views::graph_view(g, max_nodes, max_edges))
    }

    fn graph_view_request(
        &self,
        request: &crate::views::GraphViewRequest,
    ) -> Result<CompactGraph, SessionError> {
        reading_with(self, |g| crate::views::graph_view_request(g, request))
    }

    fn graph_filter_query(&self, filter: &Value) -> Result<String, SessionError> {
        reading_with(self, |g| crate::views::graph_filter_query(g, filter))
    }

    fn add_degrees(&self, view: &mut CompactGraph) -> Result<(), GraphError> {
        reading(self, |g| view.add_degrees(g))
    }

    fn statistics_view(&self) -> Result<Value, GraphError> {
        reading_with(self, crate::views::statistics_view)
    }

    fn memory_view(&self) -> Result<Value, GraphError> {
        let mut view = reading_with(self, crate::views::memory_usage)?;
        if let (Some(stats), Some(map)) = (Graph::workload_stats(self), view.as_object_mut()) {
            map.insert("workload_bytes".into(), stats.memory_bytes().into());
        }
        Ok(view)
    }

    fn element(&self, kind: ElementKind, id: &str) -> Result<Option<InspectedElement>, GraphError> {
        reading(self, |g| crate::display::element_of(g, kind, id))
    }

    fn subgraph(
        &self,
        results: &[ElementProperty],
        max_elements: usize,
    ) -> Result<Option<CompactGraph>, GraphError> {
        reading(self, |g| {
            crate::display::collect_subgraph(g, results, max_elements)
        })
    }

    fn neighbourhood(
        &self,
        request: &NeighbourhoodRequest,
        cancel: &CancelToken,
    ) -> Result<SelectionView, SessionError> {
        reading_with(self, |g| {
            crate::selection::neighbourhood(g, request, cancel)
        })
    }

    fn label_sample(
        &self,
        request: &SampleRequest,
        cancel: &CancelToken,
    ) -> Result<SelectionView, SessionError> {
        reading_with(self, |g| crate::selection::label_sample(g, request, cancel))
    }

    fn meta_graph(&self, cancel: &CancelToken) -> Result<MetaGraph, SessionError> {
        reading_with(self, |g| crate::selection::meta_graph(g, cancel))
    }

    fn element_view(&self, kind: ElementKind, id: &str) -> Result<Value, GraphError> {
        reading(self, |g| crate::edit::element_view(g, kind, id))
    }

    fn update_element(
        &self,
        kind: ElementKind,
        id: &str,
        request: &Value,
        validate_only: bool,
    ) -> Result<bool, Vec<FieldError>> {
        writing_fields(self, |g| {
            crate::edit::update_request_in(g, kind, id, request, validate_only)
        })
    }

    fn label_choices(&self, request: &Value) -> Result<Value, SessionError> {
        reading_with(self, |g| crate::create::label_choices_in(g, request))
    }

    fn create_element(
        &self,
        kind: ElementKind,
        request: &Value,
        validate_only: bool,
    ) -> Result<Option<String>, Vec<FieldError>> {
        writing_fields(self, |g| {
            crate::create::create_request_in(g, kind, request, validate_only)
        })
    }

    fn drawn(&self, kind: ElementKind, id: &str) -> Result<Value, GraphError> {
        reading(self, |g| crate::create::drawn_in(g, kind, id))
    }

    fn incident_edges(&self, kind: ElementKind, id: &str) -> Result<Option<usize>, GraphError> {
        reading(self, |g| crate::create::incident_edges(g, kind, id))
    }

    fn delete_element(
        &self,
        kind: ElementKind,
        id: &str,
        validate_only: bool,
    ) -> Result<bool, GraphError> {
        writing_with(self, |g| {
            crate::create::delete_in(g, kind, id, validate_only)
        })
    }

    fn queries(&self, folder: Option<&str>) -> Result<Vec<(String, QueryDefinition)>, GraphError> {
        reading_with(self, |g| crate::queries::queries_in(g, folder))
    }

    fn query(&self, name: &str) -> Result<Option<QueryDefinition>, GraphError> {
        reading_with(self, |g| crate::queries::query_in(g, name))
    }

    fn count_kind(&self, kind: DefinitionKind) -> Result<usize, GraphError> {
        reading_with(self, |g| crate::queries::count_kind(g, kind))
    }

    fn store_query(
        &self,
        definition: Definition,
        is_edit: bool,
        replaces: Option<&str>,
    ) -> Result<(), Vec<FieldError>> {
        writing_fields(self, |g| {
            crate::queries::store_query(g, definition, is_edit, replaces)
        })
    }

    fn drop_definition(&self, kind: DefinitionKind, name: &str) -> Result<bool, SessionError> {
        writing_with(self, |g| {
            crate::queries::remove_definition_in(g, kind, name)
        })
    }

    fn move_query_into(&self, name: &str, folder: Option<&str>) -> Result<(), SessionError> {
        writing_with(self, |g| crate::queries::move_query_in(g, name, folder))
    }

    fn catalog_text(&self) -> Result<String, GraphError> {
        reading_with(self, crate::queries::catalog_text)
    }

    fn load_catalog(
        &self,
        definitions: Vec<Definition>,
        replace: bool,
    ) -> Result<(), SessionError> {
        writing_with(self, |g| {
            crate::queries::load_catalog_in(g, definitions, replace)
        })
    }

    fn carry_catalog(&self, catalog: Vec<Definition>) -> Result<(), SessionError> {
        writing_with(self, |g| crate::queries::carry_catalog(g, catalog))
    }

    fn compressions(&self) -> Result<Value, GraphError> {
        reading_with(self, crate::compression::compressions_in)
    }

    fn compression(&self, name: &str) -> Result<Value, GraphError> {
        reading_with(self, |g| crate::compression::compression_in(g, name))
    }

    fn store_compression(
        &self,
        definition: Definition,
        is_edit: bool,
        replaces: Option<&str>,
    ) -> Result<(), Vec<FieldError>> {
        writing_fields(self, |g| {
            crate::compression::store_compression(g, definition, is_edit, replaces)
        })
    }

    fn recompress_rule(&self, name: &str) -> Result<(), SessionError> {
        writing_with(self, |g| crate::compression::recompress_in(g, name))
    }

    fn estimate_compression_json(
        &self,
        request: &graphersal::catalog::CompressionEstimateRequest,
    ) -> Result<Value, SessionError> {
        reading_with(self, |g| {
            crate::compression::estimate_compression_in(g, request)
        })
    }

    fn indexes(&self) -> Result<Value, GraphError> {
        reading_with(self, crate::indexes::indexes_in)
    }

    fn index(&self, name: &str) -> Result<Value, GraphError> {
        reading_with(self, |g| crate::indexes::index_in(g, name))
    }

    fn store_index(&self, definition: Definition) -> Result<(), Vec<FieldError>> {
        writing_fields(self, |g| crate::indexes::store_index(g, definition))
    }

    fn begin_run(&self) -> Result<UnitMark, GraphError> {
        // Held by this thread across the run's locks (`Graph::begin_unit`): other threads (a
        // Store's checkpoint) never see the run's uncommitted changes.
        Graph::begin_unit(self)
    }

    fn run_changes(&self, mark: UnitMark) -> Result<UnitChanges, GraphError> {
        // `UnitNotHeld` when called on another thread than `begin_run`: the caller's error.
        Graph::unit_changes(self, mark)
    }

    fn commit_run(&self, mark: UnitMark) -> Result<(), GraphError> {
        Graph::commit_unit(self, mark)
    }

    fn rollback_run(&self, mark: UnitMark, reason: &RollbackReason) -> Result<(), GraphError> {
        // `UnitNotHeld` when called on another thread than `begin_run`: the caller's error.
        Graph::rollback_unit(self, mark, reason)
    }

    fn register_hook(&self, hook: Box<dyn CommitHook>) -> Result<(), GraphError> {
        writing_with(self, |g| g.register_commit_hook(hook))
    }
}

/// What [`Session::load_shared`](crate::Session::load_shared) accepts: an `Arc<Graph<S>>` of
/// any served storage (a Store's graph, a graph the host keeps), or an `Arc<dyn SessionGraph>`.
pub trait IntoSessionGraph {
    /// The graph as the session holds it.
    fn into_session_graph(self) -> Arc<dyn SessionGraph>;
}

impl<S> IntoSessionGraph for Arc<Graph<S>>
where
    S: GraphStorage + Default + Send + Sync + 'static,
{
    fn into_session_graph(self) -> Arc<dyn SessionGraph> {
        self
    }
}

impl IntoSessionGraph for Arc<dyn SessionGraph> {
    fn into_session_graph(self) -> Arc<dyn SessionGraph> {
        self
    }
}

// ------------------------------------------------------------------------------- GraphFactory

/// The host's storage for the graphs and stores a [`Session`](crate::Session) builds
/// ([`Session::with_storage`](crate::Session::with_storage)): an empty graph, a loaded graph
/// (the session reads every file format into a [`TraversalGraph`] first and hands it over
/// here), a packed snapshot, and Stores. Object-safe; [`StorageKind`] implements it.
pub trait GraphFactory: Send + Sync {
    /// The storage's type name, for messages (not stable).
    fn storage_name(&self) -> &'static str;

    /// A new empty graph.
    fn empty(&self) -> Arc<dyn SessionGraph>;

    /// `graph` (a sample, or what a GraphML/GraphSON load built, with its schema and catalog)
    /// as a graph of this storage: itself when the storage IS [`TraversalGraph`], otherwise a
    /// copy (see [`StorageKind`]). An error as its diagnostic text.
    fn adopt(&self, graph: TraversalGraph) -> Result<Arc<dyn SessionGraph>, SessionError>;

    /// A graph read from a packed snapshot (`.gsnap`): only for a [`PersistentStorage`],
    /// otherwise the "not supported" error ([`unsupported_text`]).
    fn load_snapshot(&self, reader: &mut dyn Read) -> Result<Arc<dyn SessionGraph>, SessionError>;

    /// Whether [`Self::create_store`] and [`Self::open_store`] work (a [`PersistentStorage`]).
    fn supports_stores(&self) -> bool;

    /// A new Store in `dir` whose first snapshot is `graph` (a graph of this factory that
    /// nobody else holds: it is moved into the store). An error as its diagnostic text.
    fn create_store(
        &self,
        dir: Arc<dyn StoreDir>,
        graph: Arc<dyn SessionGraph>,
        options: StoreOptions,
    ) -> Result<Arc<dyn SessionStore>, SessionError>;

    /// Opens the Store in `dir` with this storage. An error as its diagnostic text.
    fn open_store(
        &self,
        dir: Arc<dyn StoreDir>,
        options: StoreOptions,
    ) -> Result<Arc<dyn SessionStore>, SessionError>;
}

/// A store of the session's storage, or why it could not be created or opened.
type StoreResult = Result<Arc<dyn SessionStore>, PersistError>;

/// What only a [`PersistentStorage`] can do, monomorphised by [`StorageKind::persistent`].
struct PersistentOps<S: GraphStorage> {
    read_snapshot: fn(&mut dyn Read) -> Result<S, PersistError>,
    create_store: fn(Arc<dyn StoreDir>, S, StoreOptions) -> StoreResult,
    open_store: fn(Arc<dyn StoreDir>, StoreOptions) -> StoreResult,
}

/// The [`GraphFactory`] of the storage `S` (EXPERIMENTAL, like the session's other boundary
/// types). There is no specialisation, so the constructor says what `S` can do:
///
/// - [`StorageKind::new`] for any `S: GraphStorage + Default`: graphs only. A loaded graph is
///   copied into `S::default()` through the [`GraphStorage`] write API (schema, catalog,
///   vertices, edges in the source's order, outside any unit; the auto-id sequences raised to
///   the source's; the commit position and lineage id are the storage's own). `.gsnap` loads
///   and Stores fail with the "not supported by this storage" error, and the session serves
///   its graphs without a Store (the Store menu says so).
/// - [`StorageKind::persistent`] for `S: PersistentStorage + Default`: everything. A loaded
///   graph is copied losslessly through the storage's load sink
///   ([`PersistentStorage::load_vertex`], ..., in the source's order; lineage id, commit
///   position and time, auto-id sequences carried); graphs are wrapped with
///   [`Graph::new_persistent`] (so "Save graph" as `.gsnap` works); Stores are
///   [`Store<S>`](Store) ([`Store::create_in`], [`Store::open_in`]).
///
/// When `S` is [`TraversalGraph`] nothing is copied: the loaded graph is served as it is
/// ([`crate::Session::new`] uses `StorageKind::<TraversalGraph>::persistent()`).
pub struct StorageKind<S: GraphStorage> {
    wrap: fn(S) -> Graph<S>,
    copy: fn(&TraversalGraph) -> Result<S, GraphError>,
    persistent: Option<PersistentOps<S>>,
    _storage: PhantomData<fn() -> S>,
}

impl<S> StorageKind<S>
where
    S: GraphStorage + Default + Send + Sync + 'static,
{
    /// The factory of a storage without persistence: graphs only (see [`StorageKind`]).
    pub fn new() -> Self {
        Self {
            wrap: Graph::new,
            copy: copy_plain::<S>,
            persistent: None,
            _storage: PhantomData,
        }
    }
}

impl<S> Default for StorageKind<S>
where
    S: GraphStorage + Default + Send + Sync + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<S> StorageKind<S>
where
    S: PersistentStorage + Default,
{
    /// The factory of a persistent storage: graphs, `.gsnap` loads and Stores (see
    /// [`StorageKind`]).
    pub fn persistent() -> Self {
        Self {
            wrap: Graph::new_persistent,
            copy: copy_loaded::<S>,
            persistent: Some(PersistentOps {
                read_snapshot: |reader| {
                    graphersal::persist::read_snapshot_into(
                        reader,
                        &ReadOptions::default(),
                        S::default(),
                    )
                },
                create_store: |dir, graph, options| {
                    Store::<S>::create_in(dir, graph, options)
                        .map(|store| Arc::new(store) as Arc<dyn SessionStore>)
                },
                open_store: |dir, options| {
                    Store::<S>::open_in(dir, options, S::default)
                        .map(|store| Arc::new(store) as Arc<dyn SessionStore>)
                },
            }),
            _storage: PhantomData,
        }
    }
}

impl<S> GraphFactory for StorageKind<S>
where
    S: GraphStorage + Default + Send + Sync + 'static,
{
    fn storage_name(&self) -> &'static str {
        std::any::type_name::<S>()
    }

    fn empty(&self) -> Arc<dyn SessionGraph> {
        Arc::new((self.wrap)(S::default()))
    }

    fn adopt(&self, graph: TraversalGraph) -> Result<Arc<dyn SessionGraph>, SessionError> {
        let boxed: Box<dyn Any> = Box::new(graph);
        let storage = match boxed.downcast::<S>() {
            Ok(same) => *same,
            Err(boxed) => match boxed.downcast::<TraversalGraph>() {
                Ok(graph) => (self.copy)(&graph).map_err(|err| format_diagnostic(&err))?,
                Err(_) => {
                    return Err("internal: a loaded graph of an unknown type"
                        .to_string()
                        .into());
                }
            },
        };
        Ok(Arc::new((self.wrap)(storage)))
    }

    fn load_snapshot(&self, reader: &mut dyn Read) -> Result<Arc<dyn SessionGraph>, SessionError> {
        let Some(ops) = &self.persistent else {
            return Err(unsupported_text("packed snapshots (.gsnap)", self.storage_name()).into());
        };
        let storage = (ops.read_snapshot)(reader).map_err(|err| format_diagnostic(&err))?;
        Ok(Arc::new((self.wrap)(storage)))
    }

    fn supports_stores(&self) -> bool {
        self.persistent.is_some()
    }

    fn create_store(
        &self,
        dir: Arc<dyn StoreDir>,
        graph: Arc<dyn SessionGraph>,
        options: StoreOptions,
    ) -> Result<Arc<dyn SessionStore>, SessionError> {
        let Some(ops) = &self.persistent else {
            return Err(unsupported_text("stores", self.storage_name()).into());
        };
        let any: Arc<dyn AnyGraph> = graph;
        let shared = any
            .into_any_arc()
            .downcast::<Graph<S>>()
            .map_err(|_| "internal: the graph is not of the session's storage".to_string())?;
        let graph = Arc::try_unwrap(shared)
            .map_err(|_| "the graph is shared with another owner".to_string())?
            .into_inner();
        (ops.create_store)(dir, graph, options)
            .map_err(|err| format_diagnostic(&err))
            .map_err(SessionError::from)
    }

    fn open_store(
        &self,
        dir: Arc<dyn StoreDir>,
        options: StoreOptions,
    ) -> Result<Arc<dyn SessionStore>, SessionError> {
        let Some(ops) = &self.persistent else {
            return Err(unsupported_text("stores", self.storage_name()).into());
        };
        (ops.open_store)(dir, options)
            .map_err(|err| format_diagnostic(&err))
            .map_err(SessionError::from)
    }
}

/// The properties of `map` with every value logical (a compressed string decompressed): what
/// leaves a [`TraversalGraph`] for another storage.
fn logical(map: Option<&PropertyMap>) -> PropertyMap {
    map.map(|map| {
        map.iter()
            .map(|(key, value)| (key.clone(), value.to_logical()))
            .collect()
    })
    .unwrap_or_default()
}

/// Copies `src` into a new `S` through the write API (`insert_vertex`/`insert_edge`: stored
/// exactly, no schema defaults), outside any unit: the schema first (the data satisfies it),
/// the catalog, the vertices and edges in `src`'s order, then the auto-id sequences.
fn copy_plain<S: GraphStorage + Default>(src: &TraversalGraph) -> Result<S, GraphError> {
    let mut dst = S::default();
    if let Some(schema) = GraphStorage::get_schema(src)? {
        dst.set_schema(schema)?;
    }
    for definition in GraphStorage::definitions(src)? {
        dst.set_definition(definition.clone())?;
    }
    for v in GraphStorage::vertices(src) {
        let id = GraphStorage::vertex_id(src, v).unwrap_or_default();
        dst.insert_vertex(
            Some(id.into()),
            GraphStorage::vertex_labels(src, v).to_vec(),
            logical(GraphStorage::vertex_properties(src, v)),
        )?;
    }
    for e in GraphStorage::edges(src) {
        let Some((from, to)) = GraphStorage::edge_endpoints(src, e) else {
            continue;
        };
        let endpoint = |v| {
            GraphStorage::vertex_id(src, v)
                .and_then(|id| dst.get_vertex_by_id(id))
                .ok_or_else(|| GraphError::unsupported("an edge whose endpoint was not copied"))
        };
        let (from, to) = (endpoint(from)?, endpoint(to)?);
        dst.insert_edge(
            GraphStorage::edge_id(src, e).map(Into::into),
            GraphStorage::edge_label(src, e).map(Into::into),
            from,
            to,
            logical(GraphStorage::edge_properties(src, e)),
        )?;
    }
    let (vertices, edges) = PersistentStorage::id_sequences(src);
    dst.raise_id_sequences(vertices, edges)?;
    Ok(dst)
}

/// Copies `src` into a new `S` through the load sink, losslessly and in `src`'s order: schema,
/// catalog, vertices, edges, then the lineage id, the commit position and time, the auto-id
/// sequences.
fn copy_loaded<S: PersistentStorage + Default>(src: &TraversalGraph) -> Result<S, GraphError> {
    let mut dst = S::default();
    if let Some(schema) = GraphStorage::get_schema(src)? {
        dst.load_schema(schema)?;
    }
    for definition in GraphStorage::definitions(src)? {
        dst.load_definition(definition.clone())?;
    }
    for v in GraphStorage::vertices(src) {
        dst.load_vertex(
            GraphStorage::vertex_id(src, v).unwrap_or_default().into(),
            GraphStorage::vertex_labels(src, v).to_vec(),
            logical(GraphStorage::vertex_properties(src, v)),
        )?;
    }
    for e in GraphStorage::edges(src) {
        let Some((from, to)) = GraphStorage::edge_endpoints(src, e) else {
            continue;
        };
        dst.load_edge(
            GraphStorage::edge_id(src, e).unwrap_or_default().into(),
            GraphStorage::edge_label(src, e).map(Into::into),
            GraphStorage::vertex_id(src, from).unwrap_or_default(),
            GraphStorage::vertex_id(src, to).unwrap_or_default(),
            logical(GraphStorage::edge_properties(src, e)),
        )?;
    }
    dst.finish_load()?;
    dst.set_graph_id(PersistentStorage::graph_id(src))?;
    dst.raise_position(
        GraphStorage::last_commit_seq(src)?,
        PersistentStorage::last_committed_at(src),
    )?;
    let (vertices, edges) = PersistentStorage::id_sequences(src);
    dst.raise_id_sequences(vertices, edges)?;
    Ok(dst)
}

// ------------------------------------------------------------------------------- SessionStore

/// A read-only view of a past state of a store ([`SessionStore::read_only_view`]).
pub struct StoreView {
    /// The recovered graph (unpublished, without a journal).
    pub graph: Arc<dyn SessionGraph>,
    /// Its commit position.
    pub commit_seq: u64,
    /// The commit time of that position (microseconds since the epoch).
    pub committed_at: i64,
}

/// A Store the session serves, of any storage, as one object-safe type (`Arc<dyn
/// SessionStore>`): what the Store menu and the database list call. Blanket-implemented for
/// every [`Store<S>`](Store); each method forwards to the [`Store`] method of the same name
/// (directories as `Arc<dyn StoreDir>`, writers as `&mut dyn Write`).
pub trait SessionStore: Send + Sync {
    /// The store's live graph ([`Store::graph`]) as the session holds it.
    fn session_graph(&self) -> Arc<dyn SessionGraph>;
    /// [`Store::info`].
    fn info(&self) -> Result<StoreInfo, PersistError>;
    /// [`Store::location`].
    fn location(&self) -> String;
    /// [`Store::dir`].
    fn dir(&self) -> &Arc<dyn StoreDir>;
    /// [`Store::maintenance`].
    fn maintenance(&self) -> Option<&DamageReport>;
    /// [`Store::backup_marker`].
    fn backup_marker(&self) -> Option<BackupMarker>;
    /// [`Store::read_only_reason`].
    fn read_only_reason(&self) -> Result<Option<String>, PersistError>;
    /// [`Store::is_frozen_by_damage`].
    fn is_frozen_by_damage(&self) -> bool;
    /// [`Store::damage_found`].
    fn damage_found(&self) -> Result<Vec<FoundDamage>, PersistError>;
    /// [`Store::damage_policy`].
    fn damage_policy(&self) -> Result<DamagePolicy, PersistError>;
    /// [`Store::wal_bytes_since_checkpoint`].
    fn wal_bytes_since_checkpoint(&self) -> u64;
    /// [`Store::last_auto_checkpoint_error`].
    fn last_auto_checkpoint_error(&self) -> Result<Option<Arc<PersistError>>, PersistError>;
    /// [`Store::run_due_checkpoint`].
    fn run_due_checkpoint(&self) -> Result<Option<SnapshotInfo>, PersistError>;
    /// [`Store::system_jobs`].
    fn system_jobs(&self) -> Vec<graphersal::jobs::Job>;
    /// [`Store::user_jobs`].
    fn user_jobs(&self) -> Result<graphersal::persist::UserJobs, PersistError>;
    /// [`Store::workload_stats`].
    fn workload_stats(&self) -> Arc<graphersal::workload::WorkloadStats>;
    /// [`Store::checkpoint`].
    fn checkpoint(&self, name: Option<&str>) -> Result<SnapshotInfo, PersistError>;
    /// [`Store::mark`].
    fn mark(&self, name: &str) -> Result<Mark, PersistError>;
    /// [`Store::export_packed`].
    fn export_packed(
        &self,
        commit_seq: Option<u64>,
        writer: &mut dyn Write,
    ) -> Result<SnapshotInfo, PersistError>;
    /// [`Store::export_packed_with`].
    fn export_packed_with(
        &self,
        commit_seq: Option<u64>,
        writer: &mut dyn Write,
        models: graphersal::persist::ModelCopy,
    ) -> Result<SnapshotInfo, PersistError>;
    /// [`Store::models`].
    fn models(&self) -> Result<Vec<graphersal::persist::StoredModel>, PersistError>;
    /// [`Store::import_models_from_packed`].
    fn import_models_from_packed(
        &self,
        packed: &mut dyn Read,
    ) -> Result<Vec<graphersal::persist::StoredModel>, PersistError>;
    /// [`Store::read_only_at`], served as a view (the session registers its refusal hook).
    fn read_only_view(&self, target: RecoveryTarget) -> Result<StoreView, PersistError>;
    /// [`Store::fork`].
    fn fork(
        &self,
        target: RecoveryTarget,
        new_dir: Arc<dyn StoreDir>,
    ) -> Result<ForkReport, PersistError>;
    /// [`Store::rollback_to`].
    fn rollback_to(
        &self,
        target: RecoveryTarget,
        mode: RollbackMode,
    ) -> Result<RollbackReport, PersistError>;
    /// [`Store::attic_changes`].
    fn attic_changes(&self, id: &str) -> Result<ChangesSince, PersistError>;
    /// [`Store::attic_can_restore`].
    fn attic_can_restore(&self, id: &str) -> Result<(), PersistError>;
    /// [`Store::attic_restore`].
    fn attic_restore(&self, id: &str) -> Result<u64, PersistError>;
    /// [`Store::attic_fork`].
    fn attic_fork(&self, id: &str, new_dir: Arc<dyn StoreDir>) -> Result<ForkReport, PersistError>;
    /// [`Store::attic_remove`].
    fn attic_remove(&self, id: &str) -> Result<(), PersistError>;
    /// [`Store::compaction_advice`].
    fn compaction_advice(&self) -> Result<CompactionAdvice, PersistError>;
    /// [`Store::compact`].
    fn compact(&self) -> Result<CompactReport, PersistError>;
    /// [`Store::prune`].
    fn prune(&self, up_to: u64) -> Result<PruneReport, PersistError>;
    /// [`Store::backup_with`].
    fn backup_with(
        &self,
        target_dir: Arc<dyn StoreDir>,
        mode: BackupMode,
    ) -> Result<BackupReport, PersistError>;
    /// [`Store::backup_from_memory`].
    fn backup_from_memory(
        &self,
        target_dir: Arc<dyn StoreDir>,
        mode: BackupMode,
    ) -> Result<BackupReport, PersistError>;
    /// [`Store::verify`].
    fn verify(&self) -> Result<VerifyReport, PersistError>;
    /// [`Store::shutdown`].
    fn shutdown(&self) -> Result<(), PersistError>;
}

impl<S> SessionStore for Store<S>
where
    S: PersistentStorage + Default,
{
    fn session_graph(&self) -> Arc<dyn SessionGraph> {
        Store::<S>::graph(self).clone()
    }
    fn info(&self) -> Result<StoreInfo, PersistError> {
        Store::<S>::info(self)
    }
    fn location(&self) -> String {
        Store::<S>::location(self)
    }
    fn dir(&self) -> &Arc<dyn StoreDir> {
        Store::<S>::dir(self)
    }
    fn maintenance(&self) -> Option<&DamageReport> {
        Store::<S>::maintenance(self)
    }
    fn backup_marker(&self) -> Option<BackupMarker> {
        Store::<S>::backup_marker(self)
    }
    fn read_only_reason(&self) -> Result<Option<String>, PersistError> {
        Store::<S>::read_only_reason(self)
    }
    fn is_frozen_by_damage(&self) -> bool {
        Store::<S>::is_frozen_by_damage(self)
    }
    fn damage_found(&self) -> Result<Vec<FoundDamage>, PersistError> {
        Store::<S>::damage_found(self)
    }
    fn damage_policy(&self) -> Result<DamagePolicy, PersistError> {
        Store::<S>::damage_policy(self)
    }
    fn wal_bytes_since_checkpoint(&self) -> u64 {
        Store::<S>::wal_bytes_since_checkpoint(self)
    }
    fn last_auto_checkpoint_error(&self) -> Result<Option<Arc<PersistError>>, PersistError> {
        Store::<S>::last_auto_checkpoint_error(self)
    }
    fn run_due_checkpoint(&self) -> Result<Option<SnapshotInfo>, PersistError> {
        Store::<S>::run_due_checkpoint(self)
    }
    fn system_jobs(&self) -> Vec<graphersal::jobs::Job> {
        Store::<S>::system_jobs(self)
    }
    fn user_jobs(&self) -> Result<graphersal::persist::UserJobs, PersistError> {
        Store::<S>::user_jobs(self)
    }
    fn workload_stats(&self) -> Arc<graphersal::workload::WorkloadStats> {
        Store::<S>::workload_stats(self)
    }
    fn checkpoint(&self, name: Option<&str>) -> Result<SnapshotInfo, PersistError> {
        Store::<S>::checkpoint(self, name)
    }
    fn mark(&self, name: &str) -> Result<Mark, PersistError> {
        Store::<S>::mark(self, name)
    }
    fn export_packed(
        &self,
        commit_seq: Option<u64>,
        writer: &mut dyn Write,
    ) -> Result<SnapshotInfo, PersistError> {
        Store::<S>::export_packed(self, commit_seq, writer)
    }
    fn export_packed_with(
        &self,
        commit_seq: Option<u64>,
        writer: &mut dyn Write,
        models: graphersal::persist::ModelCopy,
    ) -> Result<SnapshotInfo, PersistError> {
        Store::<S>::export_packed_with(self, commit_seq, writer, models)
    }
    fn models(&self) -> Result<Vec<graphersal::persist::StoredModel>, PersistError> {
        Store::<S>::models(self)
    }
    fn import_models_from_packed(
        &self,
        packed: &mut dyn Read,
    ) -> Result<Vec<graphersal::persist::StoredModel>, PersistError> {
        Store::<S>::import_models_from_packed(self, packed)
    }
    fn read_only_view(&self, target: RecoveryTarget) -> Result<StoreView, PersistError> {
        let recovered = Store::<S>::read_only_at(self, target)?;
        let commit_seq = GraphStorage::last_commit_seq(&recovered.graph)?;
        Ok(StoreView {
            graph: Arc::new(Graph::new_persistent(recovered.graph)),
            commit_seq,
            committed_at: recovered.report.committed_at,
        })
    }
    fn fork(
        &self,
        target: RecoveryTarget,
        new_dir: Arc<dyn StoreDir>,
    ) -> Result<ForkReport, PersistError> {
        Store::<S>::fork(self, target, new_dir)
    }
    fn rollback_to(
        &self,
        target: RecoveryTarget,
        mode: RollbackMode,
    ) -> Result<RollbackReport, PersistError> {
        Store::<S>::rollback_to(self, target, mode)
    }
    fn attic_changes(&self, id: &str) -> Result<ChangesSince, PersistError> {
        Store::<S>::attic_changes(self, id)
    }
    fn attic_can_restore(&self, id: &str) -> Result<(), PersistError> {
        Store::<S>::attic_can_restore(self, id)
    }
    fn attic_restore(&self, id: &str) -> Result<u64, PersistError> {
        Store::<S>::attic_restore(self, id)
    }
    fn attic_fork(&self, id: &str, new_dir: Arc<dyn StoreDir>) -> Result<ForkReport, PersistError> {
        Store::<S>::attic_fork(self, id, new_dir)
    }
    fn attic_remove(&self, id: &str) -> Result<(), PersistError> {
        Store::<S>::attic_remove(self, id)
    }
    fn compaction_advice(&self) -> Result<CompactionAdvice, PersistError> {
        Store::<S>::compaction_advice(self)
    }
    fn compact(&self) -> Result<CompactReport, PersistError> {
        Store::<S>::compact(self)
    }
    fn prune(&self, up_to: u64) -> Result<PruneReport, PersistError> {
        Store::<S>::prune(self, up_to)
    }
    fn backup_with(
        &self,
        target_dir: Arc<dyn StoreDir>,
        mode: BackupMode,
    ) -> Result<BackupReport, PersistError> {
        Store::<S>::backup_with(self, target_dir, mode)
    }
    fn backup_from_memory(
        &self,
        target_dir: Arc<dyn StoreDir>,
        mode: BackupMode,
    ) -> Result<BackupReport, PersistError> {
        Store::<S>::backup_from_memory(self, target_dir, mode)
    }
    fn verify(&self) -> Result<VerifyReport, PersistError> {
        Store::<S>::verify(self)
    }
    fn shutdown(&self) -> Result<(), PersistError> {
        Store::<S>::shutdown(self)
    }
}

#[cfg(test)]
mod no_compression;
#[cfg(test)]
mod tests;
