//! [`Forwarding`]: a storage that is NOT [`TraversalGraph`] but behaves exactly like it.
//!
//! **EXPERIMENTAL**, like the rest of this crate. It is the proof storage of
//! `book/src/for_developers/custom_storage.md`: every [`GraphStorage`] method forwards to a
//! wrapped [`TraversalGraph`], so any difference between a front end (the Rhai DSL, saved
//! queries, the session) running on `Graph<Forwarding>` and on `Graph<TraversalGraph>` is a bug
//! in the generic path, not a behaviour of another storage.
//!
//! Its element handles are its OWN types, [`ForwardVertexId`] and [`ForwardEdgeId`], not
//! [`VertexHandle`]/[`EdgeHandle`]: code that silently assumes the handles of
//! `TraversalGraph` does not compile for it, or shows up in a test.
//!
//! This file uses only `graphersal`'s public facade (no `crate::` paths), because
//! `graphersal`'s own integration tests include it by `#[path]` (a dev-dependency on this crate
//! would link a second copy of `graphersal`, whose types would not match).

use graphersal::ElementProperty;
use graphersal::TraversalGraph;
use graphersal::storage::{
    ChangeSet, CommitHook, CommitMetadata, CompactString, CompressionEstimate,
    CompressionEstimateRequest, CompressionStats, Definition, DefinitionKind, EdgeHandle,
    ExecutionPolicy, GraphError, GraphNodeType, GraphSchema, GraphStatistics, GraphStorage,
    JsonPath, Mark, MemoryStats, PropertyIndexInfo, PropertyIndexRange, PropertyIndexTransform,
    PropertyMap, PropertySchema, RollbackReason, StorageCapabilities, UnitChanges, UnitMark,
    VertexHandle,
};

/// The vertex handle of [`Forwarding`]: a newtype around the wrapped graph's [`VertexHandle`],
/// so it is a distinct type for the engine and the front ends.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ForwardVertexId(VertexHandle);

/// The edge handle of [`Forwarding`]: a newtype around the wrapped graph's [`EdgeHandle`], so
/// it is a distinct type for the engine and the front ends.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ForwardEdgeId(EdgeHandle);

/// A [`GraphStorage`] that forwards every method to a wrapped [`TraversalGraph`], with its own
/// handle types ([`ForwardVertexId`], [`ForwardEdgeId`]). See the [module docs](self).
///
/// Every method `TraversalGraph` implements is forwarded explicitly: the data, labels,
/// properties and path writes, the schema, the catalog, compression (`recompress`,
/// `compression_stats`, `estimate_compression`), property indexes (`property_indexes` and the two lookups), statistics, memory, the execution policy, units, commit hooks and
/// change capture (`unit_changes`, `set_unit_metadata`, `discard_unit`), `mark`, the id
/// sequences and the capabilities. Left on the trait default ON PURPOSE, because
/// `TraversalGraph` keeps the default too and the default then runs over this storage's own
/// methods (which is what a foreign storage gets): `vertex_has_label`, `infer_schema` and
/// `apply_change_set`.
///
/// With feature `persist` it also implements `PersistentStorage`, forwarded as well, so it can be
/// written as a packed snapshot and loaded from one (`graphersal::persist::read_snapshot_into`).
///
/// ```
/// use graphersal::TraversalGraph;
/// use graphersal::storage::GraphStorage;
/// use graphersal_storage_tests::forwarding::Forwarding;
///
/// let storage = Forwarding::new(TraversalGraph::tinkerpop_modern());
/// assert_eq!(storage.vertex_count(), 6);
/// assert_eq!(storage.into_inner().edge_count(), 6);
/// ```
#[derive(Default)]
pub struct Forwarding(TraversalGraph);

impl Forwarding {
    /// Wraps `graph`.
    pub fn new(graph: TraversalGraph) -> Self {
        Self(graph)
    }

    /// The wrapped graph.
    pub fn into_inner(self) -> TraversalGraph {
        self.0
    }
}

impl GraphStorage for Forwarding {
    type VertexId = ForwardVertexId;
    type EdgeId = ForwardEdgeId;

