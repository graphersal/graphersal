//! [`AnyGraph`]: the sealed, object-safe boundary the front ends hold
//! (`book/src/for_developers/custom_storage.md`).
//!
//! The engine ([`GraphTraversalSource`]) is generic over the storage and statically dispatched.
//! A front end (the script layer, a session) holds one graph type for every storage instead:
//! `Arc<dyn AnyGraph>`. Each method is one coarse operation (read the schema, store
//! definitions, export the graph, ...): it locks the [`Graph`], builds the engine for `&S` or
//! `&mut S` inside the blanket implementation, and returns storage-independent data. The cost
//! is one indirect call per operation, never one per element.

use std::sync::Arc;

use crate::error::{GraphError, TraverserResult};
use crate::graph::compression::CompressionStats;
use crate::graph::definition::{Definition, DefinitionKind};
use crate::graph::hooks::Mark;
use crate::graph::schema::{GraphSchema, SchemaDiff, SchemaPatch, SchemaValidation};
use crate::graph::{
    Graph, GraphStatistics, GraphStorage, MemoryStats, StorageCapabilities, Transactional,
};
use crate::traversal::GraphTraversalSource;

#[cfg(feature = "io")]
use crate::error::IOError;
#[cfg(feature = "io")]
use crate::io::ImportReport;
#[cfg(feature = "persist")]
use crate::persist::{PersistError, SnapshotInfo};
#[cfg(feature = "script")]
use crate::script::plan::{ScriptPlan, Terminal, TerminalOutput, UnitBody, UnitKind, UnitResult};

/// The preset options of a host session (the script layer's presets): they win over a
/// storage's [`ExecutionPolicy`](crate::graph::ExecutionPolicy) defaults unless the policy
/// locks the key. Only the memory budget of an import reads them.
#[cfg(feature = "io")]
type PresetOptions =
    std::collections::HashMap<compact_str::CompactString, crate::value::ElementProperty>;

mod sealed {
    /// Seals [`AnyGraph`](super::AnyGraph): only [`Graph`](crate::graph::Graph) implements it.
    pub trait Sealed {}
}

