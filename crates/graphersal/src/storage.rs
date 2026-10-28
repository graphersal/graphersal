//! The storage extension point: the [`GraphStorage`] trait and every type in its signatures.
//!
//! For implementers of their own storage backend (a server keeping its graph elsewhere, a test
//! double, a wrapper adding behaviour). `use graphersal::storage::*;` together with
//! `use graphersal::prelude::*;` is all an implementation needs. The trait is EXPERIMENTAL in
//! 0.2.x; its trait-level documentation is the implementer's guide, and the crate
//! `graphersal-storage-tests` is its conformance suite. A shared graph is a [`Graph<S>`](Graph);
//! the front ends hold it as the sealed, object-safe [`AnyGraph`] (see [`IntoAnyGraph`]).
//!
//! The write checks the contract requires are public functions here, the ones
//! [`TraversalGraph`] and the trait's defaults call: [`check_property_value`] /
//! [`check_properties`] (element references are not property values), [`check_label`] /
//! [`normalize_vertex_labels`] (vertex labels), and the JSONPath write helpers
//! [`path_property_name`], [`write_path`], [`write_path_in`], [`remove_path`],
//! [`remove_path_in`] over the public [`JsonPathSegment`]s. They are the minimum; a storage may
//! refuse more with errors of its own. The errors themselves have constructors on
//! [`GraphError`] (`unsupported`, `element_removed`, `invalid_property_value`,
//! `invalid_property_path`, ...).
//!
//! A storage that captures changes (`change_capture`) builds its [`ChangeSet`]s with
//! [`ChangeSet::builder`] and the [`Mutation`] constructors, and embeds a [`CommitHooks`] that it
//! calls at its commit point (user hooks in registration order, then the journal slot; the guide
//! is on [`CommitHooks`]). With feature `persist`, `PersistentStorage` (EXPERIMENTAL like
//! [`GraphStorage`]) is what a snapshot and a Store need from a storage: lineage id, commit
//! position, a load sink, `clear`, the journal slot ([`JournalSlot`]) and `replay`.
//!
//! # Example
//!
//! A storage that is always empty and refuses every write, with the optional methods overridden
//! only to show that every type they mention is reachable from these two imports:
//!
//! ```
//! use graphersal::prelude::*;
//! use graphersal::storage::*;
//!
//! struct Empty;
//!
//! // The error variants are `#[non_exhaustive]`; a storage builds them with the constructors
//! // (`unsupported`, `element_removed`, `duplicate_vertex_id`, ...).
//! fn refused() -> GraphError {
//!     GraphError::unsupported("writes")
//! }
//!
//! impl GraphStorage for Empty {
//!     type VertexId = u32;
//!     type EdgeId = u32;
//!
//!     // Every read returns a `Result`: a backend that cannot answer returns
//!     // `GraphError::storage_failed(..)`, never an empty answer.
//!     fn vertex_count(&self) -> Result<usize, GraphError> { Ok(0) }
//!     fn edge_count(&self) -> Result<usize, GraphError> { Ok(0) }
//!     fn vertices(&self) -> Result<impl Iterator<Item = u32> + '_, GraphError> {
//!         Ok(std::iter::empty())
//!     }
//!     fn edges(&self) -> Result<impl Iterator<Item = u32> + '_, GraphError> {
//!         Ok(std::iter::empty())
//!     }
//!     fn get_vertex_by_id(&self, _id: &str) -> Result<Option<u32>, GraphError> { Ok(None) }
//!     fn get_edge_by_id(&self, _id: &str) -> Result<Option<u32>, GraphError> { Ok(None) }
//!     fn out_edges(&self, _node: u32) -> Result<impl Iterator<Item = u32> + '_, GraphError> {
//!         Ok(std::iter::empty())
//!     }
//!     fn in_edges(&self, _node: u32) -> Result<impl Iterator<Item = u32> + '_, GraphError> {
//!         Ok(std::iter::empty())
//!     }
//!     fn vertex_id(&self, _node: u32) -> Result<Option<&str>, GraphError> { Ok(None) }
//!     fn edge_id(&self, _edge: u32) -> Result<Option<&str>, GraphError> { Ok(None) }
//!     fn vertex_label(&self, _node: u32) -> Result<Option<&str>, GraphError> { Ok(None) }
//!     fn edge_label(&self, _edge: u32) -> Result<Option<&str>, GraphError> { Ok(None) }
//!     fn vertex_labels(&self, _node: u32) -> Result<&[CompactString], GraphError> { Ok(&[]) }
//!     fn vertex_properties(&self, _node: u32) -> Result<Option<&PropertyMap>, GraphError> {
//!         Ok(None)
//!     }
//!     fn edge_properties(&self, _edge: u32) -> Result<Option<&PropertyMap>, GraphError> {
//!         Ok(None)
//!     }
//!     fn edge_endpoints(&self, _edge: u32) -> Result<Option<(u32, u32)>, GraphError> {
//!         Ok(None)
//!     }
//!
//!     // The contract's write checks are public helpers (`check_properties`,
//!     // `normalize_vertex_labels`, `write_path`, ...): run them first, so a bad value gets the
//!     // error every storage gives (`InvalidPropertyValue`, `InvalidLabel`), then refuse more.
//!     fn insert_vertex(
//!         &mut self,
//!         _id: Option<CompactString>,
//!         labels: Vec<CompactString>,
//!         properties: PropertyMap,
//!     ) -> Result<u32, GraphError> {
//!         normalize_vertex_labels(labels)?;
//!         check_properties(&properties)?;
//!         Err(refused())
//!     }
//!     fn add_vertex_label(&mut self, _node: u32, _label: CompactString) -> Result<bool, GraphError> {
//!         Err(refused())
//!     }
//!     fn remove_vertex_label(&mut self, _node: u32, _label: &str) -> Result<bool, GraphError> {
//!         Err(refused())
//!     }
//!     fn insert_edge(
//!         &mut self,
//!         _id: Option<CompactString>,
//!         _label: Option<CompactString>,
//!         _from: u32,
//!         _to: u32,
//!         _properties: PropertyMap,
//!     ) -> Result<u32, GraphError> {
//!         Err(refused())
//!     }
//!     fn set_vertex_property(
//!         &mut self,
//!         _node: u32,
//!         _key: CompactString,
//!         _value: ElementProperty,
//!     ) -> Result<(), GraphError> {
//!         Err(refused())
//!     }
//!     fn set_edge_property(
//!         &mut self,
//!         _edge: u32,
//!         _key: CompactString,
//!         _value: ElementProperty,
//!     ) -> Result<(), GraphError> {
//!         Err(refused())
//!     }
//!     fn drop_vertex(&mut self, _node: u32) -> Result<(), GraphError> { Ok(()) }
//!     fn drop_vertex_property(&mut self, _prop: &str, _node: u32) -> Result<(), GraphError> {
//!         Ok(())
//!     }
//!     fn drop_edge(&mut self, _edge: u32) -> Result<(), GraphError> { Ok(()) }
//!     fn drop_edge_property(&mut self, _prop: &str, _edge: u32) -> Result<(), GraphError> {
//!         Ok(())
//!     }
//!
//!     // Optional methods (all have defaults), overridden here for their types only.
//!     fn set_vertex_property_path(
//!         &mut self,
//!         _node: u32,
//!         _path: &JsonPath,
//!         _value: ElementProperty,
//!     ) -> Result<(), GraphError> {
//!         Err(refused())
//!     }
//!     // The methods beyond single elements return a `Result`: a storage whose state sits in a
//!     // backend returns `GraphError::storage_failed(..)` instead of an empty answer.
//!     fn statistics(&self) -> Result<Option<&GraphStatistics>, GraphError> { Ok(None) }
//!     fn get_schema(&self) -> Result<Option<GraphSchema>, GraphError> { Ok(None) }
//!     fn declared_property_schema(
//!         &self,
//!         _kind: GraphNodeType,
//!         _labels: &[CompactString],
//!         _key: &str,
//!     ) -> Result<Option<PropertySchema>, GraphError> {
//!         Ok(None)
//!     }
//!     fn definition(
//!         &self,
//!         _kind: DefinitionKind,
//!         _name: &str,
//!     ) -> Result<Option<&Definition>, GraphError> {
//!         Ok(None)
//!     }
//!     fn compression_stats(&self, _name: &str) -> Result<Option<CompressionStats>, GraphError> {
//!         Ok(None)
//!     }
//!     fn memory_usage(&self) -> Result<Option<MemoryStats>, GraphError> { Ok(None) }
//!     fn execution_policy(&self) -> Result<ExecutionPolicy, GraphError> {
//!         Ok(ExecutionPolicy::default())
//!     }
//!     fn begin_unit(&mut self) -> Result<UnitMark, GraphError> { Ok(UnitMark::new(0)) }
//!     fn commit_unit(&mut self, _mark: UnitMark) -> Result<(), GraphError> { Ok(()) }
//!     fn rollback_unit(&mut self, _mark: UnitMark, _reason: &RollbackReason) -> Result<(), GraphError> {
//!         Ok(())
//!     }
//!     fn unit_changes(&self, _mark: UnitMark) -> Result<UnitChanges, GraphError> {
//!         Ok(UnitChanges::NONE)
//!     }
//!     fn capabilities(&self) -> Result<StorageCapabilities, GraphError> {
//!         Ok(StorageCapabilities::none().with_isolation(IsolationLevel::default()))
//!     }
//!     fn register_commit_hook(&mut self, _hook: Box<dyn CommitHook>) -> Result<(), GraphError> {
//!         Err(refused())
//!     }
//!     fn apply_change_set(&mut self, _changes: &ChangeSet) -> Result<(), GraphError> {
//!         Err(refused())
//!     }
//! }
//!
//! let g = GraphTraversalSource::new(Empty);
//! assert_eq!(g.v(None::<()>).count().next().unwrap(), Some(ElementProperty::from(0_i64)));
//! assert!(GraphTraversalSource::new(Empty).add_v("person").next().is_err());
//! assert!(matches!(
//!     Empty.insert_vertex(None, vec!["a::b".into()], PropertyMap::default()),
//!     Err(GraphError::InvalidLabel { .. })
//! ));
//! ```