    fn vertex_count(&self) -> usize {
        GraphStorage::vertex_count(&self.0)
    }
    fn edge_count(&self) -> usize {
        GraphStorage::edge_count(&self.0)
    }
    fn vertices(&self) -> impl Iterator<Item = ForwardVertexId> + '_ {
        GraphStorage::vertices(&self.0).map(ForwardVertexId)
    }
    fn edges(&self) -> impl Iterator<Item = ForwardEdgeId> + '_ {
        GraphStorage::edges(&self.0).map(ForwardEdgeId)
    }
    fn get_vertex_by_id(&self, id: &str) -> Option<ForwardVertexId> {
        GraphStorage::get_vertex_by_id(&self.0, id).map(ForwardVertexId)
    }
    fn get_edge_by_id(&self, id: &str) -> Option<ForwardEdgeId> {
        GraphStorage::get_edge_by_id(&self.0, id).map(ForwardEdgeId)
    }
    fn out_edges(&self, node: ForwardVertexId) -> impl Iterator<Item = ForwardEdgeId> + '_ {
        GraphStorage::out_edges(&self.0, node.0).map(ForwardEdgeId)
    }
    fn in_edges(&self, node: ForwardVertexId) -> impl Iterator<Item = ForwardEdgeId> + '_ {
        GraphStorage::in_edges(&self.0, node.0).map(ForwardEdgeId)
    }
    fn vertex_id(&self, node: ForwardVertexId) -> Option<&str> {
        GraphStorage::vertex_id(&self.0, node.0)
    }
    fn edge_id(&self, edge: ForwardEdgeId) -> Option<&str> {
        GraphStorage::edge_id(&self.0, edge.0)
    }
    fn vertex_label(&self, node: ForwardVertexId) -> Option<&str> {
        GraphStorage::vertex_label(&self.0, node.0)
    }
    fn edge_label(&self, edge: ForwardEdgeId) -> Option<&str> {
        GraphStorage::edge_label(&self.0, edge.0)
    }
    fn vertex_labels(&self, node: ForwardVertexId) -> &[CompactString] {
        GraphStorage::vertex_labels(&self.0, node.0)
    }
    fn vertex_properties(&self, node: ForwardVertexId) -> Option<&PropertyMap> {
        GraphStorage::vertex_properties(&self.0, node.0)
    }
    fn edge_properties(&self, edge: ForwardEdgeId) -> Option<&PropertyMap> {
        GraphStorage::edge_properties(&self.0, edge.0)
    }
    fn edge_endpoints(&self, edge: ForwardEdgeId) -> Option<(ForwardVertexId, ForwardVertexId)> {
        GraphStorage::edge_endpoints(&self.0, edge.0)
            .map(|(from, to)| (ForwardVertexId(from), ForwardVertexId(to)))
    }
    fn contains_vertex(&self, node: ForwardVertexId) -> bool {
        GraphStorage::contains_vertex(&self.0, node.0)
    }
    fn contains_edge(&self, edge: ForwardEdgeId) -> bool {
        GraphStorage::contains_edge(&self.0, edge.0)
    }
    fn vertices_by_label<'a>(
        &'a self,
        label: &'a str,
    ) -> Box<dyn Iterator<Item = ForwardVertexId> + 'a> {
        Box::new(GraphStorage::vertices_by_label(&self.0, label).map(ForwardVertexId))
    }
    fn edges_by_label<'a>(
        &'a self,
        label: &'a str,
    ) -> Box<dyn Iterator<Item = ForwardEdgeId> + 'a> {
        Box::new(GraphStorage::edges_by_label(&self.0, label).map(ForwardEdgeId))
    }
    fn statistics(&self) -> Option<&GraphStatistics> {
        GraphStorage::statistics(&self.0)
    }

    fn set_schema(&mut self, schema: GraphSchema) -> Result<(), GraphError> {
        GraphStorage::set_schema(&mut self.0, schema)
    }
    fn get_schema(&self) -> Option<GraphSchema> {
        GraphStorage::get_schema(&self.0)
    }
    fn declared_property_schema(
        &self,
        kind: GraphNodeType,
        labels: &[CompactString],
        key: &str,
    ) -> Option<PropertySchema> {
        GraphStorage::declared_property_schema(&self.0, kind, labels, key)
    }

    fn definitions(&self) -> impl Iterator<Item = &Definition> + '_ {
        GraphStorage::definitions(&self.0)
    }
    fn definition(&self, kind: DefinitionKind, name: &str) -> Option<&Definition> {
        GraphStorage::definition(&self.0, kind, name)
    }
    fn set_definition(&mut self, definition: Definition) -> Result<(), GraphError> {
        GraphStorage::set_definition(&mut self.0, definition)
    }
    fn remove_definition(&mut self, kind: DefinitionKind, name: &str) -> Result<bool, GraphError> {
        GraphStorage::remove_definition(&mut self.0, kind, name)
    }
    fn recompress(&mut self, name: &str) -> Result<bool, GraphError> {
        GraphStorage::recompress(&mut self.0, name)
    }
    fn compression_stats(&self, name: &str) -> Option<CompressionStats> {
        GraphStorage::compression_stats(&self.0, name)
    }
    fn estimate_compression(
        &self,
        request: &CompressionEstimateRequest,
    ) -> Result<CompressionEstimate, GraphError> {
        GraphStorage::estimate_compression(&self.0, request)
    }
    fn property_indexes(&self) -> Vec<PropertyIndexInfo> {
        GraphStorage::property_indexes(&self.0)
    }
    fn vertices_by_property_index<'a>(
        &'a self,
        label: &'a str,
        key: &'a str,
        range: &PropertyIndexRange,
    ) -> Option<Box<dyn Iterator<Item = ForwardVertexId> + 'a>> {
        let found = GraphStorage::vertices_by_property_index(&self.0, label, key, range)?;
        Some(Box::new(found.map(ForwardVertexId)))
    }
    fn edges_by_property_index<'a>(
        &'a self,
        label: &'a str,
        key: &'a str,
        range: &PropertyIndexRange,
    ) -> Option<Box<dyn Iterator<Item = ForwardEdgeId> + 'a>> {
        let found = GraphStorage::edges_by_property_index(&self.0, label, key, range)?;
        Some(Box::new(found.map(ForwardEdgeId)))
    }
    fn vertices_by_transformed_property_index<'a>(
        &'a self,
        label: &'a str,
        key: &'a str,
        transform: &PropertyIndexTransform,
        range: &PropertyIndexRange,
    ) -> Option<Box<dyn Iterator<Item = ForwardVertexId> + 'a>> {
        let found = GraphStorage::vertices_by_transformed_property_index(
            &self.0, label, key, transform, range,
        )?;
        Some(Box::new(found.map(ForwardVertexId)))
    }
    fn edges_by_transformed_property_index<'a>(
        &'a self,
        label: &'a str,
        key: &'a str,
        transform: &PropertyIndexTransform,
        range: &PropertyIndexRange,
    ) -> Option<Box<dyn Iterator<Item = ForwardEdgeId> + 'a>> {
        let found = GraphStorage::edges_by_transformed_property_index(
            &self.0, label, key, transform, range,
        )?;
        Some(Box::new(found.map(ForwardEdgeId)))
    }

    fn insert_vertex(
        &mut self,
        id: Option<CompactString>,
        labels: Vec<CompactString>,
        properties: PropertyMap,
    ) -> Result<ForwardVertexId, GraphError> {
        GraphStorage::insert_vertex(&mut self.0, id, labels, properties).map(ForwardVertexId)
    }
    fn add_vertex_label(
        &mut self,
        node: ForwardVertexId,
        label: CompactString,
    ) -> Result<bool, GraphError> {
        GraphStorage::add_vertex_label(&mut self.0, node.0, label)
    }
    fn remove_vertex_label(
        &mut self,
        node: ForwardVertexId,
        label: &str,
    ) -> Result<bool, GraphError> {
        GraphStorage::remove_vertex_label(&mut self.0, node.0, label)
    }
    fn set_edge_label(
        &mut self,
        edge: ForwardEdgeId,
        label: CompactString,
    ) -> Result<bool, GraphError> {
        GraphStorage::set_edge_label(&mut self.0, edge.0, label)
    }
    fn insert_edge(
        &mut self,
        id: Option<CompactString>,
        label: Option<CompactString>,
        from: ForwardVertexId,
        to: ForwardVertexId,
        properties: PropertyMap,
    ) -> Result<ForwardEdgeId, GraphError> {
        GraphStorage::insert_edge(&mut self.0, id, label, from.0, to.0, properties)
            .map(ForwardEdgeId)
    }
    fn set_vertex_property(
        &mut self,
        node: ForwardVertexId,
        key: CompactString,
        value: ElementProperty,
    ) -> Result<(), GraphError> {
        GraphStorage::set_vertex_property(&mut self.0, node.0, key, value)
    }
    fn set_edge_property(
        &mut self,
        edge: ForwardEdgeId,
        key: CompactString,
        value: ElementProperty,
    ) -> Result<(), GraphError> {
        GraphStorage::set_edge_property(&mut self.0, edge.0, key, value)
    }
    fn set_vertex_property_path(
        &mut self,
        node: ForwardVertexId,
        path: &JsonPath,
        value: ElementProperty,
    ) -> Result<(), GraphError> {
        GraphStorage::set_vertex_property_path(&mut self.0, node.0, path, value)
    }
    fn set_edge_property_path(
        &mut self,
        edge: ForwardEdgeId,
        path: &JsonPath,
        value: ElementProperty,
    ) -> Result<(), GraphError> {
        GraphStorage::set_edge_property_path(&mut self.0, edge.0, path, value)
    }
    fn remove_vertex_property_path(
        &mut self,
        node: ForwardVertexId,
        path: &JsonPath,
    ) -> Result<(), GraphError> {
        GraphStorage::remove_vertex_property_path(&mut self.0, node.0, path)
    }
    fn remove_edge_property_path(
        &mut self,
        edge: ForwardEdgeId,
        path: &JsonPath,
    ) -> Result<(), GraphError> {
        GraphStorage::remove_edge_property_path(&mut self.0, edge.0, path)
    }
    fn drop_vertex(&mut self, node: ForwardVertexId) -> Result<(), GraphError> {
        GraphStorage::drop_vertex(&mut self.0, node.0)
    }
    fn drop_vertex_property(
        &mut self,
        prop: &str,
        node: ForwardVertexId,
    ) -> Result<(), GraphError> {
        GraphStorage::drop_vertex_property(&mut self.0, prop, node.0)
    }
    fn drop_edge(&mut self, edge: ForwardEdgeId) -> Result<(), GraphError> {
        GraphStorage::drop_edge(&mut self.0, edge.0)
    }
    fn drop_edge_property(&mut self, prop: &str, edge: ForwardEdgeId) -> Result<(), GraphError> {
        GraphStorage::drop_edge_property(&mut self.0, prop, edge.0)
    }
    fn shrink_to_fit(&mut self) {
        GraphStorage::shrink_to_fit(&mut self.0);
    }

    fn memory_usage(&self) -> Option<MemoryStats> {
        GraphStorage::memory_usage(&self.0)
    }
    fn memory_footprint_bytes(&self) -> Option<u64> {
        GraphStorage::memory_footprint_bytes(&self.0)
    }
    fn execution_policy(&self) -> ExecutionPolicy {
        GraphStorage::execution_policy(&self.0)
    }

    fn begin_unit(&mut self) -> UnitMark {
        GraphStorage::begin_unit(&mut self.0)
    }
    fn commit_unit(&mut self, mark: UnitMark) -> Result<(), GraphError> {
        GraphStorage::commit_unit(&mut self.0, mark)
    }
    fn rollback_unit(&mut self, mark: UnitMark, reason: &RollbackReason) {
        GraphStorage::rollback_unit(&mut self.0, mark, reason);
    }
    fn unit_changes(&self, mark: UnitMark) -> UnitChanges {
        GraphStorage::unit_changes(&self.0, mark)
    }
    fn set_unit_metadata(&mut self, metadata: CommitMetadata) {
        GraphStorage::set_unit_metadata(&mut self.0, metadata);
    }
    fn discard_unit(&mut self, mark: UnitMark) -> Result<ChangeSet, GraphError> {
        GraphStorage::discard_unit(&mut self.0, mark)
    }
    fn mark(&mut self, name: &str) -> Result<Mark, GraphError> {
        GraphStorage::mark(&mut self.0, name)
    }
    fn capabilities(&self) -> StorageCapabilities {
        GraphStorage::capabilities(&self.0)
    }
    fn register_commit_hook(&mut self, hook: Box<dyn CommitHook>) -> Result<(), GraphError> {
        GraphStorage::register_commit_hook(&mut self.0, hook)
    }
    fn last_commit_seq(&self) -> u64 {
        GraphStorage::last_commit_seq(&self.0)
    }
    fn unit_started_at(&self) -> Option<i128> {
        GraphStorage::unit_started_at(&self.0)
    }
    fn raise_id_sequences(
        &mut self,
        node_sequence: u64,
        edge_sequence: u64,
    ) -> Result<(), GraphError> {
        GraphStorage::raise_id_sequences(&mut self.0, node_sequence, edge_sequence)
    }
}

