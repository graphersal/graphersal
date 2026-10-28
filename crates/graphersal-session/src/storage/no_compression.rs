//! A test storage: [`Forwarding`] without compressed properties and without property indexes.
//! Every method forwards to the wrapped [`Forwarding`] except `recompress`,
//! `compression_stats` and `estimate_compression`, which keep the trait defaults
//! (`Unsupported`, `None`, `Unsupported`), and
//! `property_indexes` and the index lookups, which keep theirs (none; the capability is off):
//! the behaviour of a foreign storage that stores compression rules and index definitions but
//! never applies them.

use graphersal::ElementProperty;
use graphersal::storage::{
    ChangeSet, CommitHook, CommitMetadata, CompactString, Definition, DefinitionKind,
    ExecutionPolicy, GraphError, GraphNodeType, GraphSchema, GraphStatistics, GraphStorage,
    JsonPath, Mark, MemoryStats, PropertyMap, PropertySchema, RollbackReason, StorageCapabilities,
    UnitChanges, UnitMark,
};
use graphersal_storage_tests::forwarding::{
    ForwardEdgeId as Fe, ForwardVertexId as Fv, Forwarding,
};

/// [`Forwarding`] without compressed properties (module docs).
#[derive(Default)]
pub struct NoCompression(Forwarding);

impl GraphStorage for NoCompression {
    type VertexId = Fv;
    type EdgeId = Fe;