/// A shared graph of ANY storage type, as one object-safe type: what the front ends (the
/// script layer, a session) hold as `Arc<dyn AnyGraph>`. EXPERIMENTAL in 0.2.x, like
/// [`GraphStorage`]: methods are added as the front ends need them.
///
/// **Sealed:** only [`Graph<S>`](Graph) implements it (a blanket implementation for every
/// `S: GraphStorage + Default + Send + Sync + 'static`); no other crate can. Adding a method is
/// therefore never a breaking change. A host plugs its own storage in by wrapping it in a
/// [`Graph`] and converting it with [`IntoAnyGraph`]; the book page `for_developers/custom_storage.md` describes the whole
/// path (the DSL, the session, the Store) and what stays `TraversalGraph`-only (scratch graphs,
/// compressed properties, a `withSideEffect` subgraph target).
///
/// **Why `Default`:** the front ends build new graphs of the served storage (the session
/// copies every sample and loaded file into an `S::default()`). An in-memory storage can always
/// be empty, so the bound is cheap (design decision 1). (A whole script runs as one unit held by
/// its thread across locks, `Graph::begin_unit`; it no longer needs the bound itself.)
///
/// Every method is ONE operation on the locked graph: the reads take the read lock, the
/// writes the write lock, for the duration of the call. On the thread that holds a unit of the
/// graph (`Graph::begin_unit`) they use views of the unit's lock; a view that thread may not
/// have (a commit hook or an embedder asking about the graph its unit is changing,
/// [`GraphError::GraphViewHeld`]) is the error of the method, never a panic and never an empty
/// answer: every method that reads or writes the graph returns a `Result` (the error types are
/// `#[non_exhaustive]`), even where it cannot fail today, so a new failure is never a breaking
/// change. The methods mirror what the script
/// layer calls today; they ask no authorizer (the front end checks access before it calls)
/// and run like the [`GraphTraversalSource`] method of the same name on the locked storage.
/// Results never carry a storage's element ids.
///
/// ```
/// use std::sync::Arc;
/// use graphersal::prelude::*;
/// use graphersal::storage::{AnyGraph, IntoAnyGraph};
///
/// let graph = Arc::new(Graph::new(TraversalGraph::tinkerpop_modern()));
/// let any: Arc<dyn AnyGraph> = graph.clone().into_any_graph();
/// assert_eq!(any.statistics().unwrap().unwrap().vertex_count(), 6);
/// assert!(any.as_any().downcast_ref::<Graph<TraversalGraph>>().is_some());
/// ```
pub trait AnyGraph: Send + Sync + 'static + sealed::Sealed {
    /// The graph as [`std::any::Any`], to downcast it to its [`Graph<S>`](Graph).
    fn as_any(&self) -> &dyn std::any::Any;

    /// The shared graph as a shared [`std::any::Any`], to downcast it to an `Arc<Graph<S>>`
    /// ([`Arc::downcast`]).
    fn into_any_arc(self: Arc<Self>) -> Arc<dyn std::any::Any + Send + Sync>;

    /// The storage's type name (`std::any::type_name::<S>()`), for diagnostics only: its text
    /// is not stable.
    fn storage_type_name(&self) -> &'static str;

    /// [`GraphStorage::capabilities`] of the storage. Fails with
    /// [`GraphError::GraphViewHeld`] when the current thread may not read the graph.
    fn capabilities(&self) -> Result<StorageCapabilities, GraphError>;

    /// [`GraphStorage::last_commit_seq`] of the storage. Fails like [`Self::capabilities`].
    fn last_commit_seq(&self) -> Result<u64, GraphError>;

    /// [`GraphStorage::statistics`] of the storage, cloned out of the lock (`None`: the
    /// storage keeps none). Fails like [`Self::capabilities`].
    fn statistics(&self) -> Result<Option<GraphStatistics>, GraphError>;

    /// [`GraphStorage::memory_usage`] of the storage, with the graph's
    /// [`MemoryStats::workload_bytes`] (`None`: the storage reports none). Fails like
    /// [`Self::capabilities`].
    fn memory_usage(&self) -> Result<Option<MemoryStats>, GraphError>;

    /// The workload statistics attached to the graph ([`Graph::attach_workload_stats`]).
    fn workload_stats(&self) -> Option<std::sync::Arc<crate::graph::workload::WorkloadStats>>;

    /// The memory report of the storage
    /// ([`GraphTraversalSource::memory_report`](crate::GraphTraversalSource::memory_report)),
    /// computed under one read lock. Fails for an invalid request and when cancelled (the
    /// request's or the running thread's [`CancelToken`](crate::CancelToken)). Unauthorized:
    /// the front end asks its authorizer first.
    fn memory_report(
        &self,
        request: &crate::graph::MemoryReportRequest,
    ) -> TraverserResult<crate::graph::MemoryReport>;

    /// [`Graph::advise`]: the storage advisor's recommendations. Unauthorized: the front end
    /// asks its authorizer first.
    fn advise(
        &self,
        request: &crate::graph::advisor::AdviceRequest,
    ) -> TraverserResult<crate::graph::advisor::Advice>;

    /// The graph as one value, `{"vertices": [..], "edges": [..]}`, with at most `keep`
    /// vertices and `keep` edges (all with `None`): what a script host shows for a graph value
    /// as a script's final value. Internal to the script layer.
    #[doc(hidden)]
    fn listing(&self, keep: Option<usize>) -> TraverserResult<crate::value::ElementProperty>;

    // ---- schema ---------------------------------------------------------------------------

    /// The stored schema ([`GraphStorage::get_schema`]); `None` without one. Fails like
    /// [`Self::capabilities`].
    fn get_schema(&self) -> Result<Option<GraphSchema>, GraphError>;

    /// [`GraphTraversalSource::set_schema`]: replaces the stored schema, in the current unit or
    /// as one of its own.
    fn set_schema(&self, schema: GraphSchema) -> TraverserResult<()>;

    /// [`GraphTraversalSource::patch_schema`]: applies a JSON Merge Patch to the stored schema,
    /// stores the result and returns it.
    fn patch_schema(&self, patch: &SchemaPatch) -> TraverserResult<GraphSchema>;

    /// [`GraphTraversalSource::validate_schema`]: the dry run of [`Self::set_schema`].
    fn validate_schema(&self, schema: &GraphSchema) -> TraverserResult<SchemaValidation>;

    /// [`GraphTraversalSource::validate_schema_patch`]: the dry run of [`Self::patch_schema`].
    fn validate_schema_patch(&self, patch: &SchemaPatch) -> TraverserResult<SchemaValidation>;

    /// [`GraphTraversalSource::infer_schema`]: the schema the stored data satisfies.
    fn infer_schema(&self) -> TraverserResult<GraphSchema>;

    /// [`GraphTraversalSource::diff_schema`]: the changes from the stored schema to `schema`.
    fn diff_schema(&self, schema: &GraphSchema) -> TraverserResult<SchemaDiff>;

    // ---- catalog --------------------------------------------------------------------------

    /// The catalog's definitions of `kind` (`None`: every kind), by kind and then by name
    /// ([`GraphTraversalSource::list_definitions`]). Fails like [`Self::capabilities`].
    fn definitions(&self, kind: Option<DefinitionKind>) -> Result<Vec<Definition>, GraphError>;

    /// The definition of `kind` named `name` ([`GraphStorage::definition`]); `None` without
    /// one. Fails like [`Self::capabilities`].
    fn definition(
        &self,
        kind: DefinitionKind,
        name: &str,
    ) -> Result<Option<Definition>, GraphError>;

    /// Stores every definition of `definitions` ([`GraphTraversalSource::set_definition`],
    /// each replacing the one of the same kind and name) in ONE unit
    /// ([`Transactional::transaction`]): on an atomic storage either all of them or none. A
    /// compression rule is stored like any other definition on every storage; only a storage
    /// with compressed properties ([`TraversalGraph`](crate::TraversalGraph)) applies it.
    fn set_definitions(&self, definitions: Vec<Definition>) -> TraverserResult<()>;

    /// [`GraphTraversalSource::remove_definition`]; `false` when there is none.
    fn remove_definition(&self, kind: DefinitionKind, name: &str) -> TraverserResult<bool>;

    /// [`GraphTraversalSource::move_query`]: moves the saved query `name` into `folder`.
    fn move_query(&self, name: &str, folder: Option<&str>) -> TraverserResult<()>;

    /// [`GraphTraversalSource::describe_query`]: sets or removes the description of the saved
    /// query `name`.
    fn describe_query(&self, name: &str, description: Option<&str>) -> TraverserResult<()>;

    /// [`GraphTraversalSource::recompress`]: re-runs the explicit pass of the compression rule
    /// `name`. A storage without compressed properties fails with [`GraphError::Unsupported`]
    /// (the [`GraphStorage::recompress`] default).
    fn recompress(&self, name: &str) -> TraverserResult<()>;

    /// [`GraphStorage::compression_stats`] of the rule `name`; `None` without such a rule or on
    /// a storage without compressed properties. Fails like [`Self::capabilities`].
    fn compression_stats(&self, name: &str) -> Result<Option<CompressionStats>, GraphError>;

    /// [`GraphTraversalSource::estimate_compression`] under the read lock: what each candidate
    /// rule would hold over the current values at `(element, label, path)`; changes nothing. A
    /// storage without compressed properties fails with [`GraphError::Unsupported`] (the
    /// [`GraphStorage::estimate_compression`] default); a cancelled estimate (the request's or
    /// the running thread's [`CancelToken`](crate::CancelToken)) with
    /// [`TraverserError::Cancelled`](crate::TraverserError::Cancelled). Unauthorized: the
    /// front end asks its authorizer first.
    fn estimate_compression(
        &self,
        request: &crate::graph::CompressionEstimateRequest,
    ) -> TraverserResult<crate::graph::CompressionEstimate>;

    /// [`GraphStorage::property_indexes`] of the storage: the property indexes it serves, with
    /// their stats (empty on a storage without property indexes). Fails like
    /// [`Self::capabilities`].
    fn property_indexes(&self) -> Result<Vec<crate::graph::PropertyIndexInfo>, GraphError>;

    /// [`Graph::embed_property`]: the job `embed_property` (book page "Vectors"), two-phase
    /// (the embedder runs without this graph's lock), or under one write lock when the storage
    /// is inside an open unit (a whole-script unit).
    fn embed_property(
        &self,
        job: &crate::value::EmbedJob,
        embedders: &crate::value::Embedders,
        cancel: &crate::traversal::cancel::CancelToken,
        progress: &mut dyn FnMut(&crate::value::EmbedReport),
    ) -> Result<crate::value::EmbedReport, crate::value::EmbedJobError>;

    // ---- journal --------------------------------------------------------------------------

    /// [`GraphStorage::mark`]: a named mark at the current commit position; without a journal
    /// [`GraphError::MarkUnavailable`].
    fn mark(&self, name: &str) -> Result<Mark, GraphError>;

    // ---- io -------------------------------------------------------------------------------

    /// Imports GraphML from `reader` as ONE unit
    /// ([`GraphMLImport::import_reader`](crate::io::GraphMLImport::import_reader)), under the
    /// memory budget of the storage's policy and the host's `presets`.
    #[cfg(feature = "io")]
    fn import_graphml(
        &self,
        reader: &mut dyn std::io::BufRead,
        presets: Option<&PresetOptions>,
    ) -> Result<(), IOError>;

    /// Imports GraphSON from `reader` as ONE unit
    /// ([`GraphSONImport::import_reader`](crate::io::GraphSONImport::import_reader)) and returns
    /// its report, under the memory budget like [`Self::import_graphml`].
    #[cfg(feature = "io")]
    fn import_graphson(
        &self,
        reader: &mut dyn std::io::BufRead,
        presets: Option<&PresetOptions>,
    ) -> Result<ImportReport, IOError>;

    /// Exports the graph as GraphML to `writer`
    /// ([`GraphMLExport::export_writer`](crate::io::GraphMLExport::export_writer)), indented
    /// when `beautify`. The caller flushes `writer`.
    #[cfg(feature = "io")]
    fn export_graphml(
        &self,
        writer: &mut dyn std::io::Write,
        beautify: bool,
    ) -> Result<(), IOError>;

    /// Exports the graph as GraphSON 3.0, line form, to `writer`
    /// ([`GraphSONExport::export_writer`](crate::io::GraphSONExport::export_writer)). The
    /// caller flushes `writer`.
    #[cfg(feature = "io")]
    fn export_graphson(&self, writer: &mut dyn std::io::Write) -> Result<(), IOError>;

    // ---- persist --------------------------------------------------------------------------

    /// Writes the graph as a packed snapshot to `writer`
    /// ([`write_snapshot`](crate::persist::write_snapshot) with the default options). Written
    /// are a graph built by [`Graph::new_persistent`] (any
    /// [`PersistentStorage`](crate::graph::PersistentStorage)) and every
    /// [`Graph<TraversalGraph>`](Graph); any other graph fails with [`PersistError::Graph`]
    /// holding [`GraphError::Unsupported`] (the dyn boundary cannot test
    /// `S: PersistentStorage` itself).
    #[cfg(feature = "persist")]
    fn write_snapshot(&self, writer: &mut dyn std::io::Write)
    -> Result<SnapshotInfo, PersistError>;

    // ---- script ---------------------------------------------------------------------------

    /// Runs a script plan with one of the script terminals (`to_list()`, `next()`,
    /// `profile()`, `execute()`, the visualizers, `iterate()`, `to_graph()`): lowers it for
    /// `&mut S` under the write lock (a writing plan) or for `&S` under the read lock. Internal
    /// to the script layer: its types cannot be built outside this crate.
    #[cfg(feature = "script")]
    #[doc(hidden)]
    fn run_plan(&self, plan: &ScriptPlan<'_>, terminal: Terminal) -> TerminalOutput;

    /// Runs `body` as ONE unit over the whole script (`eval_value_atomic`,
    /// `eval_value_dry_run`, a writing saved-query call): the current thread holds the unit
    /// across locks (`Graph::begin_unit`), `body` runs the script on this same graph (its
    /// terminals get views of the held lock), and the unit closes as `kind` says, also on
    /// unwind. Internal to the script layer like [`Self::run_plan`].
    #[cfg(feature = "script")]
    #[doc(hidden)]
    fn run_unit(&self, kind: UnitKind, body: &mut dyn FnMut() -> UnitBody) -> UnitResult;
}

