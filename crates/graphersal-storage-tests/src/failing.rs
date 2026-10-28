//! [`Failing`]: a storage whose backend fails on demand, to check that the engine reports a
//! storage error instead of panicking or answering with made-up data.
//!
//! **EXPERIMENTAL**, like the rest of this crate. It wraps a [`Forwarding`] (so it is not
//! `TraversalGraph` either) and forwards every method; a [`Faults`] switch makes one group of
//! methods return [`GraphError::StorageFailed`]: the statistics, the property indexes, the index
//! lookups, opening a unit, rolling a unit back or discarding it (the wrapped graph IS rolled
//! back, then the failure is reported, like a backend whose undo half failed; behind a `Graph`
//! that poisons the graph), the execution policy, the schema,
//! the element scans, the property reads of single elements (also only those after the first
//! `n`, [`Faults::fail_element_reads_after`], counted in [`Faults::element_reads`]), the ids and
//! labels of single elements, the adjacency of a vertex and the unit clock.
//! The switches are shared ([`Failing::faults`]), so a test can turn a fault on while the storage
//! sits inside a `Graph`. The [`failing`](self) contract functions use it.
//!
//! This file uses only `graphersal`'s public facade, like [`forwarding`](crate::forwarding).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::forwarding::{ForwardEdgeId, ForwardVertexId, Forwarding};
use graphersal::ElementProperty;
use graphersal::TraversalGraph;
use graphersal::storage::{
    ChangeSet, CommitHook, CommitMetadata, CompactString, CompressionEstimate,
    CompressionEstimateRequest, CompressionStats, Definition, DefinitionKind, ExecutionPolicy,
    GraphError, GraphNodeType, GraphSchema, GraphStatistics, GraphStorage, JsonPath, Mark,
    MemoryStats, PropertyIndexInfo, PropertyIndexRange, PropertyIndexTransform, PropertyMap,
    PropertySchema, RollbackReason, StorageCapabilities, UnitChanges, UnitMark,
};

/// The fault switches of a [`Failing`] storage: while a switch is on, the methods of its group
/// fail with [`GraphError::StorageFailed`]. All off by default.
#[derive(Debug, Default)]
pub struct Faults {
    /// `statistics`.
    pub statistics: AtomicBool,
    /// `property_indexes`.
    pub property_indexes: AtomicBool,
    /// The four property index lookups.
    pub index_lookup: AtomicBool,
    /// `begin_unit` and `begin_unit_at`.
    pub begin_unit: AtomicBool,
    /// `rollback_unit` (after the wrapped graph rolled back).
    pub rollback_unit: AtomicBool,
    /// `discard_unit` (after the wrapped graph discarded the unit).
    pub discard_unit: AtomicBool,
    /// `execution_policy`.
    pub execution_policy: AtomicBool,
    /// `get_schema`.
    pub get_schema: AtomicBool,
    /// The element scans: `vertices`, `edges`, `vertices_by_label`, `edges_by_label`.
    pub element_scan: AtomicBool,
    /// The property reads of single elements: `vertex_properties`, `edge_properties`.
    pub element_read: AtomicBool,
    /// The adjacency of a vertex: `out_edges`, `in_edges`.
    pub adjacency: AtomicBool,
    /// `unit_started_at` (read when an element is created).
    pub unit_started_at: AtomicBool,
    /// The ids and labels of single elements: `vertex_id`, `edge_id`, `vertex_label`,
    /// `edge_label`.
    pub element_identity: AtomicBool,
    /// How many property reads of single elements (`vertex_properties`, `edge_properties`) the
    /// storage answered or refused so far; a test may reset it.
    pub element_reads: AtomicUsize,
    /// 0: off; `n + 1`: the property reads of single elements after the first `n` fail
    /// ([`Faults::fail_element_reads_after`]).
    element_read_limit: AtomicUsize,
}

impl Faults {
    /// Turns `switch` on (`true`) or off.
    pub fn set(switch: &AtomicBool, on: bool) {
        switch.store(on, Ordering::SeqCst);
    }

    /// Makes the property reads of single elements fail after the first `n` ones from now on
    /// (`None`: never), counting from 0 again ([`Faults::element_reads`] is reset). A test runs
    /// a query once to count its reads, then fails exactly its last ones.
    pub fn fail_element_reads_after(&self, n: Option<usize>) {
        self.element_reads.store(0, Ordering::SeqCst);
        let limit = n.map_or(0, |n| n.saturating_add(1));
        self.element_read_limit.store(limit, Ordering::SeqCst);
    }