    fn vertex_count(&self) -> Result<usize, GraphError> {
        GraphStorage::vertex_count(&self.0)
    }
    fn edge_count(&self) -> Result<usize, GraphError> {
        GraphStorage::edge_count(&self.0)
    }
    fn vertices(&self) -> Result<impl Iterator<Item = Fv> + '_, GraphError> {
        GraphStorage::vertices(&self.0)
    }
    fn edges(&self) -> Result<impl Iterator<Item = Fe> + '_, GraphError> {
        GraphStorage::edges(&self.0)
    }
    fn get_vertex_by_id(&self, id: &str) -> Result<Option<Fv>, GraphError> {
        GraphStorage::get_vertex_by_id(&self.0, id)
    }
    fn get_edge_by_id(&self, id: &str) -> Result<Option<Fe>, GraphError> {
        GraphStorage::get_edge_by_id(&self.0, id)
    }
    fn out_edges(&self, node: Fv) -> Result<impl Iterator<Item = Fe> + '_, GraphError> {
        GraphStorage::out_edges(&self.0, node)
    }
    fn in_edges(&self, node: Fv) -> Result<impl Iterator<Item = Fe> + '_, GraphError> {
        GraphStorage::in_edges(&self.0, node)
    }
    fn vertex_id(&self, node: Fv) -> Result<Option<&str>, GraphError> {
        GraphStorage::vertex_id(&self.0, node)
    }
    fn edge_id(&self, edge: Fe) -> Result<Option<&str>, GraphError> {
        GraphStorage::edge_id(&self.0, edge)
    }
    fn vertex_label(&self, node: Fv) -> Result<Option<&str>, GraphError> {
        GraphStorage::vertex_label(&self.0, node)
    }
    fn edge_label(&self, edge: Fe) -> Result<Option<&str>, GraphError> {
        GraphStorage::edge_label(&self.0, edge)
    }
    fn vertex_labels(&self, node: Fv) -> Result<&[CompactString], GraphError> {
        GraphStorage::vertex_labels(&self.0, node)
    }
    fn vertex_properties(&self, node: Fv) -> Result<Option<&PropertyMap>, GraphError> {
        GraphStorage::vertex_properties(&self.0, node)
    }
    fn edge_properties(&self, edge: Fe) -> Result<Option<&PropertyMap>, GraphError> {
        GraphStorage::edge_properties(&self.0, edge)
    }
    fn edge_endpoints(&self, edge: Fe) -> Result<Option<(Fv, Fv)>, GraphError> {
        GraphStorage::edge_endpoints(&self.0, edge)
    }
    fn contains_vertex(&self, node: Fv) -> Result<bool, GraphError> {
        GraphStorage::contains_vertex(&self.0, node)
    }
    fn contains_edge(&self, edge: Fe) -> Result<bool, GraphError> {
        GraphStorage::contains_edge(&self.0, edge)
    }
    fn vertices_by_label<'a>(
        &'a self,
        label: &'a str,
    ) -> Result<Box<dyn Iterator<Item = Fv> + 'a>, GraphError> {
        GraphStorage::vertices_by_label(&self.0, label)
    }
    fn edges_by_label<'a>(
        &'a self,
        label: &'a str,
    ) -> Result<Box<dyn Iterator<Item = Fe> + 'a>, GraphError> {
        GraphStorage::edges_by_label(&self.0, label)
    }
    fn statistics(&self) -> Result<Option<&GraphStatistics>, GraphError> {
        GraphStorage::statistics(&self.0)
    }

    fn set_schema(&mut self, schema: GraphSchema) -> Result<(), GraphError> {
        GraphStorage::set_schema(&mut self.0, schema)
    }
    fn get_schema(&self) -> Result<Option<GraphSchema>, GraphError> {
        GraphStorage::get_schema(&self.0)
    }
    fn declared_property_schema(
        &self,
        kind: GraphNodeType,
        labels: &[CompactString],
        key: &str,
    ) -> Result<Option<PropertySchema>, GraphError> {
        GraphStorage::declared_property_schema(&self.0, kind, labels, key)
    }

    fn definitions(&self) -> Result<impl Iterator<Item = &Definition> + '_, GraphError> {
        GraphStorage::definitions(&self.0)
    }
    fn definition(
        &self,
        kind: DefinitionKind,
        name: &str,
    ) -> Result<Option<&Definition>, GraphError> {
        GraphStorage::definition(&self.0, kind, name)
    }
    fn set_definition(&mut self, definition: Definition) -> Result<(), GraphError> {
        GraphStorage::set_definition(&mut self.0, definition)
    }
    fn remove_definition(&mut self, kind: DefinitionKind, name: &str) -> Result<bool, GraphError> {
        GraphStorage::remove_definition(&mut self.0, kind, name)
    }

    fn insert_vertex(
        &mut self,
        id: Option<CompactString>,
        labels: Vec<CompactString>,
        properties: PropertyMap,
    ) -> Result<Fv, GraphError> {
        GraphStorage::insert_vertex(&mut self.0, id, labels, properties)
    }
    fn add_vertex_label(&mut self, node: Fv, label: CompactString) -> Result<bool, GraphError> {
        GraphStorage::add_vertex_label(&mut self.0, node, label)
    }
    fn remove_vertex_label(&mut self, node: Fv, label: &str) -> Result<bool, GraphError> {
        GraphStorage::remove_vertex_label(&mut self.0, node, label)
    }
    fn set_edge_label(&mut self, edge: Fe, label: CompactString) -> Result<bool, GraphError> {
        GraphStorage::set_edge_label(&mut self.0, edge, label)
    }
    fn insert_edge(
        &mut self,
        id: Option<CompactString>,
        label: Option<CompactString>,
        from: Fv,
        to: Fv,
        properties: PropertyMap,
    ) -> Result<Fe, GraphError> {
        GraphStorage::insert_edge(&mut self.0, id, label, from, to, properties)
    }
    fn set_vertex_property(
        &mut self,
        node: Fv,
        key: CompactString,
        value: ElementProperty,
    ) -> Result<(), GraphError> {
        GraphStorage::set_vertex_property(&mut self.0, node, key, value)
    }
    fn set_edge_property(
        &mut self,
        edge: Fe,
        key: CompactString,
        value: ElementProperty,
    ) -> Result<(), GraphError> {
        GraphStorage::set_edge_property(&mut self.0, edge, key, value)
    }
    fn set_vertex_property_path(
        &mut self,
        node: Fv,
        path: &JsonPath,
        value: ElementProperty,
    ) -> Result<(), GraphError> {
        GraphStorage::set_vertex_property_path(&mut self.0, node, path, value)
    }
    fn set_edge_property_path(
        &mut self,
        edge: Fe,
        path: &JsonPath,
        value: ElementProperty,
    ) -> Result<(), GraphError> {
        GraphStorage::set_edge_property_path(&mut self.0, edge, path, value)
    }
    fn remove_vertex_property_path(&mut self, node: Fv, path: &JsonPath) -> Result<(), GraphError> {
        GraphStorage::remove_vertex_property_path(&mut self.0, node, path)
    }
    fn remove_edge_property_path(&mut self, edge: Fe, path: &JsonPath) -> Result<(), GraphError> {
        GraphStorage::remove_edge_property_path(&mut self.0, edge, path)
    }
    fn drop_vertex(&mut self, node: Fv) -> Result<(), GraphError> {
        GraphStorage::drop_vertex(&mut self.0, node)
    }
    fn drop_vertex_property(&mut self, prop: &str, node: Fv) -> Result<(), GraphError> {
        GraphStorage::drop_vertex_property(&mut self.0, prop, node)
    }
    fn drop_edge(&mut self, edge: Fe) -> Result<(), GraphError> {
        GraphStorage::drop_edge(&mut self.0, edge)
    }
    fn drop_edge_property(&mut self, prop: &str, edge: Fe) -> Result<(), GraphError> {
        GraphStorage::drop_edge_property(&mut self.0, prop, edge)
    }
    fn shrink_to_fit(&mut self) -> Result<(), GraphError> {
        GraphStorage::shrink_to_fit(&mut self.0)
    }

    fn memory_usage(&self) -> Result<Option<MemoryStats>, GraphError> {
        GraphStorage::memory_usage(&self.0)
    }
    fn memory_footprint_bytes(&self) -> Result<Option<u64>, GraphError> {
        GraphStorage::memory_footprint_bytes(&self.0)
    }
    fn execution_policy(&self) -> Result<ExecutionPolicy, GraphError> {
        GraphStorage::execution_policy(&self.0)
    }

    fn begin_unit(&mut self) -> Result<UnitMark, GraphError> {
        GraphStorage::begin_unit(&mut self.0)
    }
    fn commit_unit(&mut self, mark: UnitMark) -> Result<(), GraphError> {
        GraphStorage::commit_unit(&mut self.0, mark)
    }
    fn rollback_unit(&mut self, mark: UnitMark, reason: &RollbackReason) -> Result<(), GraphError> {
        GraphStorage::rollback_unit(&mut self.0, mark, reason)
    }
    fn unit_changes(&self, mark: UnitMark) -> Result<UnitChanges, GraphError> {
        GraphStorage::unit_changes(&self.0, mark)
    }
    fn set_unit_metadata(&mut self, metadata: CommitMetadata) -> Result<(), GraphError> {
        GraphStorage::set_unit_metadata(&mut self.0, metadata)
    }
    fn discard_unit(&mut self, mark: UnitMark) -> Result<ChangeSet, GraphError> {
        GraphStorage::discard_unit(&mut self.0, mark)
    }
    fn mark(&mut self, name: &str) -> Result<Mark, GraphError> {
        GraphStorage::mark(&mut self.0, name)
    }
    fn capabilities(&self) -> Result<StorageCapabilities, GraphError> {
        // No property indexes either: `property_indexes` and the lookups keep the defaults.
        Ok(GraphStorage::capabilities(&self.0)?.with_property_indexes(false))
    }
    fn register_commit_hook(&mut self, hook: Box<dyn CommitHook>) -> Result<(), GraphError> {
        GraphStorage::register_commit_hook(&mut self.0, hook)
    }
    fn last_commit_seq(&self) -> Result<u64, GraphError> {
        GraphStorage::last_commit_seq(&self.0)
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
impl graphersal::storage::PersistentStorage for NoCompression {
    fn graph_id(&self) -> u128 {
        graphersal::storage::PersistentStorage::graph_id(&self.0)
    }
    fn set_graph_id(&mut self, graph_id: u128) -> Result<(), GraphError> {
        graphersal::storage::PersistentStorage::set_graph_id(&mut self.0, graph_id)
    }
    fn last_committed_at(&self) -> i64 {
        graphersal::storage::PersistentStorage::last_committed_at(&self.0)
    }
    fn id_sequences(&self) -> (u64, u64) {
        graphersal::storage::PersistentStorage::id_sequences(&self.0)
    }
    fn raise_position(&mut self, commit_seq: u64, committed_at: i64) -> Result<(), GraphError> {
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
    ) -> Result<Fv, GraphError> {
        graphersal::storage::PersistentStorage::load_vertex(&mut self.0, id, labels, properties)
    }
    fn load_edge(
        &mut self,
        id: CompactString,
        label: Option<CompactString>,
        from_id: &str,
        to_id: &str,
        properties: PropertyMap,
    ) -> Result<Fe, GraphError> {
        graphersal::storage::PersistentStorage::load_edge(
            &mut self.0,
            id,
            label,
            from_id,
            to_id,
            properties,
        )
    }
    fn load_schema(&mut self, schema: GraphSchema) -> Result<(), GraphError> {
        graphersal::storage::PersistentStorage::load_schema(&mut self.0, schema)
    }
    fn load_definition(&mut self, definition: Definition) -> Result<(), GraphError> {
        graphersal::storage::PersistentStorage::load_definition(&mut self.0, definition)
    }
    fn reserve_load(&mut self, vertices: u64, edges: u64) -> Result<(), GraphError> {
        graphersal::storage::PersistentStorage::reserve_load(&mut self.0, vertices, edges)
    }
    fn finish_load(&mut self) -> Result<(), GraphError> {
        graphersal::storage::PersistentStorage::finish_load(&mut self.0)
    }
    fn clear(&mut self) -> Result<(), GraphError> {
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