impl<S> sealed::Sealed for Graph<S> where S: GraphStorage + Default + Send + Sync + 'static {}

impl<S> AnyGraph for Graph<S>
where
    S: GraphStorage + Default + Send + Sync + 'static,
{
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn into_any_arc(self: Arc<Self>) -> Arc<dyn std::any::Any + Send + Sync> {
        self
    }

    fn storage_type_name(&self) -> &'static str {
        std::any::type_name::<S>()
    }

    fn capabilities(&self) -> Result<StorageCapabilities, GraphError> {
        Ok(self.read_checked()?.capabilities())
    }

    fn last_commit_seq(&self) -> Result<u64, GraphError> {
        Ok(self.read_checked()?.last_commit_seq())
    }

    fn statistics(&self) -> Result<Option<GraphStatistics>, GraphError> {
        Ok(self.read_checked()?.statistics().cloned())
    }

    fn memory_usage(&self) -> Result<Option<MemoryStats>, GraphError> {
        let stats = self.read_checked()?.memory_usage();
        Ok(stats.map(|mut stats| {
            stats.workload_bytes = Graph::workload_stats(self).map_or(0, |w| w.memory_bytes());
            stats
        }))
    }

    fn workload_stats(&self) -> Option<std::sync::Arc<crate::graph::workload::WorkloadStats>> {
        Graph::workload_stats(self).cloned()
    }

    fn memory_report(
        &self,
        request: &crate::graph::MemoryReportRequest,
    ) -> TraverserResult<crate::graph::MemoryReport> {
        GraphTraversalSource::new(&*self.read_checked()?).memory_report(request)
    }

    fn advise(
        &self,
        request: &crate::graph::advisor::AdviceRequest,
    ) -> TraverserResult<crate::graph::advisor::Advice> {
        Graph::advise(self, request)
    }

    fn listing(&self, keep: Option<usize>) -> TraverserResult<crate::value::ElementProperty> {
        crate::graph::subgraph::graph_listing(&*self.read_checked()?, keep)
    }

    fn get_schema(&self) -> Result<Option<GraphSchema>, GraphError> {
        Ok(self.read_checked()?.get_schema())
    }

    fn set_schema(&self, schema: GraphSchema) -> TraverserResult<()> {
        GraphTraversalSource::new(&mut *self.write_checked()?).set_schema(schema)
    }

    fn patch_schema(&self, patch: &SchemaPatch) -> TraverserResult<GraphSchema> {
        GraphTraversalSource::new(&mut *self.write_checked()?).patch_schema(patch)
    }

    fn validate_schema(&self, schema: &GraphSchema) -> TraverserResult<SchemaValidation> {
        GraphTraversalSource::new(&*self.read_checked()?).validate_schema(schema)
    }

    fn validate_schema_patch(&self, patch: &SchemaPatch) -> TraverserResult<SchemaValidation> {
        GraphTraversalSource::new(&*self.read_checked()?).validate_schema_patch(patch)
    }

    fn infer_schema(&self) -> TraverserResult<GraphSchema> {
        GraphTraversalSource::new(&*self.read_checked()?).infer_schema()
    }

    fn diff_schema(&self, schema: &GraphSchema) -> TraverserResult<SchemaDiff> {
        GraphTraversalSource::new(&*self.read_checked()?).diff_schema(schema)
    }

    fn definitions(&self, kind: Option<DefinitionKind>) -> Result<Vec<Definition>, GraphError> {
        Ok(self
            .read_checked()?
            .definitions()
            .filter(|definition| kind.is_none_or(|kind| definition.kind() == kind))
            .cloned()
            .collect())
    }

    fn definition(
        &self,
        kind: DefinitionKind,
        name: &str,
    ) -> Result<Option<Definition>, GraphError> {
        Ok(self.read_checked()?.definition(kind, name).cloned())
    }

    fn set_definitions(&self, definitions: Vec<Definition>) -> TraverserResult<()> {
        self.write_checked()?.transaction(|unit| {
            for definition in definitions {
                GraphTraversalSource::new(&mut *unit).set_definition(definition)?;
            }
            Ok(())
        })
    }

    fn remove_definition(&self, kind: DefinitionKind, name: &str) -> TraverserResult<bool> {
        GraphTraversalSource::new(&mut *self.write_checked()?).remove_definition(kind, name)
    }

    fn move_query(&self, name: &str, folder: Option<&str>) -> TraverserResult<()> {
        GraphTraversalSource::new(&mut *self.write_checked()?).move_query(name, folder)
    }

    fn describe_query(&self, name: &str, description: Option<&str>) -> TraverserResult<()> {
        GraphTraversalSource::new(&mut *self.write_checked()?).describe_query(name, description)
    }

    fn recompress(&self, name: &str) -> TraverserResult<()> {
        GraphTraversalSource::new(&mut *self.write_checked()?).recompress(name)
    }

    fn compression_stats(&self, name: &str) -> Result<Option<CompressionStats>, GraphError> {
        Ok(self.read_checked()?.compression_stats(name))
    }

    fn estimate_compression(
        &self,
        request: &crate::graph::CompressionEstimateRequest,
    ) -> TraverserResult<crate::graph::CompressionEstimate> {
        GraphTraversalSource::new(&*self.read_checked()?).estimate_compression(request)
    }

    fn embed_property(
        &self,
        job: &crate::value::EmbedJob,
        embedders: &crate::value::Embedders,
        cancel: &crate::traversal::cancel::CancelToken,
        progress: &mut dyn FnMut(&crate::value::EmbedReport),
    ) -> Result<crate::value::EmbedReport, crate::value::EmbedJobError> {
        Graph::embed_property(self, job, embedders, cancel, progress)
    }

    fn property_indexes(&self) -> Result<Vec<crate::graph::PropertyIndexInfo>, GraphError> {
        Ok(self.read_checked()?.property_indexes())
    }

    fn mark(&self, name: &str) -> Result<Mark, GraphError> {
        self.write_checked()?.mark(name)
    }

    #[cfg(feature = "io")]
    fn import_graphml(
        &self,
        reader: &mut dyn std::io::BufRead,
        presets: Option<&PresetOptions>,
    ) -> Result<(), IOError> {
        crate::io::GraphMLImport {}.import_reader_with(
            &mut *self.write_checked()?,
            quick_xml::Reader::from_reader(reader),
            presets,
        )
    }

    #[cfg(feature = "io")]
    fn import_graphson(
        &self,
        reader: &mut dyn std::io::BufRead,
        presets: Option<&PresetOptions>,
    ) -> Result<ImportReport, IOError> {
        crate::io::GraphSONImport::new().import_reader_with(
            &mut *self.write_checked()?,
            reader,
            presets,
        )
    }

    #[cfg(feature = "io")]
    fn export_graphml(
        &self,
        writer: &mut dyn std::io::Write,
        beautify: bool,
    ) -> Result<(), IOError> {
        crate::io::GraphMLExport::new()
            .with_beautify(beautify)
            .export_writer(&*self.read_checked()?, writer)
    }

    #[cfg(feature = "io")]
    fn export_graphson(&self, writer: &mut dyn std::io::Write) -> Result<(), IOError> {
        crate::io::GraphSONExport::new().export_writer(&*self.read_checked()?, writer)
    }

    #[cfg(feature = "persist")]
    fn write_snapshot(
        &self,
        writer: &mut dyn std::io::Write,
    ) -> Result<SnapshotInfo, PersistError> {
        // The writer of `Graph::new_persistent` (design 5.3); a `TraversalGraph` wrapped by
        // plain `Graph::new` is persistent all the same, so it is written without one.
        if let Some(write) = self.snapshot_writer() {
            return write(&*self.read_checked().map_err(PersistError::Graph)?, writer);
        }
        match self.as_any().downcast_ref::<Graph<crate::TraversalGraph>>() {
            Some(graph) => graph
                .read_checked()
                .map_err(PersistError::Graph)?
                .write_snapshot(writer),
            None => Err(PersistError::Graph(GraphError::unsupported("snapshots"))),
        }
    }

    #[cfg(feature = "script")]
    fn run_plan(&self, plan: &ScriptPlan<'_>, terminal: Terminal) -> TerminalOutput {
        crate::script::plan::run_plan_on(self, plan, terminal)
    }

    #[cfg(feature = "script")]
    fn run_unit(&self, kind: UnitKind, body: &mut dyn FnMut() -> UnitBody) -> UnitResult {
        crate::script::plan::run_unit_on(self, kind, body)
    }
}

/// What every front-end entry point that takes a graph accepts: an `Arc<Graph<S>>` of any
/// served storage, or an `Arc<dyn AnyGraph>` a front end already holds.
pub trait IntoAnyGraph {
    /// The graph as the front ends hold it.
    fn into_any_graph(self) -> Arc<dyn AnyGraph>;
}

impl<S> IntoAnyGraph for Arc<Graph<S>>
where
    S: GraphStorage + Default + Send + Sync + 'static,
{
    fn into_any_graph(self) -> Arc<dyn AnyGraph> {
        self
    }
}

impl IntoAnyGraph for Arc<dyn AnyGraph> {
    fn into_any_graph(self) -> Arc<dyn AnyGraph> {
        self
    }
}

#[cfg(test)]
mod tests;