/// Persistence (feature `persist`): every method forwards to the wrapped [`TraversalGraph`],
/// `replay` included (its raw, verified path), so a snapshot or a Store of `Forwarding` holds
/// exactly the bytes of one of `TraversalGraph`. The load sink returns this storage's own
/// handles.
#[cfg(feature = "persist")]
impl graphersal::storage::PersistentStorage for Forwarding {
    fn graph_id(&self) -> u128 {
        graphersal::storage::PersistentStorage::graph_id(&self.0)
    }
    fn set_graph_id(&mut self, graph_id: u128) {
        graphersal::storage::PersistentStorage::set_graph_id(&mut self.0, graph_id)
    }
    fn last_committed_at(&self) -> i64 {
        graphersal::storage::PersistentStorage::last_committed_at(&self.0)
    }
    fn id_sequences(&self) -> (u64, u64) {
        graphersal::storage::PersistentStorage::id_sequences(&self.0)
    }
    fn raise_position(&mut self, commit_seq: u64, committed_at: i64) {
        graphersal::storage::PersistentStorage::raise_position(
            &mut self.0,
            commit_seq,
            committed_at,
        )
    }
    fn load_vertex(
        &mut self,
        id: CompactString,
        labels: Vec<CompactString>,
        properties: PropertyMap,
    ) -> Result<ForwardVertexId, GraphError> {
        graphersal::storage::PersistentStorage::load_vertex(&mut self.0, id, labels, properties)
            .map(ForwardVertexId)
    }
    fn load_edge(
        &mut self,
        id: CompactString,
        label: Option<CompactString>,
        from_id: &str,
        to_id: &str,
        properties: PropertyMap,
    ) -> Result<ForwardEdgeId, GraphError> {
        graphersal::storage::PersistentStorage::load_edge(
            &mut self.0,
            id,
            label,
            from_id,
            to_id,
            properties,
        )
        .map(ForwardEdgeId)
    }
    fn load_schema(&mut self, schema: GraphSchema) -> Result<(), GraphError> {
        graphersal::storage::PersistentStorage::load_schema(&mut self.0, schema)
    }
    fn load_definition(&mut self, definition: Definition) -> Result<(), GraphError> {
        graphersal::storage::PersistentStorage::load_definition(&mut self.0, definition)
    }
    fn finish_load(&mut self) {
        graphersal::storage::PersistentStorage::finish_load(&mut self.0)
    }
    fn clear(&mut self) {
        graphersal::storage::PersistentStorage::clear(&mut self.0)
    }
    fn attach_journal(
        &mut self,
        journal: graphersal::storage::JournalSlot,
    ) -> Result<(), GraphError> {
        graphersal::storage::PersistentStorage::attach_journal(&mut self.0, journal)
    }
    fn replay(&mut self, changes: &ChangeSet, verify: bool) -> Result<(), String> {
        graphersal::storage::PersistentStorage::replay(&mut self.0, changes, verify)
    }
}