pub use crate::error::GraphError;
#[cfg(feature = "persist")]
pub use crate::graph::PersistentStorage;
pub use crate::graph::compression::CompressionStats;
pub use crate::graph::compression_estimate::{CompressionEstimate, CompressionEstimateRequest};
pub use crate::graph::memory_report::{
    DegreeStats, LabelMemory, MemoryReport, MemoryReportProgress, MemoryReportRequest,
    PropertyMemory, PropertyTarget, RuleMemory, SIZE_BUCKET_BOUNDS, ShapeBucket, ShapeSummary,
    SizeBucket, SlotUsage, StringMemory, StructureMemory, ValueTypeCount, VectorMemory,
};
pub use crate::graph::property_index::{
    PropertyIndexInfo, PropertyIndexRange, PropertyIndexStats, PropertyIndexTransform,
};
pub use crate::graph::schema::defaults::CreateContext;
pub use crate::graph::schema::{ElementKind, GraphSchema, PropertySchema, SchemaInferer};
pub use crate::graph::write_checks::{
    check_label, check_properties, check_property_value, normalize_vertex_labels,
    path_property_name, remove_path, remove_path_in, write_path, write_path_in,
};
pub use crate::graph::{
    AnyGraph, ChangeSet, ChangeSetBuilder, CommitHook, CommitHooks, CommitMetadata, Definition,
    DefinitionKind, EdgeHandle, EdgeState, ElementRef, ExecutionPolicy, Graph, GraphNodeType,
    GraphRead, GraphStatistics, GraphStorage, GraphWrite, HookError, IntoAnyGraph, IsolationLevel,
    JournalSlot, Mark, MemoryStats, Mutation, RollbackReason, StorageCapabilities, Transactional,
    TraversalGraph, UnitChanges, UnitMark, VertexHandle, VertexState,
};
pub use crate::value::{ElementProperty, JsonPath, JsonPathSegment, PropertyMap};
pub use compact_str::CompactString;
