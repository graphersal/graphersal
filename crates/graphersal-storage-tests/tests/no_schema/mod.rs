//! A storage outside `graphersal` without schema support: `NoSchema` wraps any storage and
//! forwards every method except the schema ones, which keep the trait defaults (`set_schema` is
//! `GraphError::Unsupported`, no stored or declared schema). It is the shape of a foreign storage
//! today (schema enforcement is not reusable outside the crate yet), written against the public
//! facade only, and must pass the full `conformance!` suite: the schema items skip themselves.

use graphersal::ElementProperty;
use graphersal::TraversalGraph;
use graphersal::storage::{
    ChangeSet, CommitHook, CommitMetadata, CompactString, CompressionStats, Definition,
    DefinitionKind, ExecutionPolicy, GraphError, GraphStatistics, GraphStorage, JsonPath, Mark,
    MemoryStats, PropertyMap, RollbackReason, StorageCapabilities, UnitChanges, UnitMark,
};
use graphersal_storage_tests::conformance;

/// Forwards to the wrapped storage, except `set_schema`/`get_schema`/`declared_property_schema`.
pub struct NoSchema<G>(pub G);

impl<G: GraphStorage> GraphStorage for NoSchema<G> {
    type VertexId = G::VertexId;
    type EdgeId = G::EdgeId;