    /// Counts one property read of a single element and fails it while `element_read` is on or
    /// the reads allowed by [`Faults::fail_element_reads_after`] are used up.
    fn element_read(&self, operation: &'static str) -> Result<(), GraphError> {
        let before = self.element_reads.fetch_add(1, Ordering::SeqCst);
        let limit = self.element_read_limit.load(Ordering::SeqCst);
        if limit != 0 && before >= limit - 1 {
            return Err(GraphError::storage_failed(
                operation,
                "the backend is unavailable",
            ));
        }
        self.check(&self.element_read, operation)
    }

    /// The backend error of `operation` while `switch` is on.
    fn check(&self, switch: &AtomicBool, operation: &'static str) -> Result<(), GraphError> {
        match switch.load(Ordering::SeqCst) {
            true => Err(GraphError::storage_failed(
                operation,
                "the backend is unavailable",
            )),
            false => Ok(()),
        }
    }
}

/// A [`GraphStorage`] that forwards to a [`Forwarding`] and fails where its [`Faults`] say. See
/// the [module docs](self).
///
/// ```
/// use graphersal::TraversalGraph;
/// use graphersal::storage::GraphStorage;
/// use graphersal_storage_tests::failing::{Failing, Faults};
///
/// let storage = Failing::new(TraversalGraph::tinkerpop_modern());
/// assert!(storage.statistics().is_ok());
/// Faults::set(&storage.faults().statistics, true);
/// assert!(storage.statistics().is_err());
/// ```
#[derive(Default)]
pub struct Failing {
    inner: Forwarding,
    faults: Arc<Faults>,
}

impl Failing {
    /// Wraps `graph`, every fault off.
    pub fn new(graph: TraversalGraph) -> Self {
        Self {
            inner: Forwarding::new(graph),
            faults: Arc::default(),
        }
    }

    /// The fault switches, shared with this storage.
    pub fn faults(&self) -> Arc<Faults> {
        Arc::clone(&self.faults)
    }
}

impl GraphStorage for Failing {
    type VertexId = ForwardVertexId;
    type EdgeId = ForwardEdgeId;

    fn vertex_count(&self) -> Result<usize, GraphError> {
        GraphStorage::vertex_count(&self.inner)
    }

    fn edge_count(&self) -> Result<usize, GraphError> {
        GraphStorage::edge_count(&self.inner)
    }

    fn vertices(&self) -> Result<impl Iterator<Item = ForwardVertexId> + '_, GraphError> {
        self.faults
            .check(&self.faults.element_scan, "scan its vertices")?;
        GraphStorage::vertices(&self.inner)
    }

    fn edges(&self) -> Result<impl Iterator<Item = ForwardEdgeId> + '_, GraphError> {
        self.faults
            .check(&self.faults.element_scan, "scan its edges")?;
        GraphStorage::edges(&self.inner)
    }

    fn get_vertex_by_id(&self, id: &str) -> Result<Option<ForwardVertexId>, GraphError> {
        GraphStorage::get_vertex_by_id(&self.inner, id)
    }

    fn get_edge_by_id(&self, id: &str) -> Result<Option<ForwardEdgeId>, GraphError> {
        GraphStorage::get_edge_by_id(&self.inner, id)
    }

    fn out_edges(
        &self,
        node: ForwardVertexId,
    ) -> Result<impl Iterator<Item = ForwardEdgeId> + '_, GraphError> {
        self.faults.check(
            &self.faults.adjacency,
            "read the outgoing edges of a vertex",
        )?;
        GraphStorage::out_edges(&self.inner, node)
    }

    fn in_edges(
        &self,
        node: ForwardVertexId,
    ) -> Result<impl Iterator<Item = ForwardEdgeId> + '_, GraphError> {
        self.faults.check(
            &self.faults.adjacency,
            "read the incoming edges of a vertex",
        )?;
        GraphStorage::in_edges(&self.inner, node)
    }

    fn vertex_id(&self, node: ForwardVertexId) -> Result<Option<&str>, GraphError> {
        self.faults
            .check(&self.faults.element_identity, "read the id of a vertex")?;
        GraphStorage::vertex_id(&self.inner, node)
    }

    fn edge_id(&self, edge: ForwardEdgeId) -> Result<Option<&str>, GraphError> {
        self.faults
            .check(&self.faults.element_identity, "read the id of an edge")?;
        GraphStorage::edge_id(&self.inner, edge)
    }

    fn vertex_label(&self, node: ForwardVertexId) -> Result<Option<&str>, GraphError> {
        self.faults
            .check(&self.faults.element_identity, "read the label of a vertex")?;
        GraphStorage::vertex_label(&self.inner, node)
    }

    fn edge_label(&self, edge: ForwardEdgeId) -> Result<Option<&str>, GraphError> {
        self.faults
            .check(&self.faults.element_identity, "read the label of an edge")?;
        GraphStorage::edge_label(&self.inner, edge)
    }

    fn vertex_labels(&self, node: ForwardVertexId) -> Result<&[CompactString], GraphError> {
        GraphStorage::vertex_labels(&self.inner, node)
    }

    fn vertex_properties(&self, node: ForwardVertexId) -> Result<Option<&PropertyMap>, GraphError> {
        self.faults
            .element_read("read the properties of a vertex")?;
        GraphStorage::vertex_properties(&self.inner, node)
    }

    fn edge_properties(&self, edge: ForwardEdgeId) -> Result<Option<&PropertyMap>, GraphError> {
        self.faults.element_read("read the properties of an edge")?;
        GraphStorage::edge_properties(&self.inner, edge)
    }

    fn edge_endpoints(
        &self,
        edge: ForwardEdgeId,
    ) -> Result<Option<(ForwardVertexId, ForwardVertexId)>, GraphError> {
        GraphStorage::edge_endpoints(&self.inner, edge)
    }

    fn contains_vertex(&self, node: ForwardVertexId) -> Result<bool, GraphError> {
        GraphStorage::contains_vertex(&self.inner, node)
    }

    fn contains_edge(&self, edge: ForwardEdgeId) -> Result<bool, GraphError> {
        GraphStorage::contains_edge(&self.inner, edge)
    }

    fn vertices_by_label<'a>(
        &'a self,
        label: &'a str,
    ) -> Result<Box<dyn Iterator<Item = ForwardVertexId> + 'a>, GraphError> {
        self.faults
            .check(&self.faults.element_scan, "scan its vertices by label")?;
        GraphStorage::vertices_by_label(&self.inner, label)
    }

    fn edges_by_label<'a>(
        &'a self,
        label: &'a str,
    ) -> Result<Box<dyn Iterator<Item = ForwardEdgeId> + 'a>, GraphError> {
        self.faults
            .check(&self.faults.element_scan, "scan its edges by label")?;
        GraphStorage::edges_by_label(&self.inner, label)
    }

    fn statistics(&self) -> Result<Option<&GraphStatistics>, GraphError> {
        self.faults
            .check(&self.faults.statistics, "read its statistics")?;
        GraphStorage::statistics(&self.inner)
    }

    fn set_schema(&mut self, schema: GraphSchema) -> Result<(), GraphError> {
        GraphStorage::set_schema(&mut self.inner, schema)
    }

    fn get_schema(&self) -> Result<Option<GraphSchema>, GraphError> {
        self.faults
            .check(&self.faults.get_schema, "read its schema")?;
        GraphStorage::get_schema(&self.inner)
    }

    fn declared_property_schema(
        &self,
        kind: GraphNodeType,
        labels: &[CompactString],
        key: &str,
    ) -> Result<Option<PropertySchema>, GraphError> {
        GraphStorage::declared_property_schema(&self.inner, kind, labels, key)
    }

    fn definitions(&self) -> Result<impl Iterator<Item = &Definition> + '_, GraphError> {
        GraphStorage::definitions(&self.inner)
    }

    fn definition(
        &self,
        kind: DefinitionKind,
        name: &str,
    ) -> Result<Option<&Definition>, GraphError> {
        GraphStorage::definition(&self.inner, kind, name)
    }

    fn set_definition(&mut self, definition: Definition) -> Result<(), GraphError> {
        GraphStorage::set_definition(&mut self.inner, definition)
    }

    fn remove_definition(&mut self, kind: DefinitionKind, name: &str) -> Result<bool, GraphError> {
        GraphStorage::remove_definition(&mut self.inner, kind, name)
    }

    fn recompress(&mut self, name: &str) -> Result<bool, GraphError> {
        GraphStorage::recompress(&mut self.inner, name)
    }

    fn compression_stats(&self, name: &str) -> Result<Option<CompressionStats>, GraphError> {
        GraphStorage::compression_stats(&self.inner, name)
    }

    fn estimate_compression(
        &self,
        request: &CompressionEstimateRequest,
    ) -> Result<CompressionEstimate, GraphError> {
        GraphStorage::estimate_compression(&self.inner, request)
    }

    fn property_indexes(&self) -> Result<Vec<PropertyIndexInfo>, GraphError> {
        self.faults
            .check(&self.faults.property_indexes, "read its property indexes")?;
        GraphStorage::property_indexes(&self.inner)
    }

    fn vertices_by_property_index<'a>(
        &'a self,
        label: &'a str,
        key: &'a str,
        range: &PropertyIndexRange,
    ) -> Result<Option<Box<dyn Iterator<Item = ForwardVertexId> + 'a>>, GraphError> {
        self.faults
            .check(&self.faults.index_lookup, "read a property index")?;
        GraphStorage::vertices_by_property_index(&self.inner, label, key, range)
    }

    fn edges_by_property_index<'a>(
        &'a self,
        label: &'a str,
        key: &'a str,
        range: &PropertyIndexRange,
    ) -> Result<Option<Box<dyn Iterator<Item = ForwardEdgeId> + 'a>>, GraphError> {
        self.faults
            .check(&self.faults.index_lookup, "read a property index")?;
        GraphStorage::edges_by_property_index(&self.inner, label, key, range)
    }

    fn vertices_by_transformed_property_index<'a>(
        &'a self,
        label: &'a str,
        key: &'a str,
        transform: &PropertyIndexTransform,
        range: &PropertyIndexRange,
    ) -> Result<Option<Box<dyn Iterator<Item = ForwardVertexId> + 'a>>, GraphError> {
        self.faults
            .check(&self.faults.index_lookup, "read a property index")?;
        GraphStorage::vertices_by_transformed_property_index(
            &self.inner,
            label,
            key,
            transform,
            range,
        )
    }

    fn edges_by_transformed_property_index<'a>(
        &'a self,
        label: &'a str,
        key: &'a str,
        transform: &PropertyIndexTransform,
        range: &PropertyIndexRange,
    ) -> Result<Option<Box<dyn Iterator<Item = ForwardEdgeId> + 'a>>, GraphError> {
        self.faults
            .check(&self.faults.index_lookup, "read a property index")?;
        GraphStorage::edges_by_transformed_property_index(&self.inner, label, key, transform, range)
    }

    fn insert_vertex(
        &mut self,
        id: Option<CompactString>,
        labels: Vec<CompactString>,
        properties: PropertyMap,
    ) -> Result<ForwardVertexId, GraphError> {
        GraphStorage::insert_vertex(&mut self.inner, id, labels, properties)
    }

    fn add_vertex_label(
        &mut self,
        node: ForwardVertexId,
        label: CompactString,
    ) -> Result<bool, GraphError> {
        GraphStorage::add_vertex_label(&mut self.inner, node, label)
    }

    fn remove_vertex_label(
        &mut self,
        node: ForwardVertexId,
        label: &str,
    ) -> Result<bool, GraphError> {
        GraphStorage::remove_vertex_label(&mut self.inner, node, label)
    }

    fn set_edge_label(
        &mut self,
        edge: ForwardEdgeId,
        label: CompactString,
    ) -> Result<bool, GraphError> {
        GraphStorage::set_edge_label(&mut self.inner, edge, label)
    }

    fn insert_edge(
        &mut self,
        id: Option<CompactString>,
        label: Option<CompactString>,
        from: ForwardVertexId,
        to: ForwardVertexId,
        properties: PropertyMap,
    ) -> Result<ForwardEdgeId, GraphError> {
        GraphStorage::insert_edge(&mut self.inner, id, label, from, to, properties)
    }

    fn set_vertex_property(
        &mut self,
        node: ForwardVertexId,
        key: CompactString,
        value: ElementProperty,
    ) -> Result<(), GraphError> {
        GraphStorage::set_vertex_property(&mut self.inner, node, key, value)
    }

    fn set_edge_property(
        &mut self,
        edge: ForwardEdgeId,
        key: CompactString,
        value: ElementProperty,
    ) -> Result<(), GraphError> {
        GraphStorage::set_edge_property(&mut self.inner, edge, key, value)
    }

    fn set_vertex_property_path(
        &mut self,
        node: ForwardVertexId,
        path: &JsonPath,
        value: ElementProperty,
    ) -> Result<(), GraphError> {
        GraphStorage::set_vertex_property_path(&mut self.inner, node, path, value)
    }

    fn set_edge_property_path(
        &mut self,
        edge: ForwardEdgeId,
        path: &JsonPath,
        value: ElementProperty,
    ) -> Result<(), GraphError> {
        GraphStorage::set_edge_property_path(&mut self.inner, edge, path, value)
    }

    fn remove_vertex_property_path(
        &mut self,
        node: ForwardVertexId,
        path: &JsonPath,
    ) -> Result<(), GraphError> {
        GraphStorage::remove_vertex_property_path(&mut self.inner, node, path)
    }

    fn remove_edge_property_path(
        &mut self,
        edge: ForwardEdgeId,
        path: &JsonPath,
    ) -> Result<(), GraphError> {
        GraphStorage::remove_edge_property_path(&mut self.inner, edge, path)
    }

    fn drop_vertex(&mut self, node: ForwardVertexId) -> Result<(), GraphError> {
        GraphStorage::drop_vertex(&mut self.inner, node)
    }

    fn drop_vertex_property(
        &mut self,
        prop: &str,
        node: ForwardVertexId,
    ) -> Result<(), GraphError> {
        GraphStorage::drop_vertex_property(&mut self.inner, prop, node)
    }

    fn drop_edge(&mut self, edge: ForwardEdgeId) -> Result<(), GraphError> {
        GraphStorage::drop_edge(&mut self.inner, edge)
    }

    fn drop_edge_property(&mut self, prop: &str, edge: ForwardEdgeId) -> Result<(), GraphError> {
        GraphStorage::drop_edge_property(&mut self.inner, prop, edge)
    }

    fn shrink_to_fit(&mut self) -> Result<(), GraphError> {
        GraphStorage::shrink_to_fit(&mut self.inner)
    }

    fn memory_usage(&self) -> Result<Option<MemoryStats>, GraphError> {
        GraphStorage::memory_usage(&self.inner)
    }

    fn memory_footprint_bytes(&self) -> Result<Option<u64>, GraphError> {
        GraphStorage::memory_footprint_bytes(&self.inner)
    }

    fn execution_policy(&self) -> Result<ExecutionPolicy, GraphError> {
        self.faults
            .check(&self.faults.execution_policy, "read its execution policy")?;
        GraphStorage::execution_policy(&self.inner)
    }

    fn begin_unit(&mut self) -> Result<UnitMark, GraphError> {
        self.faults.check(&self.faults.begin_unit, "open a unit")?;
        GraphStorage::begin_unit(&mut self.inner)
    }

    fn begin_unit_at(&mut self, started_at: i128) -> Result<UnitMark, GraphError> {
        self.faults.check(&self.faults.begin_unit, "open a unit")?;
        GraphStorage::begin_unit_at(&mut self.inner, started_at)
    }

    fn next_unit_started_at(&self) -> Result<Option<i128>, GraphError> {
        GraphStorage::next_unit_started_at(&self.inner)
    }

    fn commit_unit(&mut self, mark: UnitMark) -> Result<(), GraphError> {
        GraphStorage::commit_unit(&mut self.inner, mark)
    }

    fn rollback_unit(&mut self, mark: UnitMark, reason: &RollbackReason) -> Result<(), GraphError> {
        let undone = GraphStorage::rollback_unit(&mut self.inner, mark, reason);
        self.faults
            .check(&self.faults.rollback_unit, "roll a unit back")?;
        undone
    }

    fn unit_changes(&self, mark: UnitMark) -> Result<UnitChanges, GraphError> {
        GraphStorage::unit_changes(&self.inner, mark)
    }

    fn set_unit_metadata(&mut self, metadata: CommitMetadata) -> Result<(), GraphError> {
        GraphStorage::set_unit_metadata(&mut self.inner, metadata)
    }

    fn discard_unit(&mut self, mark: UnitMark) -> Result<ChangeSet, GraphError> {
        let discarded = GraphStorage::discard_unit(&mut self.inner, mark);
        self.faults
            .check(&self.faults.discard_unit, "discard a unit")?;
        discarded
    }

    fn mark(&mut self, name: &str) -> Result<Mark, GraphError> {
        GraphStorage::mark(&mut self.inner, name)
    }

    fn capabilities(&self) -> Result<StorageCapabilities, GraphError> {
        GraphStorage::capabilities(&self.inner)
    }

    fn register_commit_hook(&mut self, hook: Box<dyn CommitHook>) -> Result<(), GraphError> {
        GraphStorage::register_commit_hook(&mut self.inner, hook)
    }

    fn last_commit_seq(&self) -> Result<u64, GraphError> {
        GraphStorage::last_commit_seq(&self.inner)
    }

    fn unit_started_at(&self) -> Result<Option<i128>, GraphError> {
        self.faults
            .check(&self.faults.unit_started_at, "read the unit clock")?;
        GraphStorage::unit_started_at(&self.inner)
    }

    fn raise_id_sequences(
        &mut self,
        node_sequence: u64,
        edge_sequence: u64,
    ) -> Result<(), GraphError> {
        GraphStorage::raise_id_sequences(&mut self.inner, node_sequence, edge_sequence)
    }
}