    fn vertex_count(&self) -> usize {
        self.0.vertex_count()
    }
    fn edge_count(&self) -> usize {
        self.0.edge_count()
    }
    fn vertices(&self) -> impl Iterator<Item = Self::VertexId> + '_ {
        self.0.vertices()
    }
    fn edges(&self) -> impl Iterator<Item = Self::EdgeId> + '_ {
        self.0.edges()
    }
    fn get_vertex_by_id(&self, id: &str) -> Option<Self::VertexId> {
        self.0.get_vertex_by_id(id)
    }
    fn get_edge_by_id(&self, id: &str) -> Option<Self::EdgeId> {
        self.0.get_edge_by_id(id)
    }
    fn out_edges(&self, node: Self::VertexId) -> impl Iterator<Item = Self::EdgeId> + '_ {
        self.0.out_edges(node)
    }
    fn in_edges(&self, node: Self::VertexId) -> impl Iterator<Item = Self::EdgeId> + '_ {
        self.0.in_edges(node)
    }
    fn vertices_by_label<'b>(
        &'b self,
        label: &'b str,
    ) -> Box<dyn Iterator<Item = Self::VertexId> + 'b> {
        self.0.vertices_by_label(label)
    }
    fn edges_by_label<'b>(&'b self, label: &'b str) -> Box<dyn Iterator<Item = Self::EdgeId> + 'b> {
        self.0.edges_by_label(label)
    }
    fn vertex_id(&self, node: Self::VertexId) -> Option<&str> {
        self.0.vertex_id(node)
    }
    fn edge_id(&self, edge: Self::EdgeId) -> Option<&str> {
        self.0.edge_id(edge)
    }
    fn vertex_label(&self, node: Self::VertexId) -> Option<&str> {
        self.0.vertex_label(node)
    }
    fn edge_label(&self, edge: Self::EdgeId) -> Option<&str> {
        self.0.edge_label(edge)
    }
    fn vertex_labels(&self, node: Self::VertexId) -> &[CompactString] {
        self.0.vertex_labels(node)
    }
    fn vertex_properties(&self, node: Self::VertexId) -> Option<&PropertyMap> {
        self.0.vertex_properties(node)
    }
    fn edge_properties(&self, edge: Self::EdgeId) -> Option<&PropertyMap> {
        self.0.edge_properties(edge)
    }
    fn edge_endpoints(&self, edge: Self::EdgeId) -> Option<(Self::VertexId, Self::VertexId)> {
        self.0.edge_endpoints(edge)
    }

    fn contains_vertex(&self, node: Self::VertexId) -> bool {
        self.0.contains_vertex(node)
    }

    fn contains_edge(&self, edge: Self::EdgeId) -> bool {
        self.0.contains_edge(edge)
    }

    fn statistics(&self) -> Result<Option<&GraphStatistics>, GraphError> {
        self.0.statistics()
    }

    fn insert_vertex(
        &mut self,
        id: Option<CompactString>,
        labels: Vec<CompactString>,
        properties: PropertyMap,
    ) -> Result<Self::VertexId, GraphError> {
        self.0.insert_vertex(id, labels, properties)
    }

    fn add_vertex_label(
        &mut self,
        node: Self::VertexId,
        label: CompactString,
    ) -> Result<bool, GraphError> {
        self.0.add_vertex_label(node, label)
    }

    fn remove_vertex_label(
        &mut self,
        node: Self::VertexId,
        label: &str,
    ) -> Result<bool, GraphError> {
        self.0.remove_vertex_label(node, label)
    }

    fn set_edge_label(
        &mut self,
        edge: Self::EdgeId,
        label: CompactString,
    ) -> Result<bool, GraphError> {
        self.0.set_edge_label(edge, label)
    }

    fn insert_edge(
        &mut self,
        id: Option<CompactString>,
        label: Option<CompactString>,
        from: Self::VertexId,
        to: Self::VertexId,
        properties: PropertyMap,
    ) -> Result<Self::EdgeId, GraphError> {
        self.0.insert_edge(id, label, from, to, properties)
    }

    fn set_vertex_property(
        &mut self,
        node: Self::VertexId,
        key: CompactString,
        value: ElementProperty,
    ) -> Result<(), GraphError> {
        self.0.set_vertex_property(node, key, value)
    }

    fn set_edge_property(
        &mut self,
        edge: Self::EdgeId,
        key: CompactString,
        value: ElementProperty,
    ) -> Result<(), GraphError> {
        self.0.set_edge_property(edge, key, value)
    }

    fn set_vertex_property_path(
        &mut self,
        node: Self::VertexId,
        path: &JsonPath,
        value: ElementProperty,
    ) -> Result<(), GraphError> {
        self.0.set_vertex_property_path(node, path, value)
    }

    fn set_edge_property_path(
        &mut self,
        edge: Self::EdgeId,
        path: &JsonPath,
        value: ElementProperty,
    ) -> Result<(), GraphError> {
        self.0.set_edge_property_path(edge, path, value)
    }

    fn remove_vertex_property_path(
        &mut self,
        node: Self::VertexId,
        path: &JsonPath,
    ) -> Result<(), GraphError> {
        self.0.remove_vertex_property_path(node, path)
    }

    fn remove_edge_property_path(
        &mut self,
        edge: Self::EdgeId,
        path: &JsonPath,
    ) -> Result<(), GraphError> {
        self.0.remove_edge_property_path(edge, path)
    }

    fn drop_vertex(&mut self, node: Self::VertexId) -> Result<(), GraphError> {
        self.0.drop_vertex(node)
    }

    fn drop_vertex_property(&mut self, prop: &str, node: Self::VertexId) -> Result<(), GraphError> {
        self.0.drop_vertex_property(prop, node)
    }

    fn drop_edge(&mut self, edge: Self::EdgeId) -> Result<(), GraphError> {
        self.0.drop_edge(edge)
    }

    fn drop_edge_property(&mut self, prop: &str, edge: Self::EdgeId) -> Result<(), GraphError> {
        self.0.drop_edge_property(prop, edge)
    }

    fn definitions(&self) -> Result<impl Iterator<Item = &Definition> + '_, GraphError> {
        self.0.definitions()
    }

    fn definition(
        &self,
        kind: DefinitionKind,
        name: &str,
    ) -> Result<Option<&Definition>, GraphError> {
        self.0.definition(kind, name)
    }

    fn set_definition(&mut self, definition: Definition) -> Result<(), GraphError> {
        self.0.set_definition(definition)
    }

    fn remove_definition(&mut self, kind: DefinitionKind, name: &str) -> Result<bool, GraphError> {
        self.0.remove_definition(kind, name)
    }

    fn recompress(&mut self, name: &str) -> Result<bool, GraphError> {
        self.0.recompress(name)
    }

    fn compression_stats(&self, name: &str) -> Result<Option<CompressionStats>, GraphError> {
        self.0.compression_stats(name)
    }

    fn memory_usage(&self) -> Result<Option<MemoryStats>, GraphError> {
        self.0.memory_usage()
    }

    fn memory_footprint_bytes(&self) -> Result<Option<u64>, GraphError> {
        self.0.memory_footprint_bytes()
    }

    fn execution_policy(&self) -> Result<ExecutionPolicy, GraphError> {
        self.0.execution_policy()
    }

    fn begin_unit(&mut self) -> Result<UnitMark, GraphError> {
        self.0.begin_unit()
    }

    fn commit_unit(&mut self, mark: UnitMark) -> Result<(), GraphError> {
        self.0.commit_unit(mark)
    }

    fn rollback_unit(&mut self, mark: UnitMark, reason: &RollbackReason) -> Result<(), GraphError> {
        self.0.rollback_unit(mark, reason)
    }

    fn unit_changes(&self, mark: UnitMark) -> Result<UnitChanges, GraphError> {
        self.0.unit_changes(mark)
    }

    fn set_unit_metadata(&mut self, metadata: CommitMetadata) -> Result<(), GraphError> {
        self.0.set_unit_metadata(metadata)
    }

    fn discard_unit(&mut self, mark: UnitMark) -> Result<ChangeSet, GraphError> {
        self.0.discard_unit(mark)
    }

    fn mark(&mut self, name: &str) -> Result<Mark, GraphError> {
        self.0.mark(name)
    }

    fn capabilities(&self) -> Result<StorageCapabilities, GraphError> {
        self.0.capabilities()
    }

    fn register_commit_hook(&mut self, hook: Box<dyn CommitHook>) -> Result<(), GraphError> {
        self.0.register_commit_hook(hook)
    }

    fn last_commit_seq(&self) -> Result<u64, GraphError> {
        self.0.last_commit_seq()
    }

    fn raise_id_sequences(
        &mut self,
        node_sequence: u64,
        edge_sequence: u64,
    ) -> Result<(), GraphError> {
        self.0.raise_id_sequences(node_sequence, edge_sequence)
    }

    fn apply_change_set(&mut self, changes: &ChangeSet) -> Result<(), GraphError> {
        self.0.apply_change_set(changes)
    }
}

conformance!(traversal_graph_without_schema, base = NoSchema(TraversalGraph::new()) => base);

#[test]
fn set_schema_keeps_the_trait_default() {
    use graphersal::schema::GraphSchema;
    let mut storage = NoSchema(TraversalGraph::new());
    assert!(matches!(
        storage.set_schema(GraphSchema::new()),
        Err(GraphError::Unsupported { .. })
    ));
    assert!(storage.get_schema().unwrap().is_none());
}
